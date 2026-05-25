# pi-gen patches for OrbStack on Apple Silicon

Two small patches against `pi-gen` (branch `bookworm-arm64`) that are required
to bake an image on macOS / Apple Silicon / OrbStack. Both work around a
single underlying limitation: **OrbStack's kernel detects loop-device
partitions (lsblk sees them) but doesn't fully expose them as block-device
nodes that mkfs/mount can open.** The vanilla pi-gen flow uses
`losetup --partscan`, which hits EPERM on `/dev/loop0p1` open() in this
environment.

## Apply

```bash
cd ~/pi-gen
git checkout -B bookworm-arm64 origin/bookworm-arm64
git apply /path/to/aeon-cursed-kvm/image-builder/pi-gen-patches/*.patch
```

## What each patch does

### `01-unmount-image-multi-loop.patch`

Makes `scripts/common`'s `unmount_image()` iterate over **multiple loop
devices per image file** (the second patch creates one loop per partition,
so a single image file maps to two loops).

### `02-export-image-offset-loops.patch`

Replaces the `losetup --partscan` partition-detection path in
`export-image/prerun.sh` with two **offset-based loop devices**, one per
partition:

```
BOOT_DEV=$(losetup --show -f --offset=$BOOT_PART_START --sizelimit=$BOOT_PART_SIZE $IMG_FILE)
ROOT_DEV=$(losetup --show -f --offset=$ROOT_PART_START --sizelimit=$ROOT_PART_SIZE $IMG_FILE)
```

Each loop device maps a byte range directly to the image file — no kernel
partition scanning needed, works in every container environment.

## Why upstream this

If you're a pi-gen contributor reading this: the offset-loop approach is
strictly more portable than `--partscan` (it works in every environment
that `--partscan` does, plus the broken ones). Worth considering as the
default.
