#!/usr/bin/env python3
"""aeon-ups — Waveshare UPS HAT (E) monitor + low-battery safe-shutdown.

The UPS HAT (E) exposes an MCU at I2C address 0x2D (bus 1) that reports the
whole power picture; we never touch the cells directly. Register map (verified
against Waveshare's UPS_HAT_E demo `ups.py`):

  0x02 (1B)  state bits: 0x40 fast-charging, 0x80 charging, 0x20 discharging
  0x10 (6B)  VBUS (Type-C input):  voltage / current / power  (mV/mA/mW, LE u16)
  0x20 (12B) battery: voltage(mV), current(mA, SIGNED), percent(%),
             remaining(mAh), run-to-empty(min), avg-to-full(min)
  0x30 (8B)  four 21700 cell voltages (mV, LE u16)
  0x01 <- 0x55  tell the MCU to cut the Pi rail 30s later (clean power-off)

What this daemon does, every POLL_S seconds:
  * read the registers and publish /run/aeon/ups.json (atomic) for the
    supervisor's GET /api/ups + the dashboard + the AI agent;
  * if the Orb is running ON BATTERY and the pack is genuinely low (fuel-gauge %
    low AND a cell below LOW_VOL, from a SELF-CONSISTENT read), count down and,
    after GRACE_S, write 0x55 and `systemctl poweroff` — a clean shutdown before
    the cells are damaged / power is yanked.

The low-battery trigger is HARDENED against spurious I2C misreads (the shared bus
also carries the audio codec + other HATs): a reading that is inconsistent
(pack ≠ Σ cells, or a cell out of Li-ion range), or that claims "low" while on
external power or at a healthy %, is IGNORED — it's bad data, not a dying battery.
Without this, one corrupt read of a single cell register could power the Orb off
with a full battery.

Self-disables (idles) if 0x2D isn't on the bus, so the image is harmless on a
Pi without the HAT. Tunables come from the environment (set in the unit):
  AEON_UPS_BUS (default 1), AEON_UPS_LOW_MV (3150), AEON_UPS_GRACE_S (60),
  AEON_UPS_LOW_PCT (15).
"""
import json
import os
import sys
import time

ADDR = 0x2D
BUS = int(os.environ.get("AEON_UPS_BUS", "1"))
LOW_VOL = int(os.environ.get("AEON_UPS_LOW_MV", "3150"))   # per-cell mV
GRACE_S = int(os.environ.get("AEON_UPS_GRACE_S", "60"))    # sustained-low before poweroff
LOW_PCT = int(os.environ.get("AEON_UPS_LOW_PCT", "15"))    # fuel-gauge % floor (misread guard)
POLL_S = 2
OUT = "/run/aeon/ups.json"

try:
    import smbus
except ImportError:
    sys.stderr.write("aeon-ups: python3-smbus not installed; exiting\n")
    sys.exit(0)


def u16(d, i):
    return d[i] | (d[i + 1] << 8)


def s16(v):
    return v - 0x10000 if v > 0x7FFF else v


def write_json(obj):
    tmp = OUT + ".tmp"
    try:
        with open(tmp, "w") as f:
            json.dump(obj, f)
        os.replace(tmp, OUT)
    except OSError as e:
        sys.stderr.write(f"aeon-ups: write {OUT}: {e}\n")


def poweroff(bus):
    # Best-effort: re-confirm the device is really there before pulling the
    # plug (mirrors Waveshare's pre-shutdown re-check), then tell the MCU to
    # cut the rail 30s from now and halt immediately.
    try:
        bus.read_byte_data(ADDR, 0x02)
    except OSError:
        sys.stderr.write("aeon-ups: 0x2d vanished before shutdown — aborting poweroff\n")
        return
    try:
        bus.write_byte_data(ADDR, 0x01, 0x55)
    except OSError as e:
        sys.stderr.write(f"aeon-ups: 0x55 write failed: {e}\n")
    sys.stderr.write("aeon-ups: LOW BATTERY — system poweroff now\n")
    os.system("systemctl poweroff")


