# Threat model (initial)

## Trust boundaries

The browser is untrusted. Every state-changing route authenticates the session,
checks backend RBAC and verifies a per-session CSRF token. Docker and systemd are
reachable only from the daemon. OpenCode is loopback-only and protected with a
random password that is never sent to the browser.

Remote users, project contents, container metadata, logs and AI output are all
untrusted inputs. Commands are constructed as executable plus argument vectors;
shell string concatenation is prohibited. Unit names, container identifiers and
paths receive allowlist or canonicalization checks before privileged use.

## Principal risks

- Stolen admin session: short-lived opaque tokens are hashed in SQLite; secure,
  HttpOnly and SameSite cookies are supported; TLS termination is mandatory on
  untrusted networks.
- CSRF/WebSocket hijacking: mutation requests require the session CSRF token;
  terminal upgrades additionally validate Origin and token.
- AI confused-deputy attacks: read-only is default; approval decisions are tied
  to user, session, scope and exact action; unrestricted remains visible and all
  actions are audited.
- Log/metric injection: the UI renders data through text nodes, never `innerHTML`.
- Database/disk exhaustion: telemetry retention is bounded; streaming readers
  cap lines and bytes; audit data has explicit operator-managed retention.
- Local privilege escalation: the web daemon must not run as root by default;
  future helper IPC will authenticate peers and use a fixed operation protocol.

