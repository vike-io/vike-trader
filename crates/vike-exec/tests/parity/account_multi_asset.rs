//! Phase A gate: the ADDITIVE per-asset ledger (accounting-upgrade design, parity law A1).
//! Asserts the new `balances_by_asset` map + `value_in` view AND that the oracle's scalar
//! collapse (`balance`, `balance_mode`) is untouched on every `apply_account_state` path —
//! the byte-level scalar pins stay with `account_parity.rs`; this file guards the new
//! surface and the merge (upsert) semantics venue partial frames rely on.

use vike_exec::{Account, BalanceMode};
use vike_model::events::{AccountState, FillEvent};
use vike_model::RateBook;

fn acct() -> Account {
    Account::new(1.0, "binance", None, BalanceMode::Delta)
}

/// A binance BTCUSDT buy of 1@100 carrying a signed `commission` denominated in `asset`
/// (empty `asset` = venue did not surface a fee currency).
/// Per-test-process fill counter so every `fill(..)` carries a DISTINCT `trade_id`. Load-bearing
/// since the dedup ledger moved onto `Account`: `apply_fill` is idempotent per `trade_id`, so a
/// shared `"t1"` turned the second fill of a sequence into a refusal — which is what a real venue
/// stream would also have done. The hardcoded id was the test asserting an accumulation no venue
/// could produce, not the guard being wrong.
static FILL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fill(asset: &str, commission: f64) -> FillEvent {
    let n = FILL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FillEvent {
        trade_id: vike_model::events::TradeId::prefixed("t", n),
        client_order_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission,
        commission_asset: asset.into(),
        liquidity_side: String::new().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn state(balances: &[(&str, f64)]) -> AccountState {
    AccountState {
        venue: "binance".to_string().into(),
        balances: balances.iter().map(|(a, q)| (a.to_string(), *q)).collect(),
        ts: 0,
        route_key: None,
    }
}

#[test]
fn quote_match_path_writes_ledger_and_keeps_scalar() {
    let mut a = acct();
    a.apply_account_state(&state(&[("BTC", 2.0), ("USDT", 1_000.0)]), "USDT");
    // scalar collapse: quote match wins — unchanged oracle behavior
    assert_eq!(a.balance, 1_000.0);
    assert_eq!(a.balance_mode, BalanceMode::Authoritative);
    // new: every pair remembered, venue-given order
    let got: Vec<(&str, f64)> = a.balances_by_asset.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(got, vec![("BTC", 2.0), ("USDT", 1_000.0)]);
}

#[test]
fn single_balance_path_writes_ledger() {
    let mut a = acct();
    a.apply_account_state(&state(&[("USDC", 500.0)]), "USDT");
    assert_eq!(a.balance, 500.0); // single-balance fallback — oracle behavior
    assert_eq!(a.balances_by_asset.get("USDC"), Some(&500.0));
}

#[test]
fn sum_fallback_path_writes_ledger() {
    let mut a = acct();
    a.apply_account_state(&state(&[("USDC", 100.0), ("DAI", 200.0), ("BUSD", 300.0)]), "USDT");
    assert_eq!(a.balance, 600.0); // py_sum fallback — oracle behavior (mixed-asset footgun)
    assert_eq!(a.balances_by_asset.len(), 3);
    // the ledger now carries the truth the scalar collapse loses
    assert_eq!(a.balances_by_asset.get("DAI"), Some(&200.0));
}

#[test]
fn partial_frames_merge_not_wipe() {
    let mut a = acct();
    a.apply_account_state(&state(&[("BTC", 2.0), ("USDT", 1_000.0)]), "USDT");
    // Binance outboundAccountPosition style: only the changed asset arrives
    a.apply_account_state(&state(&[("USDT", 900.0)]), "USDT");
    assert_eq!(a.balance, 900.0);
    assert_eq!(a.balances_by_asset.get("BTC"), Some(&2.0)); // survived the partial frame
    assert_eq!(a.balances_by_asset.get("USDT"), Some(&900.0));
}

#[test]
fn value_in_converts_and_sums() {
    let mut a = acct();
    a.apply_account_state(&state(&[("BTC", 2.0), ("USDT", 1_000.0)]), "USDT");
    let mut rb = RateBook::new();
    rb.set("BTC", "USDT", 50_000.0);
    assert_eq!(a.value_in(&rb, "USDT"), Some(101_000.0));
}

#[test]
fn value_in_none_when_any_leg_unpriced() {
    let mut a = acct();
    a.apply_account_state(&state(&[("BTC", 2.0), ("USDT", 1_000.0)]), "USDT");
    let rb = RateBook::new(); // no BTC rate
    assert_eq!(a.value_in(&rb, "USDT"), None); // LEAN convention: caller decides, no guess
}

#[test]
fn value_in_skips_zero_qty_without_rate() {
    let mut a = acct();
    a.apply_account_state(&state(&[("DUST", 0.0), ("USDT", 1_000.0)]), "USDT");
    let rb = RateBook::new();
    assert_eq!(a.value_in(&rb, "USDT"), Some(1_000.0));
}

#[test]
fn fill_commission_attributes_the_fee_asset() {
    let mut a = acct();
    a.apply_fill(&fill("BNB", 0.5)); // paid 0.5 BNB in fees
                                     // fees_by_asset is the per-asset twin of the `fees_paid` scalar: SAME sign convention
                                     // (>0 = paid), so 0.5 BNB paid reads as +0.5.
    assert_eq!(a.fees_by_asset.get(&ustr::ustr("BNB")), Some(&0.5));
}

#[test]
fn fills_do_not_touch_the_balances_ledger() {
    // INVARIANT: balances_by_asset is the authoritative HOLDINGS mirror, written ONLY by
    // apply_account_state (venue snapshots). Fee attribution lives in the separate
    // fees_by_asset map, so a fee never masquerades as a (negative) holding in value_in.
    let mut a = acct();
    a.apply_fill(&fill("BNB", 0.5));
    assert!(a.balances_by_asset.is_empty());
}

#[test]
fn fill_commission_leaves_the_scalar_fold_byte_identical() {
    // A1 parity law: the per-asset fee attribution is purely additive — the scalar
    // balance/fees_paid must move EXACTLY as they did before attribution existed
    // (balance -= commission, fees_paid += commission), whether or not an asset is present.
    // Uses a non-power-of-two commission so the equality genuinely pins the fold, not luck.
    let mut with_asset = acct();
    with_asset.apply_fill(&fill("BNB", 0.1));
    let mut without_asset = acct();
    without_asset.apply_fill(&fill("", 0.1));
    assert_eq!(with_asset.balance, without_asset.balance);
    assert_eq!(with_asset.fees_paid, without_asset.fees_paid);
    assert_eq!(with_asset.balance, -0.1);
    assert_eq!(with_asset.fees_paid, 0.1);
}

#[test]
fn fill_without_commission_asset_leaves_fee_ledger_empty() {
    // venues that don't surface a fee currency (or single-ccy venues) stay scalar-only:
    // fees_paid still moves, but the per-asset fee ledger is untouched.
    let mut a = acct();
    a.apply_fill(&fill("", 0.5));
    assert!(a.fees_by_asset.is_empty());
    assert_eq!(a.fees_paid, 0.5); // scalar still accrues
}

#[test]
fn maker_rebate_is_negative_in_the_fee_ledger() {
    // signed commission < 0 is a rebate (income): the per-asset fee tally goes negative,
    // mirroring fees_paid.
    let mut a = acct();
    a.apply_fill(&fill("USDT", -0.2));
    assert_eq!(a.fees_by_asset.get(&ustr::ustr("USDT")), Some(&-0.2));
}

#[test]
fn repeated_fee_fills_accumulate_per_asset() {
    let mut a = acct();
    a.apply_fill(&fill("BNB", 0.5));
    a.apply_fill(&fill("BNB", 0.25));
    assert_eq!(a.fees_by_asset.get(&ustr::ustr("BNB")), Some(&0.75));
}
