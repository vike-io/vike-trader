use super::*;
use vike_options::{Expiry, OptionChain, OptionQuote, StrikeRow, UnderlyingKind};

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
fn btc_bundle() -> UnderlyingChains {
    let rows = vec![
        StrikeRow {
            strike: 100000.0,
            call: Some(quote(100000.0, OptionKind::Call, 0.625, 5720.0, "BTC-27JUN26-100000-C")),
            put: Some(quote(100000.0, OptionKind::Put, 0.61, 4680.0, "BTC-27JUN26-100000-P")),
        },
        StrikeRow {
            strike: 110000.0,
            call: Some(quote(110000.0, OptionKind::Call, 0.64, 2600.0, "BTC-27JUN26-110000-C")),
            put: None,
        },
    ];
    let chain = OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(104000.0),
        expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows,
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    UnderlyingChains {
        default_expiry: "2026-06-27".into(),
        expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
        chains,
    }
}

fn books() -> BTreeMap<String, UnderlyingChains> {
    let mut m = BTreeMap::new();
    m.insert("BTC".to_string(), btc_bundle());
    m
}

#[test]
fn empty_positions_yield_empty_report() {
    let rep = position_greeks(&[], &books(), 0.0);
    assert!(rep.rows.is_empty());
    assert_eq!(rep.net_delta, 0.0);
    assert_eq!(rep.net_gamma, 0.0);
    assert_eq!(rep.net_vega, 0.0);
    assert_eq!(rep.net_theta, 0.0);
}

