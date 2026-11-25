//! `store rename` over a REAL scratch store, through the shipped binary: the plan, the confirmation,
//! the move itself, and the blind re-run the deploy's runbook makes.

use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::TickQuote;

use super::*;

/// `store rename --store <root> --venue d --symbol OLD --to NEW <rest>`.
fn rename_line<'a>(root: &'a str, rest: &[&'a str]) -> Vec<&'a str> {
    let mut args =
        vec!["store", "rename", "--store", root, "--venue", "d", "--symbol", "OLD", "--to", "NEW"];
    args.extend_from_slice(rest);
    args
}

/// The tool refuses to act without `--yes` on a pipe, rehearses under `--dry-run`, moves under
/// `--yes` (the rows read the new symbol), and a re-run matches nothing and exits 0 saying so.
#[test]
fn rename_plans_refuses_a_pipe_moves_under_yes_and_a_rerun_says_nothing_to_rename() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let root = dir.path().join("hist");
    let arg = root.to_str().expect("a UTF-8 path").to_string();
    let quote = |ts: i64| TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "OLD".into(),
    };
    {
        let store = HistoryStoreDataFusion::open(&root).expect("open the scratch store");
        store.append_quotes("d", "OLD", &[quote(1), quote(2)], Some("k1")).expect("seed");
    }

    let out = run(&rename_line(&arg, &[]));
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("--yes"), "{}", stderr_of(&out));
    let out = run(&rename_line(&arg, &["--dry-run"]));
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("MOVE quote d OLD -> NEW"), "{}", stdout_of(&out));
    assert!(stdout_of(&out).contains("nothing was renamed"), "{}", stdout_of(&out));

    let out = run(&rename_line(&arg, &["--yes"]));
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("1 of 1 series renamed"), "{}", stdout_of(&out));
    let store = HistoryStoreDataFusion::open(&root).expect("reopen");
    let got = store.scan_quotes("d", "NEW", TimestampRange::all()).expect("scan");
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|q| q.symbol == "NEW"), "{got:?}");
    assert!(store.scan_quotes("d", "OLD", TimestampRange::all()).expect("scan").is_empty());

    let out = run(&rename_line(&arg, &["--yes", "--json"]));
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("one JSON document");
    assert_eq!(doc["series"], serde_json::json!([]), "{doc}");
    let out = run(&rename_line(&arg, &["--yes"]));
    assert!(stdout_of(&out).contains("nothing to rename"), "{}", stdout_of(&out));
}
