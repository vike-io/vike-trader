use super::settings::SettingsShowSource;
use super::tearsheet::{TEARSHEET_DEFAULT_SEED, tearsheet_reply};
use vike_tradehub_client::proto::Response;

/// A source carrying exactly the journal the daemon resolved (`None` = journaling off) and which
/// resolves no project — the reply reads ONLY `journal`, so the other fields are inert here.
fn source(journal: Option<&std::path::Path>) -> SettingsShowSource {
    SettingsShowSource {
        settings_dir: None,
        hot: None,
        journal: journal.map(|d| vike_core::JournalConfig::at(d.to_path_buf())),
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

/// A node running with no journal says so, and names both ways to turn one on: the active run
/// row's `[sinks.journal]` and, with no run profile in force, `config.journal_dir`.
#[test]
fn no_resolved_journal_names_both_ways_to_turn_one_on() {
    let msg = error_text(tearsheet_reply(Some(&source(None)), None, None));
    assert!(msg.contains("no command journal"), "{msg}");
    assert!(msg.contains("[sinks.journal]"), "the run row's table is named: {msg}");
    assert!(msg.contains("config.journal_dir"), "…and the settings key: {msg}");
}

/// A directory that is not there is an UNREADABLE journal, not an absent one: the operator
/// configured a path and the path is wrong, which is a different fact from "journaling is off"
/// and gets a different sentence (and names the path, so it can be fixed).
#[test]
fn a_missing_journal_directory_reports_the_read_failure_and_the_path() {
    let dir = std::env::temp_dir().join("vike-tradehub-tearsheet-absent-983471");
    let msg = error_text(tearsheet_reply(Some(&source(Some(&dir))), None, None));
    assert!(msg.contains("could not be read"), "{msg}");
    assert!(msg.contains("983471"), "…naming the directory it tried: {msg}");
}

/// An EMPTY journal directory is a journal with no fills in it — a zero-trade tearsheet, which
/// is an answer — and the reply is the pretty JSON spelling `tearsheet --json` prints, because
/// `remote_handle::tearsheet` hands this text to its caller verbatim.
#[test]
fn an_empty_journal_folds_to_a_zero_trade_tearsheet_in_pretty_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = source(Some(dir.path()));
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
    let src = source(Some(dir.path()));
    let Response::Tearsheet(json) = tearsheet_reply(Some(&src), Some(25_000.0), Some(365.0)) else {
        panic!("an empty journal is an answer, not a refusal");
    };
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["final_equity"], serde_json::json!(25_000.0));
}
