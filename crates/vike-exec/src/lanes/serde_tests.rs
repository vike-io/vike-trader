use super::*;
use vike_marketdata::BookLevel;
use vike_model::{ManagedOrder, OrderStatus};

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
        position_sides: vec![("BTCUSDT".into(), vike_model::events::PositionSide::Both)],
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
            mount_id: Some("maker-a".into()),
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
        Ingest::Command(Command::SetAccountTradingState {
            route_key: "binance#alt".into(),
            state: TradingState::Reducing,
        }),
        Ingest::Command(Command::HoldAccount { route_key: "binance#PERP".into(), hold: true }),
        Ingest::Command(Command::ResumeVenue { venue: "binance".into(), force: true }),
        Ingest::Command(Command::SetMargin(Box::new(MarginUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            im_requirement: 0.1,
        }))),
        Ingest::Command(Command::AdmitSymbol(Box::new(SymbolAdmission {
            route_key: "binance".into(),
            symbol: "ETHUSDT".into(),
            grid: vike_model::SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(0.0001),
                min_notional: Some(5.0),
                min_qty: None,
                on_demand: false,
            },
            contract_value: Some(vike_model::ContractValue::linear(10.0)),
            initial_mark: None,
            quote_ccy: None,
            quote_rate: None,
            rate_as_of_ms: None,
            im_requirement: None,
            release: false,
            margin_mode: None,
        }))),
        // ...and the same command for a symbol quoted in another currency: the rate rides it.
        Ingest::Command(Command::AdmitSymbol(Box::new(SymbolAdmission {
            route_key: "oanda".into(),
            symbol: "USD_JPY".into(),
            grid: vike_model::SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(1.0),
                ..vike_model::SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: Some(150.0),
            quote_ccy: Some("JPY".into()),
            quote_rate: Some(1.0 / 150.0),
            rate_as_of_ms: Some(1_000),
            im_requirement: Some(0.25),
            release: false,
            margin_mode: None,
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
            admitted_symbols: Some(vec!["ETHUSDT.P".into()]),
            lists_trigger_orders: false,
        }))),
        Ingest::Command(Command::ConfirmRecon(7)),
        Ingest::Command(Command::CancelReconOrder(7)),
        Ingest::Command(Command::UnlatchMount { mount_id: "sim__BTCUSDT__1m".into() }),
        Ingest::Command(Command::Shutdown),
        Ingest::Event(vike_model::events::Event::OrderSubmitted(
            vike_model::events::OrderSubmitted { client_order_id: "j1".into(), ts: 7 },
        )),
        Ingest::GatewayEvent(Box::new(GatewayEvent {
            event: vike_model::events::Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                client_order_id: "j1".into(),
                ts: 7,
            }),
            route_key: "binance#alt".into(),
            cursor: GatewayCursor { gw_epoch: 0xDEAD_BEEF_0000_0001, seq: 42, ts_ms: 7 },
        })),
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
        Ingest::ExecLink(Box::new(ExecLinkUpdate {
            route_key: "binance".into(),
            venue: "binance".into(),
            up: false,
            at_ts_ms: 12,
        })),
        Ingest::DataDelay(Box::new(DataDelayUpdate {
            venue: "ibkr".into(),
            symbol: "AAPL.SMART.USD".into(),
            stream: "quotes".into(),
            delay: DataDelay::DELAYED,
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

/// `ParamsUpdate::mount_id` is additive on BOTH wires the type rides (the journal and the control
/// frame): a payload written before the field has no key and decodes as `None` (every old journal
/// still reads), and a `None` serializes with no key at all (an unaddressed update keeps its old
/// bytes). The control-frame twin lives in `vike-tradehub-client`.
#[test]
fn a_params_update_without_a_mount_id_decodes_as_none_and_writes_no_key() {
    let json = serde_json::json!({
        "venue": "binance",
        "symbol": "BTCUSDT",
        "interval": "1m",
        "params": { "SpreadMaker": {
            "qty": 1.0, "half_spread": 0.5, "target_inventory": 0.0, "max_inventory": 1.0,
            "skew": 0.25, "fill_window_ms": 1000, "net_fill_threshold": 2.5,
            "suppress_cooldown_ms": 5000, "style": "Join", "depth_levels": 2, "tick_size": 0.5,
            "filter_own": true, "avellaneda_stoikov": null, "refresh_tolerance": null,
            "ladder": null, "reward": null, "toxicity": null
        } }
    });
    let old: ParamsUpdate = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(old.mount_id, None, "a key-less payload is the unaddressed shape");
    let back = serde_json::to_value(&old).unwrap();
    assert!(back.get("mount_id").is_none(), "a None writes no key: {back}");

    let named = ParamsUpdate { mount_id: Some("maker-b".into()), ..old };
    let mut wire = serde_json::to_value(&named).unwrap();
    assert_eq!(wire["mount_id"], "maker-b");
    let again: ParamsUpdate = serde_json::from_value(wire.clone()).unwrap();
    wire.as_object_mut().unwrap().remove("mount_id");
    assert_eq!(
        wire, back,
        "the key is the ONLY difference: an unaddressed update keeps its old bytes"
    );
    assert_eq!(again.mount_id.as_deref(), Some("maker-b"));
}

/// **`MountSpec::params` became `vike_model::params::Params` (it was `serde_json::Value`), and a
/// `Command::MountStrategy`'s JSON did not move a byte.** `PARAMS` is the exact text the OLD field
/// wrote — proven here by parsing it as a bare `serde_json::Value` (what the old field held) and
/// writing it back unchanged — holding every kind: integers (`i64` edges, negative), floats (a
/// whole-number float, a negative zero), strings with escapes, an array mixing kinds, nested tables
/// and an empty one. The new type reads the whole command with an integer staying an `Int` and a
/// float a `Float`, and writes the same bytes. The stdin control lane (`vike-tradehub`'s
/// `stdio.rs`) and the `vike_core::mount_topology` sidecar both read this form.
#[test]
fn a_mount_strategy_command_keeps_its_json_bytes_across_the_params_type_change() {
    const PARAMS: &str = concat!(
        r#"{"empty":{},"#,
        r#""levels":[1,2.5,"a",[true,false],{"k":-3}],"#,
        r#""max":9223372036854775807,"min":-9223372036854775808,"#,
        r#""neg_zero":-0.0,"nested":{"inner":{"flag":true,"k":3,"x":0.25}},"#,
        r#""rungs":2,"side":"buy","size":2.0,"step":1.5,"#,
        r#""text":"quote \" back \\ tab \t"}"#
    );
    // The OLD field's own form: a `serde_json::Value` writes PARAMS back byte for byte.
    let old: serde_json::Value = serde_json::from_str(PARAMS).unwrap();
    assert_eq!(serde_json::to_string(&old).unwrap(), PARAMS, "PARAMS is the old type's text");

    // The whole command, in the struct's own field order (what `Serialize` writes).
    let golden = format!(
        r#"{{"MountStrategy":{{"venue":"binance","symbol":"BTCUSDT","interval":"1m","controller_id":"rt-a","name":"grid","params":{PARAMS}}}}}"#
    );
    let cmd: Command = serde_json::from_str(&golden).unwrap();
    let Command::MountStrategy(spec) = &cmd else { panic!("expected MountStrategy, got {cmd:?}") };
    use vike_model::params::ParamValue;
    assert_eq!(spec.params.get("rungs"), Some(&ParamValue::Int(2)), "an integer stays an Int");
    assert_eq!(spec.params.get("size"), Some(&ParamValue::Float(2.0)), "2.0 stays a Float");
    assert_eq!(spec.params.get("max"), Some(&ParamValue::Int(i64::MAX)));
    assert_eq!(spec.params.get("min"), Some(&ParamValue::Int(i64::MIN)));
    assert_eq!(spec.params.get("neg_zero"), Some(&ParamValue::Float(-0.0)));
    assert_eq!(
        spec.params.get("nested").and_then(|n| n.get("inner")).and_then(|i| i.get("x")),
        Some(&ParamValue::Float(0.25))
    );
    assert_eq!(serde_json::to_string(&cmd).unwrap(), golden, "the new type writes the same bytes");
}

/// An ABSENT `params` is still the empty table (every pre-field and hand-written spec), and it
/// still writes `"params":{}` — the bytes the old `default_mount_params` produced.
#[test]
fn a_mount_spec_without_params_reads_the_empty_table_and_writes_an_empty_object() {
    let spec: MountSpec =
        serde_json::from_str(r#"{"venue":"sim","symbol":"B","interval":"1m","name":"buy_hold"}"#)
            .unwrap();
    assert!(spec.params.is_empty());
    assert_eq!(
        serde_json::to_string(&spec).unwrap(),
        r#"{"venue":"sim","symbol":"B","interval":"1m","name":"buy_hold","params":{}}"#
    );
}

/// THE SANCTIONED CHANGE: a `params` that is not a parameter table used to deserialize (the field
/// was any JSON value) and be refused later, by the daemon's `mount_factory`. It is now refused
/// when the spec is DESERIALIZED — a non-object, and the values a table cannot hold inside one.
#[test]
fn a_mount_spec_whose_params_is_not_a_parameter_table_does_not_deserialize() {
    for params in [
        "5",
        "[1]",
        "null",
        r#""text""#,
        r#"{"size":null}"#,
        r#"{"big":9223372036854775808}"#,
        r#"{"a":1,"a":2}"#,
    ] {
        let text = format!(
            r#"{{"venue":"sim","symbol":"B","interval":"1m","name":"buy_hold","params":{params}}}"#
        );
        assert!(
            serde_json::from_str::<MountSpec>(&text).is_err(),
            "`params: {params}` must be refused at deserialization"
        );
    }
}

/// A `GatewayEvent` is the same bytes as an `Event` plus its two extra keys, in a variant of its
/// own: the event itself is NOT re-spelled (a reader that wants the `Event` finds the very JSON an
/// `Ingest::Event` carries), and the cursor travels as plain numbers.
#[test]
fn a_gateway_event_carries_the_event_bytes_untouched_beside_its_cursor() {
    let event = vike_model::events::Event::OrderSubmitted(vike_model::events::OrderSubmitted {
        client_order_id: "j1".into(),
        ts: 7,
    });
    let plain = serde_json::to_string(&Ingest::Event(event.clone())).unwrap();
    let gateway = serde_json::to_string(&Ingest::GatewayEvent(Box::new(GatewayEvent {
        event,
        route_key: "binance".into(),
        cursor: GatewayCursor { gw_epoch: 9, seq: 3, ts_ms: 7 },
    })))
    .unwrap();
    let event_json = plain.strip_prefix(r#"{"Event":"#).and_then(|s| s.strip_suffix('}')).unwrap();
    assert_eq!(
        gateway,
        format!(
            r#"{{"GatewayEvent":{{"event":{event_json},"route_key":"binance","cursor":{{"gw_epoch":9,"seq":3,"ts_ms":7}}}}}}"#
        ),
        "the wire shape is the pinned one"
    );
}

/// `AdmitSymbol` is journaled (journal v19), so its bytes are pinned: the optional contract value
/// is OMITTED when absent, and the grid's `on_demand` marker is omitted while false, so the command
/// a caller builds and the row the engine stores are spelled by the same four optional numbers.
#[test]
fn an_admit_symbol_command_has_the_pinned_wire_shape() {
    let admission = |contract_value| SymbolAdmission {
        route_key: "binance".into(),
        symbol: "ETHUSDT".into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.01),
            lot_size: Some(0.001),
            ..vike_model::SymbolGrid::default()
        },
        contract_value,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
        release: false,
        margin_mode: None,
    };
    assert_eq!(
        serde_json::to_string(&Command::AdmitSymbol(Box::new(admission(None)))).unwrap(),
        concat!(
            r#"{"AdmitSymbol":{"route_key":"binance","symbol":"ETHUSDT","#,
            r#""grid":{"tick_size":0.01,"lot_size":0.001}}}"#
        ),
        "a symbol quoted in the account's own currency writes none of the currency keys"
    );
    // journal v20: a symbol quoted in another currency carries it and its rate, in this order
    let quoted = SymbolAdmission {
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(0.0075),
        rate_as_of_ms: Some(1_000),
        ..admission(None)
    };
    assert_eq!(
        serde_json::to_string(&Command::AdmitSymbol(Box::new(quoted.clone()))).unwrap(),
        concat!(
            r#"{"AdmitSymbol":{"route_key":"binance","symbol":"ETHUSDT","#,
            r#""grid":{"tick_size":0.01,"lot_size":0.001},"#,
            r#""quote_ccy":"JPY","quote_rate":0.0075,"rate_as_of_ms":1000}}"#
        )
    );
    let back: Command = serde_json::from_str(
        &serde_json::to_string(&Command::AdmitSymbol(Box::new(quoted.clone()))).unwrap(),
    )
    .unwrap();
    assert!(matches!(back, Command::AdmitSymbol(a) if *a == quoted));
    // journal v20: the symbol's own margin rate (1/leverage), after the currency keys
    let margined = SymbolAdmission { im_requirement: Some(0.25), ..admission(None) };
    assert_eq!(
        serde_json::to_string(&Command::AdmitSymbol(Box::new(margined))).unwrap(),
        concat!(
            r#"{"AdmitSymbol":{"route_key":"binance","symbol":"ETHUSDT","#,
            r#""grid":{"tick_size":0.01,"lot_size":0.001},"im_requirement":0.25}}"#
        )
    );
    // additive (decision 0218): the margin mode the symbol opens positions in, written last and only when stated
    let isolated = SymbolAdmission {
        margin_mode: Some(vike_model::MarginMode::Isolated),
        im_requirement: Some(0.25),
        ..admission(None)
    };
    let wire = serde_json::to_string(&Command::AdmitSymbol(Box::new(isolated.clone()))).unwrap();
    assert_eq!(
        wire,
        concat!(
            r#"{"AdmitSymbol":{"route_key":"binance","symbol":"ETHUSDT","#,
            r#""grid":{"tick_size":0.01,"lot_size":0.001},"#,
            r#""im_requirement":0.25,"margin_mode":"Isolated"}}"#
        )
    );
    let back: Command = serde_json::from_str(&wire).unwrap();
    assert!(matches!(back, Command::AdmitSymbol(a) if *a == isolated));
    // a pre-v20 command (no currency keys) reads back as the account's own currency
    let old: Command = serde_json::from_str(concat!(
        r#"{"AdmitSymbol":{"route_key":"binance","symbol":"ETHUSDT","#,
        r#""grid":{"tick_size":0.01,"lot_size":0.001}}}"#
    ))
    .unwrap();
    assert!(matches!(old, Command::AdmitSymbol(a) if *a == admission(None)));
    let ten = Some(vike_model::ContractValue::linear(10.0));
    assert_eq!(
        serde_json::to_string(&Command::AdmitSymbol(Box::new(admission(ten)))).unwrap(),
        concat!(
            r#"{"AdmitSymbol":{"route_key":"binance","symbol":"ETHUSDT","#,
            r#""grid":{"tick_size":0.01,"lot_size":0.001},"#,
            r#""contract_value":{"kind":"linear","per_unit":10.0}}}"#
        )
    );
}

/// An account change carries a live engine, not data (decision 0169): it is never journaled, and the
/// serializer must REFUSE it rather than write a record nothing could read back.
#[test]
fn an_account_change_cannot_be_serialized() {
    let msg =
        Ingest::Accounts(Box::new(AccountChange::AddEngine { seed: 1.0, engine: Box::new(0_u8) }));
    assert!(serde_json::to_string(&msg).is_err());
    assert!(msg.venue_event().is_none(), "it is not a venue event");
    // The stop (slice S2) carries no engine but rides the same skipped variant: refused the same way.
    let stop = Ingest::Accounts(Box::new(AccountChange::StopEngine {
        not_running: crate::NotRunningAccount {
            venue: "binance".to_string(),
            account: None,
            route_key: "binance".to_string(),
            tier: None,
            reason: "account inactive".to_string(),
        },
    }));
    assert!(serde_json::to_string(&stop).is_err());
    assert!(stop.venue_event().is_none());
}

/// The detach half of a stop carries only a key, and is refused by the serializer like every account change.
#[test]
fn a_detach_change_cannot_be_serialized_either() {
    let msg = Ingest::Accounts(Box::new(AccountChange::DetachEngine {
        route_key: "binance#ALT".to_string(),
    }));
    assert!(serde_json::to_string(&msg).is_err());
    assert!(msg.venue_event().is_none());
}

/// `ReconcileReports::admitted_symbols` is additive on the journal's wire: a pass journaled before the
/// field has no key and replays as `None` (the restore judges the engine's scope, as it did live),
/// a `None` writes no key (every box whose client keeps no list keeps its old bytes), and a list
/// round-trips, so a replayed pass takes the same restore verdicts the live one did.
#[test]
fn a_pass_without_admitted_symbols_replays_as_the_engine_scope_and_a_list_round_trips() {
    let old = serde_json::json!({
        "venue": "binance",
        "since": 42,
        "orders": [],
        "fills": [],
        "positions": [],
        "policy": { "default": "Synthesize", "per_kind": {} },
        "balance": null
    });
    let reports: crate::ReconcileReports = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(reports.admitted_symbols, None, "an absent key is the engine's scope");
    let back = serde_json::to_value(&reports).unwrap();
    assert!(back.get("admitted_symbols").is_none(), "a None writes no key: {back}");

    let told =
        crate::ReconcileReports { admitted_symbols: Some(vec!["ETHUSDT.P".into()]), ..reports };
    let wire = serde_json::to_value(&told).unwrap();
    assert_eq!(wire["admitted_symbols"], serde_json::json!(["ETHUSDT.P"]));
    let again: crate::ReconcileReports = serde_json::from_value(wire).unwrap();
    assert_eq!(again.admitted_symbols, Some(vec!["ETHUSDT.P".to_string()]));

    let none_told = crate::ReconcileReports { admitted_symbols: Some(Vec::new()), ..again };
    let wire = serde_json::to_value(&none_told).unwrap();
    assert_eq!(
        wire["admitted_symbols"],
        serde_json::json!([]),
        "an EMPTY list is a claim, not an absence"
    );
}

/// `ReconcileReports::lists_trigger_orders` is additive on the journal's wire, the kind twin of the
/// symbol claim above: a pass journaled before the field has no key and replays as `false` (the
/// restore HOLDS an absent stop, the safe side), a `false` writes no key (every box whose client
/// makes no claim keeps its old bytes), and a `true` round-trips so a replayed pass takes the
/// verdicts the live one did.
#[test]
fn a_pass_without_the_trigger_claim_replays_as_no_claim_and_a_claim_round_trips() {
    let old = serde_json::json!({
        "venue": "binance",
        "since": 42,
        "orders": [],
        "fills": [],
        "positions": [],
        "policy": { "default": "Synthesize", "per_kind": {} },
        "balance": null
    });
    let reports: crate::ReconcileReports = serde_json::from_value(old).unwrap();
    assert!(!reports.lists_trigger_orders, "an absent key is no claim");
    let back = serde_json::to_value(&reports).unwrap();
    assert!(back.get("lists_trigger_orders").is_none(), "a false writes no key: {back}");

    let claimed = crate::ReconcileReports { lists_trigger_orders: true, ..reports };
    let wire = serde_json::to_value(&claimed).unwrap();
    assert_eq!(wire["lists_trigger_orders"], serde_json::json!(true));
    let again: crate::ReconcileReports = serde_json::from_value(wire).unwrap();
    assert!(again.lists_trigger_orders);
}
