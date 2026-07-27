//! `nanny auth` — connect this machine to Nanny Cloud (`login`) or disconnect it
//! (`logout`).
//!
//! This is the ONLY thing that authenticates, and it authenticates to the
//! *optional* hosted cloud, never to the engine. Enforcement is always local and
//! never depends on any of this (manifesto: the engine works without the company).
//! Login just stores an ingest-only key locally; `nanny run` forwards only when
//! the project also sets `mode = "managed"`.
//!
//! Flow (RFC 8628 device authorization, via Better Auth):
//!   1. `POST /v1/auth/device/code`  → a user code + a URL to approve at.
//!   2. open the browser; poll `POST /v1/auth/device/token` until approved.
//!   3. the org was chosen in the browser and comes back in the token `scope`.
//!   4. `POST /v1/cli/key` (bearer) mints an ingest-only key → `{ api_key, plan }`.
//!   5. store `{ api_key, env }` locally; the ingest URL is derived from `env`.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use nanny_config::API_KEY_ENV;
use serde::Deserialize;
use std::time::{Duration, Instant};

use crate::cloud::CloudEnv;
use crate::credentials::Credentials;

/// Device-flow client identifier. The cloud does not gate on a registered client,
/// so this is just a label that shows up in the device record.
const CLIENT_ID: &str = "nanny-cli";

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Connect this machine to Nanny Cloud so `nanny run` can sync events.
    ///
    /// Opens the browser to approve, then stores an ingest-only key locally.
    /// Enforcement is unaffected and stays fully local.
    Login {
        /// Which Nanny Cloud to talk to. Defaults to prod for the browser flow;
        /// required with `--token`. dev/staging are for Nanny's own testing.
        /// Accepts `--env staging` or `--env=staging`.
        #[arg(long, value_enum)]
        env: Option<CloudEnv>,

        /// Machine login for CI / headless boxes (no browser). Reads the API key
        /// from NANNY_API_KEY or stdin and stores it. Requires `--env`.
        #[arg(long)]
        token: bool,
    },

    /// Disconnect this machine by deleting local credentials.
    ///
    /// Enforcement is unaffected. This is local only — the key still exists in
    /// the cloud until revoked from the dashboard.
    Logout,
}

pub fn cmd_auth(action: AuthCommand) -> Result<()> {
    match action {
        AuthCommand::Login { env, token } => login(env, token),
        AuthCommand::Logout => logout(),
    }
}

// ── login ───────────────────────────────────────────────────────────────────

fn login(env: Option<CloudEnv>, token: bool) -> Result<()> {
    if token {
        let env = env.context(
            "--token requires --env (e.g. `--env staging`) so the machine targets the right cloud",
        )?;
        return machine_login(env);
    }

    let env = env.unwrap_or_default(); // browser flow defaults to prod
    let client = http_client()?;
    let minted = device_login(&client, env.api_base(), |url| {
        // Best effort — the URL is already printed, so a headless box still works.
        let _ = open_browser(url);
    })?;

    Credentials { api_key: minted.api_key, env }.save()?;

    println!();
    println!("Logged in (plan {}).", minted.plan);
    println!("With `mode = \"managed\"`, `nanny run` will sync events to {}.", env.ingest_url());
    println!("Credentials saved to ~/.nanny/credentials.toml (this machine only).");
    Ok(())
}

/// Machine login: no browser. The API key is provided out of band (a secret in
/// `NANNY_API_KEY`, or piped on stdin) so CI and headless boxes can sync. `--env`
/// is required so the machine targets the cloud its key belongs to.
fn machine_login(env: CloudEnv) -> Result<()> {
    let api_key = read_machine_key()?;
    Credentials { api_key, env }.save()?;
    println!("Stored credentials for machine use ({env:?}).");
    println!("With `mode = \"managed\"`, `nanny run` will sync events to {}.", env.ingest_url());
    Ok(())
}

