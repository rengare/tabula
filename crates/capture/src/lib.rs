//! Reads whatever the compositor scans out on a DRM card, kmsgrab-style:
//! current plane framebuffer → `GETFB2` → PRIME dma-buf → mmap.
//!
//! This works with any compositor because it only looks at kernel KMS state.
//! `GETFB2` only returns buffer handles to the DRM master or to a process with
//! `CAP_SYS_ADMIN`, so the binary needs that capability. The vkms card only
//! scans out linear buffers, so the mapping can be read directly by the CPU.

use anyhow::{Context, Result, bail};
use drm::buffer::{DrmFourcc, DrmModifier};
use drm::control::{Device as ControlDevice, framebuffer, plane};
use drm::Device;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

/// Finds the `/dev/dri/card*` node whose kernel driver is `driver` (e.g. `"vkms"`).
pub fn find_card(driver: &str) -> Result<PathBuf> {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .context("listing /dev/dri")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("card")))
        .collect();
    nodes.sort();
    for path in nodes {
        let Ok(card) = open_card(&path) else { continue };
        if card.get_driver().is_ok_and(|d| d.name() == driver) {
            return Ok(path);
        }
    }
    bail!("no DRM card with driver {driver:?} found (is the virtual display set up?)")
}

fn open_card(path: &Path) -> Result<Card> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let card = Card(file);
    // Without this the kernel hides primary and cursor planes from us.
    card.set_client_capability(drm::ClientCapability::UniversalPlanes, true)
        .context("enabling universal planes")?;
    // Opening a primary node with no master yet makes us master, which would
    // stop the compositor from taking the device. Give it up right away.
    let _ = card.release_master_lock();
    Ok(card)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// DRM XRGB8888 / ARGB8888: bytes are B, G, R, X/A.
    Bgrx,
    /// DRM XBGR8888 / ABGR8888: bytes are R, G, B, X/A.
    Rgbx,
}

impl PixelFormat {
    fn from_fourcc(f: DrmFourcc) -> Option<Self> {
        match f {
            DrmFourcc::Xrgb8888 | DrmFourcc::Argb8888 => Some(Self::Bgrx),
            DrmFourcc::Xbgr8888 | DrmFourcc::Abgr8888 => Some(Self::Rgbx),
            _ => None,
        }
    }
}

/// A captured frame, tightly packed (`stride == width * 4`).
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Identifies the source buffer. Compositors flip between buffers, so an
    /// unchanged id means nothing new was presented.
    pub buffer_id: u32,
    pub data: Vec<u8>,
}

/// Something that produces frames to stream.
pub trait FrameSource: Send {
    /// Cheap check for the id of the buffer currently shown, without copying
    /// it. `None` while there is nothing to show.
    fn current_id(&mut self) -> Result<Option<u32>>;
    /// The latest frame, or `None` while there is nothing to show.
    fn grab(&mut self) -> Result<Option<Frame>>;
}

impl FrameSource for Capturer {
    fn current_id(&mut self) -> Result<Option<u32>> {
        Ok(self.current_fb()?.map(Into::into))
    }
    fn grab(&mut self) -> Result<Option<Frame>> {
        Capturer::grab(self)
    }
}

impl Frame {
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        for px in out.chunks_exact_mut(4) {
            if self.format == PixelFormat::Bgrx {
                px.swap(0, 2);
            }
            px[3] = 255;
        }
        out
    }
}

struct Mapping {
    ptr: *mut libc::c_void,
    len: usize,
    dmabuf: OwnedFd,
    format: PixelFormat,
    width: u32,
    height: u32,
    pitch: usize,
    offset: usize,
}

// The mapping is plain read-only shared memory owned by this struct.
unsafe impl Send for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

// struct dma_buf_sync { __u64 flags; }; DMA_BUF_IOCTL_SYNC = _IOW('b', 0, struct dma_buf_sync)
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 4;

fn dma_buf_sync(fd: &OwnedFd, flags: u64) {
    let arg = flags;
    // Best effort: failing to sync only risks tearing, not a wrong read.
    unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &arg) };
}

/// Upper bound on cached mappings; compositors typically cycle 2–4 buffers.
const MAX_MAPPINGS: usize = 8;

pub struct Capturer {
    card: Card,
    mappings: HashMap<framebuffer::Handle, Mapping>,
}

