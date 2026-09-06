//! Cloud sync: forwards a copy of the append-only NDJSON event log to the cloud.
//!
//! Enforcement stays entirely local (the bridge is untouched); this is a
//! best-effort, fire-and-forget side channel.
//!
//! **One credential input: the `NANNY_API_KEY` environment variable.** There is
//! no config field, no credential file, no login command, and nothing is ever
//! written to disk to make sync work. Set the variable and a run syncs; leave it
//! unset and the run is local-only. That is the whole surface, and it is the
//! shape every comparable product uses (`DD_API_KEY`, `SENTRY_DSN`,
//! `LANGFUSE_SECRET_KEY`): a durable secret handed to the process by whatever
//! platform runs it, shared unchanged across every replica.
//!
//! Two overrides turn sync off while a key is present: `--no-sync` for one run,
//! `NANNY_NO_SYNC` for a whole machine. Nothing turns sync *on* except the key,
//! because a key's presence is already a deliberate act.
//!
//! Design guarantees:
//! - **Local-first.** With no key, nothing starts and the engine runs offline
//!   with no dependency. The API key buys cloud sync and nothing else. Every
//!   enforcement guarantee holds identically without it.
//! - **Non-blocking.** `enqueue` just pushes onto a channel; a background thread
//!   batches and POSTs, so the run's poll loop never waits on the network.
//! - **Fail-safe.** Network errors are swallowed (one warning, once). A slow or
//!   down cloud never blocks, fails, or alters the agent run.
//! - **Never silent.** Whether a run syncs is printed at startup, always. A run
//!   that silently stops reporting is the failure mode a compliance product can
//!   least afford, and it is exactly what v0.5.0 shipped.
//!
//! The organization and the app are derived on the cloud side: the org from the
//! API key, the app from the `AppIdentified` event in the payload, so the
//! request carries only the key (`X-Nanny-Key`) and a per-run session id
//! (`X-Nanny-Session`).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::cloud::CloudEnv;
use nanny_config::API_KEY_ENV;

/// Machine-wide override to disable forwarding. Mirrors `--no-sync`, but
/// persists across invocations, which `--no-sync` cannot. Any non-empty value
/// disables sync; the variable was advertised in `--help` since v0.4.x but was
/// read nowhere until now.
const NO_SYNC_ENV: &str = "NANNY_NO_SYNC";

// Match the cloud ingest caps so a batch is never rejected wholesale.

/// A resolved forwarding target: the full ingest URL and the key that authorizes
/// it. The URL is derived from a compiled host, never stored or user-supplied.
///
/// `Debug` is hand-written so the key can never reach a log line or a panic
/// message; a derived one would print it verbatim.
#[derive(Clone, PartialEq, Eq)]
pub struct SyncTarget {
    pub endpoint: String,
    pub api_key: String,
}

impl std::fmt::Debug for SyncTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncTarget")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Read `NANNY_API_KEY`, treating blank or whitespace-only as unset. An unset CI
/// secret usually surfaces as `""`, so falling through on empty is what stops a
/// run from authenticating with a nonsense key and getting a 401 it cannot see
/// (the runtime never inspects ingest status codes).
fn api_key_from_env() -> Option<String> {
    let key = std::env::var(API_KEY_ENV).ok()?;
    let key = key.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

/// Whether `NANNY_NO_SYNC` is set to any non-empty value.
fn no_sync_from_env() -> bool {
    std::env::var(NO_SYNC_ENV).is_ok_and(|v| !v.trim().is_empty())
}

/// Which side of the cloud's `nny_live_`/`nny_sdbx_` split a key belongs to,
/// derived from its prefix and nothing else: never configured, never asked
/// for (`--env` stays absent from `--help`; there is one host). An
/// unrecognized prefix (a key minted before the split existed, or anything
/// malformed) defaults to Live, matching the cloud's own default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Live,
    Sandbox,
}

impl Environment {
    fn from_api_key(api_key: &str) -> Self {
        if api_key.starts_with("nny_sdbx_") {
            Environment::Sandbox
        } else {
            Environment::Live
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Environment::Live => "live",
            Environment::Sandbox => "sandbox",
        }
    }

    /// The one this run is *not*: the side the startup notice checks.
    fn other(self) -> Self {
        match self {
            Environment::Live => Environment::Sandbox,
            Environment::Sandbox => Environment::Live,
        }
    }
}

/// Why a run is not syncing. Drives the startup line, so the reason is always
/// visible rather than inferred from an absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoSyncReason {
    /// `--no-sync` was passed for this run.
    Flag,
    /// `NANNY_NO_SYNC` is set on this machine.
    EnvOverride,
    /// No `NANNY_API_KEY`. The default state, and not an error.
    NoApiKey,
}

/// Decide whether (and where) to forward. `Err(reason)` means "don't sync", and
/// enforcement never depends on this either way.
///
/// Precedence: explicit off beats everything, then the key's presence decides.
/// There is deliberately no way to ask for sync without supplying a key, so the
/// two can never disagree.
pub fn resolve_sync(env: CloudEnv, no_sync: bool) -> Result<SyncTarget, NoSyncReason> {
    if no_sync {
        return Err(NoSyncReason::Flag);
    }
    if no_sync_from_env() {
        return Err(NoSyncReason::EnvOverride);
    }
    let api_key = api_key_from_env().ok_or(NoSyncReason::NoApiKey)?;
    Ok(SyncTarget {
        endpoint: env.ingest_url(),
        api_key,
    })
}

