//! Who produced a report and over what span: `source`, `period`, the one label and the one typed read.

use super::*;
use crate::record::{TradingPeriod, TradingRecordSimulator, TradingRecordSource};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, AccountRef};

/// 2026-01-01 00:00 UTC and one day after it, in epoch-ms.
const NEW_YEAR_MS: i64 = 1_767_225_600_000;
const DAY_MS: i64 = 86_400_000;

/// A record over a real span: four samples, one a day from [`NEW_YEAR_MS`].
fn dated_result() -> TradingRecord {
    TradingRecord {
        equity_timestamp: (0..4).map(|day| NEW_YEAR_MS + day * DAY_MS).collect(),
        ..sample_result()
    }
}

fn live_on(label: &str, tier: AccountTier) -> TradingRecordSource {
    let label = AccountLabel::parse(label).expect("a legal label");
    TradingRecordSource::Live { account: AccountRef { venue: "binance", label }, tier }
}

/// A record's period is its first and last equity sample, and a record that kept no timestamps has none.
#[test]
fn a_records_period_is_its_first_and_last_equity_sample() {
    let period = dated_result().period().expect("four samples");
    assert_eq!(period, TradingPeriod { from_ms: NEW_YEAR_MS, to_ms: NEW_YEAR_MS + 3 * DAY_MS });
    assert_eq!(sample_result().period(), None, "no timestamps, no period");
}

/// `TradingRecord::default()` is the simulator's own record; only a `Backtest` source has counters.
#[test]
fn only_a_backtest_record_carries_the_simulators_counters() {
    let record = TradingRecord::default();
    assert_eq!(record.source, TradingRecordSource::Backtest(TradingRecordSimulator::default()));
    assert!(record.simulator().is_some());

    let live =
        TradingRecord { source: live_on("MAIN", AccountTier::Demo), ..TradingRecord::default() };
    assert!(live.simulator().is_none());
    let step = TradingRecord {
        source: TradingRecordSource::WalkforwardStep { window: (100, 200), params: None },
        ..TradingRecord::default()
    };
    assert!(step.simulator().is_none());
}

/// `from_record` carries the record's source and period, and NEITHER reaches the serialized bytes.
#[test]
fn from_record_carries_the_source_and_period_and_serializes_neither() {
    let record = dated_result();
    let report = TradingResults::from_record(Some("demo".to_string()), &record, 252.0);
    assert_eq!(report.source.as_ref(), Some(&record.source));
    assert_eq!(report.period, record.period());

    let bare = TradingResults { source: None, period: None, ..report.clone() };
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(
        json,
        serde_json::to_string(&bare).unwrap(),
        "source and period are not on the wire"
    );
}

/// The one label, in its three wordings; a report whose source is unknown has none.
#[test]
fn the_source_label_names_the_source_and_the_period_in_one_wording() {
    let record = dated_result();
    let report = TradingResults::from_record(Some("sma cross".to_string()), &record, 252.0);
    assert_eq!(
        report.source_label().as_deref(),
        Some("backtest sma cross 2026-01-01 00:00 – 2026-01-04 00:00 UTC"),
        "a backtest with no strategy file recorded is named by its profile name"
    );
    let filed = report.clone().with_strategy("sma_cross.rhai", "00", None);
    assert_eq!(
        filed.source_label().as_deref(),
        Some("backtest sma_cross.rhai 2026-01-01 00:00 – 2026-01-04 00:00 UTC")
    );

    let live = TradingRecord { source: live_on("MAIN", AccountTier::Demo), ..record.clone() };
    let live = TradingResults::from_record(None, &live, 365.0);
    assert_eq!(
        live.source_label().as_deref(),
        Some("binance/MAIN (demo) 2026-01-01 00:00 – 2026-01-04 00:00 UTC")
    );

    let step = TradingRecordSource::WalkforwardStep { window: (100, 200), params: None };
    assert_eq!(step.label(Some("ignored"), record.period()), "walk-forward step [100, 200)");

    let undated = TradingResults::from_record(None, &sample_result(), 252.0);
    assert_eq!(undated.source_label().as_deref(), Some("backtest (unnamed)"), "no period, no span");

    assert_eq!(a_finite_report().source_label(), None, "a literal states no source");
}

/// The human table names the source on the line after `name:`, and a report whose source is unknown
/// (a bare parse of stored bytes) prints the table it always printed.
#[test]
fn the_table_prints_the_source_line_only_when_the_source_is_known() {
    let report = TradingResults::from_record(Some("sma cross".to_string()), &dated_result(), 252.0);
    let table = report.to_string();
    let mut lines = table.lines();
    assert_eq!(lines.next(), Some("name:          sma cross"));
    assert_eq!(
        lines.next(),
        Some("source:        backtest sma cross 2026-01-01 00:00 – 2026-01-04 00:00 UTC"),
        "{table}"
    );

    let stored: TradingResults =
        serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
    let bare = stored.to_string();
    assert!(!bare.contains("source:"), "{bare}");
    let without: Vec<&str> = table.lines().filter(|l| !l.starts_with("source:")).collect();
    assert_eq!(
        bare.lines().collect::<Vec<_>>(),
        without,
        "the rest of the table is the same bytes"
    );
}

