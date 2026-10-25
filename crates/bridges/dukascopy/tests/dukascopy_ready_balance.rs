//! **The sidecar's `ready` balance reaches the engine** (`crates/bridges/dukascopy/src/ready_balance.rs`), end to end
//! against the scripted fake bridge (`src/bin/fake_jforex_bridge.rs`: no Java, no network) and the real engine.
//!
//! The defect (measured 2026-10-20, demo account 8): the sidecar said `ready balance=10000.0` while the engine's
//! equity stayed the mount's SEED (1000), so a 1000-unit USDJPY order was denied `insufficient-margin`. The venue has
//! no reconcile leg, so nothing carried the figure; the start now sends it as ONE wallet frame, for a USD account on
//! a USD-quoted mount only. What this file proves is the Rust half over the real stdio protocol and the real
//! `ExecutionEngine`; DOCS-SHAPED for the Java half (`IAccount.getBalance()` is the previous end-of-day balance by
//! the vendor's Javadoc), unobserved on a live login beyond the one measured line.

use std::time::Duration;

use vike_dukascopy::home_conversion::AccountPerQuote;
use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AdmitOutcome, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Fold,
    Ingest, MarkSource, Outbox, RiskGate, SymbolAdmission, event_channel,
};
use vike_model::events::{AccountState, Event, FillEvent, PositionSide};
use vike_model::{OrderRequest, OrderType, RiskLimits, SymbolGrid};

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

const VENUE: &str = "dukascopy";
const MOUNTED: &str = "EURUSD";
/// The figure the fake's `ready` line states (`READY_BALANCE` there): the owner's account 8.
const READY_BALANCE: f64 = 10_000.0;
/// The mount's seed cash on the owner's box, and the engine's equity before the frame.
const SEED: f64 = 1_000.0;
/// The engine clock of every fixture, ms.
const T0: i64 = 1_760_000_000_000;

type Lane = tokio::sync::mpsc::Receiver<Ingest>;

fn config(mount: Option<&str>) -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
        ledger_mount: mount.map(str::to_string),
    }
}

/// A started fake sidecar in `mode` (empty = the default mode) mounted on `mount`, and the lane its events arrive on.
fn start(mode: &str, mount: Option<&str>) -> (DukascopyExecutionClient, Lane) {
    let (events, rx) = event_channel(64);
    let args: Vec<String> = if mode.is_empty() { vec![] } else { vec![mode.to_string()] };
    let client =
        DukascopyExecutionClient::spawn_with_program(BRIDGE, &args, &config(mount), events)
            .expect("the fake sidecar says ready");
    (client, rx)
}

/// The next event on the lane, or `None` after `wait`.
fn next_event(rx: &mut Lane, wait: Duration) -> Option<Event> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt.block_on(async { tokio::time::timeout(wait, rx.recv()).await }).ok()??;
    match ingest {
        Ingest::Event(event) => Some(event),
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

fn market_order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: MOUNTED.into(),
        side: 1,
        qty: 1000.0,
        order_type: OrderType::Market,
        ts: 1,
        ..Default::default()
    }
}

/// Everything the lane carries from the sidecar's `ready` through one order's terminal fill: the sidecar answers an
/// order only after its `ready` line, and the reader handles its lines in order, so a frame (if any) is among them.
fn a_session(client: &mut DukascopyExecutionClient, rx: &mut Lane) -> Vec<Event> {
    client.submit(&market_order("probe"));
    let mut seen = Vec::new();
    loop {
        let event = next_event(rx, Duration::from_secs(10)).expect("the session's events");
        let done = matches!(&event, Event::OrderFilled(f) if f.client_order_id == "probe");
        seen.push(event);
        if done {
            return seen;
        }
    }
}

fn wallet_frames(events: &[Event]) -> Vec<&AccountState> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::AccountState(a) => Some(a),
            _ => None,
        })
        .collect()
}

/// The wallet frame of a started sidecar: waits for it.
fn the_frame(rx: &mut Lane) -> Event {
    loop {
        let event = next_event(rx, Duration::from_secs(10)).expect("a wallet frame");
        if matches!(event, Event::AccountState(_)) {
            return event;
        }
    }
}

// ---- the bridge: when a frame is sent ------------------------------------------------------------

