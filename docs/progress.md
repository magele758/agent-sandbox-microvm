# 进度

日期：2026-10-27。机器：Ubuntu1 x86_64，`/dev/kvm` ACL RW。

## M0（已验证并合入路径）

- 钉死制品：Firecracker/jailer **v1.17.0**，内核 **vmlinux-6.18.54**，rootfs=Ubuntu24.04+guest-agent。见 `artifacts/README.md`、`artifacts/VERSIONS`。
- `POST /v1/sandboxes` + `SANDBOX_ENABLE_KVM=1` 经 **jailer→Firecracker** 冷启动。无 passwordless sudo 时用 `artifacts/bin/jailer`（docker privileged + chroot /host）与 `artifacts/bin/priv` 建 tap；VM 仍是本机 KVM。
- 成功条件=客户机 guest-agent `/health`；失败与 DELETE 回收进程/tap/jail/槽位。
- 烟测：`SMOKE_M0_OK`（create→health→exec→delete→recreate）；边缘经 guestURL：`SMOKE_EDGE_OK`。
- 节点单测 11/11（KVM 存在时 `create_without_kvm…` 通过）。

## M1（模板 + FICLONE / 快照）

- `SANDBOX_TEMPLATE_DIR/<templateID>/`：rootfs + 可选 `vmstate`/`mem`。
- 创建时对 rootfs 优先 `cp --reflink=always`（FICLONE），失败回退 `--reflink=auto` 再全量拷贝。本机部分路径不支持 reflink 时走回退。
- 快照：`scripts/create-base-template-snapshot.sh` 打 Full 快照；存在 `vmstate`+`mem` 时走 `PUT /snapshot/load`（含 `network_overrides` + `resume_vm`）。默认把快照文件放在 `templates/base/snapshot/`，日常创建走冷启动+FICLONE，避免误触快照。
- 烟测：`SMOKE_M1_COLD_OK`、`SNAP_RESTORE_OK`。

## compat/

- `compat/versions.txt` 钉死 Python/JS 官方 E2B SDK 版本。
- `compat/fixtures/` 含 create/exec/read-file/destroy 请求形状；`go test` 校验契约字段。

## 仍缺口 / 不做

- envVars/allowInternet：创建后尽力写入 guest `/workspace/.sandbox-init.json`；未注入客户机进程环境变量表。
- nftables：建 tap 时尽力加 169.254.169.254 / 169.254.0.0/16 drop（需 priv）；完整槽位隔离回归未进 CI。
- pause/resume/fork 保持 501；不做 UFFD、eBPF、自写 SDK、夜间对账。
