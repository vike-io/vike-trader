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
    AdmitOutcome, BalanceMode, DEFAULT_QUOTE_RATE_TTL_MS, EventHandler, ExecutionEngine,
    MarginCall, MarkSource, Outbox, SymbolAdmission, check_margin_call_priced, state_hash,
};
use vike_model::events::{
    AccountState, Event, FillEvent, FundingEvent, PositionLiquidated, PositionSide,
};
use vike_model::{ContractValue, MarginCallConfig};

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

/// A fee in a THIRD currency the book holds NO rate for cannot be converted: it is not booked at par.
/// The balance and `fees_paid` stay put, the amount is tallied in its own asset, and it is counted (the
/// account also says it once per asset at `warn!`; that line is not asserted here because a
/// hand-rolled capture is exactly what `vike_log::capture` exists to replace and this crate has no
/// edge to it). With a rate in the book it IS booked: the section below.
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
        ccy: None,
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
        fee_ccy: None,
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

// ---- THIRD-CURRENCY fees, funding and forced-close fees (decision 0183, item 9, phase 2) -------------
//
// A USD account trades EURGBP (GBP-quoted, GBP at 1.35 USD) and the book also holds a EUR rate (1.10 USD
// per EUR) because a second admitted symbol, XAUEUR, is quoted in EUR. EUR is then a THIRD currency for
// EURGBP: neither the account's nor the symbol's quote. Its rate is the book's rate for EUR, account
// units per ONE EUR, the same orientation as every rate here.

const XAUEUR: &str = "XAUEUR";
const EUR_RATE: f64 = 1.10;
const GBP_RATE: f64 = 1.35;

/// A USD engine holding EURGBP (GBP) and XAUEUR (EUR), both rates observed at [`T0`].
fn eur_gbp_engine() -> ExecutionEngine<RecordingClient> {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted(EURGBP, "GBP", GBP_RATE, T0)), AdmitOutcome::Admitted);
    assert_eq!(e.admit_symbol(&quoted(XAUEUR, "EUR", EUR_RATE, T0)), AdmitOutcome::Admitted);
    e
}

/// A fill on EURGBP with a commission of `commission` in `asset`; a distinct trade id per call.
fn eurgbp_fee(id: &'static str, commission: f64, asset: &str) -> FillEvent {
    FillEvent {
        commission,
        commission_asset: asset.into(),
        ..fill_event(id, EURGBP, 1, 1000.0, 0.85)
    }
}

/// [`eurgbp_fee`] stamped `ts`, the time a conversion at a stale rate is judged at.
fn eurgbp_fee_at(id: &'static str, commission: f64, asset: &str, ts: i64) -> FillEvent {
    FillEvent { ts, ..eurgbp_fee(id, commission, asset) }
}

fn funding_in(symbol: &str, amount: f64, ccy: &str) -> Event {
    funding_in_at(symbol, amount, ccy, T0)
}

fn funding_in_at(symbol: &str, amount: f64, ccy: &str, ts: i64) -> Event {
    match funding(symbol, amount) {
        Event::Funding(f) => Event::Funding(FundingEvent { ccy: Some(ccy.into()), ts, ..f }),
        _ => unreachable!("`funding` builds a funding event"),
    }
}

fn liquidation_in(symbol: &str, px: f64, fee: f64, ccy: &str, id: &str) -> Event {
    match liquidation(symbol, 0.0, px, fee, id) {
        Event::PositionLiquidated(l) => {
            Event::PositionLiquidated(PositionLiquidated { fee_ccy: Some(ccy.into()), ..l })
        }
        _ => unreachable!("`liquidation` builds a liquidation event"),
    }
}

