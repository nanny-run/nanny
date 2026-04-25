//! nanny-macros — proc-macro attributes for per-function governance.
//!
//! These macros are re-exported as `nanny::tool`, `nanny::rule`, `nanny::agent`
//! via the `nanny` crate. Do not depend on this crate directly.
//!
//! # Passthrough mode
//!
//! All macros are no-ops when `NANNY_BRIDGE_SOCKET` / `NANNY_BRIDGE_PORT` are
//! absent. The original function runs exactly as written — no overhead.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, FnArg, ItemFn, LitInt, LitStr, Meta, Pat,
};

// ── #[nanny::tool(cost = N)] ─────────────────────────────────────────────────

/// Declare a function as a governed nanny tool.
///
/// # Usage
///
/// ```rust,ignore
/// use nanny::tool;
///
/// #[tool(cost = 10)]
/// fn search_web(query: &str) -> String {
///     // your implementation
/// }
/// ```
///
/// When active (running under `nanny run`):
/// 1. All registered `#[nanny::rule]` functions are evaluated first.
/// 2. The bridge is called via `POST /tool/call` — policy enforced, cost charged.
/// 3. If allowed, the original function body runs.
/// 4. If denied or stopped, the process panics with a `nanny: stopped —` message.
///
/// When inactive (no bridge env vars), the function runs without any overhead.
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let cost = parse_cost(attr.into());
    let input = parse_macro_input!(item as ItemFn);
    expand_tool(input, cost)
        .unwrap_or_else(|e| e.into_compile_error())
        .into()
}

fn parse_cost(attr: TokenStream2) -> u64 {
    // Accept `cost = N` or just `N`.
    // Fall back to 0 if unparseable.
    let meta: Result<Meta, _> = syn::parse2(attr.clone());
    if let Ok(Meta::NameValue(nv)) = meta {
        if nv.path.is_ident("cost") {
            if let syn::Expr::Lit(expr_lit) = &nv.value {
                if let syn::Lit::Int(lit) = &expr_lit.lit {
                    if let Ok(n) = lit.base10_parse::<u64>() {
                        return n;
                    }
                }
            }
        }
    }
    // Try bare integer
    if let Ok(lit) = syn::parse2::<LitInt>(attr) {
        if let Ok(n) = lit.base10_parse::<u64>() {
            return n;
        }
    }
    0
}

fn expand_tool(input: ItemFn, cost: u64) -> syn::Result<TokenStream2> {
    let vis      = &input.vis;
    let sig      = &input.sig;
    let attrs    = &input.attrs;
    let body     = &input.block;
    let generics = &sig.generics;
    let inputs   = &sig.inputs;
    let output   = &sig.output;
    let where_cl = &sig.generics.where_clause;
    let fn_name  = &sig.ident;
    let fn_str   = fn_name.to_string();

    // Check: method receivers are not supported.
    if inputs.iter().any(|a| matches!(a, FnArg::Receiver(_))) {
        return Err(syn::Error::new_spanned(
            fn_name,
            "#[nanny::tool] does not support methods with `self`. \
             Use a free function instead.",
        ));
    }

    // Collect argument names for forwarding and for last_tool_args serialization.
    let forward_args = forward_arg_names(inputs)?;

    // Build (key, value) pairs for last_tool_args: key is param name as &str,
    // value uses Display so String args don't get extra quotes.
    let arg_entries: Vec<TokenStream2> = forward_args.iter().map(|name| {
        let key = name.to_string();
        quote! {
            __nanny_tool_args.insert(#key.to_string(), format!("{}", &#name));
        }
    }).collect();

    Ok(quote! {
        #(#attrs)*
        #vis #sig {
            fn __nanny_impl #generics (#inputs) #output #where_cl {
                #body
            }

            if !::nanny::__private::is_active() {
                return __nanny_impl(#(#forward_args),*);
            }

            let mut __nanny_tool_args = ::std::collections::HashMap::<String, String>::new();
            #(#arg_entries)*

            if let Some(__rule_name) = ::nanny::__private::evaluate_local_rules(#fn_str, __nanny_tool_args) {
                ::nanny::__private::report_stop_rule(#fn_str, __rule_name);
                ::std::eprintln!("nanny: stopped — RuleDenied: {}", __rule_name);
                ::std::process::exit(1);
            }

            match ::nanny::__private::call_tool(#fn_str, #cost) {
                ::nanny::__private::ToolVerdict::Run  => __nanny_impl(#(#forward_args),*),
                ::nanny::__private::ToolVerdict::Stop(__reason) => {
                    ::std::eprintln!("nanny: stopped — {}", __reason);
                    ::std::process::exit(1);
                }
            }
        }
    })
}

// ── #[nanny::rule("name")] ───────────────────────────────────────────────────

/// Register a function as a named enforcement rule.
///
/// # Usage
///
/// ```rust,ignore
/// use nanny::{rule, PolicyContext};
///
/// #[rule("no_spiral")]
/// fn check_spiral(ctx: &PolicyContext) -> bool {
///     let h = &ctx.tool_call_history;
///     !(h.len() >= 3 && h[h.len()-3..].iter().all(|t| *t == h[h.len()-1]))
/// }
/// ```
///
/// The function is registered at link time via `inventory`. Every `#[nanny::tool]`
/// call evaluates all registered rules before contacting the bridge.
///
/// Returning `false` stops execution immediately with
/// `nanny: stopped — RuleDenied: <name>`.
///
/// When inactive (no bridge), the function still exists but is never called by nanny.
#[proc_macro_attribute]
pub fn rule(attr: TokenStream, item: TokenStream) -> TokenStream {
    let name_lit = parse_macro_input!(attr as LitStr);
    let input    = parse_macro_input!(item as ItemFn);
    expand_rule(input, name_lit)
        .unwrap_or_else(|e| e.into_compile_error())
        .into()
}

fn expand_rule(input: ItemFn, name_lit: LitStr) -> syn::Result<TokenStream2> {
    let vis   = &input.vis;
    let sig   = &input.sig;
    let attrs = &input.attrs;
    let body  = &input.block;
    let fn_name = &sig.ident;

    // The function must have signature `fn(ctx: &PolicyContext) -> bool`.
    // We trust the user got it right; the compiler will catch mismatches.

    Ok(quote! {
        #(#attrs)*
        #vis #sig {
            #body
        }

        ::nanny::__private::inventory::submit! {
            ::nanny::__private::Rule {
                name: #name_lit,
                func: #fn_name,
            }
        }
    })
}

// ── #[nanny::agent("name")] ─────────────────────────────────────────────────

/// Activate a named limits set for the duration of a function.
///
/// # Usage
///
/// ```rust,ignore
/// use nanny::agent;
///
/// #[agent("researcher")]
/// fn run_research(topic: &str) {
///     // [limits.researcher] is active here
/// }
/// ```
///
/// On function entry: `POST /agent/enter` switches to `[limits.researcher]`.
/// On function exit (including panics): `POST /agent/exit` reverts to global limits.
///
/// If the named set does not exist in `nanny.toml`, panics immediately on entry.
///
/// When inactive (no bridge), the function runs normally — no bridge calls.
#[proc_macro_attribute]
pub fn agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let name_lit = parse_macro_input!(attr as LitStr);
    let input    = parse_macro_input!(item as ItemFn);
    expand_agent(input, name_lit)
        .unwrap_or_else(|e| e.into_compile_error())
        .into()
}

