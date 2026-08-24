// Day 11 — End-to-end integration tests for the nanny Rust SDK.
//
// Exercises the __private runtime functions that #[nanny::tool],
// #[nanny::rule], and #[nanny::agent] generate at their call sites.
//
// Strategy: spin up a real Bridge in-process, inject transport env vars, and
// call the same __private functions the macros generate.  Each test creates
// its own Bridge so state never leaks between runs.
//
// The test tool name "search_web" is intentional: it is listed in
// `allowed_tools` but is NOT registered in the ToolRegistry, so the bridge
// takes the user-defined-tool path (charge cost, return "allowed") without
// making any real network calls.

use std::collections::HashMap;
use std::sync::Mutex;

use nanny::__private::{
    agent_enter, agent_exit, call_tool, evaluate_local_rules, is_active, ToolVerdict,
};
use nanny_bridge::{BridgeAddress, BridgeComponents, Bridge};

// ── Serialise env-var tests ───────────────────────────────────────────────────
//
// NANNY_BRIDGE_SOCKET / NANNY_BRIDGE_PORT / NANNY_SESSION_TOKEN are
// process-global.  Tests that set them must not run in parallel.
// Use `.unwrap_or_else(|e| e.into_inner())` so a poisoned mutex (from a
// prior panicking test) doesn't block the rest of the suite.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ── Helpers ───────────────────────────────────────────────────────────────────

fn start_bridge(allowed: &[&str]) -> Bridge {
    let components = BridgeComponents {
        registry:          nanny_runtime::default_registry(),
        allowed_tools:     allowed.iter().map(|s| s.to_string()).collect(),
        per_tool_max_calls: HashMap::new(),
        tool_labels: Default::default(),
    };
    Bridge::start(components).expect("bridge must start in tests")
}

fn inject_env(bridge: &Bridge) {
    #[allow(unused_variables)]
    let token = &bridge.session_token;
    unsafe {
        #[cfg(unix)]
        if let BridgeAddress::Unix(path) = &bridge.address {
            std::env::set_var("NANNY_BRIDGE_SOCKET", path);
        }
        #[cfg(not(unix))]
        if let BridgeAddress::Tcp(port) = &bridge.address {
            std::env::set_var("NANNY_BRIDGE_PORT", port.to_string());
        }
        std::env::set_var("NANNY_SESSION_TOKEN", &bridge.session_token);
    }
}

fn clear_env() {
    unsafe {
        std::env::remove_var("NANNY_BRIDGE_SOCKET");
        std::env::remove_var("NANNY_BRIDGE_PORT");
        std::env::remove_var("NANNY_SESSION_TOKEN");
    }
}

// ── Passthrough mode ──────────────────────────────────────────────────────────

/// Without transport env vars, `is_active()` returns false.
/// This is the passthrough gate — macros call it first; if false they invoke
/// the original function body directly without touching the bridge at all.
#[test]
fn passthrough_inactive_without_env_vars() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    assert!(!is_active(), "is_active must be false when no transport vars are set");
}

/// Once transport env vars are injected, `is_active()` returns true.
#[test]
fn bridge_active_when_env_vars_present() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"]);
    inject_env(&bridge);
    let active = is_active();
    clear_env();
    assert!(active, "is_active must be true when transport vars are set");
}

// ── call_tool ─────────────────────────────────────────────────────────────────

/// A tool in the allowed list → `Run`.
/// The generated macro wrapper calls the original function body on `Run`.
#[test]
fn call_tool_allowed_returns_run() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"]);
    inject_env(&bridge);

    let verdict = call_tool("search_web", 10);

    clear_env();
    assert!(
        matches!(verdict, ToolVerdict::Run),
        "allowed tool must return Run"
    );
}

/// A tool not on the allowed list → `Stop` with a denial reason.
/// The generated macro wrapper panics with `nanny: stopped — ToolDenied: ...`.
#[test]
fn call_tool_not_in_allowlist_returns_stop() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // "send_email" is not in the allowed list.
    let bridge = start_bridge(&["search_web"]);
    inject_env(&bridge);

    let verdict = call_tool("send_email", 0);

    clear_env();
    assert!(
        matches!(&verdict, ToolVerdict::Stop(msg) if
            msg.contains("send_email") || msg.contains("Denied")),
        "tool not in allowlist must return Stop; got: {verdict:?}"
    );
}

// ── evaluate_local_rules ──────────────────────────────────────────────────────

/// No `#[nanny::rule]` attributes exist in this test binary → always allows.
/// `evaluate_local_rules` is called by every `#[nanny::tool]` wrapper before
/// contacting the bridge; zero rules means zero denials.
///
/// Must hold ENV_LOCK: `evaluate_local_rules` calls `fetch_bridge_status`,
/// which calls `is_active()` (reads env vars) and — if active — makes an HTTP
/// request to the bridge. If this test runs concurrently with a test that has
/// set `NANNY_BRIDGE_SOCKET`, `fetch_bridge_status` may fail while `is_active`
/// returns true, causing `evaluate_local_rules` to call `std::process::exit(1)`.
#[test]
fn evaluate_local_rules_no_rules_registered_allows_all() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env(); // ensure bridge env vars are absent — passthrough mode expected
    assert!(
        evaluate_local_rules("any_tool", ::std::collections::HashMap::new()).is_none(),
        "no registered rules must produce None (allow all)"
    );
}

// ── agent enter / exit ────────────────────────────────────────────────────────

/// `agent_enter` followed by `agent_exit` completes without panic.
/// Mirrors the RAII guard that `#[nanny::agent("researcher")]` generates:
///   agent_enter on function entry, agent_exit in the guard's Drop.
#[test]
fn agent_enter_exit_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"]);
    inject_env(&bridge);

    agent_enter("researcher");
    agent_exit();

    clear_env();
    // If we reach here the round-trip succeeded.
}
