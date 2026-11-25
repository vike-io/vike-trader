//! The strategy hooks `on_feed_status`, `on_flow` and `on_order_event` through the live runtime.

use vike_execution::{FlowUpdate, StreamStatusUpdate};
use vike_model::events::OrderRejected;
use vike_model::{FeedStatus, FlowToxicity, OrderEvent, OrderEventKind};

use super::*;
use crate::kit::events::binance_quote;

// ---- feed-health hook: Strategy::on_feed_status (audit co7) ------------------------------------
//
// Feed-death signals (StreamHealth Gap/Stale/Live) reach a MOUNTED strategy through the additive
// Ingest::StreamStatus lane + the runtime's drive_strategy_feed_status dispatch. The hook fires
// ONLY on a status change (an occasional control event), NOT per market message — so it never
// touches the per-tick / event-fold hot path.

/// Records every `on_feed_status` transition, and separately counts `on_quote_tick` — so one test
/// can assert (a) a StreamStatus lane message fires `on_feed_status` with the mapped `FeedStatus`,
/// and (b) a plain market quote fires `on_quote_tick` but NOT `on_feed_status`.
struct FeedStatusRecorder {
    statuses: Arc<Mutex<Vec<FeedStatus>>>,
    quotes: Arc<AtomicUsize>,
}
impl Strategy<LiveBroker> for FeedStatusRecorder {
    fn on_quote_tick(&mut self, _broker: &mut LiveBroker, _q: &TickQuote) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_feed_status(&mut self, _broker: &mut LiveBroker, status: FeedStatus) {
        self.statuses.lock().unwrap().push(status);
    }
}

/// The required gate: a stream-status transition on the ingest lane fires `on_feed_status` (down
/// then up), while a normal market quote fires `on_quote_tick` and NEVER `on_feed_status`. Both
/// lanes are lossless + shutdown is lossless, so all three messages fold before join — deterministic.
#[test]
fn feed_status_hook_fires_on_change_not_per_market_message() {
    let engine = engine_on("binance", "BTCUSDT", RecordingClient::default());
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let quotes = Arc::<AtomicUsize>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(FeedStatusRecorder {
            statuses: Arc::clone(&statuses),
            quotes: Arc::clone(&quotes),
        }),
    ));
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // a normal market message (quote) — fires on_quote_tick, must NOT fire on_feed_status
    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: TickQuote {
                timestamp: 1,
                local_timestamp: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: Default::default(),
            },
        })
        .unwrap();
    // feed dies, then recovers — each transition fires on_feed_status exactly once
    let status = |s: FeedStatus| StreamStatusUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        stream: "quotes".into(),
        status: s,
    };
    ticks.stream_status(status(FeedStatus::Disconnected)).unwrap();
    ticks.stream_status(status(FeedStatus::Live)).unwrap();

    handle.shutdown_and_join();

    assert_eq!(quotes.load(Ordering::Relaxed), 1, "the market quote fired on_quote_tick once");
    assert_eq!(
        *statuses.lock().unwrap(),
        vec![FeedStatus::Disconnected, FeedStatus::Live],
        "on_feed_status fired once per status change (down then up) and NOT for the market quote"
    );
}

