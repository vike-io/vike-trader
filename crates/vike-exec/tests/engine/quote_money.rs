//! The MONEY of a foreign-quoted symbol, as GOLDENS (decision 0021: no tolerance is ever widened to make
//! a number pass). A USD account trades USDJPY (JPY-quoted) and EURGBP (GBP-quoted); the figures are
//! the design's section 1 table:
//!
//! | case | the old engine booked | the account's currency |
//! |---|---|---|
//! | USDJPY 100,000 units at 150, notional | 15,000,000 "USD" | 100,000 USD |
//! | the same, closed 0.10 higher | 10,000 "USD" | 10,000 / 150.10 = 66.62 USD |
//! | EURGBP 100,000 units at 0.85, notional | 85,000 "USD" | 85,000 x 1.35 = 114,750 USD |
//!
//! Realized PnL converts ONCE, at the rate in force when the fill folds (the margin-account
//! convention): `(p2 - p1) * q * u * r_close`. Where a figure is a float IDENTITY (the engine and the
//! test evaluate the same expression in the same order) it is asserted by `to_bits()`; where the test's
//! closed form differs from the engine's only in association or in a decimal literal that is not exact in
//! binary (`1/150`, `150.10 - 150.00`) it is asserted to a relative `1e-12`, which is ten thousand times
//! the rounding it must absorb and nowhere near the 150x it must tell apart.
//!
//! The last tests are the dangerous direction end to end: a USD engine holding the JPY position at a 66
//! USD loss is healthy, at a real loss that crosses maintenance it liquidates, and the SAME inputs with
//! no conversion (the old behaviour) liquidate the healthy one.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    AdmitOutcome, BalanceMode, EventHandler, ExecutionEngine, MarginCall, MarkSource, Outbox,
    check_margin_call_priced,
};
use vike_model::MarginCallConfig;
use vike_model::events::{
    AccountState, Event, FillEvent, FundingEvent, PositionLiquidated, PositionSide,
};

use crate::quote_currency::{
    EURGBP, T0, USDJPY, VENUE, engine, fill_event, fold, plain, quoted, size, usdjpy_engine,
};

/// 100,000 units: one standard lot.
const Q: f64 = 100_000.0;

/// `got` is `want` up to a relative `1e-12` (floored at 1.0 so a want near zero is not asked for
/// more than an absolute `1e-12`). See the module doc for what it absorbs.
fn close(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-12 * want.abs().max(1.0)
}

fn refresh(e: &mut ExecutionEngine<RecordingClient>, symbol: &str, ccy: &str, rate: f64, at: i64) {
    assert_eq!(e.admit_symbol(&quoted(symbol, ccy, rate, at)), AdmitOutcome::Refreshed);
}

#[test]
fn usdjpy_is_booked_in_dollars_not_yen() {
    let mut e = usdjpy_engine(1.0 / 150.0);
    fold(&mut e, fill_event("o1", USDJPY, 1, Q, 150.0));

    let value = e.account.contract_value_of(USDJPY);
    let notional = value.gross_notional(Q, 150.0);
    assert!(close(notional, 100_000.0), "the position is worth 100,000 USD: {notional}");
    assert!(
        close(value.order_notional(Q, 150.0), 100_000.0),
        "and so is the order the gate judges"
    );
    assert!(
        (notional - 15_000_000.0).abs() > 1.0,
        "NOT the 15,000,000 \"USD\" the old engine booked"
    );

    // the market rises 0.10: the venue converts at the rate of the close (1 / 150.10) and the engine
    // is told that rate by the refresh that precedes the close fill
    refresh(&mut e, USDJPY, "JPY", 1.0 / 150.10, T0 + 1);
    fold(&mut e, fill_event("c1", USDJPY, -1, Q, 150.10));

    let realized = e.account.realized_pnl;
    assert!(close(realized, 10_000.0 / 150.10), "10000 / 150.10 = 66.62: {realized}");
    assert_eq!((realized * 100.0).round() / 100.0, 66.62);
    assert!((realized - 10_000.0).abs() > 1.0, "NOT the 10,000 \"USD\" the old engine booked");
    assert_eq!(e.account.closed_pnls, vec![realized], "the closed leg is the same number");
    assert_eq!(size(&e, USDJPY), Some(0.0), "flat");
}

