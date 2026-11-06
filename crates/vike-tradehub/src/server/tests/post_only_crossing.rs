//! **A post-only LIMIT order whose price would cross the book the node already holds is refused by name at the
//! door, with both numbers, and nothing is sent** (decision 0242, item 8), driven through the production chain
//! into a REAL core: the venue's quote reaches a real [`PriceTap`] through a [`TapSink`], the door reads the best
//! bid and best ask back through [`SymbolFacts::top_of_book`], and [`accept_command`](super::super::control::accept_command)
//! answers.
//!
//! Bybit and OKX accept a crossing post-only order and cancel it a millisecond later, which the node recorded as
//! a plain `CANCELED` with no reason; the rule is one for every venue that has a post-only order, so each of the
//! five is driven. The edges: a buy AT the ask and a sell AT the bid cross (touching lifts it), a price behind the
//! touch rests; no top of book held (nothing seen, or only a last price) is NOT a refusal and the venue judges; a
//! plain limit order priced through the book is untouched; a `paper` book keeps its own law; and a venue with no
//! post-only order is refused by the core's capability row, not by this check.

use std::sync::Arc;

use vike_data::LiveDataSink;
use vike_exec::SymbolAdmission;
use vike_marketdata::{QuoteTick, TradeTick};
use vike_model::{AccountTier, OrderStatus, TimeInForce};
use vike_tradehub_client::wire::WireCommand;

use super::admits_any_symbol::{Node, limit, wait_until};
use crate::feeds::price_tap::{PriceTap, TapSink};
use crate::server::symbol_admission::{SymbolFacts, submit_symbol_refusal};

/// The five venues that have a post-only order, each with the symbol its engine is mounted on.
const POST_ONLY_VENUES: [(&str, &str); 5] = [
    ("binance", "ETHUSDT"),
    ("bybit", "ETHUSDT"),
    ("okx", "ETH-USDT-SWAP"),
    ("deribit", "ETH-PERPETUAL"),
    ("hyperliquid", "ETH"),
];

const BID: f64 = 2999.5;
const ASK: f64 = 3000.5;
/// One tick (0.1) behind the touch, spelled out so the price is exactly on the engine's grid.
const BEHIND_BID: f64 = 2999.6;
const BEHIND_ASK: f64 = 3000.4;

/// The facts source the node has for these venues: the price and the top of book are the REAL tap's, and the
/// engine's own symbol needs no admission.
struct TapFacts {
    tap: Arc<PriceTap>,
}

impl SymbolFacts for TapFacts {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, _: &str, _: &str, _: &str) -> Result<SymbolAdmission, String> {
        Err("the engine's own symbol is never looked up".into())
    }
    fn price(&self, _: &str, venue: &str, symbol: &str, _: bool) -> Result<Option<f64>, String> {
        self.tap.door_price(venue, symbol)
    }
    fn top_of_book(&self, _: &str, venue: &str, symbol: &str) -> Option<(f64, f64)> {
        self.tap.door_top(venue, symbol)
    }
    fn waits(&self) -> bool {
        false
    }
}

/// A quiet sink under the production tap: the core is not fed (the engine needs no mark to take a limit order).
struct Quiet;
impl LiveDataSink for Quiet {
    fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
    fn forming_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
    fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
    fn quote(&self, _: &str, _: &str, _: QuoteTick) {}
    fn trade(&self, _: &str, _: &str, _: TradeTick) {}
    fn book(&self, _: &str, _: &str, _: Arc<vike_marketdata::L2Book>) {}
}

fn quote(bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts: 1, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: String::new() }
}

fn print(price: f64) -> TradeTick {
    TradeTick { ts: 1, local_ts: 0, price, size: 1.0, is_buyer_maker: false, symbol: String::new() }
}

/// A post-only (or, with `tif` `None`, a plain) limit order of `side` (+1 buy, -1 sell) at `price`.
fn order(
    coid: &str,
    venue: &str,
    symbol: &str,
    side: i32,
    price: f64,
    tif: Option<&str>,
) -> WireCommand {
    let WireCommand::Submit(mut req) = limit(coid, venue, symbol, 0.01, price) else {
        unreachable!()
    };
    req.side = side;
    req.time_in_force = tif.map(str::to_string);
    WireCommand::Submit(req)
}