/// A fee in a third currency the book holds a rate for is booked times THAT rate. The goldens: 10 EUR is
/// 11.00 USD (not 10.00 at par, not 9.09 from a flipped rate), 10 GBP is 13.50 USD, and the per-asset
/// tally keeps each amount in its own asset. Case does not make a second currency.
#[test]
fn a_fee_in_a_third_currency_with_a_rate_in_the_book_is_booked_at_that_rate() {
    let mut e = eur_gbp_engine();
    fold(&mut e, eurgbp_fee("f1", 10.0, "EUR"));
    let eur = 10.0 * EUR_RATE;
    assert_eq!(e.account.fees_paid.to_bits(), eur.to_bits());
    assert_eq!(e.account.balance.to_bits(), (0.0 - eur).to_bits());
    assert_eq!((eur * 100.0).round() / 100.0, 11.0, "10 EUR is 11.00 USD");
    assert_eq!(e.account.fees_by_asset[&ustr::ustr("EUR")], 10.0, "the tally keeps EUR");
    assert_eq!(e.account.fees_unconverted(), 0);

    // a venue's lower-case spelling is the same currency, not an unknown one
    fold(&mut e, eurgbp_fee("f2", 4.0, "eur"));
    let eur2 = 4.0 * EUR_RATE;
    assert_eq!(e.account.fees_paid.to_bits(), (eur + eur2).to_bits());
    assert_eq!(e.account.fees_unconverted(), 0);

    // the symbol's own quote currency takes its own rate, as before
    fold(&mut e, eurgbp_fee("f3", 10.0, "GBP"));
    let gbp = 10.0 * GBP_RATE;
    assert_eq!(e.account.fees_paid.to_bits(), (eur + eur2 + gbp).to_bits());
    assert_eq!((gbp * 100.0).round() / 100.0, 13.5, "10 GBP is 13.50 USD");
    assert_eq!(e.account.balance.to_bits(), (0.0 - eur - eur2 - gbp).to_bits());
    assert_eq!(e.account.fees_unconverted(), 0);
    assert_eq!(e.account.fees_at_stale_rate(), 0, "both rates are fresh");

    // a REBATE (negative fee) keeps its sign: income, converted the same way
    fold(&mut e, eurgbp_fee("f4", -2.0, "EUR"));
    assert_eq!(e.account.fees_paid.to_bits(), (eur + eur2 + gbp + -2.0 * EUR_RATE).to_bits());
}

/// The book has no rate for the fee's currency: the fee stays unconverted, exactly as before this phase
/// (a rate for another third currency is no help). Counted, tallied raw, not booked.
#[test]
fn a_fee_in_a_third_currency_with_no_rate_in_the_book_stays_unconverted() {
    let mut e = engine();
    assert_eq!(e.admit_symbol(&quoted(EURGBP, "GBP", GBP_RATE, T0)), AdmitOutcome::Admitted);
    fold(&mut e, eurgbp_fee("f1", 10.0, "EUR"));
    assert_eq!(e.account.balance, 0.0, "not booked at par");
    assert_eq!(e.account.fees_paid, 0.0);
    assert_eq!(e.account.fees_by_asset[&ustr::ustr("EUR")], 10.0);
    assert_eq!(e.account.fees_unconverted(), 1);

    // a rate for ANOTHER currency does not stand in for EUR
    assert_eq!(e.admit_symbol(&quoted(USDJPY, "JPY", 1.0 / 150.0, T0)), AdmitOutcome::Admitted);
    fold(&mut e, eurgbp_fee("f2", 10.0, "EUR"));
    assert_eq!(e.account.balance, 0.0);
    assert_eq!(e.account.fees_unconverted(), 2);

    // ...and once EUR has a rate the very same fee books
    assert_eq!(e.admit_symbol(&quoted(XAUEUR, "EUR", EUR_RATE, T0)), AdmitOutcome::Admitted);
    fold(&mut e, eurgbp_fee("f3", 10.0, "EUR"));
    assert_eq!(e.account.fees_paid.to_bits(), (10.0 * EUR_RATE).to_bits());
    assert_eq!(e.account.fees_unconverted(), 2);
}

