//! A RESTING order (limit / stop) against an open position RESTS: `CtraderExec::submit` sends it as a
//! `ProtoOANewOrderReq`, and only a MARKET order is ever turned into a close-by-position-id. Before
//! this suite existed `submit` ran the reduce planner before it looked at the order type, so an
//! opposite-side stop or limit up to the open size (every bracket's stop-loss and take-profit leg,
//! `vike_model::build_bracket`: `reduce_only`, opposite side) became an IMMEDIATE market close and a
//! protective stop never protected.
//!
//! No socket: the client is built over a plain command channel (`CtraderExec::from_shared`), so each
//! test reads the `Command` the actor would have been given. The socket twins of the market paths
//! (`tests/exec_close.rs`) stay as they are; the market test here only pins that the fix left the
//! close route byte-for-byte. A `reduce_only` limit or stop carries the `positionId` of the open
//! position it closes on an account that can hold a hedge (sent plain, the venue OPENS an opposite
//! position when it triggers, measured 2026-10-25), and is refused by name when there is none to bind:
//! see `crates/bridges/ctrader/CLAUDE.md`'s "A hedged account binds a reduce-only resting exit". On an
//! account the venue stated NETTED it is the plain order it always was: the `_netted_` tests pin the
//! wire shape, the `_hedged_`/`bound` ones the binding. A bare symbol map states no account type and
//! counts as hedged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use vike_ctrader::conn::{Command, ConnShared};
use vike_ctrader::exec::{
    CtraderExec, EXIT_ALREADY_COVERED, EXIT_SPANS_POSITIONS, NO_POSITION_TO_BIND_AN_EXIT,
    NOTHING_TO_REDUCE, POSITIONS_UNVERIFIED,
};
use vike_ctrader::positions::{CloseTracker, PositionBook, TrackedPosition};
use vike_ctrader::proto::{
    ProtoOaAccountType, ProtoOaLightSymbol, ProtoOaNewOrderReq, ProtoOaOrderType, ProtoOaSymbol,
    ProtoOaTradeSide,
};
use vike_ctrader::symbols::{SymbolLists, SymbolMap};
use vike_exec::halt::HALT_REJECT_REASON;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

/// EURUSD, symbol id 1, five digits, a 0.01-lot grid (`100_000` centi-units = 1000 units). The
/// account type is the venue's word for it; `None` is a venue that states none (or a bare map).
fn symbols_of(account_type: Option<ProtoOaAccountType>) -> SymbolMap {
    SymbolMap::from_lists(&SymbolLists {
        light: vec![ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".into()),
            ..Default::default()
        }],
        full: vec![ProtoOaSymbol {
            symbol_id: 1,
            digits: 5,
            min_volume: Some(100_000),
            step_volume: Some(100_000),
            max_volume: Some(10_000_000_000),
            lot_size: Some(10_000_000),
            ..Default::default()
        }],
        account_type: account_type.map(|t| t as i32),
        ..Default::default()
    })
}

/// A venue that states no account type: read as one that can hold a hedge.
fn symbols() -> SymbolMap {
    symbols_of(None)
}

const FIRST: i64 = 700_001;
const SECOND: i64 = 700_002;

/// One seeded position: `(position_id, side, volume_centi)`, `side` `+1` long and `-1` short.
type Seed = (i64, i32, i64);

type Rig = (CtraderExec, mpsc::Receiver<Command>, tokio::sync::mpsc::Receiver<Ingest>);

/// A client over a command channel whose book holds the given LONG EURUSD positions
/// (`(position_id, volume_centi)`, the older first), and the channels to read what it did. Its
/// symbol map is a bare one, so it reports no account type: the client must treat the account as
/// HEDGED (`crates/bridges/ctrader/src/symbols.rs`'s `account_may_hold_a_hedge`).
fn rig(longs: &[(i64, i64)]) -> Rig {
    let seeds: Vec<Seed> = longs.iter().map(|&(id, volume)| (id, 1, volume)).collect();
    rig_book(&seeds, symbols())
}

/// [`rig`] on an account the venue stated NETTED, where a resting `reduce_only` exit is sent plain.
fn rig_netted(longs: &[(i64, i64)]) -> Rig {
    let seeds: Vec<Seed> = longs.iter().map(|&(id, volume)| (id, 1, volume)).collect();
    rig_book(&seeds, symbols_of(Some(ProtoOaAccountType::Netted)))
}

/// [`rig`] over any book (longs and shorts) and any symbol map. The halt sentinel is a path nobody
/// creates: the client is wired and the halt stays unengaged.
fn rig_book(seeds: &[Seed], symbols: SymbolMap) -> Rig {
    rig_book_parts(seeds, symbols).0
}

/// [`rig_book`] with the shared position book and order-id map (see [`Parts`]).
fn rig_book_parts(seeds: &[Seed], symbols: SymbolMap) -> Parts {
    let halt = std::env::temp_dir().join(format!("vike-ctrader-resting-{}", std::process::id()));
    rig_parts(seeds, symbols, halt.join("HALT"))
}

/// [`rig_book`] watching the sentinel at `halt` (engaged when the file exists).
fn rig_halted(seeds: &[Seed], symbols: SymbolMap, halt: std::path::PathBuf) -> Rig {
    rig_parts(seeds, symbols, halt).0
}

/// The shared position book and coid-to-orderId map a rig's client was built over, so a test can play
/// the actor's part (learn an order id, mark a position gone) and then read what the client does.
type Parts = (Rig, Arc<Mutex<PositionBook>>, Arc<Mutex<HashMap<String, i64>>>);

fn rig_parts(seeds: &[Seed], symbols: SymbolMap, halt: std::path::PathBuf) -> Parts {
    let (tx, commands) = mpsc::channel();
    let mut book = PositionBook::default();
    for (n, &(position_id, side, volume)) in seeds.iter().enumerate() {
        book.upsert(
            position_id,
            TrackedPosition { symbol_id: 1, side, volume, open_ts: 1_000 + n as i64 },
        );
    }
    let positions = Arc::new(Mutex::new(book));
    let orders = Arc::new(Mutex::new(HashMap::new()));
    let shared = ConnShared {
        tx,
        symbols: Arc::new(symbols),
        ctid: 99,
        orders: orders.clone(),
        positions: positions.clone(),
        closes: Arc::new(Mutex::new(CloseTracker::default())),
    };
    let (events, rx) = event_channel(64);
    // No actor answers this channel: a `reduce_only` market order that asks the venue for the positions
    // (`Command::ReadPositions`) must not sit out the production wait, so it is narrowed to a blink. The
    // tests that DO play the actor widen it again.
    let exec = CtraderExec::from_shared(shared, events)
        .with_halt_path(halt)
        .with_positions_read_wait(Duration::from_millis(20));
    ((exec, commands, rx), positions, orders)
}

/// What a submit put on the command channel: the new orders and the close legs, in order.
#[derive(Default)]
struct Sent {
    new_orders: Vec<ProtoOaNewOrderReq>,
    closes: Vec<(i64, i64)>,
    /// `(order_id, coid)` of every `CancelOrder`.
    cancels: Vec<(i64, String)>,
    /// `(coid, volume)` of every `AmendOrder`.
    amends: Vec<(String, Option<i64>)>,
    /// How many times the client asked the actor to read the venue's positions (`ReadPositions`).
    reads: usize,
}

