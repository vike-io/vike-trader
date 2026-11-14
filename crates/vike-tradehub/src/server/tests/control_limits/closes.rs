//! **A check at the door must never stop a real close, and must not refuse what the gate would have sent.**
//!
//! The rows run an order through the send (`accept_command` into a real core) and the TCP dry-run (a real server over
//! the same published blocks), and the two must answer alike. The engine's own gate judges a close at the position
//! it folded, so the blocks here state the position the door reads, and a row that goes only has to reach the venue
//! client.
use vike_execution::{PositionView, SymbolAdmission, VenueBlock};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::events::PositionSide;
use vike_model::{AccountTier, ContractValue, MarginMode};

use super::*;
use crate::server::control::accept_command;
use crate::server::symbol_admission::{AccountReadiness, SymbolAdmitter};
use crate::server::tests::admits_any_symbol::{Node, wait_until};

const KEY: &[u8] = b"control-key-for-the-close-table";

/// A node that holds a price for one symbol, spelled exactly so (the tap keys by spelling), and looks up nothing.
struct Tap(Option<(String, f64)>);

impl Tap {
    fn at(symbol: &str, px: f64) -> Self {
        Tap(Some((symbol.to_string(), px)))
    }
}

impl SymbolAdmitter for Tap {
    fn admission(
        &self,
        _: &str,
        _: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        Err(SymbolPropertiesRequestError::Refused(format!("no facts for {symbol}")))
    }
    fn price(&self, _: &str, _: &str, symbol: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(self.0.as_ref().filter(|(held, _)| held == symbol).map(|(_, px)| *px))
    }
}

