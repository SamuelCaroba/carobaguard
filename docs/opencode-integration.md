# OpenCode integration contract

Researched against OpenCode 1.18.25 and exercised end-to-end against 1.18.26 on
2026-09-02. CarobaGuard starts `opencode serve --hostname 127.0.0.1 --port <free>`
with `OPENCODE_SERVER_PASSWORD` set to an ephemeral random secret.

The adapter uses `/global/health`, `/session`, `/session/:id/message`, `/event`,
and the `/question` request/reply endpoints.
The API publishes an OpenAPI 3.1 document at `/doc`; compatibility is checked by
health/version before use. Context can be injected as a no-reply text part before
the user's prompt. Permission requests arrive on SSE and are answered only after
CarobaGuard applies its own scope policy.

OpenCode state may persist in its normal data directory, while CarobaGuard stores
the stable mapping and permission mode in SQLite. Stopping the child therefore
reclaims active memory without discarding session identity.

The lifecycle is `Sleeping -> Starting -> Ready`, with `Error` reserved for
startup failure, unexpected child exit or loss of the internal event stream.
Health probes and stream connection are bounded by timeouts. Ordinary API calls
have a 30-second limit; model message calls have a 15-minute limit so an Approval
request can remain paused while the administrator decides. Responses are streamed
into a bounded 4 MiB buffer.

Permission flow:

1. OpenCode emits `permission.asked` or `permission.v2.asked` on its loopback SSE.
2. CarobaGuard forwards the event to authenticated operators without exposing the
   OpenCode port or credentials.
3. An operator replies through the CSRF-protected CarobaGuard API.
4. CarobaGuard relays `once`, `always` or `reject` and records the decision.
5. Completed tool parts are audited separately and deduplicated by `callID`,
   including in Unrestricted mode.

Interactive question flow:

1. OpenCode emits `question.asked` (or its v2 counterpart) and pauses the tool.
2. CarobaGuard displays the bounded question payload only for mapped AI sessions.
3. An authenticated operator replies or rejects through the CSRF-protected API;
   Unrestricted sessions additionally require an administrator.
4. CarobaGuard relays the decision to loopback OpenCode and audits it without
   storing the answer text.

Primary references:

- https://dev.opencode.ai/docs/server/
- https://dev.opencode.ai/docs/sdk/
- https://dev.opencode.ai/docs/permissions/
