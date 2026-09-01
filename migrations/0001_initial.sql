PRAGMA foreign_keys = ON;

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'operator', 'viewer', 'ai_agent')),
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token TEXT NOT NULL,
    source_ip TEXT,
    user_agent TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
);
CREATE INDEX sessions_token_hash_idx ON sessions(token_hash);
CREATE INDEX sessions_expires_at_idx ON sessions(expires_at);

CREATE TABLE login_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_ip TEXT NOT NULL,
    username TEXT NOT NULL,
    succeeded INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX login_attempts_lookup_idx ON login_attempts(source_ip, created_at);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE telemetry_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sampled_at INTEGER NOT NULL,
    resolution_seconds INTEGER NOT NULL DEFAULT 15,
    cpu_percent REAL NOT NULL,
    memory_used_bytes INTEGER NOT NULL,
    memory_total_bytes INTEGER NOT NULL,
    swap_used_bytes INTEGER NOT NULL,
    disk_used_bytes INTEGER NOT NULL,
    disk_total_bytes INTEGER NOT NULL,
    network_rx_bytes_per_sec REAL NOT NULL,
    network_tx_bytes_per_sec REAL NOT NULL,
    disk_read_bytes_per_sec REAL NOT NULL,
    disk_write_bytes_per_sec REAL NOT NULL,
    carobaguard_cpu_percent REAL NOT NULL,
    carobaguard_memory_bytes INTEGER NOT NULL,
    payload_json TEXT NOT NULL
);
CREATE INDEX telemetry_sampled_at_idx ON telemetry_samples(sampled_at);

CREATE TABLE alerts (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    severity TEXT NOT NULL,
    title TEXT NOT NULL,
    details TEXT NOT NULL,
    source TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    acknowledged_at INTEGER
);

CREATE TABLE audit_events (
    id TEXT PRIMARY KEY,
    actor_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    actor_name TEXT NOT NULL,
    origin TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    command TEXT,
    result TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    exit_code INTEGER,
    ai_session_id TEXT,
    ai_permission_mode TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL
);
CREATE INDEX audit_created_at_idx ON audit_events(created_at DESC);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE backups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    source_path TEXT NOT NULL,
    destination TEXT NOT NULL,
    status TEXT NOT NULL,
    size_bytes INTEGER,
    error TEXT,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE TABLE ai_sessions (
    id TEXT PRIMARY KEY,
    opencode_session_id TEXT,
    project_path TEXT,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    permission_mode TEXT NOT NULL CHECK (permission_mode IN ('read_only', 'approval', 'unrestricted')),
    created_by TEXT NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL
);

CREATE TABLE ai_permissions (
    id TEXT PRIMARY KEY,
    scope_type TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('read_only', 'approval', 'unrestricted')),
    granted_by TEXT NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(scope_type, scope_id)
);

INSERT INTO settings(key, value, updated_at) VALUES
    ('telemetry_profile', 'balanced', unixepoch()),
    ('performance_mode', 'false', unixepoch()),
    ('telemetry_retention_days', '7', unixepoch()),
    ('opencode_idle_timeout_seconds', '600', unixepoch()),
    ('ai_global_permission_mode', 'read_only', unixepoch());
