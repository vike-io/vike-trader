use super::*;
use crate::order::{ManagedOrder, OrderStatus};
use vike_model::BookLevel;

/// The journal serializes Ingest verbatim; every journaled Ingest variant AND every
/// journaled Command variant (Command rides inside `Ingest::Command`) must round-trip
/// byte-for-byte through JSON. The loop below constructs one instance of every `Ingest`
/// variant and every `Command` variant — including a populated `ManagedOrder` inside
/// `ApplySnapshot`'s `ReconcileSnapshot` and a non-empty `L2Book` (≥1 bid, ≥1 ask) inside
/// `Book`, so the compiler-forced integer-keyed `BTreeMap` wire form is actually exercised.
#[test]
fn ingest_round_trips_through_json() {
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "j1", "venue": "sim", "symbol": "BTCUSDT",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
    }))
    .unwrap();

    // A populated ManagedOrder — not just ManagedOrder::new's zeroed fields — so the
    // Option<String>/Option<i64> Some(..) arms round-trip too, not only the None arms.
    let mut snapshot_order = ManagedOrder::new(req.clone());
    snapshot_order.status = OrderStatus::Accepted;
    snapshot_order.venue_order_id = Some("v-1".to_string());
    snapshot_order.filled_qty = 0.5;
    snapshot_order.avg_fill_px = 100.25;
    snapshot_order.created_ms = Some(12_345);

    let snapshot = ReconcileSnapshot {
        positions: vec![("BTCUSDT".into(), 1.0)],
        open_orders: vec![snapshot_order],
        position_avg_px: vec![("BTCUSDT".into(), 100.0)],
        position_mark_px: vec![("BTCUSDT".into(), 100.5)],
        position_sides: vec![("BTCUSDT".into(), "BOTH".into())],
        balance: 10_000.0,
        // Populated (not empty) so the margin-mode carrier's Some arm round-trips too.
        position_margin: vec![("BTCUSDT".into(), vike_model::MarginMode::Isolated, Some(50.0))],
    };

    // Non-empty L2Book (>=1 bid, >=1 ask) — the empty book would trivially round-trip
    // even if the integer-keyed BTreeMap wire form were broken.
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(1, &[BookLevel::new(100.0, 1.5)], &[BookLevel::new(100.5, 2.0)]);
    let book = Arc::new(book);

    for msg in [
        Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(req.clone())))),
        Ingest::Command(Command::Order(OrderIntent::Cancel("j1".into()))),
        Ingest::Command(Command::Order(OrderIntent::Modify {
            client_order_id: "j1".into(),
            new_qty: Some(2.0),
            new_price: Some(101.0),
        })),
        Ingest::Command(Command::Order(OrderIntent::SubmitBatch(vec![req.clone()]))),
        Ingest::Command(Command::Order(OrderIntent::CancelBatch(vec!["j1".into()]))),
        Ingest::Command(Command::Order(OrderIntent::Confirm("j1".into()))),
        Ingest::Command(Command::UpdateParams(Box::new(ParamsUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            params: StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
                qty: 1.0,
                half_spread: 0.5,
                target_inventory: 0.0,
                max_inventory: 1.0,
                skew: 0.25,
                fill_window_ms: 1_000,
                net_fill_threshold: 2.5,
                suppress_cooldown_ms: 5_000,
                style: vike_model::QuoteStyle::Join,
                depth_levels: 2,
                tick_size: 0.5,
                filter_own: true,
                avellaneda_stoikov: None,
                refresh_tolerance: None,
                ladder: None,
                reward: None,
                toxicity: None,
            }),
        }))),
        Ingest::Command(Command::Order(OrderIntent::MassCancel {
            venue: None,
            symbol: None,
            account: None,
        })),
        Ingest::Command(Command::SetTradingState(TradingState::Active)),
        Ingest::Command(Command::SetMargin(Box::new(MarginUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            im_requirement: 0.1,
        }))),
        Ingest::Command(Command::ApplySnapshot(Box::new(snapshot))),
        // ReconcileReports with a NON-empty `hybrid` policy so the enum-keyed
        // `BTreeMap<DivergenceKind, ReconMode>` (JSON string keys) actually round-trips.
        Ingest::Command(Command::ReconcileReports(Box::new(crate::ReconcileReports {
            venue: "binance".into(),
            since: 42,
            orders: Vec::new(),
            fills: vec![vike_model::FillReport {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                trade_id: "t1".into(),
                venue_order_id: "v9".into(),
                client_order_id: None,
                side: 1,
                last_qty: 1.0,
                last_px: 100.0,
                commission: 0.0,
                commission_asset: "USDT".into(),
                liquidity_side: vike_model::events::LiquiditySide::Taker,
                ts: 5,
            }],
            positions: Vec::new(),
            policy: crate::recon::ReconPolicy::hybrid(),
            balance: Some(9999.0),
            generate_missing_orders: false,
            reconcile_balance: false,
            balance_tol: crate::recon::BalanceTol::default(),
            // Spelled `Some` HERE only so the field is exercised on the wire; every producer
            // in this tree sends `None`. The pre-field-journal shape is gated separately by
            // `a_pre_route_key_journal_replays_routing_to_its_venue`.
            route_key: Some("binance-sub2".into()),
        }))),
        Ingest::Command(Command::ConfirmRecon(7)),
        Ingest::Command(Command::Shutdown),
        Ingest::Event(vike_model::events::Event::OrderSubmitted(
            vike_model::events::OrderSubmitted { client_order_id: "j1".into(), ts: 7 },
        )),
        Ingest::Watchdog,
        Ingest::Market,
        Ingest::BarSeed(Box::new(BarSeed {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bars: vec![Bar {
                ts: 1,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume: 10.0,
                funding: Some(0.0001),
                bid: Some(100.4),
                ask: Some(100.6),
                symbol: Some("BTCUSDT".into()),
            }],
        })),
        Ingest::BarClose(Box::new(BarUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: Bar {
                ts: 2,
                open: 100.5,
                high: 101.5,
                low: 100.0,
                close: 101.0,
                volume: 5.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            },
        })),
        Ingest::Quote(Box::new(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 3,
                local_ts: 0,
                bid: 100.4,
                ask: 100.6,
                bid_size: 1.0,
                ask_size: 1.2,
                symbol: "BTCUSDT".into(),
            },
        })),
        Ingest::Trade(Box::new(TradeUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade: TradeTick {
                ts: 4,
                local_ts: 0,
                price: 100.5,
                size: 0.25,
                is_buyer_maker: true,
                symbol: "BTCUSDT".into(),
            },
        })),
        Ingest::Book(Box::new(BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book,
        })),
        Ingest::StreamStatus(Box::new(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "quotes".into(),
            status: FeedStatus::Disconnected,
        })),
        Ingest::Flow(Box::new(FlowUpdate {
            venue: "polymarket".into(),
            symbol: "TOK".into(),
            flow: FlowToxicity { bid: 0.25, ask: 0.75, ts: 9 },
        })),
    ] {
        let json = serde_json::to_string(&msg).unwrap();
        let back: Ingest = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{msg:?}"), format!("{back:?}"), "lossy round-trip");
    }
}

