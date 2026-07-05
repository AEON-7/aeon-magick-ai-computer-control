---
name: aeon-update
description: >
  Keep an Aeon Orb current and secure — apply OS security patches, toggle
  automatic patching, and check whether a newer Orb IMAGE has been published.
  Load this when the task is to update / patch the Orb, check its version, or ask
  "is my Orb up to date?". Two distinct layers: OS packages (apt, live) and the
  whole image (re-flash from Patreon — the Orb never self-flashes).
---
# aeon-update

Two separate things, both on the **System** page and exposed as MCP tools:

1. **OS packages** — patch the running Raspberry Pi OS in place (apt). Live.
2. **Orb image** — notice when a newer image (v111 / v112 → …) is published and
   guide a backup + re-flash. The Orb only *notifies*; flashing is a manual
   Patreon download.

> **Credentials.** `Authorization: Bearer $AEON_TOKEN` against `https://$AEON_HOST`
> (self-signed → `curl -k`). Each capability is a **first-class MCP tool** (bold);
> the REST below is the same handler. OS mutation tools are Full-scope; the
> read/check tools are Read-scope.

## Is a newer IMAGE available? — **`system_image_updates`**

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/system/image-updates"
# → {track:"pi5", installed:112, latest:112, update_available:false,
#    notes:"…", patreon_url:"https://www.patreon.com/AeonForge7/…"}
```

Compares the version stamped in `/etc/aeon-image-version` against the published
manifest (`image-manifest.json` on GitHub). `update_available:true` means a newer
image exists for this board's track — tell the operator to **back up their config
and re-flash from Patreon** (the console's System page has a one-click "back up →
Patreon" flow). There is no self-flash. `installed:null` = a legacy unstamped
image (re-flash to get versioned images).

## Patch the OS — **`os_update_check`** → **`os_update_apply`** → **`os_update_status`**

```bash
# 1. how many packages can upgrade (and how many are security)?
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/system/update/check"
# → {ok:true, count:3, security:1, packages:"pkg1,pkg2,pkg3"}

# 2. apply them all (backgrounded — apt dist-upgrade). Returns immediately.
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/system/update"
# → {ok:true, started:true}

# 3. poll to completion
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/system/update/status"
# → {phase:"refreshing|upgrading|cleaning|done|failed", percent, done, ok, log,
#    reboot_required}
```

Poll `os_update_status` until `done:true`. If `reboot_required:true`, a kernel or
firmware package changed — recommend a reboot to apply it (the Orb never reboots
itself). Only one update runs at a time (single-flight); a second call returns
`started:false`.

## Automatic security patching — **`os_auto_updates`**

```bash
# read state
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/system/auto-updates"
# → {ok:true, enabled:true, installed:true}

# turn on (installs unattended-upgrades; security-only; NEVER auto-reboots)
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"enable":true}' "https://$AEON_HOST/api/system/auto-updates"
```

Fresh images ship with this **on** by default. It patches security in the
background and never reboots on its own.

**Flow:** **`system_image_updates`** (new image? → guide backup + Patreon) ·
**`os_update_check`** → **`os_update_apply`** → poll **`os_update_status`** →
reboot if flagged · **`os_auto_updates`** to keep it hands-off. These are the Pi
appliance's OS/image; a connected GPU box's bench pod updates via `bench_update`.
