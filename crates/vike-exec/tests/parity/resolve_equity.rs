//! PR-2 T2: `ExecutionEngine::resolve_equity` — mode-aware equity with every open position
//! priced through the PR-1 `PriceBoard` resolver. Mirrors the `price_board_wiring.rs`
//! integration-test convention (tests/ file, `RecordingClient`, `ExecutionEngine::new`).

use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, PositionEntry, PriceCfg, RiskGate, RiskLimits,
};

/// One-position engine: venue "sim", symbol "BTC", long `size` @ `avg_px`, Delta mode.
fn engine_with_position(size: f64, avg_px: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    e.account.positions.insert(
        ("sim".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size, avg_px, ..Default::default() },
    );
    e
}

#[test]
fn missing_price_is_byte_identical_to_equity_all() {
    // open position, NO board prices, NO marks.
    let e = engine_with_position(1.0, 100.0);
    let re = e.resolve_equity(1000.0, &PriceCfg::default());
    assert_eq!(re.missing, 1);
    assert_eq!(re.per_position[0].mark_source, None);
    assert_eq!(re.unrealized_total, 0.0);
    // bit-identical to the legacy law
    assert_eq!(re.equity.to_bits(), e.account.equity_all(1000.0).to_bits());
}

#[test]
fn quote_only_position_becomes_valued() {
    // position with NO mark but a bid/ask on the board -> resolver prices it, so equity is
    // NO LONGER the silent-zero it is under equity_all.
    let mut e = engine_with_position(1.0, 100.0);
    e.price_board.set_quote("sim", "BTC", 104.0, 106.0, 1);
    let re = e.resolve_equity(1000.0, &PriceCfg::default());
    assert_eq!(re.missing, 0);
    // long values at BID (conservative) -> unrealized (104-100)*1*mult
    assert!(re.per_position[0].mark_source.is_some());
    assert!(re.unrealized_total != 0.0);
    assert!(re.equity != e.account.equity_all(1000.0)); // the intended change
}

#[test]
fn mark_present_matches_equity_all() {
    // when a fresh mark exists, resolver returns it -> equity == equity_all exactly.
    let mut e = engine_with_position(1.0, 100.0);
    e.account.set_mark_from("sim", "BTC", 105.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("sim", "BTC", 105.0, 1);
    let re = e.resolve_equity(1000.0, &PriceCfg::default());
    assert_eq!(re.equity.to_bits(), e.account.equity_all(1000.0).to_bits());
}

#[test]
fn resolved_equity_scalar_is_bit_identical_to_resolve_equity() {
    // The one-price law's decision-path entry point: the scalar `resolved_equity` must be
    // bit-identical to the display path's `resolve_equity().equity` in every board state
    // (all-Missing, quote-only, fresh mark) — the SAME py_sum value sequence, no Vec.
    let cfg = PriceCfg::default();
    let mut e = engine_with_position(1.0, 100.0);
    // all-Missing: also bit-identical to the legacy mark-only law
    assert_eq!(
        e.resolved_equity(1000.0, &cfg).to_bits(),
        e.resolve_equity(1000.0, &cfg).equity.to_bits()
    );
    assert_eq!(e.resolved_equity(1000.0, &cfg).to_bits(), e.account.equity_all(1000.0).to_bits());
    // quote-only: valued at the bid (long) — diverges from mark-only equity_all by design
    e.price_board.set_quote("sim", "BTC", 104.0, 106.0, 1);
    assert_eq!(
        e.resolved_equity(1000.0, &cfg).to_bits(),
        e.resolve_equity(1000.0, &cfg).equity.to_bits()
    );
    assert!(e.resolved_equity(1000.0, &cfg) != e.account.equity_all(1000.0));
    // fresh mark wins the chain: back to equity_all agreement
    e.account.set_mark_from("sim", "BTC", 105.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("sim", "BTC", 105.0, 2);
    assert_eq!(
        e.resolved_equity(1000.0, &cfg).to_bits(),
        e.resolve_equity(1000.0, &cfg).equity.to_bits()
    );
    assert_eq!(e.resolved_equity(1000.0, &cfg).to_bits(), e.account.equity_all(1000.0).to_bits());
}

#[test]
fn resolved_equity_is_mode_aware() {
    // Authoritative mode: balance + Σunreal (seed/realized ignored) — same law as equity_all.
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Authoritative),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    e.account.balance = 500.0;
    e.account.positions.insert(
        ("sim".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    e.price_board.set_quote("sim", "BTC", 110.0, 111.0, 1);
    let cfg = PriceCfg::default();
    // balance 500 + (110-100)*2 = 520; the ignored seed must not leak in
    assert_eq!(e.resolved_equity(9_999.0, &cfg).to_bits(), 520.0_f64.to_bits());
    assert_eq!(
        e.resolved_equity(9_999.0, &cfg).to_bits(),
        e.resolve_equity(9_999.0, &cfg).equity.to_bits()
    );
}
