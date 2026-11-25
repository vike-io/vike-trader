//! The owner's ruling of 2026-11-16 END TO END, per venue: a `paper` account of dukascopy, fxcm or ibkr fills from
//! the quotes the venue's demo login streams. Scripted quotes go through the same sink the price-only arm builds
//! (`feeds.rs`'s `dial`: the price tap in front of the core's lane sink, the core's bar lane beside it); with the
//! paper fold switched on (`PriceTap::set_paper_bars`, what `TradeLeaseRegistry::sync_paper_quotes` does) each window's
//! bar and its two sides reach the paper book, which fills a market buy at the ASK and a sell at the BID (decision
//! 0285), never the mid. Every quote the tap forwards is the book's clock as well (0285's amendment of 2026-11-16),
//! so switched off no bar is folded and the quote alone fills.

use std::sync::Arc;
use std::time::Duration;

use vike_core::LiveBroker;
use vike_data::DataClientSink;
use vike_execution::{Command, OrderIntent, SymbolAdmission};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_log::poll::wait_until;
use vike_marketdata::{TickQuote, TickTrade};
use vike_model::{OrderRequest, OrderStatus, OrderType, Strategy, StreamDelay};
use vike_tradehub_core::{
    CoreLaneSink, MountPlacement, StrategyCorePaper, StrategyCorePaperConfig,
    StrategyCorePaperHalt, build_paper_strategy_core_with,
};

use super::price_tap::{DataClientSinkPriceTap, PriceTap};
use crate::server::symbol_admission::{AccountReadiness, PriceWait, SymbolAdmitter, await_price};

/// `(venue, symbol, bid, ask, quantity)`: one paper account per venue priced through its demo login.
const CASES: [(&str, &str, f64, f64, f64); 3] = [
    ("dukascopy", "EURUSD", 1.08495, 1.08505, 1_000.0),
    ("fxcm", "EURUSD", 1.08495, 1.08505, 1_000.0),
    ("ibkr", "AAPL", 189.99, 190.01, 5.0),
];

/// A mount that trades nothing itself: the order is the operator's.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

fn quote(sink: &dyn DataClientSink, venue: &str, symbol: &str, ts: i64, bid: f64, ask: f64) {
    sink.quote(
        venue,
        symbol,
        TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 1_000_000.0,
            ask_size: 1_000_000.0,
            symbol: symbol.into(),
        },
    );
}

/// A paper account's core on `venue`'s `symbol` and the price-only arm's sink in front of it (`tap` before the core's
/// lane sink, the core's bar lane beside it), with the temp root its HALT sentinel would live under.
fn paper_core(
    venue: &str,
    symbol: &str,
    tap: &Arc<PriceTap>,
) -> (StrategyCorePaper, DataClientSinkPriceTap, tempfile::TempDir) {
    // A paper book reads its symbol's money and unit off the KEPT row (decision 0290: ibkr's `AAPL` states neither).
    crate::testutil::hold_door_lists();
    let root =
        tempfile::tempdir().expect("a temp root for the HALT sentinel this test never creates");
    let mount = build_paper_strategy_core_with(
        Box::new(Idle),
        &MountPlacement::new(venue, symbol, 1_000_000.0, 0.00001),
        StrategyCorePaperConfig {
            halt: StrategyCorePaperHalt::Pinned(root.path().join("HALT")),
            ..Default::default()
        },
    );
    let core = Arc::new(CoreLaneSink::new(
        mount.handle.bar_sender(),
        mount.handle.market_sender(),
        mount.handle.tick_sender(),
    )) as Arc<dyn DataClientSink>;
    let sink =
        DataClientSinkPriceTap::new(core, Arc::clone(tap)).with_bars(mount.handle.bar_sender());
    (mount, sink, root)
}

fn submit(mount: &StrategyCorePaper, id: &str, venue: &str, symbol: &str, side: i32, qty: f64) {
    mount.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        order_client_id: id.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: OrderType::Market,
        ..Default::default()
    }))));
}

