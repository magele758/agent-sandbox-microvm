package server

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"sandbox.local/api/internal/nodeclient"
	"sandbox.local/api/internal/store"
)

func TestCreateTenantIsolationAndUsage(t *testing.T) {
	var gotEnv string
	node := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v1/capacity":
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"cpuMillis":4000,"memMiB":8192,"committedMemMiB":0,"running":0,"ready":true}`))
		case r.Method == http.MethodPost && r.URL.Path == "/v1/sandboxes":
			var body map[string]any
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Errorf("decode node body: %v", err)
				http.Error(w, "bad", 400)
				return
			}
			env, _ := body["envVars"].(map[string]any)
			gotEnv, _ = env["FOO"].(string)
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(map[string]string{
				"sandboxID": body["sandboxID"].(string),
				"state":     "running",
				"guestURL":  "http://172.16.0.2:49983",
			})
		case r.Method == http.MethodDelete && strings.HasPrefix(r.URL.Path, "/v1/sandboxes/"):
			w.WriteHeader(http.StatusNoContent)
		default:
			http.NotFound(w, r)
		}
	}))
	defer node.Close()

	mem := store.NewMemory()
	mem.AddTenant("11111111-1111-4111-8111-111111111111", "a", "key-a", 20, false)
	mem.AddTenant("22222222-2222-4222-8222-222222222222", "b", "key-b", 20, false)
	h := New(mem, nodeclient.New(), Config{
		InternalToken: "internal-secret",
		Domain:        "127.0.0.1",
		NodeURLs:      []string{node.URL},
	})

	req := httptest.NewRequest(http.MethodPost, "/sandboxes", strings.NewReader(`{"templateID":"base","envVars":{"FOO":"bar"},"metadata":{"k":"v"},"secure":true}`))
	req.Header.Set("X-API-Key", "key-a")
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusCreated {
		t.Fatalf("create status %d body %s", rec.Code, rec.Body.String())
	}
	var created map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &created); err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"templateID", "sandboxID", "clientID", "envdVersion", "envdAccessToken", "trafficAccessToken", "domain"} {
		v, ok := created[field]
		if !ok || v == "" {
			t.Fatalf("missing %s in %s", field, rec.Body.String())
		}
	}
	if created["templateID"] != "base" || created["envdVersion"] != "0.1.0" || created["domain"] != "127.0.0.1" {
		t.Fatalf("unexpected create body %s", rec.Body.String())
	}
	id, _ := created["sandboxID"].(string)
	if !strings.HasPrefix(id, "sbx_") || len(id) != len("sbx_")+16 {
		t.Fatalf("sandbox id %q", id)
	}
	if gotEnv != "bar" {
		t.Fatalf("env var not forwarded: %q", gotEnv)
	}
	usage, ok := mem.OpenUsage(id)
	if !ok || usage.EndedAt != nil {
		t.Fatalf("expected open usage, %+v ok=%v", usage, ok)
	}
	audits := mem.AuditsFor(id)
	if len(audits) != 1 || audits[0].Action != "create" || audits[0].TenantID != "11111111-1111-4111-8111-111111111111" {
		t.Fatalf("audit %+v", audits)
	}

	req = httptest.NewRequest(http.MethodGet, "/sandboxes/"+id, nil)
	req.Header.Set("X-API-Key", "key-b")
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusNotFound {
		t.Fatalf("other tenant status %d", rec.Code)
	}

	req = httptest.NewRequest(http.MethodGet, "/internal/routes/"+id, nil)
	req.Header.Set("Authorization", "Bearer internal-secret")
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("route status %d %s", rec.Code, rec.Body.String())
	}
	var route map[string]string
	_ = json.Unmarshal(rec.Body.Bytes(), &route)
	if route["guestURL"] != "http://172.16.0.2:49983" || route["sandboxID"] != id || route["state"] != "running" {
		t.Fatalf("route %v", route)
	}

	req = httptest.NewRequest(http.MethodDelete, "/sandboxes/"+id, nil)
	req.Header.Set("X-API-Key", "key-a")
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusNoContent {
		t.Fatalf("delete %d %s", rec.Code, rec.Body.String())
	}
	usage, ok = mem.OpenUsage(id)
	if !ok || usage.EndedAt == nil {
		t.Fatalf("usage not closed: %+v", usage)
	}
	audits = mem.AuditsFor(id)
	if len(audits) != 2 || audits[1].Action != "delete" {
		t.Fatalf("delete audit %+v", audits)
	}

	req = httptest.NewRequest(http.MethodGet, "/sandboxes/"+id, nil)
	req.Header.Set("X-API-Key", "key-a")
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusNotFound {
		t.Fatalf("get after delete %d", rec.Code)
	}
}

func TestAuthAutoPausePauseQuota(t *testing.T) {
	mem := store.NewMemory()
	mem.AddTenant("33333333-3333-4333-8333-333333333333", "c", "key-c", 20, false)
	mem.SetMaxConcurrent("33333333-3333-4333-8333-333333333333", 0)
	h := New(mem, nodeclient.New(), Config{Domain: "127.0.0.1"})

	req := httptest.NewRequest(http.MethodPost, "/v2/sandboxes", strings.NewReader(`{"templateID":"base"}`))
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("401 got %d", rec.Code)
	}
	assertErrJSON(t, rec)

	req = httptest.NewRequest(http.MethodPost, "/sandboxes", strings.NewReader(`{"templateID":"base","autoPause":true}`))
	req.Header.Set("X-API-Key", "key-c")
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("autoPause %d %s", rec.Code, rec.Body.String())
	}
	var body map[string]any
	_ = json.Unmarshal(rec.Body.Bytes(), &body)
	if body["message"] != "autoPause is not available" {
		t.Fatalf("message %v", body["message"])
	}

	req = httptest.NewRequest(http.MethodPost, "/sandboxes/sbx_deadbeefdeadbeef/pause", nil)
	req.Header.Set("X-API-Key", "key-c")
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusNotImplemented {
		t.Fatalf("pause %d", rec.Code)
	}
	assertErrJSON(t, rec)

	req = httptest.NewRequest(http.MethodPost, "/sandboxes", strings.NewReader(`{"templateID":"base"}`))
	req.Header.Set("X-API-Key", "key-c")
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusTooManyRequests {
		t.Fatalf("quota %d %s", rec.Code, rec.Body.String())
	}
	if rec.Header().Get("Retry-After") != "1" {
		t.Fatalf("retry-after %q", rec.Header().Get("Retry-After"))
	}
	assertErrJSON(t, rec)
}

func assertErrJSON(t *testing.T, rec *httptest.ResponseRecorder) {
	t.Helper()
	var body map[string]any
	b, _ := io.ReadAll(rec.Body)
	if err := json.Unmarshal(b, &body); err != nil {
		t.Fatalf("error json: %v %s", err, b)
	}
	if body["code"] == nil || body["message"] == "" {
		t.Fatalf("error body %s", b)
	}
}
