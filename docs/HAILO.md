# Hailo AI HAT — optional enhancement (Pi 5 only)

The Aeon Magick Orb runs fully on a stock Pi without any accelerator. As an
**optional enhancement on the Pi 5**, you can add a **Hailo AI HAT** over the
Pi 5's PCIe lane to run vision models, OCR, and (on the newest board)
on-device LLMs — managed entirely from the **Hailo AI** super-app tile in the
web console.

> This is a **Pi-5-track** feature: PCIe + the Hailo runtime are Pi-5-only, so
> the Hailo super-app + runtime ship only in the **Pi 5 image** (`aeon-magick-pi5`),
> not the Pi 4 image. See [the two release tracks](#release-tracks) below.

## Supported boards

The super-app **auto-detects the chip** (`hailortcli fw-control identify`) and
installs the matching runtime — you don't pick the wrong one by hand:

| Board | Chip | Memory model | Runtime package | Notes |
|---|---|---|---|---|
| **AI HAT+ 2** (recommended) | **Hailo-10H** | **Dedicated 8 GB** on-board LPDDR4X (~5.5 GB usable) | `hailo-h10-all` | Holds models in its **own RAM — does not consume system RAM**. The only board that runs generative LLMs/VLMs. The recommended path. |
| AI HAT+ / AI Kit (older) | Hailo-8 / 8L | **System RAM** (the Pi's) | `hailo-all` | Vision inference (`.hef`) only — no on-device LLMs. Model weights live in the Pi's RAM. |

### Picking models per board

The super-app's model library shows a **fit indicator** computed against the
right budget for your board:

- **Hailo-10H (AI HAT+ 2):** budget = its dedicated **~5.5 GB**. Independent of
  the Pi's RAM — every fitting model is one-click deployable.
- **Hailo-8 / 8L (older HATs):** budget = the Pi's **available system RAM**.
  Because the model competes with everything else on the Pi, **an 8 GB Pi 5 is
  the minimum and a 16 GB Pi 5 is recommended** — and pick models that fit your
  free RAM. The super-app surfaces this as `mem_model: "system"` with the
  reduced budget so the fit indicator stays honest.

## Using it

1. Stack the HAT (the AI HAT+ needs a ~16 mm header for heatsink clearance) and
   boot the Pi 5.
2. Open the web console → the **Hailo AI** tile. It shows the detected chip.
3. Click **Install packages** — the super-app installs the chip-appropriate
   runtime (`hailo-h10-all` or `hailo-all`), streams progress, then asks you to
   reboot.
4. After the reboot the tile shows live **NPU utilization, temperature, power**
   and a **memory ledger** (used/free), plus the **model library**: one-click
   **deploy**, see what's loaded, and **unload** to swap.

Nothing is baked or auto-installed — the runtime install is a deliberate,
button-driven step, and the feature self-disables when no HAT is present.

## Release tracks

The Pi 4 and Pi 5 builds are **separate releases on separate tracks** (they are
not interchangeable images):

- **`aeon-magick-pi5`** — Pi 5. Includes the Pi-5-only hardware features: the
  CSI camera / HDMI-CSI capture, the USB-webcam gadget, **and this Hailo AI HAT
  support**.
- **`aeon-magick-pi4`** — Pi 4. The core KVM + see/act stack without the
  Pi-5-only accelerator/CSI features.

Maintainers select the track at bake time with `AEON_TARGET=pi5|pi4` (gates the
Pi-5-only installs) and `IMG_NAME=aeon-magick-pi5|aeon-magick-pi4`. See
`AGENTS.md` → "Rebuilding the image".
