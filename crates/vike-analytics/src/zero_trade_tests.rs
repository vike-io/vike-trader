use super::*;
use crate::record::TradingRecordSource;

fn inputs() -> ZeroTradeInputs {
    ZeroTradeInputs { steps: 100, ..Default::default() }
}

/// The cause codes [`rank_causes`] puts out, in order, for `stale` stale-price deferrals,
/// `session` session-closed deferrals and `denied` insufficient-margin denials.
fn blocked_ranking(stale: u64, session: u64, denied: u64) -> Vec<String> {
    let got = rank_causes(&ZeroTradeInputs {
        stale_deferrals: stale,
        session_deferrals: session,
        denials: vec![("insufficient-margin".into(), denied)],
        ..inputs()
    });
    got.into_iter().map(|c| c.code).collect()
}

/// [`ZeroTradeReport::analyze`] of a run with `n_trades` closed trades over `equity_curve`.
fn analyze(n_trades: usize, equity_curve: Vec<f64>) -> Option<ZeroTradeReport> {
    ZeroTradeReport::analyze(&TradingRecord { n_trades, equity_curve, ..Default::default() })
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
    assert_eq!(blocked_ranking(20, 2, 5), ["stale-price", "orders-denied", "session-closed"]);
}

#[test]
fn ties_keep_push_order_denials_then_stale_then_session() {
    // Equal weights: a stable sort must keep the fixed push order.
    assert_eq!(blocked_ranking(7, 7, 7), ["orders-denied", "stale-price", "session-closed"]);
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
        OrderDrop {
            symbol: "BTCUSDT".to_string(),
            reason: "insufficient-margin".to_string(),
            size: 1.0,
            weight: 0.0,
        },
        OrderDrop {
            symbol: "BTCUSDT".to_string(),
            reason: "below-min-qty".to_string(),
            size: 1.0,
            weight: 0.0,
        },
        OrderDrop {
            symbol: "BTCUSDT".to_string(),
            reason: "insufficient-margin".to_string(),
            size: 2.0,
            weight: 0.0,
        },
    ];
    let got = aggregate_denials(&dropped);
    assert_eq!(got, vec![("insufficient-margin".to_string(), 2), ("below-min-qty".to_string(), 1)]);
}

/// **A reason's numbers are lifted out, its words kept** (decision 0078's amendment of 2026-11-19): a number that
/// stands apart becomes `#` and its own text a field, signed, fractional or with an exponent; a digit joined to a
/// name, a version, a word or an order tag stays in the words; a `#` of the reason's own (a tag's, `long#3` from
/// `Books::new_tag` in the simulator's busy-tag refusal) is written `##`; and the words with the numbers put back
/// ([`denial_sentence`]) are the sentence.
#[test]
fn a_reasons_numbers_are_lifted_out_and_its_words_kept() {
    let cases: [(&str, &str, &[&str]); 13] = [
        (
            "long#3 is busy: amend it, or cancel it first",
            "long##3 is busy: amend it, or cancel it first",
            &[],
        ),
        ("order #5 of long#3 at 7", "order ##5 of long##3 at #", &["7"]),
        (
            "5#x and # alone, -2 after ## and #-4",
            "5##x and ## alone, # after #### and ##-#",
            &["-2", "4"],
        ),
        ("a # at the end #", "a ## at the end ##", &[]),
        (
            "a spot account cannot sell more than it holds: it has 0, this sells 0.0629; nothing was sent",
            "a spot account cannot sell more than it holds: it has #, this sells #; nothing was sent",
            &["0", "0.0629"],
        ),
        ("it has -0.5 and 1e-5 and 2E+3.", "it has # and # and #.", &["-0.5", "1e-5", "2E+3"]),
        ("leg BTC-27MAR26-120000-C refused", "leg BTC-27MAR26-#-C refused", &["120000"]),
        ("on 2024-12-01 at 5", "on #-#-# at #", &["2024", "12", "01", "5"]),
        ("version 1.2.3 of x1 and v2", "version 1.2.3 of x1 and v2", &[]),
        ("5eth over 5 eth", "5eth over # eth", &["5"]),
        ("insufficient-margin", "insufficient-margin", &[]),
        ("", "", &[]),
        ("…7 µ8", "…# µ#", &["7", "8"]),
    ];
    for (reason, words, numbers) in cases {
        let (template, lifted) = denial_template(reason);
        let texts: Vec<&str> = lifted.iter().map(String::as_str).collect();
        assert_eq!((template.as_str(), texts.as_slice()), (words, numbers), "{reason:?}");
        assert_eq!(
            denial_sentence(&template, &lifted),
            reason,
            "the numbers put back are the sentence"
        );
    }
}