fn expand_agent(input: ItemFn, name_lit: LitStr) -> syn::Result<TokenStream2> {
    let vis      = &input.vis;
    let sig      = &input.sig;
    let attrs    = &input.attrs;
    let body     = &input.block;
    let generics = &sig.generics;
    let inputs   = &sig.inputs;
    let output   = &sig.output;
    let where_cl = &sig.generics.where_clause;
    let is_async = sig.asyncness.is_some();

    // Check: method receivers are not supported.
    if inputs.iter().any(|a| matches!(a, FnArg::Receiver(_))) {
        return Err(syn::Error::new_spanned(
            &sig.ident,
            "#[nanny::agent] does not support methods with `self`. \
             Use a free function instead.",
        ));
    }

    let forward_args = forward_arg_names(inputs)?;

    // For async functions the inner impl and its call sites must also be async.
    let async_kw  = if is_async { quote!(async) } else { quote!() };
    let call_impl = if is_async {
        quote!(__nanny_impl(#(#forward_args),*).await)
    } else {
        quote!(__nanny_impl(#(#forward_args),*))
    };

    Ok(quote! {
        #(#attrs)*
        #vis #sig {
            #async_kw fn __nanny_impl #generics (#inputs) #output #where_cl {
                #body
            }

            if !::nanny::__private::is_active() {
                return #call_impl;
            }

            ::nanny::__private::agent_enter(#name_lit);

            // RAII guard: calls agent_exit on drop — fires on panic or normal return.
            struct __NannyAgentGuard;
            impl ::std::ops::Drop for __NannyAgentGuard {
                fn drop(&mut self) {
                    ::nanny::__private::agent_exit();
                }
            }
            let __guard = __NannyAgentGuard;

            let __result = #call_impl;
            drop(__guard);
            __result
        }
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract simple identifier patterns from a function's parameter list.
/// Returns them in the same order, ready to be used as forwarding arguments.
fn forward_arg_names(inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>)
    -> syn::Result<Vec<TokenStream2>>
{
    let mut names = Vec::new();
    for arg in inputs {
        match arg {
            FnArg::Receiver(_) => {
                // Handled by caller — should never reach here.
                unreachable!()
            }
            FnArg::Typed(pat_type) => {
                match pat_type.pat.as_ref() {
                    Pat::Ident(ident) => {
                        let name = &ident.ident;
                        names.push(quote! { #name });
                    }
                    Pat::Wild(_) => {
                        // `_: T` — generate a unique ident for forwarding.
                        // This case is unusual and the inner fn will have the
                        // same `_` pattern, so we can't forward by name.
                        // Emit a compile error to keep things clean.
                        return Err(syn::Error::new_spanned(
                            pat_type,
                            "#[nanny::tool] / #[nanny::agent]: wildcard parameter `_` \
                             is not supported. Give the parameter a name.",
                        ));
                    }
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "#[nanny::tool] / #[nanny::agent]: only simple identifier \
                             parameters are supported (e.g. `x: T`, not patterns).",
                        ));
                    }
                }
            }
        }
    }
    Ok(names)
}
