//! Low-latency H.264 encoding with GStreamer.
//!
//! Output is constrained-baseline H.264 without B-frames, as Annex-B access
//! units with SPS/PPS repeated on every keyframe. Both WebCodecs and Android
//! MediaCodec decode that directly.

use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tabula_capture::{Frame, PixelFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    /// Hardware encoder through V4L2 (e.g. Qualcomm Iris, Raspberry Pi, Rockchip).
    V4l2,
    X264,
    OpenH264,
}

impl std::str::FromStr for EncoderKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "v4l2" => Ok(Self::V4l2),
            "x264" => Ok(Self::X264),
            "openh264" => Ok(Self::OpenH264),
            _ => Err(format!("unknown encoder {s:?} (expected v4l2, x264 or openh264)")),
        }
    }
}

impl EncoderKind {
    /// The first available software encoder. The V4L2 hardware encoder is
    /// opt-in: on the Qualcomm Iris driver it saves CPU but has hung the
    /// device under load (see README), so use [`EncoderKind::check`] first.
    pub fn detect() -> Result<Self> {
        gst::init()?;
        for kind in [Self::X264, Self::OpenH264] {
            if kind.available() {
                return Ok(kind);
            }
        }
        bail!("no H.264 encoder found; install gst-plugins-ugly (x264enc) or openh264enc")
    }

