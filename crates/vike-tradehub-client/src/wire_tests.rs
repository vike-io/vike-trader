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
            tier: None,
            contract_values: Vec::new(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: String::new(),
            ledger_ccy: String::new(),
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
            data_delay: None,
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
            reason: None,
            age_ms: Some(61_000),
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
        not_running: Vec::new(),
        key_changed: vec!["bybit#ALT".to_string()],
        balance_held: vec![WireBalanceHeld {
            route_key: "oanda".into(),
            reason: "assumed USD".into(),
        }],
        venue_link_down: vec![WireVenueLinkDown {
            route_key: "dukascopy".into(),
            why: "JForex session disconnected".into(),
            failed_restarts: 2,
            retry_in_ms: 20_000,
        }],
        recon: vec![WireReconAlert {
            id: 9,
            account: "bybit".into(),
            kind: "OrphanLocalPosition".into(),
            detail: "local bybit ETHUSDT BOTH position 8.34 has NO venue position row".into(),
            proposed_event_count: 0,
        }],
        uptime_ms: Some(86_400_000),
        halt_sentinel: Some(true),
        primary_ccy: String::new(),
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
        WireCommand::ReconConfirm { id: 9 },
        WireCommand::ReconCancel { id: 9 },
        WireCommand::ResumeVenue { venue: "bybit".into(), force: false },
        WireCommand::ResumeVenue { venue: "bybit".into(), force: true },
        WireCommand::SetAccountTradingState {
            venue: "binance".into(),
            account: "ALT".into(),
            state: WireTradingState::Reducing,
        },
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
            account: None,
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
        identity: WireHubIdentity {
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
    s.identity = Some(WireHubIdentity {
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
/// [`WireHubIdentity::advertise_addr`] parses with it EMPTY — not an error (which would drop the
/// whole frame) and not a fact about the daemon ("reported no address").
#[test]
fn an_identity_without_the_advertise_field_parses_as_empty() {
    let js = r#"{"name":"the build runner","strategy":"spread_maker","params":"{}","live":true,
                     "build":"vike 0.1.0 (abc1234)"}"#;
    let id: WireHubIdentity =
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
            account: None,
        })
        .addressed_venue(),
        Some("bybit")
    );
}

/// The per-venue resume names the exchange whose halt it releases, so a node with no engine for it
/// refuses before the `Ack` (`venue_refusal`) and `force` changes nothing about the address.
#[test]
fn a_resume_venue_addresses_its_venue() {
    for force in [false, true] {
        assert_eq!(
            WireCommand::ResumeVenue { venue: "bybit".into(), force }.addressed_venue(),
            Some("bybit")
        );
    }
}