impl AccountReadiness for Tap {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// A market order of `qty` on `side`, with or without the flag.
fn market(
    order_client_id: &str,
    venue: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    flag: bool,
) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: order_client_id.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: flag,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// [`market`] as a limit order at `price`.
fn priced(cmd: CommandWire, price: f64) -> CommandWire {
    let CommandWire::Submit(mut req) = cmd else { unreachable!() };
    (req.order_type, req.price) = ("limit".into(), Some(price));
    CommandWire::Submit(req)
}

/// A block holding `size` of `symbol` (long positive).
fn holding(block: &VenueBlock, symbol: &str, size: f64) -> VenueBlock {
    let mut held = block.clone();
    held.positions.push(PositionView {
        venue: block.venue.as_str().into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        size,
        average_price: 100.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liquidation_price: 0.0,
        margin_mode: MarginMode::Cross,
        isolated_margin: None,
    });
    held
}

/// A real server over `blocks`, with `facts` as its symbol source and a ceiling of `ceiling`.
fn served(blocks: Vec<VenueBlock>, facts: Arc<dyn SymbolAdmitter>, ceiling: f64) -> SocketAddr {
    let mut snap = vike_execution::CoreSnapshot::empty(&blocks[0].venue, &blocks[0].symbol);
    snap.portfolio.venues = blocks;
    let cell = Arc::new(arc_swap::ArcSwap::from_pointee(snap));
    let publisher = crate::publish::spawn(cell, None).with_symbol_sources(
        crate::server::symbol_admission::SymbolSources {
            readiness: Some(facts.clone()),
            admitter: Some(facts),
            ..Default::default()
        },
    );
    wait_until("the publisher's roster", || !publisher.engine_blocks().is_empty());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    std::thread::spawn(move || {
        let _ = serve(
            listener,
            publisher,
            HubKeys::new(b"observe-key-for-the-close-table".to_vec(), KEY.to_vec()),
            None,
            ControlLimitsConfig { max_notional: Some(ceiling), rate_per_sec: 1e9 },
            None,
            None,
            None,
        );
    });
    addr
}

/// The send's verdict on `cmd` against `blocks`.
fn sent(
    node: &mut Node,
    cmd: CommandWire,
    blocks: &[VenueBlock],
    facts: &dyn SymbolAdmitter,
) -> Result<(), String> {
    let sink = node.core.command_sink();
    accept_command(
        cmd,
        None,
        &mut node.limits,
        &sink,
        None,
        &[],
        &[],
        &[],
        blocks,
        &[],
        &[],
        &[],
        Some(facts),
        None,
        None,
    )
    .map(|_| ())
    .map_err(|e| e.message())
}

/// One row: what it is, the order, whether it goes, and what a refusal must say.
type Row<'a> = (&'a str, CommandWire, bool, &'a str);

/// Every row through the send and the TCP dry-run: both answer alike, a refusal is one sentence, a row the send takes
/// reaches the venue client and a refused one does not. `position` is what the blocks say the engine holds.
fn table(
    venue: &'static str,
    symbol: &str,
    position: f64,
    value: ContractValue,
    px: f64,
    ceiling: f64,
    rows: Vec<Row>,
) {
    let mut node = Node::start(venue, symbol, AccountTier::Demo);
    node.limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(ceiling), rate_per_sec: 1e9 });
    let facts = Arc::new(Tap::at(symbol, px));
    let mut block = holding(&node.blocks().remove(0), symbol, position);
    block.contract_value_default = value;
    let blocks = vec![block];
    let addr = served(blocks.clone(), Arc::clone(&facts) as Arc<dyn SymbolAdmitter>, ceiling);
    for (what, cmd, goes, says) in rows {
        let CommandWire::Submit(req) = &cmd else { panic!("{what}: a table row is a submit") };
        let order_client_id = req.order_client_id.clone();
        let (accepted, reason, _) =
            vike_tradehub_client::preview_command(addr, KEY, &cmd).expect("a verdict");
        let verdict = sent(&mut node, cmd, &blocks, &*facts);
        assert_eq!(accepted, goes, "{venue} {what}: the TCP preview said {reason:?}");
        assert_eq!(verdict.is_ok(), goes, "{venue} {what}: the send said {verdict:?}");
        if goes {
            node.venue_saw(&order_client_id);
        } else {
            let why = verdict.expect_err("refused");
            assert!(why.contains(says), "{venue} {what}: {why}");
            assert_eq!(reason.as_deref(), Some(why.as_str()), "{venue} {what}: one sentence");
            let seen = node.submitted.lock().unwrap();
            assert!(
                !seen.iter().any(|o| o.order_client_id == order_client_id),
                "{venue} {what}: not sent"
            );
        }
    }
    node.core.shutdown_and_join();
}

/// **Long 10 BTC, the row at 100k, a sell of 10 with no flag: a close the core calls covered.** It was refused as
/// worth 1,000,000 because only the flag said "close". The part of an order beyond the position is an open, and the
/// core then judges the WHOLE order (`vike_model::is_covered_reduce` is about all of it), so the door does too.
#[test]
fn a_close_needs_no_flag_and_an_order_past_the_position_is_sized_whole() {
    let (venue, symbol) = ("binance", "BTCUSDT");
    let rows = vec![
        ("open", market("o-1", venue, symbol, 1, 10.0, false), false, "1000000.00"),
        ("implicit close", market("i-1", venue, symbol, -1, 10.0, false), true, ""),
        ("partial close", market("i-2", venue, symbol, -1, 4.0, false), true, ""),
        ("close in the wrong case", market("i-3", venue, "btcusdt", -1, 10.0, false), true, ""),
        ("flagged close", market("f-1", venue, symbol, -1, 10.0, true), true, ""),
        ("over-close", market("x-1", venue, symbol, -1, 15.0, false), false, "1500000.00"),
        ("a small open", market("o-2", venue, symbol, 1, 0.5, false), true, ""),
    ];
    table(venue, symbol, 10.0, ContractValue::default(), 100_000.0, 100_000.0, rows);
}

