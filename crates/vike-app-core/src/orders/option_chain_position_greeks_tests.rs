use super::*;
use vike_marketdata::{
    OptionChain, OptionChainExpiry, OptionChainStrike, OptionQuote, OptionUnderlyingKind,
};
use vike_model::ContractValue;

/// 2026-06-02 08:00 UTC — well before the 2026-06-27 test expiry, so t > 0.
const NOW: i64 = 1_780_387_200_000;

fn quote(strike: f64, kind: OptionKind, iv: f64, mark: f64, name: &str) -> OptionQuote {
    OptionQuote {
        iv: Some(iv),
        mark: Some(mark),
        instrument_name: Some(name.to_string()),
        ..OptionQuote::new(strike, kind)
    }
}

/// One BTC chain, expiry 2026-06-27, spot 104000, two strikes (100k + 110k) each with a
/// call+put carrying a known IV — the live-chain source the helper reads spot/IV from.
fn btc_bundle() -> OptionChainSet {
    let rows = vec![
        OptionChainStrike {
            strike: 100000.0,
            call: Some(quote(100000.0, OptionKind::Call, 0.625, 5720.0, "BTC-27JUN26-100000-C")),
            put: Some(quote(100000.0, OptionKind::Put, 0.61, 4680.0, "BTC-27JUN26-100000-P")),
        },
        OptionChainStrike {
            strike: 110000.0,
            call: Some(quote(110000.0, OptionKind::Call, 0.64, 2600.0, "BTC-27JUN26-110000-C")),
            put: None,
        },
    ];
    bundle("BTC", 104000.0, rows)
}

/// One deribit crypto chain of `underlying` at spot `spot`, expiry 2026-06-27, holding `rows`.
fn bundle(underlying: &str, spot: f64, rows: Vec<OptionChainStrike>) -> OptionChainSet {
    let chain = OptionChain {
        underlying: underlying.into(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(spot),
        expiry: OptionChainExpiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows,
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    OptionChainSet {
        default_expiry: "2026-06-27".into(),
        expiries: vec![OptionChainExpiry {
            date: "2026-06-27".into(),
            dte: 25,
            label: "27 Jun".into(),
        }],
        chains,
    }
}

fn books() -> BTreeMap<String, OptionChainSet> {
    let mut m = BTreeMap::new();
    m.insert("BTC".to_string(), btc_bundle());
    m
}

/// The option index every position here is looked up in: the coin-settled BTC/ETH options (on the
/// loaded chain, on an unloaded expiry, outside the strike window), the USDC-settled SOL put, and
/// the perpetuals and the dated future (delta-one legs). ⚠ `BTC-27JUN26-110000-C` is deliberately
/// NOT indexed although the chain quotes it — the absent-from-the-index case.
fn index() -> OptionIndex {
    use OptionKind::{Call, Put};
    use vike_options::test_support::{test_delta_one, test_option};
    OptionIndex::from_instruments(&[
        test_option("BTC-27JUN26-100000-C", "BTC", "2026-06-27", 100_000.0, Call, "BTC"),
        test_option("BTC-27JUN26-100000-P", "BTC", "2026-06-27", 100_000.0, Put, "BTC"),
        test_option("BTC-27JUN26-500000-C", "BTC", "2026-06-27", 500_000.0, Call, "BTC"),
        test_option("BTC-25SEP26-120000-C", "BTC", "2026-09-25", 120_000.0, Call, "BTC"),
        test_option("ETH-27JUN26-3000-C", "ETH", "2026-06-27", 3_000.0, Call, "ETH"),
        test_option("SOL_USDC-27JUN26-45-P", "SOL", "2026-06-27", 45.0, Put, "USDC"),
        test_delta_one("BTC-PERPETUAL", "BTC"),
        test_delta_one("ETH-PERPETUAL", "ETH"),
        test_delta_one("BTC-27JUN26", "BTC"),
    ])
}

#[test]
fn an_option_absent_from_the_index_is_unpriced_not_parsed() {
    // A well-formed option name the loaded chain QUOTES (with IV and spot) — but the index does
    // not hold it, so it is not priced from a parse of its name: a row with `None` greeks, surfaced
    // bare in `unpriced`, nothing folded, no panic.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26-110000-C".into(),
        quantity: 1.0,
        average_price: 2000.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows.len(), 1, "the position still shows");
    assert_eq!(rep.rows[0].delta, None);
    assert_eq!(rep.rows[0].mark, None, "not even the chain mark: the leg is unknown");
    assert_eq!(rep.net_delta, 0.0);
    assert_eq!(rep.net_usd.unpriced, vec!["BTC-27JUN26-110000-C".to_string()]);
    // An empty index (before the first fetch) treats every leg the same way.
    let none = position_greeks(&positions, &books(), &OptionIndex::default(), 0.0);
    assert_eq!(none.rows[0].delta, None);
    assert_eq!(none.net_usd.unpriced, vec!["BTC-27JUN26-110000-C".to_string()]);
}

#[test]
fn a_perp_absent_from_the_index_is_not_folded() {
    // A perp WITH a venue coin delta and a loaded BTC spot, but not in the index: its underlying is
    // not guessed from the name head, so it does not fold — surfaced bare.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC_USDC-PERPETUAL".into(),
        quantity: 1.0,
        average_price: 104_000.0,
        coin_delta: Some(1.0),
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows[0].delta, None);
    assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds");
    assert!(rep.net_usd.per_underlying.is_empty());
    assert_eq!(rep.net_usd.unpriced, vec!["BTC_USDC-PERPETUAL".to_string()]);
}

