//! Shares a monitor that is already running, through the xdg-desktop-portal
//! ScreenCast API and PipeWire, the same path browsers and OBS use. GNOME,
//! KDE, COSMIC and wlroots compositors all implement it.
//!
//! Unlike the kmsgrab capture, this needs no `CAP_SYS_ADMIN` and works on
//! GPUs whose scanout buffers are tiled or compressed: the compositor renders
//! the monitor (with the cursor drawn in) into linear shared memory for us.
//! The desktop asks the user which monitor to share every time a cast starts.
//!
//! There is one portal session and one PipeWire stream per server. Each
//! client session gets a cheap [`FrameSource`] handle that reads the latest
//! frame from it.

use anyhow::{Context, Result, bail};
use ashpd::desktop::PersistMode;
use ashpd::desktop::Session;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use pipewire as pw;
use pw::spa;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use tabula_capture::{Frame, FrameSource, PixelFormat};

/// What the PipeWire thread last received.
#[derive(Default)]
struct Latest {
    /// Bumped for every new frame; doubles as the frame's buffer id.
    seq: u32,
    frame: Option<SharedFrame>,
    /// Set once the stream is gone (sharing stopped from the desktop, or an error).
    ended: Option<String>,
}

struct SharedFrame {
    width: u32,
    height: u32,
    format: PixelFormat,
    data: Arc<Vec<u8>>,
}

type Shared = Arc<Mutex<Latest>>;

/// A running screen cast. Dropping it closes the portal session.
pub struct ScreenCast {
    latest: Shared,
    _session: Session<Screencast>,
    _proxy: Screencast,
}

impl ScreenCast {
    /// Asks the desktop for a monitor to share and starts receiving it.
    pub async fn start() -> Result<Self> {
        let proxy = Screencast::new()
            .await
            .context("connecting to xdg-desktop-portal")?;
        let cursor = if proxy
            .available_cursor_modes()
            .await
            .is_ok_and(|m| m.contains(CursorMode::Embedded))
        {
            CursorMode::Embedded
        } else {
            tracing::warn!("the portal can't draw the cursor into the stream; it won't be visible");
            CursorMode::Hidden
        };
        let session = proxy
            .create_session(Default::default())
            .await
            .context("creating portal session")?;
        proxy
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(cursor)
                    .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::DoNot),
            )
            .await
            .context("selecting screen cast sources")?
            .response()
            .context("selecting screen cast sources")?;
        let streams = proxy
            .start(&session, None, Default::default())
            .await
            .context("starting screen cast")?
            .response()
            .context("screen sharing was cancelled or refused")?;
        let Some(stream) = streams.streams().first() else {
            bail!("the portal returned no stream");
        };
        let node = stream.pipe_wire_node_id();
        tracing::info!(node, size = ?stream.size(), position = ?stream.position(), "sharing monitor");
        let fd = proxy
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .context("opening PipeWire remote")?;

        let latest = Shared::default();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        {
            let latest = latest.clone();
            std::thread::Builder::new()
                .name("tabula-pipewire".into())
                .spawn(move || {
                    if let Err(e) = pipewire_loop(fd, node, &latest, &ready_tx) {
                        // Only the first message is read: this one counts if setup failed.
                        let _ = ready_tx.send(Err(anyhow::anyhow!("{e:#}")));
                        tracing::error!("screen cast stream: {e:#}");
                        latest
                            .lock()
                            .unwrap()
                            .ended
                            .get_or_insert_with(|| format!("{e:#}"));
                    }
                })
                .context("spawning PipeWire thread")?;
        }
        // Surface setup errors (e.g. PipeWire refusing the fd) right away.
        ready_rx
            .recv()
            .context("PipeWire thread exited during setup")??;
        Ok(Self {
            latest,
            _session: session,
            _proxy: proxy,
        })
    }

    /// Makes frame sources reading from this cast, one per client session.
    pub fn source_factory(
        &self,
    ) -> impl Fn() -> Box<dyn FrameSource> + Send + Sync + Clone + 'static {
        let latest = self.latest.clone();
        move || {
            Box::new(Viewer {
                latest: latest.clone(),
            }) as Box<dyn FrameSource>
        }
    }
}

struct Viewer {
    latest: Shared,
}

impl FrameSource for Viewer {
    fn current_id(&mut self) -> Result<Option<u32>> {
        let latest = self.latest.lock().unwrap();
        if let Some(why) = &latest.ended {
            bail!("screen sharing ended: {why}");
        }
        Ok(latest.frame.as_ref().map(|_| latest.seq))
    }

