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
use nanny_core::agent::limits::Limits;

// ── Serialise env-var tests ───────────────────────────────────────────────────
//
// NANNY_BRIDGE_SOCKET / NANNY_BRIDGE_PORT / NANNY_SESSION_TOKEN are
// process-global.  Tests that set them must not run in parallel.
// Use `.unwrap_or_else(|e| e.into_inner())` so a poisoned mutex (from a
// prior panicking test) doesn't block the rest of the suite.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ── Helpers ───────────────────────────────────────────────────────────────────

fn start_bridge(allowed: &[&str], budget: u64) -> Bridge {
    start_bridge_named(allowed, budget, HashMap::new())
}

fn start_bridge_named(
    allowed: &[&str],
    budget: u64,
    named: HashMap<String, Limits>,
) -> Bridge {
    let components = BridgeComponents {
        registry:          nanny_runtime::default_registry(),
        limits:            Limits { max_steps: 100, max_cost_units: budget, timeout_ms: 30_000 },
        named_limits:      named,
        allowed_tools:     allowed.iter().map(|s| s.to_string()).collect(),
        per_tool_max_calls: HashMap::new(),
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
    let bridge = start_bridge(&["search_web"], 1000);
    inject_env(&bridge);
    let active = is_active();
    clear_env();
    assert!(active, "is_active must be true when transport vars are set");
}

// ── call_tool ─────────────────────────────────────────────────────────────────

/// A tool in the allowed list with budget available → `Run`.
/// The generated macro wrapper calls the original function body on `Run`.
#[test]
fn call_tool_allowed_returns_run() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"], 1000);
    inject_env(&bridge);

    let verdict = call_tool("search_web", 10);

    clear_env();
    assert!(
        matches!(verdict, ToolVerdict::Run),
        "allowed tool within budget must return Run"
    );
}

/// A tool not on the allowed list → `Stop` with a denial reason.
/// The generated macro wrapper panics with `nanny: stopped — ToolDenied: ...`.
#[test]
fn call_tool_not_in_allowlist_returns_stop() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // "send_email" is not in the allowed list.
    let bridge = start_bridge(&["search_web"], 1000);
    inject_env(&bridge);

    let verdict = call_tool("send_email", 0);

    clear_env();
    assert!(
        matches!(&verdict, ToolVerdict::Stop(msg) if
            msg.contains("send_email") || msg.contains("Denied")),
        "tool not in allowlist must return Stop; got: {verdict:?}"
    );
}

/// Budget exhaustion: each call charges cost; once the bridge marks execution
/// stopped the next call returns Stop.
///
/// With budget = 20 and cost = 10:
///   call 1 → 10 spent, budget not yet exhausted → Run
///   call 2 → 20 spent, bridge marks stopped after returning allowed → Run
///   call 3 → execution already stopped → Stop (410 Gone)
#[test]
fn call_tool_budget_exhaustion_returns_stop() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"], 20);
    inject_env(&bridge);

    let v1 = call_tool("search_web", 10); // 10 spent
    let v2 = call_tool("search_web", 10); // 20 spent — bridge marks stopped
    let v3 = call_tool("search_web", 10); // 410 Gone → Stop

    clear_env();
    assert!(matches!(v1, ToolVerdict::Run), "first call must be Run");
    assert!(matches!(v2, ToolVerdict::Run), "second call (exhausting) must still return Run");
    assert!(
        matches!(&v3, ToolVerdict::Stop(_)),
        "call after budget exhaustion must return Stop; got: {v3:?}"
    );
}

// ── evaluate_local_rules ──────────────────────────────────────────────────────

/// No `#[nanny::rule]` attributes exist in this test binary → always allows.
/// `evaluate_local_rules` is called by every `#[nanny::tool]` wrapper before
/// contacting the bridge; zero rules means zero denials.
#[test]
fn evaluate_local_rules_no_rules_registered_allows_all() {
    // No env vars needed — this is a pure local check over the inventory.
    assert!(
        evaluate_local_rules("any_tool").is_none(),
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
    let mut named = HashMap::new();
    named.insert(
        "researcher".to_string(),
        Limits { max_steps: 200, max_cost_units: 5000, timeout_ms: 120_000 },
    );
    let bridge = start_bridge_named(&["search_web"], 1000, named);
    inject_env(&bridge);

    agent_enter("researcher");
    agent_exit();

    clear_env();
    // If we reach here the round-trip succeeded.
}

/// While inside `agent_enter("researcher")`, the bridge uses the named limits.
/// After `agent_exit` the bridge reverts to global limits.
///
/// Proof: global budget is tiny (5 units); named budget is large (5000).
/// A 100-unit call succeeds under researcher limits but would fail globally.
#[test]
fn agent_named_limits_govern_tool_calls() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut named = HashMap::new();
    named.insert(
        "researcher".to_string(),
        Limits { max_steps: 200, max_cost_units: 5000, timeout_ms: 120_000 },
    );
    // Global budget = 5; researcher budget = 5000.
    let bridge = start_bridge_named(&["search_web"], 5, named);
    inject_env(&bridge);

    agent_enter("researcher");
    // 100-unit call: exceeds global budget but within researcher budget.
    let verdict = call_tool("search_web", 100);
    agent_exit();

    clear_env();
    assert!(
        matches!(verdict, ToolVerdict::Run),
        "tool call under named limits must return Run; got: {verdict:?}"
    );
}

/// `agent_enter` with a name that does not exist in the config panics
/// immediately — no silent fallback to global limits.
#[test]
fn agent_enter_unknown_set_panics() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"], 1000); // no named sets
    inject_env(&bridge);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        agent_enter("nonexistent_set");
    }));

    clear_env();

    assert!(result.is_err(), "agent_enter with unknown limits set must panic");
    let payload = result.unwrap_err();
    let msg = payload
        .downcast_ref::<String>()
        .map(|s| s.as_str())
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic>");
    assert!(
        msg.contains("not found in nanny.toml"),
        "panic message must mention 'not found in nanny.toml'; got: {msg:?}"
    );
}
