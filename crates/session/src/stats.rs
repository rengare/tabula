//! Per-stage latency measurements for `--stats`.
//!
//! Each frame is tracked by its pts from the moment a new buffer is detected
//! until its access unit has been handed to the transport. Pen latency is
//! measured from the oldest pen sample that arrived since the previous frame
//! was detected, i.e. how long input waits until a frame that can show it
//! leaves the machine. The tablet's own network and decode time come on top
//! (the web HUD shows decode time and round trip).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_secs(2);
/// Frames dropped before sending never complete; forget them after this.
const PENDING_TTL: Duration = Duration::from_secs(1);

struct FrameTiming {
    detected: Instant,
    pushed: Instant,
    encoded: Option<Instant>,
    pen: Option<Instant>,
}

#[derive(Default)]
struct Samples(Vec<f32>);

impl Samples {
    fn add(&mut self, d: Duration) {
        self.0.push(d.as_secs_f32() * 1000.0);
    }

    fn summary(&mut self) -> String {
        if self.0.is_empty() {
            return "-".into();
        }
        self.0.sort_by(f32::total_cmp);
        let pct = |p: f32| self.0[((self.0.len() - 1) as f32 * p).round() as usize];
        let s = format!("{:.1}/{:.1}", pct(0.5), pct(0.95));
        self.0.clear();
        s
    }
}

struct Inner {
    pending: HashMap<u64, FrameTiming>,
    unattributed_pen: Option<Instant>,
    grab: Samples,
    encode: Samples,
    send: Samples,
    capture_to_sent: Samples,
    pen_to_sent: Samples,
    frames: u32,
    bytes: u64,
    window_start: Instant,
}

pub(crate) struct Stats(Mutex<Inner>);

impl Stats {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(Inner {
            pending: HashMap::new(),
            unattributed_pen: None,
            grab: Samples::default(),
            encode: Samples::default(),
            send: Samples::default(),
            capture_to_sent: Samples::default(),
            pen_to_sent: Samples::default(),
            frames: 0,
            bytes: 0,
            window_start: Instant::now(),
        }))
    }

    pub(crate) fn pen_sample(&self) {
        let mut s = self.0.lock().unwrap();
        s.unattributed_pen.get_or_insert_with(Instant::now);
    }

    /// A new frame was detected at `detected` and pushed to the encoder now.
    pub(crate) fn frame_pushed(&self, pts: u64, detected: Instant) {
        let now = Instant::now();
        let mut s = self.0.lock().unwrap();
        s.pending.retain(|_, t| now - t.detected < PENDING_TTL);
        let pen = s.unattributed_pen.take();
        s.grab.add(now - detected);
        s.pending.insert(pts, FrameTiming { detected, pushed: now, encoded: None, pen });
    }

    pub(crate) fn frame_encoded(&self, pts: u64) {
        let now = Instant::now();
        let mut s = self.0.lock().unwrap();
        let Some(t) = s.pending.get_mut(&pts) else { return };
        t.encoded = Some(now);
        let pushed = t.pushed;
        s.encode.add(now - pushed);
    }

    pub(crate) fn frame_sent(&self, pts: u64, bytes: usize) {
        let now = Instant::now();
        let mut s = self.0.lock().unwrap();
        s.frames += 1;
        s.bytes += bytes as u64;
        if let Some(t) = s.pending.remove(&pts) {
            if let Some(encoded) = t.encoded {
                s.send.add(now - encoded);
            }
            s.capture_to_sent.add(now - t.detected);
            if let Some(pen) = t.pen {
                s.pen_to_sent.add(now - pen);
            }
        }
        let elapsed = now - s.window_start;
        if elapsed >= REPORT_INTERVAL {
            let secs = elapsed.as_secs_f32();
            let fps = s.frames as f32 / secs;
            let mbit = s.bytes as f32 * 8.0 / secs / 1e6;
            let grab = s.grab.summary();
            let encode = s.encode.summary();
            let send = s.send.summary();
            let total = s.capture_to_sent.summary();
            let pen = s.pen_to_sent.summary();
            tracing::info!(
                "stats: {fps:.1} fps {mbit:.2} Mbit/s | ms p50/p95: grab {grab} encode {encode} \
                 send {send} capture→sent {total} pen→sent {pen}"
            );
            s.frames = 0;
            s.bytes = 0;
            s.window_start = now;
        }
    }
}
