use super::*;

fn full_snapshot() -> WireSnapshot {
    WireSnapshot {
        seq: 42,
        accounts_epoch: 0,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trading_state: WireTradingState::Reducing,
        balance: 10_000.0,
        equity_total: 12_345.67,
        venues: vec![WireVenueBlock {
            venue: "binance".into(),
            balance: 10_000.0,
            realized_pnl: 250.5,
            fees_paid: 3.25,
            funding_paid: -1.5,
            equity: 12_345.67,
            unrealized: 2_345.67,
            missing_prices: 1,
            margin_used: 500.0,
            free_bp: 11_845.67,
            trading_state: WireTradingState::Reducing,
            account: None,
            route_key: "binance".into(),
            positions: vec![WirePositionView {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: "BOTH".into(),
                size: 0.5,
                avg_px: 60_000.0,
                unrealized: 2_345.67,
                leverage: 5.0,
                liq_price: 48_000.0,
            }],
        }],
        orders: vec![WireOrderView {
            client_order_id: "c-1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: "limit".into(),
            price: Some(59_000.0),
            trigger_price: None,
            status: "Accepted".into(),
            venue_order_id: Some("v-9".into()),
            filled_qty: 0.0,
            avg_fill_px: 0.0,
        }],
        positions: vec![WirePositionView {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: "BOTH".into(),
            size: 0.5,
            avg_px: 60_000.0,
            unrealized: 2_345.67,
            leverage: 5.0,
            liq_price: 48_000.0,
        }],
        held_exits: vec![WireHeldOrderView {
            client_order_id: "c-2".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            order_type: "stop".into(),
            price: None,
            trigger_price: Some(55_000.0),
            parent_order_id: Some("c-1".into()),
        }],
        recent_events: vec!["OrderAccepted c-1".into(), "OrderSubmitted c-1".into()],
        fault: None,
        bars: vec![WireBarSeries {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            closed: vec![
                WireBar { ts: 1_000, o: 60_000.0, h: 60_100.0, l: 59_900.0, c: 60_050.0, v: 12.5 },
                WireBar { ts: 61_000, o: 60_050.0, h: 60_200.0, l: 60_000.0, c: 60_150.0, v: 8.0 },
            ],
            forming: Some(WireBar {
                ts: 121_000,
                o: 60_150.0,
                h: 60_180.0,
                l: 60_120.0,
                c: 60_170.0,
                v: 3.2,
            }),
        }],
        identity: None,
    }
}

/// A fully-populated snapshot round-trips through `serde_json` byte-stably (every Some arm, a
/// non-empty venue/order/position/held/events set).
#[test]
fn wire_snapshot_round_trips_fully_populated() {
    let snap = full_snapshot();
    let js = serde_json::to_string(&snap).unwrap();
    let back: WireSnapshot = serde_json::from_str(&js).unwrap();
    assert_eq!(snap, back);
}

/// Every `WireCommand` variant round-trips through JSON.
#[test]
fn wire_command_round_trips_each_variant() {
    let cmds = vec![
        WireCommand::Submit(WireOrderRequest {
            client_order_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            price: None,
            trigger_price: None,
            reduce_only: false,
            account: None,
        }),
        WireCommand::Cancel("c-1".into()),
        WireCommand::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None },
        WireCommand::MassCancel { venue: None, symbol: None, account: None },
        WireCommand::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
        WireCommand::MarketExit { venue: Some("binance".into()), account: None },
        WireCommand::SetTradingState(WireTradingState::Halted),
        // The B4 strategy write verb: the params payload is the CORE's own externally-tagged
        // StrategyParams JSON, carried opaquely (delegated, not mirrored — see the variant doc).
        WireCommand::UpdateParams {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            params: serde_json::json!({"SpreadMaker": {"qty": 2.0, "half_spread": 1.0}}),
        },
        // The B5 mount verbs: both source arms (registry name / rhai path) + the unmount.
        WireCommand::MountStrategy {
            venue: "binance".into(),
            account: Some("ALT".into()),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: Some("grid-a".into()),
            name: Some("grid".into()),
            rhai: None,
            params: serde_json::json!({"qty": 1.0}),
        },
        WireCommand::MountStrategy {
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: None,
            name: None,
            rhai: Some("strategies/breaker.rhai".into()),
            params: serde_json::json!({}),
        },
        WireCommand::UnmountStrategy { controller_id: "grid-a".into() },
        // The REQ-7 settings write: a confirm-less edit, and the shape an OLDER client still
        // sends — a file NAME and a typed confirm, both ignored by the node since
        // `docs/decisions/0086` (point 7 for the confirm). Both must keep round-tripping until
        // step 2 deletes the two fields.
        WireCommand::SetSetting {
            file: "config.toml".into(),
            key: "config.tradehub_addr".into(),
            value: "127.0.0.1:7879".into(),
            confirm: None,
        },
        WireCommand::SetSetting {
            file: "policy.toml".into(),
            key: "policy.max_notional_per_order".into(),
            value: "250".into(),
            confirm: Some("policy.max_notional_per_order".into()),
        },
    ];
    for c in cmds {
        let js = serde_json::to_string(&c).unwrap();
        let back: WireCommand = serde_json::from_str(&js).unwrap();
        assert_eq!(c, back);
    }
}

