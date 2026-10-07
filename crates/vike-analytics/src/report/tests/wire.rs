//! The `report.json` wire: serialization, read-back, the `inf` sentinel and old documents.

use super::*;

#[test]
fn json_round_trips() {
    let r = sample_result();
    let report = BacktestReport::from_result(Some("demo".to_string()), &r, 252.0);

    let json = serde_json::to_string(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // serde_json's default (non-`float_roundtrip`) float parser is not always bit-exact on
    // the way back in — a documented serde_json limitation, not a report bug — so float
    // fields are compared with a tight relative tolerance (matches this crate's own
    // convention elsewhere: metrics.rs's module doc gates sqrt/pow-derived values at
    // ≤1e-12 relative for the same reason). Exact types (strings/counts) compare exactly.
    let approx = |got: f64, want: f64| {
        assert!((got - want).abs() <= want.abs() * 1e-9 + 1e-12, "got {got}, want {want}");
    };
    assert_eq!(parsed["name"], "demo");
    approx(parsed["final_equity"].as_f64().unwrap(), report.final_equity);
    assert_eq!(parsed["n_trades"], report.n_trades as u64);
    approx(parsed["win_rate"].as_f64().unwrap(), report.win_rate);
    approx(parsed["sharpe"].as_f64().unwrap(), report.sharpe);
    approx(parsed["max_drawdown"].as_f64().unwrap(), report.max_drawdown);
    approx(parsed["total_return"].as_f64().unwrap(), report.total_return);
    assert_eq!(parsed["per_symbol_pnl"][0][0], "BTCUSDT");
    approx(parsed["per_symbol_pnl"][0][1].as_f64().unwrap(), 5.0);
    approx(parsed["profit_factor"].as_f64().unwrap(), report.profit_factor);
    // 10/5 = 2.0
}

/// The house `inf` sentinel (`profit_factor` with no losing trades) serializes as `null`.
/// This pins the CONTRACT a `--json` consumer sees, not a failure mode it rescues: serde_json
/// would emit `null` for a non-finite float anyway (see [`ser_f64_null_when_nonfinite`]) — the
/// point is that the shape is `null`, never a `NaN`/`inf` token and never an error.
#[test]
fn nonfinite_profit_factor_serializes_as_null() {
    let mut r = sample_result();
    r.trades = vec![trade(10.0)]; // wins only -> profit_factor = inf
    let report = BacktestReport::from_result(None, &r, 252.0);
    assert!(report.profit_factor.is_infinite());

    let json = serde_json::to_string(&report).expect("inf sentinel must not break JSON");
    assert!(!json.contains("inf"), "no bare inf token (invalid JSON): {json}");
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["profit_factor"].is_null());
}

/// ⚠ **The round trip a stored run needs.** `report.json` has been a write-only document: this
/// type derived `Serialize` and nothing else, so a `show`, a `diff` or a `gate` verb could not
/// read back the numbers this binary itself wrote. Asserted through the SERIALIZED FORM rather
/// than through a struct clone, because the bytes on disk are what a reader holds.
#[test]
fn a_serialized_report_reads_back_into_the_same_values() {
    let written = BacktestReport {
        name: Some("sma cross".to_string()),
        final_equity: 100_009.8,
        total_return: 0.000098,
        n_trades: 3,
        win_rate: 0.6667,
        sharpe: 1.25,
        max_drawdown: 0.031,
        profit_factor: 2.5,
        funding_paid: -1.5,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.0)],
        zero_trade: None,
        extended: None,
        honesty: None,
        realism: None,
    };

    let json = serde_json::to_string(&written).unwrap();
    let back: BacktestReport = serde_json::from_str(&json).unwrap();

    assert_eq!(back.name.as_deref(), Some("sma cross"));
    assert_eq!(back.final_equity, 100_009.8);
    assert_eq!(back.total_return, 0.000098);
    assert_eq!(back.n_trades, 3);
    assert_eq!(back.win_rate, 0.6667);
    assert_eq!(back.sharpe, 1.25);
    assert_eq!(back.max_drawdown, 0.031);
    assert_eq!(back.profit_factor, 2.5);
    assert_eq!(back.funding_paid, -1.5);
    assert_eq!(back.per_symbol_pnl, vec![("BTCUSDT".to_string(), 10.0)]);
    assert!(back.zero_trade.is_none(), "an absent key must read as None, not fail the parse");
}

/// The house `inf` sentinel survives the round trip EXACTLY, and it can: `metrics::profit_factor`
/// answers `f64::INFINITY` when there are no losing trades and some profit, `0.0` when there is
/// neither — and `0.0` is finite, so it serializes as `0.0`. `INFINITY` is the ONLY value that
/// ever becomes `null`, which is what makes `null -> INFINITY` a mapping rather than a guess.
#[test]
fn an_infinite_profit_factor_round_trips_through_its_json_null() {
    let written = BacktestReport { profit_factor: f64::INFINITY, ..a_finite_report() };

    let json = serde_json::to_string(&written).unwrap();
    assert!(json.contains("\"profit_factor\":null"), "the wire shape must not change: {json}");

    let back: BacktestReport = serde_json::from_str(&json).unwrap();
    assert!(back.profit_factor.is_infinite() && back.profit_factor.is_sign_positive());
}

