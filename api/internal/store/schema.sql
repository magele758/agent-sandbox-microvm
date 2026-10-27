CREATE TABLE tenants (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    max_concurrent INT NOT NULL DEFAULT 20,
    blocked BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants (id),
    prefix TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    revoked_at TIMESTAMPTZ
);

CREATE TABLE templates (
    id TEXT PRIMARY KEY,
    vcpu INT NOT NULL,
    mem_mib INT NOT NULL,
    disk_mb INT NOT NULL
);

CREATE TABLE sandboxes (
    id TEXT PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants (id),
    template_id TEXT NOT NULL REFERENCES templates (id),
    state TEXT NOT NULL,
    node_url TEXT,
    guest_url TEXT,
    envd_access_token TEXT NOT NULL,
    traffic_access_token TEXT NOT NULL,
    domain TEXT NOT NULL,
    envd_version TEXT NOT NULL,
    client_id TEXT NOT NULL,
    cpu_count INT NOT NULL,
    memory_mb INT NOT NULL,
    disk_size_mb INT NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}',
    allow_internet BOOLEAN,
    started_at TIMESTAMPTZ NOT NULL,
    end_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ
);

CREATE INDEX sandboxes_tenant_state ON sandboxes (tenant_id, state);

CREATE TABLE usage_events (
    id BIGSERIAL PRIMARY KEY,
    sandbox_id TEXT NOT NULL,
    tenant_id UUID NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    vcpu INT NOT NULL,
    mem_mib INT NOT NULL,
    reason TEXT NOT NULL
);

CREATE TABLE audit_log (
    id BIGSERIAL PRIMARY KEY,
    tenant_id UUID,
    action TEXT NOT NULL,
    sandbox_id TEXT,
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