/// A status update for a DIFFERENT (venue, symbol) than the mount must never reach the strategy —
/// the dispatch routes by (venue, symbol), so an unrelated feed's death is ignored.
#[test]
fn feed_status_for_other_symbol_is_ignored() {
    let engine = engine_on("binance", "BTCUSDT", RecordingClient::default());
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(FeedStatusRecorder {
            statuses: Arc::clone(&statuses),
            quotes: Arc::<AtomicUsize>::default(),
        }),
    ));
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // ETHUSDT feed dies — the BTCUSDT mount must not hear it
    ticks
        .stream_status(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            stream: "quotes".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();
    // the mounted symbol's feed dies — this one DOES reach the strategy
    ticks
        .stream_status(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "quotes".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();
    handle.shutdown_and_join();

    assert_eq!(
        *statuses.lock().unwrap(),
        vec![FeedStatus::Disconnected],
        "only the mounted (venue, symbol)'s status change reaches the strategy"
    );
}

// ---- flow-toxicity hook: Strategy::on_flow (RTDS wallet-toxicity guard, 5c) --------------------
//
// Per-side toxic-flow readings reach a MOUNTED strategy through the additive Ingest::Flow lane +
// the runtime's drive_strategy_flow dispatch — the exact twin of the feed-status lane above. The
// hook fires only on a toxicity update (an occasional control event), routed by the mount's OWN
// (venue, symbol), never touching the per-tick / event-fold hot path.

/// Records every `on_flow` reading so a test can assert the runtime routed the right FlowToxicity.
struct FlowRecorder {
    flows: Arc<Mutex<Vec<FlowToxicity>>>,
}
impl Strategy<LiveBroker> for FlowRecorder {
    fn on_flow(&mut self, _broker: &mut LiveBroker, flow: FlowToxicity) {
        self.flows.lock().unwrap().push(flow);
    }
}

/// A flow update for the MOUNTED (venue, symbol) fires `on_flow` with exactly the delivered reading,
/// while a flow for a DIFFERENT (venue, symbol) never reaches the strategy — the dispatch routes by
/// the mount's own key, the same predicate the feed-status lane uses.
#[test]
fn flow_hook_fires_for_the_mounted_pair_only() {
    let engine = engine_on("binance", "BTCUSDT", RecordingClient::default());
    let flows = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(FlowRecorder { flows: Arc::clone(&flows) }),
    ));
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // a DIFFERENT symbol's toxicity — the BTCUSDT mount must not hear it
    ticks
        .flow(FlowUpdate {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            flow: FlowToxicity { bid: 0.9, ask: 0.9, timestamp: 1 },
        })
        .unwrap();
    // the mounted (venue, symbol)'s toxicity — this one DOES reach the strategy
    ticks
        .flow(FlowUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            flow: FlowToxicity { bid: 0.25, ask: 0.75, timestamp: 2 },
        })
        .unwrap();

    handle.shutdown_and_join();

    assert_eq!(
        *flows.lock().unwrap(),
        vec![FlowToxicity { bid: 0.25, ask: 0.75, timestamp: 2 }],
        "only the mounted (venue, symbol)'s toxicity reaches on_flow"
    );
}

/// Rests a tagged quote on the first market quote, then PULLS it via `mass_cancel` when the feed
/// disconnects — the canonical "pull my quotes when my feed dies" behavior. Proves orders buffered
/// INSIDE `on_feed_status` route through the one live path (drain_broker) exactly like the tick lanes.
struct PullOnDisconnect {
    rested: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for PullOnDisconnect {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &TickQuote) {
        if !self.rested.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
        }
    }
    fn on_feed_status(&mut self, broker: &mut LiveBroker, status: FeedStatus) {
        if matches!(status, FeedStatus::Disconnected | FeedStatus::Stale) {
            broker.mass_cancel(); // pull all resting quotes on a dead/stale feed
        }
    }
}

#[test]
fn feed_status_disconnect_pulls_resting_quotes() {
    let rested = Arc::<AtomicBool>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(PullOnDisconnect { rested: Arc::clone(&rested) }),
    ));
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    // rest a tagged quote (BatchTestClient accepts → Accepted)
    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: TickQuote {
                timestamp: 1,
                local_timestamp: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: Default::default(),
            },
        })
        .unwrap();
    // feed dies → the strategy pulls its resting quote from inside on_feed_status
    ticks
        .stream_status(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "quotes".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "the one resting quote (pulled, not re-created)");
    assert_eq!(
        snap.orders[0].status,
        OrderStatus::Canceled,
        "the resting quote was pulled on disconnect via on_feed_status → mass_cancel"
    );
}

