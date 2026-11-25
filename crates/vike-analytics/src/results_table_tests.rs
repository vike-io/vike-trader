use super::*;
use crate::equity::equity_curve_from_trades;
use crate::metric_catalog::{MetricHome, spec_for};
use crate::test_support::trade;
// The annualization default, imported rather than reached through `super::*`: the module above
// stopped re-exporting it under a second name when it moved here out of vike-report.
use crate::results::DAILY_PERIODS_PER_YEAR;
// Imported HERE rather than at module scope: production code composes through
// `TradingResults`, and the tests want the direct call precisely because comparing a value
// against it is what proves nothing was reimplemented.
use crate::metrics;
use crate::results::TradingResultsExtendedMetrics;
use vike_model::NotionalKind;

/// A small trades vec with BOTH wins and losses so every metric is finite.
fn sample_trades() -> Vec<Trade> {
    vec![
        trade(10.0, 0.5, true, 2_000),
        trade(-4.0, 0.5, true, 3_000),
        trade(6.0, 0.3, false, 4_000),
        trade(-2.0, 0.2, true, 5_000),
    ]
}

fn sample_sheet() -> (TradingResultsTable, Vec<f64>, Vec<i64>) {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let sheet = TradingResultsTable::from_result_parts(
        Some("sess-1".into()),
        trades,
        eq.clone(),
        ts.clone(),
        DAILY_PERIODS_PER_YEAR,
    );
    (sheet, eq, ts)
}

fn result_of(trades: Vec<Trade>, eq: Vec<f64>, ts: Vec<i64>) -> TradingRecord {
    let n_trades = trades.len();
    let final_equity = eq.last().copied().unwrap_or(0.0);
    TradingRecord {
        trades,
        equity_curve: eq,
        final_equity,
        n_trades,
        equity_timestamp: ts,
        ..Default::default()
    }
}

/// **The roster property.** Every catalog id is carried, so this type cannot be a shorter list
/// than the catalog again — which is exactly what it was.
#[test]
fn the_catalog_is_the_roster() {
    let (sheet, _, _) = sample_sheet();
    let rendered = sheet.rendered_rows();
    // A closed-trade row appears only where a run armed that block, which no live sheet does.
    let roster: Vec<_> = METRICS.iter().filter(|m| m.home != MetricHome::ClosedTrades).collect();
    assert_eq!(rendered.len(), roster.len(), "one row per catalog metric, no more, no fewer");
    for (m, (id, _)) in roster.iter().zip(rendered.iter()) {
        assert_eq!(m.id, *id, "rows render in METRICS declaration order");
    }
}

/// ⚠ **The twelve numbers the old field list threw away and this door still computes.**
/// `TradingResults::from_record` computed every one of them and the old type copied 25 of 38;
/// a test that only asserted those 25 would have stayed green through the whole defect, so
/// these are named one by one. Eleven now: `mar_ratio`, the same formula as `calmar`, left the
/// catalog on 2026-11-09.
#[test]
fn the_discarded_metrics_are_carried_now() {
    let (sheet, _, _) = sample_sheet();
    // `funding_paid` is the thirteenth id the old shape dropped and is deliberately NOT here —
    // `from_result_parts` declares it unrecorded, with the argument on that function.
    for id in [
        "long_ratio",
        "recovery_factor",
        "ulcer_index",
        "ulcer_performance_index",
        "k_ratio",
        "risk_return_ratio",
        "returns_volatility",
        "returns_skewness",
        "returns_kurtosis",
        "tail_ratio",
        "omega",
    ] {
        assert!(sheet.metric(id).is_some(), "{id} must be carried, not discarded");
    }
}

/// **The F31 invariant, in its strongest form**: every catalog value equals
/// `TradingResults::metric_value` over the same result. The old test asserted eight shared
/// fields plus eighteen extended ones by hand; there is nothing left to enumerate, because the
/// live door composes the report rather than reading beside it.
#[test]
fn every_value_equals_the_one_report_composition() {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let sheet = TradingResultsTable::from_result(
        Some("sess-1".into()),
        &result_of(trades.clone(), eq.clone(), ts.clone()),
        DAILY_PERIODS_PER_YEAR,
    );
    let report = TradingResults::from_record(
        Some("sess-1".into()),
        &result_of(trades, eq, ts),
        DAILY_PERIODS_PER_YEAR,
    );

    assert_eq!(sheet.name, report.name);
    for m in METRICS {
        assert_eq!(
            sheet.metric(m.id),
            report.metric_value(m.id),
            "{} diverges from the report composition",
            m.id
        );
    }
}

