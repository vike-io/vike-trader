//! The WELD: `apply_fill` refuses an already-folded `trade_id` on its own, with no help from
//! `ExecutionEngine`. Every test here drives `Account` DIRECTLY — that is the point.

use super::*;
use vike_model::events::FillEvent;

fn fill(trade_id: &str, symbol: &str, side: i32, qty: f64, px: f64, comm: f64) -> FillEvent {
    FillEvent {
        trade_id: vike_model::events::TradeId::new(trade_id).expect("test ids are non-empty"),
        client_order_id: "c1".to_string(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: comm,
        commission_asset: "USDT".into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn acct() -> Account {
    Account::new(1.0, "binance", None, BalanceMode::Delta)
}

fn btc_key() -> PositionKey {
    ("binance".into(), "BTCUSDT".into(), PositionSide::Both)
}

/// THE INVARIANT. Folding one fill twice moves money exactly ONCE — every quantity, not just the
/// position: `balance` (the commission), `fees_paid`, `fees_by_asset`, `realized_pnl` and
/// `closed_pnls` — for a DIRECT caller, with no engine-side guard in front.
#[test]
fn applying_the_same_fill_twice_moves_money_exactly_once() {
    let mut a = acct();
    let open = fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.5);

    assert_eq!(a.apply_fill(&open), FillFold::Applied);
    let after_one = (
        a.balance,
        a.fees_paid,
        a.realized_pnl,
        a.positions[&btc_key()],
        a.closed_pnls.len(),
        a.fees_by_asset.get(&Ustr::from("USDT")).copied(),
    );
    assert_eq!(a.balance.to_bits(), (-0.5_f64).to_bits(), "commission netted once");

    assert_eq!(a.apply_fill(&open), FillFold::Duplicate, "the SECOND fold is refused");
    assert_eq!(
        (
            a.balance,
            a.fees_paid,
            a.realized_pnl,
            a.positions[&btc_key()],
            a.closed_pnls.len(),
            a.fees_by_asset.get(&Ustr::from("USDT")).copied(),
        ),
        after_one,
        "a refused duplicate must not move ANY money-bearing field"
    );
    assert_eq!(a.duplicate_fills_refused, 1, "and the refusal is COUNTED, not silent");
    assert_eq!(a.colliding_fills_refused, 0, "same fill re-delivered is not a collision");
}

/// The equity delta of a re-delivery is EXACTLY zero — bitwise, not within a tolerance. Stated
/// separately because equity is the number an operator watches.
#[test]
fn the_equity_delta_of_a_redelivered_fill_is_bitwise_zero() {
    let mut a = acct();
    let f = fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.5);
    a.apply_fill(&f);
    a.set_mark_from("binance", "BTCUSDT", 110.0, MarkSource::VenueMark, 0);
    let before = a.equity_all(1_000.0);
    for _ in 0..5 {
        assert_eq!(a.apply_fill(&f), FillFold::Duplicate);
    }
    assert_eq!(a.equity_all(1_000.0).to_bits(), before.to_bits());
    assert_eq!(a.duplicate_fills_refused, 5);
}

/// A realized-PnL round trip: the CLOSING fill re-delivered must not book its PnL twice. The
/// commission-only tests above would pass even if `fold` were re-run on a flat position, so this
/// exercises the `closed_pnls` push specifically.
#[test]
fn a_redelivered_closing_fill_does_not_realize_pnl_twice() {
    let mut a = acct();
    a.apply_fill(&fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.0));
    assert_eq!(a.apply_fill(&fill("t2", "BTCUSDT", -1, 2.0, 110.0, 0.0)), FillFold::Applied);
    assert_eq!(a.closed_pnls.len(), 1);
    let realized = a.realized_pnl;

    assert_eq!(a.apply_fill(&fill("t2", "BTCUSDT", -1, 2.0, 110.0, 0.0)), FillFold::Duplicate);
    assert_eq!(a.closed_pnls.len(), 1, "no second closed PnL");
    assert_eq!(a.realized_pnl.to_bits(), realized.to_bits());
}

/// Distinct ids fold independently — the guard must not be a blanket "one fill per symbol".
#[test]
fn distinct_trade_ids_all_fold() {
    let mut a = acct();
    for i in 0..4 {
        assert_eq!(
            a.apply_fill(&fill(&format!("t{i}"), "BTCUSDT", 1, 1.0, 100.0, 0.0)),
            FillFold::Applied
        );
    }
    assert_eq!(a.positions[&btc_key()].size, 4.0);
    assert_eq!(a.duplicate_fills_refused, 0);
    assert_eq!(a.seen_fill_ids().count(), 4);
}

/// The untagged-fill case has no fold test because it has no code: `TradeId::new` refuses the
/// empty string, so an unguardable fill cannot be constructed. The property is the type's, pinned
/// by `vike-model`'s `new_refuses_the_empty_string` /
/// `default_is_not_implemented_and_that_is_the_whole_point`.
#[test]
fn an_empty_trade_id_cannot_be_constructed_so_no_untagged_fill_exists() {
    assert!(vike_model::events::TradeId::new("").is_err(), "the whole branch rests on this");
}

