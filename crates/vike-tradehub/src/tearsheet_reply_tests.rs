use super::*;

/// A source whose env sweep is exactly `vars` and which resolves no project — the journal
/// resolution under test reads ONLY `env`, so the other two fields are inert here.
fn source(vars: &[(&str, &str)]) -> SettingsShowSource {
    SettingsShowSource {
        settings_dir: None,
        env: vars.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
        hot: None,
    }
}

fn error_text(r: Response) -> String {
    match r {
        Response::Error(m) => m,
        other => panic!("expected Response::Error, got {other:?}"),
    }
}

/// A server built through `serve(.., None)` cannot resolve its own journal, and says that
/// rather than reporting an empty tearsheet — the identity-less `StrategyStatus` rule.
#[test]
fn a_source_less_server_refuses_by_name() {
    let msg = error_text(tearsheet_reply(None, None, None));
    assert!(msg.contains("without a settings source"), "{msg}");
}

/// ⚠ The load-bearing refusal. A node journalling through `config.toml`'s `journal_dir` is
/// INDISTINGUISHABLE from one with journaling off through the env sweep this arm reads, so the
/// message must name that rung outright. Without this sentence the reply asserts something
/// false about a live node — that it is not journalling when it may well be.
#[test]
fn no_journal_in_the_environment_names_the_rung_this_build_cannot_see() {
    let msg = error_text(tearsheet_reply(Some(&source(&[])), None, None));
    assert!(msg.contains("VIKE_JOURNAL_DIR"), "the env knob is named: {msg}");
    assert!(msg.contains("journal_dir"), "…and the settings key it cannot see: {msg}");
    assert!(msg.contains("CANNOT SEE IT"), "…as a limitation, not as a verdict: {msg}");
}

/// A directory that is not there is an UNREADABLE journal, not an absent one: the operator
/// configured a path and the path is wrong, which is a different fact from "journaling is off"
/// and gets a different sentence (and names the path, so it can be fixed).
#[test]
fn a_missing_journal_directory_reports_the_read_failure_and_the_path() {
    let dir = std::env::temp_dir().join("vike-tradehub-tearsheet-absent-983471");
    let src = source(&[("VIKE_JOURNAL_DIR", &dir.display().to_string())]);
    let msg = error_text(tearsheet_reply(Some(&src), None, None));
    assert!(msg.contains("could not be read"), "{msg}");
    assert!(msg.contains("983471"), "…naming the directory it tried: {msg}");
}

/// An EMPTY journal directory is a journal with no fills in it — a zero-trade tearsheet, which
/// is an answer — and the reply is the pretty JSON spelling `tearsheet --json` prints, because
/// `remote_handle::tearsheet` hands this text to its caller verbatim.
#[test]
fn an_empty_journal_folds_to_a_zero_trade_tearsheet_in_pretty_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = source(&[("VIKE_JOURNAL_DIR", &dir.path().display().to_string())]);
    let Response::Tearsheet(json) = tearsheet_reply(Some(&src), None, None) else {
        panic!("an empty journal is an answer, not a refusal");
    };
    assert!(json.contains('\n'), "the payload is to_string_pretty, not compact: {json}");
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["n_trades"], serde_json::json!(0));
    // Seed `None` ⇒ the LOCAL door's default, so the remote and local reports of one journal
    // cannot scale `total_return`/`cagr`/`sharpe` off different equity bases.
    assert_eq!(doc["final_equity"], serde_json::json!(TEARSHEET_DEFAULT_SEED));
}

/// …and a caller WITH an opinion gets it: the wire's optionals are honoured, not clamped to the
/// defaults above.
#[test]
fn a_supplied_seed_reaches_the_fold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = source(&[("VIKE_JOURNAL_DIR", &dir.path().display().to_string())]);
    let Response::Tearsheet(json) = tearsheet_reply(Some(&src), Some(25_000.0), Some(365.0)) else {
        panic!("an empty journal is an answer, not a refusal");
    };
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["final_equity"], serde_json::json!(25_000.0));
}