/// ...and the EXTENDED half of that composition is still `TradingResultsExtendedMetrics`' own, with the tail
/// confidence unmoved. The numbers must not have shifted when the roster did: a stored
/// tearsheet's VaR changing silently would look exactly like a fix.
#[test]
fn the_extended_values_are_the_one_extended_composition() {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let r = result_of(trades, eq, ts);
    let sheet = TradingResultsTable::from_result(None, &r, DAILY_PERIODS_PER_YEAR);
    let ext = TradingResultsExtendedMetrics::from_result(&r, DAILY_PERIODS_PER_YEAR);

    for m in METRICS.iter().filter(|m| m.home == MetricHome::Extended) {
        assert_eq!(sheet.metric(m.id), ext.value_of(m.id), "{} left the one composition", m.id);
    }
    assert_eq!(
        sheet.metric("value_at_risk_95"),
        Some(metrics::value_at_risk(&r.equity_curve, 0.95))
    );
    assert_eq!(
        sheet.metric("expected_shortfall_95"),
        Some(metrics::expected_shortfall(&r.equity_curve, 0.95))
    );
}

/// Every carried metric is finite over a trade set with both wins and losses, and the wiring
/// matches the direct `metrics::` calls — the no-reimplementation check.
#[test]
fn every_stat_is_finite_and_matches_direct_metrics() {
    let (sheet, eq, _) = sample_sheet();
    for (spec, v) in sheet.rows() {
        if let Some(v) = v {
            assert!(v.is_finite(), "{} must be finite, got {v}", spec.id);
        }
    }
    let trades = sample_trades();
    assert_eq!(sheet.metric("n_trades"), Some(4.0));
    assert_eq!(sheet.metric("net_profit"), Some(metrics::net_profit(&trades)));
    assert_eq!(sheet.metric("win_rate"), Some(metrics::win_rate(&trades)));
    assert_eq!(sheet.metric("profit_factor"), Some(metrics::profit_factor(&trades)));
    assert_eq!(sheet.metric("sharpe"), Some(metrics::sharpe(&eq, DAILY_PERIODS_PER_YEAR)));
    assert_eq!(sheet.metric("max_drawdown"), Some(metrics::max_drawdown(&eq)));
    // net profit sanity: 10 - 4 + 6 - 2 = 10
    assert_eq!(sheet.metric("net_profit"), Some(10.0));
    // final equity = seed + Σ(pnl - fees) = 1000 + (10-.5)+(-4-.5)+(6-.3)+(-2-.2) = 1008.5
    assert!((sheet.metric("final_equity").unwrap() - 1_008.5).abs() < 1e-9);
}

/// ⚠ The journal door has no funding cashflow to fold, so it says so rather than publishing
/// the `Default` `0.0` as a measurement. See `from_result_parts`' own doc.
#[test]
fn funding_paid_is_unrecorded_on_the_synthesized_door() {
    let (sheet, _, _) = sample_sheet();
    assert_eq!(sheet.metric("funding_paid"), None, "a fill stream measures no funding");
    let rendered = sheet.rendered_rows();
    let (_, text) = rendered.iter().find(|(id, _)| *id == "funding_paid").expect("the row");
    assert_eq!(text, NOT_RECORDED, "and it renders as words, never as 0.00");

    // ...while a REAL result's measured value survives.
    let mut r = result_of(sample_trades(), vec![1_000.0, 1_010.0], vec![1, 2]);
    r.funding_paid = -1.25;
    let measured = TradingResultsTable::from_result(None, &r, DAILY_PERIODS_PER_YEAR);
    assert_eq!(measured.metric("funding_paid"), Some(-1.25));
}

/// A report with NO `extended` block yields "not recorded" for every extended metric, never a
/// tearsheet of zeros. This is the shape a `report.json` written before that block existed has,
/// and the `crates/vike-cli` `--html` door will hold exactly such files.
#[test]
fn an_extendedless_report_renders_words_not_zeros() {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let mut report =
        TradingResults::from_record(None, &result_of(trades, eq, ts), DAILY_PERIODS_PER_YEAR);
    report.extended = None;
    let sheet = TradingResultsTable::from_report(&report);

    assert_eq!(sheet.metric("sortino"), None, "no extended block, no sortino");
    assert!(sheet.metric("sharpe").is_some(), "the compact scalars still answer");
    let rendered = sheet.rendered_rows();
    let (_, text) = rendered.iter().find(|(id, _)| *id == "sortino").expect("the row");
    assert_eq!(text, NOT_RECORDED);
}

