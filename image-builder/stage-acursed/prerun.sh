#!/bin/bash -e
# pi-gen calls prerun.sh once before iterating the 0N-run.sh files.
# Use it to copy the previous stage's rootfs into ours.

if [ ! -d "${ROOTFS_DIR}" ]; then
    copy_previous
fi
