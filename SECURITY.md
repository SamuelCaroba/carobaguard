# Security policy

CarobaGuard is currently alpha software and its main branch is the only supported version. Do not expose it directly to the public internet; use loopback with an SSH tunnel, a trusted VPN, or a properly configured HTTPS reverse proxy.

Please report vulnerabilities privately through GitHub's **Security → Report a vulnerability** flow. Do not open a public issue containing an exploit, password, token, database, log, server address, or other sensitive information.

Useful reports include the affected commit, impact, a minimal reproduction, and any suggested mitigation. Never test against systems you do not own or administer.

Security invariants and known trust boundaries are documented in [`docs/threat-model.md`](docs/threat-model.md) and [`docs/architecture.md`](docs/architecture.md).