/// Read the API key for a machine login: `NANNY_API_KEY` first (a CI secret
/// store), else stdin (`echo "$KEY" | nanny auth login --token --env staging`).
/// Never a CLI argument, which would leak in process listings and shell history.
fn read_machine_key() -> Result<String> {
    if let Ok(key) = std::env::var(API_KEY_ENV) {
        let key = key.trim();
        if !key.is_empty() {
            return Ok(key.to_string());
        }
    }
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        bail!(
            "no API key found — set {API_KEY_ENV} or pipe the key on stdin. \
             `--token` is for CI and headless use, not an interactive terminal."
        );
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read the API key from stdin")?;
    let key = buf.trim().to_string();
    if key.is_empty() {
        bail!("no API key provided on stdin — set {API_KEY_ENV} or pipe the key");
    }
    Ok(key)
}

/// The full device-login exchange, minus persistence. Returns the minted key.
/// Split out from `login` so it can be driven against a mock cloud in tests
/// without touching the real `~/.nanny` or opening a real browser.
fn device_login(
    client: &reqwest::blocking::Client,
    api_base: &str,
    open: impl Fn(&str),
) -> Result<CliKeyResp> {
    // 1. Request a device + user code.
    let code: DeviceCodeResp = post_json(
        client,
        &format!("{api_base}/v1/auth/device/code"),
        &serde_json::json!({ "client_id": CLIENT_ID }),
        None,
    )?;

    let browser_url = code
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&code.verification_uri);
    println!();
    println!("To connect this machine to Nanny Cloud, open:");
    println!("    {}", code.verification_uri);
    println!("and enter the code:  {}", code.user_code);
    println!();
    println!("Opening your browser…");
    open(browser_url);

    // 2. Poll for approval. The server sets the interval; we trust it because the
    //    host can only ever be ours (no self-hosting, no arbitrary URL).
    let token_url = format!("{api_base}/v1/auth/device/token");
    let mut interval = Duration::from_secs(code.interval);
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let (access_token, scope) = loop {
        if Instant::now() >= deadline {
            bail!("login timed out — the code expired before it was approved. Run `nanny auth login` again.");
        }
        let tok: DeviceTokenResp = post_json(
            client,
            &token_url,
            &serde_json::json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "device_code": code.device_code,
                "client_id": CLIENT_ID,
            }),
            None,
        )?;
        if let Some(access_token) = tok.access_token {
            break (access_token, tok.scope.unwrap_or_default());
        }
        match tok.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += Duration::from_secs(5),
            Some("access_denied") => bail!("authorization was denied in the browser."),
            Some("expired_token") => bail!(
                "login timed out — the code expired before it was approved. Run `nanny auth login` again."
            ),
            other => bail!("unexpected response while waiting for approval: {other:?}"),
        }
        std::thread::sleep(interval);
    };

    // 3. The org was chosen in the browser and handed back in the token scope.
    //    We send it so the RIGHT org's key is minted; the response no longer
    //    echoes it back (the key identifies its own org).
    let org_id = parse_org_scope(&scope)
        .context("the cloud did not return an organization in the device scope")?;

    // 4. Exchange the session for an ingest-only key for that org.
    let key: CliKeyResp = post_json(
        client,
        &format!("{api_base}/v1/cli/key"),
        &serde_json::json!({ "org_id": org_id }),
        Some(&access_token),
    )?;
    Ok(key)
}

/// The consent page stashed the chosen org in the device-code scope as `org:<id>`;
/// Better Auth returns the scope verbatim on the token exchange. Scope is
/// space-separated, so scan for the `org:` entry.
fn parse_org_scope(scope: &str) -> Option<String> {
    scope
        .split_whitespace()
        .find_map(|s| s.strip_prefix("org:"))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

// ── logout ──────────────────────────────────────────────────────────────────

fn logout() -> Result<()> {
    if Credentials::delete()? {
        println!("Logged out on this machine.");
    } else {
        println!("Not logged in on this machine.");
        return Ok(());
    }
    println!("This only clears local credentials — the key still exists in the cloud.");
    println!("To revoke it across all machines, use the dashboard.");
    Ok(())
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build()
        .context("failed to build HTTP client")
}

/// POST a JSON body and deserialize the JSON response. reqwest's `json` feature
/// is not enabled in this workspace, so we serialize/parse with `serde_json`
/// directly. A non-2xx status is not treated as a transport error here: the
/// device-token endpoint returns pending/`error` bodies with a 4xx, and callers
/// branch on the parsed body.
fn post_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::blocking::Client,
    url: &str,
    body: &serde_json::Value,
    bearer: Option<&str>,
) -> Result<T> {
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(body)?);
    if let Some(tok) = bearer {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().with_context(|| format!("request to {url} failed"))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    serde_json::from_str::<T>(&text)
        .with_context(|| format!("unexpected response from {url} (HTTP {status}): {text}"))
}

