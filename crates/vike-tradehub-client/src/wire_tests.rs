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
            symbols: Vec::new(),
            mode: None,
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
            account: None,
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

/// A fully-populated snapshot (every `Some` arm, every list non-empty) round-trips.
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
        // The params payload is the core's own StrategyParams JSON, carried opaquely.
        WireCommand::UpdateParams {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            mount_id: None,
            params: serde_json::json!({"SpreadMaker": {"qty": 2.0, "half_spread": 1.0}}),
        },
        // Both mount source arms (registry name / rhai path), then the unmount.
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
        // A confirm-less edit, and an OLDER client's shape (a file NAME and a typed confirm, both
        // ignored since `docs/decisions/0086`): both round-trip until the fields are deleted.
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
        // The TP/SL bracket: `vike_model::BracketSpec` field for field, a MARKET entry here.
        WireCommand::Bracket(WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            entry_price: None,
            stop_loss: 61_000.0,
            take_profit: 58_000.0,
        }),
    ];
    for c in cmds {
        let js = serde_json::to_string(&c).unwrap();
        let back: WireCommand = serde_json::from_str(&js).unwrap();
        assert_eq!(c, back);
    }
}

/// **A settings write with NO `file` and NO `confirm` decodes**: the node names a row by its KEY
/// alone (`docs/decisions/0086` points 6 and 7).
///
/// ⚠ The second half is the compatibility pin: the released v0.1.35 daemon still REQUIRES `file`
/// on decode, so a client keeps SENDING one; reader tolerance changes nothing about the writer.
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

/// The status payload round-trips fully populated, and with an EMPTY mounts `Vec`.
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
            mount_id: "maker_a".into(),
            // A REAL `vike_model::StrategyParams` shape, not a toy object: the contract is that
            // the core's own bytes round-trip unexamined.
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

/// The additive-field contract: a `WireMountRow` from a node that predates
/// [`crate::proto::FEATURE_STRATEGY_PARAMS`] parses with an EMPTY addressing key and `None` params.
/// ⚠ That is indistinguishable from a new node's "no typed params", which is why the capability
/// exists; [`crate::remote_handle::strategy_params`] checks it.
#[test]
fn a_mount_row_without_the_params_fields_parses_as_empty() {
    let js = r#"{"strategy":"spread_maker","params":"qty=1 half_spread=0.5","live":true}"#;
    let row: WireMountRow = serde_json::from_str(js).expect("an old node's row still parses");
    assert_eq!(row.strategy, "spread_maker");
    assert!(row.live, "the pre-existing fields are untouched");
    assert_eq!((row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()), ("", "", ""));
    assert_eq!(row.typed_params, None);
}

/// A minimal `WireOrderRequest` (required fields only) deserializes.
#[test]
fn wire_order_request_accepts_minimal_json() {
    let js = r#"{"venue":"sim","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"market"}"#;
    let req: WireOrderRequest = serde_json::from_str(js).unwrap();
    assert_eq!(req.client_order_id, "");
    assert_eq!(req.price, None);
    assert!(!req.reduce_only);
}

/// The identity block round-trips when present; paper-vs-live survives exactly.
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

/// The additive-field contract: a frame from a node that predates `identity` parses as `None`.
#[test]
fn a_frame_without_identity_parses_as_none() {
    let mut s = full_snapshot();
    s.identity = None;
    let mut v: serde_json::Value = serde_json::to_value(&s).unwrap();
    v.as_object_mut().unwrap().remove("identity");
    let back: WireSnapshot = serde_json::from_value(v).unwrap();
    assert!(back.identity.is_none());
}

/// The same contract for the ACCOUNT-SET digest: absent parses as `0`. That `0` is the stamp an
/// MCP preview takes against an older node (`crates/vike-cli/src/cmd/mcp/venue_gate.rs`'s
/// `Server::node_accounts_epoch`), and two zeroes compare EQUAL at confirm, which keeps the
/// account-set refusal off every older node.
#[test]
fn a_frame_without_accounts_epoch_parses_as_zero() {
    let mut s = full_snapshot();
    // Non-zero, so the `0` read back below is the DEFAULT rather than a value that survived.
    s.accounts_epoch = 0xDEAD_BEEF;
    let mut v: serde_json::Value = serde_json::to_value(&s).unwrap();
    assert!(
        v.as_object_mut().unwrap().remove("accounts_epoch").is_some(),
        "the serialised frame carried the field this test drops"
    );
    let back: WireSnapshot =
        serde_json::from_value(v).expect("a frame from a node predating the field still parses");
    assert_eq!(back.accounts_epoch, 0, "an absent epoch reads as 0, the pre-field answer");
}

