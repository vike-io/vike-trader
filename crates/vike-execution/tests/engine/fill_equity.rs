//! **Between a fill and the next reconcile pass, equity tells the truth.** In `Authoritative` mode a close's PnL
//! enters `balance` only at the next pass's roll-forward, up to a minute away, and the margin sweep runs every 30 s. So
//! every equity reader takes the realized PnL since the balance was last set: the figure that roll will write. The
//! first test is the case that sells a healthy account, the second the case that admits too much.

use vike_execution::recon::{ReconcileBalanceCheck, ReconcileBalanceTolerance, diff_balance};
use vike_execution::testing::{EngineBuilder, RecordingClient};
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, MarginCall, MarkSource,
    PriceBoardConfig, check_margin_call_priced,
};
use vike_model::events::{AccountState, Event, FillEvent, LiquiditySide, PositionSide, TradeId};
use vike_model::{MarginCallConfig, OrderRequest, OrderType, RiskLimits};

const VENUE: &str = "binance";
const BTC: &str = "BTCUSDT.P";
const ETH: &str = "ETHUSDT.P";
const NOW: i64 = 1_000;

fn engine(limits: RiskLimits) -> Engine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: BTC.into(), limits, ..Default::default() }
            .build();
    e.extra_symbols.push(ETH.to_string());
    e.now_ms = NOW;
    e
}

fn wallet(figure: f64) -> AccountState {
    AccountState {
        venue: VENUE.into(),
        balances: vec![("USDT".to_string(), figure)],
        timestamp: NOW,
        route_key: None,
    }
}

