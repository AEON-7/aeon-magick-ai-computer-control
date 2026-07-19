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
  * keep a short ring of recent samples on disk so a hard power-cut still leaves
    a post-mortem trail (journald on Pi OS defaults to volatile storage);
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

Additional guards (post-hardening, still seeing unexplained rail-cuts at ~97%):
  * bat_ma must be strongly negative (actually discharging into the Pi);
  * AEON_UPS_AUTO_POWEROFF=0 disables the 0x55/`poweroff` path entirely (monitor
    + publish only) — use this to A/B-test whether software is still cutting the
    rail; set back to 1 once you're confident the pack needs protection.

Self-disables (idles) if 0x2D isn't on the bus, so the image is harmless on a
Pi without the HAT. Tunables come from the environment (set in the unit):
  AEON_UPS_BUS (default 1), AEON_UPS_LOW_MV (3150), AEON_UPS_GRACE_S (60),
  AEON_UPS_LOW_PCT (15), AEON_UPS_AUTO_POWEROFF (1), AEON_UPS_MIN_DISCHARGE_MA (100).
"""
import json
import os
import sys
import time
from collections import deque

ADDR = 0x2D
BUS = int(os.environ.get("AEON_UPS_BUS", "1"))
LOW_VOL = int(os.environ.get("AEON_UPS_LOW_MV", "3150"))   # per-cell mV
GRACE_S = int(os.environ.get("AEON_UPS_GRACE_S", "60"))    # sustained-low before poweroff
LOW_PCT = int(os.environ.get("AEON_UPS_LOW_PCT", "15"))    # fuel-gauge % floor (misread guard)
# Require the pack to be *actually discharging* into the Pi (mA < -this). Near-zero
# current is the normal "battery full / trickle" state and was part of the old
# false-positive signature (bat_ma < 50 + one corrupt cell).
MIN_DISCHARGE_MA = int(os.environ.get("AEON_UPS_MIN_DISCHARGE_MA", "100"))
# Kill-switch for the rail-cut path. "0"/"false"/"no"/"off" → monitor only.
_AUTO_RAW = os.environ.get("AEON_UPS_AUTO_POWEROFF", "1").strip().lower()
AUTO_POWEROFF = _AUTO_RAW not in ("0", "false", "no", "off")
POLL_S = 2
OUT = "/run/aeon/ups.json"
# Persistent breadcrumbs — survive the next hard power-cut (unlike volatile journal).
RING_PATH = "/var/lib/aeon/ups-ring.jsonl"
LAST_POWEROFF_PATH = "/var/lib/aeon/ups-last-poweroff.json"
RING_MAX = 60  # ~2 min of samples at POLL_S=2


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
        os.makedirs(os.path.dirname(OUT), exist_ok=True)
        with open(tmp, "w") as f:
            json.dump(obj, f)
        os.replace(tmp, OUT)
    except OSError as e:
        sys.stderr.write(f"aeon-ups: write {OUT}: {e}\n")


def append_ring(sample):
    """Append one sample to a capped on-disk ring (best-effort, fsynced)."""
    try:
        os.makedirs(os.path.dirname(RING_PATH), exist_ok=True)
        line = json.dumps(sample, separators=(",", ":")) + "\n"
        # Rewrite whole file from in-memory deque — small (RING_MAX lines).
        # Caller keeps the deque; we just flush it.
        return line
    except OSError as e:
        sys.stderr.write(f"aeon-ups: ring prepare: {e}\n")
        return None


def flush_ring(lines):
    try:
        os.makedirs(os.path.dirname(RING_PATH), exist_ok=True)
        tmp = RING_PATH + ".tmp"
        with open(tmp, "w") as f:
            f.writelines(lines)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, RING_PATH)
    except OSError as e:
        sys.stderr.write(f"aeon-ups: ring flush: {e}\n")


def write_poweroff_breadcrumb(sample, reason):
    payload = {
        "ts": time.time(),
        "ts_iso": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "reason": reason,
        "auto_poweroff": AUTO_POWEROFF,
        "sample": sample,
    }
    try:
        os.makedirs(os.path.dirname(LAST_POWEROFF_PATH), exist_ok=True)
        tmp = LAST_POWEROFF_PATH + ".tmp"
        with open(tmp, "w") as f:
            json.dump(payload, f, indent=2)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, LAST_POWEROFF_PATH)
    except OSError as e:
        sys.stderr.write(f"aeon-ups: breadcrumb write failed: {e}\n")


def poweroff(bus, sample):
    # Best-effort: re-confirm the device is really there before pulling the
    # plug (mirrors Waveshare's pre-shutdown re-check), then tell the MCU to
    # cut the rail 30s from now and halt immediately.
    write_poweroff_breadcrumb(sample, "low_battery_grace_elapsed")
    if not AUTO_POWEROFF:
        sys.stderr.write(
            "aeon-ups: LOW BATTERY would poweroff, but AEON_UPS_AUTO_POWEROFF=0 "
            "— monitor-only; NOT cutting the rail\n")
        return
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
    # Sync disks so the breadcrumb + ring survive the cut.
    try:
        os.sync()
    except Exception:
        pass
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

    sys.stderr.write(
        f"aeon-ups: monitoring (auto_poweroff={int(AUTO_POWEROFF)} "
        f"low_mv={LOW_VOL} low_pct={LOW_PCT} grace_s={GRACE_S} "
        f"min_discharge_ma={MIN_DISCHARGE_MA})\n")

    low = 0
    stall_logged = 0
    ring = deque(maxlen=RING_MAX)
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
        vbus_ma = u16(vbus, 2)
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

        # Charge-path stall: PD voltage present but almost no VBUS current while
        # the pack is discharging into the Pi. Seen for ~10+ min stretches before
        # several hard power-loss events on the Pi 5 Orb. Log periodically.
        charge_stall = (
            vbus_mv >= 10000
            and vbus_ma < 50
            and bat_ma < -100
        )

        sample = {
            "present": True, "ok": True,
            "state": state, "charging": charging, "fast_charging": fast,
            "on_battery": on_battery,
            "percent": percent,
            "battery_mv": bat_mv, "battery_ma": bat_ma,
            "remaining_mah": remaining_mah,
            "minutes_to_empty": to_empty if bat_ma < 0 else None,
            "minutes_to_full": to_full if charging else None,
            "vbus_mv": vbus_mv, "vbus_ma": vbus_ma, "vbus_mw": u16(vbus, 4),
            "cells_mv": cell_mv, "min_cell_mv": min_cell,
            "low_voltage_mv": LOW_VOL,
            "shutdown_pending_s": (GRACE_S - POLL_S * low) if low else None,
            "auto_poweroff": AUTO_POWEROFF,
            "charge_stall": charge_stall,
            "ts": time.time(),
        }
        write_json(sample)

        # Persistent ring for post-mortem (fsynced every ~30 s or on interesting events).
        ring.append(json.dumps(sample, separators=(",", ":")) + "\n")
        interesting = charge_stall or low > 0 or (percent is not None and percent <= LOW_PCT)
        if interesting or (int(time.time()) // 30) != stall_logged:
            flush_ring(list(ring))
            if not interesting:
                stall_logged = int(time.time()) // 30

        if charge_stall and (int(time.time()) // 60) != getattr(main, "_stall_min", -1):
            main._stall_min = int(time.time()) // 60
            sys.stderr.write(
                f"aeon-ups: charge-path stall — VBUS {vbus_mv}mV but only "
                f"{vbus_ma}mA while pack discharges at {bat_ma}mA "
                f"(pct={percent}% cells={cell_mv}). Pi is on battery despite "
                f"PD voltage present; check UPS Type-C PSU / cable.\n")

        # Low-battery safe-shutdown — HARDENED against spurious I2C misreads that
        # were powering the Orb off with a full battery. Only act when the reading
        # is SELF-CONSISTENT (real data, not garbage) AND every independent signal
        # agrees the battery is genuinely dying on battery power:
        #   * on_battery — no external input;
        #   * fuel-gauge percent is low (a single corrupt cell read won't move it);
        #   * a cell is actually below the floor;
        #   * pack is actually discharging into the Pi (not "full / idle" near 0 mA).
        pack_sum = sum(cell_mv)
        consistent = (
            all(2500 <= v <= 4400 for v in cell_mv)                 # cells in Li-ion range
            and 0 <= percent <= 100
            and (pack_sum == 0 or abs(bat_mv - pack_sum) <= 1500)   # pack ≈ Σ cells
        )
        really_discharging = bat_ma <= -MIN_DISCHARGE_MA
        if (consistent and on_battery and percent <= LOW_PCT
                and min_cell < LOW_VOL and really_discharging):
            low += 1
            sys.stderr.write(
                f"aeon-ups: LOW on battery — pct={percent}% min_cell={min_cell}mV "
                f"pack={bat_mv}mV bat_ma={bat_ma} — poweroff in "
                f"{GRACE_S - POLL_S * low}s unless powered "
                f"(auto_poweroff={int(AUTO_POWEROFF)})\n")
            flush_ring(list(ring))
            if POLL_S * low >= GRACE_S:
                poweroff(bus, sample)
                # If auto-poweroff is off we stay in the loop; reset the counter
                # so we don't spam every 2s forever — re-arm after grace again.
                if not AUTO_POWEROFF:
                    low = 0
                else:
                    return
        else:
            # Log WHY we're not shutting down when a cell looked low — this is the
            # smoking gun for the spurious-shutdown bug (an ignored misread).
            if any(v < LOW_VOL for v in cell_mv):
                if not consistent:
                    why = "inconsistent read"
                elif not on_battery:
                    why = "on external power"
                elif percent > LOW_PCT:
                    why = f"fuel gauge {percent}% not low"
                elif not really_discharging:
                    why = f"not discharging (bat_ma={bat_ma}, need ≤{-MIN_DISCHARGE_MA})"
                else:
                    why = "unknown"
                sys.stderr.write(
                    f"aeon-ups: ignoring low cell — {why} "
                    f"(cells={cell_mv} pack={bat_mv}mV pct={percent}% "
                    f"on_battery={on_battery} bat_ma={bat_ma})\n")
            low = 0

        time.sleep(POLL_S)


if __name__ == "__main__":
    main()