fn sent(commands: &mpsc::Receiver<Command>) -> Sent {
    let mut out = Sent::default();
    while let Ok(c) = commands.try_recv() {
        match c {
            Command::ReadPositions { .. } => out.reads += 1,
            Command::NewOrder(o) => out.new_orders.push(o),
            Command::ClosePosition { position_id, volume, .. } => {
                out.closes.push((position_id, volume));
            }
            Command::CancelOrder { order_id, client_order_id } => {
                out.cancels.push((order_id, client_order_id));
            }
            Command::AmendOrder { client_order_id, req } => {
                out.amends.push((client_order_id, req.volume));
            }
            _ => {}
        }
    }
    out
}

fn rejected(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderRejected(r)) = ing {
            out.push(r.reason.to_string());
        }
    }
    out
}

/// A EURUSD SELL of `qty` units; the caller sets the type and prices.
fn sell(coid: &str, qty: f64, order_type: OrderType, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: -1,
        qty,
        order_type,
        reduce_only,
        ..Default::default()
    }
}

fn stop(coid: &str, qty: f64, trigger: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest { trigger_price: Some(trigger), ..sell(coid, qty, OrderType::Stop, reduce_only) }
}

fn limit(coid: &str, qty: f64, price: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest { price: Some(price), ..sell(coid, qty, OrderType::Limit, reduce_only) }
}

/// The bracket's stop-loss leg (`reduce_only` SELL STOP, sized to the entry) against the long it
/// guards RESTS as a STOP `NewOrder` at its trigger, on a NETTED account. It is not a market close:
/// nothing may go on the wire as a `ProtoOAClosePositionReq`.
#[test]
fn a_reduce_only_sell_stop_against_a_long_rests_as_a_stop_on_a_netted_account() {
    let (mut exec, commands, mut rx) = rig_netted(&[(FIRST, 100_000)]);
    exec.submit(&stop("sl-1", 1000.0, 1.0950, true));
    let got = sent(&commands);
    assert!(got.closes.is_empty(), "a resting stop was turned into a close: {:?}", got.closes);
    let [order] = &got.new_orders[..] else {
        panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
    };
    assert_eq!(order.order_type, ProtoOaOrderType::Stop as i32);
    assert_eq!(order.trade_side, ProtoOaTradeSide::Sell as i32);
    assert_eq!(order.volume, 100_000);
    assert_eq!(order.stop_price, Some(1.0950));
    assert_eq!(order.limit_price, None);
    assert_eq!(order.client_order_id.as_deref(), Some("sl-1"));
    assert_eq!(
        order.position_id, None,
        "no positionId: the wire shape of a resting order is as before"
    );
    assert!(rejected(&mut rx).is_empty(), "a resting exit is not a rejection");
}

/// The take-profit leg (`reduce_only` SELL LIMIT) rests as a LIMIT `NewOrder`, on a NETTED account.
#[test]
fn a_reduce_only_sell_limit_take_profit_rests_as_a_limit_on_a_netted_account() {
    let (mut exec, commands, mut rx) = rig_netted(&[(FIRST, 100_000)]);
    exec.submit(&limit("tp-1", 1000.0, 1.1500, true));
    let got = sent(&commands);
    assert!(got.closes.is_empty(), "a resting limit was turned into a close: {:?}", got.closes);
    let [order] = &got.new_orders[..] else {
        panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
    };
    assert_eq!(order.order_type, ProtoOaOrderType::Limit as i32);
    assert_eq!(order.limit_price, Some(1.1500));
    assert_eq!(order.stop_price, None);
    assert_eq!(order.volume, 100_000);
    assert_eq!(order.position_id, None);
    assert!(rejected(&mut rx).is_empty());
}

/// A PLAIN resting opposite order (no `reduce_only`) up to the open size rests too: it is an order
/// like any other, whose intent the flag did not state.
#[test]
fn a_plain_resting_opposite_limit_rests() {
    // The long is twice the order's size, so the order is a strict part of it (the grid minimum is
    // one thousand units, so the order cannot be smaller than that).
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 200_000)]);
    exec.submit(&limit("lim-1", 1000.0, 1.1500, false));
    let got = sent(&commands);
    assert!(got.closes.is_empty(), "a resting limit was turned into a close: {:?}", got.closes);
    let [order] = &got.new_orders[..] else {
        panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
    };
    assert_eq!(order.order_type, ProtoOaOrderType::Limit as i32);
    assert_eq!(order.volume, 100_000);
    assert_eq!(order.position_id, None);
    assert!(rejected(&mut rx).is_empty());
}

/// What this proves is the BRIDGE's pass-through, and only that: on a NETTED account it sends a
/// `reduce_only` resting exit at the size asked, checking neither the size nor the book (a stop spanning
/// two positions, and one twice the size of the only position, both go out as the one `NewOrder`, no
/// close and no rejection). It does NOT show the venue handles the oversized one well: a netted account
/// can FLIP when a stop larger than the position triggers, which nothing here measures.
#[test]
fn a_netted_account_gets_a_resting_exit_at_the_size_asked_with_no_check_against_the_book() {
    let (mut exec, commands, mut rx) = rig_netted(&[(FIRST, 60_000), (SECOND, 40_000)]);
    exec.submit(&stop("span-1", 1000.0, 1.0950, true)); // 100_000 over 60_000 + 40_000
    let (mut exec_big, commands_big, mut rx_big) = rig_netted(&[(FIRST, 100_000)]);
    exec_big.submit(&stop("big-1", 2000.0, 1.0950, true)); // 200_000 over one 100_000 position
    for (got, rx) in [(sent(&commands), &mut rx), (sent(&commands_big), &mut rx_big)] {
        assert!(got.closes.is_empty(), "a resting stop was turned into a close: {:?}", got.closes);
        let [order] = &got.new_orders[..] else {
            panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
        };
        assert_eq!(order.order_type, ProtoOaOrderType::Stop as i32);
        assert_eq!(order.position_id, None);
        assert!(rejected(rx).is_empty());
    }
}

/// A STOP-LIMIT and a TAKE-PROFIT trigger are kinds this bridge does not map. They are REJECTED
/// ("unmappable order"), as with no position open; they used to be market-closed before the mapper
/// ever saw them.
#[test]
fn an_unmapped_conditional_kind_is_rejected_not_market_closed() {
    for kind in [OrderType::StopLimit, OrderType::TakeProfit] {
        let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000)]);
        let req = OrderRequest {
            price: Some(1.0940),
            trigger_price: Some(1.0950),
            ..sell("odd-1", 1000.0, kind.clone(), true)
        };
        exec.submit(&req);
        let got = sent(&commands);
        assert!(got.closes.is_empty(), "{kind:?} was turned into a close: {:?}", got.closes);
        assert!(got.new_orders.is_empty(), "{kind:?} reached the wire");
        let reasons = rejected(&mut rx);
        assert_eq!(reasons.len(), 1, "{kind:?}: exactly one terminal rejection, got {reasons:?}");
        assert!(reasons[0].contains("unmappable"), "{kind:?}: {reasons:?}");
        assert!(
            reasons[0].contains(kind.as_str()),
            "{kind:?}: the rejection must name the order type it refuses, got {reasons:?}"
        );
    }
}