    fn grab(&mut self) -> Result<Option<Frame>> {
        let (seq, f) = {
            let latest = self.latest.lock().unwrap();
            if let Some(why) = &latest.ended {
                bail!("screen sharing ended: {why}");
            }
            let Some(f) = &latest.frame else {
                return Ok(None);
            };
            (
                latest.seq,
                SharedFrame {
                    data: f.data.clone(),
                    ..*f
                },
            )
        };
        // Copy outside the lock so the PipeWire thread isn't held up.
        Ok(Some(Frame {
            width: f.width,
            height: f.height,
            format: f.format,
            buffer_id: seq,
            data: f.data.as_ref().clone(),
        }))
    }
}

/// The negotiated format, filled in by `param_changed`.
#[derive(Default)]
struct Negotiated {
    info: spa::param::video::VideoInfoRaw,
    format: Option<PixelFormat>,
}

fn pipewire_loop(
    fd: OwnedFd,
    node: u32,
    latest: &Shared,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    use spa::param::video::VideoFormat;

    pw::init();
    let mainloop = pw::main_loop::MainLoopBox::new(None).context("creating PipeWire main loop")?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)
        .context("creating PipeWire context")?;
    let core = context
        .connect_fd(fd, None)
        .context("connecting to PipeWire")?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "tabula",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .context("creating PipeWire stream")?;

    let state_latest = latest.clone();
    let frame_latest = latest.clone();
    let _listener = stream
        .add_local_listener_with_user_data(Negotiated::default())
        .state_changed(move |_, _, old, new| {
            tracing::debug!(?old, ?new, "PipeWire stream state");
            let ended = match new {
                pw::stream::StreamState::Error(e) => Some(e),
                pw::stream::StreamState::Unconnected => Some("the desktop stopped sharing".into()),
                _ => None,
            };
            if let Some(why) = ended {
                tracing::warn!("screen cast ended: {why}");
                state_latest.lock().unwrap().ended.get_or_insert(why);
            }
        })
        .param_changed(|_, neg, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if neg.info.parse(param).is_err() {
                tracing::warn!("could not parse the negotiated video format");
                return;
            }
            neg.format = match neg.info.format() {
                VideoFormat::BGRx | VideoFormat::BGRA => Some(PixelFormat::Bgrx),
                VideoFormat::RGBx | VideoFormat::RGBA => Some(PixelFormat::Rgbx),
                other => {
                    tracing::warn!(?other, "unsupported video format");
                    None
                }
            };
            let size = neg.info.size();
            tracing::info!(format = ?neg.info.format(), size = %format!("{}x{}", size.width, size.height), "screen cast format");
        })
        .process(move |stream, neg| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let Some(format) = neg.format else { return };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let chunk = d.chunk();
            // Compositors may queue empty or corrupted buffers when nothing changed.
            if chunk.size() == 0 || chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
                return;
            }
            let (offset, stride) = (chunk.offset() as usize, chunk.stride());
            let size = neg.info.size();
            let (w, h) = (size.width as usize, size.height as usize);
            let row = w * 4;
            let stride = if stride > 0 { stride as usize } else { row };
            let Some(src) = d.data() else { return };
            if w == 0 || h == 0 || stride < row || src.len() < offset + stride * (h - 1) + row {
                tracing::debug!("buffer smaller than the negotiated size, skipping");
                return;
            }
            let mut data = Vec::with_capacity(row * h);
            for y in 0..h {
                let start = offset + y * stride;
                data.extend_from_slice(&src[start..start + row]);
            }
            let mut latest = frame_latest.lock().unwrap();
            latest.seq = latest.seq.wrapping_add(1);
            latest.frame = Some(SharedFrame {
                width: w as u32,
                height: h as u32,
                format,
                data: Arc::new(data),
            });
        })
        .register()
        .context("registering stream listener")?;

    let format = enum_format();
    let mut params = [spa::pod::Pod::from_bytes(&format).context("building format pod")?];
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .context("connecting PipeWire stream")?;
    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

/// The formats we accept: 32-bit RGB in shared memory. Leaving out a
/// modifier property means the compositor won't offer dma-bufs, which might
/// be tiled and unreadable by the CPU.
fn enum_format() -> Vec<u8> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    use spa::pod::{Value, property, serialize::PodSerializer};
    use spa::utils::{Fraction, Rectangle};
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        property!(FormatProperties::MediaType, Id, MediaType::Video),
        property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 1920,
                height: 1080
            },
            Rectangle {
                width: 16,
                height: 16
            },
            Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 60, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
        .expect("serializing a static pod")
        .0
        .into_inner()
}
