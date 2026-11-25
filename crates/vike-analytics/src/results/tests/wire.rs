//! The `report.json` wire: serialization, read-back, the `inf` sentinel and old documents.

use super::*;

#[test]
fn json_round_trips() {
    let r = sample_result();
    let report = TradingResults::from_record(Some("demo".to_string()), &r, 252.0);

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
/// would emit `null` for a non-finite float anyway (see [`vike_trade_ledger::serialize_f64_null_when_nonfinite`]) — the
/// point is that the shape is `null`, never a `NaN`/`inf` token and never an error.
#[test]
fn nonfinite_profit_factor_serializes_as_null() {
    let mut r = sample_result();
    r.trades = vec![trade(10.0)]; // wins only -> profit_factor = inf
    let report = TradingResults::from_record(None, &r, 252.0);
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
    let written = TradingResults {
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
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: Some(60_000),
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    };

    let json = serde_json::to_string(&written).unwrap();
    let back: TradingResults = serde_json::from_str(&json).unwrap();
    assert_eq!(back.returns_interval_ms, Some(60_000), "a tick report's step reads back");

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

/// What a run found in its data and ran anyway reaches `report.json`, the wire and the human table;
/// a clean run's three are the bytes they were, down to the absent key and the absent line.
#[test]
fn data_warnings_reach_every_rendering_and_a_clean_report_has_none_of_it() {
    let r = TradingRecord {
        data_warnings: vec!["ETHUSDT: did not run".to_string()],
        ..sample_result()
    };
    let warned = TradingResults::from_record(None, &r, 252.0);
    let json = serde_json::to_string(&warned).unwrap();
    assert!(json.contains("\"data_warnings\":[\"ETHUSDT: did not run\"]"), "{json}");
    let back: TradingResults = serde_json::from_str(&json).unwrap();
    assert_eq!(back.data_warnings, warned.data_warnings);
    assert!(warned.to_string().contains("data_warnings:\n  ETHUSDT: did not run\n"), "{warned}");

    let clean = TradingResults::from_record(None, &sample_result(), 252.0);
    assert!(!serde_json::to_string(&clean).unwrap().contains("data_warnings"));
    assert!(!clean.to_string().contains("data_warnings"), "{clean}");
    let old: TradingResults =
        serde_json::from_str(&serde_json::to_string(&clean).unwrap()).unwrap();
    assert!(old.data_warnings.is_empty(), "a report written before the key reads as none");
}

/// The house `inf` sentinel survives the round trip EXACTLY, and it can: `metrics::profit_factor`
/// answers `f64::INFINITY` when there are no losing trades and some profit, `0.0` when there is
/// neither — and `0.0` is finite, so it serializes as `0.0`. `INFINITY` is the ONLY value that
/// ever becomes `null`, which is what makes `null -> INFINITY` a mapping rather than a guess.
#[test]
fn an_infinite_profit_factor_round_trips_through_its_json_null() {
    let written = TradingResults { profit_factor: f64::INFINITY, ..a_finite_report() };

    let json = serde_json::to_string(&written).unwrap();
    assert!(json.contains("\"profit_factor\":null"), "the wire shape must not change: {json}");

    let back: TradingResults = serde_json::from_str(&json).unwrap();
    assert!(back.profit_factor.is_infinite() && back.profit_factor.is_sign_positive());
}

/// A curve that gained and never drew down answers `inf` for four extended metrics, and the report
/// still reads back: each crosses as `null` and returns as the same sentinel, where a bare `f64`
/// refused the whole document (a walk-forward step of a rising market is such a curve).
#[test]
fn the_four_infinite_extended_metrics_round_trip_through_their_json_null() {
    let rising = TradingRecord {
        equity_curve: vec![1000.0, 1010.0, 1020.0, 1030.0],
        final_equity: 1030.0,
        ..Default::default()
    };
    let written = TradingResults::from_record(None, &rising, 252.0);
    let extended = written.extended.as_ref().expect("from_record composes the block");
    let infinite = [
        extended.calmar,
        extended.recovery_factor,
        extended.ulcer_performance_index,
        extended.omega,
    ];
    assert!(infinite.iter().all(|v| v.is_infinite()), "{infinite:?}");

    let json = serde_json::to_string(&written).unwrap();
    for key in ["calmar", "recovery_factor", "ulcer_performance_index", "omega"] {
        assert!(json.contains(&format!("\"{key}\":null")), "the bytes are serde_json's: {json}");
    }
    let back: TradingResults = serde_json::from_str(&json).expect("the document decodes");
    assert_eq!(back.extended, written.extended);
}

/// A zero-trade run's DIAGNOSIS is the half a reader most needs and the half that would have
/// been lost first: it is an `Option` skipped on serialization, so it exercises both the
/// `default` and the nested `Deserialize` this task adds.
#[test]
fn a_zero_trade_diagnosis_reads_back_with_its_ranked_causes() {
    let written = TradingResults {
        invalid: None,
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
    let back: TradingResults = serde_json::from_str(&json).unwrap();

    let causes = back.zero_trade.expect("the diagnosis must survive").causes;
    assert_eq!(causes.len(), 1);
    assert_eq!(causes[0].code, "no-data");
    assert_eq!(causes[0].headline, "the data slice was empty");
}

/// ⚠ **THE BACK-COMPATIBILITY PROOF for the SECOND persisted document**, and the reason the
/// four fields added LATER carry `#[serde(default)]` (and the seven originals do not).
///
/// Written as raw TEXT rather than through the serializer, deliberately and for the same reason
/// `vike_runs`'s manifest twin is: the bytes already on somebody's disk are what this
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

    let back: TradingResults =
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
    let back: TradingResults = serde_json::from_str(&oldest_report(None))
        .expect("the four LATER fields must default, or every old report.json stops loading");
    assert_eq!(back.profit_factor, 0.0, "absent is `no meaningful ratio`");
    assert_eq!(back.funding_paid, 0.0);
    assert!(back.per_symbol_pnl.is_empty());
    assert!(back.zero_trade.is_none());
    assert_eq!(back.final_equity, 1.0, "and what it DID carry is untouched");

    assert!(
        serde_json::from_str::<TradingResults>("{}").is_err(),
        "an EMPTY object must not deserialize into an all-zeros run a reader would compare"
    );
    assert!(
        serde_json::from_str::<TradingResults>(r#"{ "unrelated": 1 }"#).is_err(),
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
            serde_json::from_str::<TradingResults>(&without).is_err(),
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

    let back: TradingResults = serde_json::from_str(&doc).unwrap();

    assert!(back.profit_factor.is_infinite() && back.profit_factor.is_sign_positive());
}

/// The golden pair: a run that traded with every simulator counter non-zero, and a zero-trade run
/// whose diagnosis the counters drive.
fn golden_records() -> [TradingRecord; 2] {
    use crate::record::{TradingRecordSimulator, TradingRecordSource};
    let day = |n: i64| 1_767_225_600_000 + n * 86_400_000;
    let drop = |reason: &str, size: f64| vike_model::OrderDrop {
        symbol: "BTCUSDT".to_string(),
        reason: reason.to_string(),
        size,
        weight: 1.0,
    };
    let traded = TradingRecord {
        trades: vec![trade(10.0), trade(-5.0)],
        equity_curve: vec![1000.0, 1010.0, 990.0, 1005.0],
        final_equity: 1005.0,
        n_trades: 2,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 5.0)],
        equity_timestamp: (0..4).map(day).collect(),
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            intrabar_both_hit: 1,
            stale_deferrals: 2,
            impact_unpriced: 3,
            session_deferrals: 4,
            dropped: vec![drop("insufficient-margin", 0.5), drop("volume_cap", 2.0)],
            dropped_at: Vec::new(),
            below_min_reversals: 1,
            warmup: 5,
            maker_fills: 1,
            taker_fills: 3,
        }),
        funding_paid: -0.5,
        fees_paid: 0.25,
        data_warnings: vec!["ETHUSDT: did not run".to_string()],
        ..Default::default()
    };
    let zero = TradingRecord {
        equity_curve: vec![1000.0, 1000.0, 1000.0],
        final_equity: 1000.0,
        equity_timestamp: (0..3).map(day).collect(),
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            stale_deferrals: 1,
            dropped: vec![drop("insufficient-margin", 1.0); 3],
            warmup: 50,
            ..Default::default()
        }),
        ..Default::default()
    };
    [traded, zero]
}