/// A zero-trade run's DIAGNOSIS is the half a reader most needs and the half that would have
/// been lost first: it is an `Option` skipped on serialization, so it exercises both the
/// `default` and the nested `Deserialize` this task adds.
#[test]
fn a_zero_trade_diagnosis_reads_back_with_its_ranked_causes() {
    let written = BacktestReport {
        zero_trade: Some(crate::zero_trade::ZeroTradeReport {
            causes: vec![crate::zero_trade::ZeroTradeCause {
                code: "no-data".to_string(),
                headline: "the data slice was empty".to_string(),
                detail: "check [data].from/to against what the store holds".to_string(),
            }],
        }),
        ..a_finite_report()
    };

    let json = serde_json::to_string(&written).unwrap();
    let back: BacktestReport = serde_json::from_str(&json).unwrap();

    let causes = back.zero_trade.expect("the diagnosis must survive").causes;
    assert_eq!(causes.len(), 1);
    assert_eq!(causes[0].code, "no-data");
    assert_eq!(causes[0].headline, "the data slice was empty");
}

/// ⚠ **THE BACK-COMPATIBILITY PROOF for the SECOND persisted document**, and the reason the
/// four fields added LATER carry `#[serde(default)]` (and the seven originals do not).
///
/// Written as raw TEXT rather than through the serializer, deliberately and for the same reason
/// `vike_model::runs`'s manifest twin is: the bytes already on somebody's disk are what this
/// test is about, and a round trip through the CURRENT struct can never see a field the old
/// writer did not emit. The document below is a `report.json` from before `profit_factor`,
/// `funding_paid`, `per_symbol_pnl` and `zero_trade` existed — the four this file's own field
/// docs record as added later.
#[test]
fn a_report_written_before_the_later_fields_existed_still_reads() {
    let old_on_disk = r#"{
  "name": "sma cross",
  "final_equity": 100009.8,
  "total_return": 0.000098,
  "n_trades": 3,
  "win_rate": 0.6667,
  "sharpe": 1.25,
  "max_drawdown": 0.031
}
"#;

    let back: BacktestReport =
        serde_json::from_str(old_on_disk).expect("an old report.json must still load");

    assert_eq!(back.name.as_deref(), Some("sma cross"));
    assert_eq!(back.final_equity, 100_009.8, "and every field it DID carry is untouched");
    assert_eq!(back.n_trades, 3);
    assert_eq!(back.sharpe, 1.25);
    assert_eq!(back.profit_factor, 0.0, "absent is `no meaningful ratio`, not a parse failure");
    assert_eq!(back.funding_paid, 0.0);
    assert!(back.per_symbol_pnl.is_empty());
    assert!(back.zero_trade.is_none());
}

/// ⚠ **THE RULE, from both sides: the four LATER fields default and the seven ORIGINALS do
/// not.** The first half is back-compatibility. The second is the only structural check that
/// the bytes are a report AT ALL — with everything optional, `{}` and any unrelated JSON object
/// deserialize into a real-looking all-zeros run, which `show`/`diff`/`gate` would render and
/// compare against a baseline. Pre-derive that was a parse error; it stays one.
#[test]
fn only_the_four_later_fields_default_and_a_document_that_is_not_a_report_is_refused() {
    let back: BacktestReport = serde_json::from_str(&oldest_report(None))
        .expect("the four LATER fields must default, or every old report.json stops loading");
    assert_eq!(back.profit_factor, 0.0, "absent is `no meaningful ratio`");
    assert_eq!(back.funding_paid, 0.0);
    assert!(back.per_symbol_pnl.is_empty());
    assert!(back.zero_trade.is_none());
    assert_eq!(back.final_equity, 1.0, "and what it DID carry is untouched");

    assert!(
        serde_json::from_str::<BacktestReport>("{}").is_err(),
        "an EMPTY object must not deserialize into an all-zeros run a reader would compare"
    );
    assert!(
        serde_json::from_str::<BacktestReport>(r#"{ "unrelated": 1 }"#).is_err(),
        "nor must an unrelated JSON object"
    );

    // Per ORIGINAL SCALAR, because one `default` slipping back in is exactly what this
    // catches.
    //
    // ⚠ `name` is EXCLUDED and that is serde's rule rather than this type's: a bare
    // `Option<T>` field is optional whatever attributes it carries, because serde's own
    // `missing_field` helper deserializes an absent key through a unit deserializer and
    // `Option` answers `None` to it. Removing `#[serde(default)]` from `name` therefore
    // changed nothing, which is exactly why asserting it here would pin a property this crate
    // does not control. The six scalars below are what make a non-report REFUSED, and they are
    // enough: `{}` fails on the first of them.
    for (key, _) in ORIGINAL_KEYS.iter().filter(|(k, _)| *k != "name") {
        let without = oldest_report(Some(key));
        assert!(
            serde_json::from_str::<BacktestReport>(&without).is_err(),
            "a report missing the ORIGINAL field `{key}` must be a PARSE FAILURE, not a \
                 silent zero:\n{without}"
        );
    }
}

/// The `inf` sentinel still routes through the custom deserializer when the key is PRESENT —
/// `default` and `deserialize_with` answer different questions and both are needed.
#[test]
fn a_present_null_profit_factor_is_still_the_infinity_sentinel() {
    // ⚠ A COMPLETE document, because the seven ORIGINAL fields are required now — a bare
    // `{ "profit_factor": null }` is correctly a parse failure, which is the whole point of
    // narrowing the defaults.
    let doc = oldest_report_with(None, &[("profit_factor", "null")]);

    let back: BacktestReport = serde_json::from_str(&doc).unwrap();

    assert!(back.profit_factor.is_infinite() && back.profit_factor.is_sign_positive());
}
