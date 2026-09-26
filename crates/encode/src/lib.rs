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
    X264,
    OpenH264,
}

impl EncoderKind {
    /// The first available software encoder, x264 preferred.
    pub fn detect() -> Result<Self> {
        gst::init()?;
        for kind in [Self::X264, Self::OpenH264] {
            if gst::ElementFactory::find(kind.element()).is_some() {
                return Ok(kind);
            }
        }
        bail!("no H.264 encoder found; install gst-plugins-ugly (x264enc) or openh264enc")
    }

    fn element(self) -> &'static str {
        match self {
            Self::X264 => "x264enc",
            Self::OpenH264 => "openh264enc",
        }
    }

    fn launch_fragment(self, bitrate_kbps: u32) -> String {
        match self {
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
             ! videoconvert n-threads=4 ! video/x-raw,format=I420 \
             ! {enc} \
             ! video/x-h264,profile=constrained-baseline,stream-format=byte-stream,alignment=au \
             ! h264parse config-interval=-1 \
             ! appsink name=sink sync=false max-buffers=8",
            enc = opts.kind.launch_fragment(opts.bitrate_kbps),
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
        let opts = EncoderOptions { kind: EncoderKind::detect()?, bitrate_kbps: 4000 };
        let mut src = TestPattern::new(320, 240);
        let enc = Encoder::new(320, 240, PixelFormat::Rgbx, &opts)?;
        let mut out = enc.output();
        let pts = [1_000u64, 17_000, 33_500, 51_234, 70_000];
        for p in pts {
            enc.push(src.grab()?.unwrap(), p)?;
        }
        let first = out.pull().context("no output from encoder")?;
        assert!(first.keyframe);
        let codec = codec_string(&first.data).context("first access unit has no SPS")?;
        assert!(codec.starts_with("avc1.42"), "not constrained baseline: {codec}");
        assert_eq!(first.pts_us, pts[0]);
        for p in &pts[1..4] {
            assert_eq!(out.pull().context("missing output")?.pts_us, *p, "pts not preserved");
        }
        Ok(())
    }
}

