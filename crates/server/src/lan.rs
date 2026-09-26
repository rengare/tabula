//! LAN mode: HTTPS with a self-signed certificate (browsers only allow
//! WebCodecs on secure origins, and off-localhost that means HTTPS) and a
//! pairing token that every WebSocket connection on the LAN port must carry.
//!
//! Certificate and token persist in `$XDG_STATE_HOME/tabula` so the tablet
//! only has to accept the certificate once.

use anyhow::{Context, Result};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct LanConfig {
    pub token: String,
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    pub addresses: Vec<IpAddr>,
}

fn state_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/state"),
    };
    Ok(base.join("tabula"))
}

/// Writes a file readable only by the user.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(data)?;
    Ok(())
}

/// Non-loopback IPv4 addresses of this machine, most likely LAN first.
pub fn lan_addresses() -> Vec<IpAddr> {
    let mut addrs: Vec<(u8, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && i.ip().is_ipv4())
        .map(|i| {
            // Prefer Wi-Fi and Ethernet over bridges, VPNs and containers.
            let rank = match i.name.get(..2) {
                Some("wl") => 0,
                Some("en" | "et") => 1,
                _ => 2,
            };
            (rank, i.ip())
        })
        .collect();
    addrs.sort();
    addrs.dedup_by_key(|(_, ip)| *ip);
    addrs.into_iter().map(|(_, ip)| ip).collect()
}

pub fn load_or_create(new_token: bool) -> Result<LanConfig> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let addresses = lan_addresses();

    let token_path = dir.join("token");
    let token = match fs::read_to_string(&token_path) {
        Ok(t) if !new_token && t.trim().len() == 32 => t.trim().to_owned(),
        _ => {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
            let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            write_private(&token_path, token.as_bytes())?;
            token
        }
    };

    // The certificate names every address it was made for; make a new one
    // when this machine has an address it doesn't cover (the tablet then
    // has to accept it once more).
    let (cert_path, key_path, names_path) =
        (dir.join("cert.pem"), dir.join("key.pem"), dir.join("cert.names"));
    let mut names = vec!["localhost".to_owned()];
    names.extend(addresses.iter().map(IpAddr::to_string));
    let covered = fs::read_to_string(&names_path).unwrap_or_default();
    let covered: Vec<&str> = covered.lines().collect();
    let (cert_pem, key_pem) = match (fs::read(&cert_path), fs::read(&key_path)) {
        (Ok(c), Ok(k)) if names.iter().all(|n| covered.contains(&n.as_str())) => (c, k),
        _ => {
            let ck = rcgen::generate_simple_self_signed(names.clone())
                .context("generating TLS certificate")?;
            let (c, k) = (ck.cert.pem().into_bytes(), ck.signing_key.serialize_pem().into_bytes());
            write_private(&cert_path, &c)?;
            write_private(&key_path, &k)?;
            write_private(&names_path, names.join("\n").as_bytes())?;
            println!("created a new TLS certificate; the tablet has to accept it once");
            (c, k)
        }
    };
    Ok(LanConfig { token, cert_pem, key_pem, addresses })
}

pub fn print_qr(url: &str) {
    match QrCode::new(url.as_bytes()) {
        Ok(code) => {
            // Inverted colors so the code reads on dark terminal themes too.
            let image = code
                .render::<Dense1x2>()
                .dark_color(Dense1x2::Light)
                .light_color(Dense1x2::Dark)
                .quiet_zone(true)
                .build();
            println!("{image}");
        }
        Err(e) => tracing::warn!("QR code: {e}"),
    }
}