// ---- order-lifecycle hook: Strategy::on_order_event (position-executor stage 3) ----------------
//
// A NON-FILL order outcome (a venue REJECT / CANCEL / EXPIRE, or a RiskGate DENY) reaches the
// mounted strategy that OWNS the order through the additive on_order_event hook: the engine captures
// the transition (tagged with the order's (venue, symbol)) and the runtime routes it to that mount's
// hook — the on_fill lane's twin. It fires at ORDER cadence, never per market message, so a plain
// quote NEVER fires it. Fills stay on on_fill (never double-delivered here).

/// Rejects every order at the venue (emits OrderRejected for the submitted order client id). The gate passes
/// first, so the order registers then goes Submitted → Rejected — the venue-reject path
/// on_order_event must surface.
#[derive(Default)]
struct RejectingClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for RejectingClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: request.order_client_id.clone(),
            reason: "venue nope".into(),
            timestamp: request.timestamp,
            refusal_id: None,
        }));
    }
    fn cancel(&mut self, _order_client_id: &str) {} // never canceled in the reject test
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// Records every on_order_event, counts on_quote_tick, and submits ONE tagged limit on its first
/// quote (so an order client id is minted and its outcome routes back). When `cancel_on_later` is set it cancels
/// that tagged order on every subsequent quote — driving a venue cancel through the same hook.
struct OrderEventRecorder {
    events: Arc<Mutex<Vec<OrderEvent>>>,
    quotes: Arc<AtomicUsize>,
    submitted: Arc<AtomicBool>,
    cancel_on_later: bool,
    tag: &'static str,
}
impl Strategy<LiveBroker> for OrderEventRecorder {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &TickQuote) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
        if !self.submitted.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged(self.tag, 1, 1.0, 100.0);
        } else if self.cancel_on_later {
            broker.cancel_tagged(self.tag);
        }
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

/// The required gate: a venue REJECT of the strategy's own order fires on_order_event exactly once
/// (with the reject reason + the strategy's order client id), while a plain market quote fires on_quote_tick and
/// NEVER on_order_event. The tick lane is lossless + shutdown is lossless, so all messages fold
/// before join — deterministic.
#[test]
fn order_event_hook_fires_on_reject_not_per_market_message() {
    let engine = engine_on("binance", "BTCUSDT", RejectingClient::default());
    let events = Arc::new(Mutex::new(Vec::new()));
    let quotes = Arc::<AtomicUsize>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(OrderEventRecorder {
            events: Arc::clone(&events),
            quotes: Arc::clone(&quotes),
            submitted: Arc::<AtomicBool>::default(),
            cancel_on_later: false,
            tag: "entry",
        }),
    ));
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks.quote(binance_quote(1)).unwrap(); // submit → venue REJECTS → on_order_event(Rejected)
    ticks.quote(binance_quote(2)).unwrap(); // a plain market quote — on_quote_tick, NOT on_order_event
    handle.shutdown_and_join();

    assert_eq!(quotes.load(Ordering::Relaxed), 2, "both market quotes fired on_quote_tick");
    let got = events.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "exactly ONE order event (the reject), NOT one per market quote: {got:?}"
    );
    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "the one submitted-then-rejected order");
    assert_eq!(snap.orders[0].status, OrderStatus::Rejected);
    assert_eq!(
        got[0],
        OrderEvent {
            order_client_id: snap.orders[0].order_client_id.clone(),
            // The STRATEGY's own name for the order — it submitted with `submit_limit_tagged`, so
            // the runtime stamps the tag it filed in `strategy_tags`. This is the half a tagged-order
            // strategy can actually match on: it never sees the order client id beside it.
            tag: Some("entry".into()),
            kind: OrderEventKind::Rejected { reason: "venue nope".into() },
        },
        "on_order_event delivered the venue reject (reason + the strategy's OWN order client id AND tag)",
    );
}