#[test]
fn display_is_non_empty_and_labeled() {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let sheet =
        TradingResultsTable::from_result_parts(None, trades, eq, ts, DAILY_PERIODS_PER_YEAR);
    let s = sheet.to_string();
    assert!(!s.is_empty());
    // The header names the document by its own words: a stored run's table is no "live" sheet, and
    // the "Tearsheet" word is retired with the type that carried it.
    assert!(s.starts_with("=== Trading results: (unnamed) (schema"), "{s}");
    assert!(!s.contains("Tearsheet"), "{s}");
    assert!(s.contains("(unnamed)"));
    assert!(s.contains(&format!("schema {TRADING_RESULTS_TABLE_SCHEMA}")));
    assert!(s.contains("sharpe:"));
    assert!(s.contains("win_rate:"));
    assert!(s.contains("max_drawdown:"));
    // The table renders through `MetricUnit::render`, so a PERCENT row carries the `%` the
    // four hand-rolled spellings kept forgetting.
    assert!(s.contains('%'), "percent rows must be scaled and suffixed:\n{s}");
}

/// ⚠ **The wire shape**: FLAT, one top-level key per catalog id, `schema` beside `name`.
#[test]
fn the_document_is_flat_and_carries_every_catalog_key() {
    let (sheet, _, _) = sample_sheet();
    let json = serde_json::to_string(&sheet).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let obj = parsed.as_object().expect("a JSON object");

    assert_eq!(obj["schema"], TRADING_RESULTS_TABLE_SCHEMA);
    assert_eq!(obj["name"], "sess-1");
    // A closed-trade row appears only where a run armed that block, which no live sheet does, so
    // the document's key set is what it was before those rows existed.
    let roster: Vec<_> = METRICS.iter().filter(|m| m.home != MetricHome::ClosedTrades).collect();
    for m in &roster {
        assert!(obj.contains_key(m.id), "{} is missing from the document", m.id);
    }
    assert_eq!(
        obj.len(),
        roster.len() + 2,
        "no key beyond `schema`, `name` and the catalog: {json}"
    );
    // No nesting: the flat shape is what `vike-cli report account: --json` and the MCP tool `report`
    // answer.
    assert!(obj.values().all(|v| !v.is_object()), "no key may nest: {json}");
}

/// ⚠ A `Count` metric stays an INTEGER on the wire. `serde_json::Value`'s `PartialEq` does not
/// consider `2.0` equal to `2`, so widening these to `f64` internally must not reach the bytes
/// — a consumer comparing `json["n_trades"] == 2` would stop matching.
#[test]
fn a_count_metric_serializes_as_an_integer() {
    let (sheet, _, _) = sample_sheet();
    let parsed: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&sheet).unwrap()).unwrap();
    for m in METRICS.iter().filter(|m| m.unit == MetricUnit::Count) {
        let v = &parsed[m.id];
        if v.is_null() {
            continue; // an unrecorded count is `null`, which is a different assertion
        }
        assert!(v.is_i64() || v.is_u64(), "{} must be an integer on the wire, got {v}", m.id);
    }
    assert_eq!(parsed["n_trades"], 4);
}

/// The document round-trips through the reader half — the property that makes `schema` worth
/// declaring at all.
#[test]
fn serde_round_trips_through_the_reader() {
    let (sheet, _, _) = sample_sheet();
    let json = serde_json::to_string(&sheet).unwrap();
    let back: TradingResultsTable = serde_json::from_str(&json).expect("a writer's bytes parse");
    assert_eq!(back.schema, TRADING_RESULTS_TABLE_SCHEMA);
    assert_eq!(back.name.as_deref(), Some("sess-1"));
    for m in METRICS {
        // ⚠ NOT `assert_eq!(back, sheet)`: a non-finite value serializes as `null` and comes
        // back as `None`, so the `inf` house sentinel does NOT survive the wire. That is
        // pre-existing behaviour (serde_json has no `inf`), and stating it here is the point.
        match sheet.metric(m.id) {
            Some(v) if v.is_finite() => assert_eq!(back.metric(m.id), Some(v), "{}", m.id),
            _ => assert_eq!(back.metric(m.id), None, "{} was not finite", m.id),
        }
    }
}