#[test]
fn eurgbp_is_booked_in_dollars_not_pounds() {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted(EURGBP, "GBP", 1.35, T0)), AdmitOutcome::Admitted);
    fold(&mut e, fill_event("o1", EURGBP, 1, Q, 0.85));

    let value = e.account.contract_value_of(EURGBP);
    let notional = value.gross_notional(Q, 0.85);
    assert!(close(notional, 114_750.0), "85,000 GBP at 1.35 is 114,750 USD: {notional}");
    assert!(
        (notional - 85_000.0).abs() > 1.0,
        "NOT the 85,000 \"USD\" the old engine booked (low)"
    );

    fold(&mut e, fill_event("c1", EURGBP, -1, Q, 0.86));
    // 0.01 * 100,000 = 1,000 GBP, times 1.35 (0.86 - 0.85 is not exact in binary: close, not equal)
    assert!(close(e.account.realized_pnl, 1_350.0), "{}", e.account.realized_pnl);
    assert!((e.account.realized_pnl - 1_000.0).abs() > 1.0, "NOT the 1,000 pounds");
}

/// Realized PnL converts ONCE at the rate in force when the fill folds: not the rate of the open, and
/// not an entry-rate-corrected figure (design 3.4: for a margined USDJPY the accounting form books
/// nothing at all).
#[test]
fn realized_converts_once_at_the_rate_in_force_when_the_fill_folds() {
    let (r1, r2) = (1.0 / 150.0, 1.0 / 155.0);
    let (p1, p2) = (150.0, 152.0);
    let mut e = usdjpy_engine(r1);
    fold(&mut e, fill_event("o1", USDJPY, 1, Q, p1));
    refresh(&mut e, USDJPY, "JPY", r2, T0 + 1);
    fold(&mut e, fill_event("c1", USDJPY, -1, Q, p2));

    // the engine evaluates `(p2 - p1) * (sign * closing) * unit` with unit = 1.0 * r2 = r2: this is
    // that expression, so it is an identity
    let want = (p2 - p1) * Q * r2;
    assert_eq!(e.account.realized_pnl.to_bits(), want.to_bits());

    // what it is NOT: the rate of the open, or the accounting form (entry at the open rate)
    let at_open_rate = (p2 - p1) * Q * r1;
    let accounting = Q * (p2 * r2 - p1 * r1);
    assert_ne!(e.account.realized_pnl.to_bits(), at_open_rate.to_bits());
    assert!(
        (e.account.realized_pnl - accounting).abs() > 1.0,
        "the accounting form differs by the FX revaluation of the notional: {accounting}"
    );
}

/// Partial close, rate change, rest close: each closing leg at its OWN rate, and a rate change between
/// two fills never changes what the first one booked.
#[test]
fn each_closing_leg_is_converted_at_its_own_rate() {
    let (r0, ra, rb) = (1.0 / 150.0, 1.0 / 151.0, 1.0 / 153.0);
    let mut e = usdjpy_engine(r0);
    fold(&mut e, fill_event("o1", USDJPY, 1, Q, 150.0));

    refresh(&mut e, USDJPY, "JPY", ra, T0 + 1);
    fold(&mut e, fill_event("c1", USDJPY, -1, 40_000.0, 151.0));
    let leg1 = (151.0 - 150.0) * 40_000.0 * ra;
    assert_eq!(e.account.closed_pnls[0].to_bits(), leg1.to_bits());
    let first_booked = e.account.realized_pnl;

    refresh(&mut e, USDJPY, "JPY", rb, T0 + 2);
    assert_eq!(e.account.realized_pnl.to_bits(), first_booked.to_bits(), "a refresh books nothing");
    assert_eq!(e.account.closed_pnls[0].to_bits(), leg1.to_bits(), "...and moves nothing booked");

    fold(&mut e, fill_event("c2", USDJPY, -1, 60_000.0, 153.0));
    let leg2 = (153.0 - 150.0) * 60_000.0 * rb;
    assert_eq!(e.account.closed_pnls.len(), 2);
    assert_eq!(e.account.closed_pnls[0].to_bits(), leg1.to_bits());
    assert_eq!(e.account.closed_pnls[1].to_bits(), leg2.to_bits());
    assert_eq!(e.account.realized_pnl.to_bits(), (leg1 + leg2).to_bits());

    // converting both legs at the last rate (or at the first) would be a different number
    let all_at_last = (151.0 - 150.0) * 40_000.0 * rb + leg2;
    let all_at_first = (151.0 - 150.0) * 40_000.0 * r0 + (153.0 - 150.0) * 60_000.0 * r0;
    assert_ne!(e.account.realized_pnl.to_bits(), all_at_last.to_bits());
    assert_ne!(e.account.realized_pnl.to_bits(), all_at_first.to_bits());
}