/// A rate that was already STALE when the event happened (older than the window at the event's own
/// `ts`) still converts, at its last value, and the booking is FLAGGED; it is never refused or denied
/// for the age of a fee currency's rate. The gate judges an order on its OWN symbol's currency
/// (`quote_leg`), which is fresh here. The flag is judged on the side "the rate was old when it
/// happened": a fill from before the rate (history replayed after a reconnect) is not stale; an event
/// stating no time is (next test).
#[test]
fn a_fee_converted_at_a_rate_already_stale_when_it_happened_books_at_the_last_rate_and_flags() {
    let mut e = eur_gbp_engine();
    let later = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 1;
    e.now_ms = later;
    // GBP (EURGBP's own currency) is renewed; EUR is left to age
    refresh(&mut e, EURGBP, "GBP", GBP_RATE, later);

    fold(&mut e, eurgbp_fee_at("f1", 10.0, "EUR", later));
    assert_eq!(e.account.fees_paid.to_bits(), (10.0 * EUR_RATE).to_bits(), "the last rate");
    assert_eq!(e.account.fees_at_stale_rate(), 1);
    assert_eq!(e.account.fees_unconverted(), 0);

    assert_eq!(e.account.quote_leg(EURGBP, later), (GBP_RATE, false), "EURGBP's own rate is fresh");
    assert!(e.account.quote_leg(XAUEUR, later).1, "EUR is stale: XAUEUR's opens would be denied");

    // a fee at a fresh rate is not flagged
    fold(&mut e, eurgbp_fee_at("f2", 10.0, "GBP", later));
    assert_eq!(e.account.fees_at_stale_rate(), 1);
    // exactly one window after the rate is still inside it
    fold(&mut e, eurgbp_fee_at("f3", 10.0, "EUR", T0 + DEFAULT_QUOTE_RATE_TTL_MS));
    // an event from before the rate (a replayed fill from history) is not stale
    fold(&mut e, eurgbp_fee_at("f4", 10.0, "EUR", T0 - 5 * DEFAULT_QUOTE_RATE_TTL_MS));
    assert_eq!(e.account.fees_at_stale_rate(), 1);
    assert_eq!(e.account.fees_unconverted(), 0, "all of them booked");
}

/// The event's time decides the flag and NOTHING a booked number reads: the same fill and funding book
/// the same bits and hash whether the event happened inside the window or long after it (a replay
/// judges the flag identically, from the journaled event, and cannot move the ledger).
#[test]
fn the_event_time_flags_a_conversion_but_never_changes_what_is_booked() {
    let book = |ts: i64| {
        let mut e = eur_gbp_engine();
        fold(&mut e, eurgbp_fee_at("f1", 10.0, "EUR", ts));
        e.on_event(&funding_in_at(EURGBP, -5.0, "EUR", ts), &mut Outbox::default());
        (
            e.account.balance,
            e.account.fees_paid,
            e.account.funding_paid,
            e.account.fees_at_stale_rate(),
            state_hash(&[e.snapshot_state()]),
        )
    };
    let fresh = book(T0);
    let stale = book(T0 + DEFAULT_QUOTE_RATE_TTL_MS + 1);
    assert_eq!(fresh.0.to_bits(), stale.0.to_bits());
    assert_eq!(fresh.1.to_bits(), stale.1.to_bits());
    assert_eq!(fresh.2.to_bits(), stale.2.to_bits());
    assert_eq!((fresh.3, stale.3), (0, 2), "only the flag moves: the fee and the funding");
    assert_eq!(fresh.4, stale.4, "the flag is not state: the hash a replay verifies is one");
}

/// Funding that STATES its currency converts by it, sign kept (paid is negative, received positive): in
/// the account's currency as stated, in a currency the book holds a rate for times that rate, in one it
/// does not hold left out and counted; stating none stays left out (the existing test above).
#[test]
fn funding_converts_by_the_currency_it_states() {
    let mut e = eur_gbp_engine();
    let mut want = 0.0;

    e.on_event(&funding_in(EURGBP, -5.0, "EUR"), &mut Outbox::default());
    want += -5.0 * EUR_RATE;
    e.on_event(&funding_in(EURGBP, 3.0, "eur"), &mut Outbox::default());
    want += 3.0 * EUR_RATE;
    assert_eq!(e.account.funding_paid.to_bits(), want.to_bits(), "paid 5.50 out, received 3.30 in");
    assert_eq!(e.account.balance.to_bits(), want.to_bits());
    assert_eq!((want * 100.0).round() / 100.0, -2.2);

    // the account's currency: as stated, whatever the case
    e.on_event(&funding_in(EURGBP, -2.0, "USD"), &mut Outbox::default());
    e.on_event(&funding_in(EURGBP, -1.0, "usd"), &mut Outbox::default());
    want += -2.0;
    want += -1.0;
    // the symbol's own quote currency
    e.on_event(&funding_in(EURGBP, -10.0, "GBP"), &mut Outbox::default());
    want += -10.0 * GBP_RATE;
    assert_eq!(e.account.funding_paid.to_bits(), want.to_bits());
    assert_eq!(e.account.balance.to_bits(), want.to_bits());
    assert_eq!(e.account.fees_unconverted(), 0);

    // a currency the book holds no rate for: left out, counted, nothing moves
    e.on_event(&funding_in(EURGBP, -100.0, "SEK"), &mut Outbox::default());
    assert_eq!(e.account.funding_paid.to_bits(), want.to_bits());
    assert_eq!(e.account.fees_unconverted(), 1);

    // an account-quoted symbol converts a funding that states a foreign currency too: the stated
    // currency is not the account's, so booking it as stated would be par
    e.admit_symbol(&plain("GBPUSD"));
    e.on_event(&funding_in("GBPUSD", -4.0, "EUR"), &mut Outbox::default());
    want += -4.0 * EUR_RATE;
    assert_eq!(e.account.funding_paid.to_bits(), want.to_bits());
}

