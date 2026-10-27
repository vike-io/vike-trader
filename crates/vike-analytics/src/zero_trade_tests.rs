use super::*;

fn inputs() -> ZeroTradeInputs {
    ZeroTradeInputs { steps: 100, ..Default::default() }
}

#[test]
fn empty_data_is_the_top_cause() {
    let got = rank_causes(&ZeroTradeInputs { steps: 0, ..Default::default() });
    assert_eq!(got[0].code, "no-data");
}

#[test]
fn empty_data_outranks_a_stale_signal() {
    // steps == 0 is a structural certainty (sentinel weight) — it must lead even if some other
    // counter is somehow non-zero.
    let got =
        rank_causes(&ZeroTradeInputs { steps: 0, stale_deferrals: 9_999, ..Default::default() });
    assert_eq!(got[0].code, "no-data", "empty data is the definitive cause");
}

#[test]
fn warmup_shortfall_fires_when_warmup_ge_steps() {
    let got = rank_causes(&ZeroTradeInputs { steps: 10, warmup: 50, ..Default::default() });
    assert_eq!(got[0].code, "warmup-shortfall");
    assert!(got[0].headline.contains("50"));
    assert!(got[0].headline.contains("10"));
}

#[test]
fn warmup_shortfall_does_not_fire_at_boundary_below() {
    // warmup < steps ⇒ the dispatch gate DID open ⇒ not a warm-up shortfall.
    let got = rank_causes(&ZeroTradeInputs { steps: 10, warmup: 9, ..Default::default() });
    assert!(got.iter().all(|c| c.code != "warmup-shortfall"));
    // nothing else applies -> the catch-all.
    assert_eq!(got[0].code, "no-orders");
}

#[test]
fn default_warmup_never_trips_the_shortfall() {
    // The default `Strategy::warmup() == 0` must never be read as a shortfall.
    let got = rank_causes(&ZeroTradeInputs { steps: 5, warmup: 0, ..Default::default() });
    assert!(got.iter().all(|c| c.code != "warmup-shortfall"));
}

#[test]
fn denials_are_the_flagship_cause_with_a_reason_breakdown() {
    let got = rank_causes(&ZeroTradeInputs {
        denials: vec![("insufficient-margin".into(), 12), ("below-min-qty".into(), 3)],
        ..inputs()
    });
    assert_eq!(got[0].code, "orders-denied");
    assert!(got[0].headline.contains("15"), "15 = 12 + 3 total denials: {}", got[0].headline);
    // reasons rendered biggest-first
    assert!(got[0].detail.contains("insufficient-margin (12)"));
    assert!(got[0].detail.contains("below-min-qty (3)"));
    let margin_at = got[0].detail.find("insufficient-margin").unwrap();
    let minqty_at = got[0].detail.find("below-min-qty").unwrap();
    assert!(margin_at < minqty_at, "reasons ordered by count desc");
}

#[test]
fn causes_rank_by_evidence_weight() {
    // stale (20) > denials (5) > session (2): the blocked-order signals rank by count.
    let got = rank_causes(&ZeroTradeInputs {
        stale_deferrals: 20,
        session_deferrals: 2,
        denials: vec![("insufficient-margin".into(), 5)],
        ..inputs()
    });
    let order: Vec<&str> = got.iter().map(|c| c.code.as_str()).collect();
    assert_eq!(order, vec!["stale-price", "orders-denied", "session-closed"]);
}

#[test]
fn ties_keep_push_order_denials_then_stale_then_session() {
    // Equal weights: a stable sort must keep the fixed push order.
    let got = rank_causes(&ZeroTradeInputs {
        stale_deferrals: 7,
        session_deferrals: 7,
        denials: vec![("insufficient-margin".into(), 7)],
        ..inputs()
    });
    let order: Vec<&str> = got.iter().map(|c| c.code.as_str()).collect();
    assert_eq!(order, vec!["orders-denied", "stale-price", "session-closed"]);
}

#[test]
fn fallback_no_orders_when_nothing_specific_applies() {
    let got = rank_causes(&inputs());
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].code, "no-orders");
    assert!(got[0].detail.contains("100"), "mentions the bar count");
    // the symbol-route footgun hint rides in the catch-all detail
    assert!(got[0].detail.contains("SYMBOL.VENUE"));
}