/// Unrealized PnL moves with the RATE while the price does not (and the position, the average and the
/// mark stay put).
#[test]
fn unrealized_moves_with_the_rate_while_the_price_does_not() {
    let (r1, r2) = (1.0 / 150.0, 1.0 / 156.0);
    let mut e = usdjpy_engine(r1);
    fold(&mut e, fill_event("o1", USDJPY, 1, Q, 150.0));
    e.account.set_mark_from(VENUE, USDJPY, 151.0, MarkSource::VenueMark, T0);

    let upnl = |e: &ExecutionEngine<RecordingClient>| {
        e.account.unrealized_pnl(VENUE, USDJPY, PositionSide::Both)
    };
    let u1 = upnl(&e);
    assert_eq!(u1.to_bits(), ((151.0 - 150.0) * Q * r1).to_bits());
    let equity1 = e.account.equity_all(10_000.0);

    refresh(&mut e, USDJPY, "JPY", r2, T0 + 1);
    let u2 = upnl(&e);
    assert_eq!(u2.to_bits(), ((151.0 - 150.0) * Q * r2).to_bits());
    assert_ne!(u1.to_bits(), u2.to_bits(), "the mark did not move; the rate did");
    assert_eq!(e.account.mark_of(VENUE, USDJPY), Some(151.0));
    assert_eq!(size(&e, USDJPY), Some(Q));

    // equity carries it (py_sum of one term plus the seed: association differs from the plain sum)
    assert!(close(equity1, 10_000.0 + u1), "{equity1}");
    assert!(close(e.account.equity_all(10_000.0), 10_000.0 + u2));
}

fn fee(commission: f64, asset: &str) -> FillEvent {
    FillEvent {
        commission,
        commission_asset: asset.into(),
        ..fill_event("f", USDJPY, 1, 1000.0, 150.0)
    }
}

/// Distinct trade ids for the fee fills (a trade id burns on its first fold).
fn fee_fill(id: &'static str, commission: f64, asset: &str) -> FillEvent {
    FillEvent { trade_id: id.into(), ..fee(commission, asset) }
}

#[test]
fn a_fee_with_no_asset_or_in_the_account_currency_is_as_stated() {
    let mut e = usdjpy_engine(1.0 / 150.0);
    fold(&mut e, fee_fill("f1", 2.5, ""));
    assert_eq!(e.account.balance, -2.5);
    assert_eq!(e.account.fees_paid, 2.5);
    assert!(e.account.fees_by_asset.is_empty(), "no asset named, none tallied");

    fold(&mut e, fee_fill("f2", 1.5, "USD"));
    assert_eq!(e.account.balance, -4.0);
    assert_eq!(e.account.fees_paid, 4.0);
    assert_eq!(e.account.fees_by_asset[&ustr::ustr("USD")], 1.5);
    assert_eq!(e.account.fees_unconverted(), 0);
}

#[test]
fn a_fee_in_the_symbols_quote_currency_is_converted_at_its_rate() {
    let r = 1.0 / 150.0;
    let mut e = usdjpy_engine(r);
    fold(&mut e, fee_fill("f1", 2.5, "USD"));
    let (balance, paid) = (e.account.balance, e.account.fees_paid);

    fold(&mut e, fee_fill("f2", 1000.0, "JPY"));
    let charge = 1000.0 * r; // 6.67 USD, not 1,000
    assert_eq!(e.account.fees_paid.to_bits(), (paid + charge).to_bits());
    assert_eq!(e.account.balance.to_bits(), (balance - charge).to_bits());
    assert_eq!(
        e.account.fees_by_asset[&ustr::ustr("JPY")],
        1000.0,
        "the per-asset tally keeps the amount in ITS asset"
    );
    assert_eq!(e.account.fees_unconverted(), 0);
}

