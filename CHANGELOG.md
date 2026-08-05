# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-08-04

### Added

- **Per-app identity.** `nanny init` now writes a permanent `.nanny/app.toml`
  (an `app_id` plus a human-facing `name`) alongside `nanny.toml`, written
  once, ever, per app, and meant to be committed (an app id is not a
  secret). `nanny init` never regenerates an existing identity.
- **`nanny run --join=<appId>`**, explicitly joins a specific governance
  server by id, reading its state from `~/.nanny/servers/<appId>/`.
- **`--app=<id>` on `nanny status` / `nanny stop`**, targets one app's
  governor explicitly; both still default to the current directory's own
  identity when omitted.
- **Per-app Cloud credentials.** A gitignored `.nanny/credentials.local.toml`
  holds an app-scoped ingest credential, self-minted by `nanny run` on any
  machine that's logged in (`nanny auth login`), independent of the app's
  one-time `nanny init`.
- **A separate CONNECT-tunnel credential.** The HTTP CONNECT proxy (`[proxy]
  allowed_hosts`) now authenticates via its own `proxy_token`
  (`Proxy-Authorization: Basic`), never the ordinary `session_token`.
  Narrower blast radius if it ever ends up in a client's own verbose HTTP
  logs, which is the one place a CONNECT credential can leak given zero
  required app-side code changes.

### Changed

- **`[observability] log = "file"` now applies to `nanny run --serve`, not just
  local `nanny run`.** Previously `--serve` silently ignored `[observability]`
  entirely, so joined clients (`nanny run --join=<appId>`) never got a local
  event log. Both paths now share one `EventWriter`/local-log destination.
- **File logging no longer requires a path.** `[observability]`'s file-name
  field is renamed `log_file` → `file`, and is now optional: unset, it
  defaults to a filename of `log.ndjson`. The directory is no longer
  developer-specified — it's always `.nanny/logs/`, owned by Nanny,
  auto-created, and added to `.gitignore` automatically the first time it's
  created (these are local audit-trail logs, not source). `file` is a bare
  name only — no path separators, no extension, Nanny always appends
  `.ndjson` itself (e.g. `file = "events"` writes
  `.nanny/logs/events.ndjson`).
- **`--serve` state is now keyed by app id**
  (`~/.nanny/servers/<appId>/server.{addr,token,proxy_token,pid,proxy}`),
  replacing the old global, unkeyed `~/.nanny/server.*` files. Two
  unrelated apps' governors on one machine can no longer collide or
  overwrite each other's state.
- **CONNECT-request auth and rate limiting moved off axum's router layers**
  and into a single dispatch checkpoint (`GovernorService`) that every
  request passes through before reaching a handler, CONNECT included.
  Routing a CONNECT request through axum's `Router::call()` was silently
  breaking hyper's server-side upgrade handoff; this also closes the gap
  where a future protective check added only as a router layer would never
  have covered CONNECT.
- **Token comparisons are constant-time** (`session_token`, `proxy_token`),
  closing a timing side-channel in the equality check.
- **The injected `HTTPS_PROXY` URL always carries an explicit empty
  password** (`http://<token>:@host`). Some HTTP clients (Python's
  `requests`/urllib3) silently drop the username too when the password is
  merely absent rather than present-and-empty.

### Fixed

- **Python SDK: an unreachable bridge now raises `BridgeUnavailable`, not a
  raw httpx traceback.** `agent_enter`, `call_tool`, `health`, and
  `get_status` previously let a connection failure (governor not running,
  wrong address) propagate as an unhandled `httpx.ConnectError` through
  `@agent`/`@tool`. Every other bridge failure mode already gets a typed
  exception; this makes the "bridge simply isn't there" case consistent
  with the rest, matching how `@rule`'s own status check already handled it.

### Removed

- **`[runtime]` / `mode` in `nanny.toml`**, entirely. There is no config
  field for local-vs-managed anymore. Whether a run syncs to Cloud is
  decided purely by whether an app-scoped credential exists locally; no
  credential, no sync, no config knob. `--no-sync` still overrides.
