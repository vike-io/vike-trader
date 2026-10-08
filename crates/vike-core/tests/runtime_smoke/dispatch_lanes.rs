//! Piece 3 live tick / L2 dispatch, Piece 2 modify by tag, Piece 5 batch submit and mass-cancel.

use vike_exec::TradeUpdate;
use vike_model::TradeTick;

use super::*;

// ---- Piece 3: live tick / L2 dispatch ----

/// Counts each tick handler and submits a market order from it — proving both that the live
/// runtime dispatches the sub-bar lanes AND that a submission made inside a tick handler routes
/// through the one live path (mint → RiskGate → engine).
#[derive(Default)]
struct TickCounter {
    quotes: Arc<AtomicUsize>,
    trades: Arc<AtomicUsize>,
    books: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for TickCounter {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
    fn on_trade_tick(&mut self, broker: &mut LiveBroker, _t: &TradeTick) {
        self.trades.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
    fn on_order_book(&mut self, broker: &mut LiveBroker, _b: &L2Book) {
        self.books.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
}

#[test]
fn tick_lanes_dispatch_and_route_orders() {
    let engine = engine_on("binance", "BTCUSDT", RecordingClient::default());
    let quotes = Arc::<AtomicUsize>::default();
    let trades = Arc::<AtomicUsize>::default();
    let books = Arc::<AtomicUsize>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(TickCounter {
            quotes: Arc::clone(&quotes),
            trades: Arc::clone(&trades),
            books: Arc::clone(&books),
        }),
    ));
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: String::new(),
            },
        })
        .unwrap();
    ticks
        .trade(TradeUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade: TradeTick {
                ts: 2,
                local_ts: 0,
                price: 100.1,
                size: 0.5,
                is_buyer_maker: false,
                symbol: String::new(),
            },
        })
        .unwrap();
    let mut book = L2Book::new(0.01);
    book.apply_snapshot(1, &[BookLevel::new(100.0, 5.0)], &[BookLevel::new(101.0, 5.0)]); // two-sided → mid = 100.5
    ticks
        .book(BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(book),
        })
        .unwrap();

    handle.shutdown_and_join();

    assert_eq!(quotes.load(Ordering::Relaxed), 1, "on_quote_tick fired once");
    assert_eq!(trades.load(Ordering::Relaxed), 1, "on_trade_tick fired once");
    assert_eq!(books.load(Ordering::Relaxed), 1, "on_order_book fired once");

    // each handler's market order routed through the live path into the engine registry
    let snap = cell.load();
    assert_eq!(snap.orders.len(), 3, "one routed market order per tick handler");
    assert!(snap.orders.iter().all(|o| o.order_type == "market" && o.side == 1 && o.qty == 1.0));
}

// ---- Piece 2: modify / cancel-replace (strategy-assigned tags) ----

/// Rests a tagged limit on the first quote, then re-prices/-sizes it by tag on later quotes —
/// never seeing the client_order_id (the runtime mints and tracks it under the tag).
struct TagModifier {
    submitted: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for TagModifier {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if self.submitted.swap(true, Ordering::Relaxed) {
            broker.modify("bid", Some(2.0), Some(99.0)); // 2nd+ quote: re-quote by tag
        } else {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0); // 1st quote: rest a tagged order
        }
    }
}

#[test]
fn modify_by_tag_reprices_resting_order_in_place() {
    let engine = engine_on("binance", "BTCUSDT", ModifiableClient::default());
    let submitted = Arc::<AtomicBool>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(TagModifier { submitted: Arc::clone(&submitted) }),
    ));
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    let quote = |ts| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    ticks.quote(quote(1)).unwrap(); // submit tagged "bid" (qty 1 @ 100)
    ticks.quote(quote(2)).unwrap(); // modify "bid" -> qty 2 @ 99

    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "modified in place — NOT canceled + re-created");
    let o = &snap.orders[0];
    assert_eq!(o.status, OrderStatus::Accepted, "still resting after modify");
    assert_eq!(o.qty, 2.0, "qty modified by tag");
    assert_eq!(o.price, Some(99.0), "price modified by tag");
}

// ---- Piece 5: batch submit / mass-cancel ----

fn limit(coid: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(px),
        ..Default::default()
    }
}

#[test]
fn command_batch_submit_then_cancel_batch() {
    let handle = spawn_core(batch_engine(RiskLimits::new()), test_config(0.0));
    let cell = handle.snapshot_cell();
    handle
        .try_command(Command::Order(OrderIntent::SubmitBatch(vec![
            limit("b1", 1, 1.0, 100.0),
            limit("b2", 1, 1.0, 100.5),
            limit("b3", -1, 1.0, 101.0),
        ])))
        .unwrap();
    handle
        .try_command(Command::Order(OrderIntent::CancelBatch(vec!["b1".into(), "b3".into()])))
        .unwrap();
    handle.shutdown_and_join(); // lossless: both commands fold before join

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 3, "all three batch orders registered");
    let status =
        |coid: &str| snap.orders.iter().find(|o| o.client_order_id == coid).unwrap().status;
    assert_eq!(status("b1"), OrderStatus::Canceled);
    assert_eq!(status("b2"), OrderStatus::Accepted, "not in the cancel batch");
    assert_eq!(status("b3"), OrderStatus::Canceled);
}

#[test]
fn batch_submit_denies_over_cap_and_submits_the_rest() {
    let limits = RiskLimits { max_notional_per_order: Some(1_000.0), ..RiskLimits::new() };
    let handle = spawn_core(batch_engine(limits), test_config(0.0));
    let cell = handle.snapshot_cell();
    handle
        .try_command(Command::Order(OrderIntent::SubmitBatch(vec![
            limit("ok1", 1, 1.0, 100.0),   // notional 100 — ok
            limit("big", 1, 1.0e9, 100.0), // notional 1e11 — DENIED
            limit("ok2", 1, 2.0, 100.0),   // notional 200 — ok
        ])))
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 2, "the over-cap order is denied, not registered");
    assert!(
        snap.orders.iter().all(|o| o.client_order_id != "big"),
        "denied order absent; the rest of the batch still submitted"
    );
}

/// Submits two tagged quotes on the first tick, then pulls ALL quotes via mass_cancel on the next.
struct QuoteThenPull {
    submitted: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for QuoteThenPull {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if self.submitted.swap(true, Ordering::Relaxed) {
            broker.mass_cancel();
        } else {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
            broker.submit_limit_tagged("ask", -1, 1.0, 101.0);
        }
    }
}

#[test]
fn mass_cancel_pulls_all_live_orders() {
    let submitted = Arc::<AtomicBool>::default();
    let mut config = test_config(0.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(QuoteThenPull { submitted: Arc::clone(&submitted) }),
    ));
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    let quote = |ts| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    ticks.quote(quote(1)).unwrap(); // submit bid + ask (both become Accepted)
    ticks.quote(quote(2)).unwrap(); // mass_cancel

    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 2);
    assert!(
        snap.orders.iter().all(|o| o.status == OrderStatus::Canceled),
        "mass_cancel canceled every resting order"
    );
}