/// PIN: `BookUpdate.book` behind an `Arc` must NOT change one wire byte. The
/// `#[serde(serialize_with/deserialize_with)]` pair forwards to `L2Book`'s own impls, so the
/// field is written exactly as the bare book. This compares the real `BookUpdate` JSON against a
/// hand-built object carrying the SAME book serialized directly, and asserts string equality
/// (a round-trip alone would also pass a differently-shaped but self-consistent encoding).
#[test]
fn arc_book_field_serializes_exactly_like_a_bare_book() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(
        7,
        &[BookLevel::new(100.0, 1.5), BookLevel::new(99.5, 2.0)],
        &[BookLevel::new(100.5, 2.0)],
    );

    let with_arc = serde_json::to_string(&BookUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        book: Arc::new(book.clone()),
    })
    .unwrap();
    // The pre-Arc encoding, reconstructed independently: the same three fields in declaration
    // order, the book serialized straight off `L2Book`'s derive.
    let bare = format!(
        r#"{{"venue":"binance","symbol":"BTCUSDT","book":{}}}"#,
        serde_json::to_string(&book).unwrap()
    );
    assert_eq!(with_arc, bare, "Arc<L2Book> must be wire-transparent");

    // …and back, into a FRESH Arc (identity is not preserved; value is).
    let back: BookUpdate = serde_json::from_str(&with_arc).unwrap();
    assert_eq!(format!("{:?}", *back.book), format!("{book:?}"));
}

#[test]
fn reconcile_reports_deserializes_pre_generate_missing_orders_journals() {
    // Back-compat pin: a journal written BEFORE `generate_missing_orders` existed carries no
    // such key — `#[serde(default)]` must fill in `false` (the inert value), not error.
    let json = serde_json::json!({
        "venue": "binance",
        "since": 42,
        "orders": [],
        "fills": [],
        "positions": [],
        "policy": { "default": "Synthesize", "per_kind": {} },
        "balance": null
    });
    let reports: crate::ReconcileReports = serde_json::from_value(json).unwrap();
    assert!(!reports.generate_missing_orders, "absent field defaults to the inert false");
    assert!(!reports.reconcile_balance, "absent reconcile_balance defaults to the inert false");
    assert_eq!(
        reports.balance_tol,
        crate::recon::BalanceTol::default(),
        "absent balance_tol defaults to the conservative constant"
    );
}

/// The routing half of the same back-compat pin, kept SEPARATE because it is not a flag: a
/// pre-`route_key` journal carries no key, and the replay must route to the same engine the
/// live pass did — the venue's sole account — rather than to the empty string (which resolves
/// no engine at all, so the whole replayed pass would be dropped with a "no engine for venue"
/// note).
#[test]
fn a_pre_route_key_journal_replays_routing_to_its_venue() {
    let json = serde_json::json!({
        "venue": "binance",
        "since": 42,
        "orders": [],
        "fills": [],
        "positions": [],
        "policy": { "default": "Synthesize", "per_kind": {} },
        "balance": null
    });
    let reports: crate::ReconcileReports = serde_json::from_value(json).unwrap();
    assert_eq!(reports.route_key, None, "absent field is the sole-account default");
    assert_eq!(
        reports.route().as_str(),
        "binance",
        "…and it routes to the venue, exactly as the one-field payload did"
    );
}

/// …and a payload that DOES carry one routes by it, not by the venue — the two-account shape
/// this field exists for. The negative control for the test above: without it, a `route()`
/// hard-wired to `self.venue` would pass that one and this pass/fail pair is what separates
/// "defaults correctly" from "ignores the field".
#[test]
fn a_carried_route_key_routes_by_itself_not_by_the_venue() {
    let json = serde_json::json!({
        "venue": "binance",
        "since": 42,
        "orders": [],
        "fills": [],
        "positions": [],
        "policy": { "default": "Synthesize", "per_kind": {} },
        "balance": null,
        "route_key": "binance-sub2"
    });
    let reports: crate::ReconcileReports = serde_json::from_value(json).unwrap();
    assert_eq!(reports.route().as_str(), "binance-sub2");
    assert_ne!(reports.route().as_str(), reports.venue, "the two facts are not the same fact");
}