/// A fee in a THIRD currency cannot be converted: it is not booked at par. The balance and `fees_paid`
/// stay put, the amount is tallied in its own asset, and it is counted (the account also says it once
/// per asset at `warn!`; that line is not asserted here because a hand-rolled capture is exactly
/// what `vike_log::capture` exists to replace and this crate has no edge to it).
#[test]
fn a_fee_in_a_third_currency_is_tallied_and_counted_not_booked() {
    let mut e = usdjpy_engine(1.0 / 150.0);
    fold(&mut e, fee_fill("f1", 2.5, "USD"));
    let (balance, paid) = (e.account.balance, e.account.fees_paid);

    fold(&mut e, fee_fill("f2", 0.01, "BNB"));
    assert_eq!(e.account.balance.to_bits(), balance.to_bits(), "not booked at par");
    assert_eq!(e.account.fees_paid.to_bits(), paid.to_bits());
    assert_eq!(e.account.fees_by_asset[&ustr::ustr("BNB")], 0.01, "the tally has it, in BNB");
    assert_eq!(e.account.fees_unconverted(), 1);

    fold(&mut e, fee_fill("f3", 0.02, "BNB"));
    assert_eq!(e.account.fees_unconverted(), 2, "counted every time");
    assert_eq!(e.account.fees_by_asset[&ustr::ustr("BNB")], 0.01 + 0.02, "summed in its own asset");

    fold(&mut e, fee_fill("f4", 0.5, "EUR"));
    assert_eq!(e.account.fees_unconverted(), 3);
    assert_eq!(e.account.balance.to_bits(), balance.to_bits());
}

fn funding(symbol: &str, amount: f64) -> Event {
    Event::Funding(FundingEvent {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0001,
        amount,
        mark_price: None,
        ts: T0,
        route_key: None,
    })
}

/// A funding amount names no currency, so on a foreign-quoted symbol it is NOT folded (it could be the
/// account's, the quote's or a third one's, 150x apart); it is counted and said once. On an
/// account-quoted symbol it folds as it always did.
#[test]
fn funding_on_a_foreign_symbol_is_counted_not_folded_and_on_an_account_symbol_it_folds() {
    let mut e = usdjpy_engine(1.0 / 150.0);
    e.admit_symbol(&plain("GBPUSD"));

    e.on_event(&funding(USDJPY, -5.0), &mut Outbox::default());
    assert_eq!(e.account.balance, 0.0);
    assert_eq!(e.account.funding_paid, 0.0);
    assert_eq!(e.account.fees_unconverted(), 1);
    e.on_event(&funding(USDJPY, -7.0), &mut Outbox::default());
    assert_eq!(e.account.fees_unconverted(), 2);

    e.on_event(&funding("GBPUSD", -3.0), &mut Outbox::default());
    e.on_event(&funding(crate::quote_currency::MOUNTED, 1.0), &mut Outbox::default());
    assert_eq!(e.account.funding_paid, -2.0);
    assert_eq!(e.account.balance, -2.0);
    assert_eq!(e.account.fees_unconverted(), 2, "account-quoted funding is not counted");
}

fn liquidation(symbol: &str, qty: f64, px: f64, fee: f64, id: &str) -> Event {
    Event::PositionLiquidated(PositionLiquidated {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        qty,
        liq_price: px,
        fee,
        ts: T0,
        trade_id: id.into(),
        route_key: None,
    })
}