/// A frame without `force` decodes as `false` (the field defaults), and the key is written either way.
#[test]
fn a_resume_venue_frame_defaults_force_to_false() {
    let back: WireCommand = serde_json::from_str(r#"{"ResumeVenue":{"venue":"bybit"}}"#).unwrap();
    assert_eq!(back, WireCommand::ResumeVenue { venue: "bybit".into(), force: false });
    let js = serde_json::to_string(&back).unwrap();
    assert!(js.contains("\"force\":false"), "{js}");
}

/// The account-scoped state names the exchange of its account, so a node with no engine for it refuses
/// before the `Ack`; the state and the account ride the frame, and a frame an OLDER peer would send (no
/// such variant) is exactly the pre-existing vocabulary: every older frame still parses.
#[test]
fn an_account_trading_state_addresses_its_venue_and_old_frames_still_parse() {
    let cmd = WireCommand::SetAccountTradingState {
        venue: "binance".into(),
        account: "ALT".into(),
        state: WireTradingState::Halted,
    };
    assert_eq!(cmd.addressed_venue(), Some("binance"));
    let js = serde_json::to_string(&cmd).unwrap();
    assert_eq!(
        js,
        r#"{"SetAccountTradingState":{"venue":"binance","account":"ALT","state":"Halted"}}"#
    );
    assert_eq!(serde_json::from_str::<WireCommand>(&js).unwrap(), cmd);
    // The frames an old gatewayhub / node pair already exchanges, untouched by the new variant.
    let node_wide: WireCommand = serde_json::from_str(r#"{"SetTradingState":"Halted"}"#).unwrap();
    assert_eq!(node_wide, WireCommand::SetTradingState(WireTradingState::Halted));
    let resume: WireCommand = serde_json::from_str(r#"{"ResumeVenue":{"venue":"bybit"}}"#).unwrap();
    assert_eq!(resume, WireCommand::ResumeVenue { venue: "bybit".into(), force: false });
    // ...and a NEW variant is what an old node cannot decode, which is why the client refuses locally.
    assert!(serde_json::from_str::<WireCommand>(r#"{"NoSuchVerb":{"venue":"x"}}"#).is_err());
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
        WireCommand::ReconConfirm { id: 9 },
        WireCommand::ReconCancel { id: 9 },
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
// What an account's engine trades, its account's tier, which account an order rests in, and the
// accounts that do not run: all additive fields.
// -----------------------------------------------------------------------------------------

/// A block from a node that predates `symbols` and `tier` decodes as "not said".
#[test]
fn a_venue_block_without_symbols_or_tier_still_decodes() {
    let json = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_bp":0.0,"trading_state":"Active","positions":[]}"#;
    let v: WireVenueBlock = serde_json::from_str(json).expect("an older node's block decodes");
    assert!(v.symbols.is_empty());
    assert_eq!(v.tier, None);
}

/// The tier rides as its WORD (decision 0156: no tier enum on this wire, decision 0091 §3), under
/// the key `tier`, and round-trips unchanged.
#[test]
fn symbols_and_tier_round_trip() {
    let mut v = full_snapshot().venues.remove(0);
    v.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    v.tier = Some("live".into());
    let json = serde_json::to_string(&v).expect("serializes");
    assert!(json.contains("\"tier\":\"live\""), "the word on the wire: {json}");
    assert!(!json.contains("\"mode\""), "no second key for one fact: {json}");
    let back: WireVenueBlock = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, v);
}

/// **A tier value that is not a string is "not said", never a decode failure** (the observe loop
/// ends the link on an undecodable frame). Every such shape a newer node could send reads as
/// `None` with the REST of the block untouched; a STRING is carried as given (the reader's one
/// tier parser judges the word, so `quantum` arrives as `quantum` and is refused there).
#[test]
fn a_tier_value_that_is_not_a_word_reads_as_not_said_and_the_rest_of_the_block_survives() {
    let mut block = full_snapshot().venues.remove(0);
    block.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    block.account = Some("SUB".into());
    block.tier = Some("live".into());
    let mut expected = block.clone();
    expected.tier = None;
    let mut frame = serde_json::to_value(&block).expect("serializes");
    for unknown in [
        serde_json::json!(7),
        serde_json::json!(true),
        serde_json::json!(["live"]),
        serde_json::json!({"tier": "live"}),
        serde_json::Value::Null,
    ] {
        frame["tier"] = unknown.clone();
        let back: WireVenueBlock = serde_json::from_value(frame.clone())
            .unwrap_or_else(|e| panic!("`\"tier\": {unknown}` must decode as not said: {e}"));
        assert_eq!(
            back, expected,
            "`\"tier\": {unknown}` reads as not said and moves nothing else"
        );
    }
    frame.as_object_mut().expect("a block is an object").remove("tier");
    let back: WireVenueBlock = serde_json::from_value(frame.clone()).expect("decodes");
    assert_eq!(back, expected, "an absent key");
    for word in ["paper", "demo", "live", "quantum"] {
        frame["tier"] = serde_json::json!(word);
        let back: WireVenueBlock = serde_json::from_value(frame.clone()).expect("decodes");
        assert_eq!(back.tier.as_deref(), Some(word), "a word is carried as given");
    }
}

/// …and through the frame the observe loop decodes: an odd value beside a word; the frame decodes,
/// the word is kept, the odd value is "not said".
#[test]
fn a_snapshot_frame_with_one_odd_tier_still_decodes_and_keeps_the_word() {
    use crate::proto::Response;

    let mut snap = full_snapshot();
    let mut second = snap.venues[0].clone();
    second.venue = "okx".into();
    second.route_key = "okx".into();
    snap.venues[0].tier = Some("demo".into());
    second.tier = Some("live".into());
    snap.venues.push(second);

    let mut frame =
        serde_json::to_value(Response::SnapshotFrame(Box::new(snap.clone()))).expect("serializes");
    frame["SnapshotFrame"]["venues"][0]["tier"] = serde_json::json!(42);

    let Response::SnapshotFrame(back) = serde_json::from_value::<Response>(frame)
        .expect("an odd tier value must not fail the frame")
    else {
        panic!("a snapshot frame decodes as a snapshot frame");
    };
    assert_eq!(back.venues[0].tier, None, "the value this reader cannot take is not said");
    assert_eq!(back.venues[1].tier.as_deref(), Some("live"), "the word is kept");
    // Nothing else in the frame moved.
    snap.venues[0].tier = None;
    assert_eq!(*back, snap);
}

/// A block that says nothing about its engine puts NEITHER key on the wire. Asserted on the TEXT:
/// a round trip would pass with `"symbols":[]` and `"tier":null`, keys an older client never saw.
#[test]
fn a_venue_block_that_says_nothing_about_its_engine_serialises_without_either_key() {
    let mut v = full_snapshot().venues.remove(0);
    // Set outright rather than left to the fixture: the fixture's values are incidental.
    v.symbols = Vec::new();
    v.tier = None;
    let json = serde_json::to_string(&v).expect("serializes");
    assert!(!json.contains("\"symbols\""), "{json}");
    assert!(!json.contains("\"tier\""), "{json}");
}

/// **The accounts that do NOT run ride the frame with their reasons** (decision 0156): the default
/// account's key is the bare venue and carries no `account`; a labelled one carries its label; a
/// tier no row states is ABSENT, never a guessed word; the reason is the node's word, verbatim.
#[test]
fn the_accounts_that_do_not_run_round_trip_with_their_reasons() {
    let mut snap = full_snapshot();
    snap.not_running = vec![
        WireNotRunning {
            venue: "bybit".into(),
            account: None,
            route_key: "bybit".into(),
            tier: Some("live".into()),
            reason: "no live credentials".into(),
        },
        WireNotRunning {
            venue: "okx".into(),
            account: Some("ALT".into()),
            route_key: "okx#ALT".into(),
            tier: None,
            reason: "two active tiers".into(),
        },
    ];
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(json.contains("\"reason\":\"no live credentials\""), "{json}");
    let not_running = &serde_json::to_value(&snap).expect("serializes")["not_running"];
    assert!(not_running[0].get("account").is_none(), "the default account names no label");
    assert!(not_running[1].get("tier").is_none(), "no row states a tier: the key is absent");
    let back: WireSnapshot = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, snap);
}

/// A frame from a node that predates `not_running` decodes with none, and a frame in which every
/// account runs puts no `not_running` key on the wire at all.
#[test]
fn a_frame_without_not_running_decodes_and_an_empty_list_is_not_written() {
    let snap = full_snapshot();
    assert!(snap.not_running.is_empty(), "precondition");
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(!json.contains("\"not_running\""), "{json}");
    let back: WireSnapshot = serde_json::from_str(&json).expect("an older node's frame decodes");
    assert!(back.not_running.is_empty());
}

/// `key_changed` is route keys and nothing else on the wire; an empty list writes no key (a frame is
/// byte-identical to an older node's), and a frame without it decodes as "none changed".
#[test]
fn key_changed_is_route_keys_only_and_an_empty_list_is_not_written() {
    let mut snap = full_snapshot();
    assert_eq!(
        KEY_CHANGED_WARNING,
        "key changed in the database - the old key is still in use until a restart"
    );
    assert_eq!(serde_json::to_value(&snap).expect("serializes")["key_changed"][0], "bybit#ALT");
    snap.key_changed.clear();
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(!json.contains("key_changed"), "{json}");
    let back: WireSnapshot = serde_json::from_str(&json).expect("an older node's frame decodes");
    assert!(back.key_changed.is_empty());
}

/// **A held balance is a route key and short words on the wire, and an empty list is not written**: a frame with
/// nothing held is byte-identical to an older node's, a frame without the key decodes as "nothing held", and an
/// OLDER observer (this struct before the field) ignores the key because no feature string guards it (decision 0112
/// verdict 3).
#[test]
fn balance_held_is_words_only_and_an_empty_list_is_not_written() {
    let mut snap = full_snapshot();
    let value = serde_json::to_value(&snap).expect("serializes");
    assert_eq!(
        value["balance_held"],
        serde_json::json!([{ "route_key": "oanda", "reason": "assumed USD" }])
    );
    snap.balance_held.clear();
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(!json.contains("balance_held"), "{json}");
    let back: WireSnapshot = serde_json::from_str(&json).expect("an older node's frame decodes");
    assert!(back.balance_held.is_empty());
}

/// **A venue path that is down is a route key, words and two counters on the wire, an empty list is not written, and
/// every old/new pairing reads** (decision 0112 verdict 3, a pure addition to a reply): a frame with every path up is
/// byte-identical to an older node's (no key), a frame without the key decodes as "none down" (an older node: the
/// engine reads as it always did), an OLDER observer (this struct without the field) ignores the key, and a frame
/// that names the outage decodes whole under this one. No journal is involved: the field is stamped on the frame
/// by the publisher, never folded or stored.
#[test]
fn venue_link_down_is_additive_and_old_and_new_observers_read_each_other() {
    let mut snap = full_snapshot();
    let value = serde_json::to_value(&snap).expect("serializes");
    assert_eq!(
        value["venue_link_down"],
        serde_json::json!([{
            "route_key": "dukascopy",
            "why": "JForex session disconnected",
            "failed_restarts": 2,
            "retry_in_ms": 20_000
        }])
    );
    let with = serde_json::to_string(&snap).expect("serializes");
    let back: WireSnapshot = serde_json::from_str(&with).expect("a new node's frame decodes");
    assert_eq!(back.venue_link_down, snap.venue_link_down);

    // An older OBSERVER: the snapshot as v0.1.91 declared it knows no such key, and serde ignores it (no
    // `deny_unknown_fields` on the frame). A reader of exactly the old keys it needs stands in for it.
    #[derive(serde::Deserialize)]
    struct Before {
        seq: u64,
        venues: Vec<WireVenueBlock>,
        #[serde(default)]
        balance_held: Vec<WireBalanceHeld>,
    }
    let old: Before = serde_json::from_str(&with).expect("an older observer reads a newer frame");
    assert_eq!((old.seq, old.venues.len(), old.balance_held.len()), (42, 1, 1));

    // A NEWER observer against an OLDER node: the frame has no key and reads as nothing down.
    snap.venue_link_down.clear();
    let without = serde_json::to_string(&snap).expect("serializes");
    assert!(!without.contains("venue_link_down"), "an empty list is not written: {without}");
    let back: WireSnapshot = serde_json::from_str(&without).expect("an older node's frame decodes");
    assert!(back.venue_link_down.is_empty());
}

/// Every operator surface words an outage through [`WireVenueLinkDown::say`]: the reason, what failed, what to expect.
#[test]
fn a_venue_path_outage_says_the_reason_the_failures_and_what_to_expect() {
    let down = WireVenueLinkDown {
        route_key: "dukascopy".into(),
        why: "JForex session disconnected".into(),
        failed_restarts: 2,
        retry_in_ms: 20_000,
    };
    let said = down.say();
    for part in [
        "the venue path is DOWN: JForex session disconnected",
        "2 restart(s) failed so far",
        "the next try in about 20 s",
        "orders are refused (`bridge unavailable ... NOT sent`)",
        "which it does by itself",
    ] {
        assert!(said.contains(part), "{part:?} missing from {said}");
    }
    let nameless = WireVenueLinkDown { why: String::new(), retry_in_ms: 1, ..down };
    assert!(nameless.say().contains("no reason given") && nameless.say().contains("about 1 s"));
}

/// Every operator surface words the hold through [`WireBalanceHeld::line`]: the account, the exact reason words the
/// node's gate states, and what the hold means for the account's equity.
#[test]
fn a_held_balance_line_names_the_account_the_reason_and_the_consequence() {
    let line =
        WireBalanceHeld { route_key: "oanda#ALT".into(), reason: "assumed USD".into() }.line();
    assert!(line.starts_with("oanda#ALT: balance held: assumed USD"), "{line}");
    assert!(line.contains("not adopted") && line.contains("seed + PnL"), "{line}");
    let held = WireBalanceHeld { route_key: "oanda#ALT".into(), reason: "assumed USD".into() };
    assert_eq!(held.line(), format!("oanda#ALT: {}", held.say()));
    assert_eq!(held.say(), format!("balance held: assumed USD - {BALANCE_HELD_MEANING}"));
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

/// ⚠ **A bracket frame carrying a key this build does not know does not decode**
/// (`deny_unknown_fields`): never a decode that drops the key and puts the bracket on the default
/// book. This is what keeps `account` safe against a node from BEFORE the field, which sees exactly
/// this shape (an unknown key) when a new client names an account without the capability check.
#[test]
fn a_bracket_frame_with_an_unknown_key_does_not_decode() {
    let js = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,
                 "entry_price":null,"stop_loss":95.0,"take_profit":110.0,"leverage":3}}"#;
    assert!(
        serde_json::from_str::<WireCommand>(js).is_err(),
        "an unknown key on a bracket is an error, never silently dropped"
    );
    // POSITIVE CONTROL: the same frame minus the key decodes (explicit `null` = MARKET entry).
    let js = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,
                 "entry_price":null,"stop_loss":95.0,"take_profit":110.0}}"#;
    match serde_json::from_str::<WireCommand>(js).expect("the account-less frame decodes") {
        WireCommand::Bracket(b) => assert_eq!(b.entry_price, None, "an explicit null is market"),
        other => panic!("expected a Bracket, got {other:?}"),
    }
}

/// **A bracket names its account on the wire, and an OLD peer's frame still parses.** The key is
/// written only when set (so an account-less bracket is byte-identical to the pre-field frame and a
/// node from before the field still decodes it), a frame without the key reads `None`, and a named
/// one round-trips whatever the spelling (`DEFAULT` included, which is a name, not an absence).
#[test]
fn a_bracket_names_its_account_only_when_it_has_one() {
    let spec = |account: Option<&str>| WireBracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: account.map(str::to_string),
    };
    // Without: no key, and the old frame (written by a pre-field client) reads back as `None`.
    let bare = serde_json::to_string(&WireCommand::Bracket(spec(None))).expect("serializes");
    assert!(!bare.contains("account"), "an account-less bracket writes no key: {bare}");
    let old = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT.P","side":1,"qty":1.0,
                  "entry_price":100.0,"stop_loss":95.0,"take_profit":110.0}}"#;
    assert_eq!(
        serde_json::from_str::<WireCommand>(old).expect("an old-peer frame decodes"),
        WireCommand::Bracket(spec(None)),
        "…as the account-less bracket"
    );
    // With: the key is written and survives the round trip, `DEFAULT` and a label alike.
    for named in ["PERP", "DEFAULT"] {
        let sent = WireCommand::Bracket(spec(Some(named)));
        let json = serde_json::to_string(&sent).expect("serializes");
        assert!(json.contains(&format!("\"account\":\"{named}\"")), "{json}");
        assert_eq!(serde_json::from_str::<WireCommand>(&json).expect("decodes"), sent);
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
            account: None,
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
        account: None,
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

/// The wire's `order_type` stays a STRING, carried verbatim both ways: the node, not this mirror,
/// judges the word (`vike_tradehub`'s `lower_command` reads it through `vike_model::OrderType`'s
/// input door and refuses an unknown one, naming the accepted words). So every frame a client
/// ever sent — any casing, any word — still decodes, and re-encodes to the very same bytes.
#[test]
fn wire_order_request_carries_order_type_verbatim_and_byte_identically() {
    for word in ["market", "limit", "LIMIT", "stop_market", ""] {
        let js = format!(
            r#"{{"client_order_id":"c-1","venue":"sim","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"{word}","price":2.0,"trigger_price":null,"reduce_only":false}}"#
        );
        let req: WireOrderRequest = serde_json::from_str(&js).unwrap();
        assert_eq!(req.order_type, word);
        assert_eq!(serde_json::to_string(&req).unwrap(), js, "{word:?}: bytes unchanged");
    }
}

/// The two credential verbs of decision 0144's wire half: the retire carries the typed confirm the
/// node refuses without (equal to the key), the presence read carries none, and neither prints
/// anything but key NAMES in a `Debug`.
#[test]
fn the_retire_and_presence_verbs_state_their_ceremony_and_print_names_only() {
    let retire = AccountVerb::RetireCredential { key: "SOME_KEY".into() };
    assert_eq!(retire.word(), "retire-credential");
    assert_eq!(retire.required_confirm().as_deref(), Some("SOME_KEY"));
    let held = AccountVerb::HeldCredentials { names: vec!["A".into(), "B".into()] };
    assert_eq!(held.word(), "held-credentials");
    assert_eq!(held.required_confirm(), None, "a read owes no ceremony");

    let req = AccountRequest { verb: retire, confirm: Some("SOME_KEY".into()) };
    assert_eq!(format!("{req:?}"), "AccountRequest(retire-credential key=SOME_KEY confirm=typed)");
    let req = AccountRequest { verb: held, confirm: None };
    assert_eq!(format!("{req:?}"), "AccountRequest(held-credentials names=A,B confirm=absent)");
}

/// Both new verbs survive the wire's JSON round trip, and a frame WITHOUT the `confirm` key still
/// decodes (`#[serde(default)]`), so the presence read needs no field.
#[test]
fn the_retire_and_presence_requests_round_trip() {
    for req in [
        AccountRequest {
            verb: AccountVerb::RetireCredential { key: "SOME_KEY".into() },
            confirm: Some("SOME_KEY".into()),
        },
        AccountRequest {
            verb: AccountVerb::HeldCredentials { names: vec!["A".into()] },
            confirm: None,
        },
    ] {
        let js = serde_json::to_string(&req).unwrap();
        let back: AccountRequest = serde_json::from_str(&js).unwrap();
        assert_eq!(req, back);
    }
    let bare: AccountRequest =
        serde_json::from_str(r#"{"verb":{"HeldCredentials":{"names":["A"]}}}"#).unwrap();
    assert_eq!(bare.confirm, None);
}

/// `WireOrderView::reason` is ADDITIVE: an order from a node that predates it decodes as "no
/// reason", an order with none puts no `reason` key on the wire (every frame stays byte-identical to
/// before), and a refused order carries its words and round-trips. A reader of an older build
/// ignores the key (`WireOrderView` has no `deny_unknown_fields`).
#[test]
fn an_order_reason_is_additive_on_the_wire() {
    let old = r#"{"client_order_id":"c-1","venue":"binance","symbol":"BTCUSDT","side":1,
        "qty":0.5,"order_type":"limit","price":59000.0,"trigger_price":null,"status":"REJECTED",
        "venue_order_id":null,"filled_qty":0.0,"avg_fill_px":0.0}"#;
    let o: WireOrderView = serde_json::from_str(old).expect("an older node's order decodes");
    assert_eq!(o.reason, None);

    let mut o = full_snapshot().orders.remove(0);
    o.reason = None;
    let json = serde_json::to_string(&o).expect("serializes");
    assert!(!json.contains("\"reason\""), "{json}");

    o.reason = Some("INVALID_REQUEST: more digits than symbol allows".into());
    let json = serde_json::to_string(&o).expect("serializes");
    assert!(json.contains("\"reason\":\"INVALID_REQUEST"), "{json}");
    let back: WireOrderView = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, o);
}