def main():
    try:
        bus = smbus.SMBus(BUS)
    except (OSError, FileNotFoundError) as e:
        # Transient at boot (i2c-dev not ready yet) → exit non-zero so systemd
        # restarts us. i2c_arm is always enabled in this image, so this resolves.
        sys.stderr.write(f"aeon-ups: cannot open i2c bus {BUS}: {e}; will retry\n")
        sys.exit(1)

    # No HAT attached → idle quietly so we're harmless on a bare Pi.
    try:
        bus.read_byte_data(ADDR, 0x02)
    except OSError:
        write_json({"present": False})
        sys.stderr.write("aeon-ups: no UPS HAT (E) at 0x2d; idling\n")
        # Don't churn the bus; re-probe occasionally in case it appears.
        while True:
            time.sleep(60)
            try:
                bus.read_byte_data(ADDR, 0x02)
                break  # appeared — fall through to the monitor loop
            except OSError:
                continue

    low = 0
    while True:
        try:
            st = bus.read_byte_data(ADDR, 0x02)
            vbus = bus.read_i2c_block_data(ADDR, 0x10, 0x06)
            bat = bus.read_i2c_block_data(ADDR, 0x20, 0x0C)
            cells = bus.read_i2c_block_data(ADDR, 0x30, 0x08)
        except OSError as e:
            sys.stderr.write(f"aeon-ups: i2c read failed: {e}\n")
            write_json({"present": True, "ok": False, "error": str(e)})
            time.sleep(POLL_S)
            continue

        fast = bool(st & 0x40)
        charging = bool(st & 0x80) or fast
        discharging = bool(st & 0x20)
        state = ("fast-charging" if fast else "charging" if charging
                 else "discharging" if discharging else "idle")

        vbus_mv = u16(vbus, 0)
        bat_mv = u16(bat, 0)
        bat_ma = s16(u16(bat, 2))           # <0 = discharging into the Pi
        percent = u16(bat, 4)
        remaining_mah = u16(bat, 6)
        to_empty = u16(bat, 8)
        to_full = u16(bat, 10)
        cell_mv = [u16(cells, i) for i in (0, 2, 4, 6)]
        # On battery = no meaningful Type-C input feeding us.
        on_battery = vbus_mv < 4000 and not charging
        min_cell = min(cell_mv) if cell_mv else 0

        write_json({
            "present": True, "ok": True,
            "state": state, "charging": charging, "fast_charging": fast,
            "on_battery": on_battery,
            "percent": percent,
            "battery_mv": bat_mv, "battery_ma": bat_ma,
            "remaining_mah": remaining_mah,
            "minutes_to_empty": to_empty if bat_ma < 0 else None,
            "minutes_to_full": to_full if charging else None,
            "vbus_mv": vbus_mv, "vbus_ma": u16(vbus, 2), "vbus_mw": u16(vbus, 4),
            "cells_mv": cell_mv, "min_cell_mv": min_cell,
            "low_voltage_mv": LOW_VOL,
            "shutdown_pending_s": (GRACE_S - POLL_S * low) if low else None,
        })

        # Low-battery safe-shutdown — HARDENED against spurious I2C misreads that
        # were powering the Orb off with a full battery. Only act when the reading
        # is SELF-CONSISTENT (real data, not garbage) AND every independent signal
        # agrees the battery is genuinely dying on battery power:
        #   * on_battery — no external input (you can't over-discharge while it's
        #     charging, so a "low" reading on AC is necessarily bad data);
        #   * fuel-gauge percent is low (a single corrupt cell read won't move it);
        #   * a cell is actually below the floor.
        pack_sum = sum(cell_mv)
        consistent = (
            all(2500 <= v <= 4400 for v in cell_mv)                 # cells in Li-ion range
            and 0 <= percent <= 100
            and (pack_sum == 0 or abs(bat_mv - pack_sum) <= 1500)   # pack ≈ Σ cells
        )
        if consistent and on_battery and percent <= LOW_PCT and min_cell < LOW_VOL:
            low += 1
            sys.stderr.write(
                f"aeon-ups: LOW on battery — pct={percent}% min_cell={min_cell}mV "
                f"pack={bat_mv}mV — poweroff in {GRACE_S - POLL_S * low}s unless powered\n")
            if POLL_S * low >= GRACE_S:
                poweroff(bus)
                return
        else:
            # Log WHY we're not shutting down when a cell looked low — this is the
            # smoking gun for the spurious-shutdown bug (an ignored misread).
            if any(v < LOW_VOL for v in cell_mv):
                why = ("inconsistent read" if not consistent
                       else "on external power" if not on_battery
                       else f"fuel gauge {percent}% not low")
                sys.stderr.write(
                    f"aeon-ups: ignoring low cell — {why} "
                    f"(cells={cell_mv} pack={bat_mv}mV pct={percent}% on_battery={on_battery})\n")
            low = 0

        time.sleep(POLL_S)


if __name__ == "__main__":
    main()