/// The forced close of a foreign-quoted position still happens, and its PnL is converted; only the
/// FEE (which names no currency) is left out. On an account-quoted symbol the fee is charged as ever.
#[test]
fn a_forced_close_fee_on_a_foreign_symbol_is_not_charged_but_the_position_closes_converted() {
    let r = 1.0 / 150.0;
    let mut e = usdjpy_engine(r);
    e.admit_symbol(&plain("GBPUSD"));
    fold(&mut e, fill_event("o1", USDJPY, 1, Q, 150.0));
    fold(&mut e, fill_event("o2", "GBPUSD", 1, Q, 1.25));

    e.on_event(&liquidation(USDJPY, 0.0, 149.0, 12.0, "L1"), &mut Outbox::default());
    assert_eq!(size(&e, USDJPY), Some(0.0), "the position closed");
    let want = (149.0 - 150.0) * Q * r;
    assert_eq!(e.account.closed_pnls, vec![want], "at the converted figure");
    assert_eq!(e.account.balance, 0.0, "the 12 of fee is not charged");
    assert_eq!(e.account.fees_unconverted(), 1);

    e.on_event(&liquidation("GBPUSD", 0.0, 1.20, 12.0, "L2"), &mut Outbox::default());
    assert_eq!(e.account.balance, -12.0, "an account-quoted forced close charges its fee");
    assert_eq!(e.account.fees_unconverted(), 1);
}

fn balances(frame: &[(&str, f64)]) -> Event {
    Event::AccountState(AccountState {
        venue: VENUE.into(),
        balances: frame.iter().map(|(a, q)| ((*a).to_string(), *q)).collect(),
        ts: T0,
        route_key: None,
    })
}

/// With a foreign-quoted symbol held, a frame in another currency is REPORTED (`balances_by_asset`) and
/// never becomes the balance; only the `quote_asset` balance does; neither fallback ("a lone balance,
/// whatever it is", "the sum of all balances") is used, because both would write yen and dollars into one
/// number.
#[test]
fn a_frame_in_another_currency_is_reported_not_adopted_while_a_foreign_symbol_is_held() {
    let mut e = usdjpy_engine(1.0 / 150.0);
    let apply = |e: &mut ExecutionEngine<RecordingClient>, frame: &[(&str, f64)]| {
        e.on_event(&balances(frame), &mut Outbox::default());
    };

    apply(&mut e, &[("JPY", 1_500_000.0)]);
    assert_eq!(e.account.balances_by_asset.get("JPY"), Some(&1_500_000.0), "reported");
    assert_eq!(e.account.balance, 0.0, "not the ledger");
    assert_eq!(e.account.balance_mode, BalanceMode::Delta);
    assert_eq!(e.account.realized_pnl_at_balance_sync, None);

    apply(&mut e, &[("JPY", 1_500_000.0), ("EUR", 3.0)]);
    assert_eq!(e.account.balance, 0.0, "the sum of all balances is not the fallback");
    assert_eq!(e.account.balance_mode, BalanceMode::Delta);

    apply(&mut e, &[("JPY", 1_500_000.0), ("USD", 9_000.0)]);
    assert_eq!(e.account.balance, 9_000.0, "the quote_asset balance IS the ledger");
    assert_eq!(e.account.balance_mode, BalanceMode::Authoritative);
    assert_eq!(e.account.realized_pnl_at_balance_sync, Some(e.account.realized_pnl));
    assert_eq!(e.account.balances_by_asset.get("JPY"), Some(&1_500_000.0));
}

/// Without a foreign symbol the old behaviour stands: a lone balance is adopted whatever it is, and a
/// frame with no `quote_asset` balance is summed.
#[test]
fn without_a_foreign_symbol_the_old_balance_rules_are_unchanged() {
    let mut e = engine();
    let apply = |e: &mut ExecutionEngine<RecordingClient>, frame: &[(&str, f64)]| {
        e.on_event(&balances(frame), &mut Outbox::default());
    };
    apply(&mut e, &[("JPY", 100.0)]);
    assert_eq!(e.account.balance, 100.0);
    assert_eq!(e.account.balance_mode, BalanceMode::Authoritative);
    apply(&mut e, &[("JPY", 1.0), ("EUR", 2.0)]);
    assert_eq!(e.account.balance, 3.0, "the sum of all");
    apply(&mut e, &[("JPY", 1.0), ("USD", 7.0)]);
    assert_eq!(e.account.balance, 7.0);
}

