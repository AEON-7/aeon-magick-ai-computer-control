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

    // ── Composite class signaling (IAD) ──
    //
    // bDeviceClass = 0xEF (Miscellaneous), bDeviceSubClass = 0x02
    // (Common Class), bDeviceProtocol = 0x01 (Interface Association
    // Descriptor). This tells the host "I am a multi-interface
    // composite device; bind drivers per-interface, not per-device."
    //
    // Critical for the apple-magic persona: without IAD, macOS sees
    // VID 0x05ac PID 0x0265 and tries to load AppleUSBMultitouch
    // against the WHOLE device. It then encounters three HID interfaces
    // (kbd + trackpad + consumer) that don't match its expectations and
    // the driver bails. With IAD, macOS evaluates each interface
    // independently — Apple-flavored keyboard, generic multi-touch HID
    // on the trackpad, generic consumer-control HID, plus the optional
    // cdc_ecm interface when USB ethernet is enabled.
    write_str(&root.join("bDeviceClass"), "0xEF")?;
    write_str(&root.join("bDeviceSubClass"), "0x02")?;
    write_str(&root.join("bDeviceProtocol"), "0x01")?;

    // ── English (0x409) strings ──
    let strings = root.join("strings/0x409");
    mkdir(&strings)?;
    write_str(&strings.join("manufacturer"), p.manufacturer)?;
    write_str(&strings.join("product"), p.product)?;
    write_str(&strings.join("serialnumber"), &p.serial)?;

    // ── Configuration descriptor ──
    let config_dir = root.join("configs/c.1");
    mkdir(&config_dir)?;
    let config_strings = config_dir.join("strings/0x409");
    mkdir(&config_strings)?;
    write_str(&config_strings.join("configuration"), "AeonMagick")?;

    // bmAttributes = 0xC0:
    //   bit 7 (0x80) — Reserved, must be 1
    //   bit 6 (0x40) — SELF-POWERED. We're claiming the Pi provides its own
    //                  power (which is true when running off the official PSU,
    //                  a power splitter, OR a Y-cable). This is a strategic
    //                  signal to host PCs that they don't need to supply
    //                  bus current for device operation — host may then be
    //                  more generous about Type-C current delivery.
    //   bit 5 (0x20) — REMOTE WAKEUP. Lets the gadget signal wake from
    //                  suspend (useful for HID waking the host).
    // bMaxPower is still declared (USB spec requires it even for
    // self-powered devices), but as 2mA — minimal — to underscore that
    // we don't expect bus power. (USB-PD or Type-C 3A delivery, both
    // happen at a layer below USB descriptors, are unaffected.)
    write_str(&config_dir.join("bmAttributes"), "0xC0")?;
    write_str(&config_dir.join("MaxPower"), "2")?;

    // ── HID functions ──
    for f in &p.functions {
        add_function(&root, &config_dir, f)?;
    }

    // ── Optional CDC ECM (USB ethernet) function — composite-dock mode ──
    if let Some(ecm) = &p.ecm {
        add_ecm_function(&root, &config_dir, ecm)?;
    }

    // ── Bind to the UDC (USB Device Controller) — this enables the gadget ──
    write_str(&root.join("UDC"), &cfg.udc)?;
    info!(udc = %cfg.udc, "gadget bound");

    Ok(())
}

fn add_ecm_function(
    root: &Path,
    config_dir: &Path,
    ecm: &crate::persona::EcmConfig,
) -> Result<()> {
    // CDC NCM instead of CDC ECM — NCM aggregates multiple ethernet
    // frames into single USB transfers, reducing per-frame overhead.
    // On Pi 4's USB-2.0-only OTG controller, ECM tops out around
    // 250 Mbps; NCM typically reaches 350-400 Mbps. macOS supports
    // NCM natively (kCDCSubclassNCM in IOUSBFamily), as do modern
    // Linux + Windows. v19 default.
    //
    // configfs path: functions/ncm.usb0/
    let fn_dir = root.join("functions/ncm.usb0");
    mkdir(&fn_dir)?;
    // host_addr = MAC the gadget tells the HOST's stack to use
    // dev_addr  = MAC for OUR end (Pi's usb0 interface)
    write_str(&fn_dir.join("host_addr"), &ecm.host_mac)?;
    write_str(&fn_dir.join("dev_addr"), &ecm.dev_mac)?;
    // Symlink into config — this is what actually attaches it.
    let link = config_dir.join("ncm.usb0");
    if !link.exists() {
        unix::fs::symlink(&fn_dir, &link)
            .with_context(|| format!("symlink {} → {}", link.display(), fn_dir.display()))?;
    }
    info!(host_mac = %ecm.host_mac, dev_mac = %ecm.dev_mac, "added CDC NCM (USB ethernet) function");
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

    // Per-interface label (iInterface string). Newer kernels expose this
    // via `functions/hid.<n>/strings/0x409/iInterface`. Older kernels may
    // not have the path — write is best-effort; failure isn't fatal.
    if let Some(label) = f.interface_label {
        let iface_strings = fn_dir.join("strings/0x409");
        if mkdir(&iface_strings).is_ok() {
            let _ = std::fs::write(iface_strings.join("iInterface"), label);
        }
    }

    // Symlink the function into the config (this is what actually exposes it).
    let link = config_dir.join(f.name);
    if !link.exists() {
        unix::fs::symlink(&fn_dir, &link)
            .with_context(|| format!("symlink {} → {}", link.display(), fn_dir.display()))?;
    }
    info!(name = f.name, kind = ?f.kind, label = ?f.interface_label, "added function");
    Ok(())
}