/// Forward AND backward compatibility, both asserted: an id this build does not know is
/// skipped, and one that is absent stays unrecorded rather than becoming `0.0`.
#[test]
fn an_unknown_key_is_skipped_and_a_missing_one_is_unrecorded() {
    let doc = r#"{"name":"old","sharpe":1.5,"a_metric_from_the_future":42.0}"#;
    let sheet: TradingResultsTable = serde_json::from_str(doc).expect("the document still parses");
    assert_eq!(
        sheet.schema, TRADING_RESULTS_TABLE_SCHEMA_UNVERSIONED,
        "no schema key -> unversioned"
    );
    assert_eq!(sheet.metric("sharpe"), Some(1.5));
    assert_eq!(sheet.metric("sortino"), None, "absent is unrecorded, never 0.0");
    assert!(spec_for("a_metric_from_the_future").is_none(), "the unknown id really is unknown");
}

/// ⚠ **No catalog id may collide with this document's own two keys.** `#[serde(flatten)]`
/// resolves the struct's OWN field names first, so a metric called `name` or `schema` would be
/// shadowed on the way out and swallowed on the way back in — a metric that silently does not
/// exist on the wire, which is the failure mode hardest to notice from either side. Nothing in
/// the catalog is called either today; this is the test that says so rather than the reader who
/// assumes it.
#[test]
fn no_catalog_id_collides_with_the_documents_own_keys() {
    for m in METRICS {
        assert!(
            m.id != "name" && m.id != "schema",
            "{} collides with a TradingResultsTable field and would be shadowed by flatten",
            m.id
        );
    }
}

/// An id the catalog does not hold cannot be written into the document — the property that
/// keeps the key set equal to the published roster.
#[test]
fn setting_an_uncatalogued_id_writes_nothing() {
    let mut v = TradingResultsTableMetricValues::unrecorded();
    assert!(!v.set("shrapnel", Some(1.0)), "an unknown id is refused");
    assert!(v.set("sharpe", Some(1.0)), "a catalogued one is written");
    assert_eq!(v.get("sharpe"), Some(1.0));
    assert_eq!(v.get("shrapnel"), None);
}

// ---- `trading_results_table_from_fills`: the ONE fold, pinned against the three spellings it replaced --------
//
// Written BEFORE the fold was unified (`vike-report`'s `tearsheet_from_journal` and
// `trading_results_table_from_fills`, and the tearsheet CLI's `build_realized`, which
// `crates/vike-cli/src/cmd/report/journal/mod.rs` records), against the
// OLD code on a lane. The hashes below are the OLD output: every catalog row's `f64::to_bits`, FNV-1a.
// Never re-pin one to make a test pass (decision 0021): a changed hash is a changed tearsheet.
//
// ⚠ RE-PINNED ONCE, 2026-11-09, for a change the owner ordered, not to pass: `mar_ratio` (`calmar`
// under a second name) left the catalog, the schema went to 2 and the curve is sampled once per UTC
// day. Each constant's doc keeps the value 0153 recorded; `reference` gained the day step.

mod fold_pin {
    use super::*;
    use crate::trades::reconstruct_trades;
    use vike_model::events::{PositionSide, TradeId};

    const SEED: f64 = 10_000.0;
    const PERIODS_PER_YEAR: f64 = 252.0;
    /// Ascending-`ts` fills: open, add, partial reduce, close, flip, close.
    /// (0153's value: `0x02bf_1fef_90ab_7a53`.)
    // RE-PINNED 2026-11-13 for the owner's phase-4 rename (P4.5c): the serialized key and catalog-id
    // words are spelled out; every value is the same (the comparisons beside each pin still pass).
    // Was `0xf478_fbbb_2f33_406e`.
    const ORDERED_HASH: u64 = 0x4afc_2972_8f5a_24dc;
    /// The journal-order stream of [`unordered`], folded untouched: two trades.
    /// (0153's value: `0xe3a1_5809_ee35_5d7e`.)
    // Was `0x81a7_131b_d75d_a061` (the same re-pin).
    const AS_HELD_HASH: u64 = 0xb629_eee6_a899_8f7b;
    /// The same stream sorted by `ts`: one trade. (0153's value: `0xcf3c_9999_b247_70d0`.)
    // Was `0xe1f3_ed17_75d2_f966` (the same re-pin).
    const BY_TIME_HASH: u64 = 0xe932_a7ea_05f6_3614;

