package server

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/http"
	"strings"
	"sync"
	"time"

	"sandbox.local/api/internal/nodeclient"
	"sandbox.local/api/internal/store"
)

type Config struct {
	InternalToken string
	Domain        string
	NodeURLs      []string
}

type Server struct {
	store   store.Store
	nodes   *nodeclient.Client
	cfg     Config
	limiter *limiter
}

func New(st store.Store, nodes *nodeclient.Client, cfg Config) *Server {
	if cfg.Domain == "" {
		cfg.Domain = "127.0.0.1"
	}
	if nodes == nil {
		nodes = nodeclient.New()
	}
	return &Server{store: st, nodes: nodes, cfg: cfg, limiter: newLimiter(10, 20)}
}

func (s *Server) Handler() http.Handler { return s }

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	path := r.URL.Path
	switch {
	case path == "/health":
		if r.Method != http.MethodGet {
			writeErr(w, http.StatusNotImplemented, "not implemented", "")
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
		return
	case path == "/internal/routes" || strings.HasPrefix(path, "/internal/routes/"):
		s.internalRoute(w, r)
		return
	case path == "/sandboxes" || path == "/v2/sandboxes":
		s.collection(w, r)
		return
	case strings.HasPrefix(path, "/sandboxes/"):
		s.item(w, r, strings.TrimPrefix(path, "/sandboxes/"))
		return
	default:
		writeErr(w, http.StatusNotImplemented, "not implemented", "")
	}
}

func (s *Server) collection(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodPost:
		s.create(w, r)
	case http.MethodGet:
		s.list(w, r)
	default:
		writeErr(w, http.StatusNotImplemented, "not implemented", "")
	}
}

func (s *Server) item(w http.ResponseWriter, r *http.Request, rest string) {
	id, action, ok := splitAction(rest)
	if !ok || id == "" {
		writeErr(w, http.StatusNotImplemented, "not implemented", "")
		return
	}
	switch {
	case action == "" && r.Method == http.MethodGet:
		s.get(w, r, id)
	case action == "" && r.Method == http.MethodDelete:
		s.delete(w, r, id)
	case action == "timeout" && r.Method == http.MethodPost:
		s.timeout(w, r, id)
	case action == "connect" && r.Method == http.MethodPost:
		s.connect(w, r, id)
	case (action == "pause" || action == "resume" || action == "fork") && r.Method == http.MethodPost:
		if _, ok := s.auth(w, r); !ok {
			return
		}
		writeErr(w, http.StatusNotImplemented, "not implemented", "")
	default:
		writeErr(w, http.StatusNotImplemented, "not implemented", "")
	}
}

func splitAction(rest string) (id, action string, ok bool) {
	rest = strings.Trim(rest, "/")
	if rest == "" || strings.Contains(rest, "..") {
		return "", "", false
	}
	parts := strings.Split(rest, "/")
	if len(parts) == 1 {
		return parts[0], "", true
	}
	if len(parts) == 2 {
		return parts[0], parts[1], true
	}
	return "", "", false
}

type createBody struct {
	TemplateID          string            `json:"templateID"`
	AutoPause           *bool             `json:"autoPause"`
	Secure              *bool             `json:"secure"`
	AllowInternetAccess *bool             `json:"allow_internet_access"`
	EnvVars             map[string]string `json:"envVars"`
	Metadata            json.RawMessage   `json:"metadata"`
	Timeout             *int              `json:"timeout"`
}