/// A USD account (the fake's default `ready` states USD) on a USD-quoted mount: ONE frame, the venue's id, dollars,
/// the stated figure, no account key (the mount's sender stamps it), and it precedes everything the sidecar says
/// afterwards. A second order in the same session adds none: it is one frame per sidecar START.
#[test]
fn a_usd_account_on_a_usd_mount_sends_one_wallet_frame_per_start() {
    let (mut client, mut rx) = start("", Some(MOUNTED));
    let mut events = a_session(&mut client, &mut rx);
    client.submit(&market_order("second"));
    while !matches!(
        events.last(),
        Some(Event::OrderFilled(f)) if f.client_order_id == "second"
    ) {
        events.push(next_event(&mut rx, Duration::from_secs(10)).expect("the second order"));
    }

    let frames = wallet_frames(&events);
    assert_eq!(frames.len(), 1, "one frame for the whole session: {events:?}");
    let frame = frames[0];
    assert_eq!(frame.venue, VENUE);
    assert_eq!(frame.balances, vec![("USD".to_string(), READY_BALANCE)]);
    assert_eq!(frame.route_key, None, "the mount's sender stamps the account, not the bridge");
    assert!(frame.ts > 0, "stamped with the clock");
    let frame_at = events.iter().position(|e| matches!(e, Event::AccountState(_))).unwrap();
    let accepted_at = events.iter().position(|e| matches!(e, Event::OrderAccepted(_))).unwrap();
    assert!(
        frame_at < accepted_at,
        "the frame comes with `ready`, before the sidecar's own events"
    );
    client.detach();
}

/// A client nobody told its mount (a tool, a smoke, every other test of this crate) sends none.
#[test]
fn a_client_with_no_mount_sends_no_frame() {
    let (mut client, mut rx) = start("", None);
    assert!(wallet_frames(&a_session(&mut client, &mut rx)).is_empty());
    client.detach();
}

/// Every case the gate withholds sends NOTHING, and the session carries on as it always did: another money (EUR,
/// and a word that is no code), no money stated (a jar from before the field), a balance that would deny every
/// order, and a mount that is not quoted in dollars (`USDJPY` counts in yen, `EURGBP` in pounds).
#[test]
fn the_cases_the_gate_withholds_send_no_frame() {
    for (mode, mount) in [
        ("ccy-eur", "EURUSD"),
        ("ccy-bad", "EURUSD"),
        ("old-jar", "EURUSD"),
        ("ticks-no-ccy", "EURUSD"),
        ("balance-zero", "EURUSD"),
        ("", "USDJPY"),
        ("", "EURGBP"),
    ] {
        let (mut client, mut rx) = start(mode, Some(mount));
        let events = a_session(&mut client, &mut rx);
        assert!(wallet_frames(&events).is_empty(), "mode {mode:?} on {mount}: {events:?}");
        assert!(
            events.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "{mode:?}: the sidecar itself is fine"
        );
        client.detach();
    }
}

// ---- the engine: what the frame changes ----------------------------------------------------------

/// The Dukascopy engine as the node assembles it: a USD ledger told its money, mounted on `EURUSD`, in the engine's
/// DELTA mode on a seed of 1000, margined at 1x (`vike-tradehub-core`'s `assemble_engine`: "1x = no leverage").
fn engine() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits { im_requirement: Some(1.0), ..RiskLimits::new() };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    );
    e.set_account_ccy("USD").expect("a currency code");
    e.equity_seed = SEED;
    e.now_ms = T0;
    e
}

/// USDJPY, admitted the way the node admits a foreign-quoted pair: yen per dollar 150.10 at the engine's clock.
fn admit_usdjpy(e: &mut ExecutionEngine<RecordingClient>) {
    let rate = AccountPerQuote::new(1.0 / 150.10).expect("a rate");
    let admission = SymbolAdmission {
        route_key: VENUE.into(),
        symbol: "USDJPY".into(),
        grid: SymbolGrid {
            tick_size: Some(0.001),
            lot_size: Some(1000.0),
            ..SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate.get()),
        rate_as_of_ms: Some(T0 - 200),
        im_requirement: None,
    };
    assert_eq!(e.admit_symbol(&admission), AdmitOutcome::Admitted);
    e.price_board.set_mark(VENUE, "USDJPY", 150.2, T0);
}

/// The owner's order: a market BUY of 1000 USDJPY. At 150.2 yen that is 1000.67 dollars of margin at 1x.
fn usdjpy_buy(coid: &str) -> OrderRequest {
    OrderRequest { symbol: "USDJPY".into(), ..market_order(coid) }
}