/// `WireOrderView::age_ms` is ADDITIVE the same way: an order from a node that predates it decodes
/// as "no age" (never `0`: that would call every order new), an order with none puts no key on the
/// wire, and a reading round-trips, a zero included (an order the node saw a moment ago).
#[test]
fn an_order_age_is_additive_on_the_wire() {
    let old = r#"{"client_order_id":"c-1","venue":"binance","symbol":"BTCUSDT","side":1,
        "qty":0.5,"order_type":"limit","price":59000.0,"trigger_price":null,"status":"ACCEPTED",
        "venue_order_id":null,"filled_qty":0.0,"avg_fill_px":0.0}"#;
    let o: WireOrderView = serde_json::from_str(old).expect("an older node's order decodes");
    assert_eq!(o.age_ms, None);

    let mut o = full_snapshot().orders.remove(0);
    assert_eq!(o.age_ms, Some(61_000), "precondition: the fixture carries an age");
    o.age_ms = None;
    let json = serde_json::to_string(&o).expect("serializes");
    assert!(!json.contains("age_ms"), "{json}");

    for age in [0, 61_000, u64::MAX] {
        o.age_ms = Some(age);
        let json = serde_json::to_string(&o).expect("serializes");
        assert!(json.contains(&format!("\"age_ms\":{age}")), "{json}");
        let back: WireOrderView = serde_json::from_str(&json).expect("decodes");
        assert_eq!(back, o);
    }
}

