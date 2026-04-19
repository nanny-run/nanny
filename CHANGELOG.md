# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.6] - 2026-04-19

### Fixed

- **`nanny uninstall` works on Windows** — Windows locks running executables, so
  `nanny uninstall` now spawns a detached, hidden PowerShell process that waits for nanny
  to exit, then removes the binary, cleans the PATH registry entry, and removes the install
  directory if empty. No internet connection required, no second command needed.
- **Static MSVC CRT** — the Windows binary is now built with `+crt-static`, linking the
  Visual C++ runtime statically. No `VCRUNTIME140.dll` or VC++ Redistributable required on
  the target machine.

## [0.1.5] - 2026-04-19

### Added

- **Windows binary** — `nanny-windows-x86_64.zip` published to GitHub Releases. Install via
  `irm https://install.nanny.run/windows | iex` (PowerShell).
- **`install.ps1`** — Windows install script: detects arch, downloads the `.zip` from GitHub
  Releases, extracts to `%LOCALAPPDATA%\nanny\`, and adds it to the user PATH persistently.
- **`install.nanny.run`** — live install subdomain. `curl -fsSL https://install.nanny.run | sh`
  installs on macOS/Linux. `irm https://install.nanny.run/windows | iex` installs on Windows.

### Changed

- **`nanny init` overwrites with prompt** — previously exited with an error when `nanny.toml`
  already existed. It now prompts: "Replace it with the default template? Your current
  configuration will be lost. [y/N]". Answers `y` or `yes` overwrite; anything else exits
  without changes. To reset a config, run `nanny init` and confirm.
- **One `nanny.toml` per project enforced** — `nanny init` and `nanny run` now error immediately
  if multiple files matching `nanny*.toml` are found in the project directory, listing the
  offending filenames. A project must have exactly one `nanny.toml`.
- **`nanny init` template improved** — the generated `nanny.toml` now includes inline comments
  for every field, start command examples for Python, Rust, and Node, and a link to the full
  `nanny.toml` reference at `docs.nanny.run`.

### Fixed

- **`[tools] allowed = []` documented correctly** — the `nanny.toml` reference page incorrectly
  stated "Empty array means all tools are allowed." An empty `allowed` list denies every tool
  call. The reference, the generated template, and inline comments now state this correctly.
- **`fetch_bridge_status` fails closed** — `evaluate_local_rules` previously fell back to
  zeroed counters when the bridge was unreachable mid-execution. It now fails closed: if the
  bridge is active (`NANNY_BRIDGE_SOCKET` or `NANNY_BRIDGE_PORT` is set) and `/status` is
  unreachable, the process exits immediately with `BridgeUnavailable`. Silently continuing
  rule evaluation against empty counters is always a bug. Passthrough mode (no bridge env
  vars) retains zeroed defaults — correct behaviour when running outside `nanny run`.
- **`PolicyContext` counter fields populated from bridge** — `step_count` and
  `cost_units_spent` were previously always zero in rule callbacks. Both fields are now
  fetched from the bridge `/status` endpoint before every rule evaluation, giving `@rule`
  and `#[nanny::rule]` functions accurate live counters. Affects Rust SDK and Python SDK.
- **Python `@rule` decorator** — rule functions decorated with `@rule` now receive a fully
  populated `PolicyContext` including `step_count`, `cost_units_spent`, `tool_call_counts`,
  and `tool_call_history`. Previously counters were zeroed, making count-based rule logic
  unreliable. `RuleDenied` now raises correctly with the rule name as the exception argument.
- **Python SDK exception parity** — `RuleDenied(rule_name)` and `ToolDenied(tool_name)` now
  carry their respective names as the first positional argument, matching the Rust
  `StopReason` variants exactly. `AgentNotFound` is raised on 404 from `/agent/enter`.
- **Windows bridge uses OS-assigned port** — the TCP bridge previously bound to a hardcoded
  port (`47374`), which prevented concurrent `nanny run` processes on the same Windows
  machine — the second process would fail immediately with `WSAEADDRINUSE`. The bridge now
  binds to port `0` and lets the OS assign a free ephemeral port per execution. The assigned
  port is injected into the child process as `NANNY_BRIDGE_PORT` as before — nothing in the
  SDK or agent code changes.

## [0.1.4] - 2026-04-13

### Added

- **Python SDK** (`pip install nanny-sdk`) — brings the same `@tool`, `@rule`, and `@agent`
  governance model to Python agents as decorators. Works with any Python agent framework —
  LangChain, CrewAI, plain Python. All decorators are no-ops outside `nanny run`; zero
  overhead in development and CI. Requires Python 3.11+.
- **`dev_assist` example** — LangChain debug agent governed by Nanny. Given a stack trace,
  reads the relevant source files and searches for related symbols using ripgrep. Demonstrates
  `@tool(cost=N)`, `@rule("no_read_loop")`, and `@agent("debugger")` with both ReAct and
  Plan-and-Execute modes (`uv run dev debug --trace <file> --mode react|plan`).
- **`metrics_crew` example** — CrewAI four-agent incident analysis pipeline governed by
  Nanny. Ingestion, analysis, visualization, and reporter agents work in sequence on a server
  metrics CSV. Demonstrates per-role limits (`[limits.ingestion]`, `[limits.analysis]`,
  `[limits.visualization]`, `[limits.reporter]`), per-role tool allowlists enforced via
  `ToolDenied`, `@rule("no_analysis_loop")`, and Plotly HTML chart output.