/// **A settings write with NO `file` and NO `confirm` decodes** — step 1 of taking the file era out
/// of the settings wire. A write is one row in the node's settings database, named by its KEY
/// alone; the node consults neither field (`docs/decisions/0086` points 6 and 7), so a client that
/// stops sending them must still be understood. Deleting the two fields outright is step 2, after a
/// release carrying this tolerance is deployed.
///
/// ⚠ **The second half is the compatibility pin, and it is not decoration.** The released daemon
/// (v0.1.35) still REQUIRES `file` on decode, so what a client SENDS must keep carrying one until
/// the tolerant daemon is what runs — a tolerance on the reading side changes nothing about what
/// the writing side emits, and this is the assertion that says so.
#[test]
fn a_settings_write_with_no_file_and_no_confirm_decodes() {
    let bare = serde_json::json!({
        "SetSetting": { "key": "policy.max_notional_per_order", "value": "250" }
    });
    let cmd: WireCommand = serde_json::from_value(bare)
        .expect("a SetSetting naming only its key and value must decode");
    let WireCommand::SetSetting { file, key, value, confirm } = cmd else {
        panic!("decoded as a different variant")
    };
    assert_eq!((key.as_str(), value.as_str()), ("policy.max_notional_per_order", "250"));
    assert!(file.is_empty(), "an absent file decodes to nothing: {file:?}");
    assert_eq!(confirm, None, "an absent confirm decodes to none");

    let sent = serde_json::to_value(WireCommand::SetSetting {
        file: "policy".into(),
        key: "policy.max_notional_per_order".into(),
        value: "250".into(),
        confirm: None,
    })
    .unwrap();
    assert_eq!(
        sent["SetSetting"]["file"],
        serde_json::json!("policy"),
        "a client still SENDS `file` — the released daemon requires it: {sent}"
    );
}

/// The B4 status payload round-trips fully populated — identity + effective params + a mount
/// row — and an EMPTY mounts `Vec` (a node that mounted nothing) survives too.
#[test]
fn wire_strategy_status_round_trips() {
    let full = WireStrategyStatus {
        identity: WireNodeIdentity {
            name: "the build runner".into(),
            strategy: "spread_maker".into(),
            params: "qty=1 half_spread=0.5".into(),
            live: true,
            build: "vike 0.1.0 (abc1234)".into(),
            advertise_addr: "203.0.113.7:7879".into(),
        },
        effective_params: "qty=1 half_spread=0.5".into(),
        mounts: vec![WireMountRow {
            strategy: "spread_maker".into(),
            params: "qty=1 half_spread=0.5".into(),
            live: true,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            // A REAL `vike_model::StrategyParams` JSON shape (externally tagged, one variant
            // key) rather than a toy object — this field's whole contract is that the bytes
            // round-trip unexamined, so the fixture must be the shape the core actually emits.
            typed_params: Some(serde_json::json!({
                "SpreadMaker": { "qty": 1.0, "half_spread": 0.5, "avellaneda_stoikov": null }
            })),
            asset_class: Some("CryptoPerp".into()),
        }],
    };
    for status in [full.clone(), WireStrategyStatus { mounts: Vec::new(), ..full }] {
        let js = serde_json::to_string(&status).unwrap();
        let back: WireStrategyStatus = serde_json::from_str(&js).unwrap();
        assert_eq!(status, back);
    }
}