fn post_only(coid: &str, venue: &str, symbol: &str, side: i32, price: f64) -> WireCommand {
    order(coid, venue, symbol, side, price, Some("post-only"))
}

/// A node for `venue` at `tier`, a tap that has seen the venue's quote `BID`/`ASK` for `symbol`, and the facts.
fn node_with_book(venue: &'static str, symbol: &str, tier: AccountTier) -> (Node, TapFacts) {
    let tap = Arc::new(PriceTap::default());
    let sink = TapSink::new(Arc::new(Quiet), Arc::clone(&tap));
    sink.quote(venue, symbol, quote(BID, ASK));
    (Node::start(venue, symbol, tier), TapFacts { tap })
}

/// **A crossing post-only BUY is refused on every post-only venue**, in plain words with the order's price and
/// the best ask it meets, and the venue client is sent nothing. Bybit and OKX are the two that used to accept it
/// and cancel it with no reason.
#[test]
fn a_post_only_buy_through_the_ask_is_refused_by_name_with_both_numbers_on_every_venue() {
    for (venue, symbol) in POST_ONLY_VENUES {
        let (mut node, facts) = node_with_book(venue, symbol, AccountTier::Demo);
        let why = node
            .send(post_only("po-buy", venue, symbol, 1, 3001.0), Some(&facts))
            .expect_err(venue);
        assert!(
            why.contains("post-only buy 3001 would cross the best ask 3000.5"),
            "{venue}: {why}"
        );
        assert!(why.contains("NOT sent") && why.contains("taker"), "{venue}: {why}");
        assert!(why.contains(symbol) && why.contains(venue), "{venue}: {why}");
        assert!(node.submitted.lock().unwrap().is_empty(), "{venue}: a refusal sends nothing");
    }
}

/// **A crossing post-only SELL is refused the same way**, against the best bid.
#[test]
fn a_post_only_sell_through_the_bid_is_refused_by_name_with_both_numbers_on_every_venue() {
    for (venue, symbol) in POST_ONLY_VENUES {
        let (mut node, facts) = node_with_book(venue, symbol, AccountTier::Demo);
        let why = node
            .send(post_only("po-sell", venue, symbol, -1, 2998.0), Some(&facts))
            .expect_err(venue);
        assert!(
            why.contains("post-only sell 2998 would cross the best bid 2999.5"),
            "{venue}: {why}"
        );
        assert!(why.contains("NOT sent"), "{venue}: {why}");
        assert!(node.submitted.lock().unwrap().is_empty(), "{venue}: a refusal sends nothing");
    }
}

/// **The touch crosses, one step behind it rests.** A buy at the ask lifts it and a sell at the bid hits it
/// (`post_only_would_cross`'s edge); a buy a tick under the ask and a sell a tick over the bid are makers and
/// go out as post-only orders.
#[test]
fn the_touch_itself_crosses_and_a_price_behind_it_rests() {
    let (mut node, facts) = node_with_book("bybit", "ETHUSDT", AccountTier::Demo);
    for (coid, side, px) in [("at-ask", 1, ASK), ("at-bid", -1, BID)] {
        let why =
            node.send(post_only(coid, "bybit", "ETHUSDT", side, px), Some(&facts)).expect_err(coid);
        assert!(why.contains("would cross the best"), "{coid}: {why}");
    }
    assert!(node.submitted.lock().unwrap().is_empty());

    for (coid, side, px) in [("behind-ask", 1, BEHIND_ASK), ("behind-bid", -1, BEHIND_BID)] {
        node.send(post_only(coid, "bybit", "ETHUSDT", side, px), Some(&facts)).expect(coid);
        let seen = node.venue_saw(coid);
        assert_eq!(
            seen.time_in_force,
            TimeInForce::PostOnly,
            "{coid}: the lifetime reaches the venue"
        );
    }
}