/// The reason the gate denied `req`, or `None` when it reached the venue client.
fn denial(e: &mut ExecutionEngine<RecordingClient>, req: &OrderRequest) -> Option<String> {
    let mut outbox = Outbox::default();
    e.submit_order(req, T0, &mut outbox);
    outbox.0.into_iter().find_map(|event| match event {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

fn fold(e: &mut ExecutionEngine<RecordingClient>, event: &Event) {
    assert!(matches!(e.on_event(event, &mut Outbox::default()), Fold::Applied), "{event:?}");
}

fn fill(trade_id: &'static str, side: i32, px: f64, commission: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: MOUNTED.into(),
        side,
        last_qty: 1000.0,
        last_px: px,
        commission,
        commission_asset: "USD".into(),
        liquidity_side: "taker".into(),
        ts: T0,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

fn close(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-9 * want.abs().max(1.0)
}

/// THE MEASURED DEFECT, end to end. Before the frame the engine's equity is the seed and the 1000-unit USDJPY order is
/// denied `insufficient-margin`; the real client's frame, folded, makes the engine's equity the venue's 10000 and the
/// same order passes. The seed (`equity_seed`, the term of the drawdown latch's capital base) is not touched.
#[test]
fn the_adopted_balance_lets_the_usdjpy_order_through_and_leaves_the_seed_alone() {
    let mut e = engine();
    admit_usdjpy(&mut e);
    assert!(close(e.account.equity_all(e.equity_seed), SEED));
    assert_eq!(
        denial(&mut e, &usdjpy_buy("before")).as_deref(),
        Some("insufficient-margin"),
        "on the seed alone the order is denied: the defect"
    );

    let (mut client, mut rx) = start("", Some(MOUNTED));
    fold(&mut e, &the_frame(&mut rx));

    assert_eq!(e.account.balance_mode, BalanceMode::Authoritative);
    assert_eq!(e.account.balance, READY_BALANCE);
    assert!(close(e.account.equity_all(e.equity_seed), READY_BALANCE), "equity is the venue's");
    assert_eq!(
        e.equity_seed, SEED,
        "the seed is untouched: the drawdown latch's capital base is what it was"
    );
    assert_eq!(
        denial(&mut e, &usdjpy_buy("after")),
        None,
        "adopted: the order passes the margin check"
    );
    client.detach();
}

/// A HOT START: the engine is live and already holds a position when the frame arrives. The balance is adopted, the
/// position is not touched, and equity is the adopted balance plus the position's unrealized PnL.
#[test]
fn a_frame_into_a_live_engine_adopts_the_balance_and_touches_no_position() {
    let mut e = engine();
    fold(&mut e, &fill("held", 1, 1.10, 0.0));
    e.account.set_mark_from(VENUE, MOUNTED, 1.11, MarkSource::VenueMark, 0);
    e.price_board.set_mark(VENUE, MOUNTED, 1.11, T0);
    let before = e.account.positions.clone();
    assert!(close(e.account.equity_all(e.equity_seed), SEED + 10.0), "seed + 1000 x 0.01");

    let (mut client, mut rx) = start("", Some(MOUNTED));
    fold(&mut e, &the_frame(&mut rx));

    assert_eq!(e.account.positions, before, "the position is exactly what it was");
    assert!(
        close(e.account.equity_all(e.equity_seed), READY_BALANCE + 10.0),
        "balance + unrealized"
    );
    assert_eq!(e.equity_seed, SEED);
    client.detach();
}

/// A RESTART re-adopts, and the accepted residual is exactly this: between starts the figure is not refreshed, so a
/// trade's realized PnL is OUT of equity (only its fee moved the balance) until the next start's frame sets the
/// balance absolutely again.
#[test]
fn a_restart_adopts_the_figure_again_and_realized_pnl_waits_for_it() {
    let mut e = engine();
    let (mut first, mut rx) = start("", Some(MOUNTED));
    fold(&mut e, &the_frame(&mut rx));
    assert!(close(e.account.equity_all(e.equity_seed), READY_BALANCE));

    // a round trip: +10 dollars realized, a 3 dollar fee
    fold(&mut e, &fill("in", 1, 1.10, 0.0));
    fold(&mut e, &fill("out", -1, 1.11, 3.0));
    assert!(close(e.account.realized_pnl, 10.0));
    assert!(
        close(e.account.equity_all(e.equity_seed), READY_BALANCE - 3.0),
        "the fee moved the balance; the realized 10 dollars is NOT in equity (the accepted residual)"
    );
    first.detach();

    let (mut second, mut rx) = start("", Some(MOUNTED));
    fold(&mut e, &the_frame(&mut rx));
    assert!(
        close(e.account.equity_all(e.equity_seed), READY_BALANCE),
        "the restart's frame sets the balance absolutely again"
    );
    assert!(
        e.account.realized_pnl_at_balance_sync.is_some_and(|baseline| close(baseline, 10.0)),
        "and re-anchors the sync baseline at the realized PnL so far: {:?}",
        e.account.realized_pnl_at_balance_sync
    );
    second.detach();
}
