//! The missing-risk-budget STARTUP DIAGNOSTIC, end to end. Only here can both halves meet:
//! `vike_mount::MountError` renders the message, `vike_core::RunProfile` decides whether the
//! profile it tells an operator to write is valid (vike-mount depends on vike-core since absorbing
//! `vike-run`, `docs/decisions/0098-vike-run-merges-into-vike-mount.md`; `build_node` is the edge
//! every binary hits this wall through).
//!
//! The refusal is CORRECT (#817: no universal safe default for an account-dependent cap); its
//! rendering must name the env var, the flag and the `[risk]` table, not a `Debug` payload.
//!
//! THE PROPERTY PINNED — "the message IS the example": `inline_example_is_a_valid_live_profile`
//! lifts the indented block out of the message, parses it as a real `RunProfile` and asserts it
//! clears the gate that produced the message. A new required `RunProfile` field fails it — the
//! alarm, or the operator pastes a snippet that does not fix the error.

use vike_core::{Mode, RunProfile};
use vike_mount::MountError;

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
/// The workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD).
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

/// The message's four-space-indented lines, dedented and rejoined (blank lines dropped): the TOML
/// snippet exactly as an operator would select and paste it.
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
    // The ACTIONABLE spellings, never the bare names: a standalone env-shaped literal would be
    // read by `vike_model::scan` as an env read.
    assert!(msg.contains("Set VIKE_RUN_PROFILE=<run.toml>"), "names the env resolver: {msg}");
    assert!(msg.contains("--profile <run.toml>"), "names the explicit-path flag: {msg}");
    assert!(msg.contains("max_notional_per_order"), "names the missing key: {msg}");
    assert!(msg.contains("max_total_exposure"), "names the missing key: {msg}");
}

/// The shipped template the message names must EXIST and be a valid live profile. The path is read
/// OUT of the message, so a rename updating only one side fails instead of dangling.
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

/// `vike_mount::NodeError` is what every `build_node` caller holds (vike-tradehub's live arm prints
/// `{e}` and exits); its `Display` must forward the whole diagnostic.
#[test]
fn node_error_forwards_the_whole_diagnostic() {
    let node_err = vike_mount::NodeError::RiskBudget(no_profile_error());
    let via_node = format!("{node_err}");
    assert_eq!(
        via_node,
        format!("{}", no_profile_error()),
        "NodeError must forward MountError's Display verbatim"
    );
}