/// ⚠ **`report.json`'s bytes for a Backtest record, pinned against the golden written BEFORE the
/// record named its source** (package 2, step 3: `report_golden.json` was printed by the tree before
/// it). The counters moved into `TradingRecordSimulator` and `TradingResults` gained `source` and
/// `period`, and a stored report does not move by a byte. Never re-bless it to pass.
///
/// ⚠ Re-pinned ONCE, 2026-11-19, for a change the owner ordered, not to pass: the skewness' cube left
/// `libm::pow` for `vike_math::powers::cube` (decision 0294's amendment of that day), and the traded
/// record's `returns_skewness` moved one ulp, `-1.5879576556248765` -> `-1.5879576556248767`. No other
/// byte moved.
///
/// ⚠ Re-pinned a second time, 2026-11-19, for another change the owner ordered (decision 0078's amendment on
/// the denials): each `honesty` block with denials gained its `denial_details` (a group's examples; these records
/// carry no times). No byte before it moved.
#[test]
fn backtest_report_golden() {
    let reports: Vec<TradingResults> = golden_records()
        .iter()
        .map(|r| TradingResults::from_record(Some("golden".to_string()), r, 252.0))
        .collect();
    let json = serde_json::to_string_pretty(&reports).unwrap() + "\n";
    if json != include_str!("report_golden.json") {
        println!("=== GOLDEN BEGIN ===\n{json}=== GOLDEN END ===");
        panic!("report.json bytes moved");
    }
}

