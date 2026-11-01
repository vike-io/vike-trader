use super::*;
use crate::run_profile::samples;

/// A run body of one valid ceiling, so a test adds the single row it is about.
fn body_with(key: &str, value: &str) -> StoredProfile {
    let mut stored = samples::stored(&[("risk.max_notional_per_order", "100.0")]);
    stored.settings.insert(key.to_string(), value.to_string());
    stored
}

/// **A stored row for a RETIRED key is refused BY NAME through the one reader** — the path the
/// daemon takes at boot and `vike-cli config activate` takes before it activates anything — and the
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
/// is the `bootstrap-run` line that carries every OTHER row.
#[test]
fn a_parse_refused_row_gets_the_command_that_rewrites_the_body_without_it() {
    let err = rows_to_run_profile(&body_with("mode", "\"live\"")).unwrap_err();
    assert!(
        err.contains("vike-cli config bootstrap-run sample --risk.max_notional_per_order 100.0"),
        "{err}"
    );
}

/// A stored scalar becomes the command-line argument `bootstrap-run` lowers back to it: a TOML
/// string loses its quotes, a number is verbatim, and a value that is not a plain word is
/// single-quoted for the shell. No run-profile key holds free text today, so this is the one place
/// the quoting branch is exercised.
#[test]
fn a_stored_scalar_becomes_a_shell_safe_flag_value() {
    assert_eq!(flag_value("\"active\""), "active");
    assert_eq!(flag_value("3.0"), "3.0");
    assert_eq!(flag_value("\"/srv/caps dir/it's\""), "'/srv/caps dir/it'\\''s'");
}
