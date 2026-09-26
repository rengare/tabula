//! USB connection via `adb reverse`, so the tablet reaches the server at
//! `localhost`, which browsers treat as a secure origin (WebCodecs requires one).

use std::process::Command;

/// Sets up `adb reverse` on every attached device. Returns the serials it
/// succeeded on; missing adb or no devices is not an error.
pub fn reverse_all(port: u16) -> Vec<String> {
    let Ok(out) = Command::new("adb").arg("devices").output() else {
        tracing::info!("adb not found; USB forwarding disabled");
        return vec![];
    };
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut ok = Vec::new();
    for serial in devices(&listing) {
        let spec = format!("tcp:{port}");
        let status = Command::new("adb")
            .args(["-s", serial, "reverse", &spec, &spec])
            .status();
        match status {
            Ok(s) if s.success() => ok.push(serial.to_owned()),
            other => tracing::warn!("adb reverse on {serial} failed: {other:?}"),
        }
    }
    ok
}

/// Serials of devices in the `device` state from `adb devices` output.
fn devices(listing: &str) -> impl Iterator<Item = &str> {
    listing.lines().skip(1).filter_map(|l| {
        let mut parts = l.split_whitespace();
        let serial = parts.next()?;
        (parts.next()? == "device").then_some(serial)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_adb_devices() {
        let out = "List of devices attached\nR52T1234\tdevice\nemulator-5554\toffline\nXYZ\tunauthorized\n\n";
        assert_eq!(super::devices(out).collect::<Vec<_>>(), vec!["R52T1234"]);
    }
}
