# Documentation project instructions

## About this project

- This is a documentation site for [Nanny](https://github.com/nanny-run/nanny) — an open-source enforcement layer for autonomous AI agents
- Pages are MDX files with YAML frontmatter, published at [docs.nanny.run](https://docs.nanny.run) via [Mintlify](https://mintlify.com)
- Configuration lives in `docs.json`
- Run `mint dev` to preview locally
- Run `mint broken-links` to check links

## Versioning policy

- Docs are versioned at the **minor** level: `v0.5/`, `v1.0/`, etc, never at the patch level
- A new docs version is only created when the content would meaningfully mislead a user on a different release line (new mental model, removed commands, changed APIs)
- Patch releases update the existing version folder in place
- **Keep at most 4 versions** in the `docs.json` version switcher; drop the oldest when adding a fifth (add a redirect and a banner to the dropped version if it still has active users)
- All internal links must include the version prefix: `/v0.5/quickstart`, not `/quickstart`
- Redirects for removed or renamed paths live in the `"redirects"` array in `docs.json`

## Docs are present tense

Docs describe the current release and nothing else.

- **A removed feature is deleted from the docs, not marked deprecated.** No
  "removed in", no "no longer supported", no strikethrough. A reader on the
  current release should not learn that something they never had is gone.
- **A page whose whole subject was removed is deleted**, not rewritten into a
  stub explaining its own absence.
- **No "coming soon" or "not built yet"** for anything shipped, and nothing
  unshipped gets a page at all.
- **`CHANGELOG.md` is the only place history lives.** That is what it is for,
  and keeping history out of the docs is what lets a page be read as fact.

Deleting a subsystem obliges you to delete its documentation in the same pass.
Every stale page on this site got there because that step was optional.

## Terminology

- **Nanny** — the product name; capitalise in prose, lowercase as the CLI command (`nanny run`)
- **enforcement primitive** — the correct description of what Nanny is; not "middleware", "wrapper", "proxy", or "SDK"
- **governed run** — a process running under `nanny run` with enforcement active
- **passthrough mode** — when macros/decorators are no-ops because `nanny run` is not active
- **stop reason** — the value in `ExecutionStopped.reason`; always use the exact enum name (`RuleDenied`, not "rule violation")
- **tool** — a function annotated with `#[nanny::tool]` or `@tool`; not "action", "function", or "capability"
- **rule** — a function annotated with `#[nanny::rule]` or `@rule`; not "policy", "check", or "validator"
- **agent scope** — a named execution context activated by `#[nanny::agent]` or `@agent`
- **tool label** — an operator-declared property of a tool (`reads_untrusted`, `external_effect`, `destructive`, `moves_money`, `reads_sensitive`); not "tag", "category", or "permission"
- **declared authority** — what an agent was permitted to do, recorded before it did anything; not "permissions" or "grants"
- **rule pack** — a versioned set of rules installed with `nanny rules add`; not "plugin", "ruleset", or "policy bundle"
- **tokens** — measured, never enforced; the field is `tokens_spent` in `PolicyContext`; not "budget", "cost units", or "credits"
- **`nanny.toml`** — always in backticks; not "the config file" or "nanny config"
- **bridge** — internal implementation term; **never use in user-facing docs**; describe externally as "Nanny's enforcement layer"

## Audience and content boundaries

### User-facing docs (`docs/`)

Audience: developers using Nanny in their projects.

- Focus on what to do, not how it works internally
- Never expose internal implementation details: bridge, socket paths, HTTP endpoints, crate internals
- Show concrete `nanny.toml` + code examples for every feature
- Stop reasons, event types, and `PolicyContext` fields must match the authoritative enum in `crates/core`
- Never name the wire protocol, its endpoints, or its headers. `crates/bridge/PROTOCOL.md` is internal and stays internal.

### ARCHITECTURE.md

Audience: developers building integrations or wanting deep understanding of the enforcement model.

- Enforcement-layer internals may be described at a high level (the parent/child process model)
- Stop reasons must match the `StopReason` enum exactly. `ToolFailed` is an event, not a stop reason
- Direct developers toward `CONTRIBUTING.md` for contributor workflow

### CONTRIBUTING.md

Audience: OSS contributors and maintainers.

- Enforcement-layer crate internals, dependency graph, publish order are all appropriate here
- The wire protocol is documented in `crates/bridge/PROTOCOL.md`, beside its implementation. Link to it; never restate it
- Keep the codebase map in sync with actual `publish = false` settings in `Cargo.toml`
- Do not duplicate content from user-facing docs

## Style preferences

- Active voice and second person ("you", "your agent")
- Sentence case for headings
- One idea per sentence; short paragraphs
- Lead with the command or concept before explaining it
- Code formatting for: file names, commands, field names, crate names, stop reasons, event types
- Bold for the first mention of a key term being defined
- No em dashes — use commas or periods instead
- No filler: "Note that", "Please be aware", "It is important to"