    fn fill(trade_id: &str, side: i32, qty: f64, px: f64, ts: i64) -> FillEvent {
        FillEvent {
            trade_id: TradeId::new(trade_id).unwrap(),
            order_client_id: "c1".to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side,
            last_qty: qty,
            last_price: px,
            commission: 0.1,
            commission_asset: "USDT".into(),
            liquidity_side: "maker".into(),
            timestamp: ts,
            mark_price: None,
            position_side: PositionSide::Both,
        }
    }

    fn ordered() -> Vec<FillEvent> {
        vec![
            fill("t1", 1, 2.0, 100.0, 10),
            fill("t2", 1, 1.0, 130.0, 20),
            fill("t3", -1, 1.0, 150.0, 30),
            fill("t4", -1, 2.0, 140.0, 40),
            fill("t5", -1, 1.0, 120.0, 50),
            fill("t6", 1, 1.0, 125.0, 60),
        ]
    }

    /// Journal order is not time order: the short is read first, the two buys after it.
    fn unordered() -> Vec<FillEvent> {
        vec![
            fill("u1", -1, 2.0, 110.0, 30),
            fill("u2", 1, 1.0, 100.0, 10),
            fill("u3", 1, 1.0, 105.0, 20),
        ]
    }

    /// The fold, written out: what every one of the three old spellings did, plus the day step.
    fn reference(mut fills: Vec<FillEvent>, sort: bool) -> TradingResultsTable {
        if sort {
            fills.sort_by_key(|f| f.timestamp);
        }
        let trades = reconstruct_trades(&fills);
        let (equity, ts) = equity_curve_from_trades(SEED, &trades);
        let (equity, ts) = crate::equity::daily_equity_curve(&equity, &ts);
        TradingResultsTable::from_result_parts(None, trades, equity, ts, PERIODS_PER_YEAR)
    }

