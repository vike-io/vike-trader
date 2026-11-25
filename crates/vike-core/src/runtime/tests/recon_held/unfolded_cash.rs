//! **Equity between a fill and the next pass, and the cash of a fill the engine does not fold.** The sweep half: a close's
//! PnL counts in equity at once, so the margin clock cannot sell a healthy account in the minute before the pass rolls the
//! balance. The pass half: a fill or a liquidation the book did not fold (a hand trade on another symbol, a liquidation of a
//! position the book does not hold) moves the venue's wallet and nothing here, so under `quarantine` its `BalanceDrift`
//! would freeze the balance until an operator confirmed it; the next two passes take the venue's figure instead, whichever
//! way it moved, unless something else is owed for the account. Beside an open position that needs a venue whose figure is
//! wallet cash (`vike_venues::balance_is_wallet_cash`), the question decision 0179 left to a per-venue statement.

use super::*;
use vike_model::events::{Event, FillEvent, LiquiditySide, PositionLiquidated, PositionSide};

const OTHER: &str = "ETHUSDT";
const AT: i64 = 1_000;

fn fill_on(venue: &str, symbol: &str, tid: &str, side: i32, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: vike_model::events::TradeId::new(tid).expect("a non-empty trade id"),
        order_client_id: String::new(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: vike_marketdata::Symbol::new(""),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// A hand trade on a symbol this engine does not accept: the account-wide stream delivers it and the fold drops it.
fn dropped_fill(tid: &str) -> Event {
    fill_on(VENUE, OTHER, tid, 1, 1.0, 100.0)
}

fn liquidation(symbol: &str, tid: &str) -> Event {
    Event::PositionLiquidated(PositionLiquidated {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        qty: 1.0,
        liquidation_price: 100.0,
        fee: 0.0,
        timestamp: 1,
        trade_id: tid.into(),
        route_key: None,
        fee_currency: None,
    })
}

fn core_on(venue: &str) -> CoreThread<RecordingClient> {
    let engine = Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        SYMBOL,
    );
    core_of(engine, Vec::new(), CoreConfig::default())
}

/// Synced to 9_999, long 1 of the mounted symbol, the venue agreeing on both.
fn holding_core() -> CoreThread<RecordingClient> {
    let mut c = synced_core("quarantine");
    c.publish_to(0, bare_fill("own-in", 1, 1.0, 100.0));
    tick(&mut c, AT + PASS_MS, held_pass(9_999.0));
    assert!(c.reconcile_alerts.is_empty(), "PREMISE: nothing is held: {:?}", held_kinds(&c));
    c
}

fn held_pass(cash: f64) -> ReconcileReports {
    cash_pass("quarantine", cash, vec![position(SYMBOL, 1.0, AT)])
}

fn resolved(c: &CoreThread<RecordingClient>) -> f64 {
    c.engine(0).resolved_equity(0.0, &c.config.price_config)
}

#[traced_test]
#[test]
fn a_dropped_fill_reaches_the_balance_at_the_next_pass_beside_an_open_position() {
    let mut c = holding_core();
    c.publish_to(0, dropped_fill("hand-1"));

    tick(&mut c, AT + 2 * PASS_MS, held_pass(9_500.0));
    assert_eq!(c.engine(0).account.balance, 9_500.0, "the venue's figure, down, with no operator");
    assert!(c.reconcile_alerts.is_empty(), "nothing is held: {:?}", held_kinds(&c));

    tick(&mut c, AT + 3 * PASS_MS, held_pass(9_500.0));
    assert_eq!(c.engine(0).account.balance, 9_500.0, "the venue agrees from here on");
    assert!(c.reconcile_alerts.is_empty());

    let notes = absorbed_notes(&c);
    assert_eq!(notes.len(), 1, "said once: {notes:?}");
    assert!(
        notes[0].contains("absorbed -499 unexplained") && notes[0].contains("did not fold"),
        "the amount and the reason: {}",
        notes[0]
    );
    logs_assert(|lines: &[&str]| match count_at(lines, "WARN", ABSORBED_LINE) {
        1 => Ok(()),
        n => Err(format!("one ABSORBED line at WARN, got {n}")),
    });
}

/// The control, and decision 0179's rule: with no dropped fill behind it a decrease is a surprise, and it stays held.
#[test]
fn with_nothing_dropped_a_decrease_stays_held() {
    let mut c = holding_core();
    tick_held(&mut c, AT + 2 * PASS_MS, held_pass(9_500.0));
    assert_eq!(c.engine(0).account.balance, 9_999.0);
    assert_eq!(held_kinds(&c), vec![ReconcileDivergenceKind::BalanceDrift]);
}

/// The balance read that closes a pass can have left before the fill it should show: that pass agrees, and the next one
/// still heals it. Two passes and no more, so a later move with no fill behind it is held again.
#[test]
fn the_leave_outlasts_a_read_taken_ahead_of_the_fill_and_then_runs_out() {
    let mut c = holding_core();
    c.publish_to(0, dropped_fill("hand-1"));
    tick(&mut c, AT + 2 * PASS_MS, held_pass(9_999.0));
    tick(&mut c, AT + 3 * PASS_MS, held_pass(9_500.0));
    assert_eq!(c.engine(0).account.balance, 9_500.0, "the second pass still takes it");

    let mut c = holding_core();
    c.publish_to(0, dropped_fill("hand-1"));
    tick(&mut c, AT + 2 * PASS_MS, held_pass(9_999.0));
    tick(&mut c, AT + 3 * PASS_MS, held_pass(9_999.0));
    tick_held(&mut c, AT + 4 * PASS_MS, held_pass(9_500.0)); // an ordinary drift: held on its second pass
    assert_eq!(c.engine(0).account.balance, 9_999.0, "the third does not");
    assert_eq!(held_kinds(&c), vec![ReconcileDivergenceKind::BalanceDrift]);
}

/// A fill the book folded, and its re-delivery, leave nothing to take.
#[test]
fn a_folded_fill_and_its_redelivery_grant_nothing() {
    let mut c = holding_core();
    c.publish_to(0, bare_fill("own-2", 1, 1.0, 100.0));
    c.publish_to(0, bare_fill("own-2", 1, 1.0, 100.0));
    let row = position(SYMBOL, 2.0, AT);
    tick_held(&mut c, AT + 2 * PASS_MS, cash_pass("quarantine", 9_500.0, vec![row]));
    assert_eq!(c.engine(0).account.balance, 9_999.0);
    assert_eq!(held_kinds(&c), vec![ReconcileDivergenceKind::BalanceDrift]);
}

#[test]
fn a_liquidation_on_another_symbol_reaches_the_balance() {
    let mut c = holding_core();
    c.publish_to(0, liquidation(OTHER, "liq-1"));
    tick(&mut c, AT + 2 * PASS_MS, held_pass(9_000.0));
    assert_eq!(c.engine(0).account.balance, 9_000.0);
    assert!(c.reconcile_alerts.is_empty());
}

/// The shape a restart under `quarantine` leaves: the venue's position was never adopted, so its liquidation closes
/// nothing here and `apply_liquidation` books nothing, not even the fee.
#[test]
fn a_liquidation_of_a_position_the_book_does_not_hold_reaches_the_balance() {
    let mut c = synced_core("quarantine");
    c.publish_to(0, liquidation(SYMBOL, "liq-1"));
    tick(&mut c, AT + PASS_MS, balance_pass("quarantine", 9_000.0));
    assert_eq!(c.engine(0).account.balance, 9_000.0);
    assert!(c.reconcile_alerts.is_empty());
}

/// Something else owed for the account blocks it, as it blocks a deposit: the venue's figure may hold that money too, and
/// a later confirm would count it twice.
#[test]
fn a_row_that_owes_money_holds_it() {
    let mut c = holding_core();
    c.publish_to(0, dropped_fill("hand-1"));
    let row = position(SYMBOL, 2.0, AT);
    tick(&mut c, AT + 2 * PASS_MS, cash_pass("quarantine", 9_500.0, vec![row]));
    assert_eq!(c.engine(0).account.balance, 9_999.0);
    assert!(
        held_kinds(&c).contains(&ReconcileDivergenceKind::BalanceDrift),
        "{:?}",
        held_kinds(&c)
    );
}

/// Hyperliquid's classic figure may hold unrealized PnL (decision 0179), so beside a position it is not taken. A flat book
/// has no position to cause cash, so it is, on the leave's second pass.
#[test]
fn a_venue_whose_figure_is_not_wallet_cash_waits_for_a_flat_book() {
    const HL: &str = "hyperliquid";
    let on_hl = |cash: f64, qty: f64| ReconcileReports {
        venue: HL.into(),
        positions: vec![vike_model::PositionStatusReport {
            venue: HL.into(),
            ..position(SYMBOL, qty, AT)
        }],
        ..balance_pass("quarantine", cash)
    };
    let mut c = core_on(HL);
    tick(&mut c, AT, on_hl(9_999.0, 0.0));
    c.publish_to(0, fill_on(HL, SYMBOL, "own-in", 1, 1.0, 100.0));
    c.publish_to(0, fill_on(HL, OTHER, "hand-1", 1, 1.0, 100.0));

    tick(&mut c, AT + PASS_MS, on_hl(9_500.0, 1.0));
    assert_eq!(c.engine(0).account.balance, 9_999.0, "beside the position it is held");
    assert_eq!(held_kinds(&c), vec![ReconcileDivergenceKind::BalanceDrift]);

    c.publish_to(0, fill_on(HL, SYMBOL, "own-out", -1, 1.0, 100.0));
    tick(&mut c, AT + 2 * PASS_MS, on_hl(9_500.0, 0.0));
    assert_eq!(c.engine(0).account.balance, 9_500.0, "flat, it is taken");
    assert!(c.reconcile_alerts.is_empty(), "{:?}", held_kinds(&c));
}

/// A spot position moves a wallet by its whole notional, on a venue whose perpetual lane is wallet cash too.
#[test]
fn a_cash_position_beside_it_holds_it() {
    let mut c = holding_core();
    const SPOT: &str = "BTCUSDT";
    c.engine_mut(0).account.positions.insert(
        (VENUE.into(), SPOT.into(), PositionSide::Both),
        vike_execution::PositionEntry {
            size: 1.0,
            average_price: 100.0,
            margin_mode: vike_model::MarginMode::Cash,
            ..Default::default()
        },
    );
    c.publish_to(0, dropped_fill("hand-1"));
    let rows = vec![position(SYMBOL, 1.0, AT), position(SPOT, 1.0, AT)];
    tick(&mut c, AT + 2 * PASS_MS, cash_pass("quarantine", 9_500.0, rows));
    assert_eq!(c.engine(0).account.balance, 9_999.0);
    assert_eq!(held_kinds(&c), vec![ReconcileDivergenceKind::BalanceDrift]);
}

#[test]
fn only_the_quarantine_word_takes_it() {
    let mut c = synced_core("external-quarantine");
    c.publish_to(0, dropped_fill("hand-1"));
    tick(&mut c, AT + PASS_MS, balance_pass("external-quarantine", 9_500.0));
    assert_eq!(c.engine(0).account.balance, 9_999.0);
    assert_eq!(held_kinds(&c), vec![ReconcileDivergenceKind::BalanceDrift]);
}

// --- the margin clock ------------------------------------------------------------------------------------------------

/// Wallet 100, the mounted symbol long showing +1000, ETH notional 4000: the review's example, on the live sweep.
fn two_symbols_core() -> CoreThread<RecordingClient> {
    let mut c = core_on(VENUE);
    c.config.margin_call = Some(vike_model::MarginCallConfig::default());
    c.engine_mut(0).extra_symbols.push(OTHER.to_string());
    tick(&mut c, AT, balance_pass("quarantine", 100.0));
    c.publish_to(0, fill_on(VENUE, SYMBOL, "btc-in", 1, 1.0, 1_000.0));
    c.publish_to(0, fill_on(VENUE, OTHER, "eth-in", 1, 2.0, 2_000.0));
    c.engine_mut(0).price_board.set_mark(VENUE, SYMBOL, 2_000.0, AT);
    c.engine_mut(0).price_board.set_mark(VENUE, OTHER, 2_000.0, AT);
    c
}

#[test]
fn the_margin_clock_sells_nothing_after_a_winning_close() {
    let mut c = two_symbols_core();
    c.sweep_risk_clock(AT);
    assert!(c.engine(0).client.submissions.is_empty(), "PREMISE: healthy before the close");

    c.publish_to(0, fill_on(VENUE, SYMBOL, "btc-out", -1, 1.0, 2_000.0));
    assert_eq!(resolved(&c), 1_100.0, "the venue holds 1100");
    c.sweep_risk_clock(AT + 30_000);
    assert!(
        c.engine(0).client.submissions.is_empty(),
        "a healthy account was sold: {:?}",
        c.engine(0).client.submissions
    );

    // ...and the pass moves nothing: it rolls to the figure equity already reads.
    tick(&mut c, AT + PASS_MS, balance_pass("quarantine", 1_100.0));
    assert_eq!(c.engine(0).account.balance, 1_100.0);
    assert_eq!(resolved(&c), 1_100.0, "counted once");
    assert!(c.reconcile_alerts.is_empty());
    c.sweep_risk_clock(AT + 2 * PASS_MS);
    assert!(c.engine(0).client.submissions.is_empty());
}

/// The sweep still does its job: the same book with the gain never made.
#[test]
fn the_margin_clock_still_sells_a_real_breach() {
    let mut c = core_on(VENUE);
    c.config.margin_call = Some(vike_model::MarginCallConfig::default());
    c.engine_mut(0).extra_symbols.push(OTHER.to_string());
    tick(&mut c, AT, balance_pass("quarantine", 100.0));
    c.publish_to(0, fill_on(VENUE, OTHER, "eth-in", 1, 2.0, 2_000.0));
    c.engine_mut(0).price_board.set_mark(VENUE, OTHER, 2_000.0, AT);
    c.sweep_risk_clock(AT);
    assert!(!c.engine(0).client.submissions.is_empty(), "100 of equity against 200 of maintenance");
}

/// `flags.reconcile_balance` off, the default outside the CI box: every pass takes the venue's figure and must anchor there, or
/// a close after it is out of equity until the next pass.
#[test]
fn the_seed_pass_anchors_so_a_close_after_it_counts_once() {
    let off = |cash: f64| ReconcileReports {
        reconcile_balance: false,
        ..balance_pass("quarantine", cash)
    };
    let mut c = core_on(VENUE);
    tick(&mut c, AT, off(1_000.0));
    c.publish_to(0, fill_on(VENUE, SYMBOL, "in", 1, 1.0, 100.0));
    c.publish_to(0, fill_on(VENUE, SYMBOL, "out", -1, 1.0, 150.0));
    assert_eq!(resolved(&c), 1_050.0, "the close's 50 counts before the next pass");

    tick(&mut c, AT + PASS_MS, off(1_050.0));
    assert_eq!(c.engine(0).account.balance, 1_050.0);
    assert_eq!(resolved(&c), 1_050.0, "counted once");
}