/// **A run's denials group by their words**: the counts sum to the drops' (no denial is lost), each group keeps the
/// first and last time it was refused at and its first examples whose numbers or symbol differ, and a group's
/// examples stop at [`DENIAL_EXAMPLES`].
#[test]
fn denials_group_by_their_words_with_times_and_examples() {
    let drop = |symbol: &str, reason: String| OrderDrop {
        symbol: symbol.to_string(),
        reason,
        size: 1.5,
        weight: 0.0,
    };
    let sells = |n: u32| format!("it has 0, this sells 0.{n:03}");
    let drops = vec![
        drop("BTCUSDT", sells(1)),
        drop("BTCUSDT", "insufficient-margin".to_string()),
        drop("BTCUSDT", sells(1)),
        drop("ETHUSDT", sells(1)),
        drop("BTCUSDT", sells(2)),
        drop("BTCUSDT", sells(3)),
    ];
    let at = [5_000, 6_000, 7_000, 8_000, 9_000, 4_000];
    let (groups, details) =
        group_denials(drops.iter().zip(at).map(|(drop, at)| (drop, 2, Some(at))));
    assert_eq!(
        groups,
        vec![("it has #, this sells #".to_string(), 10), ("insufficient-margin".to_string(), 2)]
    );
    let total: u64 = groups.iter().map(|(_, n)| n).sum();
    assert_eq!(total, 2 * drops.len() as u64, "every drop counted once");
    assert_eq!((details[0].first_ms, details[0].last_ms), (Some(4_000), Some(9_000)));
    let shown: Vec<(&str, &[String])> =
        details[0].examples.iter().map(|e| (e.symbol.as_str(), e.numbers.as_slice())).collect();
    let numbers = |n: &str| vec!["0".to_string(), n.to_string()];
    let (one, two) = (numbers("0.001"), numbers("0.002"));
    assert_eq!(shown, vec![("BTCUSDT", &one[..]), ("ETHUSDT", &one[..]), ("BTCUSDT", &two[..])]);
    assert_eq!(details[0].examples[0].at_ms, Some(5_000));
    assert_eq!(details[1].examples.len(), 1);
    assert_eq!(aggregate_denials(&drops)[0], ("it has #, this sells #".to_string(), 5));
}

#[test]
fn analyze_returns_none_for_a_run_with_trades() {
    // The OFF path: a run that closed a trade is never diagnosed -> its report is unchanged.
    assert!(analyze(3, vec![1000.0, 1000.0, 1000.0]).is_none());
}

#[test]
fn analyze_returns_none_when_equity_moved() {
    // A buy-and-hold: 0 CLOSED trades but the marked equity moved -> NOT flagged.
    assert!(analyze(0, vec![1000.0, 1001.0, 1002.0]).is_none());
}

#[test]
fn analyze_fires_on_a_flat_zero_trade_run_with_denials() {
    let r = TradingRecord {
        n_trades: 0,
        equity_curve: vec![1000.0, 1000.0, 1000.0],
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            dropped: vec![OrderDrop {
                symbol: "BTCUSDT".to_string(),
                reason: "insufficient-margin".to_string(),
                size: 1.0,
                weight: 0.0,
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let report = ZeroTradeReport::analyze(&r).expect("flat + zero trades must diagnose");
    assert_eq!(report.causes[0].code, "orders-denied");
}

#[test]
fn analyze_threads_the_warmup_field_through() {
    // Proves `TradingRecordSimulator::warmup` reaches the diagnosis: warmup (50) > the 10 flat steps.
    let r = TradingRecord {
        n_trades: 0,
        equity_curve: vec![1000.0; 10],
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            warmup: 50,
            ..Default::default()
        }),
        ..Default::default()
    };
    let report = ZeroTradeReport::analyze(&r).expect("flat + zero trades must diagnose");
    assert_eq!(report.causes[0].code, "warmup-shortfall");
}

#[test]
fn analyze_empty_curve_is_no_data() {
    let report = analyze(0, Vec::new()).expect("empty curve is flat + zero trades");
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
    let report = ZeroTradeReport::analyze(&TradingRecord {
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
