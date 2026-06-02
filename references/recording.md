# recording — capture the target's screen to MP4

On-demand screen recording. The device subscribes to the live H.264 stream and
muxes it to MP4 with `ffmpeg -c copy` (no re-encode → nearly free on the Pi).
Load this when you need a video of what the target did, not just stills. All
calls assume `https://${AEON_HOST}/` with HTTP Basic `$AEON_USER:$AEON_PASSWD`.

**Precondition:** recording requires the **H.264 stream mode**. If `state`
(see `references/vision.md`) shows `streamer.mode` is not `h264`, set the
stream format to h264 first — otherwise `record/start` returns
`409 recording requires H.264 stream mode`.

## start a recording

```bash
# default 30s
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" -d '{}' \
    "https://$AEON_HOST/api/streamer/record/start"

# custom length, e.g. 120s
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" -d '{"seconds": 120}' \
    "https://$AEON_HOST/api/streamer/record/start"
```

Body field `seconds` (the streamer reads it as `duration_s`): **omit → 30 s
default**; a positive value auto-stops after that many seconds; `0` →
open-ended / manual-stop, **capped at 3 h**. One recording at a time. The
response carries the recording **id** — you download by that id later.

`what` → starts muxing the live H.264 to an MP4 on the Pi. `why` → on-demand,
customizable length, no re-encode cost.

## stop / status

```bash
# stop + finalize the in-progress MP4
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    "https://$AEON_HOST/api/streamer/record/stop"

# status: the active recording (id, elapsed) if any, plus finished list + note
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/streamer/record/state"
```

## list / download

```bash
# list finished recordings (id, size, human-readable timestamp), newest first
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/streamer/recordings"

# download one MP4 by id
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/streamer/recordings/<id>" -o capture.mp4

# first-frame JPEG thumbnail for an id
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/streamer/recordings/<id>/thumb" -o thumb.jpg
```

Recordings list with **human-readable timestamps**, and each gets a
**first-frame `.jpg` thumbnail** so you can eyeball which capture is which
before downloading the full MP4.

## disk behaviour (good to know)

- Recordings may occupy **at most 50% of the total disk**. A **rotating purge
  drops the oldest** recordings first to stay under that budget.
- If the *current* recording alone would blow the budget, it is **stopped** and
  the response says: `video recording allocation max capacity, use a larger
  disk for more record time`. So video can never fill the disk.
- Files live in `/var/lib/aeon/recordings` on the Pi.

## MCP

Over MCP the tools are `record_start` (optional `duration_s`), `record_stop`,
`recording_state`, and `list_recordings`. See `references/mcp.md`. (Download +
thumbnail are HTTP-only — fetch by id via the REST routes above.)