/// **Short 200,000 of an inverse perpetual, the row at 100k: a close (desktop ClosePosition, `--reduce-only`) is
/// never sized**, though an inverse contract can be sized with no price. So is a priced close on any contract: a
/// reduce-only limit or stop is a close as a market one is.
#[test]
fn a_close_is_not_sized_on_any_arm_of_the_ceiling() {
    let (venue, symbol) = ("deribit", "BTC-PERPETUAL");
    let at = |cmd| priced(cmd, 90_000.0);
    let rows = vec![
        ("flagged close", market("f-2", venue, symbol, 1, 200_000.0, true), true, ""),
        ("implicit close", market("i-4", venue, symbol, 1, 200_000.0, false), true, ""),
        ("open", market("o-3", venue, symbol, -1, 200_000.0, false), false, "200000.00"),
        ("over-close", market("x-2", venue, symbol, 1, 300_000.0, false), false, "300000.00"),
        ("priced flagged", at(market("f-3", venue, symbol, 1, 200_000.0, true)), true, ""),
        ("priced implicit", at(market("i-5", venue, symbol, 1, 200_000.0, false)), true, ""),
        ("priced open", at(market("o-4", venue, symbol, -1, 200_000.0, false)), false, "200000.00"),
    ];
    table(venue, symbol, -200_000.0, ContractValue::inverse(1.0), 100_000.0, 100_000.0, rows);

    let (venue, symbol) = ("bybit", "BTCUSDT");
    let at = |cmd| priced(cmd, 100_000.0);
    let rows = vec![
        ("priced implicit", at(market("i-6", venue, symbol, -1, 10.0, false)), true, ""),
        ("priced open", at(market("o-5", venue, symbol, 1, 10.0, false)), false, "1000000.00"),
    ];
    table(venue, symbol, 10.0, ContractValue::default(), 100_000.0, 100_000.0, rows);
}

/// **A `Modify` keeps closing while the position covers its new size**: re-pricing a resting close is no size at all,
/// whatever the order rests at. Raised past the position it opens, and an order on the side that adds is an open:
/// the published order carries no flag, so the position says it on every venue.
#[test]
fn a_modify_of_a_closing_order_is_not_sized() {
    let limits = ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(100_000.0),
        rate_per_sec: 1e9,
    });
    let engine = VenueBlock {
        venue: "bybit".into(),
        route_key: "bybit".into(),
        symbol: "BTCUSDT".into(),
        ..VenueBlock::default()
    };
    let blocks = [holding(&engine, "BTCUSDT", 10.0)];
    let resting = |order_client_id: &str, side| vike_execution::OrderView {
        side,
        ..published_order(
            order_client_id,
            "bybit",
            None,
            "BTCUSDT",
            vike_model::OrderStatus::Accepted,
        )
    };
    let modify = |order_client_id: &str, qty, price| CommandWire::Modify {
        order_client_id: order_client_id.into(),
        new_qty: Some(qty),
        new_price: price,
        new_trigger_price: None,
    };
    let sell = [resting("m-sell", -1)];
    for price in [Some(100_000.0), None] {
        assert_eq!(limits.preview_vet(&modify("m-sell", 10.0, price), &blocks, &sell), None);
    }
    assert!(limits.preview_vet(&modify("m-sell", 15.0, Some(100_000.0)), &blocks, &sell).is_some());
    let buy = [resting("m-buy", 1)];
    assert!(limits.preview_vet(&modify("m-buy", 10.0, Some(100_000.0)), &blocks, &buy).is_some());

    let at_market = [vike_execution::OrderView {
        order_type: vike_model::OrderType::Market,
        ..resting("m-mkt", -1)
    }];
    let tap = Tap::at("BTCUSDT", 100_000.0);
    let judged = |qty| {
        limits.market_reason(
            &modify("m-mkt", qty, Some(1.0)),
            &blocks,
            &at_market,
            None,
            Some(&tap),
            None,
        )
    };
    assert_eq!(judged(10.0), None, "a resting market close");
    assert!(judged(15.0).is_some(), "past the position, at the price the node holds");
}
/// A `bybit` node on `BTCUSDT` whose engine has admitted `ETHUSDT`, and its blocks.
fn eth_admitted() -> (Node, Vec<VenueBlock>) {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    node.limits = ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(100_000.0),
        rate_per_sec: 1e9,
    });
    let mut block = node.blocks().remove(0);
    block.extra_symbols = vec!["ETHUSDT".to_string()].into();
    (node, vec![block])
}

