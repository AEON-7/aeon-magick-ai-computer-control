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

    // ── Optional Mass Storage (USB CDROM) function — "dock with disk drive" ──
    // When /etc/aeon/storage.toml has an active ISO, attach it as a
    // bootable CDROM device alongside HID + ECM. The host sees a
    // multi-function USB-C dock with a disk drive — same composite
    // class, no extra negotiation. macOS's Option-key boot picker
    // recognises it as bootable install media.
    if let Some(ms) = &p.mass_storage {
        add_mass_storage_function(&root, &config_dir, ms)?;
    }

    // ── Optional UVC webcam function — Cam0 camera exposed as a USB webcam ──
    // Linked LAST (after HID/NCM/mass-storage) so existing /dev/hidg0..N
    // numbering doesn't shift. Must be fully wired before the UDC bind below.
    if let Some(uvc) = &p.uvc {
        add_uvc_function(&root, &config_dir, uvc)?;
    }

    // ── Bind to the UDC (USB Device Controller) — this enables the gadget ──
    write_str(&root.join("UDC"), &cfg.udc)?;
    info!(udc = %cfg.udc, "gadget bound");

    Ok(())
}

fn add_mass_storage_function(
    root: &Path,
    config_dir: &Path,
    ms: &crate::persona::MassStorageConfig,
) -> Result<()> {
    // ConfigFS layout for the g_mass_storage function:
    //   functions/mass_storage.0/
    //       stall=0          (USB stall on data underrun — disable for
    //                         compatibility with some host stacks)
    //       lun.0/
    //           file=<path>  (path to the ISO/IMG to expose; can be
    //                         empty at bind time and set later for
    //                         eject/insert semantics)
    //           cdrom=1      (claim CDROM-class; macOS boot-picker
    //                         only shows CDROM-class devices)
    //           removable=1  (let the host see eject/load as legal ops)
    //           ro=1         (read-only — ISO + safety)
    let fn_dir = root.join("functions/mass_storage.0");
    mkdir(&fn_dir)?;
    write_str(&fn_dir.join("stall"), "0")?;

    let lun = fn_dir.join("lun.0");
    mkdir(&lun)?;
    write_str(&lun.join("cdrom"), "1")?;
    write_str(&lun.join("removable"), "1")?;
    write_str(&lun.join("ro"), "1")?;
    // file LAST — host sees the disk appear as soon as the bind
    // completes, and a real file path here means "media loaded".
    write_str(&lun.join("file"), &ms.iso_path)?;

    let link = config_dir.join("mass_storage.0");
    if !link.exists() {
        unix::fs::symlink(&fn_dir, &link)
            .with_context(|| format!("symlink {} → {}", link.display(), fn_dir.display()))?;
    }
    info!(iso = %ms.iso_path, "added mass_storage (CDROM) function");
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

/// Create a configfs symlink if the link path doesn't already exist.
fn add_link(target: &Path, link_path: &Path) -> Result<()> {
    if !link_path.exists() {
        unix::fs::symlink(target, link_path)
            .with_context(|| format!("symlink {} → {}", link_path.display(), target.display()))?;
    }
    Ok(())
}

fn add_uvc_function(
    root: &Path,
    config_dir: &Path,
    uvc: &crate::persona::UvcConfig,
) -> Result<()> {
    // UVC (webcam) gadget function. The kernel's f_uvc auto-creates the
    // control/ + streaming/ groups (incl. their class/{fs,hs,ss} dirs and the
    // mjpeg/uncompressed format groups); we create the header + frame
    // INSTANCES and wire the class symlinks. After UDC bind a /dev/videoN
    // GADGET node appears, which the separate aeon-uvc (uvc-gadget) daemon
    // feeds from the Cam0 IMX477.
    //
    // CRITICAL: the entire tree — including the streaming header→format link —
    // must be wired BEFORE this function is symlinked into configs/c.1. An
    // incompletely described UVC function makes the WHOLE gadget fail to bind
    // at the UDC write with -EINVAL. MJPEG-only by design: advertising
    // multiple *formats* breaks macOS QuickTime/FaceTime; multiple frame
    // *sizes* within one format is fine.
    let f = root.join("functions/uvc.usb0");
    mkdir(&f)?;

    // Streaming endpoint knobs for dwc2 high-speed isochronous. maxburst is
    // SuperSpeed-only (0 on HS); interval=1 = one iso packet per microframe.
    write_str(&f.join("streaming_maxpacket"), &uvc.streaming_maxpacket.to_string())?;
    write_str(&f.join("streaming_interval"), "1")?;
    write_str(&f.join("streaming_maxburst"), "0")?;

    // ── CONTROL: header instance + per-speed class links ──
    mkdir(&f.join("control/header/h"))?;
    add_link(&f.join("control/header/h"), &f.join("control/class/fs/h"))?;
    add_link(&f.join("control/header/h"), &f.join("control/class/ss/h"))?;

    // ── STREAMING: one MJPEG format with two frame sizes ──
    // frame1 = the configured default (uvc.width×height); frame2 = a fixed
    // 640×480 fallback. Created frame1-then-frame2 AND named so a lexical sort
    // agrees, so bFrameIndex is deterministic (1 = default) on any kernel.
    let intervals = uvc
        .frame_intervals
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let m = f.join("streaming/mjpeg/m");

    let frame1 = m.join("frame1");
    mkdir(&frame1)?;
    write_str(&frame1.join("wWidth"), &uvc.width.to_string())?;
    write_str(&frame1.join("wHeight"), &uvc.height.to_string())?;
    write_str(
        &frame1.join("dwMaxVideoFrameBufferSize"),
        &(uvc.width as u32 * uvc.height as u32 * 2).to_string(),
    )?;
    write_str(&frame1.join("dwMinBitRate"), "29491200")?;
    write_str(&frame1.join("dwMaxBitRate"), "884736000")?;
    write_str(&frame1.join("dwFrameInterval"), &intervals)?;

    let frame2 = m.join("frame2");
    mkdir(&frame2)?;
    write_str(&frame2.join("wWidth"), "640")?;
    write_str(&frame2.join("wHeight"), "480")?;
    write_str(&frame2.join("dwMaxVideoFrameBufferSize"), &(640u32 * 480 * 2).to_string())?;
    write_str(&frame2.join("dwMinBitRate"), "18432000")?;
    write_str(&frame2.join("dwMaxBitRate"), "147456000")?;
    write_str(&frame2.join("dwFrameInterval"), &intervals)?;

    write_str(&m.join("bDefaultFrameIndex"), "1")?; // default to frame1

    // ── STREAMING: header instance, header→format link FIRST, then class links ──
    mkdir(&f.join("streaming/header/h"))?;
    add_link(&m, &f.join("streaming/header/h/m"))?; // header → MJPEG format
    add_link(&f.join("streaming/header/h"), &f.join("streaming/class/fs/h"))?;
    add_link(&f.join("streaming/header/h"), &f.join("streaming/class/hs/h"))?; // REQUIRED on HS dwc2
    add_link(&f.join("streaming/header/h"), &f.join("streaming/class/ss/h"))?;

    // ── attach to the config LAST (the enabling symlink) ──
    let link = config_dir.join("uvc.usb0");
    if !link.exists() {
        unix::fs::symlink(&f, &link)
            .with_context(|| format!("symlink {} → {}", link.display(), f.display()))?;
    }
    info!(w = uvc.width, h = uvc.height, "added UVC (webcam) function");
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

    // UVC function teardown (the generic functions loop below only does a
    // non-recursive rmdir, which FAILS on uvc.usb0's deep control/streaming
    // subtree — leaving a stale, attribute-locked function that breaks the
    // next persona switch, exactly like the HID-strings bug). Remove the
    // internal symlinks, then the instances deepest-first, then the function
    // dir (the kernel tears down its auto-created group dirs on that rmdir),
    // so the generic loop never sees uvc.usb0. The configs/c.1/uvc.usb0
    // symlink was already removed by the symlink loop above.
    let uvc = root.join("functions/uvc.usb0");
    if uvc.exists() {
        for l in [
            "streaming/class/fs/h",
            "streaming/class/hs/h",
            "streaming/class/ss/h",
            "streaming/header/h/m",
            "control/class/fs/h",
            "control/class/ss/h",
        ] {
            let _ = fs::remove_file(uvc.join(l));
        }
        for d in [
            "streaming/mjpeg/m/frame1",
            "streaming/mjpeg/m/frame2",
            "streaming/mjpeg/m",
            "streaming/header/h",
            "control/header/h",
        ] {
            let _ = fs::remove_dir(uvc.join(d));
        }
        if let Err(e) = fs::remove_dir(&uvc) {
            tracing::warn!(?e, "failed to remove uvc.usb0 function dir during teardown");
        }
    }

    // Functions. Each HID function dir may contain a nested strings/<lang>
    // subdir (the iInterface label written by add_function). `remove_dir` is
    // a non-recursive rmdir, so it FAILS on a non-empty function dir — and
    // because the failure was swallowed with `.ok()`, the stale function dir
    // survived teardown. That was the persona-switch bug: a leftover function
    // keeps its report_desc/report_length, which the kernel locks read-only
    // once the gadget has been bound, so the NEXT persona's setup write to
    // report_desc fails (EBUSY/EINVAL) and the switch errors out. First boot
    // worked (clean tree); the first switch broke. Remove nested strings dirs
    // first so the function dir is actually empty before we rmdir it.
    let functions = root.join("functions");
    if functions.exists() {
        for entry in fs::read_dir(&functions)? {
            let fdir = entry?.path();
            let fstrings = fdir.join("strings");
            if fstrings.exists() {
                if let Ok(langs) = fs::read_dir(&fstrings) {
                    for lang in langs.flatten() {
                        fs::remove_dir(lang.path()).ok();
                    }
                }
                fs::remove_dir(&fstrings).ok();
            }
            if let Err(e) = fs::remove_dir(&fdir) {
                // Surface it now instead of silently leaving a stale function
                // that will break the next persona switch.
                tracing::warn!(dir = %fdir.display(), ?e, "failed to remove gadget function dir during teardown");
            }
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