/// The startup line describing sync state. Printed on every run, never
/// suppressed: v0.5.0 mumbled `mode, local` and swallowed every real failure,
/// which is how an app can stop reporting for weeks without anyone noticing.
///
/// `mode` survives only as a derived display word, never read back as config.
pub fn sync_status_line(
    target: Result<&SyncTarget, NoSyncReason>,
    app_name: Option<&str>,
) -> String {
    match target {
        Ok(t) => {
            let host = t.endpoint.strip_suffix("/v1/ingest").unwrap_or(&t.endpoint);
            match app_name {
                Some(name) => format!("nanny: mode managed, syncing to {host} (app: {name})"),
                None => format!("nanny: mode managed, syncing to {host}"),
            }
        }
        Err(NoSyncReason::Flag) => {
            "nanny: mode local, not syncing (--no-sync). Enforcing locally.".to_string()
        }
        Err(NoSyncReason::EnvOverride) => {
            format!("nanny: mode local, not syncing ({NO_SYNC_ENV} is set). Enforcing locally.")
        }
        Err(NoSyncReason::NoApiKey) => {
            format!("nanny: mode local, not syncing ({API_KEY_ENV} is not set). Enforcing locally.")
        }
    }
}

// ── Delivery ──────────────────────────────────────────────────────────────────

/// How many times to attempt one batch before spooling it for later.
const SEND_ATTEMPTS: u32 = 4;
/// First backoff step; doubles each attempt (250ms, 500ms, 1s, 2s).
const BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Ceiling on the durable outbox. Generous (a month of a busy app's events is
/// far smaller) but finite, so a permanently unreachable cloud cannot fill a
/// disk. Exceeding it drops the oldest spooled batch and says so.
const MAX_SPOOL_BYTES: u64 = 64 * 1024 * 1024;

/// What happened to a batch, and therefore what to do with it next.
#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    /// Accepted. Nothing to keep.
    Sent,
    /// Not accepted, but might be later: transport failure, 5xx, or 429.
    /// Worth spooling.
    Retryable,
    /// Refused in a way that will not change: 4xx other than 429. A 401 will
    /// not start working, and a 413 will not shrink. Spooling these would fill
    /// the outbox with batches that can never drain.
    Permanent(u16),
}

/// POST one batch, retrying transient failures with bounded exponential backoff.
///
/// Runs on the forwarder thread, never the run's own, so sleeping here cannot
/// slow the agent down.
///
/// This is also where the runtime starts reading the response status at all.
/// Previously any outcome (200, 401, 500) was indistinguishable, and every
/// batch was dropped either way, so a rejected key looked exactly like success.
fn send_batch(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    api_key: &str,
    session: &str,
    body: &str,
) -> Delivery {
    let mut backoff = BACKOFF_BASE;
    for attempt in 1..=SEND_ATTEMPTS {
        let result = client
            .post(endpoint)
            .header("X-Nanny-Key", api_key)
            .header("X-Nanny-Session", session)
            .header("Content-Type", "application/x-ndjson")
            .body(body.to_string())
            .send();

        // A transport error falls through to the retry: there is no status to
        // read, and the next attempt may well connect.
        if let Ok(resp) = result {
            let status = resp.status();
            if status.is_success() {
                return Delivery::Sent;
            }
            // 429 and 5xx are worth another go; anything else 4xx is not.
            if !(status.as_u16() == 429 || status.is_server_error()) {
                return Delivery::Permanent(status.as_u16());
            }
        }

        if attempt < SEND_ATTEMPTS {
            std::thread::sleep(backoff);
            backoff *= 2;
        }
    }
    Delivery::Retryable
}

/// Why a batch is being dropped rather than retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dropped {
    /// The cloud refused this run's own events with a status that will not
    /// change on a retry.
    RefusedLive(u16),
    /// The cloud refused a batch held over from an earlier run.
    RefusedStored(u16),
    /// The outbox hit its size cap and the oldest batch had to go.
    OutboxFull,
}

/// What to tell an operator when a batch is dropped.
///
/// A pure function so each branch is directly assertable, the same shape
/// `Spool::other_environment_notice` already uses. It exists because the two
/// call sites below both used to state that the events "remain in the local
/// log" unconditionally, which is false whenever no log file was written:
/// `LogTarget` defaults to `Stdout`, and under `--serve` a stdout target is a
/// deliberate no-op, so the default fleet deployment has no local log at all.
/// Telling an operator their evidence is safe in a file that does not exist is
/// the one thing a compliance product must never do.
fn dropped_notice(dropped: Dropped, log_path: Option<&Path>) -> String {
    let cause = match dropped {
        Dropped::RefusedLive(status) => {
            format!("the cloud refused these events ({status}); not retrying")
        }
        Dropped::RefusedStored(status) => {
            format!("the cloud refused a batch held from an earlier run ({status}); discarding it")
        }
        Dropped::OutboxFull => {
            format!("outbox is full ({MAX_SPOOL_BYTES} bytes); dropping the oldest unsent batch")
        }
    };
    match log_path {
        Some(path) => format!(
            "nanny: sync: {cause}. Those events remain in {}. Enforcement is unaffected.",
            path.display()
        ),
        None => format!(
            "nanny: sync: {cause}. Those events were not written to a log file, so they are \
             gone: set [observability] log = \"file\" to keep a local copy. Enforcement is \
             unaffected."
        ),
    }
}

