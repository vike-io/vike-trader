//! The missing-risk-budget STARTUP DIAGNOSTIC, end to end — the one place in the workspace that
//! can check it, because it needs BOTH halves: `vike_mount::MountError` (which renders the message)
//! and `vike_core::RunProfile` (which decides whether the profile that message tells an operator to
//! write is actually valid). vike-mount cannot depend on vike-core (down-only layering) and vike-core
//! knows nothing of mounts; `vike-run` already depends on both, and it owns `build_node` — the exact
//! edge every binary hits this wall through.
//!
//! WHAT WENT WRONG BEFORE: a GUI run with no run profile died on an `.expect()` whose payload was a
//! `Debug`-formatted `RiskBudget(MissingRiskBudget { venue: "binance", missing: [...] })`. It named
//! no environment variable, no file to create, and no `[risk]` table — and the only working live
//! profile in the tree lived inside `run_profile.rs`'s `#[cfg(test)]` `LIVE_TOML` constant, so the
//! only way past it was to read the source. The refusal itself is CORRECT (#817: no universal safe
//! default exists for an account-dependent cap) and is untouched here; only its rendering changed.
//!
//! THE PROPERTY THESE TESTS PIN — "the message IS the example". A diagnostic that prints a config
//! snippet is a promise, and the only way that promise survives a schema change is a test that
//! actually parses the snippet. So `inline_example_is_a_valid_live_profile` lifts the indented
//! block straight out of the rendered message, parses it as a real `RunProfile`, and asserts it
//! clears the very gate that produced the message. If `RunProfile` grows a required field, this
//! test fails — which is the alarm, since the operator would otherwise copy-paste the snippet and
//! be told to fix something the snippet was supposed to fix.

use vike_core::{Mode, RunProfile};
use vike_mount::MountError;

/// The workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the
/// `load_workspace_dotenv` / `settings_registry.rs` idiom.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The refusal exactly as a GUI start with no run profile produces it.
fn no_profile_error() -> MountError {
    MountError::MissingRiskBudget {
        venue: "binance".to_string(),
        missing: vec!["max_notional_per_order", "max_total_exposure"],
        profile_supplied: false,
    }
}

/// Everything in the rendered message indented by four spaces, dedented and rejoined — i.e. the
/// TOML snippet, and nothing else. Blank lines are dropped (TOML does not need them), so the result
/// is exactly what an operator would have if they selected the indented block and pasted it.
fn inline_toml(msg: &str) -> String {
    msg.lines().filter(|l| l.starts_with("    ")).map(|l| &l[4..]).collect::<Vec<_>>().join("\n")
}

#[test]
fn inline_example_is_a_valid_live_profile() {
    let msg = format!("{}", no_profile_error());
    let toml = inline_toml(&msg);
    assert!(!toml.trim().is_empty(), "the message must embed an example, got:\n{msg}");

    let profile = RunProfile::from_toml_str(&toml).unwrap_or_else(|e| {
        panic!("the example the diagnostic prints must PARSE AND VALIDATE, got {e:?} for:\n{toml}")
    });

    // It must be a LIVE profile, or `risk_for_live_venue_mount` rejects it at the next startup.
    assert_eq!(profile.mode, Mode::Live, "the example must be a live-mode profile");
    profile
        .risk_for_live_venue_mount()
        .expect("the example must survive the live-venue-mount mode gate");
    // And it must actually close the gate that produced the message: BOTH account-dependent caps.
    assert!(
        profile.risk.max_notional_per_order.is_some(),
        "the example must set the cap the message says is missing"
    );
    assert!(
        profile.risk.max_total_exposure.is_some(),
        "the example must set the cap the message says is missing"
    );
}

#[test]
fn message_tells_an_operator_where_to_put_that_profile() {
    let msg = format!("{}", no_profile_error());
    // The ACTIONABLE spellings, not the bare names — a stronger assertion, and it keeps a
    // standalone env-shaped literal out of the tree that `vike_ops::scan` sweeps.
    assert!(msg.contains("Set VIKE_RUN_PROFILE=<run.toml>"), "names the env resolver: {msg}");
    assert!(msg.contains("--profile <run.toml>"), "names the explicit-path flag: {msg}");
    assert!(msg.contains("max_notional_per_order"), "names the missing key: {msg}");
    assert!(msg.contains("max_total_exposure"), "names the missing key: {msg}");
}

/// The message points at a shipped template; that template must EXIST and be a valid live profile.
/// The path is read back OUT of the message rather than restated here, so a rename that updates
/// only one of the two sides fails instead of silently leaving a dangling reference.
#[test]
fn the_template_the_message_names_exists_and_validates() {
    let msg = format!("{}", no_profile_error());
    let rel = msg
        .split_whitespace()
        .find(|tok| tok.ends_with(".toml") && !tok.starts_with('<'))
        .unwrap_or_else(|| panic!("the message must name a shipped template file:\n{msg}"));

    let path = workspace_root().join(rel);
    let profile = RunProfile::from_path(&path).unwrap_or_else(|e| {
        panic!("the template `{rel}` the diagnostic names must parse and validate: {e:?}")
    });
    assert_eq!(profile.mode, Mode::Live, "`{rel}` must be a live-mode profile");
    profile.risk_for_live_venue_mount().expect("template must clear the live-mount mode gate");
    assert!(
        profile.risk.max_notional_per_order.is_some() && profile.risk.max_total_exposure.is_some(),
        "`{rel}` must set BOTH account-dependent caps — it is the answer to this very error"
    );
}

/// `vike_run::NodeError` is what every `build_node` caller actually holds; its `Display` must
/// forward the whole diagnostic rather than summarizing it away. (vike-app prints `{e}` on this
/// type and exits; vike-tradehub's live arm already did.)
#[test]
fn node_error_forwards_the_whole_diagnostic() {
    let node_err = vike_run::NodeError::RiskBudget(no_profile_error());
    let via_node = format!("{node_err}");
    assert_eq!(
        via_node,
        format!("{}", no_profile_error()),
        "NodeError must forward MountError's Display verbatim"
    );
}
