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
set_ctl "Left Output Mixer PCM" on
set_ctl "Right Output Mixer PCM" on
set_ctl "Playback" 100%
set_ctl "Speaker" 90%
set_ctl "Headphone" 80%
set_ctl "Speaker AC" 5      # class-D boost gain (enum 0..5); 0 = very quiet
set_ctl "Speaker DC" 5

# Input (mic): the input-boost-mixer GAIN defaults to 0 (muted), so the ADC
# reads silence even with the connect switches on. Un-mute it + wire LINPUT1/
# RINPUT1 in. (A codec-config baseline; whether a given HAT's mic is physically
# wired to these inputs is a hardware matter this can't change.)
set_ctl "Capture" 80%
set_ctl "Left Input Boost Mixer LINPUT1" 100%
set_ctl "Right Input Boost Mixer RINPUT1" 100%
set_ctl "Left Input Mixer Boost" on
set_ctl "Right Input Mixer Boost" on
set_ctl "Left Boost Mixer LINPUT1" on
set_ctl "Right Boost Mixer RINPUT1" on
set_ctl "ADC High Pass Filter" on

alsactl store >/dev/null 2>&1 || true
echo "aeon-audio-init: WM8960 card $CARD configured (output routing on, levels set)"
exit 0
