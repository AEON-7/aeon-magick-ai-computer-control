#!/usr/bin/env python3
"""Aeon Magick Orb — Sense HAT "alive" heartbeat.

A purple orb that glows at the centre, pulses like a heart (lub-dub), and sends
a ripple radiating outward on every beat — to give the Orb a sense of being
alive. Writes RGB565 frames straight to the RPi-Sense framebuffer, so it needs
no sense_hat library. Tunables are up top.
"""
import glob
import math
import signal
import sys
import time

RPI_SENSE_NAME = "RPi-Sense FB"  # the LED matrix; the overlay assigns it fb0 or fb1
PERIOD = 0.92            # seconds per heartbeat (~65 bpm, resting)
BRIGHTNESS = 0.62        # overall scale 0..1 — the LEDs are bright; keep it a glow
FPS = 45
CX, CY = 3.5, 3.5        # centre of the 8x8 grid
SIGMA = 2.25             # orb softness (bigger = larger, softer orb)

FB = None               # resolved at runtime by find_fb()


def find_fb():
    """Locate the RPi-Sense LED-matrix framebuffer by name (fb0 on a headless
    Orb, fb1 if another framebuffer is present)."""
    for namef in glob.glob("/sys/class/graphics/fb*/name"):
        try:
            with open(namef) as fh:
                if fh.read().strip() == RPI_SENSE_NAME:
                    return "/dev/" + namef.split("/")[-2]
        except OSError:
            pass
    return None


def beat(phase):
    """Heartbeat envelope over one PERIOD: a sharp 'lub' then a softer 'dub'."""
    lub = math.exp(-(phase ** 2) / (2 * 0.045 ** 2))
    dub = 0.55 * math.exp(-((phase - 0.17) ** 2) / (2 * 0.055 ** 2))
    return min(1.0, lub + dub)


def rgb565(r, g, b):
    r = 0 if r < 0 else 255 if r > 255 else int(r)
    g = 0 if g < 0 else 255 if g > 255 else int(g)
    b = 0 if b < 0 else 255 if b > 255 else int(b)
    return ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)


def render(t):
    phase = t % PERIOD
    bb = beat(phase)
    ripple_r = (phase / PERIOD) * 9.0           # ring expands centre -> out each beat
    ripple_env = math.exp(-phase / 0.33)        # ripple strongest just after the lub
    buf = bytearray(128)
    for y in range(8):
        for x in range(8):
            dx, dy = x - CX, y - CY
            d = math.sqrt(dx * dx + dy * dy)
            orb = math.exp(-(d * d) / (2 * SIGMA * SIGMA))
            ring = math.exp(-((d - ripple_r) ** 2) / (2 * 0.6 ** 2)) * ripple_env
            inten = max(0.0, min(1.0, orb * (0.20 + 0.80 * bb) + 0.85 * ring)) * BRIGHTNESS
            # purple, whitening slightly at the core on a strong beat (a spark of life)
            r = inten * 140 + bb * orb * 70 * BRIGHTNESS
            g = inten * 14 + bb * orb * 52 * BRIGHTNESS
            b = inten * 255
            v = rgb565(r, g, b)
            off = (y * 8 + x) * 2
            buf[off] = v & 0xFF
            buf[off + 1] = (v >> 8) & 0xFF
    return buf


def clear_and_exit(*_):
    try:
        if FB:
            with open(FB, "wb", buffering=0) as f:
                f.write(bytearray(128))   # blank the matrix on stop
    except OSError:
        pass
    sys.exit(0)


def main():
    global FB
    signal.signal(signal.SIGTERM, clear_and_exit)
    signal.signal(signal.SIGINT, clear_and_exit)
    # The rpi-sense framebuffer may appear a moment after boot; wait + resolve it.
    for _ in range(120):
        FB = find_fb()
        if FB:
            break
        time.sleep(1)
    if not FB:
        sys.exit("no RPi-Sense framebuffer — is the rpi-sense overlay loaded? (dtoverlay=rpi-sense)")
    dt = 1.0 / FPS
    t0 = time.monotonic()
    with open(FB, "wb", buffering=0) as f:
        while True:
            f.seek(0)
            f.write(render(time.monotonic() - t0))
            time.sleep(dt)


if __name__ == "__main__":
    main()
