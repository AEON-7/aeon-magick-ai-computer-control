# input — drive the keyboard, mouse & trackpad

The **act** half of see+act. Every op is a single atomic HTTPS POST — press →
release happens server-side, so a dropped packet can never leave a key or
button stuck. All calls assume `https://${AEON_HOST}/` with HTTP Basic
`$AEON_USER:$AEON_PASSWD`. There is intentionally **no `key_down`/`key_up`
split** — that's the design lesson inherited from `cursed-hid`.

## type — emit a string

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"text": "Hello, world."}' \
    "https://$AEON_HOST/api/hid/type"
```

Press → release per character, paced on-device. Unmappable characters (emoji,
smart quotes) are silently skipped; the response includes `typed` + `skipped`
counters so you can detect data loss. To enter text into a field: focus it
(move + click) first, then `type`.

## key — a chord (Cmd+Space, Ctrl+C, F11, …)

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"keys": ["GUI","SPACE"], "hold_ms": 30}' \
    "https://$AEON_HOST/api/hid/key"
```

Recognized names: `CTRL ALT SHIFT GUI/CMD/WIN ENTER TAB ESC SPACE BACKSPACE
DELETE HOME END PAGEUP PAGEDOWN UP DOWN LEFT RIGHT F1..F12`, plus single ASCII
characters. The whole chord is pressed → held `hold_ms` → released as one
atomic op.

## click — left/right/middle, single/double/triple

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"button": "left", "count": 1}' \
    "https://$AEON_HOST/api/hid/click"
```

Press → release at the **current** cursor position. `button` is `left`,
`right`, or `middle`; `count` is 1/2/3 for single/double/triple. No
half-pressed-button failure mode is possible.

## move — relative cursor delta

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"dx": 100, "dy": -40}' \
    "https://$AEON_HOST/api/hid/move"
```

Boot-mouse semantics: relative deltas in HID int8 range (−127..127). For larger
or smoother moves, split client-side into multiple calls — the device
deliberately does **not** auto-segment, so the agent stays in charge of the
path and a dropped packet only drops one segment, not a whole gesture.

## move_abs / click_at — absolute point (recommended for agents)

With the `generic-absolute` persona, give a point as a **fraction of the
screen** (`(0,0)` top-left … `(1,1)` bottom-right) and the cursor lands there
exactly — no relative-acceleration drift. Compute it from a snapshot:
`x = pixel_x / frame_width`, `y = pixel_y / frame_height`.

```bash
# move (and optionally click) at an absolute point
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"x": 0.5, "y": 0.5, "buttons": 0}' \
    "https://$AEON_HOST/api/hid/move_abs"
```

Body: `x`, `y` (0..1, required) plus optional `buttons` (bitmask: 1=left,
2=right, 4=middle) and `wheel` (signed, ±127). To **click** a point, send it
once with the button bit set, then again with `buttons:0` at the same coords.
Over MCP this is `click_at` / `move_pointer` / `drag` — no math needed, just
pass `x`,`y`. **This is the reliable way for an agent to hit a specific
element.**

## drag — hold a button across a move

`move_abs` carries the button mask, so press → move → release drags:

```bash
# drag from (0.2,0.3) to (0.6,0.7) with the left button held
for step in '{"x":0.2,"y":0.3,"buttons":1}' \
            '{"x":0.6,"y":0.7,"buttons":1}' \
            '{"x":0.6,"y":0.7,"buttons":0}'; do
  curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST -H "Content-Type: application/json" \
      -d "$step" "https://$AEON_HOST/api/hid/move_abs"
done
```

For the **relative** personas, `POST /api/hid/button {"button":"left","down":true}`
/ `…"down":false` holds/releases a button at the current spot (the web UI uses
this to drag). `release_all` always clears any held button.

## scroll

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"dy": -3}' \
    "https://$AEON_HOST/api/hid/scroll"
```

Positive `dy` = standard wheel up. macOS with "natural scrolling" sees the
inverse — that's the host OS, not the device. **On `generic-absolute`, scroll
via `move_abs`'s `wheel` field instead** — the relative `/hid/scroll` report
doesn't match the absolute descriptor.

## persona — hot-swap which keyboard/mouse the target sees

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"persona": "logitech-mx"}' \
    "https://$AEON_HOST/api/hid/persona"
```

| Persona | What the target sees | Use it for |
|---|---|---|
| `generic-composite` | boot keyboard + relative boot mouse, neutral VID `1d6b`. Most compatible, smallest attack surface. Linux-safe. | natural/relative input on the most compatible identity |
| `generic-absolute` | boot keyboard + **absolute pointer**, neutral VID. Reports absolute screen coords → enables `move_abs` / `click_at`. Linux-safe. | **AI agents — deterministic clicking.** The default for UI automation |
| `logitech-mx` | Logitech VID `046d`, MX-Keys + MX-Master flavor (media keys, extra buttons). Can wedge `aeon-hid` on Linux targets. | vendor **disguise** + natural input off-Linux |
| `apple-magic-stable` | Apple VID, Apple keyboard + working trackpad (pointer + keys + modifiers). | macOS targets (natural input) |
| `apple-magic` (experimental) | Apple VID, multi-touch; gestures WIP, pointer currently unreliable. See `docs/design/apple-mt.md`. | macOS gesture experiments only |

**Which to use:** `generic-absolute` for **deterministic clicking**
(snapshot → `click_at` lands on exact coords). A **relative** persona
(`generic-composite`, `logitech-mx` off-Linux, `apple-magic-stable` on macOS)
for **natural, human-like** interaction or apps that key off relative motion
(games / 3D / drag-velocity). `logitech-mx` / `apple-magic-stable` also serve
as vendor **disguise**. Switch freely mid-session — each switch triggers a USB
re-enumeration on the target (about a 1 s blip). The selection persists across
reboots.

## release_all — panic button

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    "https://$AEON_HOST/api/hid/release_all"
```

Sends release events for every modifier and mouse button, then fires a HID
reset. Run this if the target is misbehaving (continuous letter repeats, or
"modifier is held" behavior). Atomic-op design makes it rarely needed, but it's
there.