// ---- THE DANGEROUS DIRECTION, end to end ----------------------------------------------------------

const SEED: f64 = 10_000.0;

/// A USD engine with `SEED` of equity, long 100,000 USDJPY from 150.00, marked at `mark`. With
/// `converted` the symbol is admitted quoted in JPY and the rate is refreshed to `1 / mark` (the market
/// moved, so did the rate: that is what the node sends); without it the symbol is admitted as an
/// ordinary account-quoted one, which is what the engine did before this change.
fn long_usdjpy(converted: bool, mark: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = engine();
    let admission = if converted { quoted(USDJPY, "JPY", 1.0 / 150.0, T0) } else { plain(USDJPY) };
    assert_eq!(e.admit_symbol(&admission), AdmitOutcome::Admitted);
    fold(&mut e, fill_event("o1", USDJPY, 1, Q, 150.0));
    if converted {
        refresh(&mut e, USDJPY, "JPY", 1.0 / mark, T0 + 1);
    }
    e.account.set_mark_from(VENUE, USDJPY, mark, MarkSource::VenueMark, T0 + 1);
    e
}

fn judge(e: &ExecutionEngine<RecordingClient>) -> MarginCall {
    let equity = e.account.equity_all(SEED);
    check_margin_call_priced(
        &e.account,
        equity,
        &MarginCallConfig::default(),
        |(v, s, _side), _p| e.account.mark_of(v, s),
    )
}

#[test]
fn a_usd_engine_holding_the_jpy_position_at_a_66_dollar_loss_is_healthy() {
    let e = long_usdjpy(true, 149.90);
    let upnl = e.account.unrealized_pnl(VENUE, USDJPY, PositionSide::Both);
    assert!(close(upnl, -10_000.0 / 149.90), "0.10 of JPY x 100,000 at 1/149.90: {upnl}");
    assert!(upnl < -66.0 && upnl > -67.0, "a 66-dollar loss: {upnl}");
    let equity = e.account.equity_all(SEED);
    assert!(close(equity, SEED + upnl) && equity > 9_900.0, "{equity}");
    assert_eq!(judge(&e), MarginCall::Healthy);
}

/// A loss that really crosses maintenance liquidates, in dollars: maintenance is 5% of the 100,000 USD
/// notional (5,000) and the equity is 10,000 less 900,000 JPY at 1/141.
#[test]
fn at_a_real_loss_that_crosses_maintenance_it_liquidates() {
    let e = long_usdjpy(true, 141.0);
    let MarginCall::Liquidate(intents) = judge(&e) else {
        panic!("a 6,383 USD loss on 10,000 of equity against 5,000 of maintenance must liquidate");
    };
    assert_eq!(intents.len(), 1);
    let i = &intents[0];
    assert_eq!((i.venue.as_str(), i.symbol.as_str(), i.side), (VENUE, USDJPY, -1));

    // the excess over the pool, in units: (used - equity) / (mark * unit * mm)
    let equity = SEED - 9.0 * Q / 141.0;
    let used = Q * 0.05;
    let want = (used - equity) / 0.05;
    assert!(close(i.qty, want), "{} vs {want}", i.qty);
    assert!(i.qty < Q, "only the excess is closed, not the position");
}

/// The SAME inputs with no conversion are the old failure: the 0.10 JPY move books as 10,000 "USD",
/// wipes the equity and liquidates an account that is, in dollars, 66 down.
#[test]
fn without_the_conversion_the_same_inputs_liquidate_a_healthy_account_which_is_the_old_failure() {
    let e = long_usdjpy(false, 149.90);
    let upnl = e.account.unrealized_pnl(VENUE, USDJPY, PositionSide::Both);
    assert!(close(-upnl, 10_000.0), "10,000 \"USD\" of loss for a 66-dollar move: {upnl}");
    assert!(
        matches!(judge(&e), MarginCall::Liquidate(_)),
        "unconverted, the healthy account is liquidated"
    );
    // ...which the converted engine, given the very same fills and marks, is not
    assert_eq!(judge(&long_usdjpy(true, 149.90)), MarginCall::Healthy);
}
