//! The missing-risk-budget STARTUP DIAGNOSTIC, end to end. Only here can both halves meet:
//! `vike_mount::MountError` renders the message, `vike_config::RunProfile` decides whether the
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

use vike_config::RunProfile;
use vike_model::AccountTier;
use vike_mount::MountError;

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
/// value is quoted, a number is verbatim — the shape `vike-cli config bootstrap-run` writes and
/// `vike_secrets::profile_store::render_run_toml` emits.
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

    // A `demo`/`live` account must take it WHOLE: a profile that set the venue's grid would lose
    // those fields at every such mount (decision 0156 item 7: the refusal is the mount's now).
    for tier in [AccountTier::Demo, AccountTier::Live] {
        profile
            .apply_risk(vike_model::RiskLimits::new(), tier)
            .unwrap_or_else(|e| panic!("a {tier} account must take the command's profile: {e}"));
    }
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
