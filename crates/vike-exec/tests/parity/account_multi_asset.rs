//! Phase A gate: the ADDITIVE per-asset ledger (accounting-upgrade design, parity law A1).
//! Asserts the new `balances_by_asset` map + `value_in` view AND that the oracle's scalar
//! collapse (`balance`, `balance_mode`) is untouched on every `apply_account_state` path —
//! the byte-level scalar pins stay with `account_parity.rs`; this file guards the new
//! surface and the merge (upsert) semantics venue partial frames rely on.

use vike_exec::{Account, BalanceMode, RateBook};
use vike_model::events::{AccountState, FillEvent, PositionLiquidated, PositionSide};

fn acct() -> Account {
    Account::new(1.0, "binance", None, BalanceMode::Delta)
}

/// Per-test-process counter so every `fill(..)` (a binance BTCUSDT buy of 1@100, signed
/// `commission` in `asset`; empty `asset` = no fee currency surfaced) carries a DISTINCT
/// `trade_id`: `apply_fill` is idempotent per `trade_id`, so a shared id would refuse the second
/// fill of a sequence, as a real venue stream would.
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
        position_side: vike_model::events::PositionSide::Both,
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
    // every pair remembered, venue-given order
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

/// FEES-AUDIT 2026-10-26 §4.8 c. Binance's ACCOUNT_UPDATE / outboundAccountPosition lists only the
/// CHANGED assets, so a fee paid in BNB arrives as `[BNB]` alone. That lone row used to be written
/// into `balance` whatever it was called (half a BNB read as half a dollar). It is REPORTED in the
/// per-asset ledger and adopts nothing.
#[test]
fn a_lone_balance_in_another_asset_is_reported_and_adopts_nothing() {
    let mut a = acct();
    a.apply_account_state(&state(&[("USDT", 1_000.0)]), "USDT");
    let sync = a.realized_pnl_at_balance_sync;
    a.apply_account_state(&state(&[("BNB", 0.5)]), "USDT");
    assert_eq!(a.balance, 1_000.0, "the dollar ledger did not become half a BNB");
    assert_eq!(a.balance_mode, BalanceMode::Authoritative);
    assert_eq!(a.realized_pnl_at_balance_sync, sync, "a skipped frame is not a reseed");
    assert_eq!(a.balances_by_asset.get("BNB"), Some(&0.5), "...but it is reported");

    // on a fresh Delta account it flips nothing at all
    let mut fresh = acct();
    fresh.apply_account_state(&state(&[("BTC", 2.0)]), "USDT");
    assert_eq!((fresh.balance, fresh.balance_mode), (0.0, BalanceMode::Delta));
    assert_eq!(fresh.realized_pnl_at_balance_sync, None);
    assert_eq!(fresh.balances_by_asset.get("BTC"), Some(&2.0));
}

/// The lone-balance guard is "the ledger's MONEY", not "the string `USDT`": a dollar label against a
/// dollar label is adopted (dukascopy's one `USD` frame on an engine whose `quote_asset` is still the
/// constructor's `USDT` guess; a `USDC`-margined wallet), and a lone balance NAMED the quote is.
#[test]
fn a_lone_dollar_label_is_still_the_dollar_ledger() {
    for (asset, quote) in [("USD", "USDT"), ("USDC", "USDT"), ("USDT", "USD"), ("USDT", "USDT")] {
        let mut a = acct();
        a.apply_account_state(&state(&[(asset, 250.0)]), quote);
        assert_eq!(a.balance, 250.0, "{asset} against {quote}");
        assert_eq!(a.balance_mode, BalanceMode::Authoritative, "{asset} against {quote}");
    }
    // a non-dollar quote adopts its own asset only
    let mut eur = acct();
    eur.apply_account_state(&state(&[("EUR", 90.0)]), "EUR");
    assert_eq!(eur.balance, 90.0);
    let mut other = acct();
    other.apply_account_state(&state(&[("USD", 90.0)]), "EUR");
    assert_eq!(other.balance, 0.0, "a dollar frame is not a euro ledger");
}

#[test]
fn sum_fallback_path_writes_ledger() {
    let mut a = acct();
    a.apply_account_state(&state(&[("USDC", 100.0), ("DAI", 200.0), ("BUSD", 300.0)]), "USDT");
    assert_eq!(a.balance, 600.0); // py_sum fallback — oracle behavior (mixed-asset footgun)
    assert_eq!(a.balances_by_asset.len(), 3);
    // the ledger carries the truth the scalar collapse loses
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
    // A1 parity law: per-asset fee attribution is purely additive — the scalar fold
    // (balance -= commission, fees_paid += commission) is identical whether or not an asset is
    // present. A non-power-of-two commission makes the equality pin the fold, not luck.
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

/// A whole-position forced close of the `fill(..)` position (long 1 @ 100) at 90, paying `fee`.
fn liquidation(fee: f64, ccy: Option<&str>) -> PositionLiquidated {
    PositionLiquidated {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        position_side: PositionSide::Both,
        qty: 0.0,
        liq_price: 90.0,
        fee,
        ts: 0,
        trade_id: String::new().into(),
        route_key: None,
        fee_ccy: ccy.map(Into::into),
    }
}

/// FEES-AUDIT 2026-10-26 §4.8 a. The forced-close fee reached `balance` and NOTHING ELSE, so every
/// liquidation under-stated `fees_paid` (the GUI column, the summary line) and the own-PnL the
/// drawdown latch reads. It is a fee like any other: the same signed figure in `fees_paid` and in the
/// per-asset tally, and the `Delta` identity `equity = seed + own pnl` holds across the liquidation.
#[test]
fn a_forced_close_fee_is_a_fee_in_fees_paid_and_the_asset_tally() {
    let mut a = acct();
    a.apply_fill(&fill("USDT", 0.0));
    a.apply_liquidation(&liquidation(12.0, Some("USDT")));
    assert_eq!(a.balance, -12.0);
    assert_eq!(a.fees_paid, 12.0, "the forced-close fee is in fees_paid");
    assert_eq!(a.fees_by_asset.get(&ustr::ustr("USDT")), Some(&12.0));
    assert_eq!(a.realized_pnl, -10.0, "the close itself realized (90 - 100) x 1");
    // The identity `resolved_own_pnl` documents: on a Delta account seed + own PnL == equity.
    assert_eq!(a.equity_all(0.0), a.realized_pnl - a.fees_paid + a.funding_paid);
}

/// Same conventions as a fill's commission: signed (a negative figure is a rebate), and no stated
/// currency leaves the per-asset tally alone while `fees_paid` still moves.
#[test]
fn a_forced_close_fee_keeps_the_fills_sign_and_leaves_an_unnamed_asset_untallied() {
    let mut a = acct();
    a.apply_fill(&fill("", 0.0));
    a.apply_liquidation(&liquidation(-3.0, None));
    assert_eq!(a.fees_paid, -3.0);
    assert_eq!(a.balance, 3.0);
    assert!(a.fees_by_asset.is_empty(), "no currency stated, nothing to attribute");
}

#[test]
fn repeated_fee_fills_accumulate_per_asset() {
    let mut a = acct();
    a.apply_fill(&fill("BNB", 0.5));
    a.apply_fill(&fill("BNB", 0.25));
    assert_eq!(a.fees_by_asset.get(&ustr::ustr("BNB")), Some(&0.75));
}
