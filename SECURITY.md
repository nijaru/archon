# Security Policy

Archon is pre-production R&D software. There is no SLA on response or
remediation, but reported vulnerabilities are taken seriously.

## Reporting a vulnerability

**Do not open a public issue.** Report privately through GitHub Security
Advisories:

<https://github.com/nijaru/archon/security/advisories/new>

Include, where possible: affected commit or version, reproduction steps
or proof of concept, and the impact you see (lease overlap, fencing
bypass, authentication bypass, and enforcement escape are the highest
severity classes for this project).

## What to expect

- Acknowledgment of the report.
- A fix on `main` once the cause is understood and verified; the
  `privileged-enforcement` CI job must still pass.
- Credit in the fix commit on request.

## Scope notes

- Without `--token-file` / `ARCHON_TOKEN`, links are encrypted but
  unauthenticated by design (development mode); that is documented
  behavior, not a vulnerability. Deployments must configure a token.
- The simulator (`archon-sim`) is a test oracle, not a security
  boundary.
