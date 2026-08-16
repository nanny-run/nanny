//! Cloud sync — forwards a copy of the append-only NDJSON event log to the cloud.
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
//!   with no dependency. The API key buys cloud sync and nothing else — every
//!   enforcement guarantee holds identically without it.
//! - **Non-blocking.** `enqueue` just pushes onto a channel; a background thread
//!   batches and POSTs, so the run's poll loop never waits on the network.
//! - **Fail-safe.** Network errors are swallowed (one warning, once). A slow or
//!   down cloud never blocks, fails, or alters the agent run.
//! - **Never silent.** Whether a run syncs is printed at startup, always. A run
//!   that silently stops reporting is the failure mode a compliance product can
//!   least afford, and it is exactly what v0.5.0 shipped.
//!
//! The organization and the app are derived on the cloud side — the org from the
//! API key, the app from the `AppIdentified` event in the payload — so the
//! request carries only the key (`X-Nanny-Key`) and a per-run session id
//! (`X-Nanny-Session`).

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
const MAX_LINES: usize = 1000; // NDJSON lines per request
const MAX_BODY_BYTES: usize = 256 * 1024; // 256 KB per request
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

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
    if key.is_empty() { None } else { Some(key.to_string()) }
}

/// Whether `NANNY_NO_SYNC` is set to any non-empty value.
fn no_sync_from_env() -> bool {
    std::env::var(NO_SYNC_ENV).is_ok_and(|v| !v.trim().is_empty())
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
    Ok(SyncTarget { endpoint: env.ingest_url(), api_key })
}

/// The startup line describing sync state. Printed on every run, never
/// suppressed: v0.5.0 mumbled `mode, local` and swallowed every real failure,
/// which is how an app can stop reporting for weeks without anyone noticing.
///
/// `mode` survives only as a derived display word, never read back as config.
pub fn sync_status_line(target: Result<&SyncTarget, NoSyncReason>, app_name: Option<&str>) -> String {
    match target {
        Ok(t) => {
            let host = t.endpoint.strip_suffix("/v1/ingest").unwrap_or(&t.endpoint);
            match app_name {
                Some(name) => format!("nanny: mode managed — syncing to {host} (app: {name})"),
                None => format!("nanny: mode managed — syncing to {host}"),
            }
        }
        Err(NoSyncReason::Flag) => {
            "nanny: mode local — not syncing (--no-sync). Enforcing locally.".to_string()
        }
        Err(NoSyncReason::EnvOverride) => {
            format!("nanny: mode local — not syncing ({NO_SYNC_ENV} is set). Enforcing locally.")
        }
        Err(NoSyncReason::NoApiKey) => format!(
            "nanny: mode local — not syncing ({API_KEY_ENV} is not set). Enforcing locally."
        ),
    }
}

/// A forwarder to the cloud orchestrator. Present only when a run syncs.
pub struct CloudSync {
    tx: Sender<String>,
    handle: JoinHandle<()>,
}

impl CloudSync {
    /// Start the background forwarder for a resolved target. `None` only if the
    /// HTTP client fails to build — callers treat `None` as "do nothing".
    pub fn start(endpoint: String, api_key: String, session_token: &str) -> Option<Self> {
        let session = session_token.to_string();
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let handle = std::thread::spawn(move || worker(rx, client, endpoint, api_key, session));
        Some(Self { tx, handle })
    }

    /// Queue one NDJSON event line for forwarding. Non-blocking; never fails the
    /// run (a dropped receiver just means the worker already exited).
    pub fn enqueue(&self, line: String) {
        let _ = self.tx.send(line);
    }

    /// Flush any buffered events and stop the worker. Bounded by the client's
    /// request timeout, so a slow cloud can't hang `nanny run` indefinitely.
    pub fn flush_and_join(self) {
        drop(self.tx); // close the channel → worker flushes remaining and exits
        let _ = self.handle.join();
    }
}