    /// Test-encodes a few frames, giving up after a few seconds so a wedged
    /// hardware encoder can't hang the caller.
    pub fn check(self) -> Result<()> {
        if !self.available() {
            bail!("{} is not installed", self.element());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(probe(self));
        });
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|_| anyhow!("{} did not respond within 3 s", self.element()))?
    }

    pub fn available(self) -> bool {
        gst::init().is_ok() && gst::ElementFactory::find(self.element()).is_some()
    }

    fn element(self) -> &'static str {
        match self {
            Self::V4l2 => "v4l2h264enc",
            Self::X264 => "x264enc",
            Self::OpenH264 => "openh264enc",
        }
    }

    fn input_format(self) -> &'static str {
        match self {
            Self::V4l2 => "NV12",
            Self::X264 | Self::OpenH264 => "I420",
        }
    }

    /// Extra constraints on the encoder's output caps.
    fn output_caps(self) -> &'static str {
        match self {
            // The Iris encoder writes level 1.0 into the SPS unless told
            // otherwise; 5.1 covers up to 2560x1600 at 60 fps.
            Self::V4l2 => ",level=(string)5.1",
            Self::X264 | Self::OpenH264 => "",
        }
    }

    fn launch_fragment(self, bitrate_kbps: u32) -> String {
        match self {
            Self::V4l2 => format!(
                "v4l2h264enc extra-controls=\"controls,video_bitrate={},video_gop_size=600,\
                 video_b_frames=0\"",
                bitrate_kbps * 1000
            ),
            Self::X264 => format!(
                "x264enc tune=zerolatency speed-preset=ultrafast bframes=0 key-int-max=600 \
                 bitrate={bitrate_kbps}"
            ),
            Self::OpenH264 => format!(
                "openh264enc complexity=low rate-control=bitrate gop-size=600 bitrate={}",
                bitrate_kbps * 1000
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncoderOptions {
    pub kind: EncoderKind,
    pub bitrate_kbps: u32,
}

/// One encoded access unit.
pub struct AccessUnit {
    pub pts_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

pub struct Encoder {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    sink: gst_app::AppSink,
    width: u32,
    height: u32,
    first_pts: Arc<AtomicU64>,
}

/// Not yet set.
const NO_PTS: u64 = u64::MAX;

impl Encoder {
    pub fn new(width: u32, height: u32, format: PixelFormat, opts: &EncoderOptions) -> Result<Self> {
        gst::init()?;
        let raw_format = match format {
            PixelFormat::Bgrx => "BGRx",
            PixelFormat::Rgbx => "RGBx",
        };
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=false \
               caps=video/x-raw,format={raw_format},width={width},height={height},framerate=60/1 \
             ! videoconvert n-threads=4 ! video/x-raw,format={input} \
             ! {enc} \
             ! video/x-h264,profile=constrained-baseline,stream-format=byte-stream,alignment=au{caps} \
             ! h264parse config-interval=-1 \
             ! appsink name=sink sync=false max-buffers=8",
            input = opts.kind.input_format(),
            enc = opts.kind.launch_fragment(opts.bitrate_kbps),
            caps = opts.kind.output_caps(),
        );
        let pipeline = gst::parse::launch(&desc)
            .context("building encoder pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("encoder pipeline is not a pipeline"))?;
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .context("appsrc missing")?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .context("appsink missing")?;
        pipeline
            .set_state(gst::State::Playing)
            .context("starting encoder pipeline")?;
        Ok(Self { pipeline, src, sink, width, height, first_pts: Arc::new(AtomicU64::new(NO_PTS)) })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn push(&self, frame: Frame, pts_us: u64) -> Result<()> {
        let _ = self.first_pts.compare_exchange(NO_PTS, pts_us, Ordering::Relaxed, Ordering::Relaxed);
        let mut buf = gst::Buffer::from_mut_slice(frame.data);
        buf.get_mut()
            .expect("fresh buffer is writable")
            .set_pts(gst::ClockTime::from_useconds(pts_us));
        self.src.push_buffer(buf).context("pushing frame to encoder")?;
        Ok(())
    }

    pub fn force_keyframe(&self) {
        let event = gst_video::DownstreamForceKeyUnitEvent::builder()
            .all_headers(true)
            .build();
        self.src.send_event(event);
    }

    /// A handle for pulling encoded output, usable from another thread.
    pub fn output(&self) -> EncoderOutput {
        EncoderOutput { sink: self.sink.clone(), first_in: self.first_pts.clone(), first_out: None }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.src.end_of_stream();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

pub struct EncoderOutput {
    sink: gst_app::AppSink,
    first_in: Arc<AtomicU64>,
    first_out: Option<u64>,
}

/// Encodes a few small frames to check that an encoder actually works here
/// (a V4L2 element can exist without usable hardware behind it).
fn probe(kind: EncoderKind) -> Result<()> {
    use tabula_capture::{FrameSource, TestPattern};
    let opts = EncoderOptions { kind, bitrate_kbps: 2000 };
    let mut src = TestPattern::new(320, 240);
    let enc = Encoder::new(320, 240, PixelFormat::Rgbx, &opts)?;
    for i in 0..5 {
        enc.push(src.grab()?.context("test pattern produced no frame")?, i * 16_667)?;
    }
    let sample = enc
        .sink
        .try_pull_sample(gst::ClockTime::from_seconds(2))
        .context("no output within 2 s")?;
    let buffer = sample.buffer().context("empty sample")?;
    let map = buffer.map_readable()?;
    codec_string(map.as_slice()).context("output has no SPS")?;
    Ok(())
}

impl EncoderOutput {
    /// Blocks until the next access unit. `None` once the encoder is gone.
    ///
    /// The pipeline rebases timestamps to start at zero and x264enc adds a
    /// 1000 hour offset on top, so output pts are mapped back onto the pts
    /// passed to [`Encoder::push`]. The first output always belongs to the
    /// first input since neither encoder reorders or drops frames here.
    pub fn pull(&mut self) -> Option<AccessUnit> {
        let sample = self.sink.pull_sample().ok()?;
        let buffer = sample.buffer()?;
        let map = buffer.map_readable().ok()?;
        let raw = buffer.pts().map_or(0, |t| t.useconds());
        let first_out = *self.first_out.get_or_insert(raw);
        let first_in = self.first_in.load(Ordering::Relaxed);
        Some(AccessUnit {
            pts_us: (raw - first_out).saturating_add(if first_in == NO_PTS { 0 } else { first_in }),
            keyframe: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
            data: map.as_slice().to_vec(),
        })
    }
}

/// WebCodecs codec string (`avc1.PPCCLL`) from the SPS in an Annex-B access unit.
pub fn codec_string(annexb: &[u8]) -> Option<String> {
    nal_units(annexb)
        .find(|nal| nal.first().is_some_and(|h| h & 0x1f == 7) && nal.len() >= 4)
        .map(|sps| format!("avc1.{:02X}{:02X}{:02X}", sps[1], sps[2], sps[3]))
}

/// Splits an Annex-B byte stream on 3- and 4-byte start codes.
fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|&s| {
            let e = s - 3;
            if e > 0 && data[e - 1] == 0 { e - 1 } else { e }
        })
        .chain(std::iter::once(data.len()))
        .collect();
    starts.into_iter().zip(ends).map(move |(s, e)| &data[s..e])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tabula_capture::{FrameSource, TestPattern};

    #[test]
    fn codec_string_from_sps() {
        let au = [0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x28, 0xAA, 0, 0, 1, 0x68, 0xCE];
        assert_eq!(codec_string(&au).as_deref(), Some("avc1.42C028"));
        assert_eq!(codec_string(&[0, 0, 1, 0x65, 1, 2]), None);
    }

    #[test]
    fn nal_splitting() {
        let data = [0, 0, 0, 1, 9, 9, 0, 0, 1, 7, 7, 0, 0, 0, 1, 8];
        let nals: Vec<_> = nal_units(&data).collect();
        assert_eq!(nals, vec![&[9, 9][..], &[7, 7][..], &[8][..]]);
    }

    #[test]
    fn encodes_test_pattern() -> Result<()> {
        for kind in [EncoderKind::V4l2, EncoderKind::X264, EncoderKind::OpenH264] {
            if let Err(e) = kind.check() {
                eprintln!("skipping {kind:?}: {e:#}");
                continue;
            }
            let opts = EncoderOptions { kind, bitrate_kbps: 4000 };
            let mut src = TestPattern::new(1920, 1200);
            let enc = Encoder::new(1920, 1200, PixelFormat::Rgbx, &opts)?;
            let mut out = enc.output();
            let pts = [1_000u64, 17_000, 33_500, 51_234, 70_000, 86_000, 103_000, 120_000];
            let mut latencies = Vec::new();
            let mut first = None;
            for (i, p) in pts.iter().enumerate() {
                let frame = src.grab()?.unwrap();
                let t = std::time::Instant::now();
                enc.push(frame, *p)?;
                let au = out.pull().context("no output from encoder")?;
                latencies.push(t.elapsed().as_secs_f32() * 1000.0);
                assert_eq!(au.pts_us, *p, "{kind:?}: pts not preserved");
                if i == 0 {
                    first = Some(au);
                }
            }
            let first = first.unwrap();
            assert!(first.keyframe, "{kind:?}: first frame is not a keyframe");
            let codec = codec_string(&first.data).context("first access unit has no SPS")?;
            assert!(codec.starts_with("avc1.42"), "{kind:?}: not constrained baseline: {codec}");
            assert!(codec != "avc1.42C00A", "{kind:?}: SPS claims level 1.0");
            eprintln!("{kind:?}: {codec}, push→output ms: {latencies:.1?}");
        }
        Ok(())
    }
}

