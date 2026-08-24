// Certification tests for `#[nanny::rule]` — rules that actually fire.
//
// `macro_integration.rs` covers the null case (no rules registered → allow
// all). It cannot cover the positive case: rules register through
// `inventory`, which collects at link time across the whole test binary, so
// any rule declared there would apply to every test in that file. This file
// is a separate binary for exactly that reason — the rules below are global
// to it and to nothing else.
//
// Within this binary the same constraint still applies, so each rule is keyed
// to a distinct tool name and returns true (allow) for every other tool. That
// keeps the rules independent of one another while they all stay registered.
//
// Mirrors the Python matrix in `sdks/python/tests/test_rule.py` so the two
// SDKs are certified against the same behaviours.
//
// The test tool name "search_web" is intentional, for the same reason as in
// `macro_integration.rs`: it is listed in `allowed_tools` but is NOT in the
// ToolRegistry, so the bridge takes the user-defined-tool path without making
// any real network calls.

use std::collections::HashMap;
use std::sync::Mutex;

use nanny::__private::{call_tool, evaluate_local_rules};
use nanny_bridge::{Bridge, BridgeAddress, BridgeComponents};
use nanny_core::agent::limits::Limits;
use nanny_core::policy::PolicyContext;

// ── Serialise env-var tests ───────────────────────────────────────────────────
//
// Same reasoning as `macro_integration.rs`: NANNY_BRIDGE_SOCKET /
// NANNY_BRIDGE_PORT / NANNY_SESSION_TOKEN are process-global, so tests that
// set them must not run in parallel. `.unwrap_or_else(|e| e.into_inner())` so
// a poisoned mutex from a prior panicking test doesn't block the rest.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ── Registered rules ──────────────────────────────────────────────────────────
//
// Each governs exactly one tool name and allows everything else, so they can
// all stay registered without interfering.

/// Denies one specific tool outright. Proves a rule fires on the very first
/// call, with no prior history.
#[nanny::rule("deny_forbidden_tool")]
fn deny_forbidden_tool(ctx: &PolicyContext) -> bool {
    ctx.requested_tool.as_deref() != Some("forbidden_tool")
}

/// Denies on a call-site argument. Proves `last_tool_args` reaches the rule.
#[nanny::rule("deny_secret_in_args")]
fn deny_secret_in_args(ctx: &PolicyContext) -> bool {
    !ctx.last_tool_args.contains_key("api_key")
}

/// Denies a privileged tool once an untrusted read has happened. Proves
/// `tool_call_history` is populated from real bridge state, and is the shape
/// the taint rules in the seed corpus depend on.
#[nanny::rule("deny_after_untrusted_read")]
fn deny_after_untrusted_read(ctx: &PolicyContext) -> bool {
    if ctx.requested_tool.as_deref() != Some("privileged_tool") {
        return true;
    }
    !ctx.tool_call_history.iter().any(|t| t == "search_web")
}

/// Denies once a tool has already run twice. Proves `tool_call_counts`
/// reaches the rule.
#[nanny::rule("deny_repeat_calls")]
fn deny_repeat_calls(ctx: &PolicyContext) -> bool {
    if ctx.requested_tool.as_deref() != Some("counted_tool") {
        return true;
    }
    ctx.tool_call_counts.get("counted_tool").copied().unwrap_or(0) < 2
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn start_bridge(allowed: &[&str], budget: u64) -> Bridge {
    let components = BridgeComponents {
        registry:           nanny_runtime::default_registry(),
        limits:             Limits { max_steps: 100, max_tokens: budget, timeout_ms: 30_000 },
        named_limits:       HashMap::new(),
        allowed_tools:      allowed.iter().map(|s| s.to_string()).collect(),
        per_tool_max_calls: HashMap::new(),
    };
    Bridge::start(components).expect("bridge must start in tests")
}

fn inject_env(bridge: &Bridge) {
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

fn args(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ── Rules fire ────────────────────────────────────────────────────────────────

/// A rule denies on step one, against a bridge with no history at all.
/// This is the case `macro_integration.rs` cannot reach.
#[test]
fn rule_denies_on_first_call_with_empty_history() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["forbidden_tool"], 1000);
    inject_env(&bridge);

    let denied = evaluate_local_rules("forbidden_tool", HashMap::new());

    clear_env();
    assert_eq!(
        denied,
        Some("deny_forbidden_tool"),
        "a registered rule returning false must deny by name"
    );
}

/// A tool no registered rule governs is allowed, even with rules present.
/// Guards against a rule accidentally denying everything.
#[test]
fn rules_allow_a_tool_none_of_them_govern() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"], 1000);
    inject_env(&bridge);

    let denied = evaluate_local_rules("search_web", HashMap::new());

    clear_env();
    assert!(denied.is_none(), "ungoverned tool must be allowed; got {denied:?}");
}

// ── PolicyContext is populated ────────────────────────────────────────────────

/// `last_tool_args` reaches the rule, carrying the call site's arguments.
#[test]
fn rule_receives_last_tool_args() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web"], 1000);
    inject_env(&bridge);

    let denied = evaluate_local_rules("search_web", args(&[("api_key", "sk-live-1")]));

    clear_env();
    assert_eq!(
        denied,
        Some("deny_secret_in_args"),
        "rules must see the arguments of the pending call"
    );
}

/// `tool_call_history` is populated from real bridge state after a real tool
/// call. This is the mid-execution case, and the exact mechanism the taint
/// rules in the seed corpus rely on.
#[test]
fn rule_receives_populated_tool_call_history() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["search_web", "privileged_tool"], 1000);
    inject_env(&bridge);

    // Clean history first: the privileged tool is allowed.
    let before = evaluate_local_rules("privileged_tool", HashMap::new());

    // Now make the untrusted read actually happen through the bridge.
    call_tool("search_web", 0);
    let after = evaluate_local_rules("privileged_tool", HashMap::new());

    clear_env();
    assert!(before.is_none(), "privileged tool must be allowed before any untrusted read");
    assert_eq!(
        after,
        Some("deny_after_untrusted_read"),
        "rules must see history accumulated by real tool calls"
    );
}

/// `tool_call_counts` reaches the rule and reflects real calls.
#[test]
fn rule_receives_tool_call_counts() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bridge = start_bridge(&["counted_tool"], 1000);
    inject_env(&bridge);

    call_tool("counted_tool", 0);
    let after_one = evaluate_local_rules("counted_tool", HashMap::new());

    call_tool("counted_tool", 0);
    let after_two = evaluate_local_rules("counted_tool", HashMap::new());

    clear_env();
    assert!(after_one.is_none(), "one prior call is under the rule's threshold");
    assert_eq!(
        after_two,
        Some("deny_repeat_calls"),
        "rules must see per-tool call counts from bridge state"
    );
}

// ── Passthrough ───────────────────────────────────────────────────────────────

/// Without a bridge, rules still evaluate against zeroed counters rather than
/// being skipped. A rule that denies on `requested_tool` alone still denies,
/// which is what makes offline enforcement meaningful.
#[test]
fn rules_still_evaluate_in_passthrough_mode() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();

    let denied = evaluate_local_rules("forbidden_tool", HashMap::new());

    assert_eq!(
        denied,
        Some("deny_forbidden_tool"),
        "rules must still run with no bridge present"
    );
}
