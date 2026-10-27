# sandbox

Self-built microVM sandbox control plane. Public HTTP is the E2B subset in `spec/public-api.md`. Nothing calls E2B, CubeSandbox, or another sandbox vendor.

| Directory | Process | Port |
| --- | --- | --- |
| `api/` | control plane, Postgres | 8080 |
| `node/` | node-agent, Firecracker only with Linux + `/dev/kvm` + `SANDBOX_ENABLE_KVM=1` | 5008 |
| `guest/` | envd subset | 49983 |
| `edge/` | sandbox traffic proxy | 3002 |

On a machine without KVM, `POST /v2/sandboxes` returns 503 `sandbox_no_compatible_node`. The node itself returns 503 `sandbox_create_refused`. That is fail-closed, not a process substitute.

```sh
docker compose -f deploy/docker-compose.yml up -d postgres
export DATABASE_URL=postgres://sandbox:sandbox@127.0.0.1:5433/sandbox?sslmode=disable
export INTERNAL_TOKEN=dev-internal
export BOOTSTRAP_API_KEY=e2b_devkey
export NODE_URLS=http://127.0.0.1:5008
export API_URL=http://127.0.0.1:8080
( cd api && go run ./cmd/api )
```

`deploy/smoke.sh` builds the four binaries, checks the KVM refusal, and proxies one `echo` through the edge to the guest agent.

Not in this slice: pause/resume, official SDK trace, jailer execution, UFFD, billing export. Those stay in `docs/from-scratch-build-plan.md`.
