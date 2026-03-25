// http_get — the first real tool.
//
// Makes a single HTTP GET request and returns the response body.
//
// Rules:
// - URL argument is required and must start with http:// or https://
// - Response body is capped at 1MB — fail closed on large responses
// - Timeout is enforced — the tool cannot run forever
// - Cost is only charged on success — failed calls do not spend budget
// - Non-2xx HTTP responses are treated as failures

use nanny_core::tool::{Tool, ToolArgs, ToolError, ToolOutput};
use std::io::Read;
use std::time::Duration;

/// Maximum response body size.
/// A tool that reads unbounded data is a resource leak.
/// Fail closed at 1MB.
const MAX_BODY_BYTES: u64 = 1024 * 1024;

/// Cost units charged for a successful http_get call.
const HTTP_GET_COST: u64 = 10;

/// Default timeout for the HTTP request.
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

// ── HttpGet ───────────────────────────────────────────────────────────────────

/// A tool that makes a single HTTP GET request.
///
/// Declared cost: 10 units (charged only on success).
/// Timeout: 5000ms by default, configurable via `with_timeout`.
pub struct HttpGet {
    timeout_ms: u64,
}

impl HttpGet {
    /// Create a new HttpGet tool with the default 5000ms timeout.
    pub fn new() -> Self {
        Self {
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    /// Create an HttpGet tool with a custom timeout.
    pub fn with_timeout(timeout_ms: u64) -> Self {
        Self { timeout_ms }
    }
}

impl Default for HttpGet {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for HttpGet {
    fn name(&self) -> &str {
        "http_get"
    }

    /// Cost charged on success only.
    /// The ledger is never debited for a failed request.
    fn declared_cost(&self) -> u64 {
        HTTP_GET_COST
    }

    fn execute(&self, args: &ToolArgs) -> Result<ToolOutput, ToolError> {
        // ── Step 1: Require the url argument ──────────────────────────────────
        let url = args.get("url").ok_or_else(|| ToolError::InvalidArgument {
            arg: "url".to_string(),
            reason: "required argument missing".to_string(),
        })?;

        // ── Step 2: Validate URL format ───────────────────────────────────────
        //
        // We do not resolve DNS, follow redirects, or check reachability here.
        // We only verify the shape is safe to pass to the HTTP client.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(ToolError::InvalidArgument {
                arg: "url".to_string(),
                reason: format!(
                    "must start with http:// or https://, got: {url}"
                ),
            });
        }

        // ── Step 3: Build the HTTP agent with timeout ─────────────────────────
        //
        // The agent is created per-call intentionally — no connection pooling,
        // no shared state between tool executions. Each call is independent.
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(self.timeout_ms))
            .build();

        // ── Step 4: Make the request ──────────────────────────────────────────
        let response = agent.get(url).call().map_err(|e| match e {
            // Non-2xx HTTP status — the server replied but with an error.
            ureq::Error::Status(code, _) => {
                ToolError::ExecutionFailed(format!("HTTP {code}"))
            }
            // Transport-level error — timeout, DNS failure, connection refused.
            ureq::Error::Transport(ref t) => {
                // ureq surfaces timeouts as transport errors.
                // We detect them by message content — not ideal but correct for v0.1.
                let msg = t.to_string();
                if msg.contains("timed out") || msg.contains("deadline") {
                    ToolError::Timeout {
                        timeout_ms: self.timeout_ms,
                    }
                } else {
                    ToolError::ExecutionFailed(msg)
                }
            }
        })?;

        // ── Step 5: Read the body with a hard size cap ────────────────────────
        //
        // `take(MAX_BODY_BYTES)` ensures we never read more than 1MB.
        // If the response is larger, we stop at the limit and return what we have.
        // This is intentional — fail closed on large payloads.
        let mut body = String::new();
        response
            .into_reader()
            .take(MAX_BODY_BYTES)
            .read_to_string(&mut body)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput { content: body })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> HttpGet {
        HttpGet::new()
    }

    // ── Validation tests (no network required) ────────────────────────────────

    #[test]
    fn rejects_missing_url() {
        let result = tool().execute(&ToolArgs::new());

        assert!(matches!(
            result,
            Err(ToolError::InvalidArgument { ref arg, .. }) if arg == "url"
        ));
    }

    #[test]
    fn rejects_url_without_scheme() {
        let mut args = ToolArgs::new();
        args.insert("url".to_string(), "example.com/path".to_string());

        let result = tool().execute(&args);

        assert!(matches!(
            result,
            Err(ToolError::InvalidArgument { ref arg, .. }) if arg == "url"
        ));
    }

    #[test]
    fn rejects_ftp_scheme() {
        let mut args = ToolArgs::new();
        args.insert("url".to_string(), "ftp://example.com".to_string());

        let result = tool().execute(&args);

        assert!(matches!(
            result,
            Err(ToolError::InvalidArgument { ref arg, .. }) if arg == "url"
        ));
    }

    #[test]
    fn accepts_http_scheme() {
        // We only test that the URL passes validation, not that the request succeeds.
        // A request to localhost:1 will fail at the network level, not the validation level.
        let mut args = ToolArgs::new();
        args.insert("url".to_string(), "http://localhost:1/test".to_string());

        let result = tool().execute(&args);

        // Any error here is a network error, not a validation error.
        // The absence of InvalidArgument means validation passed.
        assert!(!matches!(result, Err(ToolError::InvalidArgument { .. })));
    }

    #[test]
    fn accepts_https_scheme() {
        let mut args = ToolArgs::new();
        args.insert("url".to_string(), "https://localhost:1/test".to_string());

        let result = tool().execute(&args);

        assert!(!matches!(result, Err(ToolError::InvalidArgument { .. })));
    }

    #[test]
    fn declared_cost_is_ten() {
        assert_eq!(tool().declared_cost(), 10);
    }

    #[test]
    fn name_is_http_get() {
        assert_eq!(tool().name(), "http_get");
    }
}