/// **No top of book held is not a refusal**: nothing seen for the symbol, and a tap that has only seen a last
/// price (a trade print), both leave the order to the venue's own post-only guard. A mid is never read as a bid
/// and an ask.
#[test]
fn without_a_real_bid_and_ask_the_order_goes_to_the_venue() {
    let tap = Arc::new(PriceTap::default());
    let facts = TapFacts { tap: Arc::clone(&tap) };
    let mut node = Node::start("okx", "ETH-USDT-SWAP", AccountTier::Demo);

    node.send(post_only("none-seen", "okx", "ETH-USDT-SWAP", 1, 3001.0), Some(&facts))
        .expect("the tap holds nothing: the venue judges");
    assert_eq!(node.venue_saw("none-seen").time_in_force, TimeInForce::PostOnly);

    TapSink::new(Arc::new(Quiet), Arc::clone(&tap)).trade("okx", "ETH-USDT-SWAP", print(3000.0));
    assert_eq!(tap.door_price("okx", "ETH-USDT-SWAP"), Ok(Some(3000.0)), "a last price is held");
    node.send(post_only("last-only", "okx", "ETH-USDT-SWAP", 1, 3001.0), Some(&facts))
        .expect("a last price is no book: the venue judges");
    node.venue_saw("last-only");

    // And a surface with no facts source at all (no tap to read) refuses nothing here.
    node.send(post_only("no-source", "okx", "ETH-USDT-SWAP", -1, 1.0), None)
        .expect("no source, no check");
    node.venue_saw("no-source");
}

/// **A plain limit order is never touched**: a marketable plain limit is legal (it takes liquidity on purpose),
/// and a post-only MARKET order is `lifetime_refusal`'s, not this check's.
#[test]
fn a_plain_limit_order_priced_through_the_book_is_untouched() {
    let (mut node, facts) = node_with_book("bybit", "ETHUSDT", AccountTier::Demo);
    node.send(order("plain-buy", "bybit", "ETHUSDT", 1, 3001.0, None), Some(&facts))
        .expect("a marketable plain limit buy is legal");
    node.send(order("plain-sell", "bybit", "ETHUSDT", -1, 2998.0, Some("gtc")), Some(&facts))
        .expect("a marketable plain limit sell is legal");
    assert_eq!(node.venue_saw("plain-buy").time_in_force, TimeInForce::Gtc);
    assert_eq!(node.venue_saw("plain-sell").time_in_force, TimeInForce::Gtc);
}

/// **A `paper` book keeps its own law**: the paper exchange judges a post-only order against what its last bar
/// offered (`vike_fills`' one predicate), so the door, which reads a live book, leaves it alone.
#[test]
fn a_paper_book_is_left_to_its_own_post_only_law() {
    let (mut node, facts) = node_with_book("bybit", "ETHUSDT", AccountTier::Paper);
    node.send(post_only("paper-buy", "bybit", "ETHUSDT", 1, 3001.0), Some(&facts))
        .expect("the paper book judges it, not the door");
    node.venue_saw("paper-buy");
}

/// **A venue with no post-only order is refused by the capability row, first**: this check names no venue's
/// table, so the crossing order passes the door and the core refuses it `TIF_UNSUPPORTED`, as before.
#[test]
fn a_venue_with_no_post_only_is_still_refused_by_its_capability_row() {
    let (mut node, facts) = node_with_book("oanda", "EURUSD", AccountTier::Demo);
    node.send(post_only("no-po", "oanda", "EURUSD", 1, 3001.0), Some(&facts))
        .expect("the door does not judge a venue that has no post-only order");
    wait_until("the core to refuse it by name", || {
        node.core.snapshot_cell().load().orders.iter().any(|o| {
            o.client_order_id == "no-po"
                && matches!(o.status, OrderStatus::Rejected)
                && o.reason.as_deref().is_some_and(|r| r.contains("TIF_UNSUPPORTED"))
        })
    });
    assert!(node.submitted.lock().unwrap().is_empty(), "the capability row sent nothing");
}

/// **The preview says what the command will say** while the tap holds the same book: `submit_symbol_refusal`
/// (the Preview verb's and the Telegram preview's last step) returns the refusal, and nothing for a maker.
#[test]
fn the_preview_agrees_with_the_command() {
    let (mut node, facts) = node_with_book("bybit", "ETHUSDT", AccountTier::Demo);
    let blocks = node.blocks();
    let crossing = post_only("pv-1", "bybit", "ETHUSDT", 1, 3001.0);
    let sent = node.send(crossing.clone(), Some(&facts)).expect_err("the command is refused");
    assert_eq!(submit_symbol_refusal(&crossing, &blocks, Some(&facts)), Some(sent));

    let maker = post_only("pv-2", "bybit", "ETHUSDT", 1, BEHIND_ASK);
    assert_eq!(submit_symbol_refusal(&maker, &blocks, Some(&facts)), None);
}
