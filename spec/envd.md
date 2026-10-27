# Guest envd

Listen `:49983`. Every request except `GET /health` requires header `X-Access-Token` equal to the process env `ENVD_TOKEN`. Mismatch is 401 `{"code":401,"message":"unauthorized"}`.

`GET /health` -> 200 `{"status":"ok"}` with no token.

Connect-style JSON, unary, `Content-Type: application/json`:

`POST /process.Process/Start` body:

```json
{"process":{"cmd":"/bin/echo","args":["hi"],"envs":{"A":"b"}}}
```

Runs `cmd` with `args` and `envs`, waits up to 30s, returns 200:

```json
{"event":{"start":{"pid":1},"data":{"stdout":"<base64>","stderr":"<base64>"},"end":{"exitCode":0}}}
```

Non-zero exit is still 200; the exit code is in the body. Timeout is 408.

`POST /process.Process/List` -> `{"processes":[{"pid":1,"cmd":"/bin/echo"}]}` for processes started and not yet reaped. After wait, the process may be absent. Keep the last 16 exited processes in the list with `"exited":true`.

`POST /process.Process/SendSignal` body `{"pid":1,"signal":15}` -> `{}`. Unknown pid is 404.

`POST /filesystem.Filesystem/List` body `{"path":"/workspace"}` -> `{"entries":[{"name":"a","type":"file"}]}`.

`POST /filesystem.Filesystem/Write` body `{"path":"/workspace/a","data":"<base64>"}` -> `{}`.

`POST /filesystem.Filesystem/Read` body `{"path":"/workspace/a"}` -> `{"data":"<base64>"}`.

All filesystem paths must stay under `/workspace` after `filepath.Clean` / Rust `canonicalize` of the parent. Reject `..`, absolute paths outside the root, and symlinks whose target is outside the root, with 400. Create `/workspace` at start.

`GET /init`, `POST /init`, `/upgrade`, `/collapse` return 404 even with a valid token.

Do not open a vsock in this slice. The host pushes the token via `ENVD_TOKEN`.