/// The additive-field contract for the params read (`a_frame_without_identity_parses_as_none`'s
/// shape, one struct over): a `WireMountRow` from a node that predates
/// [`crate::proto::FEATURE_STRATEGY_PARAMS`] carries none of the four new keys, and must still
/// parse — as an EMPTY addressing key and `None` params, never an error. That is what lets them
/// ride without a `NODE_PROTO_VERSION` bump (the version is folded into the auth MAC).
///
/// ⚠ And it is exactly why the capability exists: the value this test asserts is
/// INDISTINGUISHABLE from an honest "this mount publishes no typed params" on a NEW node. A
/// client tells the two apart by `Welcome.features`, never by looking at these fields — the
/// negotiated [`crate::remote_handle::strategy_params`] read is where that check lives.
#[test]
fn a_mount_row_without_the_params_fields_parses_as_empty() {
    let js = r#"{"strategy":"spread_maker","params":"qty=1 half_spread=0.5","live":true}"#;
    let row: WireMountRow = serde_json::from_str(js).expect("an old node's row still parses");
    assert_eq!(row.strategy, "spread_maker");
    assert!(row.live, "the pre-existing fields are untouched");
    assert_eq!((row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()), ("", "", ""));
    assert_eq!(row.typed_params, None);
}

/// `WireOrderRequest`'s `#[serde(default)]` fields let a minimal JSON (only the required fields)
/// deserialize — the thin-client convenience the real `OrderRequest` also affords.
#[test]
fn wire_order_request_accepts_minimal_json() {
    let js = r#"{"venue":"sim","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"market"}"#;
    let req: WireOrderRequest = serde_json::from_str(js).unwrap();
    assert_eq!(req.client_order_id, "");
    assert_eq!(req.price, None);
    assert!(!req.reduce_only);
}

/// The identity block round-trips when present (split-plane B3): a GUI holding several
/// backends labels them from this, and paper-vs-live must survive the wire exactly.
#[test]
fn identity_roundtrips_when_present() {
    let mut s = full_snapshot();
    s.identity = Some(WireNodeIdentity {
        name: "the build runner".into(),
        strategy: "spread_maker".into(),
        params: "qty=1 half_spread=0.5".into(),
        live: true,
        build: "vike 0.1.0 (abc1234)".into(),
        advertise_addr: "203.0.113.7:7879".into(),
    });
    let json = serde_json::to_string(&s).unwrap();
    let back: WireSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.identity.as_ref().unwrap().name, "the build runner");
    assert!(back.identity.as_ref().unwrap().live);
}

/// The additive-field contract (the `bars` precedent): a frame from an OLDER node that
/// predates `identity` must still parse, as `None` — never an error. This is what lets the
/// field ride WITHOUT a `NODE_PROTO_VERSION` bump (the version is folded into the auth MAC,
/// so a bump would break the handshake against every running node).
#[test]
fn a_frame_without_identity_parses_as_none() {
    let mut s = full_snapshot();
    s.identity = None;
    let mut v: serde_json::Value = serde_json::to_value(&s).unwrap();
    v.as_object_mut().unwrap().remove("identity");
    let back: WireSnapshot = serde_json::from_value(v).unwrap();
    assert!(back.identity.is_none());
}

/// The same contract ONE STRUCT DOWN, for the field a client reads to say WHICH BOX the daemon
/// is on: a node old enough to send an identity block but not
/// [`WireNodeIdentity::advertise_addr`] must still parse, as EMPTY rather than as an error.
///
/// ⚠ The two failure shapes this pins are different and both matter. A parse ERROR would take
/// the whole frame down — the address field would break the snapshot, the orders and the
/// positions of every daemon that has not been redeployed. And an empty string must not be
/// read as a FACT about the daemon: it says "this node reported no address", which is exactly
/// what `vike_app_core::backend::backend_identity` renders it as.
#[test]
fn an_identity_without_the_advertise_field_parses_as_empty() {
    let js = r#"{"name":"the build runner","strategy":"spread_maker","params":"{}","live":true,
                     "build":"vike 0.1.0 (abc1234)"}"#;
    let id: WireNodeIdentity =
        serde_json::from_str(js).expect("an old node's identity block still parses");
    assert_eq!(id.name, "the build runner", "the pre-existing fields are untouched");
    assert!(id.live);
    assert_eq!(id.advertise_addr, "", "absent is EMPTY, never an error and never a default IP");
}

