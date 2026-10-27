package edge

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"time"
)

const (
	headerSandboxID   = "E2b-Sandbox-Id"
	headerSandboxPort = "E2b-Sandbox-Port"
	headerAccessToken = "X-Access-Token"
)

// Route is the control-plane lookup result.
type Route struct {
	SandboxID          string `json:"sandboxID"`
	State              string `json:"state"`
	GuestURL           string `json:"guestURL"`
	EnvdAccessToken    string `json:"envdAccessToken"`
	TrafficAccessToken string `json:"trafficAccessToken"`
}

// Proxy reverse-proxies sandbox traffic to the guest envd URL.
type Proxy struct {
	APIURL        string
	InternalToken string
	Client        *http.Client
	Transport     http.RoundTripper
}

// Handler is the public edge handler.
func (p *Proxy) Handler() http.Handler {
	return http.HandlerFunc(p.serve)
}

func (p *Proxy) client() *http.Client {
	if p.Client != nil {
		return p.Client
	}
	return &http.Client{Timeout: 5 * time.Second}
}

func (p *Proxy) serve(w http.ResponseWriter, r *http.Request) {
	if blocked(r.URL.Path) {
		writeErr(w, http.StatusNotFound, "not found")
		return
	}
	id, ok := sandboxID(r)
	if !ok {
		writeErr(w, http.StatusNotFound, "sandbox not found")
		return
	}
	route, status, msg := p.lookup(r.Context(), id)
	if status != 0 {
		writeErr(w, status, msg)
		return
	}
	if subtle.ConstantTimeCompare([]byte(r.Header.Get(headerAccessToken)), []byte(route.EnvdAccessToken)) != 1 {
		writeErr(w, http.StatusUnauthorized, "unauthorized")
		return
	}
	guest, err := url.Parse(route.GuestURL)
	if err != nil || guest.Scheme == "" || guest.Host == "" {
		writeErr(w, http.StatusBadGateway, "bad guest url")
		return
	}
	rp := &httputil.ReverseProxy{
		Director: func(out *http.Request) {
			out.URL.Scheme = guest.Scheme
			out.URL.Host = guest.Host
			out.URL.Path = r.URL.Path
			out.URL.RawPath = r.URL.RawPath
			out.URL.RawQuery = r.URL.RawQuery
			out.Host = guest.Host
		},
		Transport: p.Transport,
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, _ error) {
			writeErr(w, http.StatusBadGateway, "guest unavailable")
		},
	}
	rp.ServeHTTP(w, r)
}

func blocked(path string) bool {
	switch path {
	case "/init", "/upgrade", "/collapse":
		return true
	default:
		return false
	}
}

func sandboxID(r *http.Request) (string, bool) {
	id := r.Header.Get(headerSandboxID)
	port := r.Header.Get(headerSandboxPort)
	if id != "" && port != "" {
		return id, true
	}
	return parseHost(r.Host)
}

func parseHost(host string) (string, bool) {
	h := host
	if stripped, _, err := net.SplitHostPort(host); err == nil {
		h = stripped
	}
	var suffix string
	switch {
	case strings.HasSuffix(h, ".127.0.0.1"):
		suffix = ".127.0.0.1"
	case strings.HasSuffix(h, ".localhost"):
		suffix = ".localhost"
	default:
		return "", false
	}
	label := strings.TrimSuffix(h, suffix)
	dash := strings.IndexByte(label, '-')
	if dash <= 0 || dash == len(label)-1 {
		return "", false
	}
	port, id := label[:dash], label[dash+1:]
	if port == "" || id == "" || strings.IndexFunc(port, func(r rune) bool { return r < '0' || r > '9' }) >= 0 {
		return "", false
	}
	return id, true
}

func (p *Proxy) lookup(ctx context.Context, id string) (Route, int, string) {
	u := strings.TrimRight(p.APIURL, "/") + "/internal/routes/" + url.PathEscape(id)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return Route{}, http.StatusBadGateway, "route lookup failed"
	}
	req.Header.Set("Authorization", "Bearer "+p.InternalToken)
	resp, err := p.client().Do(req)
	if err != nil {
		return Route{}, http.StatusBadGateway, "route lookup failed"
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if resp.StatusCode == http.StatusNotFound {
		return Route{}, http.StatusNotFound, "sandbox not found"
	}
	if resp.StatusCode != http.StatusOK {
		return Route{}, http.StatusBadGateway, "route lookup failed"
	}
	var route Route
	if err := json.Unmarshal(body, &route); err != nil || route.GuestURL == "" {
		return Route{}, http.StatusBadGateway, "route lookup failed"
	}
	if route.State != "running" {
		return Route{}, http.StatusConflict, "sandbox is not running"
	}
	return route, 0, ""
}

func writeErr(w http.ResponseWriter, code int, message string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(map[string]any{"code": code, "message": message})
}