/// F7. A coin-settled (INVERSE) contract on an account-quoted symbol states its funding in the coin, and
/// the single-currency ledger books that at the event's mark. A book that also holds a foreign-quoted
/// symbol must not turn it into "no rate for BTC": the exception the commission of such a contract
/// already has (`Account::fx_commission`). The exception is the inverse contract's alone, so a LINEAR
/// account-quoted symbol funding in a currency the book cannot convert is still left out.
#[test]
fn a_coin_funding_on_an_inverse_account_quoted_symbol_books_at_the_mark_like_the_old_ledger() {
    let mut e = eur_gbp_engine();
    let inverse =
        SymbolAdmission { contract_value: Some(ContractValue::inverse(1.0)), ..plain("BTCUSD") };
    assert_eq!(e.admit_symbol(&inverse), AdmitOutcome::Admitted);
    e.admit_symbol(&plain("GBPUSD"));
    let coin = |amount: f64| match funding_in("BTCUSD", amount, "BTC") {
        Event::Funding(f) => Event::Funding(FundingEvent { mark_price: Some(50_000.0), ..f }),
        _ => unreachable!("`funding_in` builds a funding event"),
    };

    e.on_event(&coin(-0.001), &mut Outbox::default());
    let booked = -0.001_f64 * 50_000.0;
    assert_eq!(e.account.funding_paid.to_bits(), booked.to_bits(), "0.001 BTC at 50,000 is 50 USD");
    assert_eq!(e.account.balance.to_bits(), booked.to_bits());
    assert_eq!(e.account.fees_unconverted(), 0, "nothing was left out");

    // the same currency on a LINEAR account-quoted symbol has no rate to convert at: left out
    e.on_event(&funding_in("GBPUSD", -1.0, "BTC"), &mut Outbox::default());
    assert_eq!(e.account.funding_paid.to_bits(), booked.to_bits());
    assert_eq!(e.account.fees_unconverted(), 1);
}

/// A forced-close fee converts the same way, and the position closes at the converted PnL either way.
#[test]
fn a_forced_close_fee_converts_by_the_currency_it_states() {
    let mut e = eur_gbp_engine();
    fold(&mut e, fill_event("o1", EURGBP, 1, Q, 0.85));
    e.on_event(&liquidation_in(EURGBP, 0.84, 12.0, "EUR", "L1"), &mut Outbox::default());
    assert_eq!(size(&e, EURGBP), Some(0.0), "the position closed");
    assert_eq!(
        e.account.balance.to_bits(),
        (0.0 - 12.0 * EUR_RATE).to_bits(),
        "12 EUR is 13.20 USD"
    );
    assert_eq!(e.account.fees_unconverted(), 0);

    // no rate for the stated currency: the fee is left out, the close is not undone
    fold(&mut e, fill_event("o2", EURGBP, 1, Q, 0.85));
    let before = e.account.balance;
    e.on_event(&liquidation_in(EURGBP, 0.84, 12.0, "SEK", "L2"), &mut Outbox::default());
    assert_eq!(size(&e, EURGBP), Some(0.0));
    assert_eq!(e.account.balance.to_bits(), before.to_bits());
    assert_eq!(e.account.fees_unconverted(), 1);
}

