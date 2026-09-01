# OpenCode integration contract

Researched against OpenCode 1.18.25 and the upstream documentation on
2026-09-01. CarobaGuard starts `opencode serve --hostname 127.0.0.1 --port <free>`
with `OPENCODE_SERVER_PASSWORD` set to an ephemeral random secret.

The adapter uses `/global/health`, `/session`, `/session/:id/message`, and `/event`.
The API publishes an OpenAPI 3.1 document at `/doc`; compatibility is checked by
health/version before use. Context can be injected as a no-reply text part before
the user's prompt. Permission requests arrive on SSE and are answered only after
CarobaGuard applies its own scope policy.

OpenCode state may persist in its normal data directory, while CarobaGuard stores
the stable mapping and permission mode in SQLite. Stopping the child therefore
reclaims active memory without discarding session identity.

Primary references:

- https://dev.opencode.ai/docs/server/
- https://dev.opencode.ai/docs/sdk/
- https://dev.opencode.ai/docs/permissions/
