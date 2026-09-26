//! One client session: handshake, capture → encode → send, and input
//! handling. Knows nothing about the transport; adapters (WebSocket now,
//! raw TCP for a native app later) implement [`MessageSink`] and
//! [`MessageStream`].

use anyhow::{Context, Result, bail};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tabula_capture::FrameSource;
use tabula_encode::{AccessUnit, Encoder, EncoderOptions, codec_string};
use tabula_protocol::{DecodeError, Hello, Message, PROTOCOL_VERSION, Pen, StreamConfig, Video};
use tokio::sync::mpsc;

pub trait MessageSink: Send {
    fn send(&mut self, msg: Message) -> impl Future<Output = Result<()>> + Send;
}

pub trait MessageStream: Send {
    /// The next message, `Ok(None)` when the peer closed the connection.
    /// Transports should return undecodable messages as errors; the session
    /// skips unknown tags.
    fn recv(&mut self) -> impl Future<Output = Result<Option<Result<Message, DecodeError>>>> + Send;
}

/// Receives pen (and later touch) input from the client.
pub trait InputHandler: Send {
    fn pen(&mut self, pen: &Pen);
    /// Called once when the session ends.
    fn release(&mut self) {}
}

/// Logs input instead of injecting it.
pub struct LogInput;

impl InputHandler for LogInput {
    fn pen(&mut self, pen: &Pen) {
        tracing::trace!(?pen, "pen");
    }
}

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub encoder: EncoderOptions,
    pub fps: u32,
}

/// If the source keeps showing the same buffer, still send a frame this often
/// so a newly (re)started decoder always gets a picture.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// How often to look for a new buffer. Much faster than the display's refresh
/// so flips are picked up right away instead of beating against our timer.
const POLL_INTERVAL: Duration = Duration::from_millis(4);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Encoded frames queued for the network before we start dropping.
const OUTPUT_QUEUE: usize = 3;

enum Out {
    Config(StreamConfig),
    Video(AccessUnit),
}

pub async fn run<S, R, F, I>(
    mut sink: S,
    mut stream: R,
    make_source: F,
    make_input: I,
    opts: SessionOptions,
) -> Result<()>
where
    S: MessageSink,
    R: MessageStream,
    F: FnOnce() -> Result<Box<dyn FrameSource>> + Send + 'static,
    I: FnOnce(&Hello) -> Box<dyn InputHandler>,
{
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, stream.recv()).await {
        Err(_) => bail!("client sent no Hello within {HELLO_TIMEOUT:?}"),
        Ok(r) => match r? {
            Some(Ok(Message::Hello(h))) => h,
            None => return Ok(()),
            Some(other) => bail!("expected Hello, got {other:?}"),
        },
    };
    if hello.proto_ver != PROTOCOL_VERSION {
        bail!("client speaks protocol {}, we speak {PROTOCOL_VERSION}", hello.proto_ver);
    }
    tracing::info!(
        kind = ?hello.client_kind,
        screen = %format!("{}x{}", hello.screen_w, hello.screen_h),
        dpi = hello.dpi,
        features = hello.features.0,
        "client connected"
    );
    let mut input = make_input(&hello);

    let stop = Arc::new(AtomicBool::new(false));
    let want_key = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = mpsc::channel::<Out>(OUTPUT_QUEUE);
    let producer = {
        let (stop, want_key) = (stop.clone(), want_key.clone());
        std::thread::Builder::new()
            .name("tabula-capture".into())
            .spawn(move || {
                let result = make_source()
                    .and_then(|source| produce(source, &opts, &stop, &want_key, tx));
                if let Err(e) = &result {
                    tracing::error!("capture/encode stopped: {e:#}");
                }
            })
            .context("spawning capture thread")?
    };

    let result = async {
        loop {
            tokio::select! {
                out = rx.recv() => match out {
                    Some(Out::Config(c)) => {
                        tracing::info!(codec = %c.codec, size = %format!("{}x{}", c.width, c.height), "stream config");
                        sink.send(Message::StreamConfig(c)).await?;
                    }
                    Some(Out::Video(au)) => {
                        sink.send(Message::Video(Video {
                            pts_us: au.pts_us,
                            keyframe: au.keyframe,
                            data: au.data,
                        }))
                        .await?;
                    }
                    None => bail!("capture stopped"),
                },
                msg = stream.recv() => match msg? {
                    None => return Ok(()),
                    Some(Err(DecodeError::UnknownTag(t))) => tracing::debug!("ignoring message tag {t:#04x}"),
                    Some(Err(e)) => bail!("bad message from client: {e}"),
                    Some(Ok(msg)) => match msg {
                        Message::Pen(p) => input.pen(&p),
                        Message::Ping { t } => sink.send(Message::Pong { t }).await?,
                        Message::RequestKeyframe => want_key.store(true, Ordering::Relaxed),
                        other => tracing::debug!("unexpected message from client: {other:?}"),
                    },
                },
            }
        }
    }
    .await;

    input.release();
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = tokio::task::spawn_blocking(move || producer.join()).await;
    tracing::info!("client disconnected");
    result
}

