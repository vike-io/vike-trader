//! The MARK FLOOR through the REAL runtime, in the shape of a binance LANE TWIN: `spawn_core_multi`
//! over the venue's default engine (`binance` on `BTCUSDT`) and a labelled one (`binance#PERP` on
//! `BTCUSDT.P`), BOTH at the `live` tier with their ceilings armed, and the floor on for the TWIN alone
//! (`RiskGate::require_mark`, what `vike_tradehub_core`'s `assemble_engine` does for the derived lane
//! twin engine and for no other).
//!
//! `Ingest::Quote/Trade/Book` reach only the venue's DEFAULT engine, and the default engine does not
//! accept `BTCUSDT.P`, so the twin's own symbol is never priced by the tick lanes. Before the floor, a
//! market order there was judged at notional zero and every cap passed it. What must hold:
//!
//! - no mark: a market BUY far above the cap is DENIED by name and nothing rests;
//! - after ONE mark (the mark lane, the way the node's twin mitigation hands one in): the same order is
//!   `over-max-notional` and a small one rests;
//! - a covered reduce goes through with no mark at all, while an opening order is still denied;
//! - the venue's DEFAULT engine, which the ticks do reach and which is NOT armed, is judged as it always
//!   was: an order before its first quote is not held by the floor, and once priced the ceilings bind.

use vike_core::spawn_core_multi;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, MarketTick, OrderIntent, RiskGate,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{AccountTier, OrderRequest, OrderType, RiskLimits};

use crate::kit::engines::test_config;
use crate::kit::handle::wait_for_snapshot;

const VENUE: &str = "binance";
const SPOT: &str = "BTCUSDT";
const PERP: &str = "BTCUSDT.P";
/// 0.1 BTC at the mark is over it, 0.01 BTC is far under.
const CAP: f64 = 5_000.0;
const MARK: f64 = 74_300.0;

/// Switch the floor on: what the live assembly does for the derived lane twin engine, and for no other.
fn arm(e: &mut ExecutionEngine<RecordingClient>) {
    e.gate.require_mark = true;
}

/// A `live`-tier binance engine under `route_key` on `symbol`, ceilings armed; `floor` says whether the
/// mark floor is on (the production shape: the twin's, not the default spot engine's).
fn engine(route_key: &str, symbol: &str, floor: bool) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        max_notional_per_order: Some(CAP),
        max_total_exposure: Some(CAP),
        ..RiskLimits::new()
    };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        VENUE,
        symbol,
    );
    e.route_key = route_key.into();
    e.tier = AccountTier::Live;
    if floor {
        arm(&mut e);
    }
    e
}

/// The twin shape: the default spot engine (NOT armed) and the labelled perp one (armed).
fn core() -> vike_core::CoreHandle {
    spawn_core_multi(
        engine(VENUE, SPOT, false),
        vec![(1_000.0, engine("binance#PERP", PERP, true))],
        test_config(10_000.0),
    )
}

/// An order on the TWIN's account (`label` names the account; `None` is the default account).
fn order(
    label: Option<&str>,
    symbol: &str,
    coid: &str,
    side: i32,
    qty: f64,
    reduce_only: bool,
) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: OrderType::Market,
        reduce_only,
        account: Some(match label {
            None => AccountLabel::Default,
            Some(l) => AccountLabel::Named(l.into()),
        }),
        ..Default::default()
    })))
}

fn perp_order(coid: &str, side: i32, qty: f64, reduce_only: bool) -> Command {
    order(Some("PERP"), PERP, coid, side, qty, reduce_only)
}

fn denied_with(s: &vike_exec::CoreSnapshot, coid: &str, word: &str) -> bool {
    let head = format!("OrderDenied {coid}");
    s.recent_events.iter().any(|l| l.contains(&head) && l.contains(word))
}

