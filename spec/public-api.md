# Public API

Origin: `http://127.0.0.1:8080`. Auth header: `X-API-Key`. Error JSON: `{"code":<int>,"message":"...","error_code":"optional"}`. 429 includes `Retry-After`.

Keys are stored as SHA-256 hex. Bootstrap env `BOOTSTRAP_API_KEY` inserts one tenant and one key when the table is empty. Platform template `base` is seeded: 2 vCPU, 2048 MiB, 10240 MiB disk.

## Endpoints

| Method | Path | Success |
| --- | --- | --- |
| GET | `/health` | 200 `{"status":"ok"}` no auth |
| POST | `/v2/sandboxes` and `/sandboxes` | 201 Sandbox |
| GET | `/sandboxes` and `/v2/sandboxes` | 200 JSON array of listed sandboxes for this tenant only |
| GET | `/sandboxes/{id}` | 200 detail |
| DELETE | `/sandboxes/{id}` | 204 |
| POST | `/sandboxes/{id}/timeout` | 204 body `{"timeout":<seconds>}` |
| POST | `/sandboxes/{id}/connect` | 200 Sandbox. Body requires `timeout`. Extends `end_at`. Does not resume a paused sandbox. |
| POST | `/sandboxes/{id}/pause` `/resume` `/fork` | 501 |
| other unimplemented | | 501 |

Create body uses `templateID`. `autoPause: true` is 400 `autoPause is not available`. Unknown template is 404. Missing key is 401. Unknown key is 401. Blocked tenant is 403. Over quota is 429. `secure` is ignored. `allow_internet_access`, `envVars`, `metadata`, `timeout` are honored. Default timeout 300 seconds. Default internet access true.

201 body required fields: `templateID`, `sandboxID`, `clientID`, `envdVersion`, plus `envdAccessToken`, `trafficAccessToken`, `domain`. `envdVersion` is `"0.1.0"`. `domain` comes from env `SANDBOX_DOMAIN` default `127.0.0.1`. IDs look like `sbx_` + 16 hex chars. Another tenant's id is 404, not 403.

Detail adds `startedAt`, `endAt`, `cpuCount`, `memoryMB`, `diskSizeMB`, `state`. State values: `running`. Times are RFC3339.

Create calls the node `POST /v1/sandboxes`. Node 503 maps to HTTP 503 `error_code=sandbox_create_failed`. No compatible node is 503 `sandbox_no_compatible_node`.

On running, append a `usage_events` row with `ended_at` null. On delete, set `ended_at` and sandbox `state=killed`. Write `audit_log` for create and delete.

Rate limit: 10 requests/second per key, burst 20. Exceeded is 429 `Retry-After: 1`.

Internal, header `Authorization: Bearer $INTERNAL_TOKEN`:

`GET /internal/routes/{id}` -> `{"sandboxID","state","guestURL","envdAccessToken","trafficAccessToken"}`. Unknown id is 404. Do not require a tenant key.

Placement: `NODE_URLS` comma-separated. Poll `GET /v1/capacity`. Pick the ready node with the lowest `committedMemMiB`. One refusal retries the next node once.
