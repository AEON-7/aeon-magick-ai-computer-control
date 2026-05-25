# Apple Magic Trackpad emulation — design notes

This is the highest-risk part of the project. macOS's gesture engine
(`Quartz Event Services`) only fires `kCGEventTabletPointer` / NSEvent
gesture events from devices that the OS recognizes as Apple-vendor
multi-touch. A Microsoft Precision Touchpad (PTP) will produce multi-touch
input events, but **macOS will not synthesize 3- or 4-finger swipe
gestures** from PTP reports — those gestures are conditioned on Apple's
proprietary "MultiTouch" service path.

## Two-phase plan

### Phase A — Precision Touchpad emulation (works on Windows + Linux now)

The PTP HID descriptor + report format is fully documented in Microsoft's
"Precision Touchpad" specification. Our `apple_magic` persona currently
ships a simplified single-contact PTP descriptor (see `persona.rs`). To get
multi-finger reports flowing on Windows + Linux targets:

- Expand `APPLE_TOUCHPAD_DESC` to declare 5 contact collections (one per
  finger), each with `Tip Switch`, `In Range`, `Contact ID`, `X`, `Y`.
- Add the PTP "Capabilities" feature report and a single "Configuration"
  feature that tells the host this is a precision touchpad.
- Send 1-byte `Contact Count` + N contact records per input report.

This is well-trodden. ~1 day of work once the rest of the device works.

### Phase B — Apple multi-touch emulation (the hard one)

To produce 3/4-finger swipe events on macOS, our device needs to:

1. Identify with Apple's VID (`0x05ac`) and a Magic-Trackpad-family PID
   (`0x0265` original, `0x0269` USB-C Magic Trackpad gen 2, etc.)
2. Send reports in Apple's proprietary multi-touch format (NOT PTP).
3. Pass the "fingerprint" check macOS's `MultitouchSupport.framework`
   does on first connect.

The Apple multi-touch report format has been partially reverse-engineered:

- [linux-magicmouse-hidraw](https://github.com/free5lot/hid-apple-patched)
- The Linux kernel's `hid-magicmouse.c` and `hid-magictrackpad2.c`
- `usbmon` captures of a real wired Magic Trackpad

The general report shape (8-byte header + 9-byte contact records):

```
struct mt_report {
    uint8_t  report_id;          // 0x29 (input report)
    uint8_t  timestamp[3];       // µs since device boot, little-endian
    uint8_t  button_state;       // bit 0 = primary click
    uint8_t  contact_count;
    uint8_t  reserved[2];
    struct {
        uint8_t  abs_x_lsb;
        uint8_t  abs_x_msb;       // 13-bit X (0..4096)
        uint8_t  abs_y_lsb;
        uint8_t  abs_y_msb;       // 13-bit Y (0..3072)
        uint8_t  touch_major;     // major axis of contact ellipse
        uint8_t  touch_minor;     // minor axis
        uint8_t  orientation;     // rotation of ellipse
        uint8_t  pressure;
        uint8_t  contact_id_state; // upper nibble = contact ID, lower = phase
    } contacts[];
};
```

…but the exact byte layout varies between Magic Trackpad generations and
across Apple firmware versions. **We need a usbmon trace from a real wired
Magic Trackpad** on the same macOS version we plan to target. Don't trust
documentation alone — capture, replay, iterate.

## How to capture a reference trace

On a Linux host (or macOS with a USB analyzer):

```bash
# Find the device
lsusb | grep Apple
# Watch reports
sudo modprobe usbmon
sudo cat /sys/kernel/debug/usb/usbmon/Nu  # N = bus number
```

Save 30 seconds of:
- Single-finger movement (cursor tracking)
- Two-finger scroll
- Two-finger pinch
- Three-finger swipe (Mission Control)
- Three-finger swipe down (App Exposé)
- Four-finger swipe (Spaces)
- Force click (pressure)

We need each gesture's report sequence to copy the timing + report ID
patterns. Without this, our emulation won't trip macOS's gesture
recognizers correctly.

## Build / test plan

1. Land Phase A (PTP multi-finger). Validate on Windows VM as smoke test.
2. Capture usbmon trace from a real Magic Trackpad against the macOS version
   we want to support. (May need multiple traces if Apple changes the
   protocol between macOS versions.)
3. Translate captured report formats into Rust structs (`personas/apple_mt.rs`).
4. Patch `persona::apple_magic` to use the new descriptor + report format.
5. Test 3/4-finger gestures end-to-end on the target Mac.
6. If macOS rejects (no gesture events), iterate on:
   - Descriptor's exact byte layout (off-by-one bytes can break recognition)
   - VID/PID + serial pattern
   - Report timestamp pacing (Apple's firmware paces at exactly 90 Hz)

## Realistic risk assessment

The PTP path is straightforward and will land cleanly.

The Apple-vendor path has **20-30% chance of working on first complete
iteration**, more like 60-70% after ~3 iteration cycles with hardware
traces. If macOS adds a check we can't replicate (e.g., a per-device HMAC
based on a key burned into Apple's MFI chip), there is no software-only
path; we'd be stuck on PTP.

In that case, the fallback is:

- Ship PTP-only Apple persona (gestures work on Windows + Linux targets).
- For macOS targets, supply a Karabiner config that maps PTP multi-finger
  events to the corresponding `CGEvent` synthesized gestures.

Document the Karabiner fallback path in the README so users have a clear
escape hatch.
