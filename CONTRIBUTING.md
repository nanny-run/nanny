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

**Nanny is a primitive. It enforces authority. It does not think.**

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
- Declare a fixed `name()` and `declared_cost()` default
- Be deterministic and side-effect-bounded (not network-stateful)

### Config validation

`crates/config/src/lib.rs` accepts values like `max_calls = 0` without complaint. Adding clear range checks with actionable error messages is a high-value, low-risk contribution.

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

---

## Doc map

This repository has three doc surfaces. Keep each one in scope and consistent with its audience.

| Surface   | Where                                                                    | Audience                       | What it owns                                                                      |
| --------- | ------------------------------------------------------------------------ | ------------------------------ | --------------------------------------------------------------------------------- |
| Docs site | `docs/` (Mintlify)                                                       | Developers using Nanny         | Commands, configuration, SDK usage, concepts, reference                           |
| Root docs | `README.md`, `ARCHITECTURE.md`, `CHANGELOG.md`, `SECURITY.md`, this file | Contributors and evaluators    | Project overview, deep implementation model, contribution workflow, release notes |
| Wire protocol | `crates/bridge/PROTOCOL.md`                                          | Maintainers                    | Endpoints, headers, event shape. Internal, never published                        |

Entry points:

- **Docs site**
  - `docs/v0.4/index.mdx` (current version)
  - `docs/docs.json` (navigation, versions, redirects)
- **Root docs**
  - `README.md` (product overview and first-run path)
  - `ARCHITECTURE.md` (enforcement model, internal terms allowed)
  - `CHANGELOG.md` (release notes, authoritative user-visible changes)
  - `CONTRIBUTING.md` (how to work in this repo)
- **Examples**

If your PR changes user-facing behaviour, CLI output, config schema, or event format, update the docs site (`docs/`) and any affected example READMEs in the same PR.

**Rust crates** — all six are published to crates.io. `nannyd` (`cli`) is the developer-facing crate. The others are its published dependencies and are not intended to be used directly.

| Crate     | crates.io name  | Developer-facing | What it does                                                                                   |
| --------- | --------------- | ---------------- | ---------------------------------------------------------------------------------------------- |
| `cli`     | `nannyd`        | ✓                | The `nanny` binary and Rust SDK (`#[tool]`, `#[rule]`, `#[agent]`)                             |
| `core`    | `nanny-core`    | ✗                | Traits (`Policy`, `Ledger`, `ToolExecutor`) and the `ExecutionEvent` type. No implementations. |
| `runtime` | `nanny-runtime` | ✗                | Concrete impls: `ToolPermissionPolicy`, `RuleEvaluator`, `ChainPolicy`, `ToolRegistry`, built-in tools |
| `bridge`  | `nanny-bridge`  | ✗                | Local HTTP enforcement server (Unix socket / TCP); holds all execution state                   |
| `config`  | `nanny-config`  | ✗                | Parses `nanny.toml`; owns `NannyConfig`                                                        |
| `macros`  | `nanny-macros`  | ✗                | The `#[tool]`, `#[rule]`, `#[agent]` proc-macros (re-exported by `cli`)                        |

**The dependency direction is strict:** `core` has no internal dependencies. Everything else depends on `core`. `core` never imports `runtime`, `bridge`, or `cli`.

**Python SDK** — lives at `sdks/python/`. Published as `nanny-sdk` on PyPI. Toolchain: `uv` (package manager), `hatchling` (build backend), `pytest` + `pytest-httpserver` (tests), `ruff` (lint), `mypy` (type checking). The root `Cargo.toml` workspace does not include `sdks/` — there is no toolchain collision.

| Path                         | What it is                                                             |
| ---------------------------- | ---------------------------------------------------------------------- |
| `sdks/python/nanny_sdk/`     | The importable package (`from nanny_sdk import tool, rule, agent`)     |
| `sdks/python/tests/`         | Unit tests — all use a `mock_bridge` fixture, no real bridge required  |
| `sdks/python/pyproject.toml` | Package metadata, build config, tool config (`ruff`, `mypy`, `pytest`) |

If you are adding a new enforcement rule, it goes in `runtime`. If you are adding a new event type, it goes in `core/src/events/event.rs`. If you are changing CLI behaviour, it goes in `cli/src/main.rs`.

---

## Reference examples

`packs/` contains the first-party rule packs. Their tests run from `sdks/python`:

```sh
uv run pytest ../../packs/nanny-recommended/tests
```

_Nanny is open source under the [Apache-2.0 license](LICENSE)._
