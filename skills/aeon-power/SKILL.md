---
name: aeon-power
description: >
  Check whether an AEON Magick Orb is on wall power or running on its battery
  backup (a Waveshare UPS HAT), and how much charge is left. Read-only. Load
  this before any long, unattended operation on an Orb so you know it won't
  power off mid-task and leave you blind. Returns {present:false} (not an error)
  on an Orb with no UPS. Per-Orb battery state is also summarized in the
  aeon-fleet roster, so for many Orbs read the roster instead of polling each.
---
# aeon-power

Is this Orb on battery, and is it about to die? An Orb can run off a **Waveshare
UPS HAT (E)** stacked on the Pi. `GET /api/ups` relays the live battery picture
so you know whether the device you're operating is on wall power or about to
power off. Read-tier — a plain GET, no writes.

> **Credentials.** Auth = `Authorization: Bearer $AEON_TOKEN` against
> `https://$AEON_HOST` (self-signed → `curl -k`). The token is never in this
> skill — `export AEON_TOKEN=<your token>` (e.g. from your password manager) or
> load `~/.openclaw/aeon-magick.env` (`set -a; . ~/.openclaw/aeon-magick.env; set +a`).
> A `401` means the token is unset/wrong — fix that, don't switch tools.

## ups — battery state

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/ups"
```

The `aeon-ups` daemon owns the HAT's I2C bus and publishes the power picture to
`/run/aeon/ups.json` every 2 s; this endpoint relays that file.

```json
{
  "present": true,
  "charge": 84,
  "on_battery": false,
  "charging": true,
  "voltage": 16.4,
  "minutes_to_empty": null
}
```

| Field | Meaning |
|---|---|
| `present` | HAT detected + daemon running. **`false` = no UPS on this Orb** — power is whatever the wall gives it. |
| `charge` | battery state-of-charge, 0–100 %. |
| `on_battery` | `true` = wall/USB-C input is gone, running on the cells **right now**. |
| `charging` | input present and topping the cells up. |
| `minutes_to_empty` | estimated runtime left when `on_battery`; `null` while charging / on wall power. |

Per-cell voltages / VBUS / current may also appear — informational. The four
that matter: `present`, `charge`, `on_battery`, `minutes_to_empty`.

`{"present": false}` is the **normal** answer on an Orb with no UPS HAT — not an
error. Don't treat a missing battery as a fault; it just means power-state is
unmonitored there.

## when to care

Check `ups` **before any long, unattended operation** (an OS install, a slow
download, a multi-step macro you won't be watching):

- `on_battery: true` + low/falling `charge` → the Orb may shut down mid-task.
  The controlled machine keeps running, but you lose your eyes and hands on it
  the moment the Pi dies.
- `present: false` → no battery backstop at all; a power blip drops the Orb
  hard. Same caution for unattended work.
- Pair with a snapshot diagnosis: **snapshot frozen / stream dropped while
  `on_battery: true` and `charge` low → the Orb is likely powering down**, not a
  capture problem.

For a fleet, every peer's `ups` summary is already in
`GET /api/fleet/roster` (see the `aeon-fleet` skill) — spot a low Orb without
polling each one.

No write surface here — purely an observability read. Pi power itself
(`/api/system/pi-reboot`, `/pi-poweroff`) is a separate **admin-only**
capability, not an agent action. No MCP tool — REST-only.