/// Forwards a governance server's per-run events to the cloud, one ingest batch
/// per run. Each batch's `X-Nanny-Session` is `{server_secret}:{run_id}` — a
/// per-run value that folds in the server's secret token, so the cloud groups
/// events per run with an unguessable, cross-org-collision-proof session (the API
/// key is still the real auth). Fire and forget, like `CloudSync`.
pub struct ServerForwarder;

impl ServerForwarder {
    /// Spawn a background forwarder draining `(run_id, lines)` from the engine.
    pub fn spawn(
        rx: Receiver<(String, Vec<String>)>,
        endpoint: String,
        api_key: String,
        server_secret: String,
    ) {
        std::thread::spawn(move || {
            let Ok(client) = reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .build()
            else {
                return;
            };
            let mut warned = false;
            while let Ok((run_id, lines)) = rx.recv() {
                if lines.is_empty() {
                    continue;
                }
                let session = format!("{server_secret}:{run_id}");
                let result = client
                    .post(&endpoint)
                    .header("X-Nanny-Key", &api_key)
                    .header("X-Nanny-Session", &session)
                    .header("Content-Type", "application/x-ndjson")
                    .body(lines.join("\n"))
                    .send();
                if result.is_err() && !warned {
                    eprintln!(
                        "nanny: sync — failed to forward fleet events to the cloud (continuing locally)"
                    );
                    warned = true;
                }
            }
        });
    }
}

