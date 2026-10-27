package compat_test

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func fixturesDir(t *testing.T) string {
	t.Helper()
	dir := "fixtures"
	if _, err := os.Stat(dir); err != nil {
		dir = filepath.Join("compat", "fixtures")
	}
	return dir
}

func loadJSON(t *testing.T, name string) map[string]any {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(fixturesDir(t), name))
	if err != nil {
		t.Fatalf("read %s: %v", name, err)
	}
	var m map[string]any
	if err := json.Unmarshal(b, &m); err != nil {
		t.Fatalf("json %s: %v", name, err)
	}
	return m
}

func TestPinnedVersionsFile(t *testing.T) {
	b, err := os.ReadFile("versions.txt")
	if err != nil {
		b, err = os.ReadFile(filepath.Join("compat", "versions.txt"))
	}
	if err != nil {
		t.Fatal(err)
	}
	s := string(b)
	if !contains(s, "python=e2b==") || !contains(s, "javascript=@e2b/") {
		t.Fatalf("versions.txt missing pinned python/js SDKs:\n%s", s)
	}
}

func TestCreateFixtureShape(t *testing.T) {
	req := loadJSON(t, "create.request.json")
	if req["method"] != "POST" || req["path"] != "/v2/sandboxes" {
		t.Fatalf("create request shape: %#v", req)
	}
	headers, _ := req["headers"].(map[string]any)
	if headers["X-API-Key"] == nil {
		t.Fatal("create requires X-API-Key")
	}
	body, _ := req["body"].(map[string]any)
	if body["templateID"] == nil {
		t.Fatal("create body requires templateID")
	}
	resp := loadJSON(t, "create.response.json")
	fields, _ := resp["required_fields"].([]any)
	need := map[string]bool{"sandboxID": true, "envdAccessToken": true, "domain": true}
	for _, f := range fields {
		delete(need, f.(string))
	}
	if len(need) > 0 {
		t.Fatalf("create response missing required fields: %v", need)
	}
}

func TestExecFixtureShape(t *testing.T) {
	req := loadJSON(t, "exec.request.json")
	if req["path"] != "/process.Process/Start" {
		t.Fatalf("exec path: %v", req["path"])
	}
	headers, _ := req["headers"].(map[string]any)
	for _, h := range []string{"E2b-Sandbox-Id", "E2b-Sandbox-Port", "X-Access-Token"} {
		if headers[h] == nil {
			t.Fatalf("exec missing header %s", h)
		}
	}
	body, _ := req["body"].(map[string]any)
	proc, _ := body["process"].(map[string]any)
	if proc["cmd"] == nil {
		t.Fatal("exec body.process.cmd required")
	}
}

func TestReadFileAndDestroyFixtures(t *testing.T) {
	read := loadJSON(t, "read-file.request.json")
	if read["path"] != "/filesystem.Filesystem/Read" {
		t.Fatalf("read path: %v", read["path"])
	}
	destroy := loadJSON(t, "destroy.request.json")
	if destroy["method"] != "DELETE" {
		t.Fatalf("destroy method: %v", destroy["method"])
	}
}

func contains(s, sub string) bool {
	return len(s) >= len(sub) && (s == sub || len(sub) == 0 ||
		(func() bool {
			for i := 0; i+len(sub) <= len(s); i++ {
				if s[i:i+len(sub)] == sub {
					return true
				}
			}
			return false
		})())
}
