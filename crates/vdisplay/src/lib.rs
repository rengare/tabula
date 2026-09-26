//! A virtual monitor backed by the kernel's vkms driver, configured through
//! configfs (`Documentation/gpu/vkms.rst`). The result is an ordinary DRM
//! card with one connected connector, so every compositor (Mutter, KWin,
//! cosmic-comp, Xorg, …) picks it up like a hotplugged monitor.
//!
//! All operations here need root.

use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const CONFIGFS_ROOT: &str = "/sys/kernel/config/vkms";
pub const DEFAULT_NAME: &str = "tabula";

const DEFAULT_DEV_PARAM: &str = "/sys/module/vkms/parameters/create_default_dev";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The vkms module is not loaded.
    ModuleMissing,
    /// No configfs instance with this name.
    Absent,
    /// The instance exists but is not enabled.
    Disabled,
    Enabled,
}

pub struct VirtualDisplay {
    dir: PathBuf,
}

impl VirtualDisplay {
    pub fn new(name: &str) -> Self {
        Self { dir: Path::new(CONFIGFS_ROOT).join(name) }
    }

    pub fn status(&self) -> Status {
        if !Path::new(CONFIGFS_ROOT).exists() {
            return Status::ModuleMissing;
        }
        match fs::read_to_string(self.dir.join("enabled")) {
            Err(_) => Status::Absent,
            Ok(s) if s.trim() == "1" => Status::Enabled,
            Ok(_) => Status::Disabled,
        }
    }

    /// Loads vkms without its default device, then creates and enables a
    /// single-output instance: plane0 (primary) → crtc0 ← encoder0 ← connector0.
    /// Idempotent: an already enabled instance is left alone.
    pub fn create(&self) -> Result<()> {
        load_module()?;
        if self.status() == Status::Enabled {
            return Ok(());
        }
        let d = &self.dir;
        mkdir(d)?;
        for sub in ["planes/plane0", "crtcs/crtc0", "encoders/encoder0", "connectors/connector0"] {
            mkdir(&d.join(sub))?;
        }
        write(&d.join("planes/plane0/type"), "1")?;
        write(&d.join("connectors/connector0/status"), "1")?;
        link(&d.join("crtcs/crtc0"), &d.join("planes/plane0/possible_crtcs/crtc0"))?;
        link(&d.join("crtcs/crtc0"), &d.join("encoders/encoder0/possible_crtcs/crtc0"))?;
        link(
            &d.join("encoders/encoder0"),
            &d.join("connectors/connector0/possible_encoders/encoder0"),
        )?;
        write(&d.join("enabled"), "1")
    }

    /// Disables and removes the instance. The monitor disappears from the desktop.
    pub fn destroy(&self) -> Result<()> {
        let d = &self.dir;
        if !d.exists() {
            return Ok(());
        }
        if self.status() == Status::Enabled {
            write(&d.join("enabled"), "0")?;
        }
        for pattern in [
            "planes/*/possible_crtcs",
            "encoders/*/possible_crtcs",
            "connectors/*/possible_encoders",
        ] {
            let (group, links) = pattern.split_once("/*/").unwrap();
            for item in read_dir_paths(&d.join(group))? {
                for l in read_dir_paths(&item.join(links))? {
                    fs::remove_file(&l).with_context(|| format!("removing {}", l.display()))?;
                }
            }
        }
        for group in ["planes", "crtcs", "encoders", "connectors"] {
            for item in read_dir_paths(&d.join(group))? {
                fs::remove_dir(&item).with_context(|| format!("removing {}", item.display()))?;
            }
        }
        fs::remove_dir(d).with_context(|| format!("removing {}", d.display()))
    }
}

fn load_module() -> Result<()> {
    if Path::new(CONFIGFS_ROOT).exists() {
        if fs::read_to_string(DEFAULT_DEV_PARAM).is_ok_and(|v| v.trim() == "Y") {
            eprintln!(
                "warning: vkms was loaded with its default device, which shows up as an extra \
                 monitor; `modprobe -r vkms` and rerun to get rid of it"
            );
        }
        return Ok(());
    }
    let status = Command::new("modprobe")
        .args(["vkms", "create_default_dev=0"])
        .status()
        .context("running modprobe")?;
    if !status.success() {
        bail!("modprobe vkms failed ({status})");
    }
    if !Path::new(CONFIGFS_ROOT).exists() {
        bail!(
            "{CONFIGFS_ROOT} missing after loading vkms: is configfs mounted and does this \
             kernel's vkms have configfs support?"
        );
    }
    Ok(())
}

fn mkdir(p: &Path) -> Result<()> {
    match fs::create_dir(p) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        r => r.with_context(|| format!("creating {}", p.display())),
    }
}

fn write(p: &Path, v: &str) -> Result<()> {
    fs::write(p, v).with_context(|| format!("writing {v:?} to {}", p.display()))
}

fn link(target: &Path, at: &Path) -> Result<()> {
    if at.exists() {
        return Ok(());
    }
    symlink(target, at).with_context(|| format!("linking {} -> {}", at.display(), target.display()))
}

fn read_dir_paths(p: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        r => r
            .with_context(|| format!("listing {}", p.display()))?
            .map(|e| Ok(e?.path()))
            .collect(),
    }
}