- **Blind auto-join.** Bare `nanny run` no longer silently joins whatever
  governance server it happens to detect on the machine; `--join=<appId>`
  is now required and explicit.

## [0.4.2] — 2026-07-27

### Changed

- **The governance server is now `nanny run --serve`.** Manage it with `nanny status` and `nanny stop`. mTLS, certs, shared budget, and the network path are unchanged.

### Added

- **`nanny auth login` / `logout`** — connect a machine to Nanny Cloud via a browser device flow, or `--token` for CI/headless (reading `NANNY_API_KEY` or stdin). Stores an ingest-only key in `~/.nanny/credentials.toml`.
- **Cloud sync from `mode = "managed"` + login.** Both `nanny run` and the governance server forward the event log to the cloud when the project sets `mode = "managed"` and the machine is logged in; `--no-sync` skips a run. Enforcement stays fully local.

### Removed

- **`nanny server`** — use `nanny run --serve` to start the governance server, and `nanny status` / `nanny stop` to manage it.
- **`[managed]` endpoint/api_key config** — cloud connection is now `mode = "managed"` plus `nanny auth login`. A stale `[managed]` block is ignored with a one-time notice pointing at `nanny auth login`.

## [0.4.1] — 2026-07-21

### Added

- **`NANNY_API_KEY` environment variable** — the managed-mode API key is now read
  from the environment, so `nanny.toml` no longer has to hold a live credential.
  Resolution order is `NANNY_API_KEY` → `[managed] api_key`; a blank or
  whitespace-only env value counts as unset (an unset CI secret usually surfaces
  as `""`) and falls through rather than authenticating with an empty string.
  Matches the injection pattern already used by `NANNY_BRIDGE_CERT` /
  `NANNY_BRIDGE_KEY` / `NANNY_SESSION_TOKEN`, so one secrets manager can populate
  all of them.

### Changed

- **`[managed] api_key` is now optional** (`Option<String>`). Existing
  `nanny.toml` files are unaffected — a key already in the file keeps working.
  It remains supported as a local-experimentation fallback, but is overridden by
  `NANNY_API_KEY`.
  <br>**API note:** `ManagedConfig.api_key` changed from `String` to
  `Option<String>`. Code constructing or reading that field directly needs
  `Some(..)` / `.as_deref()`. Shipped in a patch release deliberately: there are
  no production consumers yet, and `nanny-config` is a published dependency of
  `nannyd` rather than a crate meant for direct use.
  This resolves a contradiction: the manifesto makes `nanny.toml` the source of
  truth — versionable and reviewable — while that same file was required to carry
  a secret that must never be committed. Managed mode is now two non-secret lines.
- When managed mode is enabled but no API key can be resolved, the runtime prints
  a warning and **continues enforcing locally** instead of silently forwarding
  nothing. Enforcement never depends on the cloud.

## [0.4.0] — 2026-07-07

### Added

- **Managed-mode cloud sender** — when `nanny.toml` sets `[runtime] mode = "managed"`,
  the runtime forwards a copy of its append-only event log to the cloud endpoint you
  configure (`[managed] endpoint` + `api_key`). Enforcement stays fully local;
  forwarding is best-effort — batched, non-blocking, and fail-safe (a slow or
  unreachable cloud never blocks or fails the run). No-op in local mode.