func (s *Server) create(w http.ResponseWriter, r *http.Request) {
	tenant, ok := s.auth(w, r)
	if !ok {
		return
	}
	var body createBody
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, http.StatusBadRequest, "invalid json", "")
		return
	}
	if body.AutoPause != nil && *body.AutoPause {
		writeErr(w, http.StatusBadRequest, "autoPause is not available", "")
		return
	}
	if body.TemplateID == "" {
		writeErr(w, http.StatusBadRequest, "templateID is required", "")
		return
	}
	if body.Timeout != nil && *body.Timeout < 0 {
		writeErr(w, http.StatusBadRequest, "timeout must be non-negative", "")
		return
	}
	tpl, err := s.store.GetTemplate(r.Context(), body.TemplateID)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "template not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	n, err := s.store.CountRunning(r.Context(), tenant.ID)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	if n >= tenant.MaxConcurrent {
		writeErr(w, http.StatusTooManyRequests, "quota exceeded", "quota_exceeded")
		return
	}
	timeout := 300
	if body.Timeout != nil {
		timeout = *body.Timeout
	}
	allow := true
	if body.AllowInternetAccess != nil {
		allow = *body.AllowInternetAccess
	}
	meta := body.Metadata
	if len(meta) == 0 || string(meta) == "null" {
		meta = json.RawMessage(`{}`)
	}
	now := time.Now().UTC()
	sb := store.Sandbox{
		ID:                 newSandboxID(),
		TenantID:           tenant.ID,
		TemplateID:         tpl.ID,
		State:              "running",
		EnvdAccessToken:    newToken(),
		TrafficAccessToken: newToken(),
		Domain:             s.cfg.Domain,
		EnvdVersion:        store.EnvdVersion,
		ClientID:           newUUID(),
		CPUCount:           tpl.VCPU,
		MemoryMB:           tpl.MemMiB,
		DiskSizeMB:         tpl.DiskMB,
		Metadata:           meta,
		AllowInternet:      allow,
		StartedAt:          now,
		EndAt:              now.Add(time.Duration(timeout) * time.Second),
	}
	nodeURL, created, err := s.nodes.Place(r.Context(), s.cfg.NodeURLs, nodeclient.CreateRequest{
		SandboxID:       sb.ID,
		TemplateID:      sb.TemplateID,
		VCPU:            sb.CPUCount,
		MemMiB:          sb.MemoryMB,
		EnvVars:         body.EnvVars,
		AllowInternet:   allow,
		EnvdAccessToken: sb.EnvdAccessToken,
	})
	if err != nil {
		if errors.Is(err, nodeclient.ErrNoCompatible) {
			writeErr(w, http.StatusServiceUnavailable, "no compatible node", "sandbox_no_compatible_node")
			return
		}
		writeErr(w, http.StatusServiceUnavailable, "sandbox create failed", "sandbox_create_failed")
		return
	}
	sb.NodeURL = nodeURL
	sb.GuestURL = created.GuestURL
	if err := s.store.CreateSandbox(r.Context(), sb); err != nil {
		_ = s.nodes.Delete(r.Context(), nodeURL, sb.ID)
		if errors.Is(err, store.ErrQuota) {
			writeErr(w, http.StatusTooManyRequests, "quota exceeded", "quota_exceeded")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	_ = body.Secure
	writeJSON(w, http.StatusCreated, sandboxView(sb, true))
}

func (s *Server) list(w http.ResponseWriter, r *http.Request) {
	tenant, ok := s.auth(w, r)
	if !ok {
		return
	}
	items, err := s.store.ListRunning(r.Context(), tenant.ID)
	if err != nil {
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	out := make([]sandboxJSON, 0, len(items))
	for _, sb := range items {
		out = append(out, sandboxView(sb, true))
	}
	writeJSON(w, http.StatusOK, out)
}

func (s *Server) get(w http.ResponseWriter, r *http.Request, id string) {
	tenant, ok := s.auth(w, r)
	if !ok {
		return
	}
	sb, err := s.store.GetRunning(r.Context(), tenant.ID, id)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "sandbox not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	writeJSON(w, http.StatusOK, sandboxView(sb, true))
}

func (s *Server) delete(w http.ResponseWriter, r *http.Request, id string) {
	tenant, ok := s.auth(w, r)
	if !ok {
		return
	}
	sb, err := s.store.GetRunning(r.Context(), tenant.ID, id)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "sandbox not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	if err := s.nodes.Delete(r.Context(), sb.NodeURL, sb.ID); err != nil {
		writeErr(w, http.StatusBadGateway, "node delete failed", "")
		return
	}
	if err := s.store.DeleteRunning(r.Context(), tenant.ID, id); err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "sandbox not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

type timeoutBody struct {
	Timeout *int `json:"timeout"`
}

func (s *Server) timeout(w http.ResponseWriter, r *http.Request, id string) {
	tenant, ok := s.auth(w, r)
	if !ok {
		return
	}
	var body timeoutBody
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, http.StatusBadRequest, "invalid json", "")
		return
	}
	if body.Timeout == nil {
		writeErr(w, http.StatusBadRequest, "timeout is required", "")
		return
	}
	if *body.Timeout < 0 {
		writeErr(w, http.StatusBadRequest, "timeout must be non-negative", "")
		return
	}
	end := time.Now().UTC().Add(time.Duration(*body.Timeout) * time.Second)
	if _, err := s.store.ExtendEndAt(r.Context(), tenant.ID, id, end); err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "sandbox not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) connect(w http.ResponseWriter, r *http.Request, id string) {
	tenant, ok := s.auth(w, r)
	if !ok {
		return
	}
	var body timeoutBody
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, http.StatusBadRequest, "invalid json", "")
		return
	}
	if body.Timeout == nil {
		writeErr(w, http.StatusBadRequest, "timeout is required", "")
		return
	}
	if *body.Timeout < 0 {
		writeErr(w, http.StatusBadRequest, "timeout must be non-negative", "")
		return
	}
	cur, err := s.store.GetRunning(r.Context(), tenant.ID, id)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "sandbox not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	if cur.State != "running" {
		writeErr(w, http.StatusBadRequest, "sandbox is not running", "")
		return
	}
	end := time.Now().UTC().Add(time.Duration(*body.Timeout) * time.Second)
	sb, err := s.store.ExtendEndAt(r.Context(), tenant.ID, id, end)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "sandbox not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	writeJSON(w, http.StatusOK, sandboxView(sb, true))
}

