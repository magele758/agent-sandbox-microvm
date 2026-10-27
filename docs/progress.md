# 进度

日期：2026-10-27。机器：darwin arm64，没有 `/dev/kvm`。下一台是 Ubuntu，从这里接着做。

仓库还不是可签约产品。完成的是控制面、节点拒绝路径、客户机代理和边缘代理。Firecracker 只写了调用序列，没有在真机上启动过。

## 已落地

| 路径 | 状态 |
| --- | --- |
| `api/` | Go 控制面。`X-API-Key`、SHA-256、租户隔离、配额、用量事件、10 rps 限流、Postgres。`pause` / `resume` / `fork` 返回 501 |
| `node/` | Rust 节点。槽位分配和 Firecracker JSON / jailer argv 有单测。没有 KVM 时 `POST /v1/sandboxes` 返回 503 `sandbox_create_refused` |
| `guest/` | envd 子集：`/process.Process/Start`、List、SendSignal、文件读写。路径逃逸和 `/init` 有测试 |
| `edge/` | `E2b-Sandbox-Id` 反代。`/init` `/upgrade` `/collapse` 不转发 |
| `spec/` | 公开 API、节点 API、envd、表结构。实现不要偏离这些文件 |
| `docs/from-scratch-build-plan.md` | 仍是总计划。本文只记录做到哪 |

本机验证：

- `go test`：`api/internal/server`、`edge` 通过
- `cargo test`：`node` 9 个、`guest` 4 个通过
- `deploy/smoke.sh`：创建返回 503 `sandbox_no_compatible_node`；直接打节点返回 503 `kvm unavailable`；边缘 `/init` 为 404；插入一条假的 running 记录后，边缘把 `/process.Process/Start` 转到本机 `guest-agent`，stdout 是 `hi`

那次 `echo` 不是 microVM。烟测用 SQL 把 `guest_url` 指到本机进程，只证明协议。不要在 Ubuntu 上把这个当成启动成功。

## 到 Ubuntu 先确认

```sh
test -e /dev/kvm && echo kvm-ok
id | grep -E 'kvm|root'
```

没有 `/dev/kvm` 就不要继续写业务。先打开嵌套虚拟化或换裸金属。不要加进程冒充虚拟机的驱动。

依赖：Go、Rust、Docker 或本机 Postgres、`curl`。Firecracker 和 jailer 用钉死版本，不要运行时从网上下。还需要一个直接启动的客户机内核，和一份已经装了 `guest-agent` 的 raw ext4。这些制品现在仓库里没有。

节点真正执行 jailer 要同时满足：Linux、`/dev/kvm` 存在、`SANDBOX_ENABLE_KVM=1`，以及：

```sh
SANDBOX_JAILER
SANDBOX_FIRECRACKER
SANDBOX_KERNEL
SANDBOX_ROOTFS
```

`GET /v1/capacity` 的 `ready` 只看设备在不在，不看 `SANDBOX_ENABLE_KVM`。所以 Ubuntu 上只要有 `/dev/kvm`，控制面就会来创建。没设后三个变量时，节点应返回启动失败，而不是 200。

Postgres 在 `deploy/docker-compose.yml` 里映射到宿主机 `5433`，因为开发机的 `5432` 已被占用。Ubuntu 若 `5432` 空着，可以把映射改回 `5432`，并同步 `DATABASE_URL`。`README.md` 仍写 `5432`，以 compose 文件为准。

默认端口：api `8080`，node `5008`，guest `49983`，edge `3002`。可用 `ADDR` 覆盖。烟测在本机改用了 `18080` / `15008` / `14983` / `13002`，因为 `8080` 被别的容器占了。

## 下一步，按这个顺序

1. 在 Ubuntu 上让 `POST /v1/sandboxes` 真的拉起 jailer，并在删除时回收进程和槽位。失败必须释放槽位。这是 M0 的退出条件。
2. 客户机里的 `guest-agent` 在 `:49983` 回答 `/health`。边缘能经 `guestURL` 跑一条命令。没有这一步，不要接控制面的成功路径。
3. 再做模板快照和 `FICLONE`。现在的创建路径是冷启动调用序列，不是方案里的快照恢复。
4. 钉两个官方 E2B SDK 版本，抓创建、执行、读文件、销毁的请求，写进 `compat/`。现在的 envd JSON 是我们自己定的，还没对照过 SDK。
5. 不要做 pause、UFFD、eBPF、自写 SDK。`autoPause: true` 继续返回 400。

已知缺口，改的时候不要假装已经有：

- 节点创建忽略了 `envVars` 和 `allowInternet`，没有下发到客户机。
- 槽位 IP 是逻辑地址，没有创建 tap，也没有 nftables。元数据地址拦截还没落地。
- `guest-agent` 不在 rootfs 里。`WORKSPACE_ROOT` 只为了在没有 `/workspace` 的宿主机上测协议，虚拟机里仍用 `/workspace`。
- 用量事件有写入，没有夜间对账作业。
- 官方 SDK 一次都没打过这个 API。
