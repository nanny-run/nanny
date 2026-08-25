# Bridge wire protocol

Internal contract between the governor and an SDK. **Not public documentation.**
It lives here, beside the implementation, rather than in `docs/`, because it
describes internals that no user should build against and that we intend to
change without ceremony. `crates/bridge/` is the source of truth; this file
explains the shape and the reasoning.

Written after v0.6.0 rather than before it: the reframe changed the endpoints
and the stop-reason set, and a spec written first would have documented a design
that no longer exists.

## Transport

- Unix domain socket on macOS and Linux, TCP loopback on Windows, never `0.0.0.0`.
- Plain HTTP on loopback; mTLS off loopback, mirrored between server and client.
- `X-Nanny-Session` is authentication ("who you are").
- `X-Nanny-Run-Id` is the enforcement partition ("which run"). Absent means the
  default run, so a headerless client still works.
- A stop returns `410` for that run only. The host process survives.

## The event line

Every line written to the log or forwarded to a sink is a `LoggedEvent`: the
event, flattened, plus two envelope fields.

```json
{"run_id":"a1b2","seq":3,"event":"ToolAllowed","ts":1756100000000,"tool":"web_search"}
```

| Field | Meaning |
|---|---|
| `run_id` | Which run produced this. Under `--serve` one governor serves many. |
| `seq` | Position in this run's stream, from 0. A gap means an event is missing. |
| `event` | Variant tag. The remaining keys belong to that variant. |
| `ts` | Milliseconds since the Unix epoch. |

**Why the envelope exists.** `nanny run --serve` is the supported launch mode.
The governor holds every live run in one map and drains all of them into one
shared log file. Without `run_id` on the line, entries from different runs
interleave with nothing to tell them apart, and the interleaving cannot be
reconstructed afterwards: draining is per-run and batched, so a run drained later
can append older timestamps after a run drained earlier appended newer ones, and
sorting by `ts` does not recover it. Pairing `ExecutionStarted` with
`ExecutionStopped` fails too, because a missing stop is a documented outcome (the
process crashed) rather than a parse error.

**Why it is flattened.** Consumers already parse `event` at the top level. A
nested envelope would have broken every one of them to gain nothing.

**Why `seq` is assigned at append time.** The drain loop walks many runs.
Numbering there would count across interleaved runs and manufacture gaps in the
one field whose purpose is making genuine gaps detectable. `BridgeState` owns the
counter, one per run.

**Who numbers what.** The CLI writes `ExecutionStarted` before the bridge exists
and reserves `seq: 0` for it. The bridge continues from 1. At the end the CLI
asks the bridge for `next_seq()` to stamp `ExecutionStopped`. One run, one
sequence, rather than two that collide at 0.

## Declared authority

Split across two events on purpose.

`ExecutionStarted` carries the config-side half, which the governor reads from
`nanny.toml`:

```json
{"event":"ExecutionStarted","allowed_tools":["web_search"],
 "tool_labels":{"web_search":["reads_untrusted"]},"config_hash":"9f2a…"}
```

`config_hash` is SHA-256 over the **parsed** config, so reformatting does not
mint a new policy version. It is the join key between a run and the revision that
governed it.

`RulesDeclared` carries the rules half, because rule bodies compile into the
agent's own process and the governor cannot see them:

```json
{"event":"RulesDeclared","rules":[{"name":"no_send_after_read","version":"2.1.0","pack":"nanny:owasp"}]}
```

`version` and `pack` are omitted for hand-written rules rather than sent as
null: a declaration carrying `version: null` reads as "this pack has no version"
instead of "this rule came from no pack".

`POST /rules` accepts both a bare string and the object form, so a governor
upgrade does not break agents that have not upgraded with it.

## Verdict attribution

`ToolAllowed` and `RuleDenied` carry `cleared_by`: the rules that evaluated and
allowed this call, in evaluation order.

```json
{"event":"ToolAllowed","tool":"send_outreach","cleared_by":["no_send_after_read","send_outreach.max_calls"]}
{"event":"RuleDenied","tool":"send_outreach","rule_name":"no_send_after_read","cleared_by":["ran_first"]}
```

- `RuleDenied` lists only rules that cleared **before** the one that fired.
  Evaluation short-circuits, so rules after it never produced a verdict and
  listing them would claim a control operated when it did not.
- `ToolDenied` carries nothing, correctly: the allowlist runs before any rule, so
  no rule evaluated. That is the "never reached" case, which an auditor needs
  distinguished from "ran clean".
- Absent rather than empty when nothing governed the call.

The list is assembled **client-side**. Rule bodies run in the agent's process
before the bridge is contacted, so the SDK is the only party that observes them;
it sends the list with `POST /tool/call` and with the `RuleDenied` stop report.
The engine appends its own `max_calls` pseudo-rule to the same list, so its
control is not the one control that leaves no evidence.

This is what satisfies the requirement to log every verdict, allow and refuse
alike. It records every evaluation's outcome per action rather than emitting one
event per evaluation, which would multiply log volume by rules times calls to
add per-rule timestamps that no evidence requirement asks for.

## `GET /status`

The context a rule reads before every call. Fields mirror the Rust
`PolicyContext` field for field; `tests/test_context_parity.py` fails if they
drift, because a field on one side only means the same rule text behaves
differently depending on which SDK runs it.

`now_ms` is wall-clock at evaluation. It is supplied rather than sampled so a
rule about *when* an action is permitted stays a pure function of its context.

## Stop reasons

Four, and the set is closed:

| Reason | Meaning |
|---|---|
| `ToolDenied` | Allowlist violation. Fires before any rule. |
| `RuleDenied` | A rule or a `max_calls` cap denied. |
| `AgentCompleted` | Normal termination. |
| `ManualStop` | The caller stopped the run. |

Only the first two are policy violations. `POST /stop` validates against this set
and maps anything else to `ProcessCrashed`, because a child process holding the
session token could otherwise falsify the log by claiming a clean exit.

## What this protocol deliberately does not do

- **No pack fetching.** `nanny rules add` puts packs on disk; the engine reads
  files. A runtime that could fetch a control would break the offline guarantee,
  the ban on remote dependencies, and determinism.
- **No signature verification during a run.** That needs trust roots and possibly
  a network call on the path that must stay deterministic. It happens at install.
- **No instruction from the cloud.** The orchestrator ingests facts the engine
  already produced. It never tells the engine what to enforce.
