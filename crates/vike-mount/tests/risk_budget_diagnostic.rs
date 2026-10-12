//! The missing-risk-budget STARTUP DIAGNOSTIC, end to end. Only here can both halves meet:
//! `vike_mount::MountError` renders the message, `vike_core::RunProfile` decides whether the
//! profile it tells an operator to write is valid (vike-mount depends on vike-core since absorbing
//! `vike-run`, `docs/decisions/0098-vike-run-merges-into-vike-mount.md`; `build_node` is the edge
//! every binary hits this wall through).
//!
//! The refusal is CORRECT (#817: no universal safe default for an account-dependent cap); its
//! rendering must name where the budget lives (the ACTIVE `run` row of the settings database),
//! the command that writes it and the `[risk]` keys, not a `Debug` payload.
//!
//! THE PROPERTY PINNED — "the message IS the fix": `inline_command_is_a_valid_live_profile` lifts
//! the `vike-cli config bootstrap-run` command out of the message, lowers its `--<table>.<key>`
//! arguments the way that verb stores them (one row per dotted path, the TOML scalar verbatim, a
//! WORD quoted), parses the result as a real `RunProfile` and asserts it clears the gate that
//! produced the message. A new required `RunProfile` field fails it — the alarm, or the operator
//! runs a command that does not fix the error.

use std::collections::BTreeMap;

use vike_core::{Mode, RunProfile};
use vike_mount::MountError;

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
/// The workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD).
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The refusal exactly as a daemon start with no run profile produces it.
fn no_profile_error() -> MountError {
    MountError::MissingRiskBudget {
        venue: "binance".to_string(),
        missing: vec!["max_notional_per_order", "max_total_exposure"],
        profile_supplied: false,
    }
}

/// The `bootstrap-run` command the message prints, as its argument tokens after the verb: the
/// indented line naming the verb plus every `\`-continued line after it.
fn inline_command(msg: &str) -> Vec<String> {
    let mut lines = msg.lines().skip_while(|l| !l.trim_start().starts_with("vike-cli config"));
    let mut text = String::new();
    for line in lines.by_ref() {
        let continued = line.trim_end().ends_with('\\');
        text.push_str(line.trim_end().trim_end_matches('\\'));
        text.push(' ');
        if !continued {
            break;
        }
    }
    text.split_whitespace()
        .skip_while(|t| *t != "bootstrap-run")
        .skip(1)
        .map(str::to_string)
        .collect()
}

/// Lower `<name> --<path> <value> …` into the TOML document the stored rows render to: a word
/// value (the `mode`) is quoted, a number is verbatim — the shape `vike-cli config bootstrap-run`
/// writes and `vike_secrets::profile_store::render_run_toml` emits.
fn lowered(args: &[String]) -> String {
    let mut top = String::new();
    let mut tables: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut it = args.iter().skip(1);
    while let (Some(flag), Some(value)) = (it.next(), it.next()) {
        let path = flag.strip_prefix("--").unwrap_or_else(|| panic!("not a flag: {flag}"));
        let rendered =
            if value.parse::<f64>().is_ok() { value.clone() } else { format!("{value:?}") };
        match path.rsplit_once('.') {
            Some((table, key)) => {
                tables.entry(table.to_string()).or_default().push(format!("{key} = {rendered}"))
            }
            None => top.push_str(&format!("{path} = {rendered}\n")),
        }
    }
    for (table, keys) in tables {
        top.push_str(&format!("\n[{table}]\n{}\n", keys.join("\n")));
    }
    top
}

#[test]
fn inline_command_is_a_valid_live_profile() {
    let msg = format!("{}", no_profile_error());
    let args = inline_command(&msg);
    assert!(args.len() >= 3, "the message must embed a runnable command, got:\n{msg}");
    let toml = lowered(&args);

    let profile = RunProfile::from_toml_str(&toml).unwrap_or_else(|e| {
        panic!(
            "the command the diagnostic prints must store a profile that PARSES AND VALIDATES, \
                got {e:?} for:\n{toml}"
        )
    });

    // It must be a LIVE profile, or `risk_for_live_venue_mount` rejects it at the next startup.
    assert_eq!(profile.mode, Mode::Live, "the command must write a live-mode profile");
    profile
        .risk_for_live_venue_mount()
        .expect("the command must survive the live-venue-mount mode gate");
    // And it must actually close the gate that produced the message: BOTH account-dependent caps.
    assert!(
        profile.risk.max_notional_per_order.is_some(),
        "the command must set the cap the message says is missing"
    );
    assert!(
        profile.risk.max_total_exposure.is_some(),
        "the command must set the cap the message says is missing"
    );
}

#[test]
fn message_tells_an_operator_where_the_profile_lives() {
    let msg = format!("{}", no_profile_error());
    assert!(msg.contains("ACTIVE `run` row"), "names the row the daemon reads: {msg}");
    assert!(msg.contains("vike-cli config bootstrap-run"), "names the writer: {msg}");
    assert!(msg.contains("max_notional_per_order"), "names the missing key: {msg}");
    assert!(msg.contains("max_total_exposure"), "names the missing key: {msg}");
}

/// The commented reference the message names must EXIST and be a valid live profile. The path is
/// read OUT of the message, so a rename updating only one side fails instead of dangling.
///
/// ⚠ Read at run time and SKIPPED LOUDLY when absent: `docs/` is withheld from the public mirror
/// (`crates/vike-mount/CLAUDE.md`'s Mirror trap), so an `include_str!` would stop the mirror
/// compiling.
#[test]
fn the_reference_the_message_names_exists_and_validates() {
    let msg = format!("{}", no_profile_error());
    let rel = msg
        .split_whitespace()
        .find(|tok| tok.ends_with(".toml") && !tok.starts_with('<'))
        .unwrap_or_else(|| panic!("the message must name a shipped reference file:\n{msg}"));

    let path = workspace_root().join(rel);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "SKIPPED: `{rel}` is not in this tree ({e}) — the public mirror withholds docs/, \
                 so this check runs only in the private repository"
            );
            return;
        }
        Err(e) => panic!("reading `{rel}`: {e}"),
    };
    let profile = RunProfile::from_toml_str(&text).unwrap_or_else(|e| {
        panic!("the reference `{rel}` the diagnostic names must parse and validate: {e:?}")
    });
    assert_eq!(profile.mode, Mode::Live, "`{rel}` must be a live-mode profile");
    profile.risk_for_live_venue_mount().expect("reference must clear the live-mount mode gate");
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
