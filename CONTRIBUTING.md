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
  - [Setting up locally](#setting-up-locally)
  - [Running tests](#running-tests)
  - [Opening a pull request](#opening-a-pull-request)
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

All source lives in `crates/`. Only `cli` is published to crates.io. The rest are internal implementation crates.

| Crate     | Published    | What it does                                                                                   |
| --------- | ------------ | ---------------------------------------------------------------------------------------------- |
| `cli`     | ✓ (`nannyd`) | The `nanny` binary and Rust SDK (`#[tool]`, `#[rule]`, `#[agent]`)                             |
| `core`    | ✗            | Traits (`Policy`, `Ledger`, `ToolExecutor`) and the `ExecutionEvent` type. No implementations. |
| `runtime` | ✗            | Concrete impls: `LimitsPolicy`, `RuleEvaluator`, `FakeLedger`, `ToolRegistry`, built-in tools  |
| `bridge`  | ✗            | Local HTTP enforcement server (Unix socket / TCP); holds all execution state                   |
| `config`  | ✗            | Parses `nanny.toml`; owns `NannyConfig`                                                        |
| `macros`  | ✗            | The `#[tool]`, `#[rule]`, `#[agent]` proc-macros (re-exported by `cli`)                        |

**The dependency direction is strict:** `core` has no internal dependencies. Everything else depends on `core`. `core` never imports `runtime`, `bridge`, or `cli`.

If you are adding a new enforcement rule, it goes in `runtime`. If you are adding a new event type, it goes in `core/src/events/event.rs`. If you are changing CLI behaviour, it goes in `cli/src/main.rs`.

---

## Setting up locally

**Requirements:** Rust stable (1.75+).

```bash
# Clone the repo
git clone https://github.com/nanny-run/nanny.git
cd nanny

# Build everything
cargo build --workspace

# Run all tests
cargo test --workspace

# Check for warnings (CI enforces this)
cargo clippy --workspace -- -D warnings
```

There are no external service dependencies. The bridge runs in-process during tests — no ports need to be open.

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

1. **Open an issue first** for anything beyond a small bug fix or typo. This avoids duplicate work and confirms the change fits the project's scope.
2. **Keep PRs focused.** One logical change per PR. Reviewers will ask you to split large PRs.
3. **Write tests.** PRs without tests for new behaviour will not be merged.
4. **Run clippy before pushing.** `cargo clippy --workspace -- -D warnings` must be clean.
5. **No `unwrap()` in non-test code.** Use `?` or explicit error handling.
6. **Update the docs** if your change affects user-facing behaviour, config schema, or events. The documentation lives in the `docs/` directory of this repository — update the relevant `.mdx` files in the same PR.

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
