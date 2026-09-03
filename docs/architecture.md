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

## Implemented control-plane components

- Authenticated HTTP API with Argon2 password hashing, server-side sessions, CSRF
  checks on mutations and backend RBAC.
- Bounded Linux telemetry with SSE delivery and SQLite retention.
- Docker and systemd adapters whose mutating operations are authorized and
  audited by the backend.
- Read-only Server Doctor and a bounded, redacting AI Context Engine.
- On-demand OpenCode lifecycle, persistent session mappings, permission approval
  relay, tool execution auditing and a static AI workspace in the web UI. Session
  history is read back through the authenticated backend with injected context
  removed before it reaches the browser.
- On-demand host PTYs behind an authenticated WebSocket. The browser can only
  exchange terminal bytes and resize messages; it cannot select an executable or
  bypass the daemon's OS identity.
- Bounded SSE log streams for Docker and systemd. Docker's multiplexed frames are
  decoded incrementally; journal followers use fixed argument vectors and are
  killed when their authenticated client disconnects.
- Local project registrations store canonical paths only inside configured roots.
  Git inspection disables optional locks, hooks, fsmonitor and pagers, and applies
  strict time/output limits. Removing a registration never removes project files.

## Terminal security invariants

- Only Operator and Admin roles can upgrade a terminal connection. The upgrade
  requires the normal session cookie, an exact same-origin check and the current
  CSRF token transported as a WebSocket subprotocol, never in the URL.
- The backend chooses an absolute executable shell and passes it directly to the
  PTY API without shell-command construction. The terminal inherits exactly the
  daemon account's OS privileges.
- Concurrent sessions, input message size, terminal dimensions, idle time and
  absolute lifetime are bounded. Slow WebSocket writes have a finite timeout.
- Closing the browser, leaving the terminal page, reaching a timeout or stopping
  the shell kills and reaps the managed child. A bounded channel applies
  backpressure to PTY output rather than accumulating it in memory.
- Session open/close, actor, source, duration, outcome and byte counts are
  audited. Keystrokes and PTY output are deliberately never persisted because
  they commonly contain passwords, tokens and other secrets.

## OpenCode security invariants

- The child binds only to `127.0.0.1` and uses an ephemeral random Basic Auth
  password that is never sent to the browser or stored in SQLite.
- `Sleeping` means there is no managed child and reported OpenCode RAM is zero.
- Only one prompt may execute at a time, and a project or permission-mode change
  cannot replace the managed child while an OpenCode request is active.
  Startup does not become `Ready` until health and the internal SSE audit stream
  are connected.
- Process starts and stops are serialized. Slow child termination never holds the
  process-state mutex, and a generation number prevents responses from an older
  child from changing counters for its replacement.
- Losing the internal OpenCode SSE stream or failing to persist a tool record is
  fail-closed: CarobaGuard stops that child and enters `Error`, because continued
  execution could not be fully audited.
- Read Only explicitly denies the wildcard permission and every mutating or
  delegating tool, while selectively allowing inspection tools. Approval asks for
  mutating tools and relays the request over authenticated CarobaGuard SSE.
  Unrestricted maps to OpenCode `allow`, but only after exact administrator
  confirmation; it does not bypass authentication, RBAC or OS permissions.
- Changing mode stops the current child. A persisted unrestricted session cannot
  wake a new child after the global mode has returned to a safer setting.
- Completed tool events are deduplicated by OpenCode `callID` and stored with the
  CarobaGuard AI session, actor, result, permission mode and bounded metadata.
  Credential-like commands are redacted; a SHA-256 fingerprint is retained for
  correlation without storing the original secret-bearing command.

## Known boundaries

- The OpenCode adapter is version-sensitive at its HTTP and event schema boundary;
  upgrades must be exercised with a real headless process before release.
- Selecting an ephemeral loopback port and then starting OpenCode has an inherent
  local bind race because OpenCode does not accept an already-bound listener.
  Authentication prevents use of an unauthenticated substitute, and readiness
  fails if the expected authenticated endpoints are unavailable.
- Privilege is exactly that of the CarobaGuard OS account and configured groups or
  helpers. Unrestricted mode cannot manufacture root privileges.