/// A one-strike SOL chain (the USDC book), expiry 2026-06-27, spot 75.64, premiums already USD.
fn sol_books() -> BTreeMap<String, OptionChainSet> {
    let chain = OptionChain {
        underlying: "SOL".into(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(75.64),
        expiry: OptionChainExpiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows: vec![OptionChainStrike {
            strike: 45.0,
            call: None,
            put: Some(quote(45.0, OptionKind::Put, 0.7115, 0.2477, "SOL_USDC-27JUN26-45-P")),
        }],
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    let mut m = BTreeMap::new();
    m.insert(
        "SOL".to_string(),
        OptionChainSet {
            default_expiry: "2026-06-27".into(),
            expiries: vec![OptionChainExpiry {
                date: "2026-06-27".into(),
                dte: 25,
                label: "27 Jun".into(),
            }],
            chains,
        },
    );
    m
}

#[test]
fn a_usdc_settled_option_prices_off_its_payload_underlying() {
    // `SOL_USDC-27JUN26-45-P`: the underlying is the payload's `SOL`, not the name's head
    // `SOL_USDC`, so the leg joins the SOL chain and is priced (a long put: negative delta), its
    // USD mark carried verbatim, and its delta folds under `SOL` at SOL's spot.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "SOL_USDC-27JUN26-45-P".into(),
        quantity: 10.0,
        average_price: 0.2,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &sol_books(), &index(), 0.0);
    let d = rep.rows[0].delta.expect("priced from the SOL chain");
    assert!(d < 0.0, "long put → negative delta, got {d}");
    assert_eq!(rep.rows[0].mark, Some(0.2477), "the chain's USD mark, unscaled");
    let sol = &rep.net_usd.per_underlying["SOL"];
    assert_eq!(sol.spot, 75.64);
    assert!((sol.delta_usd - d * 75.64).abs() < 1e-9);
    assert!(rep.net_usd.unpriced.is_empty());
}

#[test]
fn empty_positions_yield_empty_report() {
    let rep = position_greeks(&[], &books(), &index(), 0.0);
    assert!(rep.rows.is_empty());
    assert_eq!(rep.net_delta, 0.0);
    assert_eq!(rep.net_gamma, 0.0);
    assert_eq!(rep.net_vega, 0.0);
    assert_eq!(rep.net_theta, 0.0);
}

#[test]
fn long_call_has_positive_delta() {
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26-100000-C".into(),
        quantity: 2.0,
        average_price: 5000.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows.len(), 1);
    let d = rep.rows[0].delta.expect("priced from the loaded chain");
    assert!(d > 0.0, "long call → positive delta, got {d}");
    assert!(rep.rows[0].gamma.unwrap() > 0.0, "gamma always positive");
    assert!(rep.rows[0].vega.unwrap() > 0.0, "long vega positive");
    // net equals the single row exactly
    assert_eq!(rep.net_delta, d);
    // mark/uPnL carried through: (5720 - 5000) * 2
    assert_eq!(rep.rows[0].mark, Some(5720.0));
    assert_eq!(rep.rows[0].unrealized_pnl, Some((5720.0 - 5000.0) * 2.0));
}

