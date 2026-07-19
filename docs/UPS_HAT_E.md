# Waveshare UPS HAT (E) — battery monitor + safe-shutdown

From **v104**, the aeon image ships battery/power-management support for the
**Waveshare UPS HAT (E)** (4× 21700 Li-ion, 5V/6A, bi-directional PD fast
charge, for Raspberry Pi 5). It's **plug-and-play** — no overlay or config.txt
change (the HAT is pure userspace I2C, and the image already enables `i2c_arm`
+ `i2c-dev`).

> ⚠️ **Hardware-in-the-loop.** Built against Waveshare's documented register map
> and their tested `ups.py`, but **not** verified on a physical HAT. Run the
> checklist below on the real device. The daemon self-disables (idles) if the
> HAT isn't present, so it's harmless on any Pi without it.

## What it does

`aeon-ups.service` runs `/usr/local/bin/aeon-ups` (a small Python daemon) that
every 2s talks to the HAT's MCU at **I2C `0x2D` (bus 1)** and:

1. **Publishes** the full power picture to `/run/aeon/ups.json` (atomic) — the
   supervisor relays it at **`GET /api/ups`**, so the dashboard and an AI agent
   can see charge state without anyone else touching the bus.
2. **Safe-shutdown:** if any 21700 cell drops below **3150 mV** and the pack
   isn't charging, it counts down and after **60s** of sustained low battery
   writes the HAT's "cut the Pi rail in 30s" command (`0x2D` reg `0x01` ← `0x55`)
   and runs `systemctl poweroff` — a clean halt before the cells are damaged or
   power is yanked.

Tunables (env vars on the unit):

| Env | Default | Meaning |
|---|---|---|
| `AEON_UPS_BUS` | `1` | I2C bus |
| `AEON_UPS_LOW_MV` | `3150` | per-cell mV floor |
| `AEON_UPS_GRACE_S` | `60` | sustained-low seconds before poweroff |
| `AEON_UPS_LOW_PCT` | `15` | fuel-gauge % must also be ≤ this (misread guard) |
| `AEON_UPS_MIN_DISCHARGE_MA` | `100` | pack must be discharging at least this hard (mA) |
| `AEON_UPS_AUTO_POWEROFF` | `1` | set `0` to **monitor only** (never write `0x55` / never `poweroff`) |

The trigger only fires when **all** of these agree on a self-consistent read:
on battery, percent ≤ `LOW_PCT`, min cell < `LOW_MV`, pack discharging
(≤ `-MIN_DISCHARGE_MA`), sustained for `GRACE_S`. Anything else is logged and
ignored — a single corrupt I2C cell read used to power the Orb off with a full
pack.

**Diagnostics that survive a hard cut** (Pi OS journals are volatile by default;
the image now overrides that to persistent):

| Path | What |
|---|---|
| `/var/log/aeon-ups-trend.log` | once-a-minute thr + load + ups.json |
| `/var/lib/aeon/ups-ring.jsonl` | last ~2 min of 2 s samples (fsynced) |
| `/var/lib/aeon/ups-last-poweroff.json` | breadcrumb written *before* any 0x55 cut |
| `journalctl -u aeon-ups` | ignored-misread + charge-stall + LOW lines |

## `GET /api/ups` fields

```json
{
  "present": true, "ok": true,
  "state": "discharging",            // fast-charging | charging | discharging | idle
  "charging": false, "on_battery": true,
  "percent": 87,
  "battery_mv": 15600, "battery_ma": -1850,   // mA <0 = discharging into the Pi
  "remaining_mah": 4350,
  "minutes_to_empty": 141, "minutes_to_full": null,
  "vbus_mv": 0, "vbus_ma": 0, "vbus_mw": 0,    // Type-C input
  "cells_mv": [3902, 3898, 3905, 3899], "min_cell_mv": 3898,
  "low_voltage_mv": 3150, "shutdown_pending_s": null
}
```
`{"present": false}` when no HAT / the daemon isn't running.

## Register map (MCU @ 0x2D, bus 1)

| Reg | Bytes | Meaning |
|---|---|---|
| `0x02` | 1 | state bits: `0x40` fast-charge · `0x80` charge · `0x20` discharge · else idle |
| `0x10` | 6 | VBUS voltage / current / power (mV·mA·mW, LE u16) |
| `0x20` | 12 | batt mV · **signed** mA · percent · remaining mAh · min-to-empty · min-to-full |
| `0x30` | 8 | 4× cell voltages (mV, LE u16) |
| `0x01` ← `0x55` | — | MCU cuts the Pi rail 30s later (used by the safe-shutdown) |

## Post-flash verification checklist (run on the real Pi 5 + HAT)

1. `sudo i2cdetect -y 1` → shows a device at **`2d`**. (Empty → check the HAT is
   seated and `dtparam=i2c_arm=on` is in `/boot/firmware/config.txt`.)
2. `systemctl status aeon-ups` → `active (running)`; journal shows battery lines.
3. `cat /run/aeon/ups.json` → real values (percent, cells_mv ≈ 3.6–4.2 V each).
4. `curl -s .../api/ups | jq` → same JSON via the supervisor.
5. Unplug Type-C → `state` flips to `discharging`, `on_battery: true`, and
   `minutes_to_empty` populates.
6. **Don't trigger a real low-battery shutdown to test** unless you mean it. To
   sanity-check the path safely, temporarily set `AEON_UPS_LOW_MV` above the
   current cell voltage in the unit, `systemctl daemon-reload && restart`, watch
   the journal count down `poweroff in …s`, then **restore it before 60s** and
   restart. (At the real threshold it will halt the Pi.)

## Notes / on-device knobs

- **Bus:** the 40-pin user I2C is `i2c-1` on the Pi 5 (Waveshare confirms
  `i2cdetect -y 1`). If yours differs, set `AEON_UPS_BUS` on the unit.
- **Manual poweroff on battery:** the HAT keeps a *halted* Pi powered (slow
  drain) unless told otherwise. The daemon handles the low-battery case; to power
  off cleanly on battery yourself, run `i2cset -y 1 0x2d 0x01 0x55` just before
  `poweroff` (a shutdown hook was deliberately *not* auto-installed — telling it
  apart from a reboot is error-prone and a wrong guess could cut power mid-reboot).
- **A/B-test unexplained rail-cuts:** if the Orb still drops while the pack is
  healthy, set `AEON_UPS_AUTO_POWEROFF=0` on the unit (or drop a drop-in),
  `systemctl daemon-reload && systemctl restart aeon-ups`. If cutouts **stop**,
  software was still cutting the rail; if they **continue**, look at hardware
  (UPS Type-C PSU must be PD fast-charge into the **HAT's** Type-C, not the Pi's;
  pogo-pin seating; HAT power switch; `PSU_MAX_CURRENT=5000` in the Pi 5 EEPROM).
- **Charge-path stall:** the daemon logs when VBUS shows ~15 V PD but almost no
  VBUS current while the pack is discharging. That's "on battery despite a wall
  brick" — fix the PSU/cable before blaming the OS.
- Source of truth: Waveshare `UPS_HAT_E.zip` → `ups.py`.
