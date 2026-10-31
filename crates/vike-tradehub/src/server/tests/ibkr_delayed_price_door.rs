//! **An IBKR stock whose stream is only DELAYED is admitted, priced and recorded as delayed** (decision 0140: an
//! order MAY be priced off delayed data, the label is information and never a gate), driven through the
//! production chain into a REAL core: the ibkr feed's `PriceTap::labelled_quotes` behind a `TapSink` over the
//! `CoreLaneSink`, read back by the door through the tap's `door_price`, as `lane_twin_pricing` does for a lane
//! twin.
//!
//! The account the owner runs has no real-time stock entitlement, so every stock is delayed (about 15 minutes
//! old). The tap used to REFUSE such a stream by name (`no price feed for ...: ... is delayed ...`), which refused
//! every new stock and offered one cure, a market-data subscription no API buys. What these pin: the stock is
//! admitted and its order reaches the venue; the order's record (the registry's request, hence the published
//! order view the GUI badges) and its `PRE-TRADE` line say delayed; a stream that has stated no label is still no
//! price; and a real-time label, when the venue serves one, is what an order records.
//!
//! DOCS-SHAPED: the ticks are scripted. What the paper gateway really sends for a stock is `vike_ibkr`'s
//! `ibkr_delayed_price_smoke`, run by hand on a lane.

use std::sync::Arc;

use vike_data::LiveDataSink;
use vike_exec::SymbolAdmission;
use vike_marketdata::QuoteTick;
use vike_model::{AccountTier, DataDelay, SymbolGrid};
use vike_tradehub_core::CoreLaneSink;

use super::admits_any_symbol::{Node, limit, wait_until};
use crate::feeds::price_tap::{PriceTap, TapSink};
use crate::server::symbol_admission::SymbolFacts;

const IBKR: &str = "ibkr";
const MSFT: &str = "MSFT.SMART.USD";

/// The facts source the production node has for ibkr's stocks, with the gatewayhub's answer scripted (a one-cent
/// grid, whole shares) and the price the REAL tap's `door_price`.
struct TapFacts {
    tap: Arc<PriceTap>,
}

impl SymbolFacts for TapFacts {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        Ok(SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(1.0),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: None,
            quote_rate: None,
            rate_as_of_ms: None,
            im_requirement: None,
            release: false,
            margin_mode: None,
        })
    }
    fn price(&self, _: &str, venue: &str, symbol: &str, _: bool) -> Result<Option<f64>, String> {
        self.tap.door_price(venue, symbol)
    }
    /// The surface does not hold an order for a first price (Telegram's): a missing one answers at once.
    fn waits(&self) -> bool {
        false
    }
}