/// The node's own readings of itself ride the FRAME and are ADDITIVE (decision 0112 verdict 3: a
/// pure addition to a reply needs no feature string). The pair is tri-state on purpose: a node that
/// predates them decodes as "not said" - never `0` ms of uptime and never "no sentinel" - and a node
/// that says nothing writes no key, so its frame is byte-identical to an older node's.
#[test]
fn the_uptime_and_the_halt_sentinel_are_additive_and_unsaid_is_neither_zero_nor_false() {
    let mut snap = full_snapshot();
    let said = (snap.uptime_ms, snap.halt_sentinel);
    assert_eq!(said, (Some(86_400_000), Some(true)), "precondition: the fixture carries both");
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(json.contains("\"uptime_ms\":86400000"), "{json}");
    assert!(json.contains("\"halt_sentinel\":true"), "{json}");
    let back: WireSnapshot = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, snap);

    // `Some(false)` is a reading too, and it is WRITTEN: it is the node saying the switch is clear.
    snap.halt_sentinel = Some(false);
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(json.contains("\"halt_sentinel\":false"), "{json}");

    // A node with nothing to say writes neither key, and a frame without them decodes as unsaid.
    snap.uptime_ms = None;
    snap.halt_sentinel = None;
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(!json.contains("uptime_ms") && !json.contains("halt_sentinel"), "{json}");
    let back: WireSnapshot = serde_json::from_str(&json).expect("an older node's frame decodes");
    assert_eq!((back.uptime_ms, back.halt_sentinel), (None, None));
}

