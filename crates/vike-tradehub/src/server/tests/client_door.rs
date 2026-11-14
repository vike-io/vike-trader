//! **What a client's preview says of an order, held to what the door does with it.** Each side has its own copy of
//! the choice (the door's facts are not on the frame, the client names no `VenueBlock`), so one table runs both: the
//! guardrail against the door's ceiling, and the engine a preview names against the one `lane_addressed` picks.

use super::*;
use crate::publish::{named_orders, project};
use crate::server::symbol_admission::lane_addressed;
use vike_client::addressed::{contract_value_of, guardrail_of};
use vike_client::verbs::{GuardrailCapabilities, bare_venue_refusal};
use vike_execution::{OrderView, VenueBlock};
use vike_model::{ContractValue, OrderStatus, OrderType};
use vike_tradehub_client::wire::{CoreSnapshotWire, OrderBracketWire};

const CEILING: f64 = 100_000.0;
const HELD_PRICE: f64 = 50_000.0;

fn frame_of(blocks: &[VenueBlock], orders: &[OrderView]) -> CoreSnapshotWire {
    let mut snap = vike_execution::CoreSnapshot::empty("bybit", "BTCUSDT");
    snap.portfolio.venues = blocks.to_vec();
    snap.orders = orders.to_vec();
    project(&snap, None, &[])
}

fn holding(mut block: VenueBlock, symbol: &str, size: f64) -> VenueBlock {
    block.positions.push(vike_execution::PositionView {
        venue: block.venue.as_str().into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
        size,
        average_price: HELD_PRICE,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liquidation_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    });
    block
}

fn order(venue: &str, symbol: &str, side: i32, qty: f64, price: Option<f64>) -> CommandWire {
    let CommandWire::Submit(mut req) = submit(venue, symbol, None) else { unreachable!() };
    req.side = side;
    req.qty = qty;
    req.price = price;
    req.order_type = if price.is_some() { "limit" } else { "market" }.into();
    CommandWire::Submit(req)
}

fn flagged(cmd: CommandWire) -> CommandWire {
    let CommandWire::Submit(mut req) = cmd else { unreachable!() };
    req.reduce_only = true;
    CommandWire::Submit(req)
}

fn bracket(qty: f64, entry: Option<f64>, stop: f64, target: f64) -> CommandWire {
    CommandWire::Bracket(OrderBracketWire {
        order_client_id: None,
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        entry_price: entry,
        stop_loss: stop,
        take_profit: target,
        account: None,
    })
}