/// The honesty block is a SIMULATOR's: a live account's results carry none, and a flat live record
/// with no trades is still diagnosed, its simulator counters read as zero.
#[test]
fn a_live_record_has_no_honesty_block_and_its_counters_read_zero() {
    let live = TradingRecord { source: live_on("MAIN", AccountTier::Demo), ..dated_result() };
    assert!(TradingResults::from_record(None, &live, 365.0).honesty.is_none());
    assert!(TradingResults::from_record(None, &dated_result(), 252.0).honesty.is_some());

    let flat = TradingRecord {
        source: live_on("MAIN", AccountTier::Demo),
        equity_curve: vec![1000.0; 3],
        final_equity: 1000.0,
        ..TradingRecord::default()
    };
    let zero = TradingResults::from_record(None, &flat, 365.0).zero_trade.expect("flat, no trades");
    assert!(zero.causes.iter().all(|c| c.code != "orders-denied"), "{zero:?}");
}

/// **A live record's funding is NOT RECORDED**: the execution stream carries no funding, so its `0.0`
/// would be a false number. Every rendering reads it so: the table's text cell, its JSON (`null`) and a
/// comparison row; a backtest keeps its measured value.
#[test]
fn a_live_records_funding_is_not_recorded() {
    use crate::results_comparison::TradingResultsComparison;
    use crate::results_table::{NOT_RECORDED, TradingResultsTable};
    let live = TradingRecord { source: live_on("MAIN", AccountTier::Demo), ..dated_result() };
    let live = TradingResults::from_record(None, &live, 365.0);
    assert!(live.is_live());
    assert_eq!(live.metric_value("funding_paid"), None);
    let table = TradingResultsTable::from_report(&live);
    let rendered = table.rendered_rows();
    let (_, text) = rendered.iter().find(|(id, _)| *id == "funding_paid").expect("the row");
    assert_eq!(text, NOT_RECORDED, "never 0.00");
    assert!(serde_json::to_value(&table).unwrap()["funding_paid"].is_null());

    let backtest = TradingResults::from_record(None, &dated_result(), 252.0);
    assert!(!backtest.is_live());
    assert_eq!(backtest.metric_value("funding_paid"), Some(backtest.funding_paid));
    let comparison = TradingResultsComparison::of(backtest, live);
    let row = comparison.rows().into_iter().find(|r| r.spec.id == "funding_paid").expect("a row");
    assert_eq!((row.b, row.difference), (None, None), "no live value, no difference");
}

/// **A live window that measured nothing** (no trade closed, a flat curve: no fills in it) keeps its money
/// and counts and records no ratio: the table says "not recorded" (`null` in its JSON), and a comparison
/// holds NO difference against it, not even of money — `final_equity` b − a would read the backtest's
/// gain as the account's loss. A backtest that traded nothing keeps its numbers.
#[test]
fn a_live_record_that_measured_nothing_records_no_ratio_and_no_difference() {
    use crate::results_comparison::TradingResultsComparison;
    use crate::results_table::{NOT_RECORDED, TradingResultsTable};
    let flat = TradingRecord {
        source: live_on("MAIN", AccountTier::Demo),
        trades: Vec::new(),
        n_trades: 0,
        per_symbol_pnl: Vec::new(),
        equity_curve: vec![1_000.0; 4],
        final_equity: 1_000.0,
        ..dated_result()
    };
    let empty = TradingResults::from_record(None, &flat, 365.0);
    assert!(empty.measured_nothing());
    for id in ["sharpe", "win_rate", "total_return", "max_drawdown", "profit_factor"] {
        assert_eq!(empty.metric_value(id), None, "{id}");
    }
    assert_eq!(empty.metric_value("final_equity"), Some(1_000.0));
    assert_eq!(empty.metric_value("n_trades"), Some(0.0));
    let table = TradingResultsTable::from_report(&empty);
    let rendered = table.rendered_rows();
    let (_, text) = rendered.iter().find(|(id, _)| *id == "sharpe").expect("the row");
    assert_eq!(text, NOT_RECORDED, "never 0.00");
    assert!(serde_json::to_value(&table).unwrap()["sharpe"].is_null());

    let idle = TradingRecord { source: TradingRecordSource::default(), ..flat };
    let idle = TradingResults::from_record(None, &idle, 252.0);
    assert!(!idle.measured_nothing(), "a backtest's zero is its own measurement");
    assert!(idle.metric_value("sharpe").is_some());
    let backtest = TradingResults::from_record(None, &dated_result(), 252.0);
    let comparison = TradingResultsComparison::of(backtest, empty);
    let rows = comparison.rows();
    assert!(rows.iter().all(|r| r.difference.is_none()), "no difference against an empty side");
    let equity = rows.iter().find(|r| r.spec.id == "final_equity").expect("a row");
    assert_eq!(equity.b, Some(1_000.0), "its money is still shown");
}

/// `read_stored` is the typed read of stored bytes: the reader's source and period are attached, and
/// text that is not a report is refused in words.
#[test]
fn read_stored_attaches_the_readers_source_and_refuses_what_is_not_a_report() {
    let record = dated_result();
    let text = serde_json::to_string(&TradingResults::from_record(None, &record, 252.0)).unwrap();
    let source = live_on("MAIN", AccountTier::Live);
    let back = TradingResults::read_stored(&text, source.clone(), record.period()).unwrap();
    assert_eq!(back.source, Some(source));
    assert_eq!(back.period, record.period());
    assert_eq!(serde_json::to_string(&back).unwrap(), text, "the bytes read back are the bytes");

    let refused = TradingResults::read_stored("{}", TradingRecordSource::default(), None);
    assert!(refused.unwrap_err().starts_with("not a stored report: "));
}
