//! Counts page flips per second on the vkms card by polling the scanned-out
//! framebuffer id every millisecond: `cargo run --release -p tabula-capture --example flips [seconds]`
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args().nth(1).map_or(Ok(5), |s| s.parse())?;
    let card = tabula_capture::find_card("vkms")?;
    let cap = tabula_capture::Capturer::open(&card)?;
    let mut last = None;
    let mut per_second = Vec::new();
    let mut count = 0;
    let mut window = Instant::now();
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        let fb = cap.current_fb()?;
        if fb != last {
            count += 1;
            last = fb;
        }
        if window.elapsed() >= Duration::from_secs(1) {
            per_second.push(count);
            count = 0;
            window = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("flips per second: {per_second:?}");
    Ok(())
}
