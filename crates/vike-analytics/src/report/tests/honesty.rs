//! The honesty counters, the realism stamp and "not recorded" rather than zero.

use super::*;
use vike_model::OrderDrop;

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
        OrderDrop {
            symbol: "BTCUSDT".into(),
            reason: "insufficient-margin".into(),
            size: 1.0,
            weight: 1.0,
        },
        OrderDrop {
            symbol: "BTCUSDT".into(),
            reason: "insufficient-margin".into(),
            size: 2.0,
            weight: 1.0,
        },
        OrderDrop { symbol: "ETHUSDT".into(), reason: "volume_cap".into(), size: 3.0, weight: 1.0 },
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

/// The realism harm, as a test: a free run and a costed one must not print the same table. Each
/// stamped table says what it was charged, in words, under the name; an unstamped one adds nothing.
#[test]
fn a_stamped_run_says_what_it_was_charged_and_an_unstamped_one_adds_nothing() {
    use crate::realism::{RealismStamp, RealismStampCosts};
    let base = BacktestReport::from_result(None, &sample_result(), 252.0);
    let stamped = |fees: &str, charged_nothing: Option<&str>| {
        base.clone().with_realism(RealismStamp::new(
            [("engine.fee_rate".to_string(), "0".to_string())],
            RealismStampCosts {
                fees: fees.to_string(),
                slippage: "none".to_string(),
                funding: "not applicable".to_string(),
            },
            charged_nothing.map(str::to_string),
        ))
    };

    let free = stamped("no fees", Some("every fill was free: no fees, no slippage")).to_string();
    assert!(
        free.contains("costs:         fees: no fees; slippage: none; funding: not applicable\n"),
        "{free}"
    );
    let venue = stamped("venue fees: binance, taker 0.1%, maker 0.1%", None).to_string();
    assert!(venue.contains("costs:         fees: venue fees: binance, taker 0.1%"), "{venue}");
    for table in [&free, &venue] {
        assert!(!table.to_lowercase().contains("frictionless"), "{table}");
    }
    assert_ne!(free, venue);
    assert!(!base.to_string().contains("costs:"), "no stamp, no row: {base}");
}

/// A run booked in another currency than the dollar labels it, right under the name, because every
/// money cell below is an amount of THAT currency; a dollar run's table does not move by one byte.
#[test]
fn a_run_booked_off_the_dollar_labels_its_currency_and_a_dollar_run_does_not() {
    use crate::realism::{ACCOUNT_CURRENCY_KEY, DOLLAR_LEDGER, RealismStamp};

    let base = BacktestReport::from_result(Some("demo".into()), &sample_result(), 252.0);
    // A stamp carrying the ledger key alone (no cost words), so only the ledger label can move a row.
    let stamped = |currency: &str| {
        base.clone().with_realism(RealismStamp {
            values: [(ACCOUNT_CURRENCY_KEY.to_string(), currency.to_string())].into(),
            ..RealismStamp::default()
        })
    };

    let yen = stamped("JPY").to_string();
    assert_eq!(yen.lines().nth(1), Some("account_currency:   JPY"), "under the name:\n{yen}");
    assert_eq!(yen.lines().next(), base.to_string().lines().next());

    assert_eq!(stamped(DOLLAR_LEDGER).to_string(), base.to_string(), "a dollar table moves no row");
    assert_eq!(base.to_string().lines().filter(|l| l.starts_with("account_currency")).count(), 0);
}

/// ⚠ **"Not recorded" is not "zero".** A run whose report predates the extended block must say
/// so by name — rendering `0.0000` for its Sortino is the failure this whole `Option` exists to
/// prevent, and it would look exactly like a strategy with no downside.
#[test]
fn an_unrecorded_metric_says_so_rather_than_rendering_zero() {
    use crate::metric_catalog::{MetricSelection, parse_metric_selection};

    let old = a_finite_report(); // no extended block, as an older document reads back
    assert!(old.metric_value("sortino").is_none());

    let selection = parse_metric_selection("sortino").unwrap();
    assert!(selection.needs_extended());
    let rendered = old.render_metrics(&selection);
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
    r.dropped = vec![OrderDrop {
        symbol: "BTCUSDT".into(),
        reason: "below-min-qty".into(),
        size: 0.1,
        weight: 1.0,
    }];
    let written = BacktestReport::from_result(Some("demo".into()), &r, 252.0).with_realism(
        crate::realism::RealismStamp::new(
            [("engine.slippage".to_string(), "0.0001".to_string())],
            crate::realism::RealismStampCosts::default(),
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