fn fill(trade_id: &str, symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(trade_id).expect("a non-empty trade id"),
        order_client_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: NOW,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn fold(e: &mut Engine<RecordingClient>, event: Event) {
    let _ = e.on_event(&event, &mut EventBusOutbox::default());
}

fn mark(e: &mut Engine<RecordingClient>, symbol: &str, px: f64) {
    e.account.set_mark_from(VENUE, symbol, px, MarkSource::VenueMark, NOW);
    e.price_board.set_mark(VENUE, symbol, px, NOW);
}

fn config() -> PriceBoardConfig {
    PriceBoardConfig::default()
}

/// What the margin sweep judges (`vike_core`'s `sweep_margin_call_engine`): resolver-priced equity, 5% maintenance.
fn sweep(e: &Engine<RecordingClient>) -> MarginCall {
    let equity = e.resolved_equity(0.0, &config());
    check_margin_call_priced(
        &e.account,
        equity,
        &MarginCallConfig::default(),
        |(v, s, _side), p| e.resolved_position_price(v, s, p.size, &config()),
    )
}

/// Wallet 100, BTC long showing +1000, ETH notional 4000 (maintenance 200): the venue holds 1100.
fn long_two_symbols() -> Engine<RecordingClient> {
    let mut e = engine(RiskLimits::new());
    fold(&mut e, Event::AccountState(wallet(100.0)));
    fold(&mut e, Event::Fill(fill("btc-in", BTC, 1, 1.0, 1_000.0)));
    fold(&mut e, Event::Fill(fill("eth-in", ETH, 1, 2.0, 2_000.0)));
    mark(&mut e, BTC, 2_000.0);
    mark(&mut e, ETH, 2_000.0);
    assert_eq!(e.resolved_equity(0.0, &config()), 1_100.0, "PREMISE: wallet plus the open gain");
    assert_eq!(sweep(&e), MarginCall::Healthy, "PREMISE: healthy before the close");
    e
}

#[test]
fn a_winning_close_leaves_equity_where_it_was_and_the_sweep_sells_nothing() {
    let mut e = long_two_symbols();
    fold(&mut e, Event::Fill(fill("btc-out", BTC, -1, 1.0, 2_000.0)));

    assert_eq!(e.account.balance, 100.0, "the balance waits for the pass, as it always did");
    assert_eq!(e.resolved_equity(0.0, &config()), 1_100.0, "the sweep's figure");
    assert_eq!(e.sizing_equity(0.0, &config()), 1_100.0, "the gate's and the sizer's figure");
    assert_eq!(e.resolve_equity(0.0, &config()).equity, 1_100.0, "the screens' figure");
    assert_eq!(e.account.equity_all(0.0), 1_100.0, "a strategy's `on_fill` figure");
    assert_eq!(sweep(&e), MarginCall::Healthy, "a healthy account is not liquidated");
}

/// A breach with no realized gain behind it still liquidates: the same book with no close and a wallet that really
/// is 100 against 200 of maintenance.
#[test]
fn a_real_breach_still_liquidates() {
    let mut e = engine(RiskLimits::new());
    fold(&mut e, Event::AccountState(wallet(100.0)));
    fold(&mut e, Event::Fill(fill("eth-in", ETH, 1, 2.0, 2_000.0)));
    mark(&mut e, ETH, 2_000.0);
    assert!(matches!(sweep(&e), MarginCall::Liquidate(_)));
}

/// The reverse: a losing close must not leave the gate admitting against money the venue no longer holds.
#[test]
fn a_losing_close_lowers_equity_at_once_and_the_gate_refuses() {
    let limits = RiskLimits { initial_margin_requirement: Some(1.0), ..RiskLimits::new() };
    let mut e = engine(limits);
    fold(&mut e, Event::AccountState(wallet(2_000.0)));
    fold(&mut e, Event::Fill(fill("btc-in", BTC, 1, 1.0, 2_000.0)));
    mark(&mut e, BTC, 1_000.0);
    mark(&mut e, ETH, 1_000.0);
    fold(&mut e, Event::Fill(fill("btc-out", BTC, -1, 1.0, 1_000.0)));
    assert_eq!(e.resolved_equity(0.0, &config()), 1_000.0, "the venue holds 1000, not 2000");

    let order = OrderRequest {
        order_client_id: "c1".into(),
        venue: VENUE.into(),
        symbol: ETH.into(),
        side: 1,
        qty: 1.5,
        order_type: OrderType::Limit,
        price: Some(1_000.0),
        timestamp: NOW,
        ..Default::default()
    };
    let mut outbox = EventBusOutbox::default();
    e.submit_order(&order, NOW, &mut outbox);
    let denied = outbox.0.iter().find_map(|event| match event {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    });
    assert!(
        denied.is_some_and(|r| r.contains("insufficient-margin")),
        "1500 against 1000 of equity"
    );
}

/// No double count at the pass: equity is the figure the roll-forward writes, so the roll moves no bit of it, and the
/// venue's figure agrees with `expected`.
#[test]
fn the_pass_rolls_to_the_figure_equity_already_reads() {
    let mut e = long_two_symbols();
    fold(&mut e, Event::Fill(fill("btc-out", BTC, -1, 1.0, 2_000.0)));
    let before = e.resolved_equity(0.0, &config());

    let owned = e.local_view();
    let check = diff_balance(
        &owned.as_view(),
        Some(1_100.0),
        "USDT",
        ReconcileBalanceTolerance::default(),
        NOW,
    );
    let ReconcileBalanceCheck::Anchored { expected, drift: None } = check else {
        panic!("the venue agrees: {check:?}");
    };
    e.account.balance = expected;
    e.account.realized_pnl_at_balance_sync = Some(e.account.realized_pnl);

    assert_eq!(e.resolved_equity(0.0, &config()).to_bits(), before.to_bits());
}

/// A wallet frame with no fill behind it (funding, a transfer) is absolute. Whichever side of a close it lands on, the
/// fill is counted once, provided the frame states what the venue held when it was built.
#[test]
fn a_wallet_frame_on_either_side_of_a_close_counts_the_close_once() {
    let after_close = |frames_first: bool| {
        let mut a = Account::new(1.0, VENUE, None, BalanceMode::Delta);
        a.apply_account_state(&wallet(100.0), "USDT");
        a.apply_fill(&fill("in", BTC, 1, 1.0, 1_000.0));
        if frames_first {
            a.apply_account_state(&wallet(90.0), "USDT"); // 10 of funding, ahead of the close
            a.apply_fill(&fill("out", BTC, -1, 1.0, 2_000.0));
        } else {
            a.apply_fill(&fill("out", BTC, -1, 1.0, 2_000.0));
            a.apply_account_state(&wallet(1_090.0), "USDT"); // the same funding, and the close in it
        }
        a.equity_all(0.0)
    };
    assert_eq!(after_close(true), 1_090.0);
    assert_eq!(after_close(false), 1_090.0);
}

/// The anchor does not survive a restart, on purpose (the first pass adopts the venue's figure), so a restored account
/// reads the balance its snapshot holds until that pass. Pinned as what it is: the one window the term does not reach.
#[test]
fn a_restored_account_reads_its_snapshot_balance_until_the_first_pass() {
    let mut e = long_two_symbols();
    fold(&mut e, Event::Fill(fill("btc-out", BTC, -1, 1.0, 2_000.0)));
    let mut restored = Account::new(1.0, VENUE, None, BalanceMode::Delta);
    restored.restore_folded(&e.account.snapshot());
    assert_eq!(restored.realized_pnl_at_balance_sync, None);
    assert_eq!(restored.balance_mode, BalanceMode::Authoritative);
    assert_eq!(restored.equity_all(0.0), 100.0, "no mark restored by this path, no anchor either");
}
