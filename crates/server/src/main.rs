mod adb;
mod ws;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use std::net::SocketAddr;
use std::sync::Arc;
use tabula_capture::{Capturer, FrameSource, TestPattern};
use tabula_encode::{EncoderKind, EncoderOptions};
use tabula_protocol::DEFAULT_PORT;
use tabula_session::{LogInput, SessionOptions};
use tabula_vdisplay::{DEFAULT_NAME, Status, VirtualDisplay};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stream the virtual monitor to connected tablets.
    Run {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Stream a synthetic test pattern instead of the virtual monitor.
        #[arg(long)]
        test_pattern: bool,
        /// Test pattern size, WIDTHxHEIGHT.
        #[arg(long, default_value = "1920x1200", value_parser = parse_size)]
        test_size: (u32, u32),
        #[arg(long, default_value_t = 60)]
        fps: u32,
        #[arg(long, default_value_t = 12_000)]
        bitrate_kbps: u32,
    },
    /// Create the virtual monitor and grant this binary capture rights (run as root).
    Setup {
        /// Binary to grant CAP_SYS_ADMIN to; defaults to this executable.
        #[arg(long)]
        binary: Option<PathBuf>,
    },
    /// Remove the virtual monitor (run as root).
    Teardown,
    /// Show whether the virtual monitor exists.
    Status,
    /// Grab one frame from the virtual monitor and save it as PNG.
    CaptureTest {
        #[arg(long, default_value = "tabula-frame.png")]
        out: PathBuf,
        /// Seconds to wait for the compositor to light up the output.
        #[arg(long, default_value_t = 10)]
        wait: u64,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tabula=info,tabula_session=info".into()),
        )
        .init();
    match Cli::parse().cmd {
        Cmd::Run { port, test_pattern, test_size, fps, bitrate_kbps } => {
            let source = if test_pattern { Source::Test(test_size) } else { Source::VirtualDisplay };
            run(port, source, fps, bitrate_kbps)
        }
        Cmd::Setup { binary } => setup(binary),
        Cmd::Teardown => VirtualDisplay::new(DEFAULT_NAME).destroy(),
        Cmd::Status => {
            println!("{:?}", VirtualDisplay::new(DEFAULT_NAME).status());
            Ok(())
        }
        Cmd::CaptureTest { out, wait } => capture_test(&out, Duration::from_secs(wait)),
    }
}

fn setup(binary: Option<PathBuf>) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("setup must run as root (sudo)");
    }
    VirtualDisplay::new(DEFAULT_NAME).create()?;
    println!("virtual monitor enabled; it should now appear in your display settings");

    let binary = match binary {
        Some(b) => b,
        None => std::env::current_exe().context("locating this executable")?,
    };
    let status = Command::new("setcap")
        .arg("cap_sys_admin+ep")
        .arg(&binary)
        .status()
        .context("running setcap (is libcap installed?)")?;
    if !status.success() {
        bail!("setcap on {} failed ({status})", binary.display());
    }
    println!(
        "granted CAP_SYS_ADMIN to {} (rebuilding the binary drops it; rerun setup)",
        binary.display()
    );
    Ok(())
}

fn capture_test(out: &Path, wait: Duration) -> Result<()> {
    if VirtualDisplay::new(DEFAULT_NAME).status() != Status::Enabled {
        bail!("virtual monitor is not set up; run `sudo tabula setup` first");
    }
    let card = tabula_capture::find_card("vkms")?;
    let mut cap = Capturer::open(&card)?;
    let deadline = Instant::now() + wait;
    let frame = loop {
        if let Some(f) = cap.grab()? {
            break f;
        }
        if Instant::now() >= deadline {
            bail!(
                "{} has no active framebuffer; enable the virtual monitor in display settings",
                card.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let file = std::fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&frame.to_rgba())?;
    println!(
        "saved {}x{} frame from {} to {}",
        frame.width,
        frame.height,
        card.display(),
        out.display()
    );
    Ok(())
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let parse = |v: &str| v.parse::<u32>().map_err(|e| e.to_string());
    let (w, h) = (parse(w)?, parse(h)?);
    if w < 16 || h < 16 || w % 2 != 0 || h % 2 != 0 {
        return Err("width and height must be even and at least 16".into());
    }
    Ok((w, h))
}

#[derive(Clone, Copy)]
enum Source {
    VirtualDisplay,
    Test((u32, u32)),
}

fn run(port: u16, source: Source, fps: u32, bitrate_kbps: u32) -> Result<()> {
    let card = match source {
        Source::VirtualDisplay => {
            if VirtualDisplay::new(DEFAULT_NAME).status() != Status::Enabled {
                bail!("virtual monitor is not set up; run `sudo tabula setup` (or use --test-pattern)");
            }
            Some(tabula_capture::find_card("vkms")?)
        }
        Source::Test(_) => None,
    };
    let opts = SessionOptions {
        encoder: EncoderOptions { kind: EncoderKind::detect()?, bitrate_kbps },
        fps,
    };
    tracing::info!(encoder = ?opts.encoder.kind, "using encoder");

    let on_connect: ws::SessionFactory = Arc::new(move |sink, stream| {
        let card = card.clone();
        let make_source = move || -> Result<Box<dyn FrameSource>> {
            Ok(match (source, card) {
                (Source::Test((w, h)), _) => Box::new(TestPattern::new(w, h)),
                (Source::VirtualDisplay, Some(card)) => Box::new(Capturer::open(&card)?),
                (Source::VirtualDisplay, None) => unreachable!(),
            })
        };
        let opts = opts.clone();
        tokio::spawn(async move {
            let result =
                tabula_session::run(sink, stream, make_source, Box::new(LogInput), opts).await;
            if let Err(e) = result {
                tracing::warn!("session ended: {e:#}");
            }
        });
    });

    tokio::runtime::Runtime::new()?.block_on(async move {
        // Loopback only: USB clients arrive through `adb reverse`.
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        let devices = adb::reverse_all(port);
        println!("tabula listening on http://localhost:{port}");
        if devices.is_empty() {
            println!("no tablet on USB; connect one with USB debugging and restart, or open the URL locally");
        } else {
            println!("forwarded to {}: open http://localhost:{port} on the tablet", devices.join(", "));
        }
        axum::serve(listener, ws::router(on_connect))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}
