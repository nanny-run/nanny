# OpenClaw capability inventory — notes from live setup (2026-08-02)

## What OpenClaw is

Real, open-source, self-hosted personal AI assistant (OpenClaw Foundation, non-profit). Node.js-based, not Python.

- Repo: https://github.com/openclaw/openclaw
- Site: https://openclaw.ai/
- Version installed: 2026.7.1-2

## Install

Used npm, not the `curl -fsSL https://openclaw.ai/install.sh | bash` shell installer that also exists. Both end up needing the same Node runtime underneath (Node 24.15+/22.22.3+/25.9+ required either way), so npm was chosen for auditability — no blind remote-script execution.

```
npm install -g openclaw@latest
openclaw onboard --install-daemon
```

Installed via nvm-managed Node v24.15.0.

## Runtime shape

- Installs as a macOS **LaunchAgent** (persistent background service), not a one-shot process.
  - Service file: `~/Library/LaunchAgents/ai.openclaw.gateway.plist`
  - Command: `/opt/homebrew/bin/node /Users/evans/.nvm/versions/node/v24.15.0/lib/node_modules/openclaw/dist/index.js gateway --port 18789`
  - Working dir: `~/.openclaw`
- Gateway binds loopback-only by default (`127.0.0.1:18789`), local dashboard at `http://127.0.0.1:18789/`.
- **Config file: `~/.openclaw/openclaw.json`** — both CLI and service config live here.
- Default model: `openai/gpt-5.5` — outbound LLM calls go to `api.openai.com`. Relevant for `[proxy] allowed_hosts` later.

**Implication for the demo:** since our proxy auto-injection only works when `nanny run` itself spawns the process, testing/recording requires stopping the LaunchAgent (`openclaw gateway stop`, or `launchctl unload` the plist) and launching the gateway command manually under `nanny run` instead of the managed service.

## Channels

Confirmed available (from the onboarding channel picker): WhatsApp, Telegram, Discord, Slack, Signal, iMessage, SMS (Twilio), IRC, Teams, Matrix, Feishu, LINE, Mattermost, Nextcloud Talk, Nostr, Synology Chat, Tlon, Twitch, Zalo (x3), WeCom, Weixin, QQ, ClickClack, Raft, Yuanbao, and more.

**Decision: Telegram, not mail, and not WhatsApp.**

- Mail was the original plan (matches the real cited incident) but isn't a "channel" at all — see below. Building a safe sandbox for it is meaningfully harder than the alternative.
- WhatsApp was ruled out for safety: its channel authenticates by **linking your real WhatsApp account via QR code** (Baileys library, same mechanism as WhatsApp Web) — the agent would get whatever visibility that grants into your actual account, not an isolated identity. This is almost certainly how the real incident's channel worked too (the cited screenshot showed WhatsApp).
- **Telegram uses a fully separate bot identity** (created via `@BotFather`, a fresh token) with zero access to anything until explicitly invited to a chat — structurally the safest option for a disposable sandbox.

Setup used: private test group (not linked to any real contacts), bot added as admin, seeded with dummy "clutter" messages.

## Mail is a skill, not a channel — and not a simple base-URL override

Onboarding auto-installs CLI dependencies for many skills regardless of which you'll actually use, including:
- **`himalaya`** — generic IMAP/SMTP CLI (list, read, search, compose, reply, forward, copy, move — the operations that would let an agent mass-delete an inbox).
- **`gog`** — Google Workspace CLI, includes Gmail specifically.
- Also installed: `1password`, `peekaboo` (macOS UI automation), `apple-notes`, `apple-reminders`, `xurl` (X/Twitter), and ~20 others.

Neither mail tool exposes a simple "point at a custom base URL" override the way an HTTP-API integration would — Himalaya needs real IMAP/SMTP account config. Building a safe sandbox would mean either a mock IMAP server or real disposable mailbox credentials, meaningfully more setup than Telegram's model. **Left disabled — no mail credentials configured, matching least-privilege.**

## Security posture (from `openclaw security audit --deep` on a fresh install)

Default agent profile (`agents.defaults`) is wide open: `sandbox=off`, `runtime=[exec, process]`, filesystem `[read, write, edit, apply_patch]` not scoped to a workspace (`fs.workspaceOnly=false`), browser control enabled. Also flagged: Telegram group had no sender allowlist (`allowFrom` missing — anyone in the group could invoke commands), and `gateway.controlUi.allowInsecureAuth=true`.

**Attempted fix (reverted):** tightening `agents.defaults.tools.deny` to remove `exec`/`process`/`fs`/`browser` broke basic message sending; reverted via `openclaw doctor --fix`, which restored a fresh working config. **Full tool/sandbox hardening deferred** — not required for the current demo (task is narrowly scoped to Telegram message actions only), but worth revisiting before this is a public-facing example, not just a local test.

**Fixed and kept:** Telegram group `allowFrom` allowlist, scoped to the specific test group's chat ID and our own user ID — found both via `openclaw logs --follow` after sending a message in the group (look for `chat.id` and `from.id` in the log line).

## CLI usage notes

- `openclaw message send --channel telegram --target <id> --message "..."` — target is a numeric chat ID or `@username`. Bots cannot message other bots (including themselves) — target the group's chat ID, not the bot's own handle.
- Negative group chat IDs need the `=` form to avoid the leading `-` being parsed as a flag: `--target=-1001234567890`.
- Confirmed working: message successfully posted to the test group via this CLI.

## Open, unverified (Step 2, next)

Whether OpenClaw's own outbound HTTP calls (Node/undici-based) honor `HTTPS_PROXY`/`HTTP_PROXY` — undocumented either way in OpenClaw's own docs. Requires the live test in the tracker's Step 2, not assumption.
