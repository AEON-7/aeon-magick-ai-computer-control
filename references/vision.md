# vision — see the target's screen

state, snapshot, and the live stream. Load this when you need to **see** what
the target is doing. All calls assume `https://${AEON_HOST}/` with a Bearer token
`$AEON_TOKEN` (see the master SKILL.md Environment table).

## state — what is the device seeing right now?

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/state"
```

Returns:
```json
{
  "streamer": {"online": true, "resolution": "1920x1080", "fps": 30, "mode": "h264"},
  "hid":      {"persona": "logitech-mx", "keyboard_online": true, "mouse_online": true}
}
```

**Call this first every session.** If `keyboard_online: false`, stop and tell
the user — there's no point sending input that won't land. If
`streamer.online` is false, snapshots will be stale or empty — re-seat the
capture stick and confirm the target is outputting HDMI.

`mode` tells you the active stream pipeline. `h264` is the low-latency
WebSocket pipeline and is the one screen **recording** needs (see
`references/recording.md`). `state` tells you *whether* the feed is live; to
learn *which* capture source it's coming from, read `/api/streamer/config`
below.

## snapshot — one JPEG of the current frame

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    "https://$AEON_HOST/api/streamer/snapshot" -o /tmp/aeon_capture.jpg
```

One JPEG, current frame. 1920×1080 at default settings; mirrors the source's
native resolution when `match_source` is on (capped at 1080p); 4K sources
downscale to fit. Latency ~50–150 ms on LAN (Pi 4 hardware JPEG encode ~40–80
ms; Pi 5 CSI sources encode in software, similar order). **After it returns,
`Read` the file path to look at the frame** — that JPEG is what you reason over
to pick click targets. There is no separate "cursor position" to query; compute
targets in pixel space off the frame.

`what` → it's the eye half of see+act. `why` → drives every decision in the
vision loop; pair it with `click_at` from `references/input.md`.

## Reading the screen: OCR text, find-to-click, and a semantic description

On a **Hailo AI HAT+ Orb** the on-device vision stack turns a frame into text +
coordinates so you don't have to eyeball pixels. Three tools, all first-class
**MCP tools** (`tools/list` + `tools/call`) *and* REST. They need the on-device
vision stack running — the `aeon-vision` OCR daemon for text/find, and a deployed
Qwen2-VL `.hef` for describe (deploy it from the Hailo app).

### screen_text — all the OCR text on screen

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/vision/detections"
```

Every text run the OCR found, each with `text`, `conf`, and a `box` in **0..1
fractions** of the frame (plus `box_px`). `{present:false}` if vision is disabled
or there's no frame. Use it to dump everything on screen at once.

### screen_find — find a label, get its clickable centre (the fast path)

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    "https://$AEON_HOST/api/vision/find?query=Save"
# → {"ok":true,"query":"Save","count":1,
#    "matches":[{"text":"Save","match_score":1.0,"ocr_conf":0.98,
#                "center":{"x":0.13,"y":0.22}}],
#    "hint":"feed center into click(x,y)"}
```

Searches the live OCR for `query` and returns ranked matches, each with a
`center` `{x,y}` in **0..1 fractions** — feed it straight into `click_at`
(`references/input.md`), no pixel math. `{ok:false}` if vision is off or nothing
matches. Needs **no NPU GenAI slot**, so it runs alongside the local LLM. This is
the one-call answer to "click the X button / link / field."

### describe_screen — a natural-language read of the screen

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    -H "Content-Type: application/json" -X POST \
    "https://$AEON_HOST/api/vision/describe" \
    -d '{"prompt":"which dialog is open?","max_tokens":64}'
# → {"ok":true,"description":"The screen shows a message that says
#    \"Can't Connect to Server.\"","gen_ms":2629}
```

A Qwen2-VL vision-language model on the NPU describes the whole frame; pass an
optional `prompt` for a specific visual question. Use it for **semantic**
understanding ("what app/state is this?") when exact text + coordinates
(`screen_text` / `screen_find`) aren't enough. ~3 s warm, ~13 s cold (the first
call loads the 2.2 GB VLM; it then stays resident and idle-unloads after ~180 s).
It borrows the NPU's **single GenAI slot**, so it's available when the local
hailo-ollama LLM isn't resident (the normal online case); otherwise it returns
`{ok:false}` with guidance to free the slot.

**The perception ladder:** `snapshot` (pixels) → `screen_text` (all OCR) /
`screen_find` (one label → click it) → `describe_screen` (semantic) →
`click_at` / `type_text`.

## live stream (MJPEG / H.264)

Most agents only need `snapshot`. For a continuous feed into ffmpeg / OpenCV /
an `<img>` tag, use the multipart MJPEG endpoint:

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/streamer/stream"
```

There is also a low-latency H.264-over-WebSocket feed at
`GET /api/streamer/ws` (WebCodecs client; the web UI uses it). It authenticates
via the same-origin session cookie, so headless agents that can't set a cookie
should stick to `snapshot` over plain HTTP — that's the supported agent path.

## Which source am I seeing?

An Orb can have more than one input wired to it (a USB capture stick, a Pi-5
HDMI→CSI bridge, a Pi-5 CSI camera), and the snapshot/stream show whichever is
**currently selected**. If you just need to know what you're looking at,
`GET /api/streamer/config` reports the active `source` + `available_sources`:

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/streamer/config"
```

To **switch** what the eye is pointed at — flip from the controlled screen to a
physical camera and back, with the full source vocabulary + Pi-5 specifics —
load the **`aeon-cameras`** skill. That switching restarts the streamer (brief
feed blip), so you don't need `relaunch` on top of it.

## relaunch (rarely needed)

`POST /api/streamer/relaunch` force-respawns the streamer to pick up new
capture parameters from scratch (e.g. after the target's display resolution
changed and the feed is stuck on an old format). Prefer `snapshot`/`state`
first; only relaunch if the feed is genuinely wedged. Note switching `source`
via `PUT /api/streamer/config` already restarts the streamer, so you don't need
to relaunch on top of a source switch.