/// The unchanged half, pinned: a MARKET sell up to the open size closes by position id (no
/// `NewOrder`), the `reduce_only` market variant likewise and CAPPED at the position; a market sell
/// past the open size is a flip and stays a plain `NewOrder`.
#[test]
fn a_market_order_still_closes_by_position_id() {
    let (mut exec, commands, _rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&sell("mkt-1", 1000.0, OrderType::Market, false));
    let got = sent(&commands);
    assert_eq!(got.closes, vec![(FIRST, 100_000)]);
    assert!(got.new_orders.is_empty(), "a market flatten opened a hedge");

    let (mut exec, commands, _rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&sell("mkt-2", 5000.0, OrderType::Market, true)); // reduce_only caps at the position
    let got = sent(&commands);
    assert_eq!(got.closes, vec![(FIRST, 100_000)]);
    assert!(got.new_orders.is_empty());

    let (mut exec, commands, _rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&sell("mkt-3", 5000.0, OrderType::Market, false)); // a flip: a plain new order
    let got = sent(&commands);
    assert!(got.closes.is_empty());
    let [order] = &got.new_orders[..] else { panic!("a flip is one NewOrder") };
    assert_eq!(order.order_type, ProtoOaOrderType::Market as i32);
    assert_eq!(order.position_id, None);
}

/// A `reduce_only` exit with NO open position to bind to is refused by name and nothing is sent: sent
/// plain it would OPEN a position when it triggered (the 2026-10-25 hedge). Flat, or a book that holds
/// only the order's own side.
#[test]
fn a_reduce_only_resting_exit_with_no_position_to_bind_is_refused_by_name() {
    for (label, seeds) in
        [("flat", vec![]), ("only a short for a SELL", vec![(FIRST, -1, 100_000)])]
    {
        let (mut exec, commands, mut rx) = rig_book(&seeds, symbols());
        exec.submit(&stop("sl-none", 1000.0, 1.0950, true));
        let got = sent(&commands);
        assert!(got.new_orders.is_empty(), "{label}: a plain exit reached the venue");
        assert!(got.closes.is_empty(), "{label}");
        let reasons = rejected(&mut rx);
        assert_eq!(reasons, vec![NO_POSITION_TO_BIND_AN_EXIT.to_string()], "{label}");
        assert!(reasons[0].contains("HEDGED") && reasons[0].contains("nothing was sent"));
    }
}

/// **(c) Two INDEPENDENT stops on one position cannot exceed it together.** The second stop finds the
/// position's stop capacity used up and is refused by name ([`EXIT_ALREADY_COVERED`]); a partial first
/// stop leaves room, and a second one is CAPPED to what is left (then a third is refused). A take-profit
/// is a different kind and still binds.
#[test]
fn two_independent_stops_on_one_position_cannot_exceed_its_volume_together() {
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&stop("sl-a", 1000.0, 1.0950, true));
    exec.submit(&stop("sl-b", 1000.0, 1.0940, true));
    exec.submit(&limit("tp-a", 1000.0, 1.1100, true));
    let got = sent(&commands);
    assert_eq!(rejected(&mut rx), vec![EXIT_ALREADY_COVERED.to_string()], "only sl-b is refused");
    let ids: Vec<_> = got.new_orders.iter().map(|o| o.client_order_id.clone().unwrap()).collect();
    assert_eq!(ids, ["sl-a", "tp-a"]);

    // A position of 2000 units, a first stop of 1000: a second stop of 2000 is capped to the 1000 left.
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 200_000)]);
    exec.submit(&stop("sl-1", 1000.0, 1.0950, true));
    exec.submit(&stop("sl-2", 2000.0, 1.0940, true));
    exec.submit(&stop("sl-3", 1000.0, 1.0930, true));
    let got = sent(&commands);
    let volumes: Vec<_> = got.new_orders.iter().map(|o| (o.position_id, o.volume)).collect();
    assert_eq!(
        volumes,
        [(Some(FIRST), 100_000), (Some(FIRST), 100_000)],
        "capped at the room left"
    );
    assert_eq!(rejected(&mut rx), vec![EXIT_ALREADY_COVERED.to_string()], "the third is refused");
}

/// **(d) Two positions open: FIFO.** The first exit of a kind binds the OLDEST position, the next one
/// of that kind the next position, a third is refused; the take-profits do the same, independently.
#[test]
fn exits_take_the_oldest_position_first_and_the_next_exit_the_next_position() {
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000), (SECOND, 100_000)]);
    for coid in ["sl-a", "sl-b", "sl-c"] {
        exec.submit(&stop(coid, 1000.0, 1.0950, true));
    }
    for coid in ["tp-a", "tp-b"] {
        exec.submit(&limit(coid, 1000.0, 1.1100, true));
    }
    let got = sent(&commands);
    let bound: Vec<_> = got
        .new_orders
        .iter()
        .map(|o| (o.client_order_id.clone().unwrap(), o.position_id.unwrap()))
        .collect();
    assert_eq!(
        bound,
        [
            ("sl-a".to_string(), FIRST),
            ("sl-b".to_string(), SECOND),
            ("tp-a".to_string(), FIRST),
            ("tp-b".to_string(), SECOND)
        ]
    );
    assert_eq!(rejected(&mut rx), vec![EXIT_ALREADY_COVERED.to_string()], "sl-c: both are covered");
}

/// A position a MARKET close is already working on takes no new exit (it is going away), and the
/// refusal says so; an exit the mapper cannot build releases its claim on the position's room.
#[test]
fn a_position_closing_in_flight_takes_no_exit_and_a_failed_mapping_frees_the_room() {
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&sell("flat-1", 1000.0, OrderType::Market, true));
    exec.submit(&stop("sl-late", 1000.0, 1.0950, true));
    let got = sent(&commands);
    assert_eq!(got.closes, vec![(FIRST, 100_000)]);
    assert!(got.new_orders.is_empty());
    let reasons = rejected(&mut rx);
    assert_eq!(reasons.len(), 1);
    assert!(reasons[0].contains("closing in flight"), "{reasons:?}");

    // A stop with no trigger price cannot be mapped ("unmappable"); it must not keep the room.
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&OrderRequest { trigger_price: None, ..stop("sl-bad", 1000.0, 0.0, true) });
    exec.submit(&stop("sl-ok", 1000.0, 1.0950, true));
    let got = sent(&commands);
    let [order] = &got.new_orders[..] else { panic!("the good stop must bind") };
    assert_eq!((order.client_order_id.as_deref(), order.position_id), (Some("sl-ok"), Some(FIRST)));
    let reasons = rejected(&mut rx);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("unmappable"), "{reasons:?}");
}

