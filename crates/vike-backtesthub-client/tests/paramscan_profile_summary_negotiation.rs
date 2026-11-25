//! A profile's SUMMARIZED parameter search (`FEATURE_PARAMSCAN_PROFILE_SUMMARY`, decision 0078's amendment of
//! 2026-11-19), driven against the fake peer: the client's ONE fallback
//! (`BacktesthubClient::run_paramscan_profile_answer`) asks an older backtesthub for the whole report, asks a newer
//! one for the summary and reads every page of its rows, and a page read is refused in words, nothing sent, by an
//! older one.
//!
//! Loopback only, no store, no server crate.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::client::ParamscanProfileAnswer;
use vike_backtesthub_client::proto::{
    FEATURE_PARAMSCAN_PROFILE_SUMMARY, FEATURE_SEARCH_METHOD, Request, Response,
};
use vike_backtesthub_client::summary::{
    ParamscanProfileSummaryWire, ParamscanRowSummaryWire, ParamscanRowsPageWire,
};
use vike_backtesthub_client::test_support::spawn_fake_peer;
use vike_model::params::ParamValue;

const PROFILE: &str = "[paramscan]\nfast = [1, 2, 3, 4, 5]\n";

/// A backtesthub from before the summarized search: it serves the whole one.
fn an_older_backtesthub() -> Vec<String> {
    ["backtest", "run_paramscan_profile", FEATURE_SEARCH_METHOD]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn a_newer_backtesthub() -> Vec<String> {
    let mut features = an_older_backtesthub();
    features.push(FEATURE_PARAMSCAN_PROFILE_SUMMARY.to_string());
    features
}

/// Point `i` of the planted search: its override, and an error that names it.
fn row(i: i64) -> ParamscanRowSummaryWire {
    ParamscanRowSummaryWire {
        overrides: vec![("fast".to_string(), ParamValue::Int(i))],
        report: None,
        error: Some(format!("point {i}")),
        score: None,
    }
}

/// The whole report an older backtesthub answers.
const WHOLE: &str = "{\"rows\":[],\"rank_by\":\"sharpe\",\"from\":\"the whole verb\"}";

/// An older backtesthub is sent the plain verb, and the client hands back its whole report's very text.
#[test]
fn an_older_backtesthub_is_asked_for_the_whole_report() {
    let (addr, rx, handle) = spawn_fake_peer(an_older_backtesthub(), |request| match request {
        Request::RunParamscanProfile { .. } => Some(Response::ParamscanReport(WHOLE.to_string())),
        _ => None,
    });
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let answer =
        client.run_paramscan_profile_answer(PROFILE, None, None, false).expect("the search");
    let refused = client.read_paramscan_rows("a-1", 1, 2).expect_err("no rows to page");
    drop(client);
    assert!(matches!(&answer, ParamscanProfileAnswer::Whole(json) if json == WHOLE), "{answer:?}");
    assert!(
        refused.contains(FEATURE_PARAMSCAN_PROFILE_SUMMARY) && refused.contains("nothing was sent"),
        "{refused}"
    );
    let sent = rx.recv().expect("what arrived");
    assert_eq!(sent.len(), 1, "one request, the plain verb; the page read sent nothing: {sent:?}");
    assert!(matches!(sent[0], Request::RunParamscanProfile { .. }), "{sent:?}");
    handle.join().expect("fake server thread joins cleanly");
}

/// A newer backtesthub is asked for the summary, and every page after the first is read in order, one frame a
/// page, into the search's whole row list; `whole` still asks it for the whole report, and a data plan (the one
/// `Report` a summarized search answers) is handed back as the whole answer it is.
#[test]
fn a_newer_backtesthub_answers_the_summary_and_every_page_is_read() {
    let rows: Vec<ParamscanRowSummaryWire> = (1..=5).map(row).collect();
    let held = rows.clone();
    let (addr, rx, handle) = spawn_fake_peer(a_newer_backtesthub(), move |request| match request {
        Request::Summarized(inner) => match &**inner {
            Request::RunParamscanProfile { profile_toml, .. } if profile_toml == "plan" => {
                Some(Response::Report("{\"plan\":true}".to_string()))
            }
            Request::RunParamscanProfile { .. } => {
                let first = ParamscanRowsPageWire::of(&held, 0, 2);
                Some(Response::ParamscanProfileSummary(Box::new(ParamscanProfileSummaryWire {
                    run: "a-1".to_string(),
                    saved_run: "1763424000-a-0".to_string(),
                    total: first.total,
                    page_size: first.page_size,
                    rows: first.rows,
                    rank_by: "sharpe".to_string(),
                    summary: None,
                    account_currency: None,
                    strategy_file: String::new(),
                    strategy_sha256: String::new(),
                    strategy_sdk: None,
                    data_warnings: Vec::new(),
                    strategy_failures: Default::default(),
                    answered_from_saved_run: String::new(),
                })))
            }
            _ => None,
        },
        Request::ReadParamscanRows { run, page, page_size } if run == "a-1" => {
            Some(Response::ParamscanRows(ParamscanRowsPageWire::of(&held, *page, *page_size)))
        }
        Request::RunParamscanProfile { .. } => Some(Response::ParamscanReport(WHOLE.to_string())),
        _ => None,
    });
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let summary = client.run_paramscan_profile_answer(PROFILE, None, None, false).expect("summary");
    let whole = client.run_paramscan_profile_answer(PROFILE, None, None, true).expect("whole");
    let plan = client.run_paramscan_profile_answer("plan", None, None, false).expect("a plan");
    drop(client);
    let ParamscanProfileAnswer::Summary(summary) = summary else {
        panic!("a newer backtesthub answers the summary: {summary:?}");
    };
    assert_eq!(summary.rows, rows, "every row, in rank order, across three pages");
    assert_eq!((summary.total, summary.saved_run.as_str()), (5, "1763424000-a-0"));
    assert!(matches!(&whole, ParamscanProfileAnswer::Whole(json) if json == WHOLE), "{whole:?}");
    assert!(
        matches!(&plan, ParamscanProfileAnswer::Whole(json) if json.contains("plan")),
        "{plan:?}"
    );
    let sent = rx.recv().expect("what arrived");
    let pages: Vec<usize> = sent
        .iter()
        .filter_map(|request| match request {
            Request::ReadParamscanRows { page, page_size, .. } => {
                assert_eq!(*page_size, 2, "each page at the size the first was cut at");
                Some(*page)
            }
            _ => None,
        })
        .collect();
    assert_eq!(pages, [1, 2], "the two pages after the first, one frame each: {sent:?}");
    assert!(
        matches!(&sent[0], Request::Summarized(inner) if matches!(**inner, Request::RunParamscanProfile { .. }))
    );
    assert!(matches!(sent[3], Request::RunParamscanProfile { .. }), "`whole` sends the plain verb");
    handle.join().expect("fake server thread joins cleanly");
}

/// A page the daemon no longer holds (eight newer summarized runs pushed the search out of its memory): the client's
/// error still names the saved run that keeps every point, so the finished search is not lost with the page.
#[test]
fn a_lost_page_names_the_saved_run_that_keeps_the_search() {
    let (addr, _rx, handle) =
        spawn_fake_peer(a_newer_backtesthub(), move |request| match request {
            Request::Summarized(_) => {
                let first = ParamscanRowsPageWire::of(&(1..=5).map(row).collect::<Vec<_>>(), 0, 2);
                Some(Response::ParamscanProfileSummary(Box::new(ParamscanProfileSummaryWire {
                    run: "a-1".to_string(),
                    saved_run: "1763424000-a-0".to_string(),
                    total: first.total,
                    page_size: first.page_size,
                    rows: first.rows,
                    rank_by: "sharpe".to_string(),
                    summary: None,
                    account_currency: None,
                    strategy_file: String::new(),
                    strategy_sha256: String::new(),
                    strategy_sdk: None,
                    data_warnings: Vec::new(),
                    strategy_failures: Default::default(),
                    answered_from_saved_run: String::new(),
                })))
            }
            Request::ReadParamscanRows { .. } => {
                Some(Response::Error("the backtesthub no longer holds search a-1".to_string()))
            }
            _ => None,
        });
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let lost = client.run_paramscan_profile_answer(PROFILE, None, None, false).expect_err("lost");
    drop(client);
    assert!(lost.contains("no longer holds search a-1"), "the daemon's own words: {lost}");
    assert!(lost.contains("1763424000-a-0"), "and the saved run: {lost}");
    handle.join().expect("fake server thread joins cleanly");
}
