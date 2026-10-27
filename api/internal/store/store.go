package store

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"time"
)

var (
	ErrNotFound = errors.New("not found")
	ErrQuota    = errors.New("quota exceeded")
)

const EnvdVersion = "0.1.0"

type Tenant struct {
	ID             string
	Name           string
	MaxConcurrent  int
	Blocked        bool
}

type Template struct {
	ID     string
	VCPU   int
	MemMiB int
	DiskMB int
}

type Sandbox struct {
	ID                   string
	TenantID             string
	TemplateID           string
	State                string
	NodeURL              string
	GuestURL             string
	EnvdAccessToken      string
	TrafficAccessToken   string
	Domain               string
	EnvdVersion          string
	ClientID             string
	CPUCount             int
	MemoryMB             int
	DiskSizeMB           int
	Metadata             json.RawMessage
	AllowInternet        bool
	StartedAt            time.Time
	EndAt                time.Time
	EndedAt              *time.Time
}

type UsageEvent struct {
	ID        int64
	SandboxID string
	TenantID  string
	StartedAt time.Time
	EndedAt   *time.Time
	VCPU      int
	MemMiB    int
	Reason    string
}

type Audit struct {
	ID        int64
	TenantID  string
	Action    string
	SandboxID string
	At        time.Time
}

// Store is the persistence boundary. Implementations must scope tenant reads
// and writes by tenant id, reject creates that would exceed max_concurrent
// running sandboxes, and keep an open usage_events row aligned with a running
// sandbox.
type Store interface {
	Authenticate(ctx context.Context, keyHash string) (Tenant, error)
	GetTemplate(ctx context.Context, id string) (Template, error)
	CountRunning(ctx context.Context, tenantID string) (int, error)
	CreateSandbox(ctx context.Context, sb Sandbox) error
	ListRunning(ctx context.Context, tenantID string) ([]Sandbox, error)
	GetRunning(ctx context.Context, tenantID, id string) (Sandbox, error)
	DeleteRunning(ctx context.Context, tenantID, id string) error
	ExtendEndAt(ctx context.Context, tenantID, id string, endAt time.Time) (Sandbox, error)
	GetByID(ctx context.Context, id string) (Sandbox, error)
}

func HashKey(raw string) string {
	sum := sha256.Sum256([]byte(raw))
	return hex.EncodeToString(sum[:])
}