/// The same contract one struct down: an identity block without
/// [`WireNodeIdentity::advertise_addr`] parses with it EMPTY — not an error (which would drop the
/// whole frame) and not a fact about the daemon ("reported no address").
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

/// Every venue-carrying variant answers with ITS OWN venue field: the node routes on exactly this
/// string, so a mis-wired arm is a mis-routed order.
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
            mount_id: None,
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
    assert_eq!(
        WireCommand::Bracket(WireBracketSpec {
            venue: "bybit".into(),
            symbol: "SYM".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(1.0),
            stop_loss: 0.9,
            take_profit: 1.1,
        })
        .addressed_venue(),
        Some("bybit")
    );
}

/// The three ADDRESS-LESS classes answer `None`. The UNSCOPED panic button is here deliberately:
/// a routing gate that could refuse it would be a kill switch with a prerequisite.
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

/// The venue is REQUIRED and always was: a `Submit` body without it does not decode, so the
/// node-side routing gate checks an existing field (no [`crate::proto::NODE_PROTO_VERSION`] bump).
#[test]
fn a_submit_body_without_a_venue_key_does_not_decode() {
    let js = r#"{"Submit":{"client_order_id":"c-1","symbol":"SYM","side":1,"qty":1.0,
                     "order_type":"limit","price":1.0}}"#;
    assert!(
        serde_json::from_str::<WireCommand>(js).is_err(),
        "the venue is a plain String with no serde default — absent is an ERROR, never empty"
    );
}

// -----------------------------------------------------------------------------------------
// What an account's engine trades, what stands behind it, and which account an order rests in:
// all three are additive fields.
// -----------------------------------------------------------------------------------------

/// A block from a node that predates `symbols` and `mode` decodes as "not said".
#[test]
fn a_venue_block_without_symbols_or_mode_still_decodes() {
    let json = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_bp":0.0,"trading_state":"Active","positions":[]}"#;
    let v: WireVenueBlock = serde_json::from_str(json).expect("an older node's block decodes");
    assert!(v.symbols.is_empty());
    assert_eq!(v.mode, None);
}

#[test]
fn symbols_and_mode_round_trip() {
    let mut v = full_snapshot().venues.remove(0);
    v.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    v.mode = Some(WireEngineMode::Live);
    let json = serde_json::to_string(&v).expect("serializes");
    assert!(json.contains("\"mode\":\"live\""), "lowercase on the wire: {json}");
    let back: WireVenueBlock = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, v);
}

/// **All three mode words are pinned**: a GUI shows this word before an order, so a drifted
/// spelling would be a real-money label that lies.
#[test]
fn every_engine_mode_has_its_lowercase_wire_spelling() {
    for (mode, word) in [
        (WireEngineMode::Paper, "\"paper\""),
        (WireEngineMode::Demo, "\"demo\""),
        (WireEngineMode::Live, "\"live\""),
    ] {
        assert_eq!(serde_json::to_string(&mode).expect("serializes"), word);
        assert_eq!(serde_json::from_str::<WireEngineMode>(word).expect("decodes"), mode);
    }
}

/// **A mode value this reader does not know is "not said", never a decode failure** (the observe
/// loop ends the link on an undecodable frame). Every shape a newer node could send must read as
/// `None` with the REST of the block untouched.
#[test]
fn an_unknown_mode_value_reads_as_not_said_and_the_rest_of_the_block_survives() {
    let mut block = full_snapshot().venues.remove(0);
    block.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    block.account = Some("SUB".into());
    block.mode = Some(WireEngineMode::Live);
    let mut expected = block.clone();
    expected.mode = None;
    let mut frame = serde_json::to_value(&block).expect("serializes");
    for unknown in [
        serde_json::json!("quantum"),
        serde_json::json!(7),
        serde_json::json!(true),
        serde_json::json!(["live"]),
        serde_json::json!({"mode": "live"}),
        serde_json::json!("LIVE"),
        serde_json::json!(""),
    ] {
        frame["mode"] = unknown.clone();
        let back: WireVenueBlock = serde_json::from_value(frame.clone())
            .unwrap_or_else(|e| panic!("`\"mode\": {unknown}` must decode as not said: {e}"));
        assert_eq!(
            back, expected,
            "`\"mode\": {unknown}` reads as not said and moves nothing else"
        );
    }
}

