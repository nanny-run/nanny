# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-03-26

First public release of Nanny — an execution boundary for autonomous AI agents.

### Added

- **`nanny init`** — scaffolds a `nanny.toml` with safe default limits in the current
  directory and prints a usage snippet.
- **`nanny run [--limits=<name>] <cmd>`** — runs any command (Python, Rust, Go, Node,
  or any binary) under enforcement. Hard limits on steps, cost units, and wall-clock
  time are checked before each step; the process is killed immediately on breach.
- **Named limits sets** — `[limits.<name>]` blocks in `nanny.toml` allow per-agent
  overrides; `--limits=researcher` activates one set for a single run.
- **Tool allowlist** — `[tools] allowed` in `nanny.toml` declares which tool names
  may be called; any unlisted tool call stops execution with `TOOL_DENIED`.
- **Rust SDK macros** —
  - `#[tool(cost = N)]` — wraps a free function as a governed tool; cost is charged
    and all registered rules are evaluated before the function body runs.
  - `#[rule("name")]` — registers a `fn(&PolicyContext) -> bool` enforcement rule
    evaluated before every tool call; returning `false` stops execution with `RULE_DENIED`.
  - `#[agent("name")]` — activates a named limits set for the duration of a function,
    reverting on exit (including panics).
- **Passthrough mode** — all macros are zero-overhead no-ops when `nanny run` is not
  active; the original function runs exactly as written.
- **Structured NDJSON event log** — append-only log with these event types:
  - `ExecutionStarted` — limits in effect and command, emitted once at the start.
  - `ToolAllowed` / `ToolDenied` / `ToolFailed` — per-tool-call audit trail.
  - `StepCompleted` — emitted after each step by the SDK bridge.
  - `ExecutionStopped` — final event with `reason`, steps, cost spent, and elapsed ms.
    Stop reasons: `AGENT_COMPLETED`, `MAX_STEPS_REACHED`, `BUDGET_EXHAUSTED`,
    `TIMEOUT_EXPIRED`, `TOOL_DENIED`, `RULE_DENIED`, `MANUAL_STOP`.
- **Cross-platform binaries** — pre-built for macOS ARM, macOS Intel, and Linux x86_64,
  attached to each GitHub Release as `.tar.gz` archives.
- **curl installer** — `curl -fsSL https://install.nanny.run | sh` detects OS/arch
  and installs the `nanny` binary to `/usr/local/bin` or `~/.local/bin`.
- **Homebrew tap** — `brew install nanny-run/tap/nannyd` via `nanny-run/homebrew-tap`.
- **CI** — GitHub Actions workflows for test, clippy, and cross-compiled release builds.
  SHA256 checksums for each binary are computed and pushed to the homebrew tap automatically
  on every tagged release.

[0.1.0]: https://github.com/nanny-run/nanny/releases/tag/v0.1.0
