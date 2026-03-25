# Security Policy

> **Audience:** Anyone who discovers a potential security vulnerability in the nanny engine or CLI.

---

## Supported versions

| Version | Security updates |
| ------- | ---------------- |
| 0.x.x   | ✓ Active         |

Only the latest published release receives security patches. If you are running an older build, update to the latest version first to confirm the issue still exists.

---

## What counts as a vulnerability

Report these:

- **Process escape** — a way for a child process to continue running after nanny has issued `SIGKILL`
- **Limit bypass** — a mechanism by which an agent can exceed its configured step, cost, or timeout limits without nanny detecting or stopping it
- **Bridge token forgery** — a way for a process to forge a `NANNY_SESSION_TOKEN` or hijack a bridge session belonging to another execution
- **Event log tampering** — a way to suppress or modify events in the NDJSON log without nanny detecting it
- **Path traversal or arbitrary file write** in `nanny init` or any config-reading path
- **Any crash or panic in the CLI** reachable via normal config input

---

## What is not a vulnerability

These are design decisions, not bugs:

- **An agent that calls tools not in the allowlist** — nanny stops it. That is the intended behaviour.
- **A `nanny.toml` that sets `steps = 0` (unlimited)** — the operator configured it that way.
- **Passthrough mode** — when running outside of `nanny run`, macros and decorators are no-ops by design.
- **A developer choosing weak limits** — nanny enforces what it is told to enforce.

---

## How to report

**Do not open a public GitHub issue.** Public disclosure before a fix is in place puts users at risk.

Send a report to **security@nanny.run** with:

1. A description of the vulnerability and the affected component
2. Steps to reproduce (minimal reproduction preferred)
3. Your assessment of the impact
4. Your name or handle if you would like to be credited

---

## Response commitment

| Milestone                     | Target                 |
| ----------------------------- | ---------------------- |
| Acknowledge receipt           | Within 48 hours        |
| Confirm or dismiss            | Within 5 business days |
| Patch released (if confirmed) | Within 30 days         |

We will keep you informed at each step and credit you in the release notes unless you prefer to remain anonymous.