impl Capturer {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self { card: open_card(path)?, mappings: HashMap::new() })
    }

    /// The framebuffer currently scanned out on the largest active plane, or
    /// `None` when the compositor has not lit up the output (yet).
    pub fn current_fb(&self) -> Result<Option<framebuffer::Handle>> {
        let mut best: Option<(u64, framebuffer::Handle)> = None;
        for p in self.card.plane_handles().context("listing planes")? {
            let info: plane::Info = self.card.get_plane(p).context("reading plane")?;
            let (Some(_), Some(fb)) = (info.crtc(), info.framebuffer()) else { continue };
            let Ok(fbinfo) = self.card.get_framebuffer(fb) else { continue };
            let (w, h) = fbinfo.size();
            let area = w as u64 * h as u64;
            if best.is_none_or(|(a, _)| area > a) {
                best = Some((area, fb));
            }
        }
        Ok(best.map(|(_, fb)| fb))
    }

    /// Copies the current frame out. Returns `None` if nothing is being displayed.
    pub fn grab(&mut self) -> Result<Option<Frame>> {
        let Some(fb) = self.current_fb()? else { return Ok(None) };
        if !self.mappings.contains_key(&fb) {
            if self.mappings.len() >= MAX_MAPPINGS {
                self.mappings.clear();
            }
            let m = self.map(fb)?;
            self.mappings.insert(fb, m);
        }
        let m = &self.mappings[&fb];
        let row = m.width as usize * 4;
        let mut data = Vec::with_capacity(row * m.height as usize);
        dma_buf_sync(&m.dmabuf, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
        let src = unsafe { std::slice::from_raw_parts(m.ptr as *const u8, m.len) };
        for y in 0..m.height as usize {
            let start = m.offset + y * m.pitch;
            data.extend_from_slice(&src[start..start + row]);
        }
        dma_buf_sync(&m.dmabuf, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
        Ok(Some(Frame {
            width: m.width,
            height: m.height,
            format: m.format,
            buffer_id: fb.into(),
            data,
        }))
    }

    fn map(&self, fb: framebuffer::Handle) -> Result<Mapping> {
        let info = self
            .card
            .get_planar_framebuffer(fb)
            .context("GETFB2 failed")?;
        let handles = info.buffers();
        let result = (|| {
            let Some(handle) = handles[0] else {
                bail!(
                    "kernel returned no buffer handle for the framebuffer; \
                     the binary needs CAP_SYS_ADMIN (run `tabula setup`)"
                );
            };
            let Some(format) = PixelFormat::from_fourcc(info.pixel_format()) else {
                bail!("unsupported scanout format {:?}", info.pixel_format());
            };
            if let Some(m) = info.modifier()
                && m != DrmModifier::Linear
            {
                bail!("unsupported framebuffer modifier {m:?} (only linear buffers can be read)");
            }
            let (width, height) = info.size();
            let pitch = info.pitches()[0] as usize;
            let offset = info.offsets()[0] as usize;
            let len = offset + pitch * height as usize;
            let dmabuf = self
                .card
                .buffer_to_prime_fd(handle, libc::O_RDONLY as u32 | libc::O_CLOEXEC as u32)
                .context("exporting framebuffer as dma-buf")?;
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    dmabuf.as_raw_fd(),
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                bail!("mmap of framebuffer failed: {}", std::io::Error::last_os_error());
            }
            Ok(Mapping { ptr, len, dmabuf, format, width, height, pitch, offset })
        })();
        // GETFB2 hands out fresh GEM handles every call; the dma-buf keeps the
        // buffer alive, so close them (deduplicated) regardless of the outcome.
        let mut closed = Vec::new();
        for h in handles.into_iter().flatten() {
            if !closed.contains(&h) {
                let _ = self.card.close_buffer(h);
                closed.push(h);
            }
        }
        result
    }
}

/// A synthetic animated pattern, for testing the pipeline without a
/// virtual monitor: color bars with a square bouncing across them.
pub struct TestPattern {
    width: u32,
    height: u32,
    start: std::time::Instant,
}

impl TestPattern {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, start: std::time::Instant::now() }
    }
}

impl FrameSource for TestPattern {
    /// Behaves like a display flipping at 60 Hz.
    fn current_id(&mut self) -> Result<Option<u32>> {
        Ok(Some((self.start.elapsed().as_secs_f64() * 60.0) as u32))
    }

    fn grab(&mut self) -> Result<Option<Frame>> {
        const BARS: [[u8; 3]; 7] = [
            [192, 192, 192],
            [192, 192, 0],
            [0, 192, 192],
            [0, 192, 0],
            [192, 0, 192],
            [192, 0, 0],
            [0, 0, 192],
        ];
        let (w, h) = (self.width as usize, self.height as usize);
        let t = self.start.elapsed().as_secs_f32();
        let side = (h / 6).max(8);
        let span_x = (w - side) as f32;
        let span_y = (h - side) as f32;
        let bounce = |v: f32, span: f32| {
            let p = v % (2.0 * span);
            if p > span { 2.0 * span - p } else { p }
        };
        let sx = bounce(t * 300.0, span_x) as usize;
        let sy = bounce(t * 200.0, span_y) as usize;
        let mut data = vec![0u8; w * h * 4];
        for (y, row) in data.chunks_exact_mut(w * 4).enumerate() {
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let in_square = (sx..sx + side).contains(&x) && (sy..sy + side).contains(&y);
                let rgb = if in_square { [255, 255, 255] } else { BARS[x * BARS.len() / w] };
                px[..3].copy_from_slice(&rgb);
                px[3] = 255;
            }
        }
        Ok(Some(Frame {
            width: self.width,
            height: self.height,
            format: PixelFormat::Rgbx,
            buffer_id: (t * 60.0) as u32,
            data,
        }))
    }
}