// -----------------------------------------------------------------------------------------
// The ADDRESS a command carries
// -----------------------------------------------------------------------------------------

fn order(venue: &str) -> WireOrderRequest {
    WireOrderRequest {
        client_order_id: "c-1".into(),
        venue: venue.into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: None,
    }
}

/// Every venue-carrying variant answers with ITS OWN venue field — not the first one in the
/// struct, not a constant, not the `symbol`. The node routes on exactly this string, so a
/// mis-wired arm here is a mis-routed order.
#[test]
fn every_venue_carrying_variant_answers_with_its_own_venue() {
    assert_eq!(WireCommand::Submit(order("binance")).addressed_venue(), Some("binance"));
    assert_eq!(
        WireCommand::Flatten { venue: "okx".into(), symbol: "SYM".into(), account: None }
            .addressed_venue(),
        Some("okx")
    );
    assert_eq!(
        WireCommand::MassCancel { venue: Some("bybit".into()), symbol: None, account: None }
            .addressed_venue(),
        Some("bybit")
    );
    assert_eq!(
        WireCommand::MarketExit { venue: Some("deribit".into()), account: None }.addressed_venue(),
        Some("deribit")
    );
    assert_eq!(
        WireCommand::UpdateParams {
            venue: "aster".into(),
            symbol: "SYM".into(),
            interval: "1m".into(),
            params: serde_json::json!({}),
        }
        .addressed_venue(),
        Some("aster")
    );
    assert_eq!(
        WireCommand::MountStrategy {
            venue: "hyperliquid".into(),
            account: None,
            symbol: "SYM".into(),
            interval: "1m".into(),
            controller_id: None,
            name: Some("buy_hold".into()),
            rhai: None,
            params: serde_json::json!({}),
        }
        .addressed_venue(),
        Some("hyperliquid")
    );
}

/// The three ADDRESS-LESS classes answer `None`, each for its own documented reason — an
/// order-scoped verb, an account-wide one, and one that is not about a book at all. The
/// UNSCOPED panic button is in here deliberately: naming the whole set IS naming the target,
/// and a routing gate that could refuse it would be a kill switch with a prerequisite.
#[test]
fn the_address_less_variants_answer_none() {
    for cmd in [
        WireCommand::Cancel("c-1".into()),
        WireCommand::Modify { client_order_id: "c-1".into(), new_qty: None, new_price: None },
        WireCommand::SetTradingState(WireTradingState::Halted),
        WireCommand::MassCancel { venue: None, symbol: None, account: None },
        WireCommand::MarketExit { venue: None, account: None },
        WireCommand::UnmountStrategy { controller_id: "m-1".into() },
        WireCommand::SetSetting {
            file: "policy.toml".into(),
            key: "policy.max_notional_per_order".into(),
            value: "250".into(),
            confirm: Some("policy.max_notional_per_order".into()),
        },
    ] {
        assert_eq!(cmd.addressed_venue(), None, "{cmd:?} addresses no venue");
    }
}

/// The field is REQUIRED on the wire and always has been: a `Submit` body with no `venue` key
/// does not decode. That is what makes the node-side routing gate a check on an existing field
/// rather than a new field — there is no "old client that sends no venue" to be compatible
/// with, and so no [`crate::proto::NODE_PROTO_VERSION`] bump anywhere in this change.
#[test]
fn a_submit_body_without_a_venue_key_does_not_decode() {
    let js = r#"{"Submit":{"client_order_id":"c-1","symbol":"SYM","side":1,"qty":1.0,
                     "order_type":"limit","price":1.0}}"#;
    assert!(
        serde_json::from_str::<WireCommand>(js).is_err(),
        "the venue is a plain String with no serde default — absent is an ERROR, never empty"
    );
}
