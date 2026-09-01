# Architecture

CarobaGuard is a modular Rust monolith. One native process owns the authenticated
HTTP API, static web UI, Linux telemetry, SQLite pool and lifecycle of optional
workers. Docker, systemd and OpenCode are adapters, not independent services.

## Decisions

- **Rust + Tokio + Axum:** predictable memory use, native Linux integration and a
  single distributable binary.
- **SQLite/WAL:** the default database needs no daemon. Time-series retention is
  bounded and later migrations can add downsampling without changing the API.
- **Static browser client:** assets are embedded in the binary. There is no Node
  runtime in production and no periodic animation loop.
- **SSE for telemetry and AI events:** server-to-browser data is one-way in these
  paths. WebSocket is reserved for interactive PTY traffic.
- **Native host daemon:** Docker is optional and its Unix socket is never exposed
  to the browser.
- **OpenCode sidecar process on demand:** `opencode serve` binds only to loopback,
  uses a random internal password, and is stopped after an idle timeout.
- **Least privilege:** the main daemon is expected to run as `carobaguard`; a
  narrowly-scoped privileged helper will own operations that cannot be mediated
  safely through polkit/system groups. Full control means OS-granted power, not
  an authentication bypass.

See `docs/threat-model.md` for trust boundaries and `docs/opencode-integration.md`
for the versioned integration contract.