/// Decision 0170: the engine's non-default contract values ride the venue block as text and a
/// number, round-trip unchanged, are ABSENT from the frame when empty, and a block from a node that
/// predates the field decodes with none (the default for every symbol).
#[test]
fn contract_values_round_trip_and_an_older_block_decodes_without_them() {
    let mut v = full_snapshot().venues.remove(0);
    let empty = serde_json::to_string(&v).expect("serializes");
    assert!(!empty.contains("contract_values"), "an empty grid writes no key: {empty}");
    v.contract_values = vec![WireContractValue {
        symbol: "BTC-PERPETUAL".into(),
        kind: "inverse".into(),
        per_unit: 1.0,
    }];
    let json = serde_json::to_string(&v).expect("serializes");
    assert!(
        json.contains(
            r#""contract_values":[{"symbol":"BTC-PERPETUAL","kind":"inverse","per_unit":1.0}]"#
        ),
        "{json}"
    );
    let back: WireVenueBlock = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, v);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_bp":0.0,"trading_state":"Active","positions":[]}"#;
    let v: WireVenueBlock = serde_json::from_str(older).expect("an older node's block decodes");
    assert!(v.contract_values.is_empty());
}

/// The "trade any symbol" capability rides the venue block as one bool: ABSENT from the frame when
/// false (a venue that does not offer it publishes the frame it always did), written when true, and
/// an older node's block, which has never heard of it, decodes to false: one symbol only.
#[test]
fn admits_symbols_round_trips_is_absent_when_false_and_an_older_block_reads_false() {
    let mut v = full_snapshot().venues.remove(0);
    let off = serde_json::to_string(&v).expect("serializes");
    assert!(!off.contains("admits_symbols"), "false writes no key: {off}");
    v.admits_symbols = true;
    let on = serde_json::to_string(&v).expect("serializes");
    assert!(on.contains(r#""admits_symbols":true"#), "{on}");
    let back: WireVenueBlock = serde_json::from_str(&on).expect("decodes");
    assert!(back.admits_symbols);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_bp":0.0,"trading_state":"Active","positions":[],"tier":"live"}"#;
    let v: WireVenueBlock = serde_json::from_str(older).expect("an older node's block decodes");
    assert!(!v.admits_symbols, "an older node offers nothing");

    // The OLD-client half: a reader that predates a key ignores it (no `deny_unknown_fields`), which
    // is how a desktop built before this field reads a current node's block.
    let newer = older.replacen('{', r#"{"admits_symbols":true,"a_key_nobody_reads_yet":1,"#, 1);
    assert!(serde_json::from_str::<WireVenueBlock>(&newer).is_ok());
}

/// **The conversion-rate table rides the venue block** (decision 0183, phase 1): ABSENT while empty (an
/// account with no symbol quoted in another currency publishes the frame it always did), written when it has
/// rows, round-tripped with the currency the rates are expressed in, decoded to empty from an older node's
/// block, and ignored by a reader that predates it.
#[test]
fn quote_rates_round_trip_are_absent_when_empty_and_an_older_block_reads_none() {
    let mut v = full_snapshot().venues.remove(0);
    let off = serde_json::to_string(&v).expect("serializes");
    assert!(!off.contains("quote_rates"), "an empty table writes no key: {off}");
    v.quote_rates = vec![
        WireQuoteRate { ccy: "GBP".into(), rate: 1.35, as_of_ms: 1_700_000_000_000, stale: false },
        WireQuoteRate {
            ccy: "JPY".into(),
            rate: 1.0 / 150.0,
            as_of_ms: 1_699_999_000_000,
            stale: true,
        },
    ];
    v.quote_rates_in = "USD".into();
    let on = serde_json::to_string(&v).expect("serializes");
    assert!(on.contains(r#""quote_rates_in":"USD""#) && on.contains(r#""ccy":"GBP""#), "{on}");
    let back: WireVenueBlock = serde_json::from_str(&on).expect("decodes");
    assert_eq!(back, v);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_bp":0.0,"trading_state":"Active","positions":[],"tier":"live"}"#;
    let old: WireVenueBlock = serde_json::from_str(older).expect("an older node's block decodes");
    assert!(old.quote_rates.is_empty() && old.quote_rates_in.is_empty());

    // The OLD-client half: a reader without the field ignores the key (no `deny_unknown_fields`).
    #[derive(serde::Deserialize)]
    struct BeforeTheField {
        venue: String,
    }
    let before: BeforeTheField = serde_json::from_str(&on).expect("an old reader ignores the key");
    assert_eq!(before.venue, "binance");
}

/// **The currency group rides the venue block** (decision 0183, item 13) under decision 0112 verdict 3,
/// with no feature string: ABSENT for the dollar group (a node of today writes the block it always did),
/// written for another currency, round-tripped, read as the dollar group from an older node's block, and
/// ignored by a reader that predates it.
#[test]
fn ledger_ccy_is_absent_for_the_dollar_group_and_an_older_block_reads_it_empty() {
    let mut v = full_snapshot().venues.remove(0);
    let off = serde_json::to_string(&v).expect("serializes");
    assert!(!off.contains("ledger_ccy"), "the dollar group writes no key: {off}");
    v.ledger_ccy = "EUR".into();
    let on = serde_json::to_string(&v).expect("serializes");
    assert!(on.contains(r#""ledger_ccy":"EUR""#), "{on}");
    let back: WireVenueBlock = serde_json::from_str(&on).expect("decodes");
    assert_eq!(back, v);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_bp":0.0,"trading_state":"Active","positions":[],"tier":"live"}"#;
    let old: WireVenueBlock = serde_json::from_str(older).expect("an older node's block decodes");
    assert!(old.ledger_ccy.is_empty(), "no key reads as the dollar group");

    // The OLD-client half: a reader without the field ignores the key (no `deny_unknown_fields`).
    #[derive(serde::Deserialize)]
    struct BeforeTheField {
        venue: String,
    }
    let before: BeforeTheField = serde_json::from_str(&on).expect("an old reader ignores the key");
    assert_eq!(before.venue, "binance");
}