/// Decision 0296: a report names the SDK its strategy file's artifact was built against, the number
/// reads back, and a report with no SDK (a `.rhai` file, a development tree's build) writes no key,
/// so its bytes are a report's from before the field (the golden above holds them).
#[test]
fn the_strategy_sdk_round_trips_and_an_unset_one_writes_no_key() {
    let report = TradingResults::from_record(None, &sample_result(), 252.0);
    let pinned = report.clone().with_strategy("trend/momentum.rs", &"ab".repeat(32), Some(3));
    let json = serde_json::to_string(&pinned).unwrap();
    assert!(json.contains(r#""strategy_sdk":3"#), "{json}");
    let back: TradingResults = serde_json::from_str(&json).unwrap();
    assert_eq!(back.strategy_sdk, Some(3));

    let unpinned = report.with_strategy("trend/momentum.rs", &"ab".repeat(32), None);
    let json = serde_json::to_string(&unpinned).unwrap();
    assert!(!json.contains("strategy_sdk"), "{json}");
    assert_eq!(serde_json::from_str::<TradingResults>(&json).unwrap().strategy_sdk, None);
}

/// **A long run's `report.json`, pinned by the FNV-1a of its bytes**: 20,000 steps whose returns TIE (every seventh
/// step is flat, so thousands of exact zeros) and sort into the tail metrics, 400 trades, and refusals whose reasons
/// repeat. Pinned against the tree BEFORE `metrics::ascending` sorted unstably (2026-11-19): `f64::total_cmp` calls two
/// values equal only when their bits are, so the order among equals shows in no byte. Never re-pin it to pass.
///
/// ⚠ Re-pinned ONCE, 2026-11-19, for a change the owner ordered (decision 0078's amendment on the denials): the 120
/// refusal sentences, each naming its quantity, are one row of `honesty.denials` now, its numbers lifted out into
/// `honesty.denial_details`. The pin before was `f35c358b19af9552`.
#[test]
fn a_long_runs_report_is_pinned() {
    use crate::record::{TradingRecordSimulator, TradingRecordSource};
    const STEPS: i64 = 20_000;
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut equity = 10_000.0;
    let equity_curve: Vec<f64> = (0..STEPS)
        .map(|i| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            if i % 7 != 0 {
                equity *= 1.0 + ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.006;
            }
            equity
        })
        .collect();
    let drop = |i: usize| vike_model::OrderDrop {
        symbol: "BTCUSDT".to_string(),
        reason: format!("cannot sell: it has 0, this sells 0.{:04}", i % 120),
        size: 1.0,
        weight: 1.0,
    };
    let record = TradingRecord {
        final_equity: *equity_curve.last().unwrap(),
        equity_timestamp: (0..STEPS).map(|i| 1_733_011_200_000 + i * 900_000).collect(),
        equity_curve,
        trades: (0..400).map(|i| trade(f64::from(i % 17) - 8.25)).collect(),
        n_trades: 400,
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            dropped: (0..300).map(drop).collect(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let report = TradingResults::from_record(Some("long".to_string()), &record, 35_040.0);
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(report.honesty.as_ref().map(|h| h.denials.len()), Some(1), "one reason");
    assert_eq!(vike_utility::fnv::fnv1a64_hex(json.as_bytes()), "51e6cb6879556e49");
}

/// **A portfolio's mount table** (`TradingResults::mounts`): copied off the record, on the wire and back (an infinite
/// profit factor as `null`), and printed one line a mount with its trades and win %, then what no mount holds. A run
/// that is no portfolio writes no `mounts` key and prints no table.
#[test]
fn a_portfolios_mount_table_crosses_the_wire_and_prints_trades_and_win_rate() {
    let row = |mount: usize, file: &str, n_trades: usize, win_rate: f64, profit_factor: f64| {
        super::super::TradingResultsMount {
            mount,
            strategy_file: file.to_string(),
            strategy_sha256: String::new(),
            strategy_sdk: None,
            symbols: vec!["BTCUSDT".to_string()],
            n_trades,
            win_rate,
            profit_factor,
            pnl: 2.5,
            max_leverage: Some(2.0),
            max_loss: None,
            leverage_refusals: 1,
            latched_at: None,
        }
    };
    let mounts = super::super::TradingResultsMounts {
        rows: vec![row(0, "a.rhai", 4, 0.75, 3.0), row(1, "b.rhai", 2, 1.0, f64::INFINITY)],
        unattributed_pnl: -0.5,
    };
    let record = TradingRecord { mounts: Some(mounts.clone()), ..sample_result() };
    let report = TradingResults::from_record(None, &record, 252.0);
    assert_eq!(report.mounts.as_ref(), Some(&mounts));
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("\"profit_factor\":null"), "{json}");
    let read: TradingResults = serde_json::from_str(&json).unwrap();
    assert_eq!(read.mounts, Some(mounts));
    let text = report.to_string();
    assert!(
        text.contains("mounts:\n  [0] a.rhai (BTCUSDT): n_trades 4, win_rate 75.0000%"),
        "{text}"
    );
    assert!(text.contains("  [1] b.rhai (BTCUSDT): n_trades 2, win_rate 100.0000%"), "{text}");
    assert!(text.contains("leverage_refusals 1, latched never"), "{text}");
    assert!(text.ends_with("  unattributed_pnl: -0.50\n"), "{text}");

    let plain = TradingResults::from_record(None, &sample_result(), 252.0);
    assert!(!serde_json::to_string(&plain).unwrap().contains("mounts"));
    assert!(!plain.to_string().contains("mounts:") && plain.mounts_table().is_none());
}