/// What a paper market order of `side` on `venue` fills at, through the price-only arm's sink with the paper fold
/// `folded` or not: `None` when nothing filled. `stated` is the contract value the node states for the symbol first
/// (an ig epic's size, which its demo login states: `TradeLeaseRegistry::state_paper_units`), `None` for every other.
fn fill_price(
    (venue, symbol, bid, ask, qty): (&str, &str, f64, f64, f64),
    side: i32,
    folded: bool,
    stated: Option<vike_model::ContractValue>,
) -> Option<f64> {
    let tap = Arc::new(PriceTap::default());
    let (mount, sink, _root) = paper_core(venue, symbol, &tap);
    tap.set_paper_bars(venue, symbol, folded);
    if let Some(value) = stated {
        mount.handle.send_command(Command::AdmitSymbol(Box::new(
            vike_execution::SymbolAdmission {
                route_key: venue.into(),
                symbol: symbol.into(),
                grid: vike_model::SymbolGrid::default(),
                contract_value: Some(value),
                initial_mark: None,
                initial_mark_ms: None,
                quote_currency: None,
                quote_rate: None,
                rate_as_of_ms: None,
                initial_margin_requirement: None,
                release: false,
                margin_mode: None,
            },
        )));
        assert!(
            wait_until(Duration::from_secs(10), || {
                mount.handle.snapshot().contract_value_of(venue, symbol) == value
            }),
            "{venue}: the engine books the symbol at its stated contract value"
        );
    }

    // window 0 opens on the demo login's book; the order fills at that quote (at once, or as it comes)
    quote(&sink, venue, symbol, 1_000, bid, ask);
    let id = format!("{venue}-{side}");
    submit(&mount, &id, venue, symbol, side, qty);
    assert!(
        wait_until(Duration::from_secs(10), || mount.handle.snapshot().order(&id).is_some()),
        "{venue}: the market order never reached the paper book"
    );
    // a quote in window 1 closes window 0: its bar and its sides reach the book
    quote(&sink, venue, symbol, 61_000, bid, ask);
    let wait = if folded { Duration::from_secs(10) } else { Duration::from_secs(2) };
    let filled = wait_until(wait, || !mount.fills.lock().unwrap().is_empty());
    let price = filled.then(|| {
        let fill = mount.fills.lock().unwrap()[0].clone();
        assert_eq!((fill.side, fill.size), (side, qty), "{venue}");
        fill.price
    });
    let cell = mount.handle.snapshot_cell();
    mount.handle.shutdown_and_join();
    assert!(cell.load_full().fault.is_none(), "{venue}");
    price
}

#[test]
fn a_paper_market_buy_fills_at_the_demo_logins_ask_and_a_sell_at_its_bid() {
    vike_log::test_init();
    for (venue, symbol, bid, ask, qty) in CASES {
        assert_eq!(
            fill_price((venue, symbol, bid, ask, qty), 1, true, None),
            Some(ask),
            "{venue}: a buy pays the ask"
        );
        assert_eq!(
            fill_price((venue, symbol, bid, ask, qty), -1, true, None),
            Some(bid),
            "{venue}: a sell gets the bid"
        );
    }
}

/// The fold off makes no bar, and the quote the tap forwards to the core still fills the order at the ask: the
/// quote is the paper book's clock (decision 0285's amendment of 2026-11-16), the bars only a fallback for a
/// series no quote reaches. With no demo login no quote comes at all, and the door refuses the order first.
#[test]
fn with_the_fold_off_no_bar_is_made_and_the_quote_alone_fills() {
    vike_log::test_init();
    let (venue, symbol, bid, ask, qty) = CASES[0];
    assert_eq!(fill_price((venue, symbol, bid, ask, qty), 1, false, None), Some(ask));
}

/// `(venue, symbol, bid, ask, quantity, stated contract value)`: one paper account's quote.
type VenueQuote = (&'static str, &'static str, f64, f64, f64, Option<f64>);

