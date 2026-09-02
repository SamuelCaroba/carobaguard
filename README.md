# CarobaGuard

CarobaGuard is a lightweight Linux control plane with an on-demand OpenCode
sysadmin copilot. It is built as one native Rust daemon with an embedded web UI
and SQLite database; Docker, Prometheus, Grafana, Node.js and Redis are not runtime
requirements.

> Early development: the authenticated telemetry, Docker, systemd, audit,
> Server Doctor, secure web terminal and on-demand OpenCode flows are operational.

## Run from source

Requirements: Linux, Rust 1.85+ and a C compiler for bundled SQLite.

```bash
export CAROBAGUARD_DATA_DIR="$PWD/data"
export CAROBAGUARD_ADMIN_PASSWORD='use-a-long-unique-password'
cargo run --release
```

Open <http://127.0.0.1:8090>. If no admin password is provided on the first run,
CarobaGuard generates one and prints it once to the service log. The default bind
is loopback-only; set `CAROBAGUARD_HOST=0.0.0.0` only behind an HTTPS reverse proxy
or on a trusted VPN/LAN.

| Variable | Default | Purpose |
| --- | --- | --- |
| `CAROBAGUARD_HOST` | `127.0.0.1` | Listen address |
| `CAROBAGUARD_PORT` | `8090` | Dedicated HTTP port |
| `CAROBAGUARD_DATA_DIR` | `$XDG_DATA_HOME/carobaguard` | SQLite and state |
| `CAROBAGUARD_ADMIN_USERNAME` | `admin` | First-run admin |
| `CAROBAGUARD_ADMIN_PASSWORD` | generated | First-run password (12+ chars) |
| `CAROBAGUARD_COOKIE_SECURE` | `false` | Require HTTPS cookies |
| `CAROBAGUARD_TERMINAL_MAX_SESSIONS` | `4` | Maximum concurrent PTY sessions |
| `CAROBAGUARD_TERMINAL_IDLE_TIMEOUT_SECONDS` | `900` | Close terminals without user input |
| `CAROBAGUARD_TERMINAL_MAX_DURATION_SECONDS` | `14400` | Absolute terminal lifetime |
| `CAROBAGUARD_LOG_MAX_STREAMS` | `8` | Maximum concurrent Docker/journal streams |

## Implemented foundation

- native `/proc` and `/sys` telemetry: aggregate/per-core CPU, frequency, load,
  memory, swap, root filesystem, block I/O, network, uptime, processes and thermal
  zones;
- live SSE dashboard and bounded SQLite history;
- visible self-overhead: CPU, RSS, binary/database size, SSE traffic and database
  writes;
- configurable 5/15/30 second profiles and a 60 second Performance Mode;
- Argon2 password hashing, opaque hashed session tokens, SameSite/HttpOnly cookies,
  CSRF tokens, login throttling and backend RBAC;
- SQLite WAL migrations covering users, sessions, audit, alerts, projects,
  backups, AI sessions and scoped AI permissions;
- embedded, dependency-free production frontend with a strict CSP.
- direct Docker Engine Unix-socket API for container listing, inspect, stats,
  bounded logs and lifecycle operations, with graceful behavior when absent;
- systemd service inventory, details, journal reads and lifecycle operations using
  fixed argument vectors (no shell interpolation);
- mandatory audit records for successful and failed mutations;
- evidence-backed, read-only Server Doctor checks with severity and confidence.
- on-demand OpenCode headless lifecycle with loopback-only transport, persistent
  session mappings, bounded Context Engine and authenticated SSE events;
- Read Only, Approval and explicitly confirmed Unrestricted AI permission modes,
  including deduplicated tool execution records in the Audit Log.
- host PTY terminal over an authenticated same-origin WebSocket, with backend
  RBAC, CSRF-bound subprotocol, bounded sessions, process cleanup and metadata-only
  audit records.
- central SSE log viewer for Docker and systemd with bounded lines, backpressure,
  pause/resume, text and severity filters, export and disconnect cleanup.

## Development

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Architecture, threat model and the researched OpenCode headless contract live in
[`docs/`](docs/). The project is licensed under AGPL-3.0-only.
