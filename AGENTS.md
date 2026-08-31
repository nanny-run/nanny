# AGENTS.md: Nanny repository

## Quick start

**This is a Rust + Python monorepo with two independent build systems.**

- **Rust workspace**: `crates/`, 6 crates, published to crates.io
- **Python SDK**: `sdks/python/`, published as `nanny-sdk` on PyPI

They share the same repo and version number but have no toolchain overlap.

## Architecture

**Nanny is the authorization and audit layer for AI agents that take real-world actions.** It refuses tool calls the operator has not authorized, and records every decision.

**Key concept**: Nanny becomes the **parent process** of your agent via `nanny run`. All enforcement happens in the parent; the child cannot bypass it.

### Core abstractions

| Term | Description |
|------|-------------|
| **tool** | Function annotated with `#[nanny::tool]` / `@tool`, passes through bridge for enforcement |
| **rule** | Function annotated with `#[nanny::rule]` / `@rule`, returns `false` to stop execution |
| **agent scope** | Named limits context activated by `#[nanny::agent]` / `@agent` |
| **bridge** | Internal enforcement layer (Unix socket / TCP). **Never mention in user-facing docs.** |

### Three limits

Any one stops execution:

- `timeout`: wall-clock ms (no instrumentation needed)
- `steps`: tool calls (requires SDK)
- `tokens`: token budget (requires SDK)

## Developer workflow

### Install CLI (global, one-time)

```bash
# macOS
brew tap nanny-run/nanny && brew install nannyd

# Linux
curl -fsSL https://install.nanny.run | sh

# Windows
irm https://install.nanny.run/windows | iex

# Or
cargo install nannyd
```

### Build and test

```bash
# Rust workspace
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings

# Python SDK
cd sdks/python && uv sync && uv run pytest
cd sdks/python && uv run ruff check .
cd sdks/python && uv run mypy nanny_sdk
```

### Run rule pack tests

Rule packs live in `packs/`. Their tests run from `sdks/python`:

```bash
uv run pytest ../../packs/nanny-recommended/tests
```

## Important constraints

### What belongs in this repo

- Built-in tools (`crates/runtime/src/tools/`)
- Config validation (`crates/config/src/lib.rs`)
- Stop reasons (add missing paths in `crates/core/src/events/event.rs`)
- Tests (edge cases in policy engine, per-tool limits, event log)
- Bug fixes
- Documentation (typos, clarity, missing examples)

### What does NOT belong

| Feature | Reason |
|---------|--------|
| LLM calls or semantic analysis | Nanny doesn't understand agent intent |
| Retry or recovery logic | Hard stops are real stops |
| Dashboards, reporting, analytics | That's the cloud layer |
| Authentication or multi-tenancy | Out of scope for OSS engine |
| TOML DSL for rules | Rules are code, not config |
| Soft limits or warnings | Nanny either stops or it doesn't |

### Code style

- **Rust**: `rustfmt`, no `unwrap()`/`expect()` outside tests, `thiserror` for errors, doc comments on public items
- **Python**: `ruff` (line-length=100, target-version=py311), `mypy --strict`, `pytest` + `pytest-httpserver` for tests
- **Versioning**: Docs folders are versioned at minor level only (`v0.4/`, `v1.0/`), never patch; a patch release updates the current folder in place

## Branching and releases

- **`main`** is the only active development branch
- **Fork model**: Fork → clone your fork → PR to `main`
- **Conventional commits**: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`
- **Tag protection**: `v*` tags restricted to maintainers only
- **Release checklist**: Bump workspace version → Homebrew formula → Python SDK → `CHANGELOG.md` → tests pass → clippy clean

## Key directories

| Path | Purpose |
|------|---------|
| `crates/core` | Traits and types only, no implementations |
| `crates/runtime` | Concrete impls: `ToolPermissionPolicy`, `RuleEvaluator`, built-in tools |
| `crates/bridge` | Local HTTP enforcement server. Wire protocol: `crates/bridge/PROTOCOL.md` |
| `crates/config` | Parses `nanny.toml` |
| `crates/macros` | `#[tool]`, `#[rule]`, `#[agent]` proc-macros |
| `crates/cli` | `nanny` binary + Rust SDK re-export |
| `sdks/python/` | Python SDK (`nanny-sdk` on PyPI) |
| `packs/` | First-party rule packs |
| `docs/` | Mintlify site (MDX + YAML) |

## Testing

- **Rust**: `cargo test --workspace` runs in parallel; use unique temp file names
- **Python**: `uv run pytest` uses `mock_bridge` fixture, no real bridge required
- **Packs**: `uv run pytest ../../packs/nanny-recommended/tests` from `sdks/python`

## Documentation surfaces

| Surface | Where | Audience | What |
|---------|-------|----------|------|
| Docs site | `docs/` (Mintlify) | Developers using Nanny | Commands, config, SDK usage |
| Root docs | `README.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md` | Contributors | Overview, implementation model, workflow |
| Wire protocol | `crates/bridge/PROTOCOL.md` | Maintainers | Endpoints, headers, event shape |

**Keep each surface consistent with its audience.** User-facing docs never expose internal terms like "bridge", nor the wire protocol.

**Docs are present tense.** A removed feature is deleted from the docs, never marked deprecated, and `CHANGELOG.md` is the only place history lives. Full rules in `docs/AGENTS.md`.

## Critical gotchas

1. **Direct-call pattern**, your code must drive tool calls; the LLM should reason, not dispatch tools
2. **Passthrough mode**, decorators and macros are no-ops outside `nanny run`; zero overhead in dev and CI
3. **Stop reasons**, four, and the set is closed: `ToolDenied`, `RuleDenied`, `AgentCompleted`, `ManualStop`. Only the first two are policy violations
4. **Rules reference labels, not tool names**, a rule naming `send_outreach` governs one app; a rule reading `external_effect` governs every app whose operator labelled their tools
5. **Token tracking**, Python: `nanny_sdk.instrument(client)` once at startup. Rust: `nanny::report_usage(...)` after each LLM call. Measured for attribution, never enforced
6. **`--serve` is the launch mode**, one governor, many runs, one shared log. Every event carries its `run_id`