#[test]
fn aggregate_denials_counts_by_reason_first_seen_order() {
    let dropped = vec![
        ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 1.0, 0.0),
        ("BTCUSDT".to_string(), "below-min-qty".to_string(), 1.0, 0.0),
        ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 2.0, 0.0),
    ];
    let got = aggregate_denials(&dropped);
    assert_eq!(got, vec![("insufficient-margin".to_string(), 2), ("below-min-qty".to_string(), 1)]);
}

#[test]
fn analyze_returns_none_for_a_run_with_trades() {
    // The OFF path: a run that closed a trade is never diagnosed -> its report is unchanged.
    let r = BacktestResult {
        n_trades: 3,
        equity_curve: vec![1000.0, 1000.0, 1000.0],
        ..Default::default()
    };
    assert!(ZeroTradeReport::analyze(&r).is_none());
}

#[test]
fn analyze_returns_none_when_equity_moved() {
    // A buy-and-hold: 0 CLOSED trades but the marked equity moved -> NOT flagged.
    let r = BacktestResult {
        n_trades: 0,
        equity_curve: vec![1000.0, 1001.0, 1002.0],
        ..Default::default()
    };
    assert!(ZeroTradeReport::analyze(&r).is_none());
}

#[test]
fn analyze_fires_on_a_flat_zero_trade_run_with_denials() {
    let r = BacktestResult {
        n_trades: 0,
        equity_curve: vec![1000.0, 1000.0, 1000.0],
        dropped: vec![("BTCUSDT".to_string(), "insufficient-margin".to_string(), 1.0, 0.0)],
        ..Default::default()
    };
    let report = ZeroTradeReport::analyze(&r).expect("flat + zero trades must diagnose");
    assert_eq!(report.causes[0].code, "orders-denied");
}

#[test]
fn analyze_threads_the_warmup_field_through() {
    // Proves `BacktestResult::warmup` reaches the diagnosis: warmup (50) > the 10 flat steps.
    let r = BacktestResult {
        n_trades: 0,
        equity_curve: vec![1000.0; 10],
        warmup: 50,
        ..Default::default()
    };
    let report = ZeroTradeReport::analyze(&r).expect("flat + zero trades must diagnose");
    assert_eq!(report.causes[0].code, "warmup-shortfall");
}

#[test]
fn analyze_empty_curve_is_no_data() {
    let r = BacktestResult { n_trades: 0, equity_curve: Vec::new(), ..Default::default() };
    let report = ZeroTradeReport::analyze(&r).expect("empty curve is flat + zero trades");
    assert_eq!(report.causes[0].code, "no-data");
}

#[test]
fn equity_is_flat_recognizes_constant_and_empty_curves() {
    assert!(equity_is_flat(&[]));
    assert!(equity_is_flat(&[1000.0]));
    assert!(equity_is_flat(&[1000.0, 1000.0, 1000.0]));
    assert!(!equity_is_flat(&[1000.0, 1000.0, 1000.01]));
}

#[test]
fn display_numbers_the_ranked_causes() {
    let report = ZeroTradeReport {
        causes: vec![
            ZeroTradeCause {
                code: "orders-denied".to_string(),
                headline: "H1".to_string(),
                detail: "D1".to_string(),
            },
            ZeroTradeCause {
                code: "stale-price".to_string(),
                headline: "H2".to_string(),
                detail: "D2".to_string(),
            },
        ],
    };
    let s = report.to_string();
    assert!(s.contains("probable cause"));
    assert!(s.contains("[1] H1"));
    assert!(s.contains("[2] H2"));
    assert!(s.contains("D1"));
}

#[test]
fn report_serializes_causes_as_an_array() {
    let report = ZeroTradeReport::analyze(&BacktestResult {
        n_trades: 0,
        equity_curve: vec![1000.0, 1000.0],
        ..Default::default()
    })
    .unwrap();
    let json = serde_json::to_string(&report).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["causes"].is_array());
    assert_eq!(v["causes"][0]["code"], "no-orders");
}