/// **A preview sizes on the symbol the send would carry**: `ethusdt` is the admitted `ETHUSDT`, and the price the
/// tap holds is keyed by that spelling, so the preview used to read none and say accepted over a send that refuses.
#[test]
fn a_preview_sizes_on_the_symbol_the_send_carries() {
    let (mut node, blocks) = eth_admitted();
    let facts = Arc::new(Tap::at("ETHUSDT", 2_500.0));
    let addr = served(blocks.clone(), Arc::clone(&facts) as Arc<dyn SymbolAdmitter>, 100_000.0);
    let order = market("eth-1", "bybit", "ethusdt", 1, 100.0, false);
    let (accepted, reason, _) =
        vike_tradehub_client::preview_command(addr, KEY, &order).expect("a verdict");
    let refused = sent(&mut node, order, &blocks, &*facts).expect_err("250,000 over 100,000");
    assert!(!accepted, "the send refused: {refused}");
    assert_eq!(reason.as_deref(), Some(refused.as_str()), "one sentence");
    node.core.shutdown_and_join();
}

/// **With no price held the preview says so**, on the symbol the send would carry: the send holds the order for
/// the first price and sizes it then, which a preview cannot do.
#[test]
fn a_preview_with_no_price_held_says_the_order_was_not_sized() {
    let (node, blocks) = eth_admitted();
    let addr = served(blocks, Arc::new(Tap(None)), 100_000.0);
    for (order_client_id, typed, spelled) in
        [("n-1", "ethusdt", "ETHUSDT"), ("n-2", "BTCUSDT", "BTCUSDT")]
    {
        let order = market(order_client_id, "bybit", typed, 1, 100.0, false);
        let (accepted, reason, notes) =
            vike_tradehub_client::preview_command(addr, KEY, &order).expect("a verdict");
        assert!(accepted, "{order_client_id}: the send decides, at the first price: {reason:?}");
        assert_eq!(notes.len(), 1, "{order_client_id}: {notes:?}");
        assert!(
            notes[0].contains(spelled) && notes[0].contains("no price"),
            "{order_client_id}: {notes:?}"
        );
    }
    let limit = priced(market("n-3", "bybit", "BTCUSDT", 1, 100.0, false), 1.0);
    let close = market("n-4", "bybit", "BTCUSDT", -1, 1.0, true);
    for order in [limit, close] {
        let (accepted, reason, notes) =
            vike_tradehub_client::preview_command(addr, KEY, &order).expect("a verdict");
        assert!(accepted, "{reason:?}");
        assert!(notes.is_empty(), "a priced order and a close are not held: {notes:?}");
    }
    node.core.shutdown_and_join();
}

