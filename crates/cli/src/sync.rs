//! Cloud sync — forwards a copy of the append-only NDJSON event log to the cloud.
//!
//! Enforcement stays entirely local (the bridge is untouched); this is a
//! best-effort, fire-and-forget side channel. It runs only when the run opted in
//! two ways: `mode = "managed"` in nanny.toml AND a prior `nanny auth login`
//! (a stored credential). `--no-sync` skips a run.
//!
//! Design guarantees:
//! - **Local-first.** In local mode, or when not logged in, nothing starts and
//!   the engine runs offline with no dependency.
//! - **Non-blocking.** `enqueue` just pushes onto a channel; a background thread
//!   batches and POSTs, so the run's poll loop never waits on the network.
//! - **Fail-safe.** Network errors are swallowed (one warning, once). A slow or
//!   down cloud never blocks, fails, or alters the agent run.
//!
//! The organization is derived from the API key on the cloud side, so only the
//! key (`X-Nanny-Key`) and a per-run session id (`X-Nanny-Session`) are sent.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use nanny_config::{Mode, NannyConfig};

use crate::credentials::Credentials;

// Match the cloud ingest caps so a batch is never rejected wholesale.
const MAX_LINES: usize = 1000; // NDJSON lines per request
const MAX_BODY_BYTES: usize = 256 * 1024; // 256 KB per request
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// A resolved forwarding target: the full ingest URL and the key that authorizes
/// it. The URL is derived from the credential's env, never stored or user-supplied.
pub struct SyncTarget {
    pub endpoint: String,
    pub api_key: String,
}

/// Decide whether — and where — to forward. Sync is opt-in twice over:
/// `mode = "managed"` (this project wants cloud) AND a stored credential (this
/// machine logged in). `--no-sync` overrides to off. `None` means "don't sync";
/// enforcement never depends on this.
pub fn resolve_sync(
    config: &NannyConfig,
    credentials: Option<&Credentials>,
    no_sync: bool,
) -> Option<SyncTarget> {
    if no_sync {
        return None;
    }
    if config.runtime.mode != Mode::Managed {
        // Local mode never syncs, even when logged in.
        return None;
    }
    let Some(creds) = credentials else {
        // Managed mode was asked for but this machine isn't logged in. Say so
        // loudly; local enforcement is unaffected.
        eprintln!(
            "nanny: mode is \"managed\" but this machine is not logged in — run \
             `nanny auth login`. Enforcing locally, not syncing."
        );
        return None;
    };
    Some(SyncTarget {
        endpoint: creds.env.ingest_url(),
        api_key: creds.api_key.clone(),
    })
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

    fn parse_config(toml_str: &str) -> NannyConfig {
        toml::from_str(toml_str).expect("test config must parse")
    }

    fn managed_config() -> NannyConfig {
        parse_config(
            "[runtime]\nmode = \"managed\"\n\n[start]\ncmd = \"true\"\n\n\
             [limits]\nsteps = 10\ntokens = 100\ntimeout = 1000\n\n[tools]\nallowed = []\n",
        )
    }

    fn local_config() -> NannyConfig {
        parse_config(
            "[runtime]\nmode = \"local\"\n\n[start]\ncmd = \"true\"\n\n\
             [limits]\nsteps = 10\ntokens = 100\ntimeout = 1000\n\n[tools]\nallowed = []\n",
        )
    }

    fn cred(env: CloudEnv) -> Credentials {
        Credentials { api_key: "nny_k".to_string(), env }
    }

    // ── resolve_sync (the gate) — no network ──────────────────────────────

    #[test]
    fn syncs_when_managed_and_logged_in() {
        let c = cred(CloudEnv::Prod);
        let t = resolve_sync(&managed_config(), Some(&c), false).expect("managed + login → sync");
        assert_eq!(t.endpoint, CloudEnv::Prod.ingest_url(), "endpoint derived from the credential env");
        assert_eq!(t.api_key, "nny_k");
    }

    #[test]
    fn no_sync_in_local_mode_even_when_logged_in() {
        let c = cred(CloudEnv::Prod);
        assert!(resolve_sync(&local_config(), Some(&c), false).is_none(), "local mode never syncs");
    }

    #[test]
    fn no_sync_when_managed_but_not_logged_in() {
        assert!(resolve_sync(&managed_config(), None, false).is_none(), "managed without a credential → no sync");
    }

    #[test]
    fn no_sync_flag_disables_sync() {
        let c = cred(CloudEnv::Prod);
        assert!(resolve_sync(&managed_config(), Some(&c), true).is_none(), "--no-sync overrides to off");
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
