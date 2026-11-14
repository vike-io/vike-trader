use super::*;
use vike_model::OrderStopAdjustTo;

fn full_snapshot() -> CoreSnapshotWire {
    CoreSnapshotWire {
        sequence: 42,
        accounts_epoch: 0,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trading_state: TradingState::Reducing,
        balance: 10_000.0,
        equity_total: 12_345.67,
        venues: vec![VenueBlockWire {
            account_code: None,
            order_client_id_shape: None,
            venue: "binance".into(),
            balance: 10_000.0,
            realized_pnl: 250.5,
            fees_paid: 3.25,
            funding_paid: -1.5,
            equity: 12_345.67,
            unrealized: 2_345.67,
            missing_prices: 1,
            margin_used: 500.0,
            free_buying_power: 11_845.67,
            trading_state: TradingState::Reducing,
            account: None,
            route_key: "binance".into(),
            symbols: Vec::new(),
            tier: None,
            contract_values: Vec::new(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: String::new(),
            ledger_currency: String::new(),
            currency_assumed: false,
            balance_is_own_book: false,
            positions: vec![PositionViewWire {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: "BOTH".into(),
                size: 0.5,
                average_price: 60_000.0,
                unrealized: 2_345.67,
                leverage: 5.0,
                liquidation_price: 48_000.0,
            }],
        }],
        orders: vec![OrderViewWire {
            data_delay: None,
            order_client_id: "c-1".into(),
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
            average_fill_price: 0.0,
            reason: None,
            age_ms: Some(61_000),
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
        }],
        positions: vec![PositionViewWire {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: "BOTH".into(),
            size: 0.5,
            average_price: 60_000.0,
            unrealized: 2_345.67,
            leverage: 5.0,
            liquidation_price: 48_000.0,
        }],
        held_exits: vec![OrderHeldViewWire {
            order_client_id: "c-2".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            order_type: "stop".into(),
            price: None,
            trigger_price: Some(55_000.0),
            parent_order_id: Some("c-1".into()),
            account: None,
            release: None,
            waiting: None,
            release_venue: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            display_quantity: None,
        }],
        recent_events: vec!["OrderAccepted c-1".into(), "OrderSubmitted c-1".into()],
        fault: None,
        bars: vec![BarSeriesWire {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            closed: vec![
                BarWire {
                    timestamp: 1_000,
                    o: 60_000.0,
                    h: 60_100.0,
                    l: 59_900.0,
                    c: 60_050.0,
                    v: 12.5,
                },
                BarWire {
                    timestamp: 61_000,
                    o: 60_050.0,
                    h: 60_200.0,
                    l: 60_000.0,
                    c: 60_150.0,
                    v: 8.0,
                },
            ],
            forming: Some(BarWire {
                timestamp: 121_000,
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
        balance_withheld: vec![BalanceWithheldWire {
            route_key: "oanda".into(),
            reason: "assumed USD".into(),
        }],
        venue_link_down: vec![VenueLinkDownWire {
            route_key: "dukascopy".into(),
            why: "JForex session disconnected".into(),
            failed_restarts: 2,
            retry_in_ms: 20_000,
            effect: String::new(),
            no_adapter: false,
        }],
        venue_path_up: vec!["binance".into()],
        trade_link_down: vec![TradeLinkDownWire {
            route_key: "binance".into(),
            why: "the gatewayhub is stopping".into(),
            down_ms: 1_500,
            closed: false,
        }],
        price_link_down: vec![PriceLinkDownWire {
            venue: "bybit".into(),
            why: "connection refused".into(),
            down_ms: 2_500,
            of: 2,
        }],
        price_dials_up: Some(2),
        core_held_stops: vec![OrderEmulationStopViewWire {
            arm_id: "s0a1".into(),
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            account: "polymarket".into(),
            side: -1,
            qty: 5.0,
            trigger_price: 0.4,
            held: true,
            rearms: 1,
        }],
        emulated_orders: vec![emulated_chase()],
        reconcile: vec![ReconcileAlertViewWire {
            id: 9,
            account: "bybit".into(),
            kind: "OrphanLocalPosition".into(),
            detail: "local bybit ETHUSDT BOTH position 8.34 has NO venue position row".into(),
            proposed_event_count: 0,
        }],
        uptime_ms: Some(86_400_000),
        halt_sentinel: Some(true),
        primary_currency: String::new(),
        published_ms: 0,
    }
}

/// A fully-populated snapshot (every `Some` arm, every list non-empty) round-trips.
#[test]
fn wire_snapshot_round_trips_fully_populated() {
    let snap = full_snapshot();
    let js = serde_json::to_string(&snap).unwrap();
    let back: CoreSnapshotWire = serde_json::from_str(&js).unwrap();
    assert_eq!(snap, back);
}

/// Every `CommandWire` variant round-trips through JSON.
#[test]
fn wire_command_round_trips_each_variant() {
    let commands = vec![
        CommandWire::Submit(OrderRequestWire {
            order_client_id: String::new(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            price: None,
            trigger_price: None,
            reduce_only: false,
            account: None,
            time_in_force: None,
            gtd_expiry: None,
            display_quantity: None,
            release: None,
            trail: None,
            limit_offset: None,
            peg: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
        }),
        CommandWire::Cancel("c-1".into()),
        CommandWire::Modify {
            order_client_id: "c-1".into(),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
        },
        CommandWire::MassCancel { venue: None, symbol: None, account: None },
        CommandWire::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into(), account: None },
        CommandWire::MarketExit { venue: Some("binance".into()), account: None },
        CommandWire::SetTradingState(TradingState::Halted),
        // The params payload is the core's own StrategyParams JSON, carried opaquely.
        CommandWire::UpdateParams {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            mount_id: None,
            params: serde_json::json!({"SpreadMaker": {"qty": 2.0, "half_spread": 1.0}}),
        },
        // The strategy FILE mount, then the two retired source arms (registry name / rhai path),
        // which still round-trip for the node to refuse with the fix, then the unmount.
        CommandWire::MountStrategy {
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: Some("momentum-a".into()),
            file: Some("trend/momentum.rs".into()),
            name: None,
            rhai: None,
            params: serde_json::json!({"size": 2.0}),
        },
        CommandWire::MountStrategy {
            venue: "binance".into(),
            account: Some("ALT".into()),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: Some("grid-a".into()),
            file: None,
            name: Some("grid".into()),
            rhai: None,
            params: serde_json::json!({"qty": 1.0}),
        },
        CommandWire::MountStrategy {
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            controller_id: None,
            file: None,
            name: None,
            rhai: Some("strategies/breaker.rhai".into()),
            params: serde_json::json!({}),
        },
        CommandWire::UnmountStrategy { controller_id: "grid-a".into() },
        CommandWire::ReconcileConfirm { id: 9 },
        CommandWire::ReconcileCancel { id: 9 },
        CommandWire::ResumeVenue { venue: "bybit".into(), force: false },
        CommandWire::ResumeVenue { venue: "bybit".into(), force: true },
        CommandWire::SetAccountTradingState {
            venue: "binance".into(),
            account: Some("ALT".into()),
            state: TradingState::Reducing,
        },
        CommandWire::UnadmitSymbol {
            venue: "binance".into(),
            account: Some("DEFAULT".into()),
            symbol: "ETHUSDT.P".into(),
        },
        CommandWire::UnlatchMount { mount_id: "binance__BTCUSDT__1m".into() },
        // A confirm-less edit, and an OLDER client's shape (a file NAME and a typed confirm, both
        // ignored since `docs/decisions/0086`): both round-trip until the fields are deleted.
        CommandWire::SetSetting {
            file: "config.toml".into(),
            key: "config.tradehub_addr".into(),
            value: "127.0.0.1:7879".into(),
            confirm: None,
        },
        CommandWire::SetSetting {
            file: "policy.toml".into(),
            key: "policy.max_notional_per_order".into(),
            value: "250".into(),
            confirm: Some("policy.max_notional_per_order".into()),
        },
        // The TP/SL bracket: `vike_model::OrderBracket` field for field, a MARKET entry here.
        CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
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
    for c in commands {
        let js = serde_json::to_string(&c).unwrap();
        let back: CommandWire = serde_json::from_str(&js).unwrap();
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
    let cmd: CommandWire = serde_json::from_value(bare)
        .expect("a SetSetting naming only its key and value must decode");
    let CommandWire::SetSetting { file, key, value, confirm } = cmd else {
        panic!("decoded as a different variant")
    };
    assert_eq!((key.as_str(), value.as_str()), ("policy.max_notional_per_order", "250"));
    assert!(file.is_empty(), "an absent file decodes to nothing: {file:?}");
    assert_eq!(confirm, None, "an absent confirm decodes to none");

    let sent = serde_json::to_value(CommandWire::SetSetting {
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
    let full = StrategyStatusWire {
        identity: HubIdentityWire {
            name: "the build runner".into(),
            strategy: "spread_maker".into(),
            params: "qty=1 half_spread=0.5".into(),
            live: true,
            build: "vike 0.1.0 (abc1234)".into(),
            advertise_addr: "203.0.113.7:7879".into(),
        },
        effective_params: "qty=1 half_spread=0.5".into(),
        mounts: vec![MountViewWire {
            order_client_id_code: None,
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
            latched: false,
            route_key: "binance#ALT".into(),
            tier: Some("demo".into()),
            not_running: true,
        }],
    };
    for status in [full.clone(), StrategyStatusWire { mounts: Vec::new(), ..full }] {
        let js = serde_json::to_string(&status).unwrap();
        let back: StrategyStatusWire = serde_json::from_str(&js).unwrap();
        assert_eq!(status, back);
    }
}

/// The additive-field contract: a `MountViewWire` from a node that predates
/// [`crate::proto::FEATURE_STRATEGY_PARAMS`] parses with an EMPTY addressing key and `None` params.
/// ⚠ That is indistinguishable from a new node's "no typed params", which is why the capability
/// exists; [`crate::tradehub_handle::strategy_params`] checks it.
#[test]
fn a_mount_row_without_the_params_fields_parses_as_empty() {
    let js = r#"{"strategy":"spread_maker","params":"qty=1 half_spread=0.5","live":true}"#;
    let row: MountViewWire = serde_json::from_str(js).expect("an old node's row still parses");
    assert_eq!(row.strategy, "spread_maker");
    assert!(row.live, "the pre-existing fields are untouched");
    assert_eq!((row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()), ("", "", ""));
    assert_eq!(row.typed_params, None);
    assert_eq!((row.route_key.as_str(), row.tier.as_deref(), row.not_running), ("", None, false));
}

/// A minimal `OrderRequestWire` (required fields only) deserializes.
#[test]
fn wire_order_request_accepts_minimal_json() {
    let js = r#"{"venue":"sim","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"market"}"#;
    let req: OrderRequestWire = serde_json::from_str(js).unwrap();
    assert_eq!(req.order_client_id, "");
    assert_eq!(req.price, None);
    assert!(!req.reduce_only);
}

/// The identity block round-trips when present; paper-vs-live survives exactly.
#[test]
fn identity_roundtrips_when_present() {
    let mut s = full_snapshot();
    s.identity = Some(HubIdentityWire {
        name: "the build runner".into(),
        strategy: "spread_maker".into(),
        params: "qty=1 half_spread=0.5".into(),
        live: true,
        build: "vike 0.1.0 (abc1234)".into(),
        advertise_addr: "203.0.113.7:7879".into(),
    });
    let json = serde_json::to_string(&s).unwrap();
    let back: CoreSnapshotWire = serde_json::from_str(&json).unwrap();
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
    let back: CoreSnapshotWire = serde_json::from_value(v).unwrap();
    assert!(back.identity.is_none());
}

/// The same contract for the ACCOUNT-SET digest: absent parses as `0`. That `0` is the stamp an
/// MCP preview takes against an older node (`crates/vike-mcp/src/venue_gate.rs`'s
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
    let back: CoreSnapshotWire =
        serde_json::from_value(v).expect("a frame from a node predating the field still parses");
    assert_eq!(back.accounts_epoch, 0, "an absent epoch reads as 0, the pre-field answer");
}

/// The same contract one struct down: an identity block without
/// [`HubIdentityWire::advertise_addr`] parses with it EMPTY — not an error (which would drop the
/// whole frame) and not a fact about the daemon ("reported no address").
#[test]
fn an_identity_without_the_advertise_field_parses_as_empty() {
    let js = r#"{"name":"the build runner","strategy":"spread_maker","params":"{}","live":true,
                     "build":"vike 0.1.0 (abc1234)"}"#;
    let id: HubIdentityWire =
        serde_json::from_str(js).expect("an old node's identity block still parses");
    assert_eq!(id.name, "the build runner", "the pre-existing fields are untouched");
    assert!(id.live);
    assert_eq!(id.advertise_addr, "", "absent is EMPTY, never an error and never a default IP");
}

// -----------------------------------------------------------------------------------------
// The ADDRESS a command carries
// -----------------------------------------------------------------------------------------

fn order(venue: &str) -> OrderRequestWire {
    OrderRequestWire {
        order_client_id: "c-1".into(),
        venue: venue.into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    }
}

/// Every venue-carrying variant answers with ITS OWN venue field: the node routes on exactly this
/// string, so a mis-wired arm is a mis-routed order.
#[test]
fn every_venue_carrying_variant_answers_with_its_own_venue() {
    assert_eq!(CommandWire::Submit(order("binance")).addressed_venue(), Some("binance"));
    assert_eq!(
        CommandWire::Flatten { venue: "okx".into(), symbol: "SYM".into(), account: None }
            .addressed_venue(),
        Some("okx")
    );
    assert_eq!(
        CommandWire::MassCancel { venue: Some("bybit".into()), symbol: None, account: None }
            .addressed_venue(),
        Some("bybit")
    );
    assert_eq!(
        CommandWire::MarketExit { venue: Some("deribit".into()), account: None }.addressed_venue(),
        Some("deribit")
    );
    assert_eq!(
        CommandWire::UpdateParams {
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
        CommandWire::MountStrategy {
            venue: "hyperliquid".into(),
            account: None,
            symbol: "SYM".into(),
            interval: "1m".into(),
            controller_id: None,
            file: None,
            name: Some("buy_hold".into()),
            rhai: None,
            params: serde_json::json!({}),
        }
        .addressed_venue(),
        Some("hyperliquid")
    );
    assert_eq!(
        CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
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
            CommandWire::ResumeVenue { venue: "bybit".into(), force }.addressed_venue(),
            Some("bybit")
        );
    }
}

/// A frame without `force` decodes as `false` (the field defaults), and the key is written either way.
#[test]
fn a_resume_venue_frame_defaults_force_to_false() {
    let back: CommandWire = serde_json::from_str(r#"{"ResumeVenue":{"venue":"bybit"}}"#).unwrap();
    assert_eq!(back, CommandWire::ResumeVenue { venue: "bybit".into(), force: false });
    let js = serde_json::to_string(&back).unwrap();
    assert!(js.contains("\"force\":false"), "{js}");
}

/// The account-scoped state names the exchange of its account, so a node with no engine for it refuses
/// before the `Ack`; the state and the account ride the frame, and a frame an OLDER peer would send (no
/// such variant) is exactly the pre-existing vocabulary: every older frame still parses.
#[test]
fn an_account_trading_state_addresses_its_venue_and_old_frames_still_parse() {
    let cmd = CommandWire::SetAccountTradingState {
        venue: "binance".into(),
        account: Some("ALT".into()),
        state: TradingState::Halted,
    };
    assert_eq!(cmd.addressed_venue(), Some("binance"));
    let js = serde_json::to_string(&cmd).unwrap();
    assert_eq!(
        js,
        r#"{"SetAccountTradingState":{"venue":"binance","account":"ALT","state":"Halted"}}"#
    );
    assert_eq!(serde_json::from_str::<CommandWire>(&js).unwrap(), cmd);
    // The frames an old gatewayhub / node pair already exchanges, untouched by the new variant.
    let node_wide: CommandWire = serde_json::from_str(r#"{"SetTradingState":"Halted"}"#).unwrap();
    assert_eq!(node_wide, CommandWire::SetTradingState(TradingState::Halted));
    let resume: CommandWire = serde_json::from_str(r#"{"ResumeVenue":{"venue":"bybit"}}"#).unwrap();
    assert_eq!(resume, CommandWire::ResumeVenue { venue: "bybit".into(), force: false });
    // ...and a NEW variant is what an old node cannot decode, which is why the client refuses locally.
    assert!(serde_json::from_str::<CommandWire>(r#"{"NoSuchVerb":{"venue":"x"}}"#).is_err());
}

/// The give-back of an admitted symbol (decision 0205) names the exchange of its account, carries the book and the
/// symbol, and is a frame an older node cannot decode (the client owes `unadmit-symbol` before it sends one).
#[test]
fn an_unadmit_addresses_its_venue_and_has_a_pinned_shape() {
    let cmd = CommandWire::UnadmitSymbol {
        venue: "binance".into(),
        account: Some("ALT".into()),
        symbol: "ETHUSDT.P".into(),
    };
    assert_eq!(cmd.addressed_venue(), Some("binance"));
    let js = serde_json::to_string(&cmd).unwrap();
    assert_eq!(js, r#"{"UnadmitSymbol":{"venue":"binance","account":"ALT","symbol":"ETHUSDT.P"}}"#);
    assert_eq!(serde_json::from_str::<CommandWire>(&js).unwrap(), cmd);
}

/// The un-latch of a mount's budget (decision 0239) names its mount by id and no venue, has a pinned shape, and
/// the mount row's `latched` flag is ADDITIVE: absent from the bytes while false (the row an older node sends and
/// receives), present when the latch is spent.
#[test]
fn an_unlatch_names_a_mount_not_a_venue_and_the_row_flag_is_additive() {
    let cmd = CommandWire::UnlatchMount { mount_id: "binance__BTCUSDT__1m".into() };
    assert_eq!(cmd.addressed_venue(), None);
    let js = serde_json::to_string(&cmd).unwrap();
    assert_eq!(js, r#"{"UnlatchMount":{"mount_id":"binance__BTCUSDT__1m"}}"#);
    assert_eq!(serde_json::from_str::<CommandWire>(&js).unwrap(), cmd);

    let row = |latched| MountViewWire {
        order_client_id_code: None,
        strategy: "grid".into(),
        params: String::new(),
        live: true,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: "binance__BTCUSDT__1m".into(),
        typed_params: None,
        asset_class: None,
        latched,
        route_key: String::new(),
        tier: None,
        not_running: false,
    };
    let free = serde_json::to_string(&row(false)).unwrap();
    assert!(!free.contains("latched"), "an unlatched row writes no key: {free}");
    for added in ["route_key", "tier", "not_running"] {
        assert!(!free.contains(added), "a row that says nothing writes no `{added}`: {free}");
    }
    assert!(serde_json::to_string(&row(true)).unwrap().contains(r#""latched":true"#));
    // A row from a node that predates the flag decodes as not latched.
    let old: MountViewWire = serde_json::from_str(&free).unwrap();
    assert!(!old.latched);
    assert_eq!(old, row(false));
}

/// The three ADDRESS-LESS classes answer `None`. The UNSCOPED panic button is here deliberately:
/// a routing gate that could refuse it would be a kill switch with a prerequisite.
#[test]
fn the_address_less_variants_answer_none() {
    for cmd in [
        CommandWire::Cancel("c-1".into()),
        CommandWire::Modify {
            order_client_id: "c-1".into(),
            new_qty: None,
            new_price: None,
            new_trigger_price: None,
        },
        CommandWire::SetTradingState(TradingState::Halted),
        CommandWire::MassCancel { venue: None, symbol: None, account: None },
        CommandWire::MarketExit { venue: None, account: None },
        CommandWire::UnmountStrategy { controller_id: "m-1".into() },
        CommandWire::ReconcileConfirm { id: 9 },
        CommandWire::ReconcileCancel { id: 9 },
        CommandWire::UnlatchMount { mount_id: "m-1".into() },
        CommandWire::SetSetting {
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
    let js = r#"{"Submit":{"order_client_id":"c-1","symbol":"SYM","side":1,"qty":1.0,
                     "order_type":"limit","price":1.0}}"#;
    assert!(
        serde_json::from_str::<CommandWire>(js).is_err(),
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
        "free_buying_power":0.0,"trading_state":"Active","positions":[]}"#;
    let v: VenueBlockWire = serde_json::from_str(json).expect("an older node's block decodes");
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
    let back: VenueBlockWire = serde_json::from_str(&json).expect("decodes");
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
        let back: VenueBlockWire = serde_json::from_value(frame.clone())
            .unwrap_or_else(|e| panic!("`\"tier\": {unknown}` must decode as not said: {e}"));
        assert_eq!(
            back, expected,
            "`\"tier\": {unknown}` reads as not said and moves nothing else"
        );
    }
    frame.as_object_mut().expect("a block is an object").remove("tier");
    let back: VenueBlockWire = serde_json::from_value(frame.clone()).expect("decodes");
    assert_eq!(back, expected, "an absent key");
    for word in ["paper", "demo", "live", "quantum"] {
        frame["tier"] = serde_json::json!(word);
        let back: VenueBlockWire = serde_json::from_value(frame.clone()).expect("decodes");
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
        ArmingNotRunningViewWire {
            venue: "bybit".into(),
            account: None,
            route_key: "bybit".into(),
            tier: Some("live".into()),
            reason: "no live credentials".into(),
            detail: None,
            block_word: None,
        },
        ArmingNotRunningViewWire {
            venue: "okx".into(),
            account: Some("ALT".into()),
            route_key: "okx#ALT".into(),
            tier: None,
            reason: "two active tiers".into(),
            detail: None,
            block_word: None,
        },
    ];
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(json.contains("\"reason\":\"no live credentials\""), "{json}");
    let not_running = &serde_json::to_value(&snap).expect("serializes")["not_running"];
    assert!(not_running[0].get("account").is_none(), "the default account names no label");
    assert!(not_running[1].get("tier").is_none(), "no row states a tier: the key is absent");
    let back: CoreSnapshotWire = serde_json::from_str(&json).expect("decodes");
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
    let back: CoreSnapshotWire =
        serde_json::from_str(&json).expect("an older node's frame decodes");
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
    let back: CoreSnapshotWire =
        serde_json::from_str(&json).expect("an older node's frame decodes");
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
        value["balance_withheld"],
        serde_json::json!([{ "route_key": "oanda", "reason": "assumed USD" }])
    );
    snap.balance_withheld.clear();
    let json = serde_json::to_string(&snap).expect("serializes");
    assert!(!json.contains("balance_withheld"), "{json}");
    let back: CoreSnapshotWire =
        serde_json::from_str(&json).expect("an older node's frame decodes");
    assert!(back.balance_withheld.is_empty());
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
    let back: CoreSnapshotWire = serde_json::from_str(&with).expect("a new node's frame decodes");
    assert_eq!(back.venue_link_down, snap.venue_link_down);

    // An older OBSERVER: the snapshot as v0.1.91 declared it knows no such key, and serde ignores it (no
    // `deny_unknown_fields` on the frame). A reader of exactly the old keys it needs stands in for it.
    #[derive(serde::Deserialize)]
    struct Before {
        sequence: u64,
        venues: Vec<VenueBlockWire>,
        #[serde(default)]
        balance_withheld: Vec<BalanceWithheldWire>,
    }
    let old: Before = serde_json::from_str(&with).expect("an older observer reads a newer frame");
    assert_eq!((old.sequence, old.venues.len(), old.balance_withheld.len()), (42, 1, 1));

    // A NEWER observer against an OLDER node: the frame has no key and reads as nothing down.
    snap.venue_link_down.clear();
    let without = serde_json::to_string(&snap).expect("serializes");
    assert!(!without.contains("venue_link_down"), "an empty list is not written: {without}");
    let back: CoreSnapshotWire =
        serde_json::from_str(&without).expect("an older node's frame decodes");
    assert!(back.venue_link_down.is_empty());
}

/// **The readings of a session are additive both ways**: `no_adapter` is written only when true, an older node's frame
/// reads as not known (an empty `venue_path_up`, no `price_dials_up`, which is never zero), and `say` does not call a
/// missing adapter a path.
#[test]
fn the_session_readings_are_additive_and_a_missing_adapter_says_so() {
    let mut snap = full_snapshot();
    let with = serde_json::to_string(&snap).expect("serializes");
    let back: CoreSnapshotWire = serde_json::from_str(&with).expect("a new node's frame decodes");
    assert_eq!(back.venue_path_up, ["binance"]);
    assert_eq!(back.price_dials_up, Some(2));
    assert!(!back.venue_link_down[0].no_adapter);

    snap.venue_path_up.clear();
    snap.price_dials_up = None;
    let without = serde_json::to_string(&snap).expect("serializes");
    assert!(!without.contains("venue_path_up") && !without.contains("price_dials_up"), "{without}");
    let older: CoreSnapshotWire =
        serde_json::from_str(&without).expect("an older node's frame decodes");
    assert_eq!((older.venue_path_up.len(), older.price_dials_up), (0, None));

    let gone = VenueLinkDownWire {
        route_key: "ctrader".into(),
        why: "the bridge panicked".into(),
        failed_restarts: 0,
        retry_in_ms: 0,
        effect: "every order is refused".into(),
        no_adapter: true,
    };
    assert_eq!(
        gone.say(),
        "the gatewayhub has NO LIVE ADAPTER for this account (the bridge panicked): every order is refused"
    );
    let value = serde_json::to_value(&gone).expect("serializes");
    assert_eq!(value["no_adapter"], true);
    assert!(
        serde_json::to_value(VenueLinkDownWire { no_adapter: false, ..gone })
            .expect("serializes")
            .get("no_adapter")
            .is_none(),
        "absent when false"
    );
}

/// **The two hub links and a NOT RUNNING row's cause are additive both ways**: a frame with every link up carries no
/// key, so it is byte-identical to an older node's; `closed` and `detail` are written only when they say something; a
/// frame from an older node decodes as nothing listed; and an older observer ignores the keys.
#[test]
fn the_hub_links_and_the_cause_of_a_refused_start_are_additive_both_ways() {
    let mut snap = full_snapshot();
    let value = serde_json::to_value(&snap).expect("serializes");
    assert_eq!(
        value["trade_link_down"],
        serde_json::json!([{
            "route_key": "binance",
            "why": "the gatewayhub is stopping",
            "down_ms": 1_500
        }]),
        "a link that redials writes no `closed`"
    );
    assert_eq!(
        value["price_link_down"],
        serde_json::json!([{
            "venue": "bybit",
            "why": "connection refused",
            "down_ms": 2_500,
            "of": 2
        }])
    );
    let with = serde_json::to_string(&snap).expect("serializes");
    let back: CoreSnapshotWire = serde_json::from_str(&with).expect("a new node's frame decodes");
    assert_eq!(back.trade_link_down, snap.trade_link_down);
    assert_eq!(back.price_link_down, snap.price_link_down);

    #[derive(serde::Deserialize)]
    struct Before {
        sequence: u64,
    }
    let old: Before = serde_json::from_str(&with).expect("an older observer reads a newer frame");
    assert_eq!(old.sequence, 42);

    snap.trade_link_down[0].closed = true;
    let closed = serde_json::to_value(&snap).expect("serializes");
    assert_eq!(closed["trade_link_down"][0]["closed"], true);

    snap.trade_link_down.clear();
    snap.price_link_down.clear();
    let without = serde_json::to_string(&snap).expect("serializes");
    assert!(!without.contains("trade_link_down"), "no list, no key: {without}");
    assert!(!without.contains("price_link_down"), "no list, no key: {without}");
    let back: CoreSnapshotWire =
        serde_json::from_str(&without).expect("an older node's frame decodes");
    assert!(back.trade_link_down.is_empty() && back.price_link_down.is_empty());

    let row = ArmingNotRunningViewWire {
        venue: "ibkr".into(),
        account: None,
        route_key: "ibkr".into(),
        tier: None,
        reason: "adapter failed".into(),
        detail: None,
        block_word: None,
    };
    let text = serde_json::to_string(&row).expect("serializes");
    assert!(!text.contains("detail"), "no cause, no key: {text}");
    let row = ArmingNotRunningViewWire { detail: Some("held by another client".into()), ..row };
    let text = serde_json::to_string(&row).expect("serializes");
    assert!(text.contains("\"detail\":\"held by another client\""), "{text}");
    let back: ArmingNotRunningViewWire = serde_json::from_str(&text).expect("decodes");
    assert_eq!(back, row);
}

/// **A NOT RUNNING row's block KEY is additive both ways**: a row no block stopped is byte-identical to an older
/// node's (no key), an older node's row decodes with none, and an older observer ignores the key it does not know.
#[test]
fn the_block_word_of_a_not_running_row_is_additive_both_ways() {
    let row = ArmingNotRunningViewWire {
        venue: "okx".into(),
        account: None,
        route_key: "okx".into(),
        tier: Some("demo".into()),
        reason: "account inactive; position open at venue: BTCUSDT 1".into(),
        block_word: Some(ArmingBlock::AccountInactive),
        detail: None,
    };
    let text = serde_json::to_string(&row).expect("serializes");
    assert!(text.contains("\"block_word\":\"account_inactive\""), "{text}");
    let back: ArmingNotRunningViewWire =
        serde_json::from_str(&text).expect("a new node's row decodes");
    assert_eq!(back, row);

    #[derive(serde::Deserialize)]
    struct Before {
        reason: String,
    }
    let old: Before = serde_json::from_str(&text).expect("an older observer reads a newer row");
    assert_eq!(old.reason, row.reason);

    let unstated = ArmingNotRunningViewWire { block_word: None, ..row };
    let text = serde_json::to_string(&unstated).expect("serializes");
    assert!(!text.contains("block_word"), "no block, no key: {text}");
    let back: ArmingNotRunningViewWire =
        serde_json::from_str(&text).expect("an older node's row decodes");
    assert_eq!(back, unstated);
}

/// **The typed block crosses in the bytes the string did**: for EVERY `ArmingBlock`, the row writes exactly what a
/// row carrying `ArmingBlock::word` as a plain string wrote (the field's form before it was typed), byte for byte,
/// and reads back as that block. A word this build does not know reads as `AdapterFailed` and a value that is not a
/// word as not said: neither fails the frame.
#[test]
fn the_block_word_crosses_as_the_blocks_key_byte_for_byte() {
    #[derive(serde::Serialize)]
    struct AsString<'a> {
        venue: &'a str,
        route_key: &'a str,
        reason: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        block_word: Option<&'a str>,
    }
    let row = |block_word| ArmingNotRunningViewWire {
        venue: "okx".into(),
        account: None,
        route_key: "okx".into(),
        tier: None,
        reason: "why".into(),
        block_word,
        detail: None,
    };
    for block in ArmingBlock::ALL {
        let text = serde_json::to_string(&row(Some(block))).expect("serializes");
        let as_string = AsString {
            venue: "okx",
            route_key: "okx",
            reason: "why",
            block_word: Some(block.word()),
        };
        assert_eq!(text, serde_json::to_string(&as_string).expect("serializes"), "{block:?}");
        let back: ArmingNotRunningViewWire = serde_json::from_str(&text).expect("decodes");
        assert_eq!(back.block_word, Some(block), "{text}");
    }
    assert_eq!(
        serde_json::to_string(&row(Some(ArmingBlock::NoCredentials))).expect("serializes"),
        r#"{"venue":"okx","route_key":"okx","reason":"why","block_word":"no_credentials"}"#,
    );

    let read = |block_word: &str| {
        let text = format!(
            r#"{{"venue":"okx","route_key":"okx","reason":"why","block_word":{block_word}}}"#
        );
        serde_json::from_str::<ArmingNotRunningViewWire>(&text)
            .expect("never fails the frame")
            .block_word
    };
    assert_eq!(
        read(r#""a_newer_block""#),
        Some(ArmingBlock::AdapterFailed),
        "unknown: a cause it cannot name"
    );
    assert_eq!(read("7"), None, "not a word: not said");
    assert_eq!(read("null"), None);
}

/// **`core_held_stops` is a READING of the node, additive both ways** (decision 0112 verdict 3): a frame with none
/// armed is byte-identical to an older node's (no key), a frame without the key decodes as an empty list, an older
/// observer ignores the key, a `false` flag and a zero count are never written, and every surface words a stop
/// through [`OrderEmulationStopViewWire::say`].
#[test]
fn core_held_stops_are_additive_and_old_and_new_observers_read_each_other() {
    let mut snap = full_snapshot();
    let value = serde_json::to_value(&snap).expect("serializes");
    assert_eq!(
        value["core_held_stops"],
        serde_json::json!([{
            "arm_id": "s0a1", "venue": "polymarket", "symbol": "TOKEN", "account": "polymarket",
            "side": -1, "qty": 5.0, "trigger_price": 0.4, "held": true, "rearms": 1
        }])
    );
    let with = serde_json::to_string(&snap).expect("serializes");
    let back: CoreSnapshotWire = serde_json::from_str(&with).expect("a new node's frame decodes");
    assert_eq!(back.core_held_stops, snap.core_held_stops);

    // An older OBSERVER ignores the key (no `deny_unknown_fields` on the frame).
    #[derive(serde::Deserialize)]
    struct Before {
        sequence: u64,
        venues: Vec<VenueBlockWire>,
    }
    let old: Before = serde_json::from_str(&with).expect("an older observer reads a newer frame");
    assert_eq!((old.sequence, old.venues.len()), (42, 1));

    // A plain stop writes neither flag, and an older node's frame (no key) reads as none armed.
    snap.core_held_stops[0].held = false;
    snap.core_held_stops[0].rearms = 0;
    let plain = serde_json::to_value(&snap).expect("serializes");
    let row = plain["core_held_stops"][0].as_object().expect("a row");
    assert!(!row.contains_key("held") && !row.contains_key("rearms"), "{row:?}");
    snap.core_held_stops.clear();
    let without = serde_json::to_string(&snap).expect("serializes");
    assert!(!without.contains("core_held_stops"), "an empty list is not written: {without}");
    let back: CoreSnapshotWire =
        serde_json::from_str(&without).expect("an older node's frame decodes");
    assert!(back.core_held_stops.is_empty());
}

/// A working emulated order: a chase with a live child, part filled.
fn emulated_chase() -> OrderEmulationViewWire {
    OrderEmulationViewWire {
        id: "c-77".into(),
        venue: "bybit".into(),
        account: Some("ALT".into()),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        kind: "chase".into(),
        reduce_only: false,
        trigger: None,
        price: None,
        trail: None,
        peg: Some(OrderPegWire { anchor: "mid".into(), offset: 0.5 }),
        display_quantity: None,
        filled: 0.4,
        average_price: 60_010.0,
        child: Some("c-77-3".into()),
        waiting: None,
        rearms: 0,
        held: false,
    }
}

/// Only a reduce-only TRIGGER row is a core-held stop: a working row (no trigger) and a trigger row that may open
/// are not, and a labelled account reads as its route key.
#[test]
fn only_a_reduce_only_trigger_row_is_a_core_held_stop() {
    assert_eq!(emulated_chase().as_core_held_stop(), None, "a working row");
    let stop = OrderEmulationViewWire {
        kind: "stop".into(),
        trigger: Some(95.0),
        child: None,
        ..emulated_chase()
    };
    assert_eq!(stop.as_core_held_stop(), None, "a trigger row that may open");
    // HELD is the core's own word, never read off the waiting words, which may grow.
    let waits = OrderEmulationViewWire {
        reduce_only: true,
        waiting: Some("some other reason".into()),
        ..stop.clone()
    };
    assert!(!waits.as_core_held_stop().expect("a stop").held, "a waiting word is not the hold");
    let marked = OrderEmulationViewWire { reduce_only: true, held: true, ..stop.clone() };
    assert!(marked.as_core_held_stop().expect("a stop").held, "the core's word is");
    let held =
        OrderEmulationViewWire { reduce_only: true, ..stop }.as_core_held_stop().expect("a stop");
    assert_eq!((held.account.as_str(), held.held, held.rearms), ("bybit#ALT", false, 0));
}

/// The emulated list is a READING (decision 0112): a row writes only the terms its kind states, an empty list
/// writes no key (an older node's frame reads as none), and an older observer ignores the key.
#[test]
fn emulated_orders_are_additive_and_old_and_new_observers_read_each_other() {
    let mut snap = full_snapshot();
    let value = serde_json::to_value(&snap).expect("serializes");
    assert_eq!(
        value["emulated_orders"],
        serde_json::json!([{
            "id": "c-77", "venue": "bybit", "account": "ALT", "symbol": "BTCUSDT", "side": 1, "qty": 1.0,
            "kind": "chase", "peg": {"anchor": "mid", "offset": 0.5}, "filled": 0.4, "average_price": 60_010.0,
            "child": "c-77-3"
        }])
    );
    let with = serde_json::to_string(&snap).expect("serializes");
    let back: CoreSnapshotWire = serde_json::from_str(&with).expect("a new node's frame decodes");
    assert_eq!(back.emulated_orders, snap.emulated_orders);
    // The peg is a READING: a member a newer node adds inside it is not said, never a failed frame.
    let peg = r#""peg":{"anchor":"mid","offset":0.5}"#;
    assert!(with.contains(peg), "{with}");
    let newer = with.replace(peg, r#""peg":{"anchor":"mid","offset":0.5,"a_later_member":1}"#);
    let read: CoreSnapshotWire = serde_json::from_str(&newer).expect("the frame still decodes");
    assert_eq!(read.emulated_orders[0].peg, None, "a peg it cannot read is not said");
    assert_eq!(
        read.emulated_orders[0].child.as_deref(),
        Some("c-77-3"),
        "the rest of the row reads"
    );

    #[derive(serde::Deserialize)]
    struct Before {
        sequence: u64,
    }
    let old: Before = serde_json::from_str(&with).expect("an older observer reads a newer frame");
    assert_eq!(old.sequence, 42);

    // A trigger kind: the default account, a reduce-only stop with its trigger, nothing filled.
    snap.emulated_orders = vec![OrderEmulationViewWire {
        id: "s0a1".into(),
        account: None,
        kind: "stop".into(),
        reduce_only: true,
        trigger: Some(95.0),
        peg: None,
        filled: 0.0,
        average_price: 0.0,
        child: None,
        waiting: Some("restored: held until its account's first reconcile pass".into()),
        held: true,
        ..emulated_chase()
    }];
    let row = serde_json::to_value(&snap).expect("serializes")["emulated_orders"][0].clone();
    let keys: Vec<&str> = row.as_object().expect("a row").keys().map(String::as_str).collect();
    for absent in ["account", "price", "trail", "peg", "display_quantity", "child", "rearms"] {
        assert!(!keys.contains(&absent), "{absent} in {row}");
    }
    assert_eq!((row["reduce_only"].as_bool(), row["trigger"].as_f64()), (Some(true), Some(95.0)));
    // ...and that row IS the core-held stop the operator surfaces list, once: held as the core states it.
    snap.emulated_orders[0].rearms = 2;
    assert_eq!(
        snap.core_held_stops_listed(),
        vec![OrderEmulationStopViewWire {
            arm_id: "s0a1".into(),
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            account: "bybit".into(),
            side: 1,
            qty: 1.0,
            trigger_price: 95.0,
            held: true,
            rearms: 2,
        }]
    );
    let back: CoreSnapshotWire =
        serde_json::from_str(&serde_json::to_string(&snap).unwrap()).unwrap();
    assert_eq!(back.emulated_orders[0].rearms, 2, "a re-armed row says so on the wire");

    snap.emulated_orders.clear();
    let without = serde_json::to_string(&snap).expect("serializes");
    assert!(!without.contains("emulated_orders"), "an empty list is not written: {without}");
    let back: CoreSnapshotWire =
        serde_json::from_str(&without).expect("an older node's frame decodes");
    assert!(back.emulated_orders.is_empty());
}

#[test]
fn a_core_held_stop_is_worded_with_its_id_side_trigger_account_and_what_marks_it_out() {
    let stop = OrderEmulationStopViewWire {
        arm_id: "s0a1".into(),
        venue: "ig".into(),
        symbol: "CS.D.EURUSD.CFD.IP".into(),
        account: "ig#ALT".into(),
        side: -1,
        qty: 2.0,
        trigger_price: 1.05,
        held: false,
        rearms: 0,
    };
    let said = stop.say();
    for part in ["s0a1", "SELL 2", "CS.D.EURUSD.CFD.IP", "crosses 1.05", "ig#ALT"] {
        assert!(said.contains(part), "{part:?} missing from {said}");
    }
    assert!(!said.contains("HELD") && !said.contains("re-armed"), "{said}");
    let marked = OrderEmulationStopViewWire { held: true, rearms: 2, side: 1, ..stop }.say();
    assert!(
        marked.contains("BUY 2") && marked.contains("HELD") && marked.contains("re-armed 2"),
        "{marked}"
    );
}

/// Every operator surface words an outage through [`VenueLinkDownWire::say`]: the reason, what failed, what to expect.
#[test]
fn a_venue_path_outage_says_the_reason_the_failures_and_what_to_expect() {
    let down = VenueLinkDownWire {
        route_key: "dukascopy".into(),
        why: "JForex session disconnected".into(),
        failed_restarts: 2,
        retry_in_ms: 20_000,
        effect: String::new(),
        no_adapter: false,
    };
    let own_words = VenueLinkDownWire { effect: "orders still go out".to_string(), ..down.clone() };
    assert!(own_words.say().ends_with("; orders still go out"), "{}", own_words.say());
    assert!(!own_words.say().contains("refused"), "a stream's loss refuses no order");
    let said = down.say();
    for part in [
        "the venue path is DOWN: JForex session disconnected",
        "2 restart(s) failed so far",
        "the next try in about 20 s",
        "orders are refused until its bridge has it back",
    ] {
        assert!(said.contains(part), "{part:?} missing from {said}");
    }
    let nameless = VenueLinkDownWire { why: String::new(), retry_in_ms: 1, ..down };
    assert!(nameless.say().contains("no reason given") && nameless.say().contains("about 1 s"));
}

/// **The outage promises no try the bridge did not state**: a lane that has ended states failures and no wait (OANDA's
/// give-up), a cTrader session being opened states neither. A wait with no failure yet is a real number, and a Dukascopy
/// frame reads byte for byte as it always did. Reddens on "the next try in about 0 s" coming back, on the count going
/// with the clause when it has something to say, and on any change to the frame with numbers.
#[test]
fn a_venue_path_outage_promises_no_try_nobody_stated() {
    let frame = |failed_restarts, retry_in_ms, effect: &str| VenueLinkDownWire {
        route_key: "x".into(),
        why: "why".into(),
        failed_restarts,
        retry_in_ms,
        effect: effect.to_string(),
        no_adapter: false,
    };
    let eff = "orders still go out";
    let cases = [
        (
            frame(4, 0, eff),
            "the venue path is DOWN: why (4 restart(s) failed so far); orders still go out",
        ),
        (frame(0, 0, eff), "the venue path is DOWN: why; orders still go out"),
        (
            frame(0, 5_000, ""),
            "the venue path is DOWN: why (0 restart(s) failed so far, the next try in about 5 s); \
             orders are refused until its bridge has it back",
        ),
        (
            frame(2, 20_000, ""),
            "the venue path is DOWN: why (2 restart(s) failed so far, the next try in about 20 s); \
             orders are refused until its bridge has it back",
        ),
    ];
    for (down, said) in cases {
        assert_eq!(down.say(), said);
    }
}

/// Every operator surface words the hold through [`BalanceWithheldWire::line`]: the account, the exact reason words the
/// node's gate states, and what the hold means for the account's equity.
#[test]
fn a_held_balance_line_names_the_account_the_reason_and_the_consequence() {
    let line =
        BalanceWithheldWire { route_key: "oanda#ALT".into(), reason: "assumed USD".into() }.line();
    assert!(line.starts_with("oanda#ALT: balance held: assumed USD"), "{line}");
    assert!(line.contains("not adopted") && line.contains("seed + PnL"), "{line}");
    let held = BalanceWithheldWire { route_key: "oanda#ALT".into(), reason: "assumed USD".into() };
    assert_eq!(held.line(), format!("oanda#ALT: {}", held.say()));
    assert_eq!(held.say(), format!("balance held: assumed USD - {BALANCE_HELD_MEANING}"));
}

/// An order from a node that predates `OrderViewWire::account` decodes as "named no account".
#[test]
fn an_order_without_an_account_key_still_decodes() {
    let json = r#"{"order_client_id":"c-1","venue":"binance","symbol":"BTCUSDT","side":1,
        "qty":0.5,"order_type":"limit","price":59000.0,"trigger_price":null,"status":"Accepted",
        "venue_order_id":null,"filled_qty":0.0,"average_fill_price":0.0}"#;
    let o: OrderViewWire = serde_json::from_str(json).expect("an older node's order decodes");
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
    let back: OrderViewWire = serde_json::from_str(&json).expect("decodes");
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
        serde_json::from_str::<CommandWire>(js).is_err(),
        "an unknown key on a bracket is an error, never silently dropped"
    );
    // POSITIVE CONTROL: the same frame minus the key decodes (explicit `null` = MARKET entry).
    let js = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,
                 "entry_price":null,"stop_loss":95.0,"take_profit":110.0}}"#;
    match serde_json::from_str::<CommandWire>(js).expect("the account-less frame decodes") {
        CommandWire::Bracket(b) => assert_eq!(b.entry_price, None, "an explicit null is market"),
        other => panic!("expected a Bracket, got {other:?}"),
    }
}

/// **A bracket names its account on the wire, and an OLD peer's frame still parses.** The key is
/// written only when set (so an account-less bracket is byte-identical to the pre-field frame and a
/// node from before the field still decodes it), a frame without the key reads `None`, and a named
/// one round-trips whatever the spelling (`DEFAULT` included, which is a name, not an absence).
#[test]
fn a_bracket_names_its_account_only_when_it_has_one() {
    let spec = |account: Option<&str>| OrderBracketWire {
        order_client_id: None,
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
    let bare = serde_json::to_string(&CommandWire::Bracket(spec(None))).expect("serializes");
    assert!(!bare.contains("account"), "an account-less bracket writes no key: {bare}");
    let old = r#"{"Bracket":{"venue":"binance","symbol":"BTCUSDT.P","side":1,"qty":1.0,
                  "entry_price":100.0,"stop_loss":95.0,"take_profit":110.0}}"#;
    assert_eq!(
        serde_json::from_str::<CommandWire>(old).expect("an old-peer frame decodes"),
        CommandWire::Bracket(spec(None)),
        "…as the account-less bracket"
    );
    // With: the key is written and survives the round trip, `DEFAULT` and a label alike.
    for named in ["PERP", "DEFAULT"] {
        let sent = CommandWire::Bracket(spec(Some(named)));
        let json = serde_json::to_string(&sent).expect("serializes");
        assert!(json.contains(&format!("\"account\":\"{named}\"")), "{json}");
        assert_eq!(serde_json::from_str::<CommandWire>(&json).expect("decodes"), sent);
    }
}

/// ⚠ **DECLARED, pinned: a NON-FINITE entry price round-trips to a MARKET entry** (`serde_json`
/// writes NaN/inf as `null`); the guard is the SENDER's (`OrderBracketWire::entry_price`'s doc).
#[test]
fn a_non_finite_entry_price_round_trips_to_a_market_entry() {
    for px in [f64::NAN, f64::INFINITY] {
        let sent = CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
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
        match serde_json::from_slice::<CommandWire>(&bytes).expect("decodes") {
            CommandWire::Bracket(b) => assert_eq!(b.entry_price, None, "{px} arrives as market"),
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
        serde_json::from_str::<CommandWire>(js).is_err(),
        "a missing entry_price key must not decode as a market entry"
    );
}

/// The venue is REQUIRED, as on a `Submit`: the node routes by it and nothing else.
#[test]
fn a_bracket_body_without_a_venue_key_does_not_decode() {
    let js = r#"{"Bracket":{"symbol":"BTCUSDT","side":1,"qty":1.0,
                 "entry_price":null,"stop_loss":95.0,"take_profit":110.0}}"#;
    assert!(serde_json::from_str::<CommandWire>(js).is_err());
}

/// A MARKET entry is written as an explicit `"entry_price": null`, never a missing key.
#[test]
fn a_market_entry_bracket_writes_its_entry_price_as_null() {
    let v = serde_json::to_value(CommandWire::Bracket(OrderBracketWire {
        order_client_id: None,
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
    let decoded: CommandWire = serde_json::from_value(old_frame.clone()).unwrap();
    let CommandWire::UpdateParams { mount_id, .. } = &decoded else {
        panic!("decodes as UpdateParams");
    };
    assert_eq!(*mount_id, None, "a key-less frame is the unaddressed shape");
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        old_frame,
        "…and re-encodes to the same bytes"
    );

    let addressed = CommandWire::UpdateParams {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: Some("maker_b".into()),
        params: serde_json::json!({}),
    };
    let wire = serde_json::to_value(&addressed).unwrap();
    assert_eq!(wire["UpdateParams"]["mount_id"], "maker_b");
    assert_eq!(serde_json::from_value::<CommandWire>(wire).unwrap(), addressed);

    let old_row = serde_json::json!({
        "strategy": "spread_maker", "params": "qty=1", "live": false,
        "venue": "binance", "symbol": "BTCUSDT", "interval": "1m"
    });
    let row: MountViewWire = serde_json::from_value(old_row).unwrap();
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
            r#"{{"order_client_id":"c-1","venue":"sim","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"{word}","price":2.0,"trigger_price":null,"reduce_only":false}}"#
        );
        let req: OrderRequestWire = serde_json::from_str(&js).unwrap();
        assert_eq!(req.order_type, word);
        assert_eq!(serde_json::to_string(&req).unwrap(), js, "{word:?}: bytes unchanged");
    }
}

/// The two credential verbs of decision 0144's wire half: the retire carries the typed confirm the
/// node refuses without (equal to the key), the presence read carries none, and neither prints
/// anything but key NAMES in a `Debug`.
#[test]
fn the_retire_and_presence_verbs_state_their_ceremony_and_print_names_only() {
    let retire = AccountVerbWire::RetireCredential { key: "SOME_KEY".into() };
    assert_eq!(retire.word(), "retire-credential");
    assert_eq!(retire.required_confirm().as_deref(), Some("SOME_KEY"));
    let held = AccountVerbWire::HeldCredentials { names: vec!["A".into(), "B".into()] };
    assert_eq!(held.word(), "held-credentials");
    assert_eq!(held.required_confirm(), None, "a read owes no ceremony");

    let req = AccountRequestWire { verb: retire, confirm: Some("SOME_KEY".into()) };
    assert_eq!(
        format!("{req:?}"),
        "AccountRequestWire(retire-credential key=SOME_KEY confirm=typed)"
    );
    let req = AccountRequestWire { verb: held, confirm: None };
    assert_eq!(format!("{req:?}"), "AccountRequestWire(held-credentials names=A,B confirm=absent)");
}

/// Both new verbs survive the wire's JSON round trip, and a frame WITHOUT the `confirm` key still
/// decodes (`#[serde(default)]`), so the presence read needs no field.
#[test]
fn the_retire_and_presence_requests_round_trip() {
    for req in [
        AccountRequestWire {
            verb: AccountVerbWire::RetireCredential { key: "SOME_KEY".into() },
            confirm: Some("SOME_KEY".into()),
        },
        AccountRequestWire {
            verb: AccountVerbWire::HeldCredentials { names: vec!["A".into()] },
            confirm: None,
        },
    ] {
        let js = serde_json::to_string(&req).unwrap();
        let back: AccountRequestWire = serde_json::from_str(&js).unwrap();
        assert_eq!(req, back);
    }
    let bare: AccountRequestWire =
        serde_json::from_str(r#"{"verb":{"HeldCredentials":{"names":["A"]}}}"#).unwrap();
    assert_eq!(bare.confirm, None);
}

/// `OrderViewWire::reason` is ADDITIVE: an order from a node that predates it decodes as "no
/// reason", an order with none puts no `reason` key on the wire (every frame stays byte-identical to
/// before), and a refused order carries its words and round-trips. A reader of an older build
/// ignores the key (`OrderViewWire` has no `deny_unknown_fields`).
#[test]
fn an_order_reason_is_additive_on_the_wire() {
    let old = r#"{"order_client_id":"c-1","venue":"binance","symbol":"BTCUSDT","side":1,
        "qty":0.5,"order_type":"limit","price":59000.0,"trigger_price":null,"status":"REJECTED",
        "venue_order_id":null,"filled_qty":0.0,"average_fill_price":0.0}"#;
    let o: OrderViewWire = serde_json::from_str(old).expect("an older node's order decodes");
    assert_eq!(o.reason, None);

    let mut o = full_snapshot().orders.remove(0);
    o.reason = None;
    let json = serde_json::to_string(&o).expect("serializes");
    assert!(!json.contains("\"reason\""), "{json}");

    o.reason = Some("INVALID_REQUEST: more digits than symbol allows".into());
    let json = serde_json::to_string(&o).expect("serializes");
    assert!(json.contains("\"reason\":\"INVALID_REQUEST"), "{json}");
    let back: OrderViewWire = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, o);
}

/// `OrderViewWire::age_ms` is ADDITIVE the same way: an order from a node that predates it decodes
/// as "no age" (never `0`: that would call every order new), an order with none puts no key on the
/// wire, and a reading round-trips, a zero included (an order the node saw a moment ago).
#[test]
fn an_order_age_is_additive_on_the_wire() {
    let old = r#"{"order_client_id":"c-1","venue":"binance","symbol":"BTCUSDT","side":1,
        "qty":0.5,"order_type":"limit","price":59000.0,"trigger_price":null,"status":"ACCEPTED",
        "venue_order_id":null,"filled_qty":0.0,"average_fill_price":0.0}"#;
    let o: OrderViewWire = serde_json::from_str(old).expect("an older node's order decodes");
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
        let back: OrderViewWire = serde_json::from_str(&json).expect("decodes");
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
    let back: CoreSnapshotWire = serde_json::from_str(&json).expect("decodes");
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
    let back: CoreSnapshotWire =
        serde_json::from_str(&json).expect("an older node's frame decodes");
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
    v.contract_values = vec![ContractValueWire {
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
    let back: VenueBlockWire = serde_json::from_str(&json).expect("decodes");
    assert_eq!(back, v);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_buying_power":0.0,"trading_state":"Active","positions":[]}"#;
    let v: VenueBlockWire = serde_json::from_str(older).expect("an older node's block decodes");
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
    let back: VenueBlockWire = serde_json::from_str(&on).expect("decodes");
    assert!(back.admits_symbols);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_buying_power":0.0,"trading_state":"Active","positions":[],"tier":"live"}"#;
    let v: VenueBlockWire = serde_json::from_str(older).expect("an older node's block decodes");
    assert!(!v.admits_symbols, "an older node offers nothing");

    // The OLD-client half: a reader that predates a key ignores it (no `deny_unknown_fields`), which
    // is how a desktop built before this field reads a current node's block.
    let newer = older.replacen('{', r#"{"admits_symbols":true,"a_key_nobody_reads_yet":1,"#, 1);
    assert!(serde_json::from_str::<VenueBlockWire>(&newer).is_ok());
}

/// The ONE wording of a conversion rate: the figure, then STALE when the node judged it so, then what it
/// means when a position is open in the currency; a rate with no usable figure says so.
#[test]
fn a_conversion_rate_reads_alike_on_every_surface() {
    assert_eq!(quote_rate_phrase("JPY", 1.0 / 150.0, false, false, "USD"), "1 JPY = 0.006667 USD");
    assert_eq!(quote_rate_phrase("JPY", 0.0066, true, false, "USD"), "1 JPY = 0.006600 USD STALE");
    assert_eq!(
        quote_rate_phrase("USD", 0.862_069, true, true, "EUR"),
        "1 USD = 0.862069 EUR STALE (an open position is valued at it)"
    );
    // `held` is only ever said of a stale rate
    assert_eq!(quote_rate_phrase("USD", 0.862_069, false, true, "EUR"), "1 USD = 0.862069 EUR");
    assert_eq!(quote_rate_phrase("CAD", f64::NAN, true, false, ""), "1 CAD = no usable rate STALE");
    let row = QuoteRateViewWire {
        currency: "GBP".into(),
        rate: 1.35,
        as_of_ms: 1,
        stale: false,
        held: false,
    };
    assert_eq!(row.say("USD"), "1 GBP = 1.350000 USD");
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
        QuoteRateViewWire {
            currency: "GBP".into(),
            rate: 1.35,
            as_of_ms: 1_700_000_000_000,
            stale: false,
            held: false,
        },
        QuoteRateViewWire {
            currency: "JPY".into(),
            rate: 1.0 / 150.0,
            as_of_ms: 1_699_999_000_000,
            stale: true,
            held: true,
        },
    ];
    v.quote_rates_in = "USD".into();
    let on = serde_json::to_string(&v).expect("serializes");
    assert!(on.contains(r#""quote_rates_in":"USD""#) && on.contains(r#""currency":"GBP""#), "{on}");
    // `held` is written only when true (a row that holds nothing is the row an older node writes), and
    // an older node's row, which has no key, reads false
    assert_eq!(on.matches(r#""held":true"#).count(), 1, "{on}");
    assert!(!on.contains(r#""held":false"#), "{on}");
    let no_key: QuoteRateViewWire =
        serde_json::from_str(r#"{"currency":"JPY","rate":0.0066,"as_of_ms":1,"stale":true}"#)
            .expect("an older node's row decodes");
    assert!(no_key.stale && !no_key.held);
    let back: VenueBlockWire = serde_json::from_str(&on).expect("decodes");
    assert_eq!(back, v);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_buying_power":0.0,"trading_state":"Active","positions":[],"tier":"live"}"#;
    let old: VenueBlockWire = serde_json::from_str(older).expect("an older node's block decodes");
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
fn ledger_currency_is_absent_for_the_dollar_group_and_an_older_block_reads_it_empty() {
    let mut v = full_snapshot().venues.remove(0);
    let off = serde_json::to_string(&v).expect("serializes");
    assert!(!off.contains("ledger_currency"), "the dollar group writes no key: {off}");
    v.ledger_currency = "EUR".into();
    let on = serde_json::to_string(&v).expect("serializes");
    assert!(on.contains(r#""ledger_currency":"EUR""#), "{on}");
    let back: VenueBlockWire = serde_json::from_str(&on).expect("decodes");
    assert_eq!(back, v);

    let older = r#"{"venue":"binance","balance":0.0,"realized_pnl":0.0,"fees_paid":0.0,
        "funding_paid":0.0,"equity":0.0,"unrealized":0.0,"missing_prices":0,"margin_used":0.0,
        "free_buying_power":0.0,"trading_state":"Active","positions":[],"tier":"live"}"#;
    let old: VenueBlockWire = serde_json::from_str(older).expect("an older node's block decodes");
    assert!(old.ledger_currency.is_empty(), "no key reads as the dollar group");

    // The OLD-client half: a reader without the field ignores the key (no `deny_unknown_fields`).
    #[derive(serde::Deserialize)]
    struct BeforeTheField {
        venue: String,
    }
    let before: BeforeTheField = serde_json::from_str(&on).expect("an old reader ignores the key");
    assert_eq!(before.venue, "binance");
}

/// **That the ledger currency is only an assumed USD rides the venue block** (decision 0189, A52) under decision
/// 0112 verdict 3, with no feature string: ABSENT when false (a block that assumes nothing is the block an older
/// node writes, byte for byte), written when true and round-tripped, read as `false` from an older node's block,
/// and ignored by a reader that predates it.
#[test]
fn currency_assumed_is_absent_when_false_and_an_older_block_reads_it_false() {
    let mut v = full_snapshot().venues.remove(0);
    let off = serde_json::to_string(&v).expect("serializes");
    assert!(!off.contains("currency_assumed"), "false writes no key: {off}");
    v.currency_assumed = true;
    let on = serde_json::to_string(&v).expect("serializes");
    assert!(on.contains(r#""currency_assumed":true"#), "{on}");
    assert_eq!(serde_json::from_str::<VenueBlockWire>(&on).expect("decodes"), v);
    let older = on.replace(r#","currency_assumed":true"#, "");
    assert!(
        !serde_json::from_str::<VenueBlockWire>(&older).expect("an older block").currency_assumed,
        "no key reads as stated (or not anchored at all)"
    );

    #[derive(serde::Deserialize)]
    struct BeforeTheField {
        venue: String,
    }
    let before: BeforeTheField = serde_json::from_str(&on).expect("an old reader ignores the key");
    assert_eq!(before.venue, "binance");
}

/// ⚠ A HELD row whose release reads another venue's price decodes in a client from BEFORE the attributes wave: the
/// venue rides the row as `release_venue`, beside the nested release, so the older `OrderReleaseWire` (which refuses an
/// unknown key) never meets it. The older shapes are copied here.
#[test]
fn an_older_client_decodes_a_held_row_whose_release_reads_another_venue() {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "the older shape is decoded, not read")]
    struct OldRelease {
        at: i64,
        #[serde(default)]
        price: Option<OrderReleasePriceWire>,
        #[serde(default)]
        symbol: Option<String>,
    }
    #[derive(Deserialize)]
    #[expect(dead_code, reason = "the older shape is decoded, not read")]
    struct OldHeld {
        order_client_id: String,
        #[serde(default)]
        release: Option<OldRelease>,
    }
    let held = OrderHeldViewWire {
        order_client_id: "c-x".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        trigger_price: None,
        parent_order_id: None,
        account: None,
        release: Some(OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePriceWire::Above(100_000.0)),
            symbol: Some("BTCUSDT".into()),
            venue: None,
        }),
        waiting: Some("no price of BTCUSDT on binance on this node yet".into()),
        release_venue: Some("binance".into()),
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    };
    let json = serde_json::to_string(&held).expect("serialises");
    assert!(json.contains(r#""release_venue":"binance""#), "{json}");
    let old: OldHeld = serde_json::from_str(&json).expect("an older client decodes the row");
    assert_eq!(old.release.map(|r| r.at), Some(4_097_217_600_000));
    let back: OrderHeldViewWire = serde_json::from_str(&json).expect("round trip");
    assert_eq!(back, held);
}

/// A resting order's attributes-wave READINGS ride the order row, and a member a NEWER node adds inside one reads as
/// "not said" instead of failing the frame (`or_not_said`).
#[test]
fn an_order_rows_attribute_readings_never_fail_the_frame() {
    let mut row = full_snapshot().orders.remove(0);
    row.attributes = Some(OrderAttributes { all_or_none: true, ..Default::default() });
    row.trail_percent = Some(1.5);
    row.adjust = Some(OrderStopAdjust { at: 120.0, to: OrderStopAdjustTo::Stop { stop: 110.0 } });
    row.release = Some(OrderReleaseWire { at: 1, price: None, symbol: None, venue: None });
    let json = serde_json::to_string(&row).expect("serialises");
    assert_eq!(serde_json::from_str::<OrderViewWire>(&json).expect("round trip"), row);
    let newer = json.replace(r#""all_or_none":true"#, r#""all_or_none":true,"a_later_member":1"#);
    let read: OrderViewWire = serde_json::from_str(&newer).expect("the frame still decodes");
    assert_eq!(read.attributes, None, "an attribute set it cannot read is not said");
    assert_eq!(read.trail_percent, Some(1.5));
}

/// A resting order's DISPLAY is a READING: absent when `None` (the frame from before), round-tripped at zero (a
/// HIDDEN order), read as `None` from an older node's row, and ignored by a reader that predates it.
#[test]
fn an_order_rows_display_is_a_reading_an_older_reader_ignores() {
    let mut row = full_snapshot().orders.remove(0);
    let off = serde_json::to_string(&row).expect("serialises");
    assert!(!off.contains("display_quantity"), "no display, no key: {off}");
    row.display_quantity = Some(0.0);
    let on = serde_json::to_string(&row).expect("serialises");
    assert!(on.contains(r#""display_quantity":0.0"#), "{on}");
    assert_eq!(serde_json::from_str::<OrderViewWire>(&on).expect("round trip"), row);
    let older: OrderViewWire = serde_json::from_str(&off).expect("an older node's row decodes");
    assert_eq!(older.display_quantity, None);

    #[derive(Deserialize)]
    struct BeforeTheField {
        order_client_id: String,
    }
    let before: BeforeTheField = serde_json::from_str(&on).expect("an old reader ignores the key");
    assert_eq!(before.order_client_id, "c-1");
}

/// A HELD row's terms (its attributes, percent trail, adjustment and display) are READINGS as a resting order's: each
/// absent when `None` (a held row without them is the frame from before), round-tripped, ignored by a reader that
/// predates them, and a member a NEWER node adds inside its attributes reads as "not said", never a failed frame.
#[test]
fn a_held_rows_terms_are_readings_that_never_fail_the_frame() {
    let mut held = full_snapshot().held_exits.remove(0);
    let off = serde_json::to_string(&held).expect("serialises");
    for key in ["attributes", "trail_percent", "adjust", "display_quantity"] {
        assert!(!off.contains(key), "a held row without its terms writes no `{key}`: {off}");
    }
    held.attributes = Some(OrderAttributes { all_or_none: true, ..Default::default() });
    held.trail_percent = Some(1.5);
    held.adjust = Some(OrderStopAdjust { at: 120.0, to: OrderStopAdjustTo::Stop { stop: 110.0 } });
    held.display_quantity = Some(0.0);
    let json = serde_json::to_string(&held).expect("serialises");
    assert_eq!(serde_json::from_str::<OrderHeldViewWire>(&json).expect("round trip"), held);
    let older: OrderHeldViewWire = serde_json::from_str(&off).expect("an older row decodes");
    assert_eq!((older.attributes, older.adjust), (None, None));

    #[derive(Deserialize)]
    struct BeforeTheTerms {
        order_client_id: String,
    }
    let before: BeforeTheTerms = serde_json::from_str(&json).expect("an old reader decodes");
    assert_eq!(before.order_client_id, "c-2");

    let newer = json.replace(r#""all_or_none":true"#, r#""all_or_none":true,"a_later_member":1"#);
    let read: OrderHeldViewWire = serde_json::from_str(&newer).expect("the frame still decodes");
    assert_eq!(read.attributes, None, "an attribute set it cannot read is not said");
    assert_eq!((read.trail_percent, read.display_quantity), (Some(1.5), Some(0.0)));
    assert_eq!(read.adjust, held.adjust);
}

/// **The order client id v2 fields are additive both ways** (decision 0270). A block's account code and shape and a
/// mount row's strategy code are READINGS: absent when `None` (the frame from before, byte for byte), round-tripped,
/// and read as "not said" from an older node; the shape is the model's serde words, field for field. A bracket's stem
/// is a FIELD: absent when `None` (the pre-field frame, which an older node decodes), round-tripped, and a key an
/// older node's `deny_unknown_fields` refuses, which is why it owes `bracket-stem`.
#[test]
fn the_order_client_id_fields_are_additive_and_old_and_new_peers_read_each_other() {
    let mut block = full_snapshot().venues.remove(0);
    let off = serde_json::to_string(&block).expect("serialises");
    assert!(!off.contains("account_code") && !off.contains("order_client_id_shape"), "{off}");
    block.account_code = Some("07".into());
    block.order_client_id_shape =
        Some(OrderClientIdShape { account: false, u_width: 11, max_gen: 0, tag: false });
    let on = serde_json::to_string(&block).expect("serialises");
    let tail = r#","account_code":"07","order_client_id_shape":{"account":false,"u_width":11,"max_gen":0,"tag":false}}"#;
    assert!(on.ends_with(tail), "{on}");
    assert_eq!(serde_json::from_str::<VenueBlockWire>(&on).expect("round trip"), block);
    let older: VenueBlockWire = serde_json::from_str(&off).expect("an older node's block decodes");
    assert_eq!((older.account_code, older.order_client_id_shape), (None, None));

    let row: MountViewWire =
        serde_json::from_str(r#"{"strategy":"grid","params":"","live":false}"#).expect("old row");
    assert_eq!(row.order_client_id_code, None, "an older node's row says no code");
    assert!(!serde_json::to_string(&row).expect("serialises").contains("order_client_id_code"));
    let coded = MountViewWire { order_client_id_code: Some("00A3".into()), ..row };
    let json = serde_json::to_string(&coded).expect("serialises");
    assert!(json.ends_with(r#","order_client_id_code":"00A3"}"#), "{json}");
    assert_eq!(serde_json::from_str::<MountViewWire>(&json).expect("round trip"), coded);

    let bracket = OrderBracketWire {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let plain = serde_json::to_string(&bracket).expect("serialises");
    let pre_field = concat!(
        r#"{"venue":"bybit","symbol":"BTCUSDT","side":1,"qty":1.0,"entry_price":null,"#,
        r#""stop_loss":90.0,"take_profit":110.0}"#
    );
    assert_eq!(plain, pre_field, "a bracket with no stem is the pre-field frame");
    let stemmed =
        OrderBracketWire { order_client_id: Some("C000007k3x9q2m7d1p4".into()), ..bracket.clone() };
    let json = serde_json::to_string(&stemmed).expect("serialises");
    assert_eq!(serde_json::from_str::<OrderBracketWire>(&json).expect("round trip"), stemmed);

    /// `OrderBracketWire` as the node before the field declared it (`deny_unknown_fields` kept).
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct BeforeTheStem {
        venue: String,
        symbol: String,
        side: i32,
        qty: f64,
        entry_price: Option<f64>,
        stop_loss: f64,
        take_profit: f64,
        #[serde(default)]
        account: Option<String>,
    }
    let read: BeforeTheStem = serde_json::from_str(&plain).expect("an older node reads it");
    let want = BeforeTheStem {
        venue: bracket.venue,
        symbol: bracket.symbol,
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
    };
    assert_eq!(read, want);
    let err = serde_json::from_str::<BeforeTheStem>(&json).expect_err("and refuses the stem");
    assert!(err.to_string().contains("unknown field `order_client_id`"), "{err}");
}

/// A route key reads as the book an operator types through the model's `#` rule: the bare venue is `DEFAULT`, a
/// label is itself (a lane twin's too), and a key that names no account of the venue is printed as it came.
#[test]
fn a_route_key_reads_as_its_book_by_the_models_rule() {
    assert_eq!(book_of_route_key("binance", "binance"), "binance/DEFAULT");
    assert_eq!(book_of_route_key("binance", "binance#ALT"), "binance/ALT");
    assert_eq!(book_of_route_key("binance", "binance#PERP"), "binance/PERP");
    assert_eq!(book_of_route_key("binance", "bybit#ALT"), "bybit#ALT", "not re-split");
    let refusal = names_no_account_refusal("this command", "bybit", &["bybit#LIVE", "bybit#DEMO"]);
    assert!(refusal.ends_with("Name one of these books: bybit/DEMO, bybit/LIVE"), "{refusal}");
}