/// The uPnL is valued through the position's contract value: an option of multiplier 100 (one contract
/// stands for 100 of the underlying) moves 100 times its premium, `(5720 − 5000) × 2 × 100`.
#[test]
fn unrealized_pnl_is_valued_through_the_contract_value() {
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26-100000-C".into(),
        quantity: 2.0,
        average_price: 5000.0,
        coin_delta: None,
        contract_value: ContractValue::linear(100.0),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows[0].unrealized_pnl, Some(144_000.0));
}

#[test]
fn long_put_has_negative_delta() {
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26-100000-P".into(),
        quantity: 1.0,
        average_price: 4000.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    let d = rep.rows[0].delta.expect("priced");
    assert!(d < 0.0, "long put → negative delta, got {d}");
}

#[test]
fn short_call_flips_delta_and_vega_sign() {
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26-100000-C".into(),
        quantity: -3.0,
        average_price: 5000.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert!(rep.rows[0].delta.unwrap() < 0.0, "short call → negative delta");
    assert!(rep.rows[0].vega.unwrap() < 0.0, "short → negative vega");
    assert!(rep.rows[0].gamma.unwrap() < 0.0, "short → negative gamma");
}

#[test]
fn net_sums_across_positions() {
    let positions = vec![
        OptionChainPositionGreeksInput {
            instrument: "BTC-27JUN26-100000-C".into(),
            quantity: 2.0,
            average_price: 5000.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
        OptionChainPositionGreeksInput {
            instrument: "BTC-27JUN26-100000-P".into(),
            quantity: 1.0,
            average_price: 4000.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
    ];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    let sum = rep.rows[0].delta.unwrap() + rep.rows[1].delta.unwrap();
    assert!((rep.net_delta - sum).abs() < 1e-12, "net delta = Σ row deltas");
    let sum_v = rep.rows[0].vega.unwrap() + rep.rows[1].vega.unwrap();
    assert!((rep.net_vega - sum_v).abs() < 1e-12);
}

#[test]
fn position_not_in_chain_gets_none_greeks_but_still_a_row() {
    // A different expiry (2026-09-25) that isn't loaded → row present, greeks None, no net.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-25SEP26-120000-C".into(),
        quantity: 1.0,
        average_price: 100.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows.len(), 1, "the position still shows");
    assert_eq!(rep.rows[0].delta, None);
    assert_eq!(rep.rows[0].gamma, None);
    assert_eq!(rep.rows[0].vega, None);
    assert_eq!(rep.rows[0].theta, None);
    assert_eq!(rep.net_delta, 0.0, "an unpriced position contributes nothing");
}

#[test]
fn unparseable_instrument_gets_none_greeks() {
    // Two non-option rows, both row-level `None` with NO effect on the USD net:
    //  (a) a recognizable perp (`BTC-PERPETUAL`) with NO venue coin delta → cannot fold, surfaced
    //      in `unpriced` with the residual reason (never folded as a guess);
    //  (b) a name the index does not hold (lowercase garbage) — surfaced bare.
    let positions = vec![
        OptionChainPositionGreeksInput {
            instrument: "BTC-PERPETUAL".into(),
            quantity: 1.0,
            average_price: 100.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
        OptionChainPositionGreeksInput {
            instrument: "not-an-instrument".into(),
            quantity: 1.0,
            average_price: 1.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
    ];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows.len(), 2, "both positions still show");
    assert_eq!(rep.rows[0].delta, None);
    assert_eq!(rep.rows[1].delta, None);
    // A perp with no venue coin delta must NOT move the USD net (never a qty-derived guess).
    assert_eq!(rep.net_usd.net_delta_usd, 0.0, "no coin delta means no fold");
    assert!(rep.net_usd.per_underlying.is_empty(), "nothing folds into per-underlying");
    assert_eq!(rep.net_delta, 0.0, "raw net unaffected too");
    // Both legs are surfaced (never silently dropped); the perp carries the residual reason.
    assert!(
        rep.net_usd.unpriced.iter().any(|s| s.contains("BTC-PERPETUAL")),
        "the perp is surfaced in unpriced"
    );
    assert!(
        rep.net_usd.unpriced.iter().any(|s| s.contains("not-an-instrument")),
        "the garbage name is surfaced in unpriced"
    );
    assert!(
        rep.net_usd.unpriced.iter().any(|s| s.contains(NON_OPTION_EXCLUDED_REASON)),
        "the perp's unpriced row carries a reason"
    );
}

// --- USD-normalized net (cross-underlying USD weighting; perp/future legs fold via coin delta) ---

/// One ETH chain, expiry 2026-06-27, spot 3000 (well away from BTC's 104000 — the whole point:
/// a 1-delta ETH option is a different dollar exposure than a 1-delta BTC one).
fn eth_bundle() -> OptionChainSet {
    let rows = vec![
        OptionChainStrike {
            strike: 3000.0,
            call: Some(quote(3000.0, OptionKind::Call, 0.70, 180.0, "ETH-27JUN26-3000-C")),
            put: Some(quote(3000.0, OptionKind::Put, 0.68, 150.0, "ETH-27JUN26-3000-P")),
        },
        OptionChainStrike {
            strike: 3200.0,
            call: Some(quote(3200.0, OptionKind::Call, 0.72, 90.0, "ETH-27JUN26-3200-C")),
            put: None,
        },
    ];
    bundle("ETH", 3000.0, rows)
}

fn books_btc_eth() -> BTreeMap<String, OptionChainSet> {
    let mut m = BTreeMap::new();
    m.insert("BTC".to_string(), btc_bundle());
    m.insert("ETH".to_string(), eth_bundle());
    m
}

#[test]
fn net_delta_usd_is_spot_weighted_across_underlyings() {
    // A BTC option + an ETH option: their raw per-unit deltas must NOT add as bare units — the
    // net is SPOT-weighted USD (BTC 104000, ETH 3000).
    let positions = vec![
        OptionChainPositionGreeksInput {
            instrument: "BTC-27JUN26-100000-C".into(),
            quantity: 1.0,
            average_price: 5000.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
        OptionChainPositionGreeksInput {
            instrument: "ETH-27JUN26-3000-C".into(),
            quantity: 1.0,
            average_price: 100.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
    ];
    let rep = position_greeks(&positions, &books_btc_eth(), &index(), 0.0);
    let d_btc = rep.rows[0].delta.expect("BTC leg priced"); // = bs_delta × qty (units)
    let d_eth = rep.rows[1].delta.expect("ETH leg priced");
    // Each leg's delta-USD = its unit delta × its own underlying spot; the total is their sum.
    let expected = d_btc * 104_000.0 + d_eth * 3_000.0;
    assert!((rep.net_usd.net_delta_usd - expected).abs() < 1e-6, "USD net = Σ unit-delta × spot");
    // ...which is emphatically NOT the naive unit sum (that a 1Δ BTC + 1Δ ETH → 2.0 would give).
    let naive_units = d_btc + d_eth;
    assert!(
        (rep.net_usd.net_delta_usd - naive_units).abs() > 1.0,
        "spot-weighted USD net must differ from the dimensionless unit sum"
    );
    // Per-underlying breakdown carries units, USD, and the spot used.
    let btc = &rep.net_usd.per_underlying["BTC"];
    assert!((btc.delta_units - d_btc).abs() < 1e-12);
    assert!((btc.delta_usd - d_btc * 104_000.0).abs() < 1e-6);
    assert_eq!(btc.spot, 104_000.0);
    let eth = &rep.net_usd.per_underlying["ETH"];
    assert!((eth.delta_usd - d_eth * 3_000.0).abs() < 1e-6);
    assert_eq!(eth.spot, 3_000.0);
}

#[test]
fn old_net_fields_stay_naive_per_unit_sum_byte_identical() {
    // The OFF/byte-identical guard: the RETAINED raw net_* fields must remain the naive Σ of
    // the row greeks (byte-identical to pre-USD behavior); only the additive net_usd carries
    // the corrected, spot-weighted total.
    let positions = vec![
        OptionChainPositionGreeksInput {
            instrument: "BTC-27JUN26-100000-C".into(),
            quantity: 1.0,
            average_price: 5000.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
        OptionChainPositionGreeksInput {
            instrument: "ETH-27JUN26-3000-C".into(),
            quantity: 1.0,
            average_price: 100.0,
            coin_delta: None,
            contract_value: ContractValue::default(),
        },
    ];
    let rep = position_greeks(&positions, &books_btc_eth(), &index(), 0.0);
    let d_btc = rep.rows[0].delta.unwrap();
    let d_eth = rep.rows[1].delta.unwrap();
    let v_btc = rep.rows[0].vega.unwrap();
    let v_eth = rep.rows[1].vega.unwrap();
    // Raw fields = naive per-unit sums, exactly as before the USD net existed.
    assert!((rep.net_delta - (d_btc + d_eth)).abs() < 1e-12, "raw net_delta unchanged");
    assert!((rep.net_vega - (v_btc + v_eth)).abs() < 1e-12, "raw net_vega unchanged");
    // vega/theta are already dollar-denominated → net_usd mirrors the raw sums bit-for-bit.
    assert_eq!(rep.net_usd.net_vega, rep.net_vega);
    assert_eq!(rep.net_usd.net_theta, rep.net_theta);
    // The corrected delta net differs from the raw dimensionless unit sum.
    assert!((rep.net_delta - rep.net_usd.net_delta_usd).abs() > 1.0);
}

#[test]
fn perp_with_coin_delta_folds_into_net_delta_usd() {
    // A LONG BTC perp: signed `quantity` is USD NOTIONAL (52_000, NOT coin units), but the venue's
    // per-position coin delta is +0.5 — the ONLY value the fold trusts. It contributes
    // coin_delta × spot = 0.5 × 104_000 into net_delta_usd; Γ/ν/Θ stay untouched.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-PERPETUAL".into(),
        quantity: 52_000.0, // USD notional — DELIBERATELY not the coin delta
        average_price: 104_000.0,
        coin_delta: Some(0.5),
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows.len(), 1);
    // The per-position row shows the linear delta; Γ/ν/Θ are None (a linear leg has none).
    assert_eq!(rep.rows[0].delta, Some(0.5));
    assert_eq!(rep.rows[0].gamma, None);
    assert_eq!(rep.rows[0].vega, None);
    assert_eq!(rep.rows[0].theta, None);
    // USD net = coin_delta × spot = 0.5 × 104_000 (NOT qty-derived).
    assert!((rep.net_usd.net_delta_usd - 0.5 * 104_000.0).abs() < 1e-6);
    let btc = &rep.net_usd.per_underlying["BTC"];
    assert!((btc.delta_units - 0.5).abs() < 1e-12);
    assert!((btc.delta_usd - 0.5 * 104_000.0).abs() < 1e-6);
    assert_eq!(btc.spot, 104_000.0);
    // The raw per-unit net_delta stays OPTION-ONLY (a perp never touches it).
    assert_eq!(rep.net_delta, 0.0, "raw net_delta is option-only");
    // A folded perp is NOT surfaced in unpriced.
    assert!(rep.net_usd.unpriced.is_empty());
}

#[test]
fn short_perp_reduces_the_option_usd_net() {
    // A long call (positive USD delta) hedged by a SHORT BTC perp (negative coin delta): the
    // perp now FOLDS (Wave 5d), so net_delta_usd = call_usd + coin_delta × spot, strictly LESS
    // than the call alone.
    let call = OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26-100000-C".into(),
        quantity: 2.0,
        average_price: 5000.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    };
    let call_only = position_greeks(std::slice::from_ref(&call), &books(), &index(), 0.0);
    let long_usd = call_only.net_usd.net_delta_usd;
    assert!(long_usd > 0.0, "a long call is long delta-USD");
    let short_perp = OptionChainPositionGreeksInput {
        instrument: "BTC-PERPETUAL".into(),
        quantity: -104_000.0,
        average_price: 104_000.0,
        coin_delta: Some(-1.0),
        contract_value: ContractValue::default(),
    };
    let hedged = position_greeks(&[call, short_perp], &books(), &index(), 0.0);
    let expected = long_usd - 104_000.0; // short 1 coin (coin_delta -1) × 104k spot
    assert!(
        (hedged.net_usd.net_delta_usd - expected).abs() < 1e-6,
        "net_delta_usd = call_usd + coin_delta × spot"
    );
    assert!(hedged.net_usd.net_delta_usd < long_usd, "the short perp reduces the net");
    // BTC per-underlying folds BOTH the call's coin delta and the perp's coin delta.
    let call_units = hedged.rows[0].delta.unwrap();
    let btc = &hedged.net_usd.per_underlying["BTC"];
    assert!((btc.delta_units - (call_units - 1.0)).abs() < 1e-12, "perp coin delta folded");
    // The perp is priced → NOT surfaced in unpriced.
    assert!(hedged.net_usd.unpriced.is_empty(), "a folded perp is not in unpriced");
}

#[test]
fn perp_without_coin_delta_stays_unpriced() {
    // No venue coin delta (coin_delta None) → the perp CANNOT be folded even though BTC spot is
    // loaded: it stays in unpriced with the reason, and the net is unchanged.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-PERPETUAL".into(),
        quantity: -104_000.0,
        average_price: 104_000.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows[0].delta, None, "no coin delta → no row delta");
    assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds");
    assert!(rep.net_usd.per_underlying.is_empty());
    assert!(rep.net_usd.unpriced.iter().any(|s| s.contains("BTC-PERPETUAL")));
    assert!(rep.net_usd.unpriced.iter().any(|s| s.contains(NON_OPTION_EXCLUDED_REASON)));
}

#[test]
fn out_of_window_option_is_flagged_unpriced_not_dropped() {
    // Strike 500000 is on a LOADED expiry (2026-06-27) but outside the fetched strike rows
    // (100k/110k) — the ±window limitation. It must be SURFACED, not silently dropped (bug #3).
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26-500000-C".into(),
        quantity: 1.0,
        average_price: 10.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows.len(), 1, "the position still shows");
    assert_eq!(rep.rows[0].delta, None, "per-position greeks stay unpriced");
    assert_eq!(rep.net_delta, 0.0, "raw net unchanged");
    assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds into the USD net");
    assert!(rep.net_usd.per_underlying.is_empty());
    assert!(
        rep.net_usd.unpriced.contains(&"BTC-27JUN26-500000-C".to_string()),
        "the out-of-window leg is surfaced in net_usd.unpriced, not silently dropped"
    );
}

#[test]
fn perp_with_coin_delta_but_no_loaded_spot_stays_unpriced() {
    // An ETH perp WITH a venue coin delta, but books() is BTC-only so ETH's spot never loads →
    // the fold has no spot to convert with, so it stays unpriced (never folded as a guess). The
    // "no spot" residual, distinct from the "no delta" residual above.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "ETH-PERPETUAL".into(),
        quantity: 3_000.0,
        average_price: 3_000.0,
        coin_delta: Some(2.0),
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows[0].delta, None, "no loaded spot → cannot fold → no row delta");
    assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds without a spot");
    assert!(rep.net_usd.per_underlying.is_empty());
    assert!(rep.net_usd.unpriced.iter().any(|s| s.contains("ETH-PERPETUAL")));
    assert!(rep.net_usd.unpriced.iter().any(|s| s.contains(NON_OPTION_EXCLUDED_REASON)));
}