/// One UNMOUNTED paper account per venue the owner named
/// (2026-11-16, "the same for ALL"), its own symbol's quote lane the only price that reaches its book; a shortable symbol
/// of each (a perpetual on the crypto venues), so the sell opens a short (a spot account sells only what it holds), by
/// its user name, whose KEPT row the book reads (decision 0290).
/// alpaca is not here: no quote lane of it opens on this node (`TradeLeaseRegistry`'s `NO_HELD_LANE`), so its paper
/// orders are refused in words at the door; nor polymarket, whose paper account is account-wide (every token an
/// admission over a trade lease a paper account does not hold).
const EVERY_VENUE: [VenueQuote; 12] = [
    ("binance", "BTCUSDT.P", 64_999.9, 65_000.1, 0.01, None),
    ("bybit", "BTCUSDT.P", 64_999.9, 65_000.1, 0.01, None),
    ("okx", "BTCUSDT.P", 64_999.9, 65_000.1, 1.0, None),
    ("aster", "BTCUSDT.P", 64_999.9, 65_000.1, 0.01, None),
    ("hyperliquid", "BTCUSDC.P", 64_999.0, 65_001.0, 0.01, None),
    ("deribit", "BTCUSD.P", 64_999.5, 65_000.5, 10.0, None),
    ("oanda", "EURUSD", 1.08495, 1.08505, 1_000.0, None),
    ("ctrader", "EURUSD", 1.08495, 1.08505, 1_000.0, None),
    ("ig", "EURUSD", 1.08495, 1.08505, 1.0, Some(10_000.0)),
    ("dukascopy", "EURUSD", 1.08495, 1.08505, 1_000.0, None),
    ("fxcm", "EURUSD", 1.08495, 1.08505, 1_000.0, None),
    ("ibkr", "AAPL", 189.99, 190.01, 5.0, None),
];

/// **On every venue an unmounted paper account's manual market order fills at the CURRENT quote: a buy at the ask, a
/// sell at the bid** (the owner, 2026-11-16: "it has to be the same for ALL"; decision 0285's quote clock). Its held
/// lane's quotes reach the core through the price-only arm's sink, no bar is folded, and the book fills on the quote
/// alone, never on a bar's price.
#[test]
fn on_every_venue_an_unmounted_paper_market_order_fills_at_the_current_quote() {
    vike_log::test_init();
    for (venue, symbol, bid, ask, qty, stated) in EVERY_VENUE {
        let stated = stated.map(vike_model::ContractValue::linear);
        assert_eq!(
            fill_price((venue, symbol, bid, ask, qty), 1, false, stated),
            Some(ask),
            "{venue}: a buy pays the ask"
        );
        assert_eq!(
            fill_price((venue, symbol, bid, ask, qty), -1, false, stated),
            Some(bid),
            "{venue}: a sell gets the bid"
        );
    }
}

/// The ibkr paper account of the CI box, whose stream is DELAYED for good (the account's entitlement).
const IBKR: (&str, &str, f64, f64, f64) = CASES[2];

/// `mount`'s order `id` once it is `status`, or a failure naming what it still is after `within`.
fn order_once(
    mount: &StrategyCorePaper,
    id: &str,
    status: OrderStatus,
    within: Duration,
) -> vike_execution::OrderView {
    let at = || mount.handle.snapshot().order(id).cloned();
    let reached = wait_until(within, || at().is_some_and(|o| o.status == status));
    assert!(reached, "{id}: never {status:?}, it is {:?}", at().map(|o| o.status));
    at().expect("the order is published")
}

/// What `vike-cli trade order cancel` prints as its outcome: the newest of the node's recent events that names the
/// order with a kind that settles a cancel (`vike-cli`'s `Target::of`, its `CANCELLED` kinds).
fn cancel_outcome(mount: &StrategyCorePaper, id: &str) -> Option<String> {
    const CANCELLED: [&str; 4] =
        ["OrderCanceled", "OrderCancelRejected", "OrderFilled", "OrderExpired"];
    mount.handle.snapshot().recent_events.iter().rev().find_map(|line| {
        let mut words = line.split(' ');
        let (kind, named) = (words.next()?, words.next()?);
        (named == id && CANCELLED.contains(&kind)).then(|| kind.to_string())
    })
}

