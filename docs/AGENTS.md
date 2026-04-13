# Documentation project instructions

## About this project

- This is a documentation site for [Nanny](https://github.com/nanny-run/nanny) — an open-source execution boundary for autonomous AI agents
- Pages are MDX files with YAML frontmatter, published at [docs.nanny.run](https://docs.nanny.run) via [Mintlify](https://mintlify.com)
- Configuration lives in `docs.json`
- Run `mint dev` to preview locally
- Run `mint broken-links` to check links

## Versioning policy

- Docs are versioned at the **minor** level: `v0.1/`, `v0.2/`, `v1.0/`, etc. — never at the patch level
- A new docs version is only created when the content would meaningfully mislead a user on a different release line (new mental model, removed commands, changed APIs)
- Patch releases update the existing version folder in place
- **Keep at most 4 versions** in the `docs.json` version switcher; drop the oldest when adding a fifth (add a redirect and a banner to the dropped version if it still has active users)
- All internal links must include the version prefix: `/v0.1/quickstart`, not `/quickstart`
- Redirects for removed or renamed paths live in the `"redirects"` array in `docs.json`

## Terminology

- **Nanny** — the product name; capitalise in prose, lowercase as the CLI command (`nanny run`)
- **execution boundary** — the correct description of what Nanny is; not "middleware", "wrapper", "proxy", or "SDK"
- **governed run** — a process running under `nanny run` with enforcement active
- **passthrough mode** — when macros/decorators are no-ops because `nanny run` is not active
- **stop reason** — the value in `ExecutionStopped.reason`; always use exact enum name (`BudgetExhausted`, not "budget exceeded")
- **named limit set** — a `[limits.<name>]` block in `nanny.toml`; not "limit profile" or "limit group"
- **tool** — a function annotated with `#[nanny::tool]` or `@tool`; not "action", "function", or "capability"
- **rule** — a function annotated with `#[nanny::rule]` or `@rule`; not "policy", "check", or "validator"
- **agent scope** — a named execution context activated by `#[nanny::agent]` or `@agent`
- **cost units** — the unit of budget; not "tokens", "credits", or "points"
- **`nanny.toml`** — always in backticks; not "the config file" or "nanny config"
- **bridge** — internal implementation term; **never use in user-facing docs**; describe externally as "Nanny's enforcement layer"

## Audience and content boundaries

### User-facing docs (`docs/`)

Audience: developers using Nanny in their projects.

- Focus on what to do, not how it works internally
- Never expose internal implementation details: bridge, socket paths, HTTP endpoints, crate internals
- Show concrete `nanny.toml` + code examples for every feature
- Stop reasons, event types, and `PolicyContext` fields must match the authoritative enum in `crates/core`
- Python SDK shipped in v0.1.4. Document it as a live, available feature. Do not add "coming soon" language.

### ARCHITECTURE.md

Audience: developers building integrations or wanting deep understanding of the enforcement model.

- Bridge internals may be described at a high level (the parent/child process model)
- Stop reasons must match `StopReason` enum exactly — `ToolFailed` is an event, not a stop reason
- Direct developers toward `CONTRIBUTING.md` for contributor workflow

### CONTRIBUTING.md

Audience: OSS contributors and maintainers.

- Bridge crate internals, dependency graph, publish order are all appropriate here
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