fn perp_fill(qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: "t1".into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: PERP.into(),
        side: 1,
        last_qty: qty,
        last_px: 74_000.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// **The hole, closed.** The twin has no price at all: the market buy that no cap would let through at
/// a mark is denied by NAME, and it never rests.
#[test]
fn with_no_tick_a_market_buy_above_the_cap_is_denied_by_name_and_nothing_rests() {
    let handle = core();
    let cell = handle.snapshot_cell();
    // A quote for the twin's symbol is dropped by the route: the default engine does not accept it.
    handle.send_command(perp_order("big", 1, 0.1, false));
    wait_for_snapshot(&cell, 5, "the denial by name", |s| {
        denied_with(s, "big", "no-mark-for-mounted-symbol")
    });
    handle.shutdown_and_join();
    assert!(cell.load_full().orders.is_empty(), "a denied order never enters the registry");
}

/// **After one mark the ceilings judge at it**: the big order is `over-max-notional`, the small one
/// rests. The mark rides the mark lane, which mirrors a venue price to every engine of the exchange.
#[test]
fn after_one_mark_the_big_order_is_over_the_cap_and_the_small_one_rests() {
    let handle = core();
    let cell = handle.snapshot_cell();
    handle.market_sender().publish(MarketTick {
        venue: VENUE.into(),
        symbol: PERP.into(),
        px: MARK,
        ts: 1,
    });
    handle.send_command(perp_order("big", 1, 0.1, false));
    handle.send_command(perp_order("small", 1, 0.01, false));
    wait_for_snapshot(&cell, 5, "the cap denial and the small order resting", |s| {
        denied_with(s, "big", "over-max-notional")
            && s.orders.iter().any(|o| o.client_order_id == "small" && o.status.is_live())
    });
    handle.shutdown_and_join();
    let snap = cell.load_full();
    assert!(!denied_with(&snap, "big", "no-mark-for-mounted-symbol"), "{:?}", snap.recent_events);
    assert_eq!(snap.orders.len(), 1, "only the small order rests");
}

/// A position must stay closable with NO price: the covered reduce rests while an opening order on the
/// same unmarked engine is still denied by name.
#[test]
fn a_covered_reduce_rests_with_no_mark_while_an_opening_order_is_denied() {
    let handle = core();
    let cell = handle.snapshot_cell();
    // Only the twin claims `BTCUSDT.P`, so the venue fill routes to it by symbol.
    handle.event_sender().blocking_send(perp_fill(0.05)).unwrap();
    wait_for_snapshot(&cell, 5, "the twin's position to be published", |s| {
        s.portfolio.venues.get(1).is_some_and(|b| b.positions.iter().any(|p| p.symbol == PERP))
    });
    handle.send_command(perp_order("open", 1, 0.01, false));
    handle.send_command(perp_order("close", -1, 0.05, true));
    wait_for_snapshot(&cell, 5, "the denial and the close resting", |s| {
        denied_with(s, "open", "no-mark-for-mounted-symbol")
            && s.orders.iter().any(|o| o.client_order_id == "close" && o.status.is_live())
    });
    handle.shutdown_and_join();
    assert_eq!(cell.load_full().orders.len(), 1, "only the close rests");
}

/// The venue's DEFAULT engine is reached by the tick lanes and is NOT armed: before its first quote an
/// order is judged exactly as it always was (a tiny one rests, no floor word), and once a quote prices it
/// the ceilings bind.
#[test]
fn the_default_engine_is_not_held_by_the_floor_and_is_priced_by_its_quote() {
    let handle = core();
    let cell = handle.snapshot_cell();
    handle.send_command(order(None, SPOT, "early", 1, 0.001, false));
    wait_for_snapshot(&cell, 5, "the early order resting on the unarmed default engine", |s| {
        s.orders.iter().any(|o| o.client_order_id == "early" && o.status.is_live())
    });
    assert!(
        !denied_with(&cell.load_full(), "early", "no-mark-for-mounted-symbol"),
        "the default engine is not armed"
    );
    handle
        .tick_sender()
        .quote(vike_exec::QuoteUpdate {
            venue: VENUE.into(),
            symbol: SPOT.into(),
            quote: vike_marketdata::QuoteTick {
                ts: 2,
                local_ts: 2,
                bid: MARK,
                ask: MARK,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: SPOT.into(),
            },
        })
        .unwrap();
    handle.send_command(order(None, SPOT, "big", 1, 0.1, false));
    handle.send_command(order(None, SPOT, "small", 1, 0.01, false));
    wait_for_snapshot(&cell, 5, "the cap denial and the small order resting", |s| {
        denied_with(s, "big", "over-max-notional")
            && s.orders.iter().any(|o| o.client_order_id == "small" && o.status.is_live())
    });
    handle.shutdown_and_join();
}