/// THE NARROW-KEY HAZARD, made loud. Two DIFFERENT executions sharing one id string — reachable
/// today via binance/aster `t` and okx `tradeId` (per-SYMBOL venue sequences) on a multi-symbol
/// engine, and via `vike_paper`'s per-client `paper-` counter under `MultiPaperExecutionClient`.
/// The second fill is still refused (see `apply_fill`'s doc for why folding on a mismatch is
/// worse), but it is reported as a COLLISION, counted separately and logged at ERROR — so a
/// dropped genuine fill can never be mistaken for a collapsed reconnect replay.
#[test]
fn a_trade_id_collision_is_reported_as_a_collision_not_a_replay() {
    let mut a = acct();
    assert_eq!(a.apply_fill(&fill("7", "BTCUSDT", 1, 2.0, 100.0, 0.0)), FillFold::Applied);
    // Same id, DIFFERENT symbol — exactly the per-symbol-sequence shape.
    assert_eq!(a.apply_fill(&fill("7", "ETHUSDT", 1, 3.0, 50.0, 0.0)), FillFold::Collision);
    assert_eq!(a.colliding_fills_refused, 1);
    assert_eq!(a.duplicate_fills_refused, 0, "a collision is NOT a routine duplicate");
    // Same id, same symbol, DIFFERENT qty — the paper-counter shape within one symbol.
    assert_eq!(a.apply_fill(&fill("7", "BTCUSDT", 1, 9.0, 100.0, 0.0)), FillFold::Collision);
    assert_eq!(a.colliding_fills_refused, 2);
    // Nothing moved on either refusal.
    assert_eq!(a.positions[&btc_key()].size, 2.0);
    let eth: PositionKey = ("binance".into(), "ETHUSDT".into(), PositionSide::Both);
    assert!(!a.positions.contains_key(&eth));
}

/// The fingerprint deliberately EXCLUDES `commission` and `ts`, because binance perp's early
/// `TRADE_LITE` fill and its authoritative twin share one `t` and differ in exactly those fields.
/// Treating that designed-in pair as a collision would put an ERROR in front of every perp fill.
#[test]
fn a_redelivery_that_restates_only_fee_or_ts_is_a_duplicate_not_a_collision() {
    let mut a = acct();
    assert_eq!(a.apply_fill(&fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.0)), FillFold::Applied);
    let mut authoritative = fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.4);
    authoritative.ts = 99;
    assert_eq!(a.apply_fill(&authoritative), FillFold::Duplicate);
    assert_eq!(a.colliding_fills_refused, 0);
    assert_eq!(a.balance.to_bits(), 0.0_f64.to_bits(), "the early fill carried no fee");
}

/// A SEEDED id (restored from `EngineSnapshot::seen_trade_ids`) carries no fingerprint, so a
/// refusal against it is a plain duplicate — never a false collision ERROR in front of a restart.
#[test]
fn a_seeded_id_refuses_without_claiming_a_collision() {
    let mut a = acct();
    a.seed_seen_fill_ids(["t1".to_string()]);
    assert!(a.has_folded_fill("t1"));
    // Deliberately a DIFFERENT fill body than anything this session folded.
    assert_eq!(a.apply_fill(&fill("t1", "ETHUSDT", -1, 7.0, 5.0, 1.0)), FillFold::Duplicate);
    assert_eq!(a.colliding_fills_refused, 0, "an unknown fingerprint is not a collision");
    assert_eq!(a.duplicate_fills_refused, 1);
    assert_eq!(a.balance.to_bits(), 0.0_f64.to_bits());
}

/// `fill_print` is an IDENTITY hash, not arithmetic: it must separate the four fields it reads and
/// ignore the ones it does not. Guards against a lazy implementation that concatenates the symbol
/// and side into an ambiguous byte stream.
#[test]
fn fill_print_separates_the_identifying_fields() {
    let base = fill("x", "BTCUSDT", 1, 2.0, 100.0, 0.0);
    assert_eq!(fill_print(&base), fill_print(&fill("y", "BTCUSDT", 1, 2.0, 100.0, 9.9)));
    assert_ne!(fill_print(&base), fill_print(&fill("x", "ETHUSDT", 1, 2.0, 100.0, 0.0)));
    assert_ne!(fill_print(&base), fill_print(&fill("x", "BTCUSDT", -1, 2.0, 100.0, 0.0)));
    assert_ne!(fill_print(&base), fill_print(&fill("x", "BTCUSDT", 1, 2.5, 100.0, 0.0)));
    assert_ne!(fill_print(&base), fill_print(&fill("x", "BTCUSDT", 1, 2.0, 100.5, 0.0)));
    // The sentinel is never mintable, so a computed print can never be read as "unknown".
    assert_ne!(fill_print(&base), PRINT_UNKNOWN);
}
