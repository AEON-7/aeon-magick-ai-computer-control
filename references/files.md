# files & ISOs — stage files for the target, manage the ISO library

Two related capabilities. A **file staging area** on the Pi
(`/var/lib/aeon/files/`) that can be served to the target, and a **USB-CDROM
ISO library** (`/var/lib/aeon/iso/`) for OS-install workflows. Load this when
you need to move a file to/from the target or mount an install image. All calls
assume `https://${AEON_HOST}/` with HTTP Basic `$AEON_USER:$AEON_PASSWD`.

## File staging

Files you upload land in `/var/lib/aeon/files/`. They can optionally be exposed
to the target as a plain HTTP server on the USB ethernet (off by default;
toggled via `/api/files/config`, human-set).

```bash
# List staged files (name, size_bytes, modified_ms)
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/files"

# Upload a file — filename rides in the Content-Disposition header
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H 'Content-Disposition: attachment; filename="local-file.bin"' \
    --data-binary @local-file.bin \
    "https://$AEON_HOST/api/files/upload"

# Download a staged file
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/files/local-file.bin" -o copy.bin

# Delete one (idempotent)
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X DELETE \
    "https://$AEON_HOST/api/files/local-file.bin"

# Read/inspect the target-facing-server config (enabled / port / allow_upload)
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/files/config"
```

`what` → a shared drop folder on the Pi. `why` → move a binary to the target
(or pull one off it) without going through the OS over USB-HID. Upload streams
to disk, so multi-GB files are fine. Filenames must be plain ASCII +
`.-_ ()+[]@` (no path traversal).

## ISO library (USB-CDROM)

A separate library used to expose **one** chosen ISO to the target as a
USB-CDROM device — the basis of OS-install workflows.

```bash
# List ISOs + the currently-active slug + free disk
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/storage"

# Activate one (target sees a fresh CDROM insertion on next USB enumeration)
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT \
    -H "Content-Type: application/json" \
    -d '{"slug": "ubuntu-24.04"}' \
    "https://$AEON_HOST/api/storage/active"

# Eject (empty slug)
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT \
    -H "Content-Type: application/json" \
    -d '{"slug": ""}' \
    "https://$AEON_HOST/api/storage/active"
```

Upload an ISO with `POST /api/storage/upload` (streamed multipart) and remove
one with `DELETE /api/storage/:slug` (refused if it's the active slug).

### OS-install sketch

1. `list_isos` to see what's available → `set_active_iso` with the slug.
2. (Human powers/reboots the target — target power is **not** an agent
   capability.) The target sees the CDROM on its next USB enumeration.
3. Drive BIOS/UEFI to boot the CDROM with the vision loop (`snapshot` +
   `key` chords — see `references/vision.md` / `references/input.md`).
4. After install, `set_active_iso` with an empty slug to eject.

## MCP

Over MCP: `list_files`, `read_file`, `delete_file` (staging area); `list_isos`,
`set_active_iso` (ISO library). See `references/mcp.md`. `read_file` returns the
file as base64; uploads are HTTP-only.

> Route note: the ISO library lives under `/api/storage/*` (REST), while its
> MCP tools are named `list_isos` / `set_active_iso`.
