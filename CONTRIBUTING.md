# Contributing to Nanny

> **Audience:** OSS contributors — developers who want to improve the engine, fix bugs, or add built-in tools. If you are looking to _use_ nanny in your project, see the [documentation](https://docs.nanny.run) instead.

Thank you for taking the time to contribute. Nanny is a small, focused primitive — every contribution should make agents more predictable, auditable, or safe. This guide explains how to contribute effectively.

---

## Table of Contents

- [Contributing to Nanny](#contributing-to-nanny)
  - [Table of Contents](#table-of-contents)
  - [The one rule that governs everything](#the-one-rule-that-governs-everything)
  - [What you can contribute](#what-you-can-contribute)
    - [Built-in tools](#built-in-tools)
    - [Config validation](#config-validation)
    - [Stop reasons](#stop-reasons)
    - [Tests](#tests)
    - [Bug fixes](#bug-fixes)
    - [Documentation](#documentation)
  - [What belongs elsewhere](#what-belongs-elsewhere)
  - [Codebase map](#codebase-map)
  - [Reference examples](#reference-examples)
  - [Setting up locally](#setting-up-locally)
  - [Running tests](#running-tests)
  - [Opening a pull request](#opening-a-pull-request)
  - [Release process](#release-process)
  - [Reporting bugs](#reporting-bugs)
  - [Code style](#code-style)

---

## The one rule that governs everything

**Nanny is a primitive. It enforces limits. It does not think.**

Every line of code in this repo must answer "yes" to this question:

> Does this make agents more predictable, auditable, or safe — from the machine's perspective?

If the answer involves humans making decisions, dashboards, retries, heuristics, soft warnings, or anything resembling intelligence — it does not belong in this repository. It belongs in an application layer built on top of nanny.

This is not a philosophical preference. It is the reason nanny is trustworthy. The moment a safety primitive starts making "smart" decisions, it stops being safe.

---

## What you can contribute

These are the areas where contributions are most welcome:

### Built-in tools

Add a new tool to the standard library. Each built-in tool lives in `crates/runtime/src/tools/` and is registered in `default_registry()`.

Use `crates/runtime/src/tools/http_get.rs` as the template. A tool must:

- Implement the `Tool` trait from `nanny-core`
- Declare a fixed `name()` and `cost_per_call()` default
- Be deterministic and side-effect-bounded (not network-stateful)

### Config validation

`crates/config/src/lib.rs` currently accepts values like `steps = 0` without complaint. Adding clear range checks with actionable error messages is a high-value, low-risk contribution.

### Stop reasons

If you find a process exit path that does not produce an `ExecutionStopped` event, that is a bug. Add the missing path and a test that verifies the event is emitted.

### Tests

More coverage of edge cases in the policy engine, per-tool limit enforcement, and event log correctness is always welcome. Tests live alongside each crate in `crates/<name>/tests/` or as `#[cfg(test)]` modules.

### Bug fixes

Check the [issue tracker](https://github.com/nanny-run/nanny/issues) for bugs labelled `good first issue` or `help wanted`.

### Documentation

The documentation lives in `docs/` in this repository. Doc contributions are welcome and do not require any Rust knowledge.

Good candidates:

- Typo or grammar fixes
- Clarity improvements to confusing explanations
- Missing examples for existing features
- Broken links or stale references

Not in scope for doc PRs:

- Documenting features that do not exist in the current release
- Speculative roadmap content
- Adding new concepts not grounded in the codebase

To preview doc changes locally, run `mint dev` from the `docs/` directory. If your code PR changes user-facing behaviour, config schema, or events, update the relevant `.mdx` files in the same PR.

---

## What belongs elsewhere

Do not open pull requests that add:

| Feature                             | Why it doesn't belong here            |
| ----------------------------------- | ------------------------------------- |
| LLM calls or semantic analysis      | Nanny doesn't understand agent intent |
| Retry or recovery logic             | Hard stops are real stops             |
| Dashboards, reporting, or analytics | That's the cloud layer                |
| Authentication or multi-tenancy     | Out of scope for the OSS engine       |
| A TOML DSL for writing rules        | Rules are code, not config            |
| Soft limits or warnings             | Nanny either stops or it doesn't      |

These are permanent constraints, not temporary gaps. They protect the property that makes nanny valuable.

---

## Codebase map

The repository has two independent build systems: a Rust workspace under `crates/` and a Python package under `sdks/python/`. They share the same repo and version number but have no toolchain overlap — `cd sdks/python && uv sync && uv run pytest` runs without touching Cargo, and `cargo build --workspace` runs without touching Python.

**Rust crates** — all six are published to crates.io. `nannyd` (`cli`) is the developer-facing crate. The others are its published dependencies and are not intended to be used directly.

| Crate     | crates.io name  | Developer-facing | What it does                                                                                   |
| --------- | --------------- | ---------------- | ---------------------------------------------------------------------------------------------- |
| `cli`     | `nannyd`        | ✓                | The `nanny` binary and Rust SDK (`#[tool]`, `#[rule]`, `#[agent]`)                             |
| `core`    | `nanny-core`    | ✗                | Traits (`Policy`, `Ledger`, `ToolExecutor`) and the `ExecutionEvent` type. No implementations. |
| `runtime` | `nanny-runtime` | ✗                | Concrete impls: `LimitsPolicy`, `RuleEvaluator`, `FakeLedger`, `ToolRegistry`, built-in tools  |
| `bridge`  | `nanny-bridge`  | ✗                | Local HTTP enforcement server (Unix socket / TCP); holds all execution state                   |
| `config`  | `nanny-config`  | ✗                | Parses `nanny.toml`; owns `NannyConfig`                                                        |
| `macros`  | `nanny-macros`  | ✗                | The `#[tool]`, `#[rule]`, `#[agent]` proc-macros (re-exported by `cli`)                        |

**The dependency direction is strict:** `core` has no internal dependencies. Everything else depends on `core`. `core` never imports `runtime`, `bridge`, or `cli`.

**Python SDK** — lives at `sdks/python/`. Published as `nanny-sdk` on PyPI. Toolchain: `uv` (package manager), `hatchling` (build backend), `pytest` + `pytest-httpserver` (tests), `ruff` (lint), `mypy` (type checking). The root `Cargo.toml` workspace does not include `sdks/` — there is no toolchain collision.

| Path | What it is |
| ---- | ---------- |
| `sdks/python/nanny_sdk/` | The importable package (`from nanny_sdk import tool, rule, agent`) |
| `sdks/python/tests/` | Unit tests — all use a `mock_bridge` fixture, no real bridge required |
| `sdks/python/pyproject.toml` | Package metadata, build config, tool config (`ruff`, `mypy`, `pytest`) |

If you are adding a new enforcement rule, it goes in `runtime`. If you are adding a new event type, it goes in `core/src/events/event.rs`. If you are changing CLI behaviour, it goes in `cli/src/main.rs`.

---

## Reference examples

`examples/` contains complete agents that exercise the full SDK — two Rust and two Python:

| Example | What it demonstrates |
| ------- | -------------------- |
| [`examples/rust/webdingo`](examples/rust/webdingo) | `#[nanny::tool]`, `#[nanny::agent]`, `nanny::http_get`, loop-detection rule |
| [`examples/rust/qabud`](examples/rust/qabud) | `#[nanny::tool]`, content-based rule (`last_tool_args`), allowlist enforcement |
| [`examples/python/dev_assist`](examples/python/dev_assist) | `@tool`, `@rule`, `@agent` — LangGraph debug agent (Groq), Python-driven StateGraph nodes |
| [`examples/python/metrics_crew`](examples/python/metrics_crew) | `@tool`, `@rule`, `@agent` — CrewAI multi-agent pipeline (Groq), single-tool tasks, per-role limits |

All four examples depend on the published crates (`nannyd` from crates.io, `nanny-sdk` from PyPI). `webdingo`, `qabud`, and `dev_assist` use Groq (`llama-3.3-70b-versatile`, free tier — set `GROQ_API_KEY`). `metrics_crew` uses OpenAI (`gpt-4.1-nano` — set `OPENAI_API_KEY`). Copy `.env.example` → `.env` in each directory and fill in the relevant key. Each example also documents a one-line swap to Ollama for offline use. All four are the best starting point for understanding how the pieces fit together before touching the crate or SDK internals.

---

## Setting up locally

**Requirements:** Rust stable (1.75+).

Contributors do not have write access to `nanny-run/nanny` directly. The standard flow is fork → clone your fork → open a PR back to the main project.

```bash
# 1. Fork nanny-run/nanny on GitHub (click "Fork" in the top right)

# 2. Clone your fork — replace <your-username> with your GitHub handle
git clone https://github.com/<your-username>/nanny.git
cd nanny

# 3. Add the upstream repo as a remote so you can pull future changes
git remote add upstream https://github.com/nanny-run/nanny.git

# 4. Create a branch for your change
git checkout -b fix/my-descriptive-branch-name

# 5. Build everything
cargo build --workspace

# 6. Run all tests
cargo test --workspace

# 7. Check for warnings (CI enforces this)
cargo clippy --workspace -- -D warnings
```

There are no external service dependencies. The bridge runs in-process during tests — no ports need to be open.

**Keeping your fork up to date:**

```bash
git fetch upstream
git rebase upstream/next
```

---

## Running tests

```bash
# All tests
cargo test --workspace

# A specific crate
cargo test -p nanny-bridge

# A specific test by name
cargo test -p nanny -- process_lifecycle
```

Tests run in parallel by default. The test suite is designed to tolerate parallelism — if you write tests that create temp files or sockets, make sure names are unique (use the `AtomicU64` counter pattern in `crates/cli/tests/process_lifecycle.rs` as a reference).

All tests must pass before a PR can be merged. The CI matrix runs on `ubuntu-latest` and `macos-latest`.

---

## Opening a pull request

```bash
# Push your branch to your fork
git push origin fix/my-descriptive-branch-name
```

Then open a pull request on GitHub from your fork's branch to `nanny-run/nanny` targeting the **`next`** branch. `next` is the default and the only active development branch — do not target `main`.

**Fork and branch model:**

- Fork `nanny-run/nanny` on GitHub, clone your fork, and branch off `next`.
- `next` is the only active development branch — all PRs must target `next`, never `main`.
- No direct pushes to the upstream repo. Always go through a PR.

**Commit style — conventional commits:**

Use conventional commit prefixes so the changelog can be generated automatically:

| Prefix      | When to use                                              |
| ----------- | -------------------------------------------------------- |
| `feat:`     | New feature or behaviour visible to users                |
| `fix:`      | Bug fix                                                  |
| `chore:`    | Maintenance (CI, deps, tooling) — no user-visible change |
| `docs:`     | Documentation only                                       |
| `refactor:` | Internal restructure with no behaviour change            |
| `test:`     | Adding or fixing tests only                              |

PR titles are checked against this format. Example: `feat: add [start] table to nanny.toml`.

**Checklist before opening:**

1. **Open an issue first** for anything beyond a small bug fix or typo. This avoids duplicate work and confirms the change fits the project's scope.
2. **Keep PRs focused.** One logical change per PR. Reviewers will ask you to split large PRs.
3. **Write tests.** PRs without tests for new behaviour will not be merged.
4. **Run clippy before pushing.** `cargo clippy --workspace -- -D warnings` must be clean.
5. **No `unwrap()` in non-test code.** Use `?` or explicit error handling.
6. **Update the docs** if your change affects user-facing behaviour, config schema, or events. The documentation lives in the `docs/` directory of this repository — update the relevant `.mdx` files in the same PR.

---

## Release process

**Who cuts releases:** Only the maintainer pushes version tags. Contributors submit PRs; the maintainer merges and tags. You never need to tag or publish anything yourself.

**Tag protection:** Version tags (`v*`) are restricted to maintainers at the GitHub repo level (Settings → Rules → Tag protection → pattern `v*`). Pushing a `v*` tag from a fork or contributor branch will be rejected.

**How a release happens:**

1. Maintainer merges the release branch into `next`, then merges `next` into `main`.
2. Maintainer pushes a `v*` tag (e.g. `v0.1.3`) on `main`.
3. The release workflow runs automatically: binaries built, GitHub Release created with notes from `CHANGELOG.md`, all six crates published to crates.io in dependency order, Homebrew formula updated in the tap repo.

**Pre-tag checklist — complete every item before pushing the tag:**

Every item below is a release participant. Missing any one of them produces a broken or misleading release.

| #   | What                          | How                                                                                                                                                                                             |
| --- | ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | **Workspace version**         | Bump `version` in `[workspace.package]` and all `version = "x.y.z"` entries in `[workspace.dependencies]` inside the root `Cargo.toml`. Run `cargo check --workspace` to confirm.               |
| 2   | **Example app versions**      | Bump `nannyd = "x.y.z"` in `examples/rust/qabud/Cargo.toml` and `examples/rust/webdingo/Cargo.toml` to match. Do this after publish — the new version must exist on crates.io first.            |
| 3   | **Homebrew formula template** | Bump `version "x.y.z"` in `homebrew/nannyd.rb`. CI substitutes the SHA256s automatically; the version line must match so the template stays readable.                                           |
| 4   | **Python SDK version**        | `version` in `sdks/python/pyproject.toml` must match the tag. The `publish-pypi` CI job validates this and fails loudly if they diverge.                                                        |
| 5   | **`CHANGELOG.md` entry**      | Add `## [x.y.z] — YYYY-MM-DD` with `### Added` / `### Fixed` sections. The release workflow reads this file and uses it as the GitHub Release body. A missing entry means blank release notes.  |
| 6   | **Rust tests pass**           | `cargo test --workspace` must be green on both Linux and macOS.                                                                                                                                 |
| 7   | **Clippy clean**              | `cargo clippy --workspace -- -D warnings` must produce no errors.                                                                                                                               |
| 8   | **Python SDK tests pass**     | `cd sdks/python && uv run pytest -q` must be green. `uv run mypy nanny_sdk` and `uv run ruff check .` must be clean.                                                                            |
| 9   | **Tag matches Cargo.toml**    | The `publish-crates` CI job validates this automatically and fails loudly — but verify locally first: the tag you push (e.g. `v0.1.4`) must equal `[workspace.package] version` (e.g. `0.1.4`). |

**What CI handles automatically (do not do manually):**

- SHA256 computation and Homebrew tap update
- `cargo publish` for all six crates in dependency order
- Python SDK wheel built and published to PyPI via OIDC trusted publishing
- GitHub Release artifact upload and release notes body

---

## Reporting bugs

Use [GitHub Issues](https://github.com/nanny-run/nanny/issues). Include:

- Nanny version (`nanny --version`)
- OS and architecture
- Your `nanny.toml` (redact any API keys)
- The command you ran
- What you expected vs what happened
- The NDJSON event log if relevant

For security vulnerabilities, do **not** open a public issue. See [SECURITY.md](SECURITY.md).

---

## Code style

- Standard `rustfmt` formatting (`cargo fmt --all`)
- No `unwrap()` or `expect()` outside of tests
- Error types use `thiserror` — match the pattern already in each crate
- Public items must have doc comments (`///`)
- Keep `nanny-core` free of any concrete implementations — traits and types only

---

_Nanny is open source under the [Apache-2.0 license](LICENSE)._
