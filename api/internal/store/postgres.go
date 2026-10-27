package store

import (
	"context"
	"crypto/rand"
	"embed"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

//go:embed schema.sql
var schemaFS embed.FS

type Postgres struct {
	pool *pgxpool.Pool
}

func NewPostgres(ctx context.Context, databaseURL string) (*Postgres, error) {
	cfg, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		return nil, fmt.Errorf("parse database url: %w", err)
	}
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		return nil, fmt.Errorf("connect postgres: %w", err)
	}
	return &Postgres{pool: pool}, nil
}

func (p *Postgres) Close() {
	p.pool.Close()
}

func (p *Postgres) Init(ctx context.Context, bootstrapAPIKey string) error {
	var exists bool
	if err := p.pool.QueryRow(ctx, `SELECT EXISTS (
		SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_name = 'tenants'
	)`).Scan(&exists); err != nil {
		return err
	}
	if !exists {
		tx, err := p.pool.Begin(ctx)
		if err != nil {
			return err
		}
		schemaSQL, err := schemaFS.ReadFile("schema.sql")
		if err != nil {
			return err
		}
		for _, stmt := range splitSQL(string(schemaSQL)) {
			if _, err := tx.Exec(ctx, stmt); err != nil {
				return fmt.Errorf("apply schema: %w", err)
			}
		}
		if err := tx.Commit(ctx); err != nil {
			return err
		}
	}
	if _, err := p.pool.Exec(ctx, `INSERT INTO templates (id, vcpu, mem_mib, disk_mb) VALUES ('base', 2, 2048, 10240) ON CONFLICT (id) DO NOTHING`); err != nil {
		return fmt.Errorf("seed template: %w", err)
	}
	if bootstrapAPIKey == "" {
		return nil
	}
	var n int
	if err := p.pool.QueryRow(ctx, `SELECT count(*) FROM api_keys`).Scan(&n); err != nil {
		return err
	}
	if n > 0 {
		return nil
	}
	tenantID, err := newUUID()
	if err != nil {
		return err
	}
	keyID, err := newUUID()
	if err != nil {
		return err
	}
	prefix := bootstrapAPIKey
	if len(prefix) > 8 {
		prefix = prefix[:8]
	}
	tx, err := p.pool.Begin(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback(ctx)
	if _, err := tx.Exec(ctx, `INSERT INTO tenants (id, name) VALUES ($1::uuid, 'bootstrap')`, tenantID); err != nil {
		return err
	}
	if _, err := tx.Exec(ctx, `INSERT INTO api_keys (id, tenant_id, prefix, key_hash) VALUES ($1::uuid, $2::uuid, $3, $4)`, keyID, tenantID, prefix, HashKey(bootstrapAPIKey)); err != nil {
		return err
	}
	return tx.Commit(ctx)
}

func (p *Postgres) Authenticate(ctx context.Context, keyHash string) (Tenant, error) {
	var t Tenant
	err := p.pool.QueryRow(ctx, `
		SELECT t.id::text, t.name, t.max_concurrent, t.blocked
		FROM api_keys k
		JOIN tenants t ON t.id = k.tenant_id
		WHERE k.key_hash = $1 AND k.revoked_at IS NULL
	`, keyHash).Scan(&t.ID, &t.Name, &t.MaxConcurrent, &t.Blocked)
	if err != nil {
		if err == pgx.ErrNoRows {
			return Tenant{}, ErrNotFound
		}
		return Tenant{}, err
	}
	return t, nil
}

func (p *Postgres) GetTemplate(ctx context.Context, id string) (Template, error) {
	var t Template
	err := p.pool.QueryRow(ctx, `SELECT id, vcpu, mem_mib, disk_mb FROM templates WHERE id = $1`, id).Scan(&t.ID, &t.VCPU, &t.MemMiB, &t.DiskMB)
	if err != nil {
		if err == pgx.ErrNoRows {
			return Template{}, ErrNotFound
		}
		return Template{}, err
	}
	return t, nil
}

func (p *Postgres) CountRunning(ctx context.Context, tenantID string) (int, error) {
	var n int
	err := p.pool.QueryRow(ctx, `SELECT count(*) FROM sandboxes WHERE tenant_id = $1::uuid AND state = 'running'`, tenantID).Scan(&n)
	return n, err
}

func (p *Postgres) CreateSandbox(ctx context.Context, sb Sandbox) error {
	tx, err := p.pool.Begin(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback(ctx)
	var max int
	err = tx.QueryRow(ctx, `SELECT max_concurrent FROM tenants WHERE id = $1::uuid FOR UPDATE`, sb.TenantID).Scan(&max)
	if err != nil {
		if err == pgx.ErrNoRows {
			return ErrNotFound
		}
		return err
	}
	var n int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM sandboxes WHERE tenant_id = $1::uuid AND state = 'running'`, sb.TenantID).Scan(&n); err != nil {
		return err
	}
	if n >= max {
		return ErrQuota
	}
	meta := sb.Metadata
	if len(meta) == 0 {
		meta = json.RawMessage(`{}`)
	}
	_, err = tx.Exec(ctx, `
		INSERT INTO sandboxes (
			id, tenant_id, template_id, state, node_url, guest_url,
			envd_access_token, traffic_access_token, domain, envd_version, client_id,
			cpu_count, memory_mb, disk_size_mb, metadata, allow_internet, started_at, end_at
		) VALUES (
			$1, $2::uuid, $3, 'running', $4, $5,
			$6, $7, $8, $9, $10,
			$11, $12, $13, $14::jsonb, $15, $16, $17
		)`,
		sb.ID, sb.TenantID, sb.TemplateID, sb.NodeURL, sb.GuestURL,
		sb.EnvdAccessToken, sb.TrafficAccessToken, sb.Domain, sb.EnvdVersion, sb.ClientID,
		sb.CPUCount, sb.MemoryMB, sb.DiskSizeMB, string(meta), sb.AllowInternet, sb.StartedAt, sb.EndAt,
	)
	if err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `
		INSERT INTO usage_events (sandbox_id, tenant_id, started_at, ended_at, vcpu, mem_mib, reason)
		VALUES ($1, $2::uuid, $3, NULL, $4, $5, 'running')`,
		sb.ID, sb.TenantID, sb.StartedAt, sb.CPUCount, sb.MemoryMB,
	)
	if err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `
		INSERT INTO audit_log (tenant_id, action, sandbox_id, at)
		VALUES ($1::uuid, 'create', $2, $3)`,
		sb.TenantID, sb.ID, sb.StartedAt,
	)
	if err != nil {
		return err
	}
	return tx.Commit(ctx)
}

func (p *Postgres) ListRunning(ctx context.Context, tenantID string) ([]Sandbox, error) {
	rows, err := p.pool.Query(ctx, sandboxSelect+` WHERE tenant_id = $1::uuid AND state = 'running' ORDER BY started_at`, tenantID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Sandbox{}
	for rows.Next() {
		sb, err := scanSandbox(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, sb)
	}
	return out, rows.Err()
}

func (p *Postgres) GetRunning(ctx context.Context, tenantID, id string) (Sandbox, error) {
	rows, err := p.pool.Query(ctx, sandboxSelect+` WHERE id = $1 AND tenant_id = $2::uuid AND state = 'running'`, id, tenantID)
	if err != nil {
		return Sandbox{}, err
	}
	defer rows.Close()
	if !rows.Next() {
		return Sandbox{}, ErrNotFound
	}
	return scanSandbox(rows)
}

func (p *Postgres) DeleteRunning(ctx context.Context, tenantID, id string) error {
	tx, err := p.pool.Begin(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback(ctx)
	var state string
	err = tx.QueryRow(ctx, `SELECT state FROM sandboxes WHERE id = $1 AND tenant_id = $2::uuid FOR UPDATE`, id, tenantID).Scan(&state)
	if err != nil {
		if err == pgx.ErrNoRows {
			return ErrNotFound
		}
		return err
	}
	if state != "running" {
		return ErrNotFound
	}
	now := time.Now().UTC()
	if _, err := tx.Exec(ctx, `UPDATE sandboxes SET state = 'killed', ended_at = $3 WHERE id = $1 AND tenant_id = $2::uuid`, id, tenantID, now); err != nil {
		return err
	}
	if _, err := tx.Exec(ctx, `UPDATE usage_events SET ended_at = $3 WHERE sandbox_id = $1 AND tenant_id = $2::uuid AND ended_at IS NULL`, id, tenantID, now); err != nil {
		return err
	}
	if _, err := tx.Exec(ctx, `INSERT INTO audit_log (tenant_id, action, sandbox_id, at) VALUES ($1::uuid, 'delete', $2, $3)`, tenantID, id, now); err != nil {
		return err
	}
	return tx.Commit(ctx)
}

func (p *Postgres) ExtendEndAt(ctx context.Context, tenantID, id string, endAt time.Time) (Sandbox, error) {
	tag, err := p.pool.Exec(ctx, `UPDATE sandboxes SET end_at = $3 WHERE id = $1 AND tenant_id = $2::uuid AND state = 'running'`, id, tenantID, endAt.UTC())
	if err != nil {
		return Sandbox{}, err
	}
	if tag.RowsAffected() == 0 {
		return Sandbox{}, ErrNotFound
	}
	return p.GetRunning(ctx, tenantID, id)
}

func (p *Postgres) GetByID(ctx context.Context, id string) (Sandbox, error) {
	rows, err := p.pool.Query(ctx, sandboxSelect+` WHERE id = $1`, id)
	if err != nil {
		return Sandbox{}, err
	}
	defer rows.Close()
	if !rows.Next() {
		return Sandbox{}, ErrNotFound
	}
	return scanSandbox(rows)
}

const sandboxSelect = `
	SELECT id, tenant_id::text, template_id, state, COALESCE(node_url, ''), COALESCE(guest_url, ''),
		envd_access_token, traffic_access_token, domain, envd_version, client_id,
		cpu_count, memory_mb, disk_size_mb, metadata, allow_internet, started_at, end_at, ended_at
	FROM sandboxes`

type rowScanner interface {
	Scan(dest ...any) error
}

func scanSandbox(row rowScanner) (Sandbox, error) {
	var sb Sandbox
	var meta []byte
	var allow *bool
	err := row.Scan(
		&sb.ID, &sb.TenantID, &sb.TemplateID, &sb.State, &sb.NodeURL, &sb.GuestURL,
		&sb.EnvdAccessToken, &sb.TrafficAccessToken, &sb.Domain, &sb.EnvdVersion, &sb.ClientID,
		&sb.CPUCount, &sb.MemoryMB, &sb.DiskSizeMB, &meta, &allow, &sb.StartedAt, &sb.EndAt, &sb.EndedAt,
	)
	if err != nil {
		return Sandbox{}, err
	}
	if len(meta) == 0 {
		sb.Metadata = json.RawMessage(`{}`)
	} else {
		sb.Metadata = json.RawMessage(meta)
	}
	if allow != nil {
		sb.AllowInternet = *allow
	}
	return sb, nil
}

func splitSQL(s string) []string {
	parts := strings.Split(s, ";")
	out := make([]string, 0, len(parts))
	for _, p := range parts {
		p = strings.TrimSpace(p)
		if p != "" {
			out = append(out, p)
		}
	}
	return out
}

func newUUID() (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:]), nil
}