/// A venue ACCEPT then CANCEL of the strategy's own order each fire on_order_event once, in order,
/// for the same order client id — and fills are NOT among them (they flow on_fill). Uses BatchTestClient
/// (accepts on submit, cancels on cancel).
#[test]
fn order_event_hook_fires_on_accept_then_cancel() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(OrderEventRecorder {
            events: Arc::clone(&events),
            quotes: Arc::<AtomicUsize>::default(),
            submitted: Arc::<AtomicBool>::default(),
            cancel_on_later: true,
            tag: "entry",
        }),
    ));
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let ticks = handle.tick_sender();

    ticks.quote(binance_quote(1)).unwrap(); // submit → BatchTestClient ACCEPTS → on_order_event(Accepted)
    ticks.quote(binance_quote(2)).unwrap(); // cancel_tagged → CANCELED → on_order_event(Canceled)
    handle.shutdown_and_join();

    let got = events.lock().unwrap().clone();
    let kinds: Vec<_> = got.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![OrderEventKind::Accepted, OrderEventKind::Canceled { reason: "batch".into() }],
        "on_order_event saw the venue accept then the cancel, in order (no fill among them): {got:?}",
    );
    assert!(
        got.iter().all(|e| e.order_client_id == got[0].order_client_id),
        "both transitions carried the SAME order order client id: {got:?}",
    );
}

/// Accepts every order at the venue, then FULLY FILLS it — the market maker's ordinary outcome, and
/// the one that used to reach a tagged-order strategy as nothing at all.
#[derive(Default)]
struct FillingClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for FillingClient {
    fn submit(&mut self, request: &OrderRequest) {
        // Initialized → Submitted → Accepted → Filled: the FSM takes each step, so the submit echo
        // leads (the `BatchTestClient` shape).
        accept_submit(&mut self.events, request);
        // ...then the fill, in the order a real venue mapper emits it: `Fill` (the MONEY lane —
        // `Account::apply_fill`, which is what populates `applied_fills` and drives `on_fill`) then
        // `OrderFilled` (the FSM terminal). `crates/bridges/binance/src/family/execution/event_mapper/spot.rs`'s
        // `map_execution_report` produces exactly this pair, in exactly this order.
        let fill = FillEvent {
            trade_id: "t1".into(),
            order_client_id: request.order_client_id.clone(),
            venue: request.venue.clone().into(),
            symbol: request.symbol.clone().into(),
            side: request.side,
            last_qty: request.qty, // the WHOLE order — this terminalizes it
            last_price: request.price.unwrap_or(100.0),
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "maker".into(),
            timestamp: request.timestamp,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        };
        self.events.push_back(Event::Fill(fill.clone()));
        self.events.push_back(Event::OrderFilled(vike_model::events::OrderFilled {
            order_client_id: request.order_client_id.clone(),
            fill,
            timestamp: request.timestamp,
        }));
    }
    fn cancel(&mut self, _order_client_id: &str) {}
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// ⚠ THE WEDGE, at the runtime seam. A tagged quote that FILLS COMPLETELY must reach its strategy as
/// a terminal `OrderEvent` carrying the strategy's OWN tag — the only name a tagged-order
/// strategy can match, since it never sees an order-client-id.
///
/// Before this existed the fill lane delivered `on_fill` and stopped: `OrderEvent::from_event`
/// returns `None` for every fill event, so a full fill produced NO lifecycle event at all, and
/// `Fill` carries neither order identity nor remaining quantity. A maker therefore went on believing
/// its filled quote was resting, re-priced a terminal order client id every tick, and
/// `Engine::modify_order`'s not-modifiable early return swallowed each one silently —
/// measured on the the CI box live mount as a permanent `orders:2` ceiling with `working` at 0.
#[test]
fn a_fully_filled_tagged_order_reaches_the_strategy_as_a_terminal_event_with_its_tag() {
    let engine = engine_on("binance", "BTCUSDT", FillingClient::default());
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(OrderEventRecorder {
            events: Arc::clone(&events),
            quotes: Arc::<AtomicUsize>::default(),
            submitted: Arc::<AtomicBool>::default(),
            cancel_on_later: false,
            tag: "bid",
        }),
    ));
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();
    ticks.quote(binance_quote(1)).unwrap(); // submit "bid" → the client queues ACCEPTED + a FULL fill
    ticks.quote(binance_quote(2)).unwrap(); // a second dispatch folds them (as the reject test does)
    handle.shutdown_and_join();

    let got = events.lock().unwrap().clone();
    let kinds: Vec<_> = got.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![OrderEventKind::Accepted, OrderEventKind::Filled],
        "the accept, then the ORDER's completion — the death `on_fill` cannot express: {got:?}",
    );
    assert!(
        got.iter().all(|e| e.tag.as_deref() == Some("bid")),
        "every transition named the strategy's OWN tag, the only name it can match: {got:?}",
    );
}

