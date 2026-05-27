#!/bin/bash -e
# pi-gen calls prerun.sh once before iterating the 0N-run.sh files.
# Copy the previous stage's rootfs into ours.
#
# Caveat: pi-gen's build.sh unconditionally sets PREV_ROOTFS_DIR to the
# last loop iteration's rootfs path, even when that stage was SKIPped.
# Since we SKIP the desktop stages (3/4/5), PREV_ROOTFS_DIR points at
# stage5/rootfs which doesn't exist. Walk backwards to the last stage
# that actually produced a rootfs.

if [ ! -d "${ROOTFS_DIR}" ]; then
    if [ ! -d "${PREV_ROOTFS_DIR:-}" ]; then
        for prev in stage5 stage4 stage3 stage2 stage1 stage0; do
            candidate="${WORK_DIR}/${prev}/rootfs"
            if [ -d "${candidate}" ]; then
                export PREV_ROOTFS_DIR="${candidate}"
                echo "prerun: PREV_ROOTFS_DIR -> ${candidate}"
                break
            fi
        done
    fi
    copy_previous
fi