/// **A venue that never reads the flag takes a flagged order as a plain one** (`vike_venues::capabilities_for`'s
/// `supports_reduce_only`), so the flag alone says nothing there: 1,000 flagged on a flat book opens a position past
/// the row, and so does whatever a flagged order holds beyond the position. With the position showing it, a close
/// over the row still goes. A venue that enforces the flag cannot open with it, so a flagged order is trusted
/// there and a stale snapshot cannot size a real close.
#[test]
fn the_flag_says_a_close_only_on_a_venue_that_reads_it() {
    let value = ContractValue::default();
    for (venue, symbol) in
        [("ibkr", "AAPL.SMART.USD"), ("alpaca", "AAPL"), ("oanda", "EURUSD"), ("ctrader", "EURUSD")]
    {
        let rows = vec![(
            "flagged on a flat book",
            market("flat-1", venue, symbol, 1, 1_000.0, true),
            false,
            "100000.00",
        )];
        table(venue, symbol, 0.0, value, 100.0, 5_000.0, rows);
        let rows = vec![
            ("the position shows it", market("c-1", venue, symbol, 1, 1_000.0, true), true, ""),
            (
                "past the position",
                market("c-2", venue, symbol, 1, 1_500.0, true),
                false,
                "150000.00",
            ),
            (
                "the side that adds",
                market("c-3", venue, symbol, -1, 100.0, true),
                false,
                "10000.00",
            ),
        ];
        table(venue, symbol, -1_000.0, value, 100.0, 5_000.0, rows);
    }
    let (venue, symbol) = ("bybit", "BTCUSDT");
    let rows =
        vec![("the venue enforces it", market("t-1", venue, symbol, 1, 1_000.0, true), true, "")];
    table(venue, symbol, 0.0, value, 100.0, 5_000.0, rows);
}

/// A Polymarket outcome token, the name it goes by, and a source that resolves one to the other and holds no price.
const TOKEN: &str = "71321045679252212594626385532706912750332728571942532289631379312455583992563";
const NAME: &str = "rain-market:Yes";

#[derive(Default)]
struct Gamma(std::sync::Mutex<Vec<String>>);

impl SymbolAdmitter for Gamma {
    fn resolve(&self, _: &str, _: &str, typed: &str) -> Result<String, String> {
        self.0.lock().unwrap().push(typed.to_string());
        if typed == NAME { Ok(TOKEN.to_string()) } else { Err(format!("no market `{typed}`")) }
    }
    fn admission(
        &self,
        _: &str,
        _: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        Err(SymbolPropertiesRequestError::Refused(format!("no facts for {symbol}")))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(None)
    }
}

impl AccountReadiness for Gamma {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// **A Polymarket close by its market NAME is a close from the first gate on.** The position is booked under the token
/// id, so a SELL by `slug:Outcome` read flat: an over-ceiling limit was refused, and a market sale with no price held
/// was refused "retry", while the same order by its id went. The name is resolved once, the send and the dry run alike.
#[test]
fn a_polymarket_close_by_its_market_name_is_a_close_at_every_gate() {
    let mut node = Node::start("polymarket", "", AccountTier::Demo);
    node.limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(1.0), rate_per_sec: 1e9 });
    let engine = VenueBlock {
        venue: "polymarket".into(),
        route_key: "polymarket".into(),
        symbol: String::new(),
        extra_symbols: vec![TOKEN.to_string()].into(),
        tier: Some(AccountTier::Demo),
        ..VenueBlock::default()
    };
    let blocks = vec![holding(&engine, TOKEN, 100.0)];
    let facts = Arc::new(Gamma::default());
    let addr = served(blocks.clone(), Arc::clone(&facts) as Arc<dyn SymbolAdmitter>, 1.0);
    let sale = |order_client_id| market(order_client_id, "polymarket", NAME, -1, 100.0, false);
    for order in [priced(sale("pm-1"), 0.5), sale("pm-2")] {
        let (accepted, reason, _) =
            vike_tradehub_client::preview_command(addr, KEY, &order).expect("a verdict");
        assert!(accepted, "the dry run: {reason:?}");
        facts.0.lock().unwrap().clear();
        let verdict = sent(&mut node, order, &blocks, &*facts);
        assert_eq!(verdict, Ok(()));
        assert_eq!(*facts.0.lock().unwrap(), [NAME], "resolved once");
    }
    let why = sent(
        &mut node,
        market("pm-3", "polymarket", "nope:Yes", -1, 100.0, false),
        &blocks,
        &*facts,
    )
    .expect_err("a name no market has");
    assert!(why.contains("no market `nope:Yes`") && why.contains("REFUSED"), "{why}");
    node.core.shutdown_and_join();
}