/// A known word is its variant; an absent key and an explicit `null` are "not said".
#[test]
fn the_known_mode_words_and_an_absent_key_decode_as_they_always_did() {
    let mut frame = serde_json::to_value(full_snapshot().venues.remove(0)).expect("serializes");
    for (word, mode) in [
        ("paper", WireEngineMode::Paper),
        ("demo", WireEngineMode::Demo),
        ("live", WireEngineMode::Live),
    ] {
        frame["mode"] = serde_json::json!(word);
        let back: WireVenueBlock = serde_json::from_value(frame.clone()).expect("decodes");
        assert_eq!(back.mode, Some(mode), "`{word}`");
    }
    frame["mode"] = serde_json::Value::Null;
    let back: WireVenueBlock = serde_json::from_value(frame.clone()).expect("decodes");
    assert_eq!(back.mode, None, "an explicit null");
    frame.as_object_mut().expect("a block is an object").remove("mode");
    let back: WireVenueBlock = serde_json::from_value(frame).expect("decodes");
    assert_eq!(back.mode, None, "an absent key");
}

/// …and through the frame the observe loop decodes: an unknown word beside a known one; the frame
/// decodes, the known word is kept, the unknown one is "not said".
#[test]
fn a_snapshot_frame_with_one_unknown_mode_still_decodes_and_keeps_the_known_one() {
    use crate::proto::Response;

    let mut snap = full_snapshot();
    let mut second = snap.venues[0].clone();
    second.venue = "okx".into();
    second.route_key = "okx".into();
    snap.venues[0].mode = Some(WireEngineMode::Demo);
    second.mode = Some(WireEngineMode::Live);
    snap.venues.push(second);

    let mut frame =
        serde_json::to_value(Response::SnapshotFrame(Box::new(snap.clone()))).expect("serializes");
    frame["SnapshotFrame"]["venues"][0]["mode"] = serde_json::json!("quantum");

    let Response::SnapshotFrame(back) = serde_json::from_value::<Response>(frame)
        .expect("an unknown mode word must not fail the frame")
    else {
        panic!("a snapshot frame decodes as a snapshot frame");
    };
    assert_eq!(back.venues[0].mode, None, "the word this reader does not know is not said");
    assert_eq!(back.venues[1].mode, Some(WireEngineMode::Live), "the known word is kept");
    // Nothing else in the frame moved.
    snap.venues[0].mode = None;
    assert_eq!(*back, snap);
}

/// A block that says nothing about its engine puts NEITHER key on the wire. Asserted on the TEXT:
/// a round trip would pass with `"symbols":[]` and `"mode":null`, keys an older client never saw.
#[test]
fn a_venue_block_that_says_nothing_about_its_engine_serialises_without_either_key() {
    let mut v = full_snapshot().venues.remove(0);
    // Set outright rather than left to the fixture: the fixture's values are incidental.
    v.symbols = Vec::new();
    v.mode = None;
    let json = serde_json::to_string(&v).expect("serializes");
    assert!(!json.contains("\"symbols\""), "{json}");
    assert!(!json.contains("\"mode\""), "{json}");
}

/// An order from a node that predates `WireOrderView::account` decodes as "named no account".
#[test]
fn an_order_without_an_account_key_still_decodes() {
    let json = r#"{"client_order_id":"c-1","venue":"binance","symbol":"BTCUSDT","side":1,
        "qty":0.5,"order_type":"limit","price":59000.0,"trigger_price":null,"status":"Accepted",
        "venue_order_id":null,"filled_qty":0.0,"avg_fill_px":0.0}"#;
    let o: WireOrderView = serde_json::from_str(json).expect("an older node's order decodes");
    assert_eq!(o.account, None);
}

/// A default-account order puts no `account` key on the wire; a labelled one carries its label.
#[test]
fn an_order_names_its_account_only_when_it_has_one() {
    let mut o = full_snapshot().orders.remove(0);
    o.account = None;
    let json = serde_json::to_string(&o).expect("serializes");
    assert!(!json.contains("\"account\""), "{json}");
    o.account = Some("SUB".into());
    let json = serde_json::to_string(&o).expect("serializes");
    assert!(json.contains("\"account\":\"SUB\""), "{json}");
    let back: WireOrderView = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, o);
}