#[test]
fn future_leg_with_coin_delta_folds_like_a_perp() {
    // A dated FUTURE (BTC-27JUN26 — indexed as a future, no option address) folds its venue
    // coin delta exactly like a perp: coin_delta × spot into net_delta_usd, Γ/ν/Θ untouched.
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: "BTC-27JUN26".into(),
        quantity: 300_000.0, // USD notional
        average_price: 104_000.0,
        coin_delta: Some(3.0),
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books(), &index(), 0.0);
    assert_eq!(rep.rows[0].delta, Some(3.0), "future shows its linear delta");
    assert_eq!(rep.rows[0].gamma, None, "linear leg, no gamma");
    assert!((rep.net_usd.net_delta_usd - 3.0 * 104_000.0).abs() < 1e-6);
    assert!((rep.net_usd.per_underlying["BTC"].delta_units - 3.0).abs() < 1e-12);
    assert!(rep.net_usd.unpriced.is_empty(), "a folded future is not in unpriced");
}

/// **A SOL (USDC-book) position prices once the index holds the catalog's USDC row.** The row is
/// built by hand exactly as `crates/bridges/deribit/tests/fixtures/deribit_get_instruments.json`
/// publishes `SOL_USDC-12OCT26-96-P` (linear, settled in USDC, base SOL, strike 96, a put expiring
/// 2026-10-12 08:00 UTC), so this crate needs no venue edge to prove it. The position asks the
/// options poll for that expiry of SOL, and the answered chain prices it with the USD mark verbatim.
#[test]
fn a_sol_usdc_position_prices_once_the_index_holds_the_usdc_row() {
    let row = vike_model::Instrument::new(
        "deribit",
        "SOL_USDC-12OCT26-96-P",
        "SOL",
        "USDC",
        vike_model::Contract::Option(vike_model::ContractOption {
            right: OptionKind::Put,
            strike: 96.0,
            expiry_ms: 1_791_792_000_000,
            style: vike_model::ContractOptionExerciseStyle::European,
            margining: vike_model::Margining::Linear,
            settle: "USDC".into(),
            value_currency: Some("SOL".into()),
            underlying: Some("SOL".into()),
        }),
    );
    let index = OptionIndex::from_instruments(&[row]);
    let name = "SOL_USDC-12OCT26-96-P";
    // That the position ASKS the poll for its own expiry of SOL is the poll's half:
    // `crates/vike-datahub-client/src/tool_feeds/poll/options_tests.rs`'s
    // `a_usdc_position_asks_for_its_own_expiry_of_its_base`.

    let expiry = OptionChainExpiry { date: "2026-10-12".into(), dte: 132, label: "12 Oct".into() };
    let chain = OptionChain {
        underlying: "SOL".into(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(100.0),
        expiry: expiry.clone(),
        asof_ms: NOW,
        source: "deribit".into(),
        rows: vec![OptionChainStrike {
            strike: 96.0,
            call: None,
            put: Some(quote(96.0, OptionKind::Put, 0.65, 7.5, name)),
        }],
    };
    let books = BTreeMap::from([(
        "SOL".to_string(),
        OptionChainSet {
            default_expiry: "2026-10-12".into(),
            expiries: vec![expiry],
            chains: BTreeMap::from([("2026-10-12".to_string(), chain)]),
        },
    )]);
    let positions = vec![OptionChainPositionGreeksInput {
        instrument: name.into(),
        quantity: 10.0,
        average_price: 7.0,
        coin_delta: None,
        contract_value: ContractValue::default(),
    }];
    let rep = position_greeks(&positions, &books, &index, 0.0);
    let delta = rep.rows[0].delta.expect("priced from the SOL chain");
    assert!(delta < 0.0, "a long put has negative delta, got {delta}");
    assert_eq!(rep.rows[0].mark, Some(7.5), "a USDC premium is already USD");
    assert!(rep.net_usd.unpriced.is_empty(), "nothing is left unpriced");
}

/// **The move into `vike_options::position` changed no bit.** A mixed book (a long BTC call, a short
/// BTC put, a long ETH call, a short BTC perpetual with a venue coin delta and an unindexed name)
/// against the arithmetic this module did before the summing core moved, spelled out here: each row
/// `bs_greek × quantity`, the raw nets the naive fold of those in position order, each underlying's units
/// the fold of option deltas and coin deltas, its USD `units × spot`, and the USD total the fold over
/// the underlyings in name order.
#[test]
fn the_report_is_the_pre_move_arithmetic_bit_for_bit() {
    let leg =
        |instrument: &str, quantity: f64, coin_delta: Option<f64>| OptionChainPositionGreeksInput {
            instrument: instrument.into(),
            quantity,
            average_price: 1.0,
            coin_delta,
            contract_value: ContractValue::default(),
        };
    let positions = vec![
        leg("BTC-27JUN26-100000-C", 2.0, None),
        leg("BTC-27JUN26-100000-P", -1.5, None),
        leg("ETH-27JUN26-3000-C", 3.0, None),
        leg("BTC-PERPETUAL", -52_000.0, Some(-0.5)),
        leg("not-an-instrument", 1.0, None),
    ];
    let rep = position_greeks(&positions, &books_btc_eth(), &index(), 0.0);

    let t = years_to_expiry("2026-06-27", NOW);
    let unit = |s: f64, k: f64, iv: f64, kind: OptionKind| {
        vike_options::black_scholes_greeks(s, k, t, iv, kind, 0.0).unwrap()
    };
    let legs = [
        (unit(104_000.0, 100_000.0, 0.625, OptionKind::Call), 2.0),
        (unit(104_000.0, 100_000.0, 0.61, OptionKind::Put), -1.5),
        (unit(3_000.0, 3_000.0, 0.70, OptionKind::Call), 3.0),
    ];
    let (mut delta, mut gamma, mut theta, mut vega) = (0.0, 0.0, 0.0, 0.0);
    for (row, &((d, g, th, v), quantity)) in rep.rows.iter().zip(&legs) {
        assert_eq!(row.delta.map(f64::to_bits), Some((d * quantity).to_bits()));
        assert_eq!(row.gamma.map(f64::to_bits), Some((g * quantity).to_bits()));
        assert_eq!(row.theta.map(f64::to_bits), Some((th * quantity).to_bits()));
        assert_eq!(row.vega.map(f64::to_bits), Some((v * quantity).to_bits()));
        delta += d * quantity;
        gamma += g * quantity;
        theta += th * quantity;
        vega += v * quantity;
    }
    assert_eq!(rep.rows[3].delta, Some(-0.5), "the perpetual's row is its coin delta");
    assert_eq!(rep.rows[4].delta, None);
    assert_eq!(
        [rep.net_delta, rep.net_gamma, rep.net_theta, rep.net_vega].map(f64::to_bits),
        [delta, gamma, theta, vega].map(f64::to_bits)
    );

    let mut btc_units = 0.0;
    btc_units += legs[0].0.0 * legs[0].1;
    btc_units += legs[1].0.0 * legs[1].1;
    btc_units += -0.5;
    let eth_units = legs[2].0.0 * legs[2].1;
    let (btc_usd, eth_usd) = (btc_units * 104_000.0, eth_units * 3_000.0);
    let net = &rep.net_usd;
    assert_eq!(net.per_underlying["BTC"].delta_units.to_bits(), btc_units.to_bits());
    assert_eq!(net.per_underlying["BTC"].delta_usd.to_bits(), btc_usd.to_bits());
    assert_eq!(net.per_underlying["ETH"].delta_usd.to_bits(), eth_usd.to_bits());
    assert_eq!(net.net_delta_usd.to_bits(), (btc_usd + eth_usd).to_bits());
    assert_eq!([net.net_vega, net.net_theta].map(f64::to_bits), [vega, theta].map(f64::to_bits));
    assert_eq!(net.unpriced, vec!["not-an-instrument".to_string()]);
}
