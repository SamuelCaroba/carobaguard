# CarobaGuard

CarobaGuard is a lightweight Linux control plane with an on-demand OpenCode
sysadmin copilot. It is built as one native Rust daemon with an embedded web UI
and SQLite database; Docker, Prometheus, Grafana, Node.js and Redis are not runtime
requirements.

> Early development: the authenticated telemetry dashboard is operational. Docker,
> systemd, terminal, Server Doctor and OpenCode adapters are being built in-tree.

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

## Development

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Architecture, threat model and the researched OpenCode headless contract live in
[`docs/`](docs/). The project is licensed under AGPL-3.0-only.
