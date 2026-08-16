//! Which Nanny Cloud deployment the runtime forwards to.
//!
//! The backend host is the tool's business, not the user's: the API base URL is a
//! compile-time constant and can only ever be one of OUR OWN hosts. There is no
//! arbitrary-URL override and no cloud self-hosting. The "take my data elsewhere"
//! escape hatch is the open, local event stream (pipe it anywhere), which never
//! needs a repointable cloud URL — so trusting our host, and even the poll
//! interval it hands back, is safe.
//!
//! The environment is chosen by an explicit `--env` selector, never inferred from
//! the build profile (that would repeat the "NODE_ENV for staging" anti-pattern,
//! and staging's host runs as production anyway). A named selector can only
//! resolve to a compiled host, so there is no exfiltration surface.
//!
//! **`--env` exists for people building Nanny, not people building apps with it.**
//! It is hidden from `--help` and documented only in CONTRIBUTING.md. A customer
//! has exactly one cloud and never chooses: `Prod` is the default and is the only
//! value anything outside this repo will ever resolve to. `Dev` and `Staging`
//! point at our own local and QA deployments.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// A first-party Nanny Cloud environment. `--env` picks one; defaults to prod.
///
/// clap accepts both `--env staging` and `--env=staging`, and rejects any value
/// outside this set with the allowed list. Serializes lowercase (`dev`/`staging`/
/// `prod`) so it round-trips through `~/.nanny/credentials.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudEnv {
    /// Local development cloud (localhost). For Nanny's own end-to-end testing.
    Dev,
    /// Staging cloud (QA).
    Staging,
    /// Production cloud. The default for everyone.
    #[default]
    Prod,
}

impl CloudEnv {
    /// The compiled API base URL for this environment, without a trailing slash.
    /// This is the ONLY host the CLI will ever reach; it is never user-supplied.
    ///
    /// Note the app URL (where the browser approves) is a *different* host in
    /// staging/prod; the CLI never needs it — it arrives as `verification_uri`
    /// in the device-code response.
    pub fn api_base(self) -> &'static str {
        match self {
            CloudEnv::Dev => "http://localhost:3000",
            CloudEnv::Staging => "https://sandbox-api.nanny.run",
            CloudEnv::Prod => "https://api.nanny.run",
        }
    }

    /// Where runs and the governance server POST their NDJSON events. Derived
    /// from the compiled API base, so the CLI never stores or trusts a URL — it
    /// only ever reaches our own host for the env it logged into.
    pub fn ingest_url(self) -> String {
        format!("{}/v1/ingest", self.api_base())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_prod() {
        assert_eq!(CloudEnv::default(), CloudEnv::Prod);
    }

    #[test]
    fn api_bases_are_our_hosts_and_have_no_trailing_slash() {
        assert_eq!(CloudEnv::Dev.api_base(), "http://localhost:3000");
        assert_eq!(CloudEnv::Staging.api_base(), "https://sandbox-api.nanny.run");
        assert_eq!(CloudEnv::Prod.api_base(), "https://api.nanny.run");
        for e in [CloudEnv::Dev, CloudEnv::Staging, CloudEnv::Prod] {
            assert!(!e.api_base().ends_with('/'), "callers append /v1/... to the base");
        }
    }

    #[test]
    fn ingest_url_is_the_api_host_plus_route() {
        assert_eq!(CloudEnv::Prod.ingest_url(), "https://api.nanny.run/v1/ingest");
        assert_eq!(CloudEnv::Staging.ingest_url(), "https://sandbox-api.nanny.run/v1/ingest");
        assert_eq!(CloudEnv::Dev.ingest_url(), "http://localhost:3000/v1/ingest");
    }

    #[test]
    fn serializes_lowercase_for_the_credential_file() {
        assert_eq!(serde_json::to_string(&CloudEnv::Staging).unwrap(), "\"staging\"");
        let back: CloudEnv = serde_json::from_str("\"prod\"").unwrap();
        assert_eq!(back, CloudEnv::Prod);
    }

    #[test]
    fn parses_lowercase_names_and_rejects_others() {
        assert_eq!(CloudEnv::from_str("dev", true).unwrap(), CloudEnv::Dev);
        assert_eq!(CloudEnv::from_str("staging", true).unwrap(), CloudEnv::Staging);
        assert_eq!(CloudEnv::from_str("prod", true).unwrap(), CloudEnv::Prod);
        assert!(CloudEnv::from_str("production", true).is_err());
        assert!(CloudEnv::from_str("", true).is_err());
    }
}