/// A source that counts what the door may ask of a gatewayhub on an order's way out.
#[derive(Default)]
struct Asked(std::sync::Mutex<Vec<&'static str>>);

impl SymbolAdmitter for Asked {
    fn admission(
        &self,
        _: &str,
        _: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        Err(SymbolPropertiesRequestError::Refused(format!("no facts for {symbol}")))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        self.0.lock().unwrap().push("price");
        Ok(Some(100.0))
    }
    fn refresh(&self, _: &str, _: &str, _: &str) -> Option<SymbolAdmission> {
        self.0.lock().unwrap().push("refresh");
        None
    }
    fn own_symbol_rate(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &VenueBlock,
    ) -> Option<SymbolAdmission> {
        self.0.lock().unwrap().push("rate");
        None
    }
}

impl AccountReadiness for Asked {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// **A close on a symbol the engine trades costs the door no round trip.** A refresh with facts due, its commit, a
/// price lane's facts and a mount's aged rate can each sit on a wedged gatewayhub for the lease's `request_reply`, while
/// the client's ack wait expires and the order goes out later. They exist for an order that opens, which still asks.
#[test]
fn a_close_asks_the_gatewayhub_nothing_on_its_way_out() {
    use crate::server::symbol_admission::plan_submit_symbol;
    let node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let mut block = node.blocks().remove(0);
    block.extra_symbols = vec!["ETHUSDT".to_string()].into();
    let blocks = [holding(&holding(&block, "ETHUSDT", 5.0), "BTCUSDT", 5.0)];
    let asked = |cmd: CommandWire| {
        let facts = Asked::default();
        plan_submit_symbol(&cmd, &blocks, Some(&facts), true);
        facts.0.into_inner().unwrap()
    };
    let close = |order_client_id, symbol| market(order_client_id, "bybit", symbol, -1, 5.0, false);
    let open = |order_client_id, symbol| market(order_client_id, "bybit", symbol, 1, 5.0, false);
    assert_eq!(asked(close("a-1", "ETHUSDT")), Vec::<&str>::new(), "an admitted symbol");
    assert_eq!(asked(open("a-2", "ETHUSDT")), ["refresh", "price"], "an open still asks");
    assert!(!asked(close("a-3", "BTCUSDT")).contains(&"rate"), "the engine's own symbol");
    assert!(asked(open("a-4", "BTCUSDT")).contains(&"rate"), "an open still renews it");
    node.core.shutdown_and_join();
}

/// **The note a flatten owes reads the engines the flatten reaches**: a stopped account's retained block holds a
/// position no fan-out reaches (the core skips a retired engine), so `binance` flat beside a stopped `binance#ALT`
/// holding closes nothing, and the note says so, and why naming that book would not help.
#[test]
fn the_flatten_note_counts_only_the_engines_that_run() {
    let node = Node::start("binance", "BTCUSDT", AccountTier::Demo);
    let own = node.blocks().remove(0);
    let alt = VenueBlock { route_key: "binance#ALT".into(), ..own.clone() };
    let blocks = vec![own, holding(&alt, "BTCUSDT", 5.0)];
    let notes = |route_keys: &[&str], account: Option<&str>| {
        let route_keys: Vec<String> = route_keys.iter().map(ToString::to_string).collect();
        accept_command(
            CommandWire::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: account.map(str::to_string),
            },
            None,
            &mut ControlLimits::new(ControlLimitsConfig { max_notional: None, rate_per_sec: 1e9 }),
            &node.core.command_sink(),
            None,
            &[],
            &route_keys,
            &[],
            &blocks,
            &[],
            &[],
            &[],
            None,
            None,
            None,
        )
        .expect("a flatten is sent")
        .into_order_client_id_and_notes()
        .1
    };
    let stopped = notes(&["binance"], None);
    assert_eq!(stopped.len(), 1, "{stopped:?}");
    assert!(stopped[0].contains("shows no position on binance"), "{stopped:?}");
    assert!(stopped[0].contains("binance#ALT") && stopped[0].contains("stopped"), "{stopped:?}");

    let both = ["binance", "binance#ALT"];
    assert!(notes(&both, Some("ALT")).is_empty(), "it holds the position and it runs");
    let hint = notes(&both, Some("DEFAULT"));
    assert!(hint[0].contains("name that book, `binance/ALT`"), "{hint:?}");
    node.core.shutdown_and_join();
}

/// **`close-all binance/DEFAULT` with the position on the twin closes nothing, and the answer says so**, naming the book
/// that holds it. A bare exit reaches both lanes and the node-wide one every book, so neither has a note to owe.
#[test]
fn an_exit_that_names_a_flat_book_says_so_and_names_the_book_that_holds() {
    let node = Node::start("binance", "BTCUSDT", AccountTier::Demo);
    let own = node.blocks().remove(0);
    let twin = VenueBlock { route_key: "binance#ALT".into(), ..own.clone() };
    let blocks = vec![own, holding(&twin, "BTCUSDT", 5.0)];
    let route_keys = ["binance".to_string(), "binance#ALT".to_string()];
    let notes = |venue: Option<&str>, account: Option<&str>| {
        accept_command(
            CommandWire::MarketExit {
                venue: venue.map(str::to_string),
                account: account.map(str::to_string),
            },
            None,
            &mut ControlLimits::new(ControlLimitsConfig { max_notional: None, rate_per_sec: 1e9 }),
            &node.core.command_sink(),
            None,
            &[],
            &route_keys,
            &[],
            &blocks,
            &[],
            &[],
            &[],
            None,
            None,
            None,
        )
        .expect("an exit is sent")
        .into_order_client_id_and_notes()
        .1
    };
    let flat = notes(Some("binance"), Some("DEFAULT"));
    assert_eq!(flat.len(), 1, "{flat:?}");
    assert!(flat[0].contains("shows no position on binance"), "{flat:?}");
    assert!(flat[0].contains("name that book, `binance/ALT`"), "{flat:?}");
    assert!(notes(Some("binance"), Some("ALT")).is_empty(), "it holds the position");
    assert!(notes(None, None).is_empty(), "the node-wide button");
    node.core.shutdown_and_join();
}

/// **Before the first publish no block answers, and only the number the order names can bind it.** A market order
/// naming a price was sized at it until the door stopped reading a market order's price, and an empty roster then
/// left it unsized. A flagged close stays unsized, and a price the node holds still beats the number.
#[test]
fn before_the_first_publish_a_market_order_is_sized_at_the_number_it_names() {
    let limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(500.0), rate_per_sec: 1e9 });
    let named = |order_client_id, px, flag| {
        let CommandWire::Submit(mut req) =
            market(order_client_id, "bybit", "BTCUSDT", 1, 0.1, flag)
        else {
            unreachable!()
        };
        req.price = Some(px);
        CommandWire::Submit(req)
    };
    let why = limits
        .market_reason(&named("e-1", 74_300.0, false), &[], &[], None, None, None)
        .expect("7,430 at the number it names");
    assert!(why.contains("7430"), "{why}");
    let low = named("e-2", 1.0, false);
    assert_eq!(limits.market_reason(&low, &[], &[], None, None, None), None);
    let close = named("e-3", 74_300.0, true);
    assert_eq!(limits.market_reason(&close, &[], &[], None, None, None), None);
    let held = Tap::at("BTCUSDT", 74_300.0);
    let plain = market("e-4", "bybit", "BTCUSDT", 1, 0.1, false);
    assert!(limits.market_reason(&plain, &[], &[], None, Some(&held), None).is_some());
    assert_eq!(limits.market_reason(&plain, &[], &[], None, None, None), None);
}
