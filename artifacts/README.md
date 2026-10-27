# Pinned microVM artifacts (Ubuntu1)

| Component | Version / id | Path |
| --- | --- | --- |
| Firecracker | v1.17.0 | `bin/firecracker` |
| jailer | v1.17.0 | `bin/jailer.real` (raw), `bin/jailer` (docker-privileged wrap) |
| Kernel | vmlinux-6.18.54 (FC CI 20261023-dd7c24b0d165-0) | `images/vmlinux` |
| Rootfs | Ubuntu 24.04 squashfs + guest-agent | `images/rootfs.ext4` |

## Obtain

See `VERSIONS` and `SHA256SUMS`. Do not download per request.

```sh
# Official FC release
curl -fL -o download/firecracker-v1.17.0-x86_64.tgz \
  https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-x86_64.tgz
# Kernel + squashfs from Firecracker CI S3 (keys in VERSIONS)
# Rootfs: unsquashfs + inject guest-agent + mkfs.ext4 -d (scripts/build-rootfs.sh)
```

## Node env

```sh
export SANDBOX_ENABLE_KVM=1
export SANDBOX_JAILER=/home/shinemo/penglei/agent-sandbox-microvm/artifacts/bin/jailer
export SANDBOX_JAILER_REAL=/home/shinemo/penglei/agent-sandbox-microvm/artifacts/bin/jailer.real
export SANDBOX_FIRECRACKER=/home/shinemo/penglei/agent-sandbox-microvm/artifacts/bin/firecracker
export SANDBOX_KERNEL=/home/shinemo/penglei/agent-sandbox-microvm/artifacts/images/vmlinux
export SANDBOX_ROOTFS=/home/shinemo/penglei/agent-sandbox-microvm/artifacts/images/rootfs.ext4
export SANDBOX_CHROOT_BASE=/tmp/sbxj
export SANDBOX_PRIV=docker   # tap/nft via artifacts/bin/priv
```

`bin/jailer` and `bin/priv` use docker privileged + `chroot /host` because this host has no passwordless sudo; jailer `unshare` and tap/nft need CAP_SYS_ADMIN. The microVM is still real Firecracker+KVM on the host netns/devices.