- **`nanny::set_harness(Harness { name, version })` (Rust SDK)** — declare the
  agentic harness that ran the agent (e.g. `opencode`, `langgraph`, `crewai`).
  Emits a `HarnessIdentified` attribution event (bridge `POST /harness`) that the
  managed sender forwards to the cloud — powering Fleet Intelligence's harness
  breakdown (our equivalent of OpenRouter's "app" column). Attribution label only:
  never content, never pricing, never touches the ledger. Distinct from
  `#[nanny::agent(...)]`, which names a limits scope. No-op in passthrough mode.
  (Python auto-detection via `instrument` is a follow-up; Rust declares explicitly.)

### Changed

- **`[managed]` config: `org_id` removed.** `ManagedConfig` is now `{ endpoint, api_key }`.

## [0.3.0] - 2026-07-05

### Added

- **`nanny_sdk.instrument(client)`** — call once at agent startup to automatically
  report LLM token usage to Nanny's budget. Intercepts completion responses from
  OpenAI, Groq, Together AI, Azure OpenAI, LiteLLM, Anthropic, Mistral, Google
  Gemini (google-genai), and Cohere v2. Uses duck-typing — no provider package is
  imported. No-op in passthrough mode.
- **`nanny::report_usage(Usage { .. })` (Rust SDK)** — the Rust counterpart to
  Python's `instrument()`. Rust cannot monkey-patch an LLM client, so usage is
  reported explicitly: after an LLM call, hand Nanny the `input`/`output` token
  counts from the response. Accepts optional `model`/`provider` attribution labels
  (identifiers only — never prompt/response content, never pricing). No-op in
  passthrough mode; fire-and-forget.
- **`LlmUsageRecorded` event** — reported LLM token usage now appears in the NDJSON
  event log with `input`, `output`, and optional `model`/`provider` labels.
  Previously usage was debited silently with no audit record.
- **`POST /llm/usage` bridge endpoint** — receives `{"input": N, "output": N}` (plus
  optional `model`/`provider` labels) and debits `input + output` from the token
  ledger. Used by `instrument()` (Python) and `report_usage()` (Rust).

### Changed

- **`cost` renamed to `tokens`** — all developer-facing surfaces now use `tokens`:
  `nanny.toml` fields (`tokens`, `tokens_per_call`), Python decorator (`@tool(tokens=N)`),
  Rust macro (`#[tool(tokens = N)]`), `PolicyContext.tokens_spent`, and the
  `ExecutionStopped` event field (`tokens_spent`). This is a breaking change for
  any `nanny.toml` files and agent code using the old `cost` field names.

---

## [0.2.0] - 2026-05-01

### Added

- **Governance server** — `nanny server start` runs a standalone enforcement daemon for cross-process
  and cross-machine agent fleets. All agents connected to the same server share one budget, one step
  counter, and one execution boundary. A runaway agent on one machine counts against the same budget
  as every other agent in the fleet.
- **Mutual TLS** — governance server on a non-loopback address enforces mTLS. The server verifies
  every connecting agent's client certificate against a CA. Connections without a valid cert are
  refused at the TLS handshake — before any governance logic runs.
- **`nanny certs` commands** — `generate`, `import`, `rotate`, `show`, `remove`. `generate` creates a
  complete PKI bundle (CA + server cert + client cert) in `~/.nanny/certs/` in one command. `import`
  accepts externally-issued certs (HashiCorp Vault, AWS ACM, any PKI system) with partial-import
  support for rotation without CA replacement. `rotate` regenerates server + client certs using the
  existing CA with zero downtime.
- **Certificate hot-reload** — the governance server watches `~/.nanny/certs/` for file changes.
  When certs are rotated or imported, the server reloads them without restarting. New connections use
  the new cert immediately; in-flight connections finish on the old cert. Works with Vault Agent,
  cert-manager, or any PKI automation that writes files to disk.
- **HTTP CONNECT proxy** — the governance server acts as an HTTP proxy on the same port (62669).
  All outbound HTTP from the agent routes through the server and is checked against an
  `allowed_hosts` allowlist in `nanny.toml`. Requests to hosts outside the list are denied with
  a `ToolDenied` event. Private IP ranges and cloud metadata endpoints (`169.254.169.254`) are
  always blocked, regardless of the allowlist.
- **`NANNY_BRIDGE_ADDR`** — new environment variable that points the Rust and Python SDKs at a
  remote governance server. Joins the existing `NANNY_BRIDGE_SOCKET` (Unix) and `NANNY_BRIDGE_PORT`
  (Windows). When set, `nanny run` skips starting a local bridge and routes the agent to the server.
- **`nanny health`** — checks all active Nanny components (local bridge, network server, certs) in
  one command. Exits `0` if healthy, `1` if not. Suitable for Docker `HEALTHCHECK`, Kubernetes
  liveness probes, and deployment scripts.
- **SIGTERM graceful drain** — `nanny server stop` sends `SIGTERM`. The server stops accepting new
  connections and waits up to 10 seconds for in-flight requests to complete before exiting. An agent
  mid-tool-call finishes cleanly rather than getting a connection reset.
- **Per-IP rate limiting** — the governance server enforces a hard 100 requests/second limit per
  client IP address. This is DoS protection, not a business feature — it prevents a runaway agent
  from starving governance for all other agents on the same server. The limit is a hardcoded
  constant, not a configuration option.
- **`[proxy]` section in `nanny.toml`** — configures the HTTP proxy allowlist. Supports exact
  hostnames and `*.suffix` wildcard patterns.

### Changed

- **`nanny run` respects `NANNY_BRIDGE_ADDR`** — when this variable is set, the CLI connects to
  the remote governance server instead of starting a local bridge. Cert env vars
  (`NANNY_BRIDGE_CERT`, `NANNY_BRIDGE_KEY`, `NANNY_BRIDGE_CA`) are auto-injected from
  `~/.nanny/certs/` for same-machine agents; set them manually for agents on other machines.
- **Default server port is 62669** — governance API and HTTP proxy share one port.
  62669 spells NANNY on a phone keypad.

## [0.1.8] - 2026-04-27

### Changed

- **Example apps switch from Ollama to hosted providers** — `webdingo`, `qabud`, and `dev_assist`
  now use Groq (`llama-3.3-70b-versatile`): free tier, no credit card required, reliable structured
  function calling. `metrics_crew` uses OpenAI (`gpt-4.1-nano`): the 12-task CrewAI pipeline
  accumulates context across tasks and needs a larger context window than Groq's free tier provides.
  Each example ships an `.env.example` and documents a one-line swap back to Ollama for offline use.
- **`dev_assist` rewritten as a LangGraph agent** — replaced the LangChain legacy ReAct agent
  with a LangGraph `StateGraph` with four explicit Python nodes: extract, read files, search,
  diagnose. Python drives every tool call directly; the LLM only reasons in the final synthesis
  node. Enforcement is guaranteed regardless of model structured-calling behavior.
- **`metrics_crew` restructured into single-tool CrewAI tasks** — each task now has exactly one
  tool and one instruction. Previously one task instructed the LLM to call five tools in
  sequence; that structure let the model hallucinate past tool calls. Single-tool tasks mean the
  LLM has one job per task and cannot skip enforcement.

### Fixed

- **Event taxonomy: `ToolDenied` and `RuleDenied` are now distinct events** — previously
  `ExecutionEvent::ToolDenied` fired for all tool denials with a `reason` field set to either
  `"ToolDenied"` (allowlist block) or `"RuleDenied"` (rule or `max_calls` violation). This
  produced contradictory NDJSON like `{"event":"ToolDenied","reason":"RuleDenied"}`. The event
  type is now the self-describing authority:
  - `ToolDenied { ts, tool }` — allowlist violation only; no `reason` field needed
  - `RuleDenied { ts, tool, rule_name }` — rule or `max_calls` violation; `rule_name` identifies
    which rule fired (e.g. `"no_spiral"` or `"http_get.max_calls"`)

## [0.1.7] - 2026-04-19

### Fixed

- **`nanny uninstall` works on Windows** — uses the `self-replace` crate
  (`FILE_FLAG_DELETE_ON_CLOSE` + spawned child, the same pattern rustup uses) to reliably
  delete the binary after the process exits. PATH registry entry and install directory are
  cleaned up in the same command. No internet required, no second command needed.
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

[0.3.0]: https://github.com/nanny-run/nanny/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/nanny-run/nanny/compare/v0.1.8...v0.2.0
[0.1.8]: https://github.com/nanny-run/nanny/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/nanny-run/nanny/compare/v0.1.5...v0.1.7
[0.1.5]: https://github.com/nanny-run/nanny/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/nanny-run/nanny/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/nanny-run/nanny/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/nanny-run/nanny/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/nanny-run/nanny/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/nanny-run/nanny/releases/tag/v0.1.0
