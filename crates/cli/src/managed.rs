//! Managed-mode cloud sender.
//!
//! When `nanny.toml` has `[runtime] mode = "managed"`, this forwards a copy of
//! the append-only NDJSON event log to the configured cloud endpoint. Enforcement
//! stays entirely local (the bridge is untouched); this is a best-effort,
//! fire-and-forget side-channel.
//!
//! Design guarantees:
//! - **Local-first.** In local mode (or if `[managed]` is absent) nothing starts
//!   and `enqueue` is a no-op. The engine still runs offline with no dependency.
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

// Match the cloud ingest caps so a batch is never rejected wholesale.
const MAX_LINES: usize = 1000; // NDJSON lines per request
const MAX_BODY_BYTES: usize = 256 * 1024; // 256 KB per request
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// A forwarder to the cloud orchestrator. Only present in managed mode.
pub struct ManagedSender {
    tx: Sender<String>,
    handle: JoinHandle<()>,
}

impl ManagedSender {
    /// Start the sender iff `mode = "managed"` and `[managed]` is configured.
    /// Returns `None` in local mode — callers treat `None` as "do nothing".
    pub fn maybe_start(config: &NannyConfig, session_token: &str) -> Option<Self> {
        if config.runtime.mode != Mode::Managed {
            return None;
        }
        let managed = config.managed.as_ref()?;
        // The version lives in the configured endpoint (e.g. ".../v1"); the runtime
        // only appends the resource, so it stays version-agnostic.
        let endpoint = format!("{}/ingest", managed.endpoint.trim_end_matches('/'));
        let api_key = managed.api_key.clone();
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

/// Background loop: batch lines and POST them; flush on cap, on idle tick, and
/// once more when the channel closes.
fn worker(rx: Receiver<String>, client: reqwest::blocking::Client, endpoint: String, api_key: String, session: String) {
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
            eprintln!("nanny: managed — failed to forward events to the cloud (continuing locally)");
            *warned = true;
        }
        batch.clear();
        *bytes = 0;
    };

    loop {
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(line) => {
                // Flush first if adding this line would exceed a cap.
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn parse_config(toml_str: &str) -> NannyConfig {
        toml::from_str(toml_str).expect("test config must parse")
    }

    fn local_config() -> NannyConfig {
        parse_config(
            "[runtime]\nmode = \"local\"\n\n[start]\ncmd = \"true\"\n\n\
             [limits]\nsteps = 10\ntokens = 100\ntimeout = 1000\n\n[tools]\nallowed = []\n",
        )
    }

    fn managed_config(base: &str) -> NannyConfig {
        // Endpoint carries the version, mirroring real config (".../v1").
        parse_config(&format!(
            "[runtime]\nmode = \"managed\"\n\n[start]\ncmd = \"true\"\n\n\
             [limits]\nsteps = 10\ntokens = 100\ntimeout = 1000\n\n[tools]\nallowed = []\n\n\
             [managed]\nendpoint = \"{base}/v1\"\napi_key = \"nny_test\"\n",
        ))
    }

    /// One-shot HTTP server: captures the first request (headers + body), returns
    /// 200, and sends the raw request back over a channel.
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
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            }
        });
        (port, rx)
    }

    #[test]
    fn no_op_in_local_mode() {
        assert!(ManagedSender::maybe_start(&local_config(), "sess").is_none());
    }

    #[test]
    fn forwards_batched_ndjson_with_headers() {
        let (port, rx) = mock_ingest_server();
        let config = managed_config(&format!("http://127.0.0.1:{port}"));
        let sender = ManagedSender::maybe_start(&config, "session-abc").expect("managed → sender");
        sender.enqueue(r#"{"event":"ExecutionStarted"}"#.to_string());
        sender.enqueue(r#"{"event":"ExecutionStopped"}"#.to_string());
        sender.flush_and_join();

        let req = rx.recv_timeout(Duration::from_secs(5)).expect("server received a request");
        let lower = req.to_ascii_lowercase();
        assert!(lower.contains("post /v1/ingest"), "wrong path/method:\n{req}");
        assert!(lower.contains("x-nanny-key: nny_test"), "missing X-Nanny-Key:\n{req}");
        assert!(lower.contains("x-nanny-session: session-abc"), "missing X-Nanny-Session:\n{req}");
        // Both events arrive in one NDJSON batch.
        assert!(req.contains(r#"{"event":"ExecutionStarted"}"#));
        assert!(req.contains(r#"{"event":"ExecutionStopped"}"#));
    }

    #[test]
    fn unreachable_endpoint_never_panics_or_hangs() {
        // Nothing listening on this port → send fails; must not panic or block.
        let config = managed_config("http://127.0.0.1:1");
        let sender = ManagedSender::maybe_start(&config, "sess").expect("managed → sender");
        sender.enqueue(r#"{"event":"StepCompleted"}"#.to_string());
        sender.flush_and_join(); // returns (bounded by connect timeout), swallows the error
    }
}