    fn bits(sheet: &TradingResultsTable) -> Vec<(&'static str, u64)> {
        let mut rows: Vec<(&'static str, u64)> =
            sheet.rows().map(|(s, v)| (s.id, v.map_or(u64::MAX, f64::to_bits))).collect();
        rows.push(("schema", u64::from(sheet.schema)));
        rows
    }

    fn fnv(rows: &[(&'static str, u64)]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (id, b) in rows {
            for byte in id.bytes().chain(b.to_le_bytes()) {
                h ^= u64::from(byte);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    }

    #[test]
    fn a_time_ordered_stream_folds_to_the_old_output_in_either_order_mode() {
        let held = trading_results_table_from_fills(
            ordered(),
            TradingResultsTableFillOrder::AsHeld,
            SEED,
            PERIODS_PER_YEAR,
        );
        let sorted = trading_results_table_from_fills(
            ordered(),
            TradingResultsTableFillOrder::ByTime,
            SEED,
            PERIODS_PER_YEAR,
        );
        assert_eq!(bits(&held.0), bits(&reference(ordered(), false)));
        assert_eq!(bits(&sorted.0), bits(&reference(ordered(), true)));
        assert_eq!(fnv(&bits(&held.0)), ORDERED_HASH);
        assert_eq!(fnv(&bits(&sorted.0)), ORDERED_HASH);
        // The curve the CLI plots is the one the document was built from: the seed, then the close
        // of the one UTC day all three trades fell on.
        assert_eq!((held.1.len(), held.2.len()), (2, 2));
        assert_eq!(held.1[0], SEED);
    }

    /// The doubt, pinned both ways: a stream whose `ts` is not ascending as held folds to
    /// DIFFERENT documents under the two modes, and each mode keeps its old callers' answer
    /// (`AsHeld`: the journal doors and the CLI; `ByTime`: the desktop).
    #[test]
    fn an_out_of_order_stream_keeps_both_old_behaviours_reachable() {
        let held = trading_results_table_from_fills(
            unordered(),
            TradingResultsTableFillOrder::AsHeld,
            SEED,
            PERIODS_PER_YEAR,
        )
        .0;
        let sorted = trading_results_table_from_fills(
            unordered(),
            TradingResultsTableFillOrder::ByTime,
            SEED,
            PERIODS_PER_YEAR,
        )
        .0;
        assert_eq!(bits(&held), bits(&reference(unordered(), false)));
        assert_eq!(bits(&sorted), bits(&reference(unordered(), true)));
        assert_eq!(fnv(&bits(&held)), AS_HELD_HASH);
        assert_eq!(fnv(&bits(&sorted)), BY_TIME_HASH);
        assert_eq!(held.metric("n_trades"), Some(2.0));
        assert_eq!(sorted.metric("n_trades"), Some(1.0));
    }

    /// Moved here from `vike-report`'s `journal_read_tests.rs`: a stream handed NEWEST-FIRST (as the
    /// desktop holds the rows it read from the datahub) folds like the time-ordered one in
    /// `ByTime` mode.
    #[test]
    fn a_tearsheet_over_held_fills_does_not_depend_on_their_order() {
        let newest_first = vec![
            fill("t4", -1, 1.0, 102.0, 40),
            fill("t3", 1, 1.0, 105.0, 30),
            fill("t2", -1, 1.0, 110.0, 20),
            fill("t1", 1, 1.0, 100.0, 10),
        ];
        let mut ordered = newest_first.clone();
        ordered.reverse();
        let a = trading_results_table_from_fills(
            newest_first,
            TradingResultsTableFillOrder::ByTime,
            1_000.0,
            252.0,
        )
        .0;
        let b = trading_results_table_from_fills(
            ordered,
            TradingResultsTableFillOrder::ByTime,
            1_000.0,
            252.0,
        )
        .0;
        assert_eq!(a.rendered_rows(), b.rendered_rows());
        let n = a.rendered_rows().into_iter().find(|(id, _)| *id == "n_trades").map(|(_, v)| v);
        assert_eq!(n.as_deref(), Some("2"), "two closed round trips");
    }

    /// **The day step, before and after** (2026-11-09): eleven round trips over seven UTC days, a
    /// 24/7 book. The sheet's curve is the opening plus one close per day (a day with no trade
    /// carries the day before) and its ratios are that curve's at 365. Until then the curve had one
    /// point per trade at 252, which read eleven trades as eleven trading days. The PR quotes these.
    #[test]
    fn the_curve_is_one_close_per_utc_day_annualised_by_calendar_days() {
        const DAY: i64 = 86_400_000;
        let days: [&[f64]; 7] =
            [&[10.0, -5.0, 8.0], &[-6.0, 4.0], &[], &[12.0, -3.0, -4.0], &[], &[6.0], &[-2.0, 9.0]];
        let mut fills = Vec::new();
        for (d, pnls) in days.iter().enumerate() {
            for (k, pnl) in pnls.iter().enumerate() {
                let ts = d as i64 * DAY + 3_600_000 * (2 * k as i64 + 1);
                fills.push(fill(&format!("b{d}{k}"), 1, 1.0, 100.0, ts));
                fills.push(fill(&format!("s{d}{k}"), -1, 1.0, 100.0 + pnl, ts + 60_000));
            }
        }
        let periods_per_year = crate::results::periods_per_year_for_days(true);
        assert_eq!(
            (periods_per_year, crate::results::periods_per_year_for_days(false)),
            (365.0, 252.0)
        );
        let (sheet, eq, ts) = trading_results_table_from_fills(
            fills.clone(),
            TradingResultsTableFillOrder::AsHeld,
            SEED,
            periods_per_year,
        );
        assert_eq!(sheet.metric("n_trades"), Some(11.0));
        assert_eq!(eq.len(), 8, "the opening and seven day closes: {eq:?}");
        assert_eq!(ts[1..].to_vec(), (1..=7).map(|d| d * DAY - 1).collect::<Vec<i64>>());
        assert_eq!(eq[2], eq[3], "day 2 has no trade and carries day 1");
        assert_eq!(sheet.metric("sharpe"), Some(metrics::sharpe(&eq, 365.0)));

        let (per_trade, _) = equity_curve_from_trades(SEED, &reconstruct_trades(&fills));
        let near = |got: f64, want: f64| (got / want - 1.0).abs() < 1e-9;
        let old = |id: &str| match id {
            "sharpe" => metrics::sharpe(&per_trade, 252.0),
            "sortino" => metrics::sortino(&per_trade, 252.0),
            "cagr" => metrics::cagr(&per_trade, 252.0),
            "calmar" => metrics::calmar(&per_trade, 252.0),
            _ => metrics::max_drawdown(&per_trade),
        };
        for (id, before, after) in [
            ("sharpe", 5.740504394703965, 14.456967451967389),
            ("sortino", 12.35493326099867, 74.68128403338827),
            ("cagr", 0.06323297118149074, 0.1497631166163711),
            ("calmar", 85.63624197115391, 624.7867620036852),
            ("max_drawdown", 0.0007383903091262506, 0.00023970276856712233),
        ] {
            assert!(near(old(id), before), "{id} before: {}", old(id));
            assert!(near(sheet.metric(id).unwrap(), after), "{id} after: {:?}", sheet.metric(id));
        }
    }
}

/// The refusal a door raises names the offenders it was handed and the truth the fold would have
/// hidden: 100,000 USDJPY closed 0.10 higher is 10,000 JPY, 66.62 USD at 150.10.
#[test]
fn the_foreign_quote_refusal_names_the_offenders_and_the_true_figure() {
    let why = trading_results_table_foreign_quote_refusal("USD_JPY (JPY), EUR_GBP (GBP)");
    assert!(why.starts_with("tearsheet refused:"), "{why}");
    assert!(why.contains("USD_JPY (JPY), EUR_GBP (GBP)"), "{why}");
    assert!(why.contains("66.62"), "the figure the fold would have gotten wrong: {why}");
    assert_eq!((10_000.0_f64 / 150.10 * 100.0).round() / 100.0, 66.62, "the figure is true");
}

/// **A sheet adds ONE currency, so the fills of a non-dollar ledger are left out and the sheet says so**
/// (decision 0183, item 13): a euro account's EURUSD fills are not dollars, and no name-based refusal sees it
/// (`EURUSD` is a dollar-quoted name). Journal order is kept, other venues' fills are untouched, and a journal
/// of one currency (no ledgers) comes back as it went in.
#[test]
fn the_fills_of_a_non_dollar_ledger_are_left_out_and_counted() {
    use vike_model::events::{PositionSide, TradeId};
    let fill = |id: &str, venue: &str| FillEvent {
        trade_id: TradeId::new(id).unwrap(),
        order_client_id: String::new(),
        venue: venue.into(),
        symbol: "EURUSD".into(),
        side: 1,
        last_qty: 1_000.0,
        last_price: 1.16,
        commission: 0.4,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let fills = vec![fill("a", "oanda"), fill("b", "fxcm"), fill("c", "oanda"), fill("d", "fxcm")];

    let (kept, left) = without_non_dollar_ledgers(fills.clone(), &[]);
    assert_eq!(kept, fills, "no ledger off the dollar: untouched");
    assert!(left.is_empty());

    let ledgers = [("fxcm".to_string(), "EUR".to_string())];
    let (kept, left) = without_non_dollar_ledgers(fills, &ledgers);
    assert_eq!(kept.iter().map(|f| f.trade_id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
    assert_eq!(
        left,
        [TradingResultsTableLeftOutLedger {
            venue: "fxcm".into(),
            currency: "EUR".into(),
            fills: 2
        }]
    );
    let label = trading_results_table_ledger_label(&left);
    assert!(label.starts_with("leaves out 2 EUR-ledger fills of fxcm"), "{label}");
    let one = [TradingResultsTableLeftOutLedger {
        venue: "fxcm".into(),
        currency: "EUR".into(),
        fills: 1,
    }];
    assert!(
        trading_results_table_ledger_label(&one).contains("1 EUR-ledger fill of fxcm"),
        "singular"
    );
}

/// **A sheet reads every fill as a linear unit worth its price, so the fills of any other contract are left out and the
/// sheet says so** (decision 0243, item 6). By `(venue, symbol)`: the venue's other symbols and the same name on another
/// venue stay, journal order is kept, and a journal of default contracts comes back as it went in. Both leave-outs
/// label one sheet.
#[test]
fn the_fills_of_a_contract_off_the_default_are_left_out_and_counted() {
    use vike_model::events::{PositionSide, TradeId};
    let fill = |id: &str, venue: &str, symbol: &str| FillEvent {
        trade_id: TradeId::new(id).unwrap(),
        order_client_id: String::new(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: 1_000.0,
        last_price: 60_000.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let fills = vec![
        fill("a", "deribit", "BTC-PERPETUAL"),
        fill("b", "deribit", "BTC-28MAR"),
        fill("c", "binance", "BTC-PERPETUAL"),
        fill("d", "deribit", "BTC-PERPETUAL"),
        fill("e", "ibkr", "ES"),
    ];

    let (kept, left) = without_non_default_contracts(fills.clone(), &[]);
    assert_eq!(kept, fills, "no contract off the default: untouched");
    assert!(left.is_empty());
    assert_eq!(trading_results_table_left_out_label(&[], &left), None);

    let inverse = ContractValue::inverse(1.0);
    let contracts = [
        ("deribit".to_string(), "BTC-PERPETUAL".to_string(), inverse),
        ("ibkr".to_string(), "ES".to_string(), ContractValue::linear(50.0)),
        ("ibkr".to_string(), "NQ".to_string(), ContractValue::linear(20.0)),
    ];
    let (kept, left) = without_non_default_contracts(fills, &contracts);
    assert_eq!(kept.iter().map(|f| f.trade_id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
    assert_eq!(left.len(), 2, "a contract with no fill names nothing");
    assert_eq!(left[0].value.kind, NotionalKind::Inverse);

    let label = trading_results_table_contract_label(&left);
    assert!(
        label.starts_with(
            "leaves out 2 fills of deribit BTC-PERPETUAL (inverse, 1 a unit), 1 fill of ibkr ES \
             (linear, 50 a unit): "
        ),
        "{label}"
    );
    assert_eq!(trading_results_table_left_out_label(&[], &left).as_deref(), Some(label.as_str()));

    let ledgers = [TradingResultsTableLeftOutLedger {
        venue: "fxcm".into(),
        currency: "EUR".into(),
        fills: 3,
    }];
    let both = trading_results_table_left_out_label(&ledgers, &left).expect("a label");
    assert!(both.contains("3 EUR-ledger fills of fxcm"), "{both}");
    assert!(both.contains("2 fills of deribit BTC-PERPETUAL"), "{both}");
    assert_eq!(
        trading_results_table_left_out_label(&ledgers, &[]).as_deref(),
        Some(trading_results_table_ledger_label(&ledgers).as_str()),
        "a ledger alone reads what it always did"
    );
}

/// **A fee in another asset is no quote money**: the fill keeps its quantity and price and loses its commission,
/// which is counted per asset, in first-seen order, and named in one sentence; the fold over the fills then adds
/// only the quote fees, to `total_fees` and to the curve alike. A rule that refuses nothing leaves every fill as it was.
#[test]
fn a_fee_in_another_asset_is_taken_off_the_fill_and_counted() {
    use vike_model::events::{PositionSide, TradeId};
    let fill = |id: &str, side: i32, px: f64, fee: f64, asset: &str| FillEvent {
        trade_id: TradeId::new(id).unwrap(),
        order_client_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: 1.0,
        last_price: px,
        commission: fee,
        commission_asset: asset.into(),
        liquidity_side: "taker".into(),
        timestamp: px as i64,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let fills = vec![
        fill("a", 1, 100.0, 0.0123, "BNB"),
        fill("b", -1, 110.0, 0.5, "USDT"),
        fill("c", 1, 120.0, 0.0077, "bnb"),
        fill("d", -1, 130.0, 0.001, "BTC"),
    ];
    let (untouched, none) = without_fees_in_other_assets(fills.clone(), |_| true);
    assert_eq!((untouched, none.len()), (fills.clone(), 0));

    let in_quote = |f: &FillEvent| f.commission_asset.as_str().eq_ignore_ascii_case("USDT");
    let (kept, other) = without_fees_in_other_assets(fills, in_quote);
    let fees: Vec<f64> = kept.iter().map(|f| f.commission).collect();
    assert_eq!(fees, [0.0, 0.5, 0.0, 0.0], "only the quote fee stays");
    assert!(kept.iter().all(|f| f.last_qty == 1.0), "no quantity moves");
    assert_eq!(other.len(), 2, "{other:?}");
    assert_eq!((other[0].asset.as_str(), other[0].fills), ("BNB", 2));
    assert!((other[0].amount - 0.02).abs() < 1e-12, "{other:?}");
    assert_eq!((other[1].asset.as_str(), other[1].amount), ("BTC", 0.001));
    assert_eq!(
        trading_results_table_other_asset_fee_label(&other),
        "fees in other assets: 0.02 BNB, 0.001 BTC (not converted; left out of total_fees and the \
         realized curve)"
    );

    let (sheet, _, _) = trading_results_table_from_fills(
        kept,
        TradingResultsTableFillOrder::ByTime,
        1_000.0,
        DAILY_PERIODS_PER_YEAR,
    );
    assert_eq!(sheet.metric("total_fees"), Some(0.5), "the USDT fee alone");
    assert_eq!(
        sheet.metric("final_equity"),
        Some(1_000.0 + 10.0 + 10.0 - 0.5),
        "the curve likewise"
    );
}