/// Capture thread: polls the source for new buffers, sends at most `fps`
/// frames per second, (re)creates the encoder when the source size changes,
/// and runs a puller thread per encoder that forwards access units into `tx`.
fn produce(
    mut source: Box<dyn FrameSource>,
    opts: &SessionOptions,
    stop: &AtomicBool,
    want_key: &Arc<AtomicBool>,
    tx: mpsc::Sender<Out>,
) -> Result<()> {
    // A little under the nominal period, so a source flipping at exactly
    // `fps` isn't throttled by timer jitter.
    let min_interval = Duration::from_secs_f64(0.9 / opts.fps.max(1) as f64);
    let start = Instant::now();
    let mut encoder: Option<(Encoder, std::thread::JoinHandle<()>)> = None;
    let mut last_id = None;
    let mut last_push: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) && !tx.is_closed() {
        std::thread::sleep(POLL_INTERVAL);
        let now = Instant::now();
        let since_push = last_push.map_or(Duration::MAX, |t| now - t);
        if since_push < min_interval {
            continue;
        }
        let Some(id) = source.current_id()? else { continue };
        let key_requested = want_key.load(Ordering::Relaxed);
        if last_id == Some(id) && since_push < REFRESH_INTERVAL && !key_requested {
            continue;
        }

        let Some(frame) = source.grab()? else { continue };
        let size = (frame.width, frame.height);
        if encoder.as_ref().is_none_or(|(e, _)| e.size() != size) {
            // Dropping the old encoder ends its puller thread.
            if let Some((old, puller)) = encoder.take() {
                drop(old);
                let _ = puller.join();
            }
            let enc = Encoder::new(frame.width, frame.height, frame.format, &opts.encoder)?;
            let puller = spawn_puller(&enc, size, tx.clone(), want_key.clone())?;
            encoder = Some((enc, puller));
        }
        let (enc, _) = encoder.as_ref().unwrap();
        if want_key.swap(false, Ordering::Relaxed) {
            enc.force_keyframe();
        }
        last_id = Some(frame.buffer_id);
        last_push = Some(now);
        enc.push(frame, (now - start).as_micros() as u64)?;
    }
    Ok(())
}

fn spawn_puller(
    enc: &Encoder,
    (width, height): (u32, u32),
    tx: mpsc::Sender<Out>,
    want_key: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    let output = enc.output();
    std::thread::Builder::new()
        .name("tabula-encode".into())
        .spawn(move || {
            let mut configured = false;
            // After dropping a frame, later delta frames are undecodable; skip
            // until the next keyframe.
            let mut resync = false;
            while let Some(au) = output.pull() {
                if !configured {
                    let Some(codec) = codec_string(&au.data) else {
                        tracing::warn!("encoder output started without SPS");
                        continue;
                    };
                    let config = StreamConfig { width: width as u16, height: height as u16, codec };
                    if tx.blocking_send(Out::Config(config)).is_err() {
                        return;
                    }
                    configured = true;
                }
                if resync && !au.keyframe {
                    continue;
                }
                resync = false;
                match tx.try_send(Out::Video(au)) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::debug!("network backed up, dropping frame");
                        resync = true;
                        want_key.store(true, Ordering::Relaxed);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
        })
        .context("spawning encoder output thread")
}
