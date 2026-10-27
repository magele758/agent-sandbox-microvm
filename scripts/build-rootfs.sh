#!/bin/bash
set -euo pipefail
REPO=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
ART=$REPO/artifacts
WORK=$ART/download/rootfs-work
# expects unsquashed tree + guest release binary already present; see docs
IMG=$ART/images/rootfs.ext4
rm -f "$IMG"
truncate -s 1G "$IMG"
mkfs.ext4 -F -d "$WORK/root" "$IMG"
echo "wrote $IMG"