#[test]
fn long_call_has_positive_delta() {
    let pos = vec![PositionViewLite {
        instrument: "BTC-27JUN26-100000-C".into(),
        qty: 2.0,
        avg_px: 5000.0,
        coin_delta: None,
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
    assert_eq!(rep.rows.len(), 1);
    let d = rep.rows[0].delta.expect("priced from the loaded chain");
    assert!(d > 0.0, "long call → positive delta, got {d}");
    assert!(rep.rows[0].gamma.unwrap() > 0.0, "gamma always positive");
    assert!(rep.rows[0].vega.unwrap() > 0.0, "long vega positive");
    // net equals the single row exactly
    assert_eq!(rep.net_delta, d);
    // mark/uPnL carried through: (5720 - 5000) * 2
    assert_eq!(rep.rows[0].mark, Some(5720.0));
    assert_eq!(rep.rows[0].upnl, Some((5720.0 - 5000.0) * 2.0));
}

#[test]
fn long_put_has_negative_delta() {
    let pos = vec![PositionViewLite {
        instrument: "BTC-27JUN26-100000-P".into(),
        qty: 1.0,
        avg_px: 4000.0,
        coin_delta: None,
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
    let d = rep.rows[0].delta.expect("priced");
    assert!(d < 0.0, "long put → negative delta, got {d}");
}

#[test]
fn short_call_flips_delta_and_vega_sign() {
    let pos = vec![PositionViewLite {
        instrument: "BTC-27JUN26-100000-C".into(),
        qty: -3.0,
        avg_px: 5000.0,
        coin_delta: None,
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
    assert!(rep.rows[0].delta.unwrap() < 0.0, "short call → negative delta");
    assert!(rep.rows[0].vega.unwrap() < 0.0, "short → negative vega");
    assert!(rep.rows[0].gamma.unwrap() < 0.0, "short → negative gamma");
}

#[test]
fn net_sums_across_positions() {
    let pos = vec![
        PositionViewLite {
            instrument: "BTC-27JUN26-100000-C".into(),
            qty: 2.0,
            avg_px: 5000.0,
            coin_delta: None,
        },
        PositionViewLite {
            instrument: "BTC-27JUN26-100000-P".into(),
            qty: 1.0,
            avg_px: 4000.0,
            coin_delta: None,
        },
    ];
    let rep = position_greeks(&pos, &books(), 0.0);
    let sum = rep.rows[0].delta.unwrap() + rep.rows[1].delta.unwrap();
    assert!((rep.net_delta - sum).abs() < 1e-12, "net delta = Σ row deltas");
    let sum_v = rep.rows[0].vega.unwrap() + rep.rows[1].vega.unwrap();
    assert!((rep.net_vega - sum_v).abs() < 1e-12);
}

#[test]
fn position_not_in_chain_gets_none_greeks_but_still_a_row() {
    // A different expiry (2026-09-25) that isn't loaded → row present, greeks None, no net.
    let pos = vec![PositionViewLite {
        instrument: "BTC-25SEP26-120000-C".into(),
        qty: 1.0,
        avg_px: 100.0,
        coin_delta: None,
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
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
    //  (b) a genuinely-unparseable name (lowercase garbage) — surfaced bare.
    let pos = vec![
        PositionViewLite {
            instrument: "BTC-PERPETUAL".into(),
            qty: 1.0,
            avg_px: 100.0,
            coin_delta: None,
        },
        PositionViewLite {
            instrument: "not-an-instrument".into(),
            qty: 1.0,
            avg_px: 1.0,
            coin_delta: None,
        },
    ];
    let rep = position_greeks(&pos, &books(), 0.0);
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
fn eth_bundle() -> UnderlyingChains {
    let rows = vec![
        StrikeRow {
            strike: 3000.0,
            call: Some(quote(3000.0, OptionKind::Call, 0.70, 180.0, "ETH-27JUN26-3000-C")),
            put: Some(quote(3000.0, OptionKind::Put, 0.68, 150.0, "ETH-27JUN26-3000-P")),
        },
        StrikeRow {
            strike: 3200.0,
            call: Some(quote(3200.0, OptionKind::Call, 0.72, 90.0, "ETH-27JUN26-3200-C")),
            put: None,
        },
    ];
    let chain = OptionChain {
        underlying: "ETH".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(3000.0),
        expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows,
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    UnderlyingChains {
        default_expiry: "2026-06-27".into(),
        expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
        chains,
    }
}

fn books_btc_eth() -> BTreeMap<String, UnderlyingChains> {
    let mut m = BTreeMap::new();
    m.insert("BTC".to_string(), btc_bundle());
    m.insert("ETH".to_string(), eth_bundle());
    m
}

#[test]
fn net_delta_usd_is_spot_weighted_across_underlyings() {
    // A BTC option + an ETH option: their raw per-unit deltas must NOT add as bare units — the
    // net is SPOT-weighted USD (BTC 104000, ETH 3000).
    let pos = vec![
        PositionViewLite {
            instrument: "BTC-27JUN26-100000-C".into(),
            qty: 1.0,
            avg_px: 5000.0,
            coin_delta: None,
        },
        PositionViewLite {
            instrument: "ETH-27JUN26-3000-C".into(),
            qty: 1.0,
            avg_px: 100.0,
            coin_delta: None,
        },
    ];
    let rep = position_greeks(&pos, &books_btc_eth(), 0.0);
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
    let pos = vec![
        PositionViewLite {
            instrument: "BTC-27JUN26-100000-C".into(),
            qty: 1.0,
            avg_px: 5000.0,
            coin_delta: None,
        },
        PositionViewLite {
            instrument: "ETH-27JUN26-3000-C".into(),
            qty: 1.0,
            avg_px: 100.0,
            coin_delta: None,
        },
    ];
    let rep = position_greeks(&pos, &books_btc_eth(), 0.0);
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
    // A LONG BTC perp: signed `qty` is USD NOTIONAL (52_000, NOT coin units), but the venue's
    // per-position coin delta is +0.5 — the ONLY value the fold trusts. It contributes
    // coin_delta × spot = 0.5 × 104_000 into net_delta_usd; Γ/ν/Θ stay untouched.
    let pos = vec![PositionViewLite {
        instrument: "BTC-PERPETUAL".into(),
        qty: 52_000.0, // USD notional — DELIBERATELY not the coin delta
        avg_px: 104_000.0,
        coin_delta: Some(0.5),
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
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
    let call = PositionViewLite {
        instrument: "BTC-27JUN26-100000-C".into(),
        qty: 2.0,
        avg_px: 5000.0,
        coin_delta: None,
    };
    let call_only = position_greeks(std::slice::from_ref(&call), &books(), 0.0);
    let long_usd = call_only.net_usd.net_delta_usd;
    assert!(long_usd > 0.0, "a long call is long delta-USD");
    let short_perp = PositionViewLite {
        instrument: "BTC-PERPETUAL".into(),
        qty: -104_000.0,
        avg_px: 104_000.0,
        coin_delta: Some(-1.0),
    };
    let hedged = position_greeks(&[call, short_perp], &books(), 0.0);
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
    let pos = vec![PositionViewLite {
        instrument: "BTC-PERPETUAL".into(),
        qty: -104_000.0,
        avg_px: 104_000.0,
        coin_delta: None,
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
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
    let pos = vec![PositionViewLite {
        instrument: "BTC-27JUN26-500000-C".into(),
        qty: 1.0,
        avg_px: 10.0,
        coin_delta: None,
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
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
    let pos = vec![PositionViewLite {
        instrument: "ETH-PERPETUAL".into(),
        qty: 3_000.0,
        avg_px: 3_000.0,
        coin_delta: Some(2.0),
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
    assert_eq!(rep.rows[0].delta, None, "no loaded spot → cannot fold → no row delta");
    assert_eq!(rep.net_usd.net_delta_usd, 0.0, "nothing folds without a spot");
    assert!(rep.net_usd.per_underlying.is_empty());
    assert!(rep.net_usd.unpriced.iter().any(|s| s.contains("ETH-PERPETUAL")));
    assert!(rep.net_usd.unpriced.iter().any(|s| s.contains(NON_OPTION_EXCLUDED_REASON)));
}

#[test]
fn future_leg_with_coin_delta_folds_like_a_perp() {
    // A dated FUTURE (BTC-27JUN26 — non-option, parse_instrument_name → None) folds its venue
    // coin delta exactly like a perp: coin_delta × spot into net_delta_usd, Γ/ν/Θ untouched.
    let pos = vec![PositionViewLite {
        instrument: "BTC-27JUN26".into(),
        qty: 300_000.0, // USD notional
        avg_px: 104_000.0,
        coin_delta: Some(3.0),
    }];
    let rep = position_greeks(&pos, &books(), 0.0);
    assert_eq!(rep.rows[0].delta, Some(3.0), "future shows its linear delta");
    assert_eq!(rep.rows[0].gamma, None, "linear leg, no gamma");
    assert!((rep.net_usd.net_delta_usd - 3.0 * 104_000.0).abs() < 1e-6);
    assert!((rep.net_usd.per_underlying["BTC"].delta_units - 3.0).abs() < 1e-12);
    assert!(rep.net_usd.unpriced.is_empty(), "a folded future is not in unpriced");
}
