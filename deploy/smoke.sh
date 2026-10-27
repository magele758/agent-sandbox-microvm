#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$root"
mkdir -p deploy/bin deploy/workspace

docker compose -f deploy/docker-compose.yml up -d postgres
i=0
until docker compose -f deploy/docker-compose.yml exec -T postgres pg_isready -U sandbox -d sandbox >/dev/null 2>&1; do
  i=$((i + 1))
  if [ "$i" -gt 40 ]; then
    echo "postgres did not become ready" >&2
    exit 1
  fi
  sleep 0.5
done

export DATABASE_URL=postgres://sandbox:sandbox@127.0.0.1:5433/sandbox?sslmode=disable
export INTERNAL_TOKEN=dev-internal
export BOOTSTRAP_API_KEY=e2b_devkey
export SANDBOX_DOMAIN=127.0.0.1
export API_URL=http://127.0.0.1:18080
export NODE_URLS=http://127.0.0.1:15008
export ENVD_TOKEN=smoke-token
export WORKSPACE_ROOT="$root/deploy/workspace"

( cd api && go build -o "$root/deploy/bin/api" ./cmd/api )
( cd edge && go build -o "$root/deploy/bin/edge" ./cmd/edge )
( cd node && cargo build --quiet )
( cd guest && cargo build --quiet )
cp node/target/debug/node-agent guest/target/debug/guest-agent deploy/bin/

ADDR=:18080 deploy/bin/api >deploy/bin/api.log 2>&1 &
api_pid=$!
ADDR=0.0.0.0:15008 deploy/bin/node-agent >deploy/bin/node.log 2>&1 &
node_pid=$!
ADDR=0.0.0.0:14983 deploy/bin/guest-agent >deploy/bin/guest.log 2>&1 &
guest_pid=$!
ADDR=:13002 deploy/bin/edge >deploy/bin/edge.log 2>&1 &
edge_pid=$!
trap 'kill $api_pid $node_pid $guest_pid $edge_pid 2>/dev/null || true' EXIT

i=0
until curl -sf http://127.0.0.1:18080/health >/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 40 ]; then
    echo "api did not listen" >&2
    cat deploy/bin/api.log >&2
    exit 1
  fi
  sleep 0.25
done

code=$(curl -s -o /tmp/sbx-create.json -w "%{http_code}" \
  -X POST http://127.0.0.1:18080/v2/sandboxes \
  -H "X-API-Key: e2b_devkey" \
  -H "content-type: application/json" \
  -d '{"templateID":"base","timeout":60}')
echo "create=$code $(cat /tmp/sbx-create.json)"
test "$code" = "503"
grep -q sandbox_no_compatible_node /tmp/sbx-create.json

node_code=$(curl -s -o /tmp/sbx-node.json -w "%{http_code}" \
  -X POST http://127.0.0.1:15008/v1/sandboxes \
  -H "content-type: application/json" \
  -d '{"sandboxID":"sbx_direct","templateID":"base","vcpu":2,"memMiB":512,"envdAccessToken":"t","allowInternet":false}')
echo "node=$node_code $(cat /tmp/sbx-node.json)"
test "$node_code" = "503"
grep -q sandbox_create_refused /tmp/sbx-node.json

docker compose -f deploy/docker-compose.yml exec -T postgres psql -U sandbox -d sandbox -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO sandboxes (
  id, tenant_id, template_id, state, guest_url,
  envd_access_token, traffic_access_token, domain, envd_version, client_id,
  cpu_count, memory_mb, disk_size_mb, started_at, end_at
) VALUES (
  'sbx_smoke00000001',
  (SELECT id FROM tenants LIMIT 1),
  'base',
  'running',
  'http://127.0.0.1:14983',
  'smoke-token',
  'smoke-traffic',
  '127.0.0.1',
  '0.1.0',
  'cli_smoke',
  2, 2048, 10240, now(), now() + interval '1 hour'
) ON CONFLICT (id) DO UPDATE SET guest_url = EXCLUDED.guest_url, state = 'running';
SQL

init_code=$(curl -s -o /tmp/sbx-init.json -w "%{http_code}" \
  -X POST http://127.0.0.1:13002/init \
  -H "E2b-Sandbox-Id: sbx_smoke00000001" \
  -H "E2b-Sandbox-Port: 49983" \
  -H "X-Access-Token: smoke-token")
echo "init=$init_code $(cat /tmp/sbx-init.json)"
test "$init_code" = "404"

exec_code=$(curl -s -o /tmp/sbx-exec.json -w "%{http_code}" \
  -X POST http://127.0.0.1:13002/process.Process/Start \
  -H "E2b-Sandbox-Id: sbx_smoke00000001" \
  -H "E2b-Sandbox-Port: 49983" \
  -H "X-Access-Token: smoke-token" \
  -H "content-type: application/json" \
  -d '{"process":{"cmd":"/bin/echo","args":["hi"]}}')
echo "exec=$exec_code $(cat /tmp/sbx-exec.json)"
test "$exec_code" = "200"
python3 - <<'PY'
import base64, json
body = json.load(open("/tmp/sbx-exec.json"))
out = base64.b64decode(body["event"]["data"]["stdout"])
assert out == b"hi\n", out
print("stdout ok")
PY
echo "smoke ok"
