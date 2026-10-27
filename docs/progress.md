# 进度

日期：2026-10-27。机器：Ubuntu1 x86_64，`/dev/kvm` ACL RW。分支工作见 `feat/m0-jailer-boot`。

## M0（已在 Ubuntu1 验证）

- 钉死制品：Firecracker/jailer **v1.17.0**，内核 **vmlinux-6.18.54**，rootfs=Ubuntu24.04+guest-agent。说明与 env：`artifacts/README.md`、`artifacts/VERSIONS`。
- `POST /v1/sandboxes` + `SANDBOX_ENABLE_KVM=1` 经 **jailer→Firecracker** 冷启动（无share/tap 经 `artifacts/bin/priv|jailer` 的 docker privileged 包装，因宿主无 passwordless sudo；VM 仍是本机 KVM）。
- 创建成功条件=客户机 `guest-agent` `/health`；失败与 `DELETE` 回收进程、tap、jail 目录与槽位。
- 烟测：`/tmp/smoke-m0.sh` → create→health→exec echo→delete→recreate = `SMOKE_M0_OK`。
- 节点单测 11/11（含 KVM 存在时 `create_without_kvm…`）。

## 下一步

1. ~~jailer 真启与回收~~（M0 完成）
2. 边缘经 guestURL 跑命令（节点 guestURL 已通；接 api/edge/Postgres 闭环）
3. 模板快照 + FICLONE 创建路径（M1）
4. `compat/` 钉官方 E2B SDK 并抓请求
5. 仍缺口：nftables 元数据丢弃、envVars 进客户机进程环境（现写入 `.sandbox-init.json` 尽力而为）、pause 保持 501