/// One exit larger than any ONE position's room, with several positions having room, rests as ONE BOUND ORDER PER
/// POSITION (`vike_ctrader::exit_legs`), the oldest position first, each no larger than its position: the
/// engine's coid is not on the wire (each leg carries `<coid>L<n>`), nothing is rejected, and both positions are
/// covered afterwards. Before 0.1.97 it was refused by name and nothing was sent, because capping it would have
/// covered the oldest and left the others naked while the venue's accept (it carries no quantity) lets the engine
/// believe the whole size is protected.
#[test]
fn a_net_sized_exit_over_two_positions_rests_as_one_bound_order_per_position() {
    let ((mut exec, commands, mut rx), positions, _orders) =
        rig_book_parts(&[(FIRST, 1, 100_000), (SECOND, 1, 100_000)], symbols());
    exec.submit(&stop("sl-net", 2000.0, 1.0950, true));
    let got = sent(&commands);
    let wire: Vec<_> = got
        .new_orders
        .iter()
        .map(|o| (o.client_order_id.clone().unwrap(), o.position_id, o.volume, o.stop_price))
        .collect();
    assert_eq!(
        wire,
        [
            ("sl-netL1".to_string(), Some(FIRST), 100_000, Some(1.0950)),
            ("sl-netL2".to_string(), Some(SECOND), 100_000, Some(1.0950)),
        ],
        "one bound stop per position, the oldest position first"
    );
    assert!(rejected(&mut rx).is_empty(), "the split exit is not a rejection");
    assert_eq!(
        positions.lock().unwrap().live_exit_legs("sl-net"),
        Some(vec!["sl-netL1".to_string(), "sl-netL2".to_string()])
    );

    // Both positions are covered by it now: another stop of the same kind is refused as already covered.
    exec.submit(&stop("sl-late", 1000.0, 1.0940, true));
    assert_eq!(rejected(&mut rx), vec![EXIT_ALREADY_COVERED.to_string()]);
    assert!(sent(&commands).new_orders.is_empty());
}

/// A split exit larger than the rooms together covers what the rooms allow (`reduce_only` never closes more than
/// the positions hold, the warning says so), and a split that cannot give two legs of at least the minimum volume
/// on the grid is refused by name with nothing sent and no row left behind.
#[test]
fn a_split_exit_is_capped_at_the_rooms_and_refused_when_two_legs_do_not_fit() {
    let ((mut exec, commands, mut rx), positions, _orders) =
        rig_book_parts(&[(FIRST, 1, 100_000), (SECOND, 1, 100_000)], symbols());
    exec.submit(&stop("sl-big", 5000.0, 1.0950, true));
    let got = sent(&commands);
    let volumes: Vec<_> = got.new_orders.iter().map(|o| o.volume).collect();
    assert_eq!(volumes, [100_000, 100_000], "capped at what the two positions hold");
    assert!(rejected(&mut rx).is_empty());

    // 1500 units over a 1000-unit and a 500-unit position: the second leg is below the minimum volume.
    let ((mut exec, commands, mut rx), positions_small, _orders) =
        rig_book_parts(&[(FIRST, 1, 100_000), (SECOND, 1, 50_000)], symbols());
    exec.submit(&stop("sl-x", 1500.0, 1.0950, true));
    assert!(sent(&commands).new_orders.is_empty(), "nothing is sent");
    assert_eq!(rejected(&mut rx), vec![EXIT_SPANS_POSITIONS.to_string()]);
    let book = positions_small.lock().unwrap();
    assert_eq!(book.live_exit_legs("sl-x"), None, "no group");
    assert!(book.bound_exit("sl-xL1").is_none() && book.bound_exit("sl-x").is_none(), "no row");
    drop(book);
    assert!(positions.lock().unwrap().live_exit_legs("sl-big").is_some());
}

/// **Nothing vanishes.** A split exit that cannot be placed ends as ONE terminal `OrderRejected` of the engine's coid
/// and leaves no group and no row behind: a leg the mapper cannot build (here no trigger price) refuses the whole exit
/// before anything is sent, and a dead actor (its channel closed) does the same.
#[test]
fn a_split_exit_that_cannot_be_placed_is_one_terminal_reject_and_leaves_nothing_behind() {
    let ((mut exec, commands, mut rx), positions, _orders) =
        rig_book_parts(&[(FIRST, 1, 100_000), (SECOND, 1, 100_000)], symbols());
    exec.submit(&OrderRequest { trigger_price: None, ..stop("sl-bad", 2000.0, 0.0, true) });
    assert!(sent(&commands).new_orders.is_empty(), "no leg may be sent when one cannot be built");
    let reasons = rejected(&mut rx);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("unmappable"), "{reasons:?}");
    {
        let book = positions.lock().unwrap();
        assert_eq!(book.live_exit_legs("sl-bad"), None, "no group");
        assert!(book.bound_exit("sl-badL1").is_none() && book.bound_exit("sl-badL2").is_none());
    }

    // The room is free again: the same exit, well formed, splits.
    exec.submit(&stop("sl-ok", 2000.0, 1.0950, true));
    assert_eq!(sent(&commands).new_orders.len(), 2);

    // A dead actor: the first send fails, the exit is refused whole and its rows are forgotten.
    let ((mut exec, commands, mut rx), positions, _orders) =
        rig_book_parts(&[(FIRST, 1, 100_000), (SECOND, 1, 100_000)], symbols());
    drop(commands);
    exec.submit(&stop("sl-dead", 2000.0, 1.0950, true));
    let reasons = rejected(&mut rx);
    assert_eq!(reasons, vec!["ctrader actor thread gone".to_string()]);
    let book = positions.lock().unwrap();
    assert_eq!(book.live_exit_legs("sl-dead"), None);
    assert!(book.bound_exit("sl-deadL1").is_none() && book.bound_exit("sl-deadL2").is_none());
}

/// The engine cancels the exit it placed: every leg that still rests is cancelled, ALL OR NONE. A leg whose venue
/// `orderId` is not known yet refuses the whole cancel, non-terminally, and writes nothing; once both are known,
/// both are written.
#[test]
fn the_cancel_of_a_split_exit_cancels_every_leg_or_none() {
    let ((mut exec, commands, mut rx), _positions, orders) =
        rig_book_parts(&[(FIRST, 1, 100_000), (SECOND, 1, 100_000)], symbols());
    exec.submit(&stop("sl-net", 2000.0, 1.0950, true));
    let _ = sent(&commands);

    orders.lock().unwrap().insert("sl-netL1".into(), 801);
    exec.cancel("sl-net"); // the second leg's accept has not arrived
    assert!(sent(&commands).cancels.is_empty(), "no leg is cancelled alone");
    let mut refused = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderCancelRejected(r)) = ing {
            refused.push((r.client_order_id.to_string(), r.reason.to_string()));
        }
    }
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "sl-net", "the refusal is the engine's order's");

    orders.lock().unwrap().insert("sl-netL2".into(), 802);
    exec.cancel("sl-net");
    assert_eq!(
        sent(&commands).cancels,
        vec![(801, "sl-netL1".to_string()), (802, "sl-netL2".to_string())]
    );
}

/// A split exit is several orders: an amend of the engine's one order would resize it to one leg's size, so it is
/// refused, non-terminal, with nothing sent.
#[test]
fn a_split_exit_cannot_be_amended_in_place() {
    let ((mut exec, commands, mut rx), _positions, orders) =
        rig_book_parts(&[(FIRST, 1, 100_000), (SECOND, 1, 100_000)], symbols());
    let net = stop("sl-net", 2000.0, 1.0950, true);
    exec.submit(&net);
    {
        let mut ids = orders.lock().unwrap();
        ids.insert("sl-netL1".into(), 801);
        ids.insert("sl-netL2".into(), 802);
    }
    let _ = sent(&commands);
    exec.modify(&net, None, Some(1.0940));
    assert!(sent(&commands).amends.is_empty(), "nothing is amended");
    let mut refused = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderModifyRejected(r)) = ing {
            refused.push((r.client_order_id.to_string(), r.reason.to_string()));
        }
    }
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "sl-net");
    assert!(refused[0].1.contains("cannot be amended in place"), "{refused:?}");
}

