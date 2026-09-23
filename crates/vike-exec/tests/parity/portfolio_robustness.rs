//! portfolio-robustness (Ext 1-3): the mark-price last-known FLOOR, the mark-health
//! (missing/stale) queries, and the net-exposure helpers — exercised at the engine level over a
//! multi-position book. Mirrors `resolve_equity.rs`'s tests/ convention (RecordingClient,
//! `ExecutionEngine::new`).

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, PositionEntry, PriceCfg, PriceSource, RiskGate,
    RiskLimits,
};
// The position key's side element is the CLOSED-SET `PositionSide`, not a free string. Build it
// with the VARIANT, never `"long".into()`: `From<&str>` matches only the uppercase labels and
// folds everything else to `Both` (pinned by `engine_snapshot`'s
// `an_unrecognized_side_label_folds_to_both_not_a_fourth_key`), so a lowercase literal here would
// silently seed a BOTH-keyed position while reading as a long one.
use vike_model::events::PositionSide;

/// Fresh engine on venue "binance", primary symbol "BTCUSDT", multiplier 1 everywhere. Tests seed
/// whatever positions/prices they need.
fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    )
}

fn long_at(sz: f64, avg: f64) -> PositionEntry {
    PositionEntry { size: sz, avg_px: avg, ..Default::default() }
}

// --- Ext 1: last-known fallback floor (engine resolver path) ----------------------------------

#[test]
fn resolved_price_stale_floor_is_opt_in_and_fresh_is_inert() {
    let mut e = engine();
    e.account
        .positions
        .insert(("binance".into(), "BTC".into(), PositionSide::Long), long_at(1.0, 100.0));
    e.price_board.set_mark("binance", "BTC", 105.0, 1_000);
    e.now_ms = 100_000;

    // fresh (no freshness window): priced at the mark, whatever stale_fallback is — the inert case.
    assert_eq!(e.resolved_position_price("binance", "BTC", 1.0, &PriceCfg::default()), Some(105.0));
    let fresh_on = PriceCfg { stale_fallback: true, ..PriceCfg::default() };
    assert_eq!(e.resolved_position_price("binance", "BTC", 1.0, &fresh_on), Some(105.0));

    // window ages the mark out; fallback OFF → None (today's behavior, byte-identical).
    let aged = PriceCfg { mark_max_age_ms: Some(1), ..PriceCfg::default() };
    assert_eq!(e.resolved_position_price("binance", "BTC", 1.0, &aged), None);
    // fallback ON → the last-known mark, valued instead of dropped to zero.
    let aged_on = PriceCfg { stale_fallback: true, ..aged };
    assert_eq!(e.resolved_position_price("binance", "BTC", 1.0, &aged_on), Some(105.0));
}

#[test]
fn resolved_equity_uses_the_stale_floor_when_enabled() {
    // Equity of a stale-only position: 0 (Missing) with the floor off, real uPnL with it on.
    let mut e = engine();
    e.account
        .positions
        .insert(("binance".into(), "BTC".into(), PositionSide::Long), long_at(2.0, 100.0));
    e.price_board.set_mark("binance", "BTC", 110.0, 1_000);
    e.now_ms = 100_000;
    let aged = PriceCfg { mark_max_age_ms: Some(1), ..PriceCfg::default() };
    // floor off: stale mark ignored → equity == equity_all's silent-zero (seed only).
    assert_eq!(e.resolved_equity(1_000.0, &aged).to_bits(), 1_000.0_f64.to_bits());
    // floor on: (110-100)*2 = 20 unrealized on top of the seed.
    let aged_on = PriceCfg { stale_fallback: true, ..aged };
    assert_eq!(e.resolved_equity(1_000.0, &aged_on).to_bits(), 1_020.0_f64.to_bits());
}

// --- Ext 2: mark-health (missing / stale) queries ---------------------------------------------

#[test]
fn missing_and_stale_marks_bucket_open_positions() {
    let mut e = engine();
    e.now_ms = 100_000;
    for (sym, sz) in [("BTC", 1.0), ("ETH", 1.0), ("SOL", 1.0), ("FLAT", 0.0)] {
        e.account
            .positions
            .insert(("binance".into(), sym.into(), PositionSide::Long), long_at(sz, 100.0));
    }
    e.price_board.set_mark("binance", "BTC", 105.0, 99_000); // fresh under a 5s window (age 1000)
    e.price_board.set_mark("binance", "SOL", 20.0, 1_000); //  stale under a 5s window (age 99000)
    // ETH: no price at all → missing. FLAT: no price but zero size → excluded from both.

    let cfg = PriceCfg { mark_max_age_ms: Some(5_000), ..PriceCfg::default() };
    let missing = e.missing_marks(&cfg);
    assert_eq!(missing, vec![("binance".to_string(), "ETH".to_string(), "LONG".to_string())]);

    let stale = e.stale_marks(&cfg);
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].symbol, "SOL");
    assert_eq!(stale[0].source, PriceSource::Mark);
    assert_eq!(stale[0].age_ms, 99_000);

    // FLAT never surfaces (flat-leg exclusion), and the two sets are disjoint.
    assert!(!missing.iter().any(|(_, s, _)| s == "FLAT"));
    assert!(!stale.iter().any(|m| m.symbol == "FLAT" || m.symbol == "ETH"));

    // default cfg (no freshness windows): nothing is stale; only the truly-priceless ETH is missing.
    let dflt = PriceCfg::default();
    assert!(e.stale_marks(&dflt).is_empty());
    assert_eq!(
        e.missing_marks(&dflt),
        vec![("binance".to_string(), "ETH".to_string(), "LONG".to_string())]
    );
}

// --- Ext 3: net-exposure helpers --------------------------------------------------------------

#[test]
fn net_gross_and_per_symbol_exposure_are_resolver_priced() {
    let mut e = engine();
    e.now_ms = 10;
    e.account.positions.insert(("binance".into(), "BTC".into(), "BOTH".into()), long_at(2.0, 90.0));
    e.account
        .positions
        .insert(("binance".into(), "ETH".into(), "BOTH".into()), long_at(-3.0, 55.0));
    e.price_board.set_mark("binance", "BTC", 100.0, 1);
    e.price_board.set_mark("binance", "ETH", 50.0, 1);
    let cfg = PriceCfg::default();

    // BTC 2·100 = +200 ; ETH -3·50 = -150 → net +50, gross 350 (never nets long vs short).
    assert_eq!(e.net_exposure(&cfg).to_bits(), 50.0_f64.to_bits());
    assert_eq!(e.gross_exposure(&cfg).to_bits(), 350.0_f64.to_bits());
    assert_eq!(
        e.exposure_by_symbol(&cfg),
        vec![("BTC".to_string(), 2.0, 200.0), ("ETH".to_string(), -3.0, -150.0)]
    );
}

#[test]
fn exposure_counts_qty_but_zero_notional_for_unpriceable() {
    let mut e = engine();
    e.now_ms = 10;
    e.account
        .positions
        .insert(("binance".into(), "NOPX".into(), "BOTH".into()), long_at(4.0, 10.0));
    let cfg = PriceCfg::default();
    // no price anywhere → 0 notional, but the qty still reads.
    assert_eq!(e.net_exposure(&cfg), 0.0);
    assert_eq!(e.gross_exposure(&cfg), 0.0);
    assert_eq!(e.exposure_by_symbol(&cfg), vec![("NOPX".to_string(), 4.0, 0.0)]);
}
