# Node API

Listen `:5008`. No tenant auth. This port is not public.

`GET /v1/health` -> `{"status":"ok","kvm":true|false}`

`GET /v1/capacity` -> `{"cpuMillis":<int>,"memMiB":<int>,"committedMemMiB":<int>,"running":<int>,"ready":<bool>}`

`ready` is false when KVM is absent. `committedMemMiB` is the sum of mem of sandboxes in state running, not RSS.

`POST /v1/sandboxes` body:

```json
{
  "sandboxID": "sbx_abc",
  "templateID": "base",
  "vcpu": 2,
  "memMiB": 2048,
  "envVars": {"FOO": "bar"},
  "allowInternet": true,
  "envdAccessToken": "token"
}
```

Success 200: `{"sandboxID","state":"running","guestURL":"http://<slot-ip>:49983"}`

If `/dev/kvm` is missing, or the OS is not Linux, return 503 `{"code":503,"message":"kvm unavailable","error_code":"sandbox_create_refused"}`. Do not start a host process as a stand-in VM.

`DELETE /v1/sandboxes/{id}` -> 204. Unknown id is 404. Must release the slot and subtract committed memory even if the Firecracker process is already dead.

Slot pool: preallocate 16 point-to-point IPs from `172.16.0.0/28` host side `.1` is the host, guests use `.2` onward, one each. Allocation is pure logic and must be unit-tested on any OS.

Firecracker client, Linux only: a function that returns the ordered unix-socket calls for a boot. Do not fork Firecracker. JSON must match upstream:

1. `PUT /machine-config` `{"vcpu_count":2,"mem_size_mib":2048,"smt":false,"track_dirty_pages":false}`
2. `PUT /drives/rootfs` raw ext4 read-write
3. `PUT /network-interfaces/eth0` tap name and guest MAC
4. `PUT /boot-source` kernel path plus `init=/sbin/init`
5. `PUT /actions` `{"action_type":"InstanceStart"}` only after the above

Snapshot load is a separate function, not used by create in this slice: `PUT /snapshot/load` with `snapshot_path`, `mem_backend.backend_path`, `mem_backend.backend_type` = `File`.

Jailer argv builder: `jailer --id <sandboxID> --exec-file <firecracker> --uid 123 --gid 123 --cgroup-version 2`. Unit-test the argv. Do not execute it unless `SANDBOX_ENABLE_KVM=1` and `/dev/kvm` exists.