/// **The cover count follows the REQUEST, not the confirmation.** From the amend request until the venue answers,
/// the exit counts at the larger of its size and the asked one: a second exit binding in that instant is refused
/// (it would otherwise see room the amend is about to take, and the pair would exceed the position). A refused
/// amend gives the room back.
#[test]
fn an_exit_counts_at_the_size_its_amend_asked_for_until_the_venue_answers() {
    let ((mut exec, commands, mut rx), positions, orders) =
        rig_book_parts(&[(FIRST, 1, 200_000)], symbols());
    let sl = stop("sl-1", 1000.0, 1.0950, true);
    exec.submit(&sl);
    orders.lock().unwrap().insert("sl-1".into(), 501);
    let _ = sent(&commands);

    exec.modify(&sl, Some(2000.0), None); // up to the whole position; the venue has not answered
    assert_eq!(sent(&commands).amends, vec![("sl-1".to_string(), Some(200_000))]);
    exec.submit(&stop("sl-2", 1000.0, 1.0940, true));
    assert_eq!(
        rejected(&mut rx),
        vec![EXIT_ALREADY_COVERED.to_string()],
        "the amend's 2000 units are counted before the venue confirms them"
    );
    assert!(sent(&commands).new_orders.is_empty());

    // The venue refuses the amend: sl-1 keeps its 1000 units and the room is back.
    positions.lock().unwrap().exit_amend_failed("sl-1");
    exec.submit(&stop("sl-3", 1000.0, 1.0930, true));
    let got = sent(&commands);
    let [order] = &got.new_orders[..] else {
        panic!("the room must be back after a refused amend")
    };
    assert_eq!((order.client_order_id.as_deref(), order.position_id), (Some("sl-3"), Some(FIRST)));
}

/// A trailing stop done as a cancel and an immediate new stop: the cancelled stop stops counting toward its
/// position's cover the moment the engine cancels it (the venue's `OrderCancelled` is a round trip away), so
/// the replacement binds; a cancel the venue REFUSES puts the old stop back in the cover.
#[test]
fn a_stop_cancelled_and_at_once_re_armed_binds_and_a_refused_cancel_puts_it_back() {
    let ((mut exec, commands, mut rx), positions, orders) =
        rig_book_parts(&[(FIRST, 1, 100_000)], symbols());
    exec.submit(&stop("sl-1", 1000.0, 1.0950, true));
    orders.lock().unwrap().insert("sl-1".into(), 501);
    let _ = sent(&commands);

    exec.submit(&stop("sl-late", 1000.0, 1.0940, true));
    assert_eq!(rejected(&mut rx), vec![EXIT_ALREADY_COVERED.to_string()], "covered while it rests");

    exec.cancel("sl-1");
    exec.submit(&stop("sl-2", 1000.0, 1.0960, true)); // the venue has not answered the cancel yet
    let got = sent(&commands);
    assert_eq!(got.cancels, vec![(501, "sl-1".to_string())]);
    let [order] = &got.new_orders[..] else { panic!("the re-armed stop must bind") };
    assert_eq!((order.client_order_id.as_deref(), order.position_id), (Some("sl-2"), Some(FIRST)));
    assert!(rejected(&mut rx).is_empty());

    // The venue refuses the cancel: sl-1 still rests and covers again (sl-2 is the extra, over-cover).
    positions.lock().unwrap().cancel_failed("sl-1");
    positions.lock().unwrap().unbind_exit("sl-2");
    exec.submit(&stop("sl-3", 1000.0, 1.0930, true));
    assert_eq!(rejected(&mut rx), vec![EXIT_ALREADY_COVERED.to_string()]);
}

/// A bound exit may not be amended UP past the room its position has (the excess would open a
/// position when it triggered): the amend is refused, non-terminal, nothing sent; within the room it goes out.
#[test]
fn a_bound_exit_cannot_be_amended_above_the_room_its_position_has() {
    let ((mut exec, commands, mut rx), _positions, orders) =
        rig_book_parts(&[(FIRST, 1, 300_000)], symbols());
    let a = stop("sl-a", 1000.0, 1.0950, true);
    exec.submit(&a);
    exec.submit(&stop("sl-b", 1000.0, 1.0940, true));
    {
        let mut ids = orders.lock().unwrap();
        ids.insert("sl-a".into(), 601);
        ids.insert("sl-b".into(), 602);
    }
    let _ = sent(&commands);

    exec.modify(&a, Some(2500.0), None); // room: 3000 units less sl-b's 1000 = 2000
    assert!(sent(&commands).amends.is_empty(), "a raise above the room must not be sent");
    let mut refused = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderModifyRejected(r)) = ing {
            refused.push(r.reason.to_string());
        }
    }
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("room"), "{refused:?}");

    exec.modify(&a, Some(2000.0), None);
    assert_eq!(sent(&commands).amends, vec![("sl-a".to_string(), Some(200_000))]);
}

/// The mapper puts a volume on the grid by NEAREST step, which can round UP past the room: the exit is
/// clamped DOWN, on the grid, and what is sent is what is recorded as covering the position (a second
/// same-kind exit cannot then push the pair past it). Here a position of 1.5 steps and an exit for all of
/// it: the mapper's 2 steps would exceed the position; 1 step goes out, and the half step left is below the
/// minimum, so a second exit for it is refused as unmappable rather than rounded up.
#[test]
fn a_mapper_rounding_up_past_the_room_is_clamped_down_to_the_grid() {
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 150_000)]);
    exec.submit(&stop("sl-1", 1500.0, 1.0950, true));
    let got = sent(&commands);
    let [order] = &got.new_orders[..] else { panic!("the stop must bind") };
    assert_eq!(order.volume, 100_000, "clamped down to the grid, not rounded up to 200_000");
    assert_eq!(order.position_id, Some(FIRST));

    exec.submit(&stop("sl-2", 500.0, 1.0940, true)); // the half step left
    assert!(sent(&commands).new_orders.is_empty());
    let reasons = rejected(&mut rx);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("sub-minimum"), "{reasons:?}");
}

