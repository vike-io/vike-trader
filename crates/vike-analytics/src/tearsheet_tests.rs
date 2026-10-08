use super::*;
use crate::equity::equity_curve_from_trades;
use crate::metric_catalog::{MetricHome, spec_for};
use crate::test_support::trade;
// The annualization default, imported rather than reached through `super::*`: the module above
// stopped re-exporting it under a second name when it moved here out of vike-report.
use crate::report::DAILY_PERIODS_PER_YEAR;
// Imported HERE rather than at module scope: production code composes through
// `BacktestReport`, and the tests want the direct call precisely because comparing a value
// against it is what proves nothing was reimplemented.
use crate::metrics;
use crate::report::ExtendedMetrics;

/// A small trades vec with BOTH wins and losses so every metric is finite.
fn sample_trades() -> Vec<Trade> {
    vec![
        trade(10.0, 0.5, true, 2_000),
        trade(-4.0, 0.5, true, 3_000),
        trade(6.0, 0.3, false, 4_000),
        trade(-2.0, 0.2, true, 5_000),
    ]
}

fn sample_sheet() -> (LiveTearsheet, Vec<f64>, Vec<i64>) {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let sheet = LiveTearsheet::from_result_parts(
        Some("sess-1".into()),
        trades,
        eq.clone(),
        ts.clone(),
        DAILY_PERIODS_PER_YEAR,
    );
    (sheet, eq, ts)
}

fn result_of(trades: Vec<Trade>, eq: Vec<f64>, ts: Vec<i64>) -> BacktestResult {
    let n_trades = trades.len();
    let final_equity = eq.last().copied().unwrap_or(0.0);
    BacktestResult {
        trades,
        equity_curve: eq,
        final_equity,
        n_trades,
        equity_ts: ts,
        ..Default::default()
    }
}

/// **The roster property.** Every catalog id is carried, so this type cannot be a shorter list
/// than the catalog again — which is exactly what it was.
#[test]
fn the_catalog_is_the_roster() {
    let (sheet, _, _) = sample_sheet();
    let rendered = sheet.rendered_rows();
    assert_eq!(rendered.len(), METRICS.len(), "one row per catalog metric, no more, no fewer");
    for (m, (id, _)) in METRICS.iter().zip(rendered.iter()) {
        assert_eq!(m.id, *id, "rows render in METRICS declaration order");
    }
}

/// ⚠ **The twelve numbers the old field list threw away and this door still computes.**
/// `BacktestReport::from_result` computed every one of them and the old type copied 25 of 38;
/// a test that only asserted those 25 would have stayed green through the whole defect, so
/// these are named one by one.
#[test]
fn the_discarded_metrics_are_carried_now() {
    let (sheet, _, _) = sample_sheet();
    // `funding_paid` is the thirteenth id the old shape dropped and is deliberately NOT here —
    // `from_result_parts` declares it unrecorded, with the argument on that function.
    for id in [
        "long_ratio",
        "mar_ratio",
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
/// `BacktestReport::metric_value` over the same result. The old test asserted eight shared
/// fields plus eighteen extended ones by hand; there is nothing left to enumerate, because the
/// live door composes the report rather than reading beside it.
#[test]
fn every_value_equals_the_one_report_composition() {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let sheet = LiveTearsheet::from_result(
        Some("sess-1".into()),
        &result_of(trades.clone(), eq.clone(), ts.clone()),
        DAILY_PERIODS_PER_YEAR,
    );
    let report = BacktestReport::from_result(
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

/// ...and the EXTENDED half of that composition is still `ExtendedMetrics`' own, with the tail
/// confidence unmoved. The numbers must not have shifted when the roster did: a stored
/// tearsheet's VaR changing silently would look exactly like a fix.
#[test]
fn the_extended_values_are_the_one_extended_composition() {
    let trades = sample_trades();
    let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
    let r = result_of(trades, eq, ts);
    let sheet = LiveTearsheet::from_result(None, &r, DAILY_PERIODS_PER_YEAR);
    let ext = ExtendedMetrics::from_result(&r, DAILY_PERIODS_PER_YEAR);

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
    let measured = LiveTearsheet::from_result(None, &r, DAILY_PERIODS_PER_YEAR);
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
        BacktestReport::from_result(None, &result_of(trades, eq, ts), DAILY_PERIODS_PER_YEAR);
    report.extended = None;
    let sheet = LiveTearsheet::from_report(&report);

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
    let sheet = LiveTearsheet::from_result_parts(None, trades, eq, ts, DAILY_PERIODS_PER_YEAR);
    let s = sheet.to_string();
    assert!(!s.is_empty());
    assert!(s.contains("Live Tearsheet"));
    assert!(s.contains("(unnamed)"));
    assert!(s.contains(&format!("schema {TEARSHEET_SCHEMA}")));
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

    assert_eq!(obj["schema"], TEARSHEET_SCHEMA);
    assert_eq!(obj["name"], "sess-1");
    for m in METRICS {
        assert!(obj.contains_key(m.id), "{} is missing from the document", m.id);
    }
    assert_eq!(
        obj.len(),
        METRICS.len() + 2,
        "no key beyond `schema`, `name` and the catalog: {json}"
    );
    // No nesting: the flat shape is what `vike_tradehub_client::proto`'s `Response::Tearsheet`
    // carries and what `vike-cli report --json` passes through verbatim.
    assert!(obj.values().all(|v| !v.is_object()), "no key may nest: {json}");
}

/// ⚠ A `Count` metric stays an INTEGER on the wire. `serde_json::Value`'s `PartialEq` does not
/// consider `2.0` equal to `2`, so widening these to `f64` internally must not reach the bytes
/// — `crates/vike-report/tests/tearsheet_cli.rs` asserts `json["n_trades"] == 2`.
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
    let back: LiveTearsheet = serde_json::from_str(&json).expect("a writer's bytes parse");
    assert_eq!(back.schema, TEARSHEET_SCHEMA);
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
    let sheet: LiveTearsheet = serde_json::from_str(doc).expect("the document still parses");
    assert_eq!(sheet.schema, TEARSHEET_SCHEMA_UNVERSIONED, "no schema key -> unversioned");
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
            "{} collides with a LiveTearsheet field and would be shadowed by flatten",
            m.id
        );
    }
}

/// An id the catalog does not hold cannot be written into the document — the property that
/// keeps the key set equal to the published roster.
#[test]
fn setting_an_uncatalogued_id_writes_nothing() {
    let mut v = MetricValues::unrecorded();
    assert!(!v.set("shrapnel", Some(1.0)), "an unknown id is refused");
    assert!(v.set("sharpe", Some(1.0)), "a catalogued one is written");
    assert_eq!(v.get("sharpe"), Some(1.0));
    assert_eq!(v.get("shrapnel"), None);
}
