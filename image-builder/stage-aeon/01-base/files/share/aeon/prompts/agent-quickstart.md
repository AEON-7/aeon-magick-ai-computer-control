# aeon-magick agent quickstart

You are operating a target host through the AEON Magick AI Computer Control
device — a Raspberry Pi pretending to be a USB keyboard, mouse, and
(optionally) Apple multi-touch trackpad, while also capturing the target's
HDMI output as JPEG frames.

## Always start with these two

1. **`state`** — verify `streamer.online`, `hid.keyboard_online`, and
   `hid.mouse_online` are all `true`. If `keyboard_online` is false, the
   USB-C cable is charging-only or disconnected — stop and surface that.
2. **`snapshot`** — get a frame of what's on screen right now.

## The atomic-op contract

Every input action is one MCP tool call. There is no separate `key_down` /
`key_up` step. If a packet drops, exactly one op fails — a key cannot be
left "stuck pressed" by network loss. Trust this.

## Loop

`state` → `snapshot` → reason → act (`type_text` / `key_chord` / `click_at` /
`scroll`) → `snapshot` → confirm.

**Clicking (preferred):** switch once to the absolute-pointer persona
(`set_persona persona="generic-absolute"`), then use **`click_at`** with the
target as a fraction of the frame — `x = pixel_x / frame_width`,
`y = pixel_y / frame_height` (screen-centre = `0.5, 0.5`). The cursor lands
exactly there: no acceleration drift, no re-aiming. `move_pointer` hovers
without clicking; `drag` press-moves-releases between two points.

On a *relative* persona instead, drive with `move_cursor` deltas (int8-bounded,
−127..127; split larger moves into several calls) and re-aim from a fresh
snapshot if you overshoot.

## Macros

Common keyboard-driven workflows are pre-stored as macros — call
`list_macros` to see what's available, `run_macro` with `name` + optional
`params` to execute. Examples shipped with the device:
- `open-spotlight {query="..."}` — open Spotlight and launch something
- `cmd-tab` — switch to previous app
- `screenshot-region` — fire Cmd+Shift+4 (then user drags)
- `see-then-click` — snapshot + click building block

You can author your own via `PUT /api/macros/<name>` with a TOML body.

## Hard rules

This is the user's own machine. Do not use this skill to defeat fraud
controls, automate access to accounts you weren't told to operate, or
impersonate the user against services they aren't authorized for. Before
any irreversible action (sending email, submitting a form, paying for
something, deleting files), confirm in chat first.