/// A durable outbox for batches the cloud could not take yet.
///
/// The local NDJSON log cannot serve as the buffer, which is worth spelling out
/// because it looks like it should. Two reasons, and the weaker one used to be
/// stated here alone:
///
/// - **Lifetime.** The spool is transient: a batch exists only while it is
///   undelivered and is removed the moment it lands, so a healthy run leaves it
///   empty. The log is permanent and append-only across every run the app has
///   ever done. Replaying it would mean re-sending history, not a backlog.
/// - **Ownership.** The log is the operator's, for their own tooling, and
///   `LogTarget` defaults to `Stdout`, so on most deployments there is no file
///   to replay at all. Sync cannot depend on a file the operator decides
///   whether to write.
///
/// An earlier version of this comment rested the case on the log carrying no
/// session. That stopped being true when every logged event gained `run_id` and
/// `seq`, and the cloud reads `run_id` from the payload. The separation still
/// holds, for the two reasons above, but not for that one.
///
/// The spool stores what the log cannot: the batch *and* the session it belongs
/// to, so a replay lands under the run that actually produced it. Cloud dedups
/// on (execution, content hash), so re-sending an overlapping batch is a no-op
/// and delivery only has to be at-least-once.
///
/// Partitioned by environment (`.nanny/spool/live/`, `.nanny/spool/sandbox/`),
/// derived from the key a `Spool` is constructed with. Before this split, the
/// spool stored `{session}\n{body}` with no endpoint or key recorded, and
/// `drain` posted with whatever key the *next* process happened to hold: a
/// batch held under a sandbox key would flush into live under a live key, and
/// report success. Partitioning makes that unreachable rather than guarded: a
/// `Spool` constructed with a live key only ever sees the live subdirectory.
pub struct Spool {
    dir: PathBuf,
    base_dir: PathBuf,
    /// Where this run also wrote its events locally, when it wrote them to a
    /// file at all. `None` under `log = "stdout"` (the default) and under
    /// `--serve`, where a file is the only local target. Carried so that a
    /// message about dropping a batch can say what actually happened to those
    /// events instead of assuming a file exists.
    log_path: Option<PathBuf>,
}

impl Spool {
    /// The outbox for an app, under its own `.nanny/` directory. Alongside the
    /// logs rather than in `~/.nanny`, so it travels with the checkout that
    /// produced it and is removed with it. `api_key` decides which
    /// environment's subdirectory this instance reads and writes: see the
    /// struct docs.
    pub fn new(base_dir: &Path, api_key: &str) -> Self {
        Self::for_environment(base_dir, Environment::from_api_key(api_key))
    }

    /// Same as `new`, given an already-resolved `Environment` rather than a
    /// key to derive one from: how the startup notice inspects the sibling
    /// side without a key for it.
    fn for_environment(base_dir: &Path, environment: Environment) -> Self {
        Self {
            dir: base_dir
                .join(".nanny")
                .join("spool")
                .join(environment.dir_name()),
            base_dir: base_dir.to_path_buf(),
            log_path: None,
        }
    }

    /// Where this run's events are also written locally, if anywhere.
    fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    /// Record where this run's events are also being written locally, so a
    /// dropped batch can name that file. Callers that write no log file leave
    /// this unset, which is the honest default rather than the convenient one.
    pub fn logging_to(mut self, log_path: Option<PathBuf>) -> Self {
        self.log_path = log_path;
        self
    }

    /// How many batches are held here, without touching any of them.
    fn count(&self) -> usize {
        self.entries().len()
    }

    /// The startup notice, as text: `None` when there is nothing to say. A
    /// pure function so the message itself is directly assertable, with the
    /// `eprintln!` side effect kept to the one-line caller below.
    fn other_environment_notice(base_dir: &Path, environment: Environment) -> Option<String> {
        let other = environment.other();
        let held = Self::for_environment(base_dir, other).count();
        (held > 0).then(|| {
            format!(
                "nanny: sync: {held} batch(es) held for {}, not sent: this run is {}",
                other.dir_name(),
                environment.dir_name()
            )
        })
    }

    /// If the *other* environment's outbox has batches held, say so: before
    /// this run's own forwarding starts, so switching from a sandbox key back
    /// to live (or vice versa) doesn't silently leave a pile of events
    /// unaccounted for. Never touches what it finds; drain only ever runs
    /// against the environment a run's own key resolved to.
    fn warn_about_the_other_environment(base_dir: &Path, environment: Environment) {
        if let Some(notice) = Self::other_environment_notice(base_dir, environment) {
            eprintln!("{notice}");
        }
    }

    /// Persist a batch for a later attempt. Best-effort: an outbox that cannot
    /// be written is a lost batch, which is exactly today's behaviour, so it
    /// must never escalate into failing the run.
    fn store(&self, session: &str, body: &str) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        // Spooled batches are event data, same as the logs beside them: they
        // belong on disk and never in git. Done here rather than at init time
        // because the directory only exists once something has to be held.
        self.ensure_gitignored();
        self.enforce_cap(body.len() as u64);

        // Session on the first line, batch after it: self-describing, and
        // parseable without a second index file that could drift out of sync.
        let contents = format!("{session}\n{body}");
        let name = format!("{}-{}.ndjson", now_millis(), uuid::Uuid::new_v4().simple());

        // Write to a temp name and rename into place, so a crash mid-write can
        // never leave a partial batch that a later drain would post as if whole.
        let tmp = self.dir.join(format!(".{name}.tmp"));
        if std::fs::write(&tmp, contents).is_ok() {
            let _ = std::fs::rename(&tmp, self.dir.join(&name));
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Best-effort `.gitignore` entry for the outbox, mirroring what the config
    /// crate already does for `.nanny/logs/`. A missed entry is a nudge, not a
    /// failure, but committed event data would be a real leak.
    fn ensure_gitignored(&self) {
        const LINE: &str = ".nanny/spool/";
        let path = self.base_dir.join(".gitignore");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let covered = existing.lines().any(|l| {
            let t = l.trim();
            t == LINE || t == ".nanny/spool" || t == ".nanny/" || t == ".nanny"
        });
        if covered {
            return;
        }
        let mut updated = existing;
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(LINE);
        updated.push('\n');
        let _ = std::fs::write(&path, updated);
    }

    /// Drop oldest batches until `incoming` fits under the cap.
    fn enforce_cap(&self, incoming: u64) {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|(_, size)| *size).sum();
        while total + incoming > MAX_SPOOL_BYTES && !entries.is_empty() {
            let (path, size) = entries.remove(0);
            eprintln!(
                "{}",
                dropped_notice(Dropped::OutboxFull, self.log_path.as_deref())
            );
            let _ = std::fs::remove_file(&path);
            total = total.saturating_sub(size);
        }
    }