func (s *Server) internalRoute(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeErr(w, http.StatusNotImplemented, "not implemented", "")
		return
	}
	if !s.internalAuth(r) {
		writeErr(w, http.StatusUnauthorized, "unauthorized", "")
		return
	}
	id := strings.TrimPrefix(r.URL.Path, "/internal/routes/")
	if id == "" || id == r.URL.Path || strings.Contains(id, "/") {
		writeErr(w, http.StatusNotFound, "sandbox not found", "")
		return
	}
	sb, err := s.store.GetByID(r.Context(), id)
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			writeErr(w, http.StatusNotFound, "sandbox not found", "")
			return
		}
		writeErr(w, http.StatusInternalServerError, "internal error", "")
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{
		"sandboxID":           sb.ID,
		"state":               sb.State,
		"guestURL":            sb.GuestURL,
		"envdAccessToken":     sb.EnvdAccessToken,
		"trafficAccessToken":  sb.TrafficAccessToken,
	})
}

func (s *Server) auth(w http.ResponseWriter, r *http.Request) (store.Tenant, bool) {
	key := r.Header.Get("X-API-Key")
	if key == "" {
		writeErr(w, http.StatusUnauthorized, "missing api key", "")
		return store.Tenant{}, false
	}
	if !s.limiter.allow(key) {
		writeErr(w, http.StatusTooManyRequests, "rate limit exceeded", "")
		return store.Tenant{}, false
	}
	t, err := s.store.Authenticate(r.Context(), store.HashKey(key))
	if err != nil {
		writeErr(w, http.StatusUnauthorized, "invalid api key", "")
		return store.Tenant{}, false
	}
	if t.Blocked {
		writeErr(w, http.StatusForbidden, "tenant blocked", "")
		return store.Tenant{}, false
	}
	return t, true
}