fn resting_sell(order_client_id: &str) -> OrderView {
    OrderView {
        data_delay: None,
        order_client_id: order_client_id.to_string(),
        venue: "bybit".to_string(),
        account: None,
        symbol: "BTCUSDT".to_string(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(HELD_PRICE),
        trigger_price: None,
        status: OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

/// The guardrail's three answers against the door's one: `within limits` of what the door refuses, or `OVER LIMIT`
/// of what it takes, is the preview saying the opposite of what the node then does; `not checked` claims nothing.
///
/// MUTATION PROOF: take the close arm out of the client's `closes`, or size a market-entry bracket by its largest
/// exit again, and the rows named for each come back.
#[test]
fn the_guardrail_says_what_the_door_does_with_the_order() {
    let bybit = || engine("bybit", "BTCUSDT");
    let long = || holding(bybit(), "BTCUSDT", 5.0);
    let modify = |qty: f64| CommandWire::Modify {
        order_client_id: "c-9".into(),
        new_qty: Some(qty),
        new_price: Some(HELD_PRICE),
        new_trigger_price: None,
    };
    let mut short = bybit();
    short.positions = long().positions;
    short.positions[0].position_side = vike_model::events::PositionSide::Short;
    let deribit = || {
        let mut grid = indexmap::IndexMap::new();
        grid.insert("BTC-PERPETUAL".to_string(), ContractValue::inverse(1.0));
        VenueBlock { contract_values: Arc::new(grid), ..engine("deribit", "BTC-PERPETUAL") }
    };
    let ibkr = || engine("ibkr", "MSFT.SMART.USD");
    let rest = vec![resting_sell("c-9")];
    let rows: Vec<(&str, Vec<VenueBlock>, Vec<OrderView>, CommandWire)> = vec![
        // a) a market-entry bracket: the door sizes the entry at the price it holds, 500,000 here.
        ("market entry, over", vec![bybit()], vec![], bracket(10.0, None, 900.0, 1_100.0)),
        ("market entry, small", vec![bybit()], vec![], bracket(0.1, None, 900.0, 1_100.0)),
        (
            "market entry, an exit over",
            vec![bybit()],
            vec![],
            bracket(10.0, None, 9_000.0, 11_000.0),
        ),
        (
            "limit entry, over",
            vec![bybit()],
            vec![],
            bracket(10.0, Some(HELD_PRICE), 45_000.0, 55_000.0),
        ),
        (
            "limit entry, under",
            vec![bybit()],
            vec![],
            bracket(1.0, Some(HELD_PRICE), 45_000.0, 55_000.0),
        ),
        // b) a close: the door never sizes one.
        (
            "a close over the cap",
            vec![long()],
            vec![],
            order("bybit", "BTCUSDT", -1, 5.0, Some(HELD_PRICE)),
        ),
        (
            "a close in another case",
            vec![long()],
            vec![],
            order("bybit", "btcusdt", -1, 5.0, Some(HELD_PRICE)),
        ),
        (
            "a short's close",
            vec![short],
            vec![],
            order("bybit", "BTCUSDT", 1, 5.0, Some(HELD_PRICE)),
        ),
        (
            "the flag where it binds",
            vec![bybit()],
            vec![],
            flagged(order("bybit", "BTCUSDT", -1, 5.0, Some(HELD_PRICE))),
        ),
        (
            "the flag where it does not",
            vec![ibkr()],
            vec![],
            flagged(order("ibkr", "MSFT.SMART.USD", -1, 5.0, Some(HELD_PRICE))),
        ),
        (
            "past the position",
            vec![long()],
            vec![],
            order("bybit", "BTCUSDT", -1, 6.0, Some(HELD_PRICE)),
        ),
        ("a modify that stays a close", vec![long()], rest.clone(), modify(5.0)),
        ("a modify past the position", vec![long()], rest, modify(6.0)),
        // Everything else was right, and stays.
        (
            "an open over the cap",
            vec![long()],
            vec![],
            order("bybit", "BTCUSDT", 1, 5.0, Some(HELD_PRICE)),
        ),
        (
            "an open under the cap",
            vec![long()],
            vec![],
            order("bybit", "BTCUSDT", 1, 1.0, Some(HELD_PRICE)),
        ),
        ("a market order", vec![bybit()], vec![], order("bybit", "BTCUSDT", 1, 10.0, None)),
        (
            "inverse, over",
            vec![deribit()],
            vec![],
            order("deribit", "BTC-PERPETUAL", 1, 200_000.0, None),
        ),
        (
            "inverse, under",
            vec![deribit()],
            vec![],
            order("deribit", "BTC-PERPETUAL", 1, 10.0, None),
        ),
    ];
    let facts = Table::eth().with_price(Price::At(HELD_PRICE));
    let limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(CEILING), rate_per_sec: 1e9 });
    let capabilities = GuardrailCapabilities { max_qty: None, max_notional: Some(CEILING) };
    let total = rows.len();
    let mut differ = Vec::new();
    for (name, blocks, all_orders, cmd) in rows {
        let orders = named_orders(&cmd, &all_orders);
        let door = limits
            .preview_vet(&cmd, &blocks, &orders)
            .or_else(|| limits.market_reason(&cmd, &blocks, &orders, None, Some(&facts), None));
        let frame = frame_of(&blocks, &all_orders);
        let guard = guardrail_of(&cmd, capabilities, Ok(&frame)).to_json();
        let says = guard["within_limits"].as_bool();
        if says.is_some_and(|within| within == door.is_some()) {
            differ.push(format!("{name}: guardrail {guard}, door {door:?}"));
        }
    }
    assert!(differ.is_empty(), "{} of {total} rows disagree:\n{}", differ.len(), differ.join("\n"));
}

/// `(label, mounted symbol, tier, the watcher's word)`: every engine that could be read as the lane twin.
fn twin_candidates() -> Vec<(&'static str, &'static str, Option<AccountTier>, bool)> {
    let mut rows = Vec::new();
    for label in ["PERP", "ALT"] {
        for symbol in ["BTCUSDT.P", "ETHUSDT.P", "BTCUSDT"] {
            for tier in
                [Some(AccountTier::Demo), Some(AccountTier::Live), Some(AccountTier::Paper), None]
            {
                for derived in [false, true] {
                    rows.push((label, symbol, tier, derived));
                }
            }
        }
    }
    rows
}

/// The value row that tells which engine a client's preview reached.
fn valued(mut block: VenueBlock, symbol: &str, per_unit: f64) -> VenueBlock {
    let mut grid = indexmap::IndexMap::new();
    grid.insert(symbol.to_string(), ContractValue::linear(per_unit));
    block.contract_values = Arc::new(grid);
    block.extra_symbols = vec![symbol.to_string()].into();
    block
}

/// **Where a default account's `.P` order goes: the door decides, and the preview follows it**, over label, mounted
/// symbol, tier and the watcher's word. ⚠ Two rows are left, and they are the frame's limit: at `live` the door also asks
/// the watcher whether it derived this engine, and that word is not on the wire. A real `binance/PERP` live account
/// mounted on a symbol of the twin's lane (`.P`; only an old row: the writer reserves the label) still previews with its
/// own tier and value, while the door leaves the order on the default book. Since the twin's symbol is its parent row's
/// (decision 0156, amended 2026-11-13) the frame tells the twin by its LANE, so every `.P` symbol is such a row.
///
/// MUTATION PROOF: drop the tier from `AccountRowLaneTwin::is_shaped` and the `paper` and untiered rows come back.
#[test]
fn a_preview_names_the_engine_the_door_puts_a_perp_order_on() {
    let mut differ = Vec::new();
    for (label, symbol, tier, derived) in twin_candidates() {
        let key = format!("binance#{label}");
        let parent = valued(engine("binance", "BTCUSDT"), "BTCUSDT.P", 2.0);
        let twin = valued(VenueBlock { tier, ..engine(&key, symbol) }, "BTCUSDT.P", 3.0);
        let blocks = vec![parent, twin];
        let facts = if derived { Table::eth().deriving(&["binance#PERP"]) } else { Table::eth() };
        let cmd = submit("binance", "BTCUSDT.P", Some("DEFAULT"));
        let door = match lane_addressed(cmd.clone(), &blocks, Some(&facts)) {
            CommandWire::Submit(req) => req.account.as_deref() == Some(label),
            other => panic!("an order stays an order: {other:?}"),
        };
        let frame = frame_of(&blocks, &[]);
        let client = contract_value_of(&cmd, Ok(&frame)).is_ok_and(|v| v.per_unit > 2.5);
        if door != client {
            differ.push((label, symbol, tier, derived));
        }
    }
    assert_eq!(
        differ,
        [
            ("PERP", "BTCUSDT.P", Some(AccountTier::Live), false),
            ("PERP", "ETHUSDT.P", Some(AccountTier::Live), false)
        ]
    );
}

/// **A bare venue: the client may let past what the door then refuses, never the other way round**, over the same
/// rows with the parent up and down. A client that refused what the door takes would stop an order no gate had a word
/// against, so `accounts_of_venue` keeps to the label: a derived twin a profile mounted off the table's symbol is one
/// account, on the watcher's word, and no frame says so. What it lets past is an operator's own account under the twin's
/// label beside its parent, 10 rows of the 96 (11 while the frame told the twin by one compiled symbol, not its lane):
/// the door counts it and refuses, listing the books.
#[test]
fn a_client_never_refuses_a_bare_venue_the_door_takes() {
    let mut lets_past = 0;
    for (label, symbol, tier, derived) in twin_candidates() {
        for parent_up in [true, false] {
            let key = format!("binance#{label}");
            let mut blocks = vec![VenueBlock { tier, ..engine(&key, symbol) }];
            if parent_up {
                blocks.insert(0, engine("binance", "BTCUSDT"));
            }
            let facts =
                if derived { Table::eth().deriving(&["binance#PERP"]) } else { Table::eth() };
            let cmd = CommandWire::MarketExit { venue: Some("binance".into()), account: None };
            let keys: Vec<String> = blocks.iter().map(|b| b.route_key.clone()).collect();
            let door = crate::server::refusal::one_account_addressed(
                cmd.clone(),
                &keys,
                &[],
                &blocks,
                Some(&facts),
            )
            .is_err();
            let client = bare_venue_refusal(&cmd, &frame_of(&blocks, &[])).is_some();
            assert!(
                !client || door,
                "{label} on {symbol}, tier {tier:?}: the client refused alone"
            );
            if door && !client {
                assert_eq!(label, "PERP", "on {symbol}, tier {tier:?}");
                lets_past += 1;
            }
        }
    }
    assert_eq!(lets_past, 10);
}
