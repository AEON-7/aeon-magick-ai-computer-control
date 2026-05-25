//! Build / tear down a USB composite HID gadget via Linux ConfigFS.
//!
//! ConfigFS layout we produce:
//!
//!     /sys/kernel/config/usb_gadget/<gadget_name>/
//!         idVendor              0x05ac
//!         idProduct             0x0265
//!         bcdDevice             0x0100
//!         bcdUSB                0x0200
//!         strings/0x409/
//!             manufacturer      "Apple Inc."
//!             product           "Magic Trackpad"
//!             serialnumber      "ACURSED-MT"
//!         configs/c.1/
//!             MaxPower          500
//!             strings/0x409/configuration  "AcursedKVM"
//!             [symlinks → functions/hid.*]
//!         functions/hid.kbd/
//!             protocol          1
//!             subclass          1
//!             report_length     8
//!             report_desc       <bytes>
//!         functions/hid.mouse/
//!             ...
//!         UDC                   "fe980000.usb"   ← writing this binds & enables
//!
//! After UDC bind, /dev/hidg0, /dev/hidg1, … appear in the order the
//! functions were linked into configs/c.1/. Order matters.

use crate::config::Config;
use crate::persona::{HidFunction, PersonaDescriptors};
use anyhow::{Context, Result};
use std::fs;
use std::os::unix;
use std::path::{Path, PathBuf};
use tracing::info;

fn write_str(path: &Path, val: &str) -> Result<()> {
    fs::write(path, val).with_context(|| format!("writing {} → {}", val, path.display()))
}

fn write_bytes(path: &Path, val: &[u8]) -> Result<()> {
    fs::write(path, val)
        .with_context(|| format!("writing {} bytes → {}", val.len(), path.display()))
}

fn mkdir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("mkdir {}", path.display()))
}

/// Write the entire ConfigFS tree and bind to the UDC.
pub fn setup(cfg: &Config, p: &PersonaDescriptors) -> Result<()> {
    let root: PathBuf = cfg.configfs_root.join(&cfg.gadget_name);

    if root.exists() {
        info!("existing gadget present, tearing down first");
        teardown(cfg)?;
    }

    mkdir(&root)?;

    // ── device-level identity ──
    write_str(&root.join("idVendor"), &format!("{:#06x}", p.id_vendor))?;
    write_str(&root.join("idProduct"), &format!("{:#06x}", p.id_product))?;
    write_str(&root.join("bcdDevice"), &format!("{:#06x}", p.bcd_device))?;
    write_str(&root.join("bcdUSB"), "0x0200")?;

    // ── English (0x409) strings ──
    let strings = root.join("strings/0x409");
    mkdir(&strings)?;
    write_str(&strings.join("manufacturer"), p.manufacturer)?;
    write_str(&strings.join("product"), p.product)?;
    write_str(&strings.join("serialnumber"), p.serial)?;

    // ── Configuration descriptor ──
    let config_dir = root.join("configs/c.1");
    mkdir(&config_dir)?;
    let config_strings = config_dir.join("strings/0x409");
    mkdir(&config_strings)?;
    write_str(&config_strings.join("configuration"), "AcursedKVM")?;
    write_str(&config_dir.join("MaxPower"), "500")?;

    // ── HID functions ──
    for f in &p.functions {
        add_function(&root, &config_dir, f)?;
    }

    // ── Bind to the UDC (USB Device Controller) — this enables the gadget ──
    write_str(&root.join("UDC"), &cfg.udc)?;
    info!(udc = %cfg.udc, "gadget bound");

    Ok(())
}

pub fn teardown(cfg: &Config) -> Result<()> {
    let root: PathBuf = cfg.configfs_root.join(&cfg.gadget_name);
    if !root.exists() {
        return Ok(());
    }

    // Unbind from UDC by writing an empty string.
    let _ = fs::write(root.join("UDC"), "");

    // Remove symlinks in configs/c.1/ — must remove BEFORE rmdir-ing the
    // function dirs.
    let configs_c1 = root.join("configs/c.1");
    if configs_c1.exists() {
        for entry in fs::read_dir(&configs_c1)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                fs::remove_file(entry.path())?;
            }
        }
        // strings subdirs first
        let s = configs_c1.join("strings/0x409");
        if s.exists() {
            fs::remove_dir(&s).ok();
        }
        if let Some(p) = s.parent() {
            fs::remove_dir(p).ok();
        }
        fs::remove_dir(&configs_c1).ok();
    }

    // Functions
    let functions = root.join("functions");
    if functions.exists() {
        for entry in fs::read_dir(&functions)? {
            fs::remove_dir(entry?.path()).ok();
        }
    }

    // Strings + the gadget itself
    let s = root.join("strings/0x409");
    fs::remove_dir(&s).ok();
    fs::remove_dir(s.parent().unwrap()).ok();
    fs::remove_dir(&root).ok();

    info!("gadget removed");
    Ok(())
}

fn add_function(root: &Path, config_dir: &Path, f: &HidFunction) -> Result<()> {
    let fn_dir = root.join("functions").join(f.name);
    mkdir(&fn_dir)?;
    write_str(&fn_dir.join("protocol"), &f.protocol.to_string())?;
    write_str(&fn_dir.join("subclass"), &f.subclass.to_string())?;
    write_str(&fn_dir.join("report_length"), &f.report_length.to_string())?;
    write_bytes(&fn_dir.join("report_desc"), f.report_desc)?;

    // Symlink the function into the config (this is what actually exposes it).
    let link = config_dir.join(f.name);
    if !link.exists() {
        unix::fs::symlink(&fn_dir, &link)
            .with_context(|| format!("symlink {} → {}", link.display(), fn_dir.display()))?;
    }
    info!(name = f.name, kind = ?f.kind, "added function");
    Ok(())
}