/// ⚠ **A bracket frame that names an ACCOUNT does not decode** (`deny_unknown_fields`): never a
/// decode that drops the key and puts the bracket on the default book.
#[test]
fn a_bracket_frame_naming_an_account_does_not_decode() {
    let js = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,
                 "entry_price":null,"stop_loss":95.0,"take_profit":110.0,"account":"ALT"}}"#;
    assert!(
        serde_json::from_str::<WireCommand>(js).is_err(),
        "an unknown key on a bracket is an error, never silently dropped"
    );
    // POSITIVE CONTROL: the same frame minus `account` decodes (explicit `null` = MARKET entry).
    let js = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,
                 "entry_price":null,"stop_loss":95.0,"take_profit":110.0}}"#;
    match serde_json::from_str::<WireCommand>(js).expect("the account-less frame decodes") {
        WireCommand::Bracket(b) => assert_eq!(b.entry_price, None, "an explicit null is market"),
        other => panic!("expected a Bracket, got {other:?}"),
    }
}

/// ⚠ **DECLARED, pinned: a NON-FINITE entry price round-trips to a MARKET entry** (`serde_json`
/// writes NaN/inf as `null`); the guard is the SENDER's (`WireBracketSpec::entry_price`'s doc).
#[test]
fn a_non_finite_entry_price_round_trips_to_a_market_entry() {
    for px in [f64::NAN, f64::INFINITY] {
        let sent = WireCommand::Bracket(WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(px),
            stop_loss: 95.0,
            take_profit: 110.0,
        });
        let bytes = serde_json::to_vec(&sent).expect("serializes");
        match serde_json::from_slice::<WireCommand>(&bytes).expect("decodes") {
            WireCommand::Bracket(b) => assert_eq!(b.entry_price, None, "{px} arrives as market"),
            other => panic!("expected a Bracket, got {other:?}"),
        }
    }
}

/// ⚠ **The `entry_price` KEY is required; only its value may be `null`**: a forgotten key must not
/// turn a meant limit into a MARKET entry (`vike_model::build_bracket` reads `is_some()`).
#[test]
fn a_bracket_body_without_an_entry_price_key_does_not_decode() {
    let js = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,
                 "stop_loss":95.0,"take_profit":110.0}}"#;
    assert!(
        serde_json::from_str::<WireCommand>(js).is_err(),
        "a missing entry_price key must not decode as a market entry"
    );
}

/// The venue is REQUIRED, as on a `Submit`: the node routes by it and nothing else.
#[test]
fn a_bracket_body_without_a_venue_key_does_not_decode() {
    let js = r#"{"Bracket":{"symbol":"BTCUSDT","side":1,"qty":1.0,
                 "entry_price":null,"stop_loss":95.0,"take_profit":110.0}}"#;
    assert!(serde_json::from_str::<WireCommand>(js).is_err());
}

/// A MARKET entry is written as an explicit `"entry_price": null`, never a missing key.
#[test]
fn a_market_entry_bracket_writes_its_entry_price_as_null() {
    let v = serde_json::to_value(WireCommand::Bracket(WireBracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 95.0,
        take_profit: 110.0,
    }))
    .unwrap();
    let body = v["Bracket"].as_object().expect("an externally tagged object");
    assert_eq!(body.get("entry_price"), Some(&serde_json::Value::Null), "{v}");
    assert!(!body.contains_key("account"), "a bracket carries no account: {v}");
}

/// `mount_id` is additive on both shapes: an OLD client's `UpdateParams` decodes as unaddressed and
/// re-encodes byte-identically, and an OLD node's status row decodes with an empty id.
#[test]
fn mount_id_is_additive_on_the_update_and_on_the_status_row() {
    let old_frame = serde_json::json!({ "UpdateParams": {
        "venue": "binance", "symbol": "BTCUSDT", "interval": "1m",
        "params": { "SpreadMaker": { "qty": 1.0 } }
    } });
    let decoded: WireCommand = serde_json::from_value(old_frame.clone()).unwrap();
    let WireCommand::UpdateParams { mount_id, .. } = &decoded else {
        panic!("decodes as UpdateParams");
    };
    assert_eq!(*mount_id, None, "a key-less frame is the unaddressed shape");
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        old_frame,
        "…and re-encodes to the same bytes"
    );

    let addressed = WireCommand::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: Some("maker_b".into()),
        params: serde_json::json!({}),
    };
    let wire = serde_json::to_value(&addressed).unwrap();
    assert_eq!(wire["UpdateParams"]["mount_id"], "maker_b");
    assert_eq!(serde_json::from_value::<WireCommand>(wire).unwrap(), addressed);

    let old_row = serde_json::json!({
        "strategy": "spread_maker", "params": "qty=1", "live": false,
        "venue": "binance", "symbol": "BTCUSDT", "interval": "1m"
    });
    let row: WireMountRow = serde_json::from_value(old_row).unwrap();
    assert_eq!(row.mount_id, "", "a row from a node before the capability reads an EMPTY id");
}
