package store

import (
	"context"
	"encoding/json"
	"sync"
	"time"
)

// Memory is a test double. It is not a production driver.
type Memory struct {
	mu        sync.Mutex
	tenants   map[string]Tenant
	keys      map[string]string // key hash -> tenant id
	templates map[string]Template
	boxes     map[string]Sandbox
	usage     []UsageEvent
	audits    []Audit
	nextUsage int64
	nextAudit int64
}

func NewMemory() *Memory {
	m := &Memory{
		tenants:   map[string]Tenant{},
		keys:      map[string]string{},
		templates: map[string]Template{},
		boxes:     map[string]Sandbox{},
	}
	m.templates["base"] = Template{ID: "base", VCPU: 2, MemMiB: 2048, DiskMB: 10240}
	return m
}

// AddTenant inserts a tenant and API key. rawKey is hashed the same way as production.
func (m *Memory) AddTenant(id, name, rawKey string, maxConcurrent int, blocked bool) Tenant {
	m.mu.Lock()
	defer m.mu.Unlock()
	if maxConcurrent == 0 {
		maxConcurrent = 20
	}
	t := Tenant{ID: id, Name: name, MaxConcurrent: maxConcurrent, Blocked: blocked}
	m.tenants[id] = t
	if rawKey != "" {
		m.keys[HashKey(rawKey)] = id
	}
	return t
}

// SetMaxConcurrent overrides the quota. Zero is a real limit, unlike AddTenant.
func (m *Memory) SetMaxConcurrent(tenantID string, n int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	t := m.tenants[tenantID]
	t.MaxConcurrent = n
	m.tenants[tenantID] = t
}

func (m *Memory) OpenUsage(sandboxID string) (UsageEvent, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	for i := len(m.usage) - 1; i >= 0; i-- {
		if m.usage[i].SandboxID == sandboxID {
			return m.usage[i], true
		}
	}
	return UsageEvent{}, false
}

func (m *Memory) AuditsFor(sandboxID string) []Audit {
	m.mu.Lock()
	defer m.mu.Unlock()
	var out []Audit
	for _, a := range m.audits {
		if a.SandboxID == sandboxID {
			out = append(out, a)
		}
	}
	return out
}

func (m *Memory) Authenticate(_ context.Context, keyHash string) (Tenant, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	tid, ok := m.keys[keyHash]
	if !ok {
		return Tenant{}, ErrNotFound
	}
	t, ok := m.tenants[tid]
	if !ok {
		return Tenant{}, ErrNotFound
	}
	return t, nil
}

func (m *Memory) GetTemplate(_ context.Context, id string) (Template, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	t, ok := m.templates[id]
	if !ok {
		return Template{}, ErrNotFound
	}
	return t, nil
}

func (m *Memory) CountRunning(_ context.Context, tenantID string) (int, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.countRunning(tenantID), nil
}

func (m *Memory) countRunning(tenantID string) int {
	n := 0
	for _, sb := range m.boxes {
		if sb.TenantID == tenantID && sb.State == "running" {
			n++
		}
	}
	return n
}

func (m *Memory) CreateSandbox(_ context.Context, sb Sandbox) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	t, ok := m.tenants[sb.TenantID]
	if !ok {
		return ErrNotFound
	}
	if m.countRunning(sb.TenantID) >= t.MaxConcurrent {
		return ErrQuota
	}
	if _, exists := m.boxes[sb.ID]; exists {
		return ErrQuota
	}
	if sb.Metadata == nil {
		sb.Metadata = json.RawMessage(`{}`)
	}
	sb.State = "running"
	sb.EndedAt = nil
	m.boxes[sb.ID] = sb
	m.nextUsage++
	m.usage = append(m.usage, UsageEvent{
		ID:        m.nextUsage,
		SandboxID: sb.ID,
		TenantID:  sb.TenantID,
		StartedAt: sb.StartedAt,
		VCPU:      sb.CPUCount,
		MemMiB:    sb.MemoryMB,
		Reason:    "running",
	})
	m.nextAudit++
	m.audits = append(m.audits, Audit{
		ID:        m.nextAudit,
		TenantID:  sb.TenantID,
		Action:    "create",
		SandboxID: sb.ID,
		At:        sb.StartedAt,
	})
	return nil
}

func (m *Memory) ListRunning(_ context.Context, tenantID string) ([]Sandbox, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := []Sandbox{}
	for _, sb := range m.boxes {
		if sb.TenantID == tenantID && sb.State == "running" {
			out = append(out, sb)
		}
	}
	return out, nil
}

func (m *Memory) GetRunning(_ context.Context, tenantID, id string) (Sandbox, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	sb, ok := m.boxes[id]
	if !ok || sb.TenantID != tenantID || sb.State != "running" {
		return Sandbox{}, ErrNotFound
	}
	return sb, nil
}

func (m *Memory) DeleteRunning(_ context.Context, tenantID, id string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	sb, ok := m.boxes[id]
	if !ok || sb.TenantID != tenantID || sb.State != "running" {
		return ErrNotFound
	}
	now := time.Now().UTC()
	sb.State = "killed"
	sb.EndedAt = &now
	m.boxes[id] = sb
	for i := range m.usage {
		if m.usage[i].SandboxID == id && m.usage[i].TenantID == tenantID && m.usage[i].EndedAt == nil {
			t := now
			m.usage[i].EndedAt = &t
		}
	}
	m.nextAudit++
	m.audits = append(m.audits, Audit{
		ID:        m.nextAudit,
		TenantID:  tenantID,
		Action:    "delete",
		SandboxID: id,
		At:        now,
	})
	return nil
}

func (m *Memory) ExtendEndAt(_ context.Context, tenantID, id string, endAt time.Time) (Sandbox, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	sb, ok := m.boxes[id]
	if !ok || sb.TenantID != tenantID || sb.State != "running" {
		return Sandbox{}, ErrNotFound
	}
	sb.EndAt = endAt.UTC()
	m.boxes[id] = sb
	return sb, nil
}

func (m *Memory) GetByID(_ context.Context, id string) (Sandbox, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	sb, ok := m.boxes[id]
	if !ok {
		return Sandbox{}, ErrNotFound
	}
	return sb, nil
}
