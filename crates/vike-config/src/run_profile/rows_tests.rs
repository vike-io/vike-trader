use super::*;
use crate::run_profile::samples;

/// A run body of one valid ceiling, so a test adds the single row it is about.
fn body_with(key: &str, value: &str) -> ProfileRecord {
    let mut stored = samples::stored(&[("risk.max_notional_per_order", "100.0")]);
    stored.settings.insert(key.to_string(), value.to_string());
    stored
}

/// **A stored row for a RETIRED key is refused BY NAME through the one reader** — the path the
/// daemon takes at boot and `vike-cli config profile activate` takes before it activates anything — and the
/// refusal carries the re-run that drops the key. The four keys are the ones no mount ever applied
/// (`Sinks::gui`, `Sinks::recorder`, `Sinks::raw_capture_dir`, `Guards::freshness_ms`); the stored
/// value is irrelevant, `false` included.
#[test]
fn a_stored_retired_key_is_refused_by_name_through_the_one_reader() {
    for (key, value) in [
        ("guards.freshness_ms", "5000"),
        ("sinks.gui", "true"),
        ("sinks.recorder", "false"),
        ("sinks.raw_capture_dir", "\"/srv/caps\""),
    ] {
        let err = rows_to_run_profile(&body_with(key, value))
            .expect_err("a retired key refuses the load");
        assert!(
            err.contains(&format!("`{key}` is no longer part of a run profile")),
            "{key}: {err}"
        );
        assert!(err.contains(&format!("WITHOUT `--{key}`")), "the fix names the flag: {err}");
    }
}

/// The parse-time refusal path still hands back the exact command: a stored `mode` row (a key the
/// type has not had since decision 0156) is the single row without which the body loads, so the fix
/// is the `profile create run` line that carries every OTHER row.
#[test]
fn a_parse_refused_row_gets_the_command_that_rewrites_the_body_without_it() {
    let err = rows_to_run_profile(&body_with("mode", "\"live\"")).unwrap_err();
    assert!(
        err.contains(
            "vike-cli config profile create run sample --risk.max_notional_per_order 100.0"
        ),
        "{err}"
    );
}

/// A stored scalar becomes the command-line argument `profile create run` lowers back to it: a TOML
/// string loses its quotes, a number is verbatim, and a value that is not a plain word is
/// single-quoted for the shell. No run-profile key holds free text today, so this is the one place
/// the quoting branch is exercised.
#[test]
fn a_stored_scalar_becomes_a_shell_safe_flag_value() {
    assert_eq!(flag_value("\"active\""), "active");
    assert_eq!(flag_value("3.0"), "3.0");
    assert_eq!(flag_value("\"/srv/caps dir/it's\""), "'/srv/caps dir/it'\\''s'");
}

/// **The fold line (decision 0297)**: a retired `run` body's run-part rows become the
/// `profile create run <daemon>` command that writes them into the daemon body, every other row left
/// out; and a DAEMON body's run part loads through the one reader with its `daemon.*` rows unread.
#[test]
fn a_run_body_folds_into_the_daemon_body_and_reads_back_the_same() {
    let mut run = samples::stored(&[
        ("name", "\"live-operator-budget\""),
        ("risk.max_notional_per_order", "100000.0"),
        ("risk.max_total_exposure", "1000000.0"),
        ("guards.initial_trading_state", "\"halted\""),
    ]);
    assert_eq!(
        fold_command(&run, "tradehub"),
        "vike-cli config profile create run tradehub --guards.initial_trading_state halted \
         --risk.max_notional_per_order 100000.0 --risk.max_total_exposure 1000000.0"
    );

    let before = rows_to_run_profile(&run).expect("the run body loads");
    run.settings.remove("name");
    run.row.kind = vike_secrets::profile_store::ProfileKind::Daemon;
    run.settings.insert("daemon.summary_ms".into(), "60000".into());
    let after = rows_to_run_profile(&run).expect("the daemon body's run part loads");
    assert_eq!(after.risk, before.risk);
    assert_eq!(after.guards, before.guards);
    assert_eq!(after.sinks, before.sinks);
}
