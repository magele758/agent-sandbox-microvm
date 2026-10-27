# E2B SDK contract fixtures

Pinned SDK versions live in `versions.txt`. Fixtures under `fixtures/` are
request/response shapes captured (or reconstructed from the public SDK wire
protocol) for create, exec, read-file, and destroy. Contract tests in
`compat/contract_test.go` exercise our control plane + envd subset against these
shapes without calling api.e2b.app.
