//! Dumps plane state of a DRM card: `cargo run -p tabula-capture --example planes [/dev/dri/cardN]`
use drm::Device as _;
use drm::control::Device as _;
use std::os::fd::{AsFd, BorrowedFd};

struct Card(std::fs::File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl drm::control::Device for Card {}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/dev/dri/card0".into());
    let card = Card(std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap());
    let _ = drm::Device::release_master_lock(&card);
    card.set_client_capability(drm::ClientCapability::UniversalPlanes, true).ok();
    card.set_client_capability(drm::ClientCapability::Atomic, true).ok();
    for p in card.plane_handles().unwrap() {
        let info = card.get_plane(p).unwrap();
        println!("plane {p:?}: crtc={:?} fb={:?}", info.crtc(), info.framebuffer());
        if let Some(fb) = info.framebuffer() {
            println!("  getfb:  {:?}", card.get_framebuffer(fb));
            println!("  getfb2: {:?}", card.get_planar_framebuffer(fb));
        }
    }
    for c in card.resource_handles().unwrap().crtcs() {
        println!("crtc {c:?}: {:?}", card.get_crtc(*c));
    }
}
