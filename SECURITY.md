# Security Policy

## Supported Versions

Before 1.0, only the latest released minor version receives security fixes.
Users should upgrade to the newest release before reporting a defect.

## Reporting a Vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's
[private vulnerability reporting form](https://github.com/Shuozeli/company-feed-server/security/advisories/new).

Include:

- the affected version or commit;
- impact and realistic attack scenario;
- reproduction steps or a minimal proof of concept;
- relevant configuration, with credentials removed; and
- any suggested mitigation.

Maintainers aim to acknowledge a complete report within five business days and
will coordinate disclosure after a fix or mitigation is available.

## High-Priority Areas

Reports are especially useful for:

- SSRF or private-network access that bypasses URL policy;
- command or path injection in Git export;
- authorization assumptions around operator write endpoints;
- credential exposure through adapter errors or logs;
- cross-company data ownership violations; and
- dependency vulnerabilities reachable in the default deployment.

## Deployment Boundary

The built-in operator APIs are not a user authentication system. The default
Compose deployment publishes API, worker, and database ports on loopback.
Deployments that expose the service must add network access control and
authentication at a trusted reverse proxy or gateway.

## Dependency Audit Exception

`RUSTSEC-2023-0071` affects `rsa`, which appears in `Cargo.lock` only through
SQLx's disabled MySQL backend. This project compiles SQLx with
`default-features = false` and the PostgreSQL feature only; neither
`sqlx-mysql` nor `rsa` appears in the compiled dependency graph. CI ignores
this advisory while continuing to deny every other RustSec advisory and
warning. Remove the exception if SQLx stops recording the disabled backend, or
before enabling any additional database backend.
