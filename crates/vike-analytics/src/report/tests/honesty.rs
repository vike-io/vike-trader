//! The honesty counters, the realism stamp and "not recorded" rather than zero.

use super::*;

/// ⚠ **A clean run's table does not move.** Everything added here is conditional on something
/// having happened (`is_noteworthy`) or on a stamp existing, which is what keeps every report
/// fixture and every operator's muscle memory intact.
#[test]
fn a_clean_run_s_table_gains_no_row() {
    let report = BacktestReport::from_result(Some("demo".into()), &sample_result(), 252.0);
    let table = report.to_string();
    assert!(!table.contains("honesty"), "a clean run printed an honesty block:\n{table}");
    assert!(!table.contains("realism"), "an unstamped run printed a realism row:\n{table}");
}

/// ⚠ **THE ITEM-3 DOOR, stated as a test.** `aggregate_denials` was always a free function and
/// the zero-trade gate was never inside it — the gate lived in
/// `ZeroTradeReport::analyze`, its only caller. So a run that traded 400 times with 3,000
/// orders refused by the margin gate had a reject ledger nothing ever read. Calling the same
/// function unconditionally is the whole fix.
#[test]
fn the_reject_ledger_reaches_a_run_that_traded() {
    let mut r = sample_result();
    r.dropped = vec![
        ("BTCUSDT".into(), "insufficient-margin".into(), 1.0, 1.0),
        ("BTCUSDT".into(), "insufficient-margin".into(), 2.0, 1.0),
        ("ETHUSDT".into(), "volume_cap".into(), 3.0, 1.0),
    ];
    r.intrabar_both_hit = 7;

    // The run traded, so the zero-trade analyzer declines it — which is exactly the state in
    // which the counters used to vanish.
    assert!(crate::zero_trade::ZeroTradeReport::analyze(&r).is_none());

    let report = BacktestReport::from_result(None, &r, 252.0);
    let h = report.honesty.as_ref().expect("a composed report always carries the counters");
    assert_eq!(h.intrabar_both_hit, 7);
    assert_eq!(h.denials, vec![("insufficient-margin".into(), 2), ("volume_cap".into(), 1)]);
    assert!(h.is_noteworthy());

    // ...and it is VISIBLE, which is the half that was missing.
    let table = report.to_string();
    assert!(table.contains("insufficient-margin"), "{table}");
    assert!(table.contains("intrabar_both_hit"), "{table}");
}

/// `warmup` is recorded and deliberately does not make the block print: essentially every
/// strategy declares one, so counting it would fire the block on every run and there would be
/// no signal left in it.
#[test]
fn a_warmup_alone_is_not_noteworthy() {
    let mut r = sample_result();
    r.warmup = 200;
    let report = BacktestReport::from_result(None, &r, 252.0);
    let h = report.honesty.as_ref().unwrap();
    assert_eq!(h.warmup, 200);
    assert!(!h.is_noteworthy());
    assert!(!report.to_string().contains("honesty"));
}

/// The realism harm, as a test: a frictionless run and a costed one must not print the same
/// table. Only the frictionless side gains a row.
#[test]
fn a_frictionless_run_says_so_and_a_costed_one_does_not() {
    let base = BacktestReport::from_result(None, &sample_result(), 252.0);

    let free = base.clone().with_realism(crate::realism::RealismStamp::new(
        [("engine.fee_rate".to_string(), "0".to_string())],
        Some("no fee_rate, no fee schedule, no slippage and no impact model".to_string()),
    ));
    assert!(free.to_string().contains("FRICTIONLESS"), "{}", free.to_string());

    let costed = base.clone().with_realism(crate::realism::RealismStamp::new(
        [("engine.fee_rate".to_string(), "0.0004".to_string())],
        None,
    ));
    assert!(!costed.to_string().contains("FRICTIONLESS"));
    assert_eq!(costed.to_string(), base.to_string(), "a costed stamp must move no row");
}

/// ⚠ **"Not recorded" is not "zero".** A run whose report predates the extended block must say
/// so by name — rendering `0.0000` for its Sortino is the failure this whole `Option` exists to
/// prevent, and it would look exactly like a strategy with no downside.
#[test]
fn an_unrecorded_metric_says_so_rather_than_rendering_zero() {
    use crate::metric_catalog::{MetricSelection, parse_metric_selection};

    let old = a_finite_report(); // no extended block, as an older document reads back
    assert!(old.metric_value("sortino").is_none());

    let sel = parse_metric_selection("sortino").unwrap();
    assert!(sel.needs_extended());
    let rendered = old.render_metrics(&sel);
    assert!(rendered.contains("not recorded"), "{rendered}");
    assert!(!rendered.contains("0.0000"), "a zero would read as a real answer: {rendered}");

    // The two non-metric keywords answer the same way, each about its own block.
    assert!(old.render_metrics(&MetricSelection::Honesty).contains("not recorded"));
    assert!(old.render_metrics(&MetricSelection::Realism).contains("not recorded"));
}

/// The long-form block, the counters and the stamp all survive the round trip — they are keys
/// of `report.json`, which is the document a reading verb holds.
#[test]
fn the_three_new_blocks_round_trip_and_stay_optional() {
    let mut r = sample_result();
    r.dropped = vec![("BTCUSDT".into(), "below-min-qty".into(), 0.1, 1.0)];
    let written = BacktestReport::from_result(Some("demo".into()), &r, 252.0).with_realism(
        crate::realism::RealismStamp::new(
            [("engine.slippage".to_string(), "0.0001".to_string())],
            None,
        ),
    );

    let json = serde_json::to_string(&written).unwrap();
    let back: BacktestReport = serde_json::from_str(&json).unwrap();

    // Exact on the counts, tolerant on the float — serde_json's default (non-`float_roundtrip`)
    // parser is not always bit-exact on the way back in, the documented limitation
    // `json_round_trips` above already states.
    let (a, b) = (back.extended.as_ref().unwrap(), written.extended.as_ref().unwrap());
    assert_eq!(a.consecutive_wins, b.consecutive_wins);
    assert!(
        (a.sortino - b.sortino).abs() <= b.sortino.abs() * 1e-9 + 1e-12,
        "sortino {} vs {}",
        a.sortino,
        b.sortino
    );
    assert_eq!(back.honesty.as_ref().unwrap().denials.len(), 1);
    assert_eq!(back.realism.as_ref().unwrap().get("engine.slippage"), Some("0.0001"));

    // ...and a document that carries none of the three still parses, which is what makes them
    // safe to add to a file already on people's disks.
    let bare = oldest_report(None);
    let old: BacktestReport = serde_json::from_str(&bare).unwrap();
    assert!(old.extended.is_none() && old.honesty.is_none() && old.realism.is_none());
}