/// **The NETTED path is exactly what it always was**: a `reduce_only` limit or stop goes out as the
/// plain order the mapper builds, with NO `positionId`, whatever the book holds (flat, smaller than
/// the order, two positions) and with no refusal. Byte for byte: the wire struct equals the mapper's own
/// output for the same request. A hedged account's differs ONLY in the `positionId`.
#[test]
fn the_netted_path_sends_exactly_the_plain_order_the_mapper_builds() {
    let netted = symbols_of(Some(ProtoOaAccountType::Netted));
    let books: [(&str, Vec<Seed>); 4] = [
        ("flat", vec![]),
        ("one long", vec![(FIRST, 1, 100_000)]),
        ("off the grid and smaller than the order", vec![(FIRST, 1, 150_000)]),
        ("two longs", vec![(FIRST, 1, 100_000), (SECOND, 1, 100_000)]),
    ];
    for (label, seeds) in &books {
        for req in [stop("n-stop", 2000.0, 1.0950, true), limit("n-limit", 2000.0, 1.1100, true)] {
            let (mut exec, commands, mut rx) = rig_book(seeds, netted.clone());
            exec.submit(&req);
            let got = sent(&commands);
            let want = vike_ctrader::event_mapper::order_to_new_order(&req, 99, &netted)
                .expect("mappable");
            assert_eq!(
                got.new_orders,
                vec![want.clone()],
                "{label}: netted differs from the mapper"
            );
            assert_eq!(want.position_id, None);
            assert!(rejected(&mut rx).is_empty(), "{label}: a netted account refuses nothing");

            // The hedged twin differs in the position id (and the volume cap) only, where it binds (one
            // position to bind to); with two positions and an exit too big for either it is refused.
            if seeds.len() == 1 {
                let (mut hedged, commands, _rx) = rig_book(seeds, symbols());
                hedged.submit(&req);
                let got = sent(&commands);
                let [bound] = &got.new_orders[..] else { panic!("{label}: hedged binds") };
                assert_eq!(bound.position_id, Some(FIRST), "{label}");
                assert_eq!(
                    vike_ctrader::proto::ProtoOaNewOrderReq {
                        position_id: None,
                        volume: want.volume,
                        ..bound.clone()
                    },
                    want,
                    "{label}"
                );
            }
        }
    }
}

/// The bridge's own cancel of an exit whose position is gone, and the engine's OCO-sibling cancel
/// racing it. The actor's part is played by hand: the position leaves the book (what an event showing
/// it closed does), the exit is orphaned, and the engine's `cancel` of it writes NOTHING (the bridge
/// cancels it; two requests would only come back as a refusal). An exit that is not orphaned cancels
/// normally. (b) in the actor's own terms is `conn_tests`'s orphan tests.
#[test]
fn the_engines_cancel_of_an_orphaned_exit_is_not_written_a_second_time() {
    let ((mut exec, commands, _rx), positions, orders) =
        rig_book_parts(&[(FIRST, 1, 100_000)], symbols());
    exec.submit(&stop("sl-1", 1000.0, 1.0950, true));
    exec.submit(&limit("tp-1", 1000.0, 1.1100, true));
    {
        let mut ids = orders.lock().unwrap();
        ids.insert("sl-1".into(), 501);
        ids.insert("tp-1".into(), 502);
    }
    // sl-1 fills and closes the position: tp-1 is orphaned (sl-1 is the order of the event itself).
    positions.lock().unwrap().remove(FIRST);
    positions.lock().unwrap().orphan_exits_on(FIRST, Some("sl-1"));
    let _ = sent(&commands);
    exec.cancel("tp-1"); // the engine's OCO-sibling cancel
    exec.cancel("sl-1"); // not orphaned: the engine's cancel of an ordinary order still goes out
    let got = sent(&commands);
    assert_eq!(got.cancels, vec![(501, "sl-1".to_string())], "tp-1's cancel is the bridge's own");
}

/// A bound exit under an engaged HALT on a hedge-capable account: judged by the flag like any other
/// (admitted, then BOUND to the position), and with no position to bind it is refused by name, not by
/// the halt. (The netted twin is `a_reduce_only_resting_exit_on_a_netted_account_is_admitted_...`.)
#[test]
fn under_a_halt_a_hedged_exit_binds_and_without_a_position_is_refused_by_name() {
    let sentinel = tempfile::Builder::new()
        .prefix("vike-ctrader-bound-halt-")
        .tempfile()
        .expect("a temp sentinel file");
    let (mut exec, commands, mut rx) =
        rig_halted(&[(FIRST, 1, 100_000)], symbols(), sentinel.path().to_path_buf());
    exec.submit(&limit("plain-lim", 1000.0, 1.15, false)); // no flag: the halt refuses it
    exec.submit(&stop("sl-h", 1000.0, 1.09, true)); // flagged: admitted, then bound
    let got = sent(&commands);
    let [order] = &got.new_orders[..] else { panic!("only the flagged stop reaches the venue") };
    assert_eq!((order.client_order_id.as_deref(), order.position_id), (Some("sl-h"), Some(FIRST)));
    assert_eq!(rejected(&mut rx), vec![HALT_REJECT_REASON.to_string()]);

    let (mut exec, commands, mut rx) = rig_halted(&[], symbols(), sentinel.path().to_path_buf());
    exec.submit(&stop("sl-none", 1000.0, 1.09, true));
    assert!(sent(&commands).new_orders.is_empty());
    assert_eq!(rejected(&mut rx), vec![NO_POSITION_TO_BIND_AN_EXIT.to_string()]);
}

/// Under an engaged HALT a NETTED account is judged by the flag alone, as on every other venue: a PLAIN
/// resting opposite limit is refused with the halt reason, and a `reduce_only` resting SELL STOP is
/// admitted at its own size, uncapped (here larger than the long). The hedged twin, where the stop is
/// admitted and then bound, is `under_a_halt_a_hedged_exit_binds_and_without_a_position_is_refused_by_name`.
#[test]
fn a_reduce_only_resting_exit_on_a_netted_account_is_admitted_under_halt_by_the_flag_alone() {
    // A sentinel file that EXISTS (so the halt is engaged) and removes itself when the guard drops.
    let sentinel = tempfile::Builder::new()
        .prefix("vike-ctrader-resting-halt-")
        .tempfile()
        .expect("a temp sentinel file");
    let (mut exec, commands, mut rx) = rig_halted(
        &[(FIRST, 1, 100_000)],
        symbols_of(Some(ProtoOaAccountType::Netted)),
        sentinel.path().to_path_buf(),
    );
    exec.submit(&limit("plain-lim", 1000.0, 1.15, false));
    exec.submit(&stop("sl-5", 2000.0, 1.09, true));
    let got = sent(&commands);
    let [order] = &got.new_orders[..] else {
        panic!("only the reduce_only stop may reach the venue, got {}", got.new_orders.len());
    };
    assert_eq!(order.client_order_id.as_deref(), Some("sl-5"));
    assert_eq!(order.order_type, ProtoOaOrderType::Stop as i32);
    assert_eq!(order.volume, 200_000, "a resting exit under a halt is not capped to the position");
    assert_eq!(rejected(&mut rx), vec![HALT_REJECT_REASON.to_string()]);
}