/// **A DELAYED quote with both sides fills an ibkr paper market order AT ONCE, and the node books it** (decision
/// 0140: delayed data is information, never a gate). Through the ibkr tap (`PriceTap::labelled_quotes`) the delayed
/// quote is the door's price and the book's clock: the buy fills at its ask and the sell at its bid, recorded as
/// priced off delayed data, and the ORDER ROWS and the position say so, not only the book's own fill list (the CI box,
/// 2026-11-17: the book filled both at once, the rows stayed `Accepted`, nothing was booked).
#[test]
fn a_delayed_two_sided_quote_fills_an_ibkr_paper_market_order_at_once_and_the_node_books_it() {
    vike_log::test_init();
    let (venue, symbol, bid, ask, qty) = IBKR;
    let tap = Arc::new(PriceTap::labelled_quotes());
    let (mount, sink, _root) = paper_core(venue, symbol, &tap);
    sink.data_delay(venue, symbol, "quotes", StreamDelay::DELAYED);
    quote(&sink, venue, symbol, 1_000, bid, ask);
    assert!(tap.door_price(venue, symbol).expect("never refused").is_some(), "a delayed price");
    for (id, side, price, held) in [("ibkr-buy", 1, ask, qty), ("ibkr-sell", -1, bid, 0.0)] {
        submit(&mount, id, venue, symbol, side, qty);
        let row = order_once(&mount, id, OrderStatus::Filled, Duration::from_secs(2));
        assert_eq!(row.filled_qty, qty, "{id}");
        assert!((row.average_fill_price - price).abs() < 1e-9, "{id}: {}", row.average_fill_price);
        assert_eq!(row.data_delay, Some(StreamDelay::DELAYED), "{id}: priced off delayed data");
        let snap = mount.handle.snapshot();
        assert_eq!(snap.position(venue, symbol).map_or(0.0, |p| p.size), held, "{id}");
        assert_eq!(cancel_outcome(&mount, id).as_deref(), Some("OrderFilled"), "{id}");
    }
    let cell = mount.handle.snapshot_cell();
    mount.handle.shutdown_and_join();
    assert!(cell.load_full().fault.is_none());
}

/// The door's price source over the real tap, as the node's lease registry answers it (`TradeLeaseRegistry::price`).
struct TapDoor(Arc<PriceTap>);

impl SymbolAdmitter for TapDoor {
    fn admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        panic!("an order on the paper engine's own symbol looks nothing up")
    }
    fn price(&self, _: &str, venue: &str, symbol: &str, _: bool) -> Result<Option<f64>, String> {
        self.0.door_price(venue, symbol)
    }
}

