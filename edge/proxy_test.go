package edge

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
)

func TestProxyStartAndGuards(t *testing.T) {
	var guestHits atomic.Int32
	var seenBody string
	guest := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		guestHits.Add(1)
		b, _ := io.ReadAll(r.Body)
		seenBody = string(b)
		if r.URL.Path != "/process.Process/Start" || r.URL.RawQuery != "x=1" {
			t.Errorf("path %s?%s", r.URL.Path, r.URL.RawQuery)
		}
		if r.Header.Get("Connection") != "" || r.Header.Get("Keep-Alive") != "" {
			t.Errorf("hop-by-hop leaked: %v", r.Header)
		}
		w.Header().Set("Location", "http://evil.example/steal")
		w.WriteHeader(http.StatusFound)
		_, _ = w.Write([]byte("proxied"))
	}))
	t.Cleanup(guest.Close)

	api := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer secret" {
			t.Errorf("auth %s", r.Header.Get("Authorization"))
		}
		id := strings.TrimPrefix(r.URL.Path, "/internal/routes/")
		switch id {
		case "sbx_running":
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"sandboxID":"sbx_running","state":"running","guestURL":"`+guest.URL+`","envdAccessToken":"tok","trafficAccessToken":"tr"}`)
		case "sbx_paused":
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"sandboxID":"sbx_paused","state":"paused","guestURL":"`+guest.URL+`","envdAccessToken":"tok"}`)
		default:
			w.WriteHeader(http.StatusNotFound)
			_, _ = io.WriteString(w, `{"code":404,"message":"no"}`)
		}
	}))
	t.Cleanup(api.Close)

	p := &Proxy{APIURL: api.URL, InternalToken: "secret"}
	h := p.Handler()

	initReq := httptest.NewRequest(http.MethodPost, "http://edge/init", strings.NewReader("nope"))
	initReq.Header.Set("E2b-Sandbox-Id", "sbx_running")
	initReq.Header.Set("E2b-Sandbox-Port", "49983")
	initReq.Header.Set("X-Access-Token", "tok")
	initRec := httptest.NewRecorder()
	h.ServeHTTP(initRec, initReq)
	if initRec.Code != 404 || guestHits.Load() != 0 {
		t.Fatalf("init code=%d hits=%d body=%s", initRec.Code, guestHits.Load(), initRec.Body.String())
	}

	bad := httptest.NewRequest(http.MethodPost, "http://edge/process.Process/Start", strings.NewReader(`{"process":{"cmd":"/bin/echo"}}`))
	bad.Host = "49983-sbx_running.127.0.0.1"
	bad.Header.Set("X-Access-Token", "wrong")
	badRec := httptest.NewRecorder()
	h.ServeHTTP(badRec, bad)
	if badRec.Code != 401 || guestHits.Load() != 0 {
		t.Fatalf("bad token code=%d hits=%d body=%s", badRec.Code, guestHits.Load(), badRec.Body.String())
	}

	body := `{"process":{"cmd":"/bin/echo","args":["hi"]}}`
	okReq := httptest.NewRequest(http.MethodPost, "http://edge/process.Process/Start?x=1", strings.NewReader(body))
	okReq.Host = "49983-sbx_running.localhost:3002"
	okReq.Header.Set("X-Access-Token", "tok")
	okReq.Header.Set("Connection", "keep-alive")
	okReq.Header.Set("Keep-Alive", "timeout=5")
	okRec := httptest.NewRecorder()
	h.ServeHTTP(okRec, okReq)
	if okRec.Code != http.StatusFound {
		t.Fatalf("proxy code=%d body=%s", okRec.Code, okRec.Body.String())
	}
	if okRec.Body.String() != "proxied" {
		t.Fatalf("body %q", okRec.Body.String())
	}
	if seenBody != body {
		t.Fatalf("guest body %q", seenBody)
	}
	if loc := okRec.Header().Get("Location"); loc != "http://evil.example/steal" {
		t.Fatalf("redirect followed or rewritten: %s", loc)
	}

	paused := httptest.NewRequest(http.MethodGet, "http://edge/health", nil)
	paused.Header.Set("E2b-Sandbox-Id", "sbx_paused")
	paused.Header.Set("E2b-Sandbox-Port", "49983")
	paused.Header.Set("X-Access-Token", "tok")
	pausedRec := httptest.NewRecorder()
	h.ServeHTTP(pausedRec, paused)
	if pausedRec.Code != 409 || !strings.Contains(pausedRec.Body.String(), "sandbox is not running") {
		t.Fatalf("paused %d %s", pausedRec.Code, pausedRec.Body.String())
	}

	missing := httptest.NewRequest(http.MethodGet, "http://edge/health", nil)
	missing.Host = "1-sbx_missing.127.0.0.1"
	missingRec := httptest.NewRecorder()
	h.ServeHTTP(missingRec, missing)
	if missingRec.Code != 404 || !strings.Contains(missingRec.Body.String(), "sandbox not found") {
		t.Fatalf("missing %d %s", missingRec.Code, missingRec.Body.String())
	}
}