/// F3. A fee in a third currency on an ACCOUNT-QUOTED symbol of a ledger that holds a foreign-quoted one
/// follows the same rule as the funding above: the book holds a EUR rate, so 10 EUR is 11.00 USD (not
/// 10.00 at par); a currency it holds no rate for is left out and counted; the account's own currency
/// and a fee naming none are as stated. Funding stated in EUR on the same symbol books the same 11.00.
#[test]
fn a_third_currency_fee_on_an_account_quoted_symbol_converts_like_funding_does() {
    let mut e = eur_gbp_engine();
    let mounted = crate::quote_currency::MOUNTED;
    let fee = |id: &'static str, amount: f64, asset: &str| FillEvent {
        commission: amount,
        commission_asset: asset.into(),
        ..fill_event(id, mounted, 1, 1000.0, 1.1)
    };

    fold(&mut e, fee("m1", 10.0, "EUR"));
    let eur = 10.0 * EUR_RATE;
    assert_eq!(e.account.fees_paid.to_bits(), eur.to_bits(), "10 EUR is 11.00 USD, not 10.00");
    assert_eq!(e.account.balance.to_bits(), (0.0 - eur).to_bits());
    assert_eq!(e.account.fees_unconverted(), 0);

    // the account's own currency, and a fee that names none, are as stated
    fold(&mut e, fee("m2", 2.0, "USD"));
    fold(&mut e, fee("m3", 1.0, ""));
    assert_eq!(e.account.fees_paid.to_bits(), (eur + 2.0 + 1.0).to_bits());

    // no rate for SEK: left out, counted, not booked at par
    let before = e.account.balance;
    fold(&mut e, fee("m4", 100.0, "SEK"));
    assert_eq!(e.account.balance.to_bits(), before.to_bits());
    assert_eq!(e.account.fees_unconverted(), 1);

    // the funding rule agrees: 5 EUR paid out is 5.50 USD
    let funding_before = e.account.funding_paid;
    e.on_event(&funding_in(mounted, -5.0, "EUR"), &mut Outbox::default());
    assert_eq!(e.account.funding_paid.to_bits(), (funding_before + -5.0 * EUR_RATE).to_bits());

    // the per-mount ledger reads the very number the account booked
    let applied = e.account.applied_fx(&fee("m5", 10.0, "EUR"));
    assert_eq!(applied.charge.to_bits(), eur.to_bits());
}

/// F6. An event that states no time (`ts <= 0`) cannot show that the rate was fresh when it happened, so
/// it is FLAGGED like a stale one. It is still booked at the last rate: refusing a venue fee would
/// leave the balance short by an amount the venue charged, and a flag costs nothing the fee does not.
#[test]
fn an_event_with_no_time_is_flagged_as_stale_and_still_booked() {
    let mut e = eur_gbp_engine();
    fold(&mut e, eurgbp_fee_at("f1", 10.0, "EUR", T0));
    assert_eq!(e.account.fees_at_stale_rate(), 0, "PREMISE: a dated, fresh event is not flagged");
    fold(&mut e, eurgbp_fee_at("f2", 10.0, "EUR", 0));
    assert_eq!(e.account.fees_at_stale_rate(), 1, "no time, no proof of freshness");
    assert_eq!(e.account.fees_paid.to_bits(), (10.0 * EUR_RATE + 10.0 * EUR_RATE).to_bits());
    e.on_event(&funding_in_at(EURGBP, -5.0, "EUR", 0), &mut Outbox::default());
    assert_eq!(e.account.fees_at_stale_rate(), 2, "funding with no time is flagged too");
    assert_eq!(e.account.fees_unconverted(), 0, "all of it booked");
}

/// A ledger with NO foreign-quoted symbol is the single-currency ledger it always was: a stated
/// currency changes nothing there (the pin `no_foreign_symbol_is_unchanged` holds the journaled form).
#[test]
fn without_a_foreign_symbol_a_stated_currency_changes_nothing() {
    let mut e = engine();
    e.on_event(&funding_in(crate::quote_currency::MOUNTED, -5.0, "EUR"), &mut Outbox::default());
    assert_eq!(e.account.funding_paid, -5.0);
    assert_eq!(e.account.balance, -5.0);
    assert_eq!(e.account.fees_unconverted(), 0);
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
