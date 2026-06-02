# vision — see the target's screen

state, snapshot, and the live stream. Load this when you need to **see** what
the target is doing. All calls assume `https://${AEON_HOST}/` with HTTP Basic
`$AEON_USER:$AEON_PASSWD` (see the master SKILL.md Environment table).

## state — what is the device seeing right now?

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/state"
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
`references/recording.md`).

## snapshot — one JPEG of the current frame

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/streamer/snapshot" -o /tmp/aeon_capture.jpg
```

One JPEG, current frame. 1920×1080 at default settings; mirrors the source's
native resolution when `match_source` is on (capped at 1080p); 4K sources
downscale to fit. Latency ~50–150 ms on LAN (Pi 4 hardware JPEG encode ~40–80
ms). **After it returns, `Read` the file path to look at the frame** — that
JPEG is what you reason over to pick click targets. There is no separate
"cursor position" to query; compute targets in pixel space off the frame.

`what` → it's the eye half of see+act. `why` → drives every decision in the
vision loop; pair it with `click_at` from `references/input.md`.

## live stream (MJPEG / H.264)

Most agents only need `snapshot`. For a continuous feed into ffmpeg / OpenCV /
an `<img>` tag, use the multipart MJPEG endpoint:

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/streamer/stream"
```

There is also a low-latency H.264-over-WebSocket feed at
`GET /api/streamer/ws` (WebCodecs client; the web UI uses it). It authenticates
via the same-origin session cookie, so headless agents that can't set a cookie
should stick to `snapshot` over plain HTTP — that's the supported agent path.

## relaunch (rarely needed)

`POST /api/streamer/relaunch` force-respawns the streamer to pick up new
capture parameters from scratch (e.g. after the target's display resolution
changed and the feed is stuck on an old format). Prefer `snapshot`/`state`
first; only relaunch if the feed is genuinely wedged.