/// Background loop: batch lines and POST them; flush on cap, on idle tick, and
/// once more when the channel closes.
fn worker(
    rx: Receiver<String>,
    client: reqwest::blocking::Client,
    endpoint: String,
    api_key: String,
    session: String,
) {
    let mut batch: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    let mut warned = false;

    let flush = |batch: &mut Vec<String>, bytes: &mut usize, warned: &mut bool| {
        if batch.is_empty() {
            return;
        }
        let body = batch.join("\n");
        let result = client
            .post(&endpoint)
            .header("X-Nanny-Key", &api_key)
            .header("X-Nanny-Session", &session)
            .header("Content-Type", "application/x-ndjson")
            .body(body)
            .send();
        if result.is_err() && !*warned {
            eprintln!("nanny: sync — failed to forward events to the cloud (continuing locally)");
            *warned = true;
        }
        batch.clear();
        *bytes = 0;
    };

    loop {
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(line) => {
                if !batch.is_empty() && (batch.len() >= MAX_LINES || bytes + line.len() + 1 > MAX_BODY_BYTES) {
                    flush(&mut batch, &mut bytes, &mut warned);
                }
                bytes += line.len() + 1;
                batch.push(line);
            }
            Err(RecvTimeoutError::Timeout) => flush(&mut batch, &mut bytes, &mut warned),
            Err(RecvTimeoutError::Disconnected) => {
                flush(&mut batch, &mut bytes, &mut warned);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
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

    // ── resolve_sync (the gate) — no network ──────────────────────────────

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
            assert_eq!(t.endpoint, CloudEnv::Dev.ingest_url(), "--env picks the host");
        });
    }

    #[test]
    fn no_sync_without_an_api_key() {
        with_env(None, None, || {
            assert_eq!(resolve_sync(CloudEnv::Prod, false), Err(NoSyncReason::NoApiKey));
        });
    }

    #[test]
    fn a_blank_api_key_counts_as_unset() {
        // An unset CI secret usually surfaces as "", and authenticating with it
        // would 401 invisibly, since the runtime never reads ingest status.
        with_env(Some("   "), None, || {
            assert_eq!(resolve_sync(CloudEnv::Prod, false), Err(NoSyncReason::NoApiKey));
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
            assert_eq!(resolve_sync(CloudEnv::Prod, false), Err(NoSyncReason::EnvOverride));
        });
    }

    #[test]
    fn a_blank_no_sync_env_var_does_not_disable_sync() {
        with_env(Some("nny_k"), Some(""), || {
            assert!(resolve_sync(CloudEnv::Prod, false).is_ok(), "empty means unset, as with the key");
        });
    }

    // ── the startup line — never silent ───────────────────────────────────

    #[test]
    fn the_status_line_names_the_host_and_app_when_syncing() {
        let t = SyncTarget { endpoint: CloudEnv::Prod.ingest_url(), api_key: "nny_k".into() };
        let line = sync_status_line(Ok(&t), Some("gotm-nanny"));
        assert!(line.contains("managed"), "{line}");
        assert!(line.contains("https://api.nanny.run"), "{line}");
        assert!(line.contains("gotm-nanny"), "{line}");
        assert!(!line.contains("/v1/ingest"), "show the host, not the route: {line}");
        assert!(!line.contains("nny_k"), "never print the key: {line}");
    }

    #[test]
    fn every_not_syncing_reason_says_why_and_reassures() {
        for reason in [NoSyncReason::Flag, NoSyncReason::EnvOverride, NoSyncReason::NoApiKey] {
            let line = sync_status_line(Err(reason), None);
            assert!(line.contains("not syncing"), "{reason:?}: {line}");
            assert!(line.contains("Enforcing locally"), "{reason:?}: {line}");
        }
        assert!(sync_status_line(Err(NoSyncReason::NoApiKey), None).contains(API_KEY_ENV));
        assert!(sync_status_line(Err(NoSyncReason::EnvOverride), None).contains(NO_SYNC_ENV));
        assert!(sync_status_line(Err(NoSyncReason::Flag), None).contains("--no-sync"));
    }

    // ── CloudSync (the sender) — mock ingest, injected endpoint ───────────

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
                                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
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
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            }
        });
        (port, rx)
    }

    #[test]
    fn forwards_batched_ndjson_with_headers() {
        let (port, rx) = mock_ingest_server();
        let endpoint = format!("http://127.0.0.1:{port}/v1/ingest");
        let sender = CloudSync::start(endpoint, "nny_test".to_string(), "session-abc").expect("sender starts");
        sender.enqueue(r#"{"event":"ExecutionStarted"}"#.to_string());
        sender.enqueue(r#"{"event":"ExecutionStopped"}"#.to_string());
        sender.flush_and_join();

        let req = rx.recv_timeout(Duration::from_secs(5)).expect("server received a request");
        let lower = req.to_ascii_lowercase();
        assert!(lower.contains("post /v1/ingest"), "wrong path/method:\n{req}");
        assert!(lower.contains("x-nanny-key: nny_test"), "missing X-Nanny-Key:\n{req}");
        assert!(lower.contains("x-nanny-session: session-abc"), "missing X-Nanny-Session:\n{req}");
        assert!(req.contains(r#"{"event":"ExecutionStarted"}"#));
        assert!(req.contains(r#"{"event":"ExecutionStopped"}"#));
    }

    #[test]
    fn unreachable_endpoint_never_panics_or_hangs() {
        let sender = CloudSync::start("http://127.0.0.1:1/v1/ingest".to_string(), "k".to_string(), "s")
            .expect("sender starts");
        sender.enqueue(r#"{"event":"StepCompleted"}"#.to_string());
        sender.flush_and_join(); // returns (bounded by connect timeout), swallows the error
    }

    #[test]
    fn server_forwarder_posts_per_run_with_derived_session() {
        let (port, rx_srv) = mock_ingest_server();
        let (tx, rx) = std::sync::mpsc::channel();
        ServerForwarder::spawn(
            rx,
            format!("http://127.0.0.1:{port}/v1/ingest"),
            "nny_k".to_string(),
            "srvsecret".to_string(),
        );
        tx.send(("run_abc".to_string(), vec![r#"{"e":"X"}"#.to_string()])).unwrap();
        drop(tx);

        let req = rx_srv.recv_timeout(Duration::from_secs(5)).expect("server received a request");
        let lower = req.to_ascii_lowercase();
        assert!(lower.contains("post /v1/ingest"), "wrong path:\n{req}");
        assert!(lower.contains("x-nanny-key: nny_k"), "missing X-Nanny-Key:\n{req}");
        assert!(
            lower.contains("x-nanny-session: srvsecret:run_abc"),
            "the per-run session must fold in the server secret:\n{req}"
        );
    }
}