/// The books the two tables below run over: five shapes, each tried on both sides, so half of the
/// (book, side) pairs have NO exposure facing the order (flat, a short for a SELL, a long for a BUY...).
fn books() -> [(&'static str, Vec<Seed>); 5] {
    [
        ("flat", vec![]),
        ("one long", vec![(FIRST, 1, 100_000)]),
        ("one short", vec![(FIRST, -1, 100_000)]),
        ("two longs", vec![(FIRST, 1, 100_000), (SECOND, 1, 100_000)]),
        ("a locked pair", vec![(FIRST, 1, 100_000), (SECOND, -1, 100_000)]),
    ]
}

/// The volume of `seeds` facing an order of `side` (a SELL faces the longs, a BUY the shorts).
fn facing(seeds: &[Seed], side: i32) -> i64 {
    seeds.iter().filter(|&&(_, s, _)| s == -side).map(|&(_, _, v)| v).sum()
}

/// **A `reduce_only` limit or stop is NEVER sent plain on an account that can hold a hedge, whatever
/// the book, side and size.** Each case is exactly one of: with a position facing it whose volume holds
/// the whole exit, BOUND to the OLDEST such one (`positionId` set, the size asked); with exactly one
/// position facing it that is too small, BOUND to it and CAPPED at its volume; with more than one facing
/// it and none holding the whole exit, SPLIT into one bound order per position (`vike_ctrader::exit_legs`: one
/// exit cannot span positions, and a cap would leave the others naked while the engine thinks them covered);
/// with none facing it, REFUSED by name (`NO_POSITION_TO_BIND_AN_EXIT`). The accounts are an EXPLICIT hedged one,
/// a spread-betting one and one the venue states nothing for (a bare map); each is read as able to hold
/// a hedge. The case counts are asserted, so no arm can be skipped silently.
#[test]
fn a_reduce_only_resting_exit_is_bound_or_refused_never_sent_plain_on_every_hedge_capable_account()
{
    let (mut bound_cases, mut capped_cases, mut none_cases, mut split_cases) = (0, 0, 0, 0);
    for account in [Some(ProtoOaAccountType::Hedged), Some(ProtoOaAccountType::SpreadBetting), None]
    {
        for (label, seeds) in &books() {
            for side in [-1, 1] {
                for kind in [OrderType::Limit, OrderType::Stop] {
                    for qty in [1000.0, 2000.0, 5000.0] {
                        let case =
                            format!("{account:?} / {label} / side {side} / {kind:?} / {qty} units");
                        let (mut exec, commands, mut rx) = rig_book(seeds, symbols_of(account));
                        let req = OrderRequest {
                            side,
                            price: Some(1.1000),
                            trigger_price: Some(1.1000),
                            ..sell("table-1", qty, kind.clone(), true)
                        };
                        exec.submit(&req);
                        let got = sent(&commands);
                        assert!(got.closes.is_empty(), "{case}: became a close");
                        let asked = (qty * 100.0) as i64;
                        // The positions facing the order, oldest first (the rig seeds the oldest first).
                        let facing: Vec<&Seed> = seeds.iter().filter(|s| s.1 == -side).collect();
                        let holds_all = facing.iter().find(|s| s.2 >= asked);
                        if let Some(&&(position_id, _, _)) = holds_all {
                            bound_cases += 1;
                            assert_bound(&case, &got, &mut rx, side, position_id, asked);
                        } else if let [&(position_id, _, volume)] = facing[..] {
                            capped_cases += 1;
                            assert_bound(&case, &got, &mut rx, side, position_id, volume);
                        } else if facing.is_empty() {
                            none_cases += 1;
                            assert!(got.new_orders.is_empty(), "{case}: a plain exit was sent");
                            let want = vec![NO_POSITION_TO_BIND_AN_EXIT.to_string()];
                            assert_eq!(rejected(&mut rx), want, "{case}");
                        } else {
                            // Several positions face it and none holds the whole exit: one bound order per
                            // position under the leg coids, oldest first, each at its position's volume.
                            split_cases += 1;
                            assert!(rejected(&mut rx).is_empty(), "{case}: refused");
                            let legs: Vec<_> = got
                                .new_orders
                                .iter()
                                .map(|o| {
                                    (o.client_order_id.clone().unwrap(), o.position_id, o.volume)
                                })
                                .collect();
                            let want: Vec<_> = facing
                                .iter()
                                .enumerate()
                                .map(|(n, &&(id, _, volume))| {
                                    (format!("table-1L{}", n + 1), Some(id), volume)
                                })
                                .collect();
                            assert_eq!(legs, want, "{case}");
                        }
                    }
                }
            }
        }
    }
    // Per account: 10 (book, side) pairs x 2 kinds x 3 sizes = 60 cases. 5 pairs face nothing (30 cases);
    // 4 pairs face ONE position (24 cases, always bound: whole at 1000 units, capped above);
    // the two-longs book facing a SELL (6 cases) binds the oldest at 1000 units and splits the rest over both.
    assert_eq!(
        (bound_cases, capped_cases, none_cases, split_cases),
        (3 * 10, 3 * 16, 3 * 30, 3 * 4),
        "the table shrank or moved"
    );
}

/// One bound `NewOrder`, for `position_id`, of `volume` centi-units, on `side`; nothing rejected.
fn assert_bound(
    case: &str,
    got: &Sent,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    side: i32,
    position_id: i64,
    volume: i64,
) {
    assert!(rejected(rx).is_empty(), "{case}: refused");
    let [order] = &got.new_orders[..] else { panic!("{case}: expected one bound NewOrder") };
    assert_eq!(order.position_id, Some(position_id), "{case}");
    assert_eq!(order.volume, volume, "{case}");
    let want = if side < 0 { ProtoOaTradeSide::Sell } else { ProtoOaTradeSide::Buy };
    assert_eq!(order.trade_side, want as i32, "{case}");
}

/// A `reduce_only` MARKET order on a hedged account, over the same books. **With exposure facing it
/// the order closes by position id and never goes out as a `NewOrder`**, capped at the exposure and at
/// the size asked and only on positions facing the other way. **With NONE tracked facing it, it is
/// REFUSED BY NAME and nothing is sent** (it used to go out as a plain market `NewOrder` and OPEN a
/// position beside what it meant to close, pinned here as expected-open until the 0.1.97 fix flipped
/// this arm). The rig's book was never fetched, so the client first asks the actor to read the venue's
/// positions (`Command::ReadPositions`, once), nobody answers, and the refusal is
/// [`POSITIONS_UNVERIFIED`]; the fetched-book and the answered-read arms are the tests below.
#[test]
fn a_reduce_only_market_order_closes_by_position_id_and_with_nothing_facing_it_is_refused_by_name()
{
    let (mut closed_cases, mut refused_cases) = (0, 0);
    for (label, seeds) in &books() {
        for side in [-1, 1] {
            let opposing = facing(seeds, side);
            for qty in [1000.0, 2000.0, 5000.0] {
                let case = format!("{label} / side {side} / {qty} units");
                let (mut exec, commands, mut rx) =
                    rig_book(seeds, symbols_of(Some(ProtoOaAccountType::Hedged)));
                exec.submit(&OrderRequest {
                    side,
                    ..sell("table-2", qty, OrderType::Market, true)
                });
                let got = sent(&commands);
                let reasons = rejected(&mut rx);
                if opposing > 0 {
                    assert!(reasons.is_empty(), "{case}: a market reduce was rejected");
                    assert_eq!(got.reads, 0, "{case}: the venue was asked with a position in hand");
                    closed_cases += 1;
                    assert!(got.new_orders.is_empty(), "{case}: a market reduce opened");
                    let closed: i64 = got.closes.iter().map(|&(_, v)| v).sum();
                    assert!(
                        closed > 0 && closed <= opposing.min((qty * 100.0) as i64),
                        "{case}: closed {closed} of {opposing} facing"
                    );
                    for (id, _) in &got.closes {
                        let leg = seeds.iter().find(|&&(p, _, _)| p == *id);
                        assert_eq!(leg.map(|&(_, s, _)| s), Some(-side), "{case}: leg {id}");
                    }
                } else {
                    refused_cases += 1;
                    assert!(got.closes.is_empty(), "{case}: closed what nothing tracks");
                    assert!(got.new_orders.is_empty(), "{case}: a plain market order was sent");
                    assert_eq!(got.reads, 1, "{case}: the venue is asked ONCE");
                    assert_eq!(reasons, vec![POSITIONS_UNVERIFIED.to_string()], "{case}");
                }
            }
        }
    }
    // Half the (book, side) pairs face nothing: the refusal arm is exercised as often as the close arm.
    assert_eq!((closed_cases, refused_cases), (15, 15), "the table shrank or moved");
}

/// A book the venue's own list was read into IN FULL (`replace_all`) and that shows nothing facing the
/// order is an answer, not silence: the order is refused by name ([`NOTHING_TO_REDUCE`]) with no read
/// and nothing sent. A book fetched WITH a position facing the order still closes it by id.
#[test]
fn an_authoritative_book_with_nothing_facing_a_reduce_only_market_order_refuses_it_without_asking()
{
    let ((mut exec, commands, mut rx), positions, _orders) =
        rig_book_parts(&[], symbols_of(Some(ProtoOaAccountType::Hedged)));
    positions.lock().unwrap().replace_all(Vec::<(i64, TrackedPosition)>::new(), 1_000);
    exec.submit(&sell("flat-none", 1000.0, OrderType::Market, true));
    let got = sent(&commands);
    assert!(got.new_orders.is_empty() && got.closes.is_empty(), "nothing may be sent");
    assert_eq!(got.reads, 0, "an authoritative book is not asked again");
    assert_eq!(rejected(&mut rx), vec![NOTHING_TO_REDUCE.to_string()]);

    // The same book, now holding the long it should close: the close route, as ever.
    positions.lock().unwrap().replace_all(
        [(FIRST, TrackedPosition { symbol_id: 1, side: 1, volume: 100_000, open_ts: 1 })],
        2_000,
    );
    exec.submit(&sell("flat-one", 1000.0, OrderType::Market, true));
    let got = sent(&commands);
    assert_eq!(got.closes, vec![(FIRST, 100_000)]);
    assert!(got.new_orders.is_empty() && got.reads == 0);
    assert!(rejected(&mut rx).is_empty());
}

/// **The point of the fix.** The venue holds a long this client's book never learned (a seed that failed
/// on a start that held a position). A `reduce_only` market SELL finds nothing facing it, asks the actor to
/// read the venue's positions, and CLOSES WHAT THE VENUE LISTS by position id: no `NewOrder`, no hedge. The
/// actor's part is played by a thread that folds the answer into the shared book the way `rebuild_position_book`
/// does and replies `true`.
#[test]
fn an_empty_unfetched_book_asks_the_venue_and_closes_what_it_lists_by_position_id() {
    let ((exec, commands, mut rx), positions, _orders) =
        rig_book_parts(&[], symbols_of(Some(ProtoOaAccountType::Hedged)));
    let mut exec = exec.with_positions_read_wait(Duration::from_secs(5));
    let book = positions.clone();
    let actor = std::thread::spawn(move || {
        let Command::ReadPositions { reply } =
            commands.recv_timeout(Duration::from_secs(5)).expect("the client asks the venue")
        else {
            panic!("the first command must be the positions read");
        };
        book.lock().unwrap().replace_all(
            [(FIRST, TrackedPosition { symbol_id: 1, side: 1, volume: 100_000, open_ts: 1 })],
            1_000,
        );
        reply.send(true).expect("the client is still waiting");
        commands
    });
    exec.submit(&sell("flat-seedless", 1000.0, OrderType::Market, true));
    let commands = actor.join().expect("the actor thread");
    let got = sent(&commands);
    assert_eq!(got.closes, vec![(FIRST, 100_000)], "closed by id what the venue listed");
    assert!(got.new_orders.is_empty(), "a plain market order OPENS a hedge: none may be sent");
    assert!(rejected(&mut rx).is_empty());

    // An answer that lists nothing facing the order: the book is now authoritative and flat, so the
    // refusal is the named one that says the list was read.
    let ((exec, commands, mut rx), positions, _orders) =
        rig_book_parts(&[], symbols_of(Some(ProtoOaAccountType::Hedged)));
    let mut exec = exec.with_positions_read_wait(Duration::from_secs(5));
    let book = positions.clone();
    let actor = std::thread::spawn(move || {
        let Command::ReadPositions { reply } =
            commands.recv_timeout(Duration::from_secs(5)).expect("the client asks the venue")
        else {
            panic!("the first command must be the positions read");
        };
        book.lock().unwrap().replace_all(Vec::<(i64, TrackedPosition)>::new(), 1_000);
        reply.send(true).expect("the client is still waiting");
        commands
    });
    exec.submit(&sell("flat-nothing", 1000.0, OrderType::Market, true));
    let commands = actor.join().expect("the actor thread");
    let got = sent(&commands);
    assert!(got.closes.is_empty() && got.new_orders.is_empty(), "nothing may be sent");
    assert_eq!(rejected(&mut rx), vec![NOTHING_TO_REDUCE.to_string()]);
}

/// Under an engaged HALT the exit is NEVER trapped by a book that may only be empty because the venue could
/// not be asked: the client still asks, closes by id what the venue lists, and when the list cannot be read
/// (or shows nothing) the order is admitted on its `reduce_only` flag and goes out as before, exactly as on
/// every other venue. A plain NETTED account is not asked at all and sends what it always sent.
#[test]
fn under_a_halt_the_exit_still_passes_and_a_netted_account_is_untouched() {
    let sentinel = tempfile::Builder::new()
        .prefix("vike-ctrader-market-halt-")
        .tempfile()
        .expect("a temp sentinel file");
    // HALT engaged, the venue cannot be read: admitted on the flag, as a plain market NewOrder.
    let (mut exec, commands, mut rx) = rig_halted(
        &[],
        symbols_of(Some(ProtoOaAccountType::Hedged)),
        sentinel.path().to_path_buf(),
    );
    exec.submit(&sell("halt-exit", 1000.0, OrderType::Market, true));
    let got = sent(&commands);
    assert_eq!(got.reads, 1, "the venue is asked first, even under a halt");
    let [order] = &got.new_orders[..] else { panic!("the exit must pass the halt") };
    assert_eq!(order.order_type, ProtoOaOrderType::Market as i32);
    assert_eq!(order.position_id, None);
    assert!(rejected(&mut rx).is_empty(), "a halt does not refuse a reduce_only exit");

    // A NETTED account: no read, no refusal, the plain market order it always sent.
    let (mut exec, commands, mut rx) = rig_netted(&[]);
    exec.submit(&sell("netted-exit", 1000.0, OrderType::Market, true));
    let got = sent(&commands);
    assert_eq!(got.reads, 0, "a netted account is not asked");
    assert_eq!(got.new_orders.len(), 1);
    assert!(rejected(&mut rx).is_empty());
}