impl AccountReadiness for TapDoor {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// **A quote with no ask is no price: the paper market order is refused in words, and the book is never asked to
/// guess a side** (decision 0285 verdict 5). The door holds an opening market order on a paper account's own symbol
/// for the first price (`own_first_price`, then `await_price`); on the ibkr tap a one-sided quote and a last trade
/// print price nothing, so the wait runs out and the order is refused the way dukascopy's was on the CI box.
#[test]
fn a_paper_market_order_on_a_quote_with_no_ask_is_refused_in_words() {
    vike_log::test_init();
    let (venue, symbol, bid, _, _) = IBKR;
    let tap = Arc::new(PriceTap::labelled_quotes());
    let (mount, sink, _root) = paper_core(venue, symbol, &tap);
    sink.data_delay(venue, symbol, "quotes", StreamDelay::DELAYED);
    quote(&sink, venue, symbol, 1_000, bid, 0.0);
    let print = TickTrade {
        timestamp: 1_000,
        local_timestamp: 0,
        price: bid,
        size: 1.0,
        is_buyer_maker: false,
        symbol: symbol.into(),
    };
    sink.trade(venue, symbol, print);
    let wait = PriceWait { route_key: venue.into(), venue: venue.into() };
    let why =
        await_price(&TapDoor(Arc::clone(&tap)), &wait, symbol).expect_err("no ask, no price").text;
    assert!(why.contains("has no price on this node yet") && why.contains("REFUSED"), "{why}");
    mount.handle.shutdown_and_join();
}

/// **A cancel of a resting paper order ends it, and the outcome the CLI prints is the row's**: no `Accepted` row is
/// left, the newest line naming the order is its `OrderCanceled`. A cancel of an order that already FILLED leaves the
/// row `Filled`, and the outcome printed for it is that fill (the CI box, 2026-11-17: the outcome said `OrderFilled` while
/// the row kept `Accepted`, the fill refused as already folded).
#[test]
fn a_cancel_of_a_resting_paper_order_ends_it_and_the_printed_outcome_is_the_rows() {
    vike_log::test_init();
    let (venue, symbol, bid, ask, qty) = IBKR;
    let tap = Arc::new(PriceTap::labelled_quotes());
    let (mount, sink, _root) = paper_core(venue, symbol, &tap);
    sink.data_delay(venue, symbol, "quotes", StreamDelay::DELAYED);
    quote(&sink, venue, symbol, 1_000, bid, ask);
    mount.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        order_client_id: "ibkr-rest".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(bid - 10.0),
        ..Default::default()
    }))));
    order_once(&mount, "ibkr-rest", OrderStatus::Accepted, Duration::from_secs(10));
    submit(&mount, "ibkr-filled", venue, symbol, 1, qty);
    order_once(&mount, "ibkr-filled", OrderStatus::Filled, Duration::from_secs(10));

    for id in ["ibkr-rest", "ibkr-filled"] {
        mount.handle.send_command(Command::Order(OrderIntent::Cancel(id.into())));
    }
    order_once(&mount, "ibkr-rest", OrderStatus::Canceled, Duration::from_secs(10));
    assert_eq!(cancel_outcome(&mount, "ibkr-rest").as_deref(), Some("OrderCanceled"));
    let filled = order_once(&mount, "ibkr-filled", OrderStatus::Filled, Duration::from_secs(1));
    assert_eq!(filled.filled_qty, qty);
    assert_eq!(cancel_outcome(&mount, "ibkr-filled").as_deref(), Some("OrderFilled"));
    let snap = mount.handle.snapshot();
    assert!(!snap.orders.iter().any(|o| o.status == OrderStatus::Accepted), "{:?}", snap.orders);
    mount.handle.shutdown_and_join();
}

/// **A paper market order whose side the CURRENT quote does not state is refused in words at once, on every venue**
/// (the owner, 2026-11-17: the current quote or not at all): a buy after a quote with a bid and no ask, a sell after
/// one with an ask and no bid, on dukascopy's paper account. Neither fills at the older two-sided quote nor waits.
/// Dukascopy's tap is an ordinary one (it takes a one-sided quote's half as the door's price), so the paper book is
/// where the rule holds for every venue.
#[test]
fn a_paper_market_order_whose_side_the_current_quote_does_not_state_is_refused_at_once() {
    vike_log::test_init();
    let (venue, symbol, bid, ask, qty) = CASES[0];
    for (id, side, now_bid, now_ask, words) in
        [("buy-no-ask", 1, bid, 0.0, "no ask"), ("sell-no-bid", -1, 0.0, ask, "no bid")]
    {
        let tap = Arc::new(PriceTap::default());
        let (mount, sink, _root) = paper_core(venue, symbol, &tap);
        quote(&sink, venue, symbol, 1_000, bid, ask);
        quote(&sink, venue, symbol, 2_000, now_bid, now_ask);
        submit(&mount, id, venue, symbol, side, qty);
        let row = order_once(&mount, id, OrderStatus::Rejected, Duration::from_secs(10));
        let reason = row.reason.unwrap_or_default();
        assert!(reason.starts_with(words) && reason.contains(symbol), "{id}: {reason}");
        assert_eq!(row.filled_qty, 0.0, "{id}");
        assert!(mount.fills.lock().unwrap().is_empty(), "{id}: never at the older quote");
        let snap = mount.handle.snapshot();
        assert_eq!(snap.position(venue, symbol).map_or(0.0, |p| p.size), 0.0, "{id}");
        mount.handle.shutdown_and_join();
    }
}
