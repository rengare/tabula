mod adb;
mod lan;
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
use tabula_session::{InputHandler, LogInput, SessionOptions};
use tabula_vinput::{PenDevice, PhysicalSize, TouchDevice};
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
        /// H.264 encoder: x264, openh264, or v4l2 (hardware, opt-in). Default: x264.
        #[arg(long)]
        encoder: Option<EncoderKind>,
        /// Don't create the virtual pen; the tablet only shows the screen.
        #[arg(long)]
        view_only: bool,
        /// Log per-stage latency percentiles every few seconds.
        #[arg(long)]
        stats: bool,
        /// Also serve HTTPS on all interfaces for tablets on the same network.
        #[arg(long)]
        lan: bool,
        #[arg(long, default_value_t = DEFAULT_PORT + 1)]
        lan_port: u16,
        /// Replace the LAN pairing token, disconnecting tablets paired with the old one.
        #[arg(long)]
        new_token: bool,
        /// Frames in flight before waiting for the client's decoder (0 = no flow control).
        #[arg(long, default_value_t = 2)]
        max_in_flight: usize,
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
        Cmd::Run {
            port,
            test_pattern,
            test_size,
            fps,
            bitrate_kbps,
            encoder,
            view_only,
            stats,
            max_in_flight,
            lan,
            lan_port,
            new_token,
        } => {
            let kind = match encoder {
                Some(kind) => {
                    kind.check().with_context(|| format!("encoder {kind:?} is not usable"))?;
                    kind
                }
                None => EncoderKind::detect()?,
            };
            let source = if test_pattern { Source::Test(test_size) } else { Source::VirtualDisplay };
            let opts = SessionOptions {
                encoder: EncoderOptions { kind, bitrate_kbps },
                fps,
                stats,
                max_in_flight,
            };
            let lan = if lan { Some((lan_port, lan::load_or_create(new_token)?)) } else { None };
            run(port, lan, source, opts, view_only)
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

    install_uinput_rule()?;
    println!("/dev/uinput is now accessible to the logged-in user (for the virtual pen)");

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

const UINPUT_RULE_PATH: &str = "/etc/udev/rules.d/70-tabula-uinput.rules";
/// `uaccess` grants the user of the active local session access, the same
/// way Steam's rules do for game controllers, without adding anyone to a group.
const UINPUT_RULE: &str = "# Installed by `tabula setup`: lets the active session create the virtual pen.\n\
KERNEL==\"uinput\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"\n";

fn install_uinput_rule() -> Result<()> {
    std::fs::write(UINPUT_RULE_PATH, UINPUT_RULE)
        .with_context(|| format!("writing {UINPUT_RULE_PATH}"))?;
    for args in [
        &["control", "--reload-rules"][..],
        &["trigger", "--action=change", "--sysname-match=uinput"][..],
    ] {
        let status = Command::new("udevadm").args(args).status().context("running udevadm")?;
        if !status.success() {
            bail!("udevadm {} failed ({status})", args.join(" "));
        }
    }
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

/// The session's virtual input devices; either may be missing.
struct DeviceInput {
    pen: Option<PenDevice>,
    touch: Option<TouchDevice>,
}

impl InputHandler for DeviceInput {
    fn pen(&mut self, pen: &tabula_protocol::Pen) {
        if let Some(dev) = &mut self.pen
            && let Err(e) = dev.pen(pen)
        {
            tracing::warn!("pen input: {e:#}");
        }
    }
    fn touch(&mut self, contacts: &[tabula_protocol::Contact]) {
        if let Some(dev) = &mut self.touch
            && let Err(e) = dev.touch(contacts)
        {
            tracing::warn!("touch input: {e:#}");
        }
    }
    fn release(&mut self) {
        if let Some(dev) = &mut self.pen {
            let _ = dev.release();
        }
        if let Some(dev) = &mut self.touch {
            let _ = dev.release();
        }
    }
}

fn make_input(hello: &tabula_protocol::Hello, view_only: bool) -> Box<dyn InputHandler> {
    use tabula_protocol::Features;
    if view_only {
        return Box::new(LogInput);
    }
    let size = PhysicalSize::from_pixels(hello.screen_w, hello.screen_h, hello.dpi);
    let pen = hello
        .features
        .contains(Features::PEN)
        .then(|| PenDevice::new(size).inspect_err(|e| tracing::warn!("no pen input: {e:#}")).ok())
        .flatten();
    let touch = hello
        .features
        .contains(Features::TOUCH)
        .then(|| TouchDevice::new(size).inspect_err(|e| tracing::warn!("no touch input: {e:#}")).ok())
        .flatten();
    if pen.is_none() && touch.is_none() {
        return Box::new(LogInput);
    }
    Box::new(DeviceInput { pen, touch })
}

fn run(
    port: u16,
    lan: Option<(u16, lan::LanConfig)>,
    source: Source,
    opts: SessionOptions,
    view_only: bool,
) -> Result<()> {
    let card = match source {
        Source::VirtualDisplay => {
            if VirtualDisplay::new(DEFAULT_NAME).status() != Status::Enabled {
                bail!("virtual monitor is not set up; run `sudo tabula setup` (or use --test-pattern)");
            }
            Some(tabula_capture::find_card("vkms")?)
        }
        Source::Test(_) => None,
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
            let make_input = move |hello: &tabula_protocol::Hello| make_input(hello, view_only);
            let result = tabula_session::run(sink, stream, make_source, make_input, opts).await;
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
        let local = axum::serve(listener, ws::router(on_connect.clone(), None)).into_future();
        let lan_server = async {
            let Some((lan_port, cfg)) = lan else {
                return std::future::pending::<Result<()>>().await;
            };
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem(cfg.cert_pem, cfg.key_pem)
                .await
                .context("loading TLS certificate")?;
            let addr = SocketAddr::from(([0, 0, 0, 0], lan_port));
            let router = ws::router(on_connect, Some(Arc::from(cfg.token.as_str())));
            let server = axum_server::bind_rustls(addr, tls).serve(router.into_make_service());
            println!("LAN: open on the tablet (accept the self-signed certificate once):");
            for ip in &cfg.addresses {
                println!("  https://{ip}:{lan_port}/?token={}", cfg.token);
            }
            if let Some(ip) = cfg.addresses.first() {
                lan::print_qr(&format!("https://{ip}:{lan_port}/?token={}", cfg.token));
            }
            server.await.with_context(|| format!("serving HTTPS on {addr}"))
        };
        tokio::select! {
            r = local => r?,
            r = lan_server => r?,
            _ = tokio::signal::ctrl_c() => {}
        }
        Ok(())
    })
}
