#!/bin/bash
set -euo pipefail
REPO=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
ART=$REPO/artifacts
TDIR=$REPO/templates/base
BASE=/tmp/sbxj
ID=sbxsnap1
TAP=tapsnap1
/tmp/m0-clean.sh >/dev/null 2>&1 || true
mkdir -p "$TDIR" /tmp/sbxj/images
cp --reflink=auto -f "$ART/images/rootfs.ext4" /tmp/sbxj/images/rootfs.ext4
cp --reflink=auto -f "$ART/images/vmlinux" /tmp/sbxj/images/vmlinux
docker run --rm --privileged --network=host -v /:/host postgres:16-alpine chroot /host /bin/bash -c "
  /usr/sbin/ip link del $TAP 2>/dev/null || true
  /usr/sbin/ip tuntap add dev $TAP mode tap
  /usr/sbin/ip addr add 172.16.0.1/28 dev $TAP
  /usr/sbin/ip link set $TAP up
"
docker rm -f jailer-$ID >/dev/null 2>&1 || true
docker run -d --name jailer-$ID --privileged --network=host --device=/dev/kvm \
  -v /:/host -v "$ART:$ART" -v "$BASE:$BASE" \
  postgres:16-alpine chroot /host "$ART/bin/jailer.real" \
  --id $ID --exec-file $ART/bin/firecracker --uid 1000 --gid 1000 \
  --cgroup-version 2 --chroot-base-dir $BASE -- --api-sock /run/firecracker.socket >/dev/null
SOCK=$BASE/firecracker/$ID/root/run/firecracker.socket
for i in $(seq 1 100); do [[ -S $SOCK ]] && break; sleep 0.05; done
ROOT=$BASE/firecracker/$ID/root
ln -f /tmp/sbxj/images/vmlinux $ROOT/vmlinux
cp --reflink=auto -f /tmp/sbxj/images/rootfs.ext4 $ROOT/rootfs.ext4
put() { curl -sS --max-time 30 -o /tmp/fc.out -w "%{http_code}" --unix-socket "$SOCK" -X PUT "http://localhost$1" -H "Content-Type: application/json" -d "$2"; echo " $1 $(cat /tmp/fc.out)"; }
put /machine-config '{"vcpu_count":1,"mem_size_mib":512,"smt":false}' >/dev/null
put /drives/rootfs '{"drive_id":"rootfs","path_on_host":"/rootfs.ext4","is_root_device":true,"is_read_only":false}' >/dev/null
put /network-interfaces/eth0 '{"iface_id":"eth0","guest_mac":"06:00:AC:10:00:02","host_dev_name":"tapsnap1"}' >/dev/null
put /boot-source '{"kernel_image_path":"/vmlinux","boot_args":"console=ttyS0 reboot=k panic=1 pci=off init=/sbin/init ip=172.16.0.2::172.16.0.1:255.255.255.240:sbx:eth0:off"}' >/dev/null
put /actions '{"action_type":"InstanceStart"}' >/dev/null
for i in $(seq 1 60); do curl -sf --connect-timeout 1 http://172.16.0.2:49983/health >/dev/null && break; sleep 1; done
curl -sf http://172.16.0.2:49983/health >/dev/null
put /actions '{"action_type":"Pause"}' >/dev/null
put /snapshot/create '{"snapshot_type":"Full","snapshot_path":"/vmstate","mem_file_path":"/mem"}' >/dev/null
cp -f $ROOT/vmstate $TDIR/vmstate
cp -f $ROOT/mem $TDIR/mem
cp --reflink=auto -f $ROOT/rootfs.ext4 $TDIR/rootfs.ext4
python3 -c "import json;from pathlib import Path;p=Path('$TDIR/manifest.json');m=json.loads(p.read_text());m['vmstate']='vmstate';m['mem']='mem';m['snapshot']='Full';p.write_text(json.dumps(m,indent=2)+'\n')"
docker rm -f jailer-$ID >/dev/null
docker run --rm --privileged --network=host -v /:/host postgres:16-alpine chroot /host /usr/sbin/ip link del $TAP >/dev/null 2>&1 || true
ls -lah $TDIR
echo SNAPSHOT_OK