    /// Spooled batches, oldest first. Filenames start with a millisecond
    /// timestamp, so lexical order is chronological.
    fn entries(&self) -> Vec<(PathBuf, u64)> {
        let Ok(read) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<(PathBuf, u64)> = read
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "ndjson"))
            .filter_map(|e| e.metadata().ok().map(|m| (e.path(), m.len())))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Try to deliver everything spooled, oldest first. Stops at the first
    /// batch that still cannot be sent, so ordering is preserved and a cloud
    /// that is still down is not hammered once per file.
    ///
    /// Returns how many batches were delivered.
    pub fn drain(
        &self,
        client: &reqwest::blocking::Client,
        endpoint: &str,
        api_key: &str,
    ) -> usize {
        let mut delivered = 0usize;
        for (path, _) in self.entries() {
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some((session, body)) = contents.split_once('\n') else {
                // Not a shape this code ever writes; drop it rather than retry
                // a malformed batch forever.
                let _ = std::fs::remove_file(&path);
                continue;
            };
            match send_batch(client, endpoint, api_key, session, body) {
                Delivery::Sent => {
                    let _ = std::fs::remove_file(&path);
                    delivered += 1;
                }
                Delivery::Permanent(status) => {
                    eprintln!(
                        "{}",
                        dropped_notice(Dropped::RefusedStored(status), self.log_path.as_deref())
                    );
                    let _ = std::fs::remove_file(&path);
                }
                Delivery::Retryable => break,
            }
        }
        delivered
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Forwards a governance server's per-run events to the cloud, one ingest batch
/// per run. Each batch's `X-Nanny-Session` is `{server_secret}:{run_id}`: a
/// per-run value that folds in the server's secret token, so the cloud groups
/// events per run with an unguessable, cross-org-collision-proof session (the API
/// key is still the real auth).
///
/// Returns a handle rather than being forgotten, because a governor exits: it
/// shuts down with the app it launched, which in development is seconds. The
/// drain that feeds this runs on a 250ms tick, so without a flush the final
/// batch never leaves, and the final batch is the one holding `ExecutionStopped`
/// and the stop reason. Delivery is still best effort, but a failure now lands
/// in the spool and is backfilled by the next run rather than disappearing.
pub struct ServerForwarder {
    handle: std::thread::JoinHandle<()>,
}

impl ServerForwarder {
    /// Spawn a background forwarder draining `(run_id, lines)` from the engine.
    ///
    /// The caller keeps the `Sender`; dropping it closes the channel, which is
    /// what tells this thread to finish and exit. See `flush_and_join`.
    pub fn spawn(
        rx: Receiver<(String, Vec<String>)>,
        endpoint: String,
        api_key: String,
        server_secret: String,
        base_dir: &Path,
        log_path: Option<PathBuf>,
    ) -> Self {
        Spool::warn_about_the_other_environment(base_dir, Environment::from_api_key(&api_key));
        let spool = Spool::new(base_dir, &api_key).logging_to(log_path);
        let handle = std::thread::spawn(move || {
            let Ok(client) = reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .build()
            else {
                return;
            };

            // Deliver anything a previous governor could not, before this one's
            // own traffic. A fleet's history matters more than one app's, since
            // every joined process reports through here.
            let recovered = spool.drain(&client, &endpoint, &api_key);
            if recovered > 0 {
                eprintln!("nanny: sync: delivered {recovered} batch(es) held from an earlier run");
            }

            let mut warned = false;
            while let Ok((run_id, lines)) = rx.recv() {
                if lines.is_empty() {
                    continue;
                }
                let session = format!("{server_secret}:{run_id}");
                let body = lines.join("\n");
                match send_batch(&client, &endpoint, &api_key, &session, &body) {
                    Delivery::Sent => {}
                    Delivery::Retryable => {
                        spool.store(&session, &body);
                        if !warned {
                            eprintln!(
                                "nanny: sync: cloud unreachable; holding fleet events locally \
                                 and retrying later (enforcement is unaffected)"
                            );
                            warned = true;
                        }
                    }
                    Delivery::Permanent(status) => {
                        if !warned {
                            eprintln!(
                                "{}",
                                dropped_notice(Dropped::RefusedLive(status), spool.log_path())
                            );
                            warned = true;
                        }
                    }
                }
            }
        });
        Self { handle }
    }

    /// Wait for the forwarder to deliver what it has, then stop.
    ///
    /// The caller drops its `Sender` first; that closes the channel, the loop
    /// above falls out of `rx.recv()`, and this waits for the thread to finish
    /// the batch it is on. Bounded by the client's 10s request timeout, so an
    /// unreachable cloud delays a shutdown rather than hanging it, and anything
    /// still undelivered has already been spooled for the next run.
    pub fn flush_and_join(self) {
        let _ = self.handle.join();
    }
}


#[cfg(test)]
mod tests {
    /// How long a test waits for its mock ingest server to see a request.
    ///
    /// Generous on purpose, for the reason the bridge's own client timeout is:
    /// the suite runs on every core, so a forwarder thread competes with the
    /// rest of the workspace. Five seconds passed alone and intermittently
    /// failed together, which reads as flakiness and is really a deadline.
    const INGEST_WAIT: Duration = Duration::from_secs(30);

    use super::*;
    use crate::cloud::CloudEnv;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// `resolve_sync` reads process-wide env vars, so these tests must not run
    /// concurrently with each other. Rust runs tests in threads by default, and
    /// a neighbour clearing NANNY_API_KEY mid-assert is a real flake, not a
    /// theoretical one.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with the two sync env vars set to exactly the given values,
    /// restoring whatever was there before. Poisoning is ignored: a panic in one
    /// test must not cascade into every other test in this module.
    fn with_env(api_key: Option<&str>, no_sync: Option<&str>, f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_key = std::env::var(API_KEY_ENV).ok();
        let prev_no_sync = std::env::var(NO_SYNC_ENV).ok();

        let apply = |name: &str, value: Option<&str>| unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        };
        apply(API_KEY_ENV, api_key);
        apply(NO_SYNC_ENV, no_sync);

        f();

        apply(API_KEY_ENV, prev_key.as_deref());
        apply(NO_SYNC_ENV, prev_no_sync.as_deref());
    }

    // ── resolve_sync (the gate): no network ──────────────────────────────

    #[test]
    fn syncs_when_the_api_key_is_set() {
        with_env(Some("nny_k"), None, || {
            let t = resolve_sync(CloudEnv::Prod, false).expect("key present → sync");
            assert_eq!(t.endpoint, CloudEnv::Prod.ingest_url());
            assert_eq!(t.api_key, "nny_k");
        });
    }

    #[test]
    fn the_env_selects_the_host_not_the_key() {
        with_env(Some("nny_k"), None, || {
            let t = resolve_sync(CloudEnv::Dev, false).expect("key present → sync");
            assert_eq!(
                t.endpoint,
                CloudEnv::Dev.ingest_url(),
                "--env picks the host"
            );
        });
    }

    #[test]
    fn no_sync_without_an_api_key() {
        with_env(None, None, || {
            assert_eq!(
                resolve_sync(CloudEnv::Prod, false),
                Err(NoSyncReason::NoApiKey)
            );
        });
    }

    #[test]
    fn a_blank_api_key_counts_as_unset() {
        // An unset CI secret usually surfaces as "", and authenticating with it
        // would 401 invisibly, since the runtime never reads ingest status.
        with_env(Some("   "), None, || {
            assert_eq!(
                resolve_sync(CloudEnv::Prod, false),
                Err(NoSyncReason::NoApiKey)
            );
        });
    }

    #[test]
    fn the_no_sync_flag_wins_over_a_present_key() {
        with_env(Some("nny_k"), None, || {
            assert_eq!(resolve_sync(CloudEnv::Prod, true), Err(NoSyncReason::Flag));
        });
    }

    #[test]
    fn the_no_sync_env_var_wins_over_a_present_key() {
        with_env(Some("nny_k"), Some("1"), || {
            assert_eq!(
                resolve_sync(CloudEnv::Prod, false),
                Err(NoSyncReason::EnvOverride)
            );
        });
    }

    #[test]
    fn a_blank_no_sync_env_var_does_not_disable_sync() {
        with_env(Some("nny_k"), Some(""), || {
            assert!(
                resolve_sync(CloudEnv::Prod, false).is_ok(),
                "empty means unset, as with the key"
            );
        });
    }

    // ── the startup line, never silent ───────────────────────────────────

    #[test]
    fn the_status_line_names_the_host_and_app_when_syncing() {
        let t = SyncTarget {
            endpoint: CloudEnv::Prod.ingest_url(),
            api_key: "nny_k".into(),
        };
        let line = sync_status_line(Ok(&t), Some("acme-agent"));
        assert!(line.contains("managed"), "{line}");
        assert!(line.contains("https://api.nanny.run"), "{line}");
        assert!(line.contains("acme-agent"), "{line}");
        assert!(
            !line.contains("/v1/ingest"),
            "show the host, not the route: {line}"
        );
        assert!(!line.contains("nny_k"), "never print the key: {line}");
    }

    #[test]
    fn every_not_syncing_reason_says_why_and_reassures() {
        for reason in [
            NoSyncReason::Flag,
            NoSyncReason::EnvOverride,
            NoSyncReason::NoApiKey,
        ] {
            let line = sync_status_line(Err(reason), None);
            assert!(line.contains("not syncing"), "{reason:?}: {line}");
            assert!(line.contains("Enforcing locally"), "{reason:?}: {line}");
        }
        assert!(sync_status_line(Err(NoSyncReason::NoApiKey), None).contains(API_KEY_ENV));
        assert!(sync_status_line(Err(NoSyncReason::EnvOverride), None).contains(NO_SYNC_ENV));
        assert!(sync_status_line(Err(NoSyncReason::Flag), None).contains("--no-sync"));
    }

    // ── The forwarder: mock ingest, injected endpoint ────────────────────

    /// One-shot HTTP server: captures the first request and returns 200.
    fn mock_ingest_server() -> (u16, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                let mut content_length = 0usize;
                let mut header_end: Option<usize> = None;
                loop {
                    let n = match stream.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            for line in String::from_utf8_lossy(&buf[..pos]).lines() {
                                if let Some(v) =
                                    line.to_ascii_lowercase().strip_prefix("content-length:")
                                {
                                    content_length = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                    if let Some(he) = header_end {
                        if buf.len() >= he + content_length {
                            break;
                        }
                    }
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            }
        });
        (port, rx)
    }

    /// A server that answers every request with `status` and nothing else.
    /// Loops rather than accepting once, since `send_batch` retries.
    fn mock_status_server(status: u16) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { break };
                let mut tmp = [0u8; 2048];
                let _ = stream.read(&mut tmp);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
            }
        });
        (port, handle)
    }

    /// A scratch app directory, so a test's outbox never touches a real one.
    fn temp_app_dir() -> PathBuf {
        static CNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = CNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("nanny-spool-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spooled_files(dir: &Path, api_key: &str) -> Vec<PathBuf> {
        Spool::new(dir, api_key)
            .entries()
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    /// Feed one batch through a `ServerForwarder` and wait for it to finish,
    /// which is exactly what governor shutdown does. These tests were written
    /// against `CloudSync`, the local path's forwarder; that path is gone, and
    /// the behaviour they cover, batching, spooling and backfill, belongs to
    /// the one forwarder that remains.
    fn forward_one(
        endpoint: String,
        api_key: &str,
        session: &str,
        dir: &Path,
        lines: &[&str],
    ) {
        let (tx, rx) = mpsc::channel();
        let forwarder = ServerForwarder::spawn(
            rx,
            endpoint,
            api_key.to_string(),
            session.to_string(),
            dir,
            None,
        );
        // The forwarder composes `{server_secret}:{run_id}` for the session
        // header, so the run id is part of what gets stored and asserted.
        tx.send((
            "r1".to_string(),
            lines.iter().map(|l| l.to_string()).collect(),
        ))
        .unwrap();
        drop(tx);
        forwarder.flush_and_join();
    }

    #[test]
    fn forwards_batched_ndjson_with_headers() {
        let (port, rx) = mock_ingest_server();
        let dir = temp_app_dir();
        let endpoint = format!("http://127.0.0.1:{port}/v1/ingest");
        forward_one(
            endpoint,
            "nny_test",
            "session-abc",
            &dir,
            &[r#"{"event":"ExecutionStarted"}"#, r#"{"event":"ExecutionStopped"}"#],
        );

        let req = rx
            .recv_timeout(INGEST_WAIT)
            .expect("server received a request");
        let lower = req.to_ascii_lowercase();
        assert!(
            lower.contains("post /v1/ingest"),
            "wrong path/method:\n{req}"
        );
        assert!(
            lower.contains("x-nanny-key: nny_test"),
            "missing X-Nanny-Key:\n{req}"
        );
        assert!(
            lower.contains("x-nanny-session: session-abc"),
            "missing X-Nanny-Session:\n{req}"
        );
        assert!(req.contains(r#"{"event":"ExecutionStarted"}"#));
        assert!(req.contains(r#"{"event":"ExecutionStopped"}"#));
    }

    #[test]
    fn unreachable_endpoint_never_panics_or_hangs() {
        let dir = temp_app_dir();
        // Returns, bounded by the connect timeout plus backoff.
        forward_one(
            "http://127.0.0.1:1/v1/ingest".to_string(),
            "k",
            "s",
            &dir,
            &[r#"{"event":"ToolAllowed"}"#],
        );
    }

    // ── Durable outbox ────────────────────────────────────────────────────────

    #[test]
    fn an_unreachable_cloud_holds_events_instead_of_dropping_them() {
        // The whole point of the outbox. Before this, a failed batch was
        // clear()ed and the events were gone. A cloud blip cost real history
        // out of a log whose value is being complete.
        let dir = temp_app_dir();
        forward_one(
            "http://127.0.0.1:1/v1/ingest".to_string(),
            "k",
            "run-1",
            &dir,
            &[r#"{"event":"ExecutionStarted"}"#],
        );

        let held = spooled_files(&dir, "k");
        assert_eq!(
            held.len(),
            1,
            "the undeliverable batch must be held, not dropped"
        );

        let contents = std::fs::read_to_string(&held[0]).unwrap();
        let (session, body) = contents
            .split_once('\n')
            .expect("session on the first line");
        assert_eq!(
            session, "run-1:r1",
            "the session must be stored with the batch, secret and run id both"
        );
        assert!(
            body.contains("ExecutionStarted"),
            "the events must be intact: {body}"
        );

        // Held batches are event data: on disk, never in git.
        let gitignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap_or_default();
        assert!(
            gitignore.contains(".nanny/spool/"),
            "the outbox must be gitignored: {gitignore:?}"
        );
    }

    #[test]
    fn a_held_batch_is_delivered_under_its_original_session() {
        // Why the outbox stores the session rather than replaying the local log:
        // the log spans every run the app has ever done and its lines carry no
        // session, so a byte-range replay would have to pick one and would merge
        // a whole history into a single bogus run. Attribution has to survive
        // the outage, not just the bytes.
        let dir = temp_app_dir();
        Spool::new(&dir, "nny_k").store("original-session", r#"{"event":"ToolAllowed"}"#);

        let (port, rx_srv) = mock_ingest_server();
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let delivered = Spool::new(&dir, "nny_k").drain(
            &client,
            &format!("http://127.0.0.1:{port}/v1/ingest"),
            "nny_k",
        );

        assert_eq!(delivered, 1);
        let req = rx_srv
            .recv_timeout(INGEST_WAIT)
            .expect("server received it");
        assert!(
            req.to_ascii_lowercase()
                .contains("x-nanny-session: original-session"),
            "a replayed batch must carry the session that produced it:\n{req}"
        );
        assert!(
            spooled_files(&dir, "nny_k").is_empty(),
            "a delivered batch must be removed"
        );
    }

    #[test]
    fn a_held_batch_survives_until_it_is_delivered() {
        // Drain against a cloud that is still down must keep the batch, not
        // consume it, otherwise the outbox loses exactly what it exists to keep.
        let dir = temp_app_dir();
        Spool::new(&dir, "k").store("s", r#"{"event":"X"}"#);

        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_millis(200))
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let delivered = Spool::new(&dir, "k").drain(&client, "http://127.0.0.1:1/v1/ingest", "k");

        assert_eq!(delivered, 0);
        assert_eq!(
            spooled_files(&dir, "k").len(),
            1,
            "an undelivered batch must be kept"
        );
    }

    #[test]
    fn batches_are_replayed_oldest_first() {
        // Ordering is part of an audit trail's meaning, so recovery must not
        // reorder it.
        let dir = temp_app_dir();
        let spool = Spool::new(&dir, "k");
        spool.store("s1", r#"{"n":1}"#);
        std::thread::sleep(Duration::from_millis(5));
        spool.store("s2", r#"{"n":2}"#);

        let files = spooled_files(&dir, "k");
        assert_eq!(files.len(), 2);
        let first = std::fs::read_to_string(&files[0]).unwrap();
        assert!(
            first.contains(r#"{"n":1}"#),
            "oldest batch must sort first: {first}"
        );
    }

    #[test]
    fn a_permanently_refused_batch_is_not_held_forever() {
        // A 4xx will not start succeeding: a rejected key stays rejected and an
        // oversized batch stays oversized. Holding those would fill the outbox
        // with batches that can never drain, pushing out ones that could.
        let dir = temp_app_dir();
        Spool::new(&dir, "k").store("s", r#"{"event":"X"}"#);

        let (port, _rx) = mock_status_server(401);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        Spool::new(&dir, "k").drain(&client, &format!("http://127.0.0.1:{port}/v1/ingest"), "k");

        assert!(
            spooled_files(&dir, "k").is_empty(),
            "a permanently refused batch must be discarded"
        );
    }

    // ── Partitioned by environment ──────────────────────────────

    #[test]
    fn a_dropped_batch_names_the_log_file_when_one_exists() {
        let path = PathBuf::from("/app/.nanny/logs/log.ndjson");
        let notice = dropped_notice(Dropped::RefusedStored(400), Some(&path));
        assert!(
            notice.contains("/app/.nanny/logs/log.ndjson"),
            "an operator needs the actual path to go and look: {notice}"
        );
        assert!(
            notice.contains("400"),
            "the refusing status belongs in it: {notice}"
        );
    }

    #[test]
    fn a_dropped_batch_never_claims_a_log_that_was_not_written() {
        // The defect this closes: both call sites used to say "the events
        // remain in the local log" unconditionally, while `LogTarget` defaults
        // to `Stdout` and `--serve` treats stdout as a no-op. On the default
        // fleet deployment that sentence was false at the exact moment Cloud
        // dropped the evidence.
        for dropped in [
            Dropped::RefusedLive(400),
            Dropped::RefusedStored(400),
            Dropped::OutboxFull,
        ] {
            let notice = dropped_notice(dropped, None);
            assert!(
                !notice.contains("remain in"),
                "must not claim the events are kept anywhere: {notice}"
            );
            assert!(
                notice.contains("not written to a log file"),
                "must say plainly that nothing was kept: {notice}"
            );
            assert!(
                notice.contains("log = \"file\""),
                "and must say how to keep it next time: {notice}"
            );
        }
    }

    #[test]
    fn a_full_outbox_says_so_rather_than_reporting_a_refusal() {
        let notice = dropped_notice(Dropped::OutboxFull, None);
        assert!(notice.contains("outbox is full"), "{notice}");
        assert!(
            !notice.contains("refused"),
            "a local cap is not the cloud rejecting anything: {notice}"
        );
    }

    #[test]
    fn a_sandbox_batch_is_invisible_to_a_live_drain_and_vice_versa() {
        // The bug this exists to close: before partitioning, `drain` posted
        // with whatever key the next process happened to hold, so a batch held
        // under a sandbox key could flush into live under a live key and
        // report success. A `Spool` constructed with a live key must never
        // even see a sandbox-held batch, not just refuse to send it.
        let dir = temp_app_dir();
        Spool::new(&dir, "nny_live_x").store("live-run", r#"{"event":"Live"}"#);
        Spool::new(&dir, "nny_sdbx_x").store("sandbox-run", r#"{"event":"Sandbox"}"#);

        assert_eq!(spooled_files(&dir, "nny_live_x").len(), 1);
        assert_eq!(spooled_files(&dir, "nny_sdbx_x").len(), 1);

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        // Draining with the live key must not touch the sandbox batch.
        let (port, rx_srv) = mock_ingest_server();
        let delivered = Spool::new(&dir, "nny_live_x").drain(
            &client,
            &format!("http://127.0.0.1:{port}/v1/ingest"),
            "nny_live_x",
        );
        assert_eq!(delivered, 1);
        let req = rx_srv
            .recv_timeout(INGEST_WAIT)
            .expect("the live batch was sent");
        assert!(req.contains("live-run"), "wrong batch sent:\n{req}");

        assert!(
            spooled_files(&dir, "nny_live_x").is_empty(),
            "the live batch was delivered"
        );
        assert_eq!(
            spooled_files(&dir, "nny_sdbx_x").len(),
            1,
            "the sandbox batch must survive a live drain untouched"
        );
    }

    #[test]
    fn an_unrecognized_key_prefix_defaults_to_live() {
        // A key minted before the split existed (or anything malformed) is
        // still a real key, and getting silently routed into a "sandbox" no
        // one is watching would be a worse failure than defaulting live,
        // matching the cloud's own default.
        let dir = temp_app_dir();
        Spool::new(&dir, "nny_oldformat").store("s", r#"{"event":"X"}"#);

        assert_eq!(spooled_files(&dir, "nny_live_anything").len(), 1);
    }

    // ── A mismatched spool is skipped, not sent ─────────────────

    #[test]
    fn the_notice_names_the_count_and_which_side_is_which() {
        let dir = temp_app_dir();
        Spool::new(&dir, "nny_sdbx_x").store("s1", r#"{"n":1}"#);
        Spool::new(&dir, "nny_sdbx_x").store("s2", r#"{"n":2}"#);

        let notice = Spool::other_environment_notice(&dir, Environment::Live)
            .expect("two sandbox batches are held");
        assert!(notice.contains("2 batch"), "{notice}");
        assert!(notice.contains("held for sandbox"), "{notice}");
        assert!(notice.contains("this run is live"), "{notice}");
    }

    #[test]
    fn no_notice_when_the_other_environment_is_empty() {
        let dir = temp_app_dir();
        assert_eq!(
            Spool::other_environment_notice(&dir, Environment::Live),
            None
        );
    }

    #[test]
    fn counts_what_the_other_environment_is_holding_without_touching_it() {
        let dir = temp_app_dir();
        Spool::new(&dir, "nny_sdbx_x").store("s1", r#"{"n":1}"#);
        Spool::new(&dir, "nny_sdbx_x").store("s2", r#"{"n":2}"#);

        assert_eq!(
            Spool::for_environment(&dir, Environment::Sandbox).count(),
            2
        );
        // The check itself must not consume anything: a count, not a drain.
        Spool::warn_about_the_other_environment(&dir, Environment::Live);
        assert_eq!(
            spooled_files(&dir, "nny_sdbx_x").len(),
            2,
            "checking the other environment's outbox must not touch it"
        );
    }

    #[test]
    fn a_live_run_never_drains_batches_held_for_sandbox_end_to_end() {
        // Switching keys back and forth (live today, sandbox tomorrow) must
        // recover cleanly: nothing spooled under one environment is ever sent
        // by a run authenticated under the other, all the way through
        // the forwarder itself, not just the lower-level `Spool::drain`.
        let dir = temp_app_dir();
        Spool::new(&dir, "nny_sdbx_x").store("held-for-sandbox", r#"{"event":"Sandbox"}"#);

        let (port, rx_srv) = mock_ingest_server();
        forward_one(
            format!("http://127.0.0.1:{port}/v1/ingest"),
            "nny_live_x",
            "live-session",
            &dir,
            &[r#"{"event":"LiveOnly"}"#],
        );

        let req = rx_srv
            .recv_timeout(INGEST_WAIT)
            .expect("the live run's own batch was sent");
        assert!(
            !req.contains("Sandbox"),
            "a live run must never send a batch held for sandbox:\n{req}"
        );
        assert_eq!(
            spooled_files(&dir, "nny_sdbx_x").len(),
            1,
            "switching back to sandbox later must recover this batch, it must survive, not be deleted"
        );
    }

    #[test]
    fn a_server_error_is_treated_as_retryable() {
        let (port, _rx) = mock_status_server(503);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let outcome = send_batch(
            &client,
            &format!("http://127.0.0.1:{port}/v1/ingest"),
            "k",
            "s",
            r#"{"event":"X"}"#,
        );
        assert_eq!(
            outcome,
            Delivery::Retryable,
            "5xx must be retried, not discarded"
        );
    }

    #[test]
    fn a_client_error_is_permanent() {
        let (port, _rx) = mock_status_server(413);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let outcome = send_batch(
            &client,
            &format!("http://127.0.0.1:{port}/v1/ingest"),
            "k",
            "s",
            r#"{"event":"X"}"#,
        );
        assert_eq!(outcome, Delivery::Permanent(413));
    }

    #[test]
    fn startup_delivers_what_an_earlier_run_could_not() {
        // End to end: a previous run left a batch behind, this run's forwarder
        // clears it before sending its own events.
        let dir = temp_app_dir();
        Spool::new(&dir, "nny_k").store("previous-run", r#"{"event":"FromEarlierRun"}"#);

        let (port, rx_srv) = mock_ingest_server();
        forward_one(
            format!("http://127.0.0.1:{port}/v1/ingest"),
            "nny_k",
            "current-run",
            &dir,
            &[],
        );

        let req = rx_srv
            .recv_timeout(INGEST_WAIT)
            .expect("backfill was sent");
        assert!(
            req.contains("FromEarlierRun"),
            "the held batch must go first:\n{req}"
        );
        assert!(
            spooled_files(&dir, "nny_k").is_empty(),
            "the outbox must be empty afterwards"
        );
    }

    #[test]
    fn server_forwarder_posts_per_run_with_derived_session() {
        let (port, rx_srv) = mock_ingest_server();
        let dir = temp_app_dir();
        let (tx, rx) = std::sync::mpsc::channel();
        ServerForwarder::spawn(
            rx,
            format!("http://127.0.0.1:{port}/v1/ingest"),
            "nny_k".to_string(),
            "srvsecret".to_string(),
            &dir,
            None,
        );
        tx.send(("run_abc".to_string(), vec![r#"{"e":"X"}"#.to_string()]))
            .unwrap();
        drop(tx);

        let req = rx_srv
            .recv_timeout(INGEST_WAIT)
            .expect("server received a request");
        let lower = req.to_ascii_lowercase();
        assert!(lower.contains("post /v1/ingest"), "wrong path:\n{req}");
        assert!(
            lower.contains("x-nanny-key: nny_k"),
            "missing X-Nanny-Key:\n{req}"
        );
        assert!(
            lower.contains("x-nanny-session: srvsecret:run_abc"),
            "the per-run session must fold in the server secret:\n{req}"
        );
    }

    #[test]
    fn flush_and_join_waits_for_the_last_batch_to_be_delivered() {
        // The governor exits when the app it launched exits, so anything still
        // in flight dies with the process. The last batch is the one carrying
        // ExecutionStopped and the stop reason.
        //
        // Asserting only that the batch arrives proves nothing: a detached
        // forwarder still delivers it, because a test process does not exit.
        // The property that actually saves those events is that shutdown
        // *waits*, so this measures that it blocked.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut tmp = [0u8; 4096];
                let n = stream.read(&mut tmp).unwrap_or(0);
                // Answer slowly, so a shutdown that does not wait is visibly
                // faster than one that does.
                std::thread::sleep(Duration::from_millis(600));
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = seen_tx.send(String::from_utf8_lossy(&tmp[..n]).to_string());
            }
        });

        let dir = std::env::temp_dir().join(format!("nanny-flush-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let (tx, rx) = mpsc::channel();
        let forwarder = ServerForwarder::spawn(
            rx,
            format!("http://127.0.0.1:{port}/v1/ingest"),
            "nny_sdbx_test".to_string(),
            "server-secret".to_string(),
            &dir,
            None,
        );
        tx.send((
            "run-last".to_string(),
            vec![r#"{"event":"ExecutionStopped"}"#.to_string()],
        ))
        .unwrap();

        // Exactly what governor shutdown does: close the channel, then wait.
        let started = std::time::Instant::now();
        drop(tx);
        forwarder.flush_and_join();
        let waited = started.elapsed();

        assert!(
            waited >= Duration::from_millis(400),
            "shutdown must wait for the in-flight batch, not detach from it; \
             returned after {waited:?}"
        );

        let body = seen
            .recv_timeout(INGEST_WAIT)
            .expect("the final batch must have been delivered");
        assert!(
            body.contains("ExecutionStopped"),
            "the last event must be in the delivered body: {body}"
        );
        assert!(
            body.contains("server-secret:run-last"),
            "and it must carry {{server_secret}}:{{run_id}}, so the cloud can group it: {body}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

}
