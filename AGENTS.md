# AGENTS.md — Nanny repository

## Quick start

**This is a Rust + Python monorepo with two independent build systems.**

- **Rust workspace**: `crates/` — 6 crates, published to crates.io
- **Python SDK**: `sdks/python/` — published as `nanny-sdk` on PyPI

They share the same repo and version number but have no toolchain overlap.

## Architecture

**Nanny is an enforcement primitive for autonomous AI agents.** It stops agents that exceed limits (steps, tokens, timeout) or violate rules.

**Key concept**: Nanny becomes the **parent process** of your agent via `nanny run`. All enforcement happens in the parent; the child cannot bypass it.

### Core abstractions

| Term | Description |
|------|-------------|
| **tool** | Function annotated with `#[nanny::tool]` / `@tool` — passes through bridge for enforcement |
| **rule** | Function annotated with `#[nanny::rule]` / `@rule` — returns `false` to stop execution |
| **agent scope** | Named limits context activated by `#[nanny::agent]` / `@agent` |
| **bridge** | Internal enforcement layer (Unix socket / TCP). **Never mention in user-facing docs.** |

### Three limits

Any one stops execution:

- `timeout` — wall-clock ms (no instrumentation needed)
- `steps` — tool calls (requires SDK)
- `tokens` — token budget (requires SDK)

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

### Run examples

All examples require API keys. Copy `.env.example` → `.env` and fill in.

```bash
# Rust examples (Groq free tier)
cd examples/rust/webdingo && cargo build --release && nanny run -- "best Rust HTTP clients"
cd examples/rust/qabud && cargo build --release && nanny run -- ./src

# Python examples
cd examples/python/dev_assist && uv sync && nanny run
cd examples/python/metrics_crew && uv sync && nanny run
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
- **Versioning**: Docs folders are versioned at minor level only (`v0.3/`, `v1.0/`) — never patch; a patch release updates the current folder in place

## Branching and releases

- **`main`** is the only active development branch
- **Fork model**: Fork → clone your fork → PR to `main`
- **Conventional commits**: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`
- **Tag protection**: `v*` tags restricted to maintainers only
- **Release checklist**: Bump workspace version → examples → Homebrew formula → Python SDK → `CHANGELOG.md` → tests pass → clippy clean

## Key directories

| Path | Purpose |
|------|---------|
| `crates/core` | Traits and types only — no implementations |
| `crates/runtime` | Concrete impls: `LimitsPolicy`, `RuleEvaluator`, built-in tools |
| `crates/bridge` | Local HTTP enforcement server |
| `crates/config` | Parses `nanny.toml` |
| `crates/macros` | `#[tool]`, `#[rule]`, `#[agent]` proc-macros |
| `crates/cli` | `nanny` binary + Rust SDK re-export |
| `sdks/python/` | Python SDK (`nanny-sdk` on PyPI) |
| `examples/` | 4 complete agents (2 Rust, 2 Python) |
| `docs/` | Mintlify site (MDX + YAML) |

## Testing

- **Rust**: `cargo test --workspace` runs in parallel; use unique temp file names
- **Python**: `uv run pytest` uses `mock_bridge` fixture — no real bridge required
- **Examples**: All use published crates (`nannyd`, `nanny-sdk`) — switch to path deps during active development

## Documentation surfaces

| Surface | Where | Audience | What |
|---------|-------|----------|------|
| Docs site | `docs/` (Mintlify) | Developers using Nanny | Commands, config, SDK usage |
| Root docs | `README.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md` | Contributors | Overview, implementation model, workflow |
| Examples | `examples/**/README.md` | Learning by copying | Runnable integrations |

**Keep each surface consistent with its audience.** User-facing docs never expose internal terms like "bridge".

## Critical gotchas

1. **Build before `nanny run`** — timeout starts at process launch; if `cargo` compiles during the governed run, it fires prematurely
2. **Direct-call pattern** — your code must drive tool calls; LLM should only reason, not dispatch tools
3. **Passthrough mode** — decorators/macros are no-ops outside `nanny run`; zero overhead in dev/CI
4. **Stop reasons** — use exact enum names: `BudgetExhausted`, not "budget exceeded"
5. **Token tracking** — Python: call `nanny_sdk.instrument(client)` once at startup to auto-report LLM usage. Rust: call `nanny::report_usage(Usage { input, output, .. })` after each LLM call (Rust can't patch a client, so reporting is explicit)
6. **Per-role limits** — named scopes inherit from base `[limits]` and override only what differs; inner scope cannot exceed outer budget
