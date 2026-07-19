#!/bin/bash
# aeon-audio-init — bring the BrainCraft WM8960 codec up AUDIBLE at boot.
#
# WHY: the WM8960 powers up at chip defaults with the DAC->output-mixer routing
# switches OFF and the Speaker/Headphone analog volumes at 0, so a freshly
# flashed card has a dead-silent speaker even though the card enumerates fine
# (aplay -l lists it, levels "look" set, but the DAC net into the analog output
# mixer is open). alsa-restore only helps once a good state has been stored.
# This oneshot establishes that known-good baseline (idempotent) on every boot:
# it opens the DAC path into the output mixer, sets sane speaker/headphone/
# capture levels + the mic input path, and persists the state. It self-skips
# cleanly when no WM8960 card is present (bare Pi, or a USB-audio Orb).
set -u

# Find the WM8960 card index by name (mirrors supervisor audio.rs::pick_card).
CARD=""
while read -r idx name; do
    case "$name" in *wm8960*) CARD="$idx"; break;; esac
done < <(sed -nE 's/^[[:space:]]*([0-9]+)[[:space:]]+\[([^]]*)\].*/\1 \2/p' /proc/asound/cards 2>/dev/null)
[ -n "$CARD" ] || { echo "aeon-audio-init: no wm8960 card present; skipping"; exit 0; }

set_ctl() { amixer -c "$CARD" sset "$1" "$2" >/dev/null 2>&1 || true; }

# Output: open the DAC -> output-mixer DAPM routing (OFF by default — the piece
# that makes the codec dead-silent until flipped) then set the analog levels.
# BrainCraft has BOTH class-D speakers (JST) and a headphone jack — drive both.
set_ctl "Left Output Mixer PCM" on
set_ctl "Right Output Mixer PCM" on
set_ctl "Playback" 100%
set_ctl "Speaker" 90%
set_ctl "Headphone" 85%
set_ctl "Speaker AC" 5      # class-D boost gain (enum 0..5); 0 = very quiet
set_ctl "Speaker DC" 5

# Input (dual electret mics on BrainCraft → LINPUT1/RINPUT1 + MICBIAS via
# the wm8960-mic dtoverlay). Chip defaults leave boost gains at 0 (muted) and
# Capture PGA low — operators report the mics are unusable until gain is
# dialed way up. We open every BrainCraft-relevant path at near-max.
set_ctl "Capture" 100%
set_ctl "Capture" cap
set_ctl "ADC PCM" 100%
# LINPUT1/RINPUT1 = primary electret path (0..3 enum, 100% = +29 dB)
set_ctl "Left Input Boost Mixer LINPUT1" 100%
set_ctl "Right Input Boost Mixer RINPUT1" 100%
# LINPUT2/3 / RINPUT2/3 also routed by the overlay on some revs (0..7)
set_ctl "Left Input Boost Mixer LINPUT2" 100%
set_ctl "Right Input Boost Mixer RINPUT2" 100%
set_ctl "Left Input Boost Mixer LINPUT3" 100%
set_ctl "Right Input Boost Mixer RINPUT3" 100%
set_ctl "Left Input Mixer Boost" on
set_ctl "Right Input Mixer Boost" on
set_ctl "Left Boost Mixer LINPUT1" on
set_ctl "Right Boost Mixer RINPUT1" on
set_ctl "Left Boost Mixer LINPUT2" on
set_ctl "Right Boost Mixer RINPUT2" on
set_ctl "Left Boost Mixer LINPUT3" on
set_ctl "Right Boost Mixer RINPUT3" on
# ALC can lift quiet electrets; keep stereo + high ceiling
set_ctl "ALC Function" Stereo
set_ctl "ALC Max Gain" 7
set_ctl "ALC Target" 12
set_ctl "ALC Min Gain" 0
set_ctl "ADC High Pass Filter" on
# Stereo ADC: left mic → left, right mic → right (default).
set_ctl "ADC Data Output Select" "Left Data = Left ADC;  Right Data = Right ADC"

alsactl store >/dev/null 2>&1 || true
echo "aeon-audio-init: WM8960 card $CARD configured (spk+hp+mics, routing on)"
# Hint when capture path is the known Pi5/6.18 zero-sample bug.
if ! arecord -D "plughw:CARD=wm8960soundcard,DEV=0" -f S16_LE -r 16000 -c 1 -d 1 /tmp/.aeon-mic-probe.wav >/dev/null 2>&1; then
    echo "aeon-audio-init: note — arecord open failed (check dtoverlay=wm8960-mic)"
else
    # Peak sample — pure zeros ⇒ RP1 I2S RX regression on 6.18 kernels.
    if command -v python3 >/dev/null 2>&1; then
        python3 - <<'PY' 2>/dev/null && true
import wave, struct, sys
try:
    w = wave.open("/tmp/.aeon-mic-probe.wav")
    n = min(w.getnframes(), 16000)
    raw = w.readframes(n)
    if w.getsampwidth() == 2 and raw:
        s = struct.unpack("<" + str(len(raw)//2) + "h", raw)
        if max(abs(x) for x in s) == 0:
            print("aeon-audio-init: WARN capture is digital silence (Pi5/6.18 I2S RX; see wm8960-mic.dts)")
            sys.exit(0)
    print("aeon-audio-init: mic probe saw non-zero samples OK")
except Exception as e:
    print("aeon-audio-init: mic probe skipped:", e)
PY
    fi
    rm -f /tmp/.aeon-mic-probe.wav
fi
exit 0