- **CI for Python SDK** — `ci-python.yml` runs `pytest`, `ruff`, and `mypy` on Ubuntu and
  macOS across Python 3.11 and 3.13 on every push or PR touching `sdks/python/**`.
- **PyPI publish** — `publish-pypi` job in `release.yml` uses OIDC trusted publishing (no
  stored API token). Re-runnable independently via `workflow_dispatch` with a `version` input,
  matching the existing pattern for `publish-crates` and `homebrew-tap-publish`.

## [0.1.3] - 2026-04-04

### Added

- **Affordability pre-check** — `BudgetExhausted` now fires _before_ a tool executes when the
  remaining budget cannot cover the next call's declared cost. Previously the check only fired
  after the cost was already debited, allowing one call to overshoot the limit. The new check
  is `cost_units_spent + next_tool_cost > max_cost_units`; `next_tool_cost` is a new field on
  `PolicyContext` populated by the enforcement layer before each tool call.
- **`ToolFailed` event from built-in tools** — when `nanny::http_get` encounters a network
  error (DNS failure, HTTP error, timeout), the enforcement layer now emits a `ToolFailed`
  event to the structured log before returning the error to the caller. Previously the failure
  was silently swallowed with no audit record.
- **`--limits` ceiling cap** — when `nanny run --limits=<name>` is passed, every named agent
  scope activated during the run is capped to `min(scope_value, CLI_limit_value)` per
  dimension. A scope cannot silently exceed the operator-specified ceiling.

### Fixed

- `nanny::http_get` no longer calls `report_stop("ToolFailed")` on network errors. A tool
  failure is an audit event, not a hard stop. Whether to abort or recover is the agent's
  decision. Limits enforcement (budget, steps, timeout, allowlist) remains a hard stop.

## [0.1.2] - 2026-03-30

### Added

- **`[start]` config** — `nanny.toml` now accepts a `[start]` table with a `cmd` field.
  `nanny run` reads the command from config rather than requiring it on the CLI. Extra
  arguments passed after `--` are appended to the configured command.
- **`nanny::http_get`** — built-in SDK function that routes HTTP GET requests through the
  bridge. Enforced by the allowlist and rule system; costs 10 units on success.
- **`AgentScopeEntered` / `AgentScopeExited` events** — the event log now records when an
  agent enters or exits a named limits scope, including the limits active during that scope.
- **`ProcessCrashed` stop reason** — `ExecutionStopped` now distinguishes between a clean
  exit (`AgentCompleted`) and an unexpected non-zero exit (`ProcessCrashed`).
- **Async `#[agent]` support** — the `#[nanny::agent]` macro now correctly handles `async fn`
  decorated functions; the inner impl and call sites are generated as async.
- **`last_tool_args` in rule context** — rules now receive the arguments of the current
  tool call via `PolicyContext::last_tool_args`, enabling content-based enforcement.
- **`nanny uninstall`** — removes the `nanny` binary from its current install location.
  Detects Homebrew-managed installations and redirects to `brew uninstall nannyd` rather
  than removing the binary directly and leaving Homebrew metadata inconsistent.
- **Real-world sample apps** — two complete Rust agent samples using Ollama:
  - `examples/rust/webdingo` — web research agent (HTTP fetch + summarise)
  - `examples/rust/qabud` — codebase review agent (file tree + source analysis)
- **`ARCHITECTURE.md`** — developer design document covering the enforcement model,
  core abstractions, the direct-call pattern, stop reasons, and testing guidance.

### Fixed

- `ExecutionStopped` no longer emits `steps: 0` and `cost_spent: 0`. Step count and cost
  are now read from bridge metrics at process exit rather than hardcoded.
- `nanny run` prints the full `anyhow` error chain on failure (`:?#` formatting).
- Bridge `/stop` endpoint validates the reason string against the known set of stop reasons;
  an unknown reason from an untrusted child now maps to `ProcessCrashed`.
- `call_tool` now returns `Stop("BridgeUnavailable")` when the bridge is unreachable during
  a governed run, rather than silently allowing the tool call to proceed ungoverned.
- JSON arguments in `http_get`, `report_stop`, and `agent_enter` are now built with
  `serde_json::json!` instead of `format!`, preventing invalid JSON on special characters.
- `TimeoutExpired` added to the governance stop set, suppressing the misleading "0 tool
  calls" warning when execution ends due to timeout.
- `[start].cmd` is parsed with shell quoting rules (via `shlex`) so paths with spaces
  work correctly; unterminated quotes produce a clear error rather than a silent failure.

## [0.1.1] - 2026-03-26

### Fixed

- Added `readme` field to `nannyd` crate manifest so the README displays on crates.io.

## [0.1.0] - 2026-03-26

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
- **Homebrew tap** — `brew tap nanny-run/nanny && brew install nannyd` via `nanny-run/nanny`.
- **CI** — GitHub Actions workflows for test, clippy, and cross-compiled release builds.
  SHA256 checksums for each binary are computed and pushed to the homebrew tap automatically
  on every tagged release.

[0.1.6]: https://github.com/nanny-run/nanny/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/nanny-run/nanny/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/nanny-run/nanny/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/nanny-run/nanny/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/nanny-run/nanny/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/nanny-run/nanny/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/nanny-run/nanny/releases/tag/v0.1.0