/// The node's production sink chain for ibkr's price feed over `node`'s core.
fn production_sink(node: &Node, tap: &Arc<PriceTap>) -> TapSink {
    let core = Arc::new(CoreLaneSink::new(
        node.core.bar_sender(),
        node.core.market_sender(),
        node.core.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    TapSink::new(core, Arc::clone(tap))
}

fn quote(bid: f64, ask: f64) -> QuoteTick {
    QuoteTick {
        ts: 1,
        local_ts: 0,
        bid,
        ask,
        bid_size: 100.0,
        ask_size: 100.0,
        symbol: String::new(),
    }
}

fn pre_trade_lines(node: &Node, coid: &str) -> Vec<String> {
    node.core
        .snapshot_cell()
        .load()
        .recent_events
        .iter()
        .filter(|l| l.starts_with("PRE-TRADE ") && l.contains(coid))
        .map(ToString::to_string)
        .collect()
}

/// **A stock with only delayed ticks is admitted and priced, and the order says delayed.** Before any label the
/// door has no price (fail closed: it is not known what the quote is), then a delayed two-sided quote releases
/// the order, the engine admits the stock, the venue gets the order, and the order's record and its `PRE-TRADE`
/// line carry the label. On the base this is refused `no price feed for `MSFT.SMART.USD`: ... is delayed ...`.
#[test]
fn a_stock_with_only_delayed_ticks_is_admitted_priced_and_recorded_delayed() {
    let tap = Arc::new(PriceTap::labelled_quotes());
    let facts = TapFacts { tap: Arc::clone(&tap) };
    let mut node = Node::start(IBKR, "AAPL.SMART.USD", AccountTier::Demo);
    let sink = production_sink(&node, &tap);

    // A quote of a stream that has stated no label is no price: the order is refused for want of one.
    sink.quote(IBKR, MSFT, quote(399.0, 401.0));
    let why = node
        .send(limit("msft-0", IBKR, MSFT, 1.0, 401.0), Some(&facts))
        .expect_err("no label, no price");
    assert!(why.contains("no price on this node yet"), "{why}");
    assert!(node.submitted.lock().unwrap().is_empty(), "a refusal sends the venue nothing");

    // IBKR labels the stream delayed (the label first, then the quote, as the quotes pump sends them).
    sink.data_delay(IBKR, MSFT, "quotes", DataDelay::DELAYED);
    sink.quote(IBKR, MSFT, quote(399.0, 401.0));
    let coid = node
        .send(limit("msft-1", IBKR, MSFT, 1.0, 401.0), Some(&facts))
        .expect("a delayed price is a price: the stock is admitted and the order goes");
    assert_eq!(coid, "msft-1");
    let seen = node.venue_saw("msft-1");
    assert_eq!(seen.symbol, MSFT);
    assert_eq!(seen.data_delay, Some(DataDelay::DELAYED), "the order was priced off delayed data");
    wait_until("the admission to publish", || node.blocks().iter().any(|b| b.trades(MSFT)));

    // The record the GUI and the operator read: the published order carries the label, and the core wrote the
    // order's one PRE-TRADE line.
    wait_until("the order to publish", || {
        node.core.snapshot_cell().load().orders.iter().any(|o| o.client_order_id == "msft-1")
    });
    let snap = node.core.snapshot_cell().load();
    let order = snap.orders.iter().find(|o| o.client_order_id == "msft-1").unwrap();
    assert_eq!(order.data_delay, Some(DataDelay::DELAYED));
    assert_eq!(
        pre_trade_lines(&node, "msft-1"),
        vec![format!(
            "PRE-TRADE msft-1 {MSFT}: priced off delayed data (recorded with the order; not a gate)"
        )]
    );
    node.core.shutdown_and_join();
}

/// **Real-time is what an order records once the venue serves it** (IBKR serves real-time for a class the
/// account IS entitled to, whatever was asked): a stream relabelled real-time makes the next order's record
/// real-time, with no `PRE-TRADE` line; and for a stock not yet admitted, the price noted under the delayed
/// label is not read under the new one (it would be called real-time while a quarter of an hour old).
#[test]
fn a_real_time_label_is_what_the_order_records_when_the_venue_serves_it() {
    let tap = Arc::new(PriceTap::labelled_quotes());
    let facts = TapFacts { tap: Arc::clone(&tap) };
    let mut node = Node::start(IBKR, "AAPL.SMART.USD", AccountTier::Demo);
    let sink = production_sink(&node, &tap);

    sink.data_delay(IBKR, MSFT, "quotes", DataDelay::DELAYED);
    sink.quote(IBKR, MSFT, quote(399.0, 401.0));
    node.send(limit("msft-d", IBKR, MSFT, 1.0, 401.0), Some(&facts)).expect("delayed");
    assert_eq!(node.venue_saw("msft-d").data_delay, Some(DataDelay::DELAYED));

    sink.data_delay(IBKR, MSFT, "quotes", DataDelay::RealTime);
    sink.quote(IBKR, MSFT, quote(401.0, 403.0));
    node.send(limit("msft-r", IBKR, MSFT, 1.0, 403.0), Some(&facts)).expect("real-time");
    assert_eq!(node.venue_saw("msft-r").data_delay, Some(DataDelay::RealTime));
    wait_until("the order to publish", || {
        node.core.snapshot_cell().load().orders.iter().any(|o| o.client_order_id == "msft-r")
    });
    assert!(pre_trade_lines(&node, "msft-r").is_empty(), "a real-time order writes no line");
    assert_eq!(pre_trade_lines(&node, "msft-d").len(), 1, "the delayed one wrote its line");

    // A stock the door has not admitted: its delayed price does not survive the relabel.
    const NVDA: &str = "NVDA.SMART.USD";
    sink.data_delay(IBKR, NVDA, "quotes", DataDelay::DELAYED);
    sink.quote(IBKR, NVDA, quote(199.0, 201.0));
    sink.data_delay(IBKR, NVDA, "quotes", DataDelay::RealTime);
    let why = node
        .send(limit("nvda-gap", IBKR, NVDA, 1.0, 201.0), Some(&facts))
        .expect_err("the delayed price is not the real-time stream's");
    assert!(why.contains("no price on this node yet"), "{why}");
    node.core.shutdown_and_join();
}
