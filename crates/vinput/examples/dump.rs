//! Prints raw events from an input device, e.g. the virtual pen or touch
//! device: `cargo run -p tabula-vinput --example dump /dev/input/eventN [seconds]`
//! (needs read access, e.g. membership in the `input` group).
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: dump /dev/input/eventN [seconds]");
    let secs: u64 = args.next().map_or(Ok(30), |s| s.parse())?;
    let mut dev = evdev::Device::open(&path)?;
    println!("{}: {:?}", path, dev.name());
    let end = Instant::now() + Duration::from_secs(secs);
    let start = Instant::now();
    let fd = std::os::fd::AsRawFd::as_raw_fd(&dev);
    while Instant::now() < end {
        let mut pfd = libc_poll(fd);
        if unsafe { poll(&mut pfd, 1, 200) } <= 0 {
            continue;
        }
        for ev in dev.fetch_events()? {
            println!("{:>8.3}s {:?}", start.elapsed().as_secs_f32(), ev.destructure());
        }
    }
    Ok(())
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

fn libc_poll(fd: i32) -> PollFd {
    PollFd { fd, events: 1, revents: 0 }
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
}