/// Records on_order_event per mount, tagging each event with the mount that received it, so a test
/// can prove an event for an order client id owned by one mount NEVER reaches the other.
struct TaggedOrderEventRecorder {
    label: &'static str,
    events: Arc<Mutex<Vec<(&'static str, OrderEvent)>>>,
    submitted: Arc<AtomicBool>,
    tag: &'static str,
}
impl Strategy<LiveBroker> for TaggedOrderEventRecorder {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &TickQuote) {
        if !self.submitted.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged(self.tag, 1, 1.0, 100.0);
        }
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.events.lock().unwrap().push((self.label, event.clone()));
    }
}

/// Multi-mount isolation: two mounts (BTCUSDT + ETHUSDT over ONE engine via extra_symbols /
/// extra_mounts) each submit their own order; the ACCEPT for each routes to the mount that OWNS that
/// order's (venue, symbol) and to NO other — the same routing key on_fill uses.
#[test]
fn order_event_routes_to_the_owning_mount_only() {
    let mut engine = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        BatchTestClient::default(),
        "binance",
        "BTCUSDT",
    );
    engine.extra_symbols = vec!["ETHUSDT".into()];

    let events = Arc::new(Mutex::new(Vec::new()));
    let mount = |label, symbol: &str, tag| {
        mount_of(
            "binance",
            symbol,
            "1m",
            Box::new(TaggedOrderEventRecorder {
                label,
                events: Arc::clone(&events),
                submitted: Arc::<AtomicBool>::default(),
                tag,
            }),
        )
    };
    let mut config = test_config(1.0);
    config.strategy = Some(mount("btc", "BTCUSDT", "b"));
    config.extra_mounts = vec![mount("eth", "ETHUSDT", "e")];
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    let quote = |symbol: &str, ts: i64| QuoteUpdate {
        venue: "binance".into(),
        symbol: symbol.into(),
        quote: TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: Default::default(),
        },
    };
    ticks.quote(quote("BTCUSDT", 1)).unwrap(); // btc mount submits → Accepted(btc order client id)
    ticks.quote(quote("ETHUSDT", 2)).unwrap(); // eth mount submits → Accepted(eth order client id)
    handle.shutdown_and_join();

    let snap = cell.load();
    let order_client_id_of = |symbol: &str| {
        snap.orders.iter().find(|o| o.symbol == symbol).unwrap().order_client_id.clone()
    };
    let (btc_order_client_id, eth_order_client_id) =
        (order_client_id_of("BTCUSDT"), order_client_id_of("ETHUSDT"));
    assert_ne!(btc_order_client_id, eth_order_client_id, "distinct orders, one per symbol");

    let got = events.lock().unwrap().clone();
    // each mount heard EXACTLY its own order's accept — and never the other's
    let for_label = |label: &str| -> Vec<OrderEvent> {
        got.iter().filter(|(l, _)| *l == label).map(|(_, e)| e.clone()).collect()
    };
    assert_eq!(
        for_label("btc"),
        vec![OrderEvent {
            order_client_id: btc_order_client_id.clone(),
            tag: Some("b".into()),
            kind: OrderEventKind::Accepted
        }],
        "the BTC mount heard only its own order's accept: {got:?}",
    );
    assert_eq!(
        for_label("eth"),
        vec![OrderEvent {
            order_client_id: eth_order_client_id.clone(),
            tag: Some("e".into()),
            kind: OrderEventKind::Accepted
        }],
        "the ETH mount heard only its own order's accept: {got:?}",
    );
}
