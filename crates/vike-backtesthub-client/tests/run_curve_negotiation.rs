//! The WHOLE curve on request's negotiation (`FEATURE_RUN_CURVE`, decision 0078's amendment of
//! 2026-11-18), driven against the fake peer: a backtesthub older than it is refused in words before a
//! frame is written, and one that advertises it is read page by page into the very curve it holds.
//!
//! Loopback only, no store, no server crate.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{FEATURE_RUN_CURVE, FEATURE_RUN_SUMMARY, Request, Response};
use vike_backtesthub_client::summary::{CURVE_PAGE_SIZE_MAX, CurvePageWire};
use vike_backtesthub_client::test_support::spawn_fake_peer;

/// A daemon that summarizes Studio runs and predates the whole curve.
fn a_summary_only_backtesthub() -> Vec<String> {
    ["backtest", "run_slice", "run_sweep", "run_walkforward", FEATURE_RUN_SUMMARY]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// One verb of the whole curve, called on a connected client.
type Call = Box<dyn Fn(&mut BacktesthubClient) -> Result<(), String>>;

/// An older backtesthub: the summarized `RunBacktest`, a curve page and the whole curve are each refused
/// in words naming the capability and the cure, and nothing is sent.
#[test]
fn an_older_backtesthub_is_refused_in_words_without_sending() {
    let calls: Vec<Call> = vec![
        Box::new(|c| c.run_backtest_summary("name = \"x\"").map(|_| ())),
        Box::new(|c| c.read_curve("a-1", 0, 100).map(|_| ())),
        Box::new(|c| c.read_whole_curve("a-1").map(|_| ())),
    ];
    for call in calls {
        let (addr, rx, handle) = spawn_fake_peer(a_summary_only_backtesthub(), |_| None);
        let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
        let err = call(&mut client).expect_err("refused client-side");
        drop(client);
        assert!(
            err.contains(FEATURE_RUN_CURVE)
                && err.contains("nothing was sent")
                && err.contains("--whole"),
            "{err}"
        );
        assert!(rx.recv().expect("count").is_empty(), "the client sent NOTHING");
        handle.join().expect("fake server thread joins cleanly");
    }
}

/// A curve longer than one page reads back through the pages, each page one frame, equal to the curve
/// the daemon holds to the bit.
#[test]
fn the_whole_curve_reads_back_page_by_page_bit_for_bit() {
    let steps = CURVE_PAGE_SIZE_MAX + 7;
    let equity: Vec<f64> = (0..steps).map(|i| 1000.0 + (i as f64).sqrt() / 7.0).collect();
    let timestamp: Vec<i64> = (0..steps as i64).map(|i| 1_704_067_200_000 + i * 60_000).collect();
    let (held_equity, held_timestamp) = (equity.clone(), timestamp.clone());
    let mut features = a_summary_only_backtesthub();
    features.push(FEATURE_RUN_CURVE.to_string());
    let (addr, rx, handle) = spawn_fake_peer(features, move |request| match request {
        Request::ReadCurve { run, page, page_size } if run == "a-1" => Some(Response::CurvePage(
            CurvePageWire::of(&held_equity, &held_timestamp, *page, *page_size),
        )),
        _ => None,
    });
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let (back_equity, back_timestamp) = client.read_whole_curve("a-1").expect("the curve");
    drop(client);
    let bits = |curve: &[f64]| curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&back_equity), bits(&equity));
    assert_eq!(back_timestamp, timestamp);
    assert_eq!(rx.recv().expect("count").len(), 2, "two pages, two frames");
    handle.join().expect("fake server thread joins cleanly");
}