/// Best-effort browser open. The verification URL is always printed first, so a
/// failure here (headless server, no opener) is not fatal.
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

// ── wire shapes (Better Auth 1.6.9 device flow + /v1/cli/key) ────────────────

#[derive(Deserialize)]
struct DeviceCodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct DeviceTokenResp {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// The `/v1/cli/key` response — trimmed to what the CLI uses (the key, and the
/// plan for the login message). The org and ingest URL are intentionally absent.
#[derive(Deserialize)]
struct CliKeyResp {
    api_key: String,
    plan: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A mock cloud serving the three device-login endpoints. The token endpoint
    /// returns `authorization_pending` once, then success, so the poll loop is
    /// exercised (device/code returns `interval: 0`, so the retry is instant).
    /// `/v1/cli/key` only returns a key when the bearer token matches, and its
    /// response is the trimmed `{ api_key, plan }`.
    fn mock_cloud() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let token_hits = Arc::new(AtomicUsize::new(0));

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };

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
                let lower = String::from_utf8_lossy(&buf).to_ascii_lowercase();

                let body: Option<String> = if lower.contains("post /v1/auth/device/code") {
                    Some(r#"{"device_code":"dc_123","user_code":"WXYZ-1234","verification_uri":"http://app.local/device","verification_uri_complete":"http://app.local/device?user_code=WXYZ-1234","expires_in":600,"interval":0}"#.to_string())
                } else if lower.contains("post /v1/auth/device/token") {
                    if token_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                        Some(r#"{"error":"authorization_pending"}"#.to_string())
                    } else {
                        Some(r#"{"access_token":"sess_tok_abc","token_type":"Bearer","expires_in":3600,"scope":"org:org_test other:x"}"#.to_string())
                    }
                } else if lower.contains("post /v1/cli/key") {
                    if lower.contains("authorization: bearer sess_tok_abc") {
                        Some(r#"{"api_key":"nny_ingest_key","plan":"FREE"}"#.to_string())
                    } else {
                        let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
                        continue;
                    }
                } else {
                    None
                };

                match body {
                    Some(body) => {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                    }
                    None => {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            }
        });
        base
    }

    #[test]
    fn device_login_polls_then_mints_a_key() {
        let base = mock_cloud();
        let client = http_client().unwrap();
        let opened = Arc::new(AtomicBool::new(false));
        let flag = opened.clone();

        let key = device_login(&client, &base, move |_url| flag.store(true, Ordering::SeqCst))
            .expect("login flow completes against the mock cloud");

        assert_eq!(key.api_key, "nny_ingest_key");
        assert_eq!(key.plan, "FREE");
        assert!(opened.load(Ordering::SeqCst), "the browser opener was invoked");
    }

    #[test]
    fn parse_org_scope_extracts_the_org() {
        assert_eq!(parse_org_scope("org:org_abc").as_deref(), Some("org_abc"));
        assert_eq!(parse_org_scope("read org:org_abc write").as_deref(), Some("org_abc"));
        assert_eq!(parse_org_scope("").as_deref(), None);
        assert_eq!(parse_org_scope("nope").as_deref(), None);
        assert_eq!(parse_org_scope("org:").as_deref(), None);
    }

    #[test]
    fn machine_key_reads_from_the_env_before_stdin() {
        // The machine path takes the key from NANNY_API_KEY (a CI secret), never
        // from an argument. With it set, stdin is not touched.
        // SAFETY: no other test in this module reads or writes NANNY_API_KEY.
        unsafe { std::env::set_var(API_KEY_ENV, "  nny_ci_key  ") };
        let key = read_machine_key().expect("env key resolves");
        unsafe { std::env::remove_var(API_KEY_ENV) };
        assert_eq!(key, "nny_ci_key", "the key is trimmed");
    }
}