func (s *Server) internalAuth(r *http.Request) bool {
	if s.cfg.InternalToken == "" {
		return false
	}
	h := r.Header.Get("Authorization")
	const p = "Bearer "
	if !strings.HasPrefix(h, p) {
		return false
	}
	return h[len(p):] == s.cfg.InternalToken
}

type sandboxJSON struct {
	TemplateID         string `json:"templateID"`
	SandboxID          string `json:"sandboxID"`
	ClientID           string `json:"clientID"`
	EnvdVersion        string `json:"envdVersion"`
	EnvdAccessToken    string `json:"envdAccessToken"`
	TrafficAccessToken string `json:"trafficAccessToken"`
	Domain             string `json:"domain"`
	StartedAt          string `json:"startedAt"`
	EndAt              string `json:"endAt"`
	CPUCount           int    `json:"cpuCount"`
	MemoryMB           int    `json:"memoryMB"`
	DiskSizeMB         int    `json:"diskSizeMB"`
	State              string `json:"state"`
}

func sandboxView(sb store.Sandbox, _ bool) sandboxJSON {
	return sandboxJSON{
		TemplateID:         sb.TemplateID,
		SandboxID:          sb.ID,
		ClientID:           sb.ClientID,
		EnvdVersion:        sb.EnvdVersion,
		EnvdAccessToken:    sb.EnvdAccessToken,
		TrafficAccessToken: sb.TrafficAccessToken,
		Domain:             sb.Domain,
		StartedAt:          sb.StartedAt.UTC().Format(time.RFC3339),
		EndAt:              sb.EndAt.UTC().Format(time.RFC3339),
		CPUCount:           sb.CPUCount,
		MemoryMB:           sb.MemoryMB,
		DiskSizeMB:         sb.DiskSizeMB,
		State:              sb.State,
	}
}

type errJSON struct {
	Code      int    `json:"code"`
	Message   string `json:"message"`
	ErrorCode string `json:"error_code,omitempty"`
}

func writeErr(w http.ResponseWriter, status int, msg, errorCode string) {
	if status == http.StatusTooManyRequests {
		w.Header().Set("Retry-After", "1")
	}
	writeJSON(w, status, errJSON{Code: status, Message: msg, ErrorCode: errorCode})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func decodeJSON(r *http.Request, dst any) error {
	dec := json.NewDecoder(r.Body)
	if err := dec.Decode(dst); err != nil {
		return err
	}
	return nil
}

func newSandboxID() string {
	var b [8]byte
	_, _ = rand.Read(b[:])
	return "sbx_" + hex.EncodeToString(b[:])
}

func newToken() string {
	var b [32]byte
	_, _ = rand.Read(b[:])
	return hex.EncodeToString(b[:])
}

func newUUID() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return hex.EncodeToString(b[0:4]) + "-" + hex.EncodeToString(b[4:6]) + "-" + hex.EncodeToString(b[6:8]) + "-" + hex.EncodeToString(b[8:10]) + "-" + hex.EncodeToString(b[10:])
}

type limiter struct {
	mu     sync.Mutex
	rate   float64
	burst  float64
	bucket map[string]*bucket
}

type bucket struct {
	tokens float64
	last   time.Time
}

func newLimiter(perSec, burst int) *limiter {
	return &limiter{rate: float64(perSec), burst: float64(burst), bucket: map[string]*bucket{}}
}

func (l *limiter) allow(key string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := time.Now()
	b := l.bucket[key]
	if b == nil {
		b = &bucket{tokens: l.burst, last: now}
		l.bucket[key] = b
	}
	b.tokens += now.Sub(b.last).Seconds() * l.rate
	if b.tokens > l.burst {
		b.tokens = l.burst
	}
	b.last = now
	if b.tokens < 1 {
		return false
	}
	b.tokens -= 1
	return true
}
