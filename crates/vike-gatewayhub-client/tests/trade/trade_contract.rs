#![cfg(feature = "trade")]
//! **The trade lease and the `ExecutionClient` contract, against the fake gatewayhub.**
//!
//! What a tradehub relies on before it ever loses a link: the lease is signed under `Scope::Trade` and
//! only then, a daemon that does not serve the class is refused BEFORE the client signs, every
//! `ExecutionClient` call is exactly one frame with the next `command_sequence` (and none is lost between a boxed
//! client and the wire), the HALT-free happy path acknowledges, the request/answer plumbing works for
//! the adapter start, the arming table and the preflight legs, and no key reaches a `Debug`.

use std::time::Duration;

use vike_execution::{ExecutionClient, RestoredOrder};
use vike_gatewayhub_client::client::TradeLeaseOpenError;
use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterRisk, AdapterStart, ArmingRowWire, ArmingTableWire,
    FEATURE_GATEWAY_ARMING, FEATURE_GATEWAY_PREFLIGHT, FEATURE_GATEWAY_TRADE_VENUE_POSITIONS,
    FEATURE_GATEWAY_TRADE_VENUE_READ, NetReadingWire, PROPERTIES_BUSY, PreflightLegWire,
    PreflightStep, Request, Response, ServerTimeGapWire, SymbolPropertiesRefusal,
    SymbolPropertiesRefusalCode, TradeDown, TradeUp, trade_venue_feature,
};
use vike_gatewayhub_client::trade::{
    SymbolPropertiesRequestError, TradeError, TradeLeaseBuilder, rpc,
};
use vike_hub_protocol::auth::Scope;
use vike_model::OrderCancelIntent;
use vike_model::{AccountTier, VenueRead, VenueReadKind, not_offered_words};
use vike_model::{GridSource, VenueAdapterDeclarationView};

use vike_gatewayhub_client::test_support::fake_peer::dead_address;
use vike_gatewayhub_client::test_support::fake_trade_peer;

use fake_trade_peer::{
    Step, TradePeer, TradePeerConfig, account, default_script, fast_trade_timings,
    keys_without_trade, lane, link_to, open, order, wait_ups,
};

const WAIT: Duration = Duration::from_secs(5);

fn start_adapter_args() -> AdapterStart {
    AdapterStart {
        rid: 0,
        symbol: "BTCUSDT".into(),
        declared_legs: vec!["ETHUSDT".into()],
        reconcile_enabled: true,
        takes_trigger: false,
        risk: Some(AdapterRisk { max_leverage: Some(3.0) }),
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_currency: false,
        route_key: "ignored: the session names the account's own".into(),
    }
}

#[test]
fn the_lease_is_signed_under_the_trade_scope_and_opens_the_account() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    assert_eq!(
        peer.auths(),
        [(Scope::Trade, true)],
        "one connection, under the Trade key, verified"
    );
    let opens = peer.opens();
    assert_eq!(opens.len(), 1);
    let (_, client_id, resume, takeover) = &opens[0];
    assert_eq!(client_id, "tradehub-1");
    assert!(resume.is_none() && !takeover, "a fresh, non-taking open");
    let opened = session.opened().expect("the open reply is kept");
    assert_eq!(opened.gateway_epoch, peer.epoch());
    assert_eq!(opened.heartbeat_ms, 500);
    assert_eq!(opened.project_token, "project-token-of-the-tests");
}

#[test]
fn a_link_without_a_trade_key_dials_nothing() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (events, _lane) = lane();
    let link = vike_gatewayhub_client::client::GatewayLink::new(
        peer.address_string(),
        keys_without_trade(),
    );
    let err =
        TradeLeaseBuilder::new(link, account("tradehub-1"), events).open().expect_err("no key");
    let TradeLeaseOpenError::Link(io) = err else { panic!("a missing key is a link error") };
    assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(io.to_string().contains("trade"), "{io}");
    assert_eq!(peer.hellos(), 0, "nothing was dialled");
}

#[test]
fn a_daemon_that_does_not_serve_the_class_is_refused_before_the_client_signs() {
    let features: Vec<String> =
        vec!["gatewayhub".into(), "gateway_data".into(), "gateway_status".into()];
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (events, _lane) = lane();
    let err = TradeLeaseBuilder::new(peer.link(), account("tradehub-1"), events)
        .open()
        .expect_err("no class");
    let text = err.to_string();
    assert!(text.contains("gateway_trade"), "the refusal names the string: {text}");
    assert_eq!(peer.hellos(), 1, "the client heard the Welcome");
    assert!(peer.auths().is_empty(), "and signed NOTHING: the ORDER key never left");
}

#[test]
fn a_missing_venue_or_lease_string_is_refused_naming_it_and_nothing_is_opened() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| *f != trade_venue_feature("binance"));
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (events, _lane) = lane();
    let err = TradeLeaseBuilder::new(peer.link(), account("tradehub-1"), events)
        .open()
        .expect_err("no venue");
    assert!(err.to_string().contains("trade_venue=binance"), "{err}");
    assert!(peer.opens().is_empty(), "no TradeOpen was sent");
}

#[test]
fn the_daemons_refusals_keep_their_shape_and_words() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let try_open = || {
        let (events, _lane) = lane();
        TradeLeaseBuilder::new(peer.link(), account("tradehub-1"), events)
            .with_timings(fast_trade_timings())
            .open()
            .expect_err("refused")
    };
    peer.refuse_open(Some(Response::LeaseHeld { holder_key_id: "nk-abc".into(), since_ms: 77 }));
    match try_open() {
        TradeLeaseOpenError::LeaseHeld { holder_key_id, since_ms } => {
            assert_eq!((holder_key_id.as_str(), since_ms), ("nk-abc", 77));
        }
        other => panic!("{other:?}"),
    }
    peer.refuse_open(Some(Response::TradeRefused {
        block_word: "tier_conflict".into(),
        text: "the account runs at live".into(),
    }));
    match try_open() {
        TradeLeaseOpenError::Refused { block_word, text } => {
            assert_eq!(block_word, "tier_conflict");
            assert_eq!(text, "the account runs at live");
        }
        other => panic!("{other:?}"),
    }
    peer.refuse_open(Some(Response::Error("gatewayhub: not today".into())));
    match try_open() {
        TradeLeaseOpenError::Daemon(text) => assert_eq!(text, "gatewayhub: not today"),
        other => panic!("{other:?}"),
    }
    assert_eq!(peer.leases_opened(), 0);
}

#[test]
fn a_dead_gatewayhub_is_a_link_error_naming_the_address() {
    let addr = dead_address();
    let (events, _lane) = lane();
    let err = TradeLeaseBuilder::new(link_to(&addr), account("tradehub-1"), events)
        .open()
        .expect_err("nothing listens");
    assert!(matches!(err, TradeLeaseOpenError::Link(_)));
    assert!(err.to_string().contains(&addr), "{err}");
}

#[test]
fn the_adapter_start_round_trips_and_the_session_names_the_accounts_route_key() {
    let peer = TradePeer::start(TradePeerConfig::new().with_script(|frame| match frame {
        TradeUp::AdapterStart(start) => {
            // The reply echoes what the daemon was told, so the test can read it back.
            let reply = if start.symbol == "BTCUSDT" {
                AdapterReply::Running {
                    bound_tier: AccountTier::Demo,
                    grid: None,
                    contract_value: Some(vike_model::ContractValue::inverse(1.0)),
                    margin_mode: None,
                    leg_grids: Vec::new(),
                    reconcile: true,
                    leverage_cap: None,
                    account_currency: None,
                    account_currency_assumed: false,
                    native_time_release: false,
                    broker_code_len: 0,
                }
            } else {
                AdapterReply::NotRunning {
                    block_word: "adapter_failed".into(),
                    refusal_text: format!("route {} symbol {}", start.route_key, start.symbol),
                }
            };
            vec![Step::Send(TradeDown::AdapterReply { rid: start.rid, reply: Box::new(reply) })]
        }
        _ => Vec::new(),
    }));
    let (session, _lane) = open(&peer, "tradehub-1");
    match session.start_adapter(start_adapter_args()).expect("answered") {
        AdapterReply::Running { bound_tier, contract_value, reconcile: recon, .. } => {
            assert_eq!(bound_tier, AccountTier::Demo);
            assert_eq!(contract_value, Some(vike_model::ContractValue::inverse(1.0)));
            assert!(recon);
        }
        other => panic!("{other:?}"),
    }
    let mut other_symbol = start_adapter_args();
    other_symbol.symbol = "NOPE".into();
    match session.start_adapter(other_symbol).expect("answered") {
        AdapterReply::NotRunning { block_word, refusal_text } => {
            assert_eq!(block_word, "adapter_failed");
            assert_eq!(
                refusal_text, "route binance symbol NOPE",
                "the session overwrote the route key"
            );
        }
        other => panic!("{other:?}"),
    }
    let sent: Vec<AdapterStart> = peer
        .ups()
        .into_iter()
        .filter_map(|f| match f {
            TradeUp::AdapterStart(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0].rid, sent[1].rid, "each request has its own id");
    assert_eq!(sent[0].risk, Some(AdapterRisk { max_leverage: Some(3.0) }));
}

#[test]
fn a_verb_the_daemon_does_not_advertise_is_refused_locally_naming_the_string() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != "gateway_adapter");
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session.start_adapter(start_adapter_args()).expect_err("not advertised");
    assert!(matches!(err, TradeError::Unsupported(_)));
    assert!(err.text().contains("gateway_adapter"), "{err}");
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::AdapterStart(_))), "nothing was sent");
}

#[test]
fn every_execution_call_is_one_frame_with_the_next_command_sequence() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();

    exec.submit(&order("c1"));
    exec.cancel("c1");
    exec.cancel_with_intent("c2", OrderCancelIntent::RiskOff);
    exec.cancel_batch_with_intent(
        &["c3".to_string(), "c4".to_string()],
        OrderCancelIntent::Routine,
    );
    exec.modify(&order("c5"), Some(2.0), None);
    exec.confirm("c6");
    exec.adopt_restored(&[RestoredOrder {
        venue_order_id: Some("v7".into()),
        ..RestoredOrder::new("c7", "BTCUSDT")
    }]);
    exec.submit_batch(&[order("b1"), order("b2")]);

    let frames = wait_ups(&peer, WAIT, 8, |f| fake_trade_peer::command_sequence_of(f).is_some());
    assert_eq!(frames.len(), 8, "{frames:?}");
    let cseqs: Vec<u64> = frames.iter().filter_map(fake_trade_peer::command_sequence_of).collect();
    assert_eq!(cseqs, [1, 2, 3, 4, 5, 6, 7, 8], "per client, +1 per frame, in call order");

    assert!(matches!(&frames[0], TradeUp::Submit { order, .. } if order.order_client_id == "c1"));
    assert!(matches!(
        &frames[1],
        TradeUp::Cancel { order_client_id, intent: OrderCancelIntent::Unspecified, .. } if order_client_id == "c1"
    ));
    assert!(matches!(
        &frames[2],
        TradeUp::Cancel { order_client_id, intent: OrderCancelIntent::RiskOff, .. } if order_client_id == "c2"
    ));
    assert!(matches!(
        &frames[3],
        TradeUp::CancelBatch { order_client_ids, intent: OrderCancelIntent::Routine, .. } if order_client_ids == &["c3", "c4"]
    ));
    assert!(matches!(
        &frames[4],
        TradeUp::Modify { order, new_qty: Some(q), new_price: None, .. }
            if order.order_client_id == "c5" && (*q - 2.0).abs() < f64::EPSILON
    ));
    assert!(
        matches!(&frames[5], TradeUp::Confirm { order_client_id, .. } if order_client_id == "c6")
    );
    assert!(matches!(
        &frames[6],
        TradeUp::AdoptRestored { pairs, venue_ids, .. }
            if pairs.len() == 1 && venue_ids == &[("c7".to_string(), "v7".to_string())]
    ));
    assert!(matches!(&frames[7], TradeUp::SubmitBatch { orders, .. } if orders.len() == 2));
}

/// A trigger amend rides the Modify frame to a daemon that advertised it reads the key, and is refused
/// LOCALLY in words, nothing sent, against one that did not: an older daemon would drop the key and move
/// the limit alone.
#[test]
fn a_trigger_amend_is_sent_only_to_a_daemon_that_advertised_it() {
    use vike_gatewayhub_client::proto::FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER;
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    assert_eq!(exec.modify_trigger(&order("t1"), None, Some(99.0), 91.0), Ok(()));
    let frames = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::Modify { .. }));
    let Some(TradeUp::Modify { order: sent, new_price, new_trigger_price, .. }) = frames.first()
    else {
        panic!("{frames:?}")
    };
    assert_eq!(
        (sent.order_client_id.as_str(), *new_price, *new_trigger_price),
        ("t1", Some(99.0), Some(91.0))
    );

    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER);
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    let refused = exec.modify_trigger(&order("t2"), None, None, 91.0);
    assert!(refused.is_err_and(|why| why.contains(FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER)));
    exec.confirm("after");
    wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::Confirm { .. }));
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::Modify { .. })), "nothing was sent");
}

/// An order carrying an attributes-wave term (decision 0267) rides the Submit frame, the term with it, to a daemon
/// that advertised it reads them, and against one that did not is REJECTED terminally on the core's lane in words,
/// nothing sent: an older daemon would drop the term and send a plain order. A plain order is unaffected.
#[test]
fn an_attributed_order_is_sent_only_to_a_daemon_that_advertised_it() {
    use vike_gatewayhub_client::proto::FEATURE_GATEWAY_TRADE_ORDER_ATTRIBUTES;
    let attributed = |order_client_id: &str| vike_model::OrderRequest {
        attributes: Some(vike_model::OrderAttributes { all_or_none: true, ..Default::default() }),
        ..order(order_client_id)
    };
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&attributed("a1"));
    let frames = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::Submit { .. }));
    let Some(TradeUp::Submit { order: sent, .. }) = frames.first() else { panic!("{frames:?}") };
    assert_eq!(sent.attributes.map(|a| a.all_or_none), Some(true));

    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != FEATURE_GATEWAY_TRADE_ORDER_ATTRIBUTES);
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit_batch(&[attributed("a2"), order("p2")]);
    exec.submit(&attributed("a3"));
    let got = lane.collect_until(WAIT, 2, |i| {
        matches!(i, vike_execution::Ingest::Event(vike_model::events::Event::OrderRejected(_)))
    });
    let rejected = fake_trade_peer::rejects(&got);
    assert_eq!(rejected.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["a2", "a3"]);
    assert!(
        rejected.iter().all(|r| r.1.contains(FEATURE_GATEWAY_TRADE_ORDER_ATTRIBUTES)),
        "{rejected:?}"
    );
    let frames = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::SubmitBatch { .. }));
    assert!(
        matches!(&frames[0], TradeUp::SubmitBatch { orders, .. } if orders.len() == 1 && orders[0].order_client_id == "p2"),
        "only the plain order went: {frames:?}"
    );
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::Submit { .. })), "a3 was not sent");
}

#[test]
fn the_default_cancel_door_is_the_flatten_safe_one() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.cancel("k1");
    exec.cancel_batch(&["k2".to_string(), "k3".to_string()]);
    let frames = wait_ups(&peer, WAIT, 2, |f| fake_trade_peer::command_sequence_of(f).is_some());
    assert!(matches!(&frames[0], TradeUp::Cancel { intent: OrderCancelIntent::Unspecified, .. }));
    assert!(matches!(
        &frames[1],
        TradeUp::CancelBatch { intent: OrderCancelIntent::Unspecified, .. }
    ));
    assert!(!OrderCancelIntent::default().may_be_shed());
}

#[test]
fn a_boxed_client_forwards_the_wire_id_and_the_adoption() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    session.prefetch_wire_ids(&["hl-1".to_string(), "bn-2".to_string()]).expect("prefetched");
    let mut boxed: Box<dyn ExecutionClient + Send> = Box::new(session.execution_client());
    assert_eq!(boxed.wire_id_for("hl-1").as_deref(), Some("0xhl-1"), "the keccak-shaped answer");
    assert_eq!(
        boxed.wire_id_for("bn-2"),
        None,
        "asked about, and the venue echoes the order client id"
    );
    assert_eq!(boxed.wire_id_for("never-asked"), None, "a miss is not an echo, and does not panic");
    boxed.adopt_restored(&[RestoredOrder::new("hl-1", "BTC")]);
    let frames = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::AdoptRestored { .. }));
    assert_eq!(frames.len(), 1, "the boxed impl delegated adopt_restored to the wire");
}

/// An operator's join asks the wire ids NOW and keeps nothing: the pairs come back for the order client ids whose wire id
/// differs, and the restore's cache behind `wire_id_for` is not filled by them.
#[test]
fn wire_pairs_answer_now_and_leave_the_restore_cache_alone() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let pairs =
        session.wire_pairs(&["hl-1".to_string(), "bn-2".to_string()], WAIT).expect("answered");
    assert_eq!(pairs, [("hl-1".to_string(), "0xhl-1".to_string())]);
    let boxed: Box<dyn ExecutionClient + Send> = Box::new(session.execution_client());
    assert_eq!(boxed.wire_id_for("hl-1"), None, "the join filled no restore cache");
}

#[test]
fn the_identity_status_and_resolve_requests_round_trip() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    session.record_identity().expect("recorded");
    assert!(session.status().expect("status").adapter_up);
    let resolved = session.resolve_orders(&["u1".to_string()]).expect("resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        session.resolution_of("u1"),
        Some(vike_gatewayhub_client::proto::ResolveState::Querying)
    );
}

#[test]
fn symbol_facts_round_trip_and_a_daemon_refusal_keeps_its_words() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let facts = session.symbol_facts("SOLUSDT.P").expect("facts");
    assert_eq!((facts.properties.tick_size, facts.properties.step_size), (0.01, 1.0));
    assert_eq!(facts.contract_value, None);
    let err = session.symbol_facts("NOPEUSDT").expect_err("unknown symbol");
    assert_eq!(
        err,
        SymbolPropertiesRequestError::Daemon(SymbolPropertiesRefusal {
            code: SymbolPropertiesRefusalCode::NoInstrument,
            reason: "binance: no instrument NOPEUSDT in the venue's instrument list".into(),
        })
    );
    assert!(!err.is_transient(), "a verdict on the symbol: remembered");
    let busy = session.symbol_facts("BUSYUSDT").expect_err("a busy daemon");
    assert_eq!(
        busy,
        SymbolPropertiesRequestError::Daemon(SymbolPropertiesRefusal {
            code: SymbolPropertiesRefusalCode::Busy,
            reason: PROPERTIES_BUSY.into(),
        })
    );
    assert!(busy.is_transient(), "not a verdict on the symbol");
}

/// A facts LOOKUP is a probe (the Preview, the Telegram look, a refused order all read facts without
/// admitting anything); only `admit_symbol`, which the node makes after it sent `AdmitSymbol`, is a
/// commit. The daemon tells the account's reconcile client about a symbol on a commit alone.
#[test]
fn a_lookup_is_a_probe_and_admit_symbol_is_the_commit() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    session.symbol_facts("SOLUSDT.P").expect("facts");
    session.admit_symbol("SOLUSDT.P").expect("committed");
    let asked: Vec<bool> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::SymbolProperties { probe, .. } => Some(*probe),
            _ => None,
        })
        .collect();
    assert_eq!(asked, [true, false], "the lookup probes, the admission commits");
}

/// An old node sends no `probe` field: the daemon reads it as a COMMIT (the pre-field behaviour).
#[test]
fn a_symbol_facts_frame_without_probe_reads_as_a_commit() {
    let frame: TradeUp =
        serde_json::from_str(r#"{"SymbolProperties":{"rid":3,"symbol":"SOLUSDT.P"}}"#)
            .expect("parses");
    assert!(matches!(frame, TradeUp::SymbolProperties { probe: false, rid: 3, .. }), "{frame:?}");
}

#[test]
fn symbol_facts_without_the_feature_are_refused_locally_and_nothing_is_sent() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != "gateway_trade_symbol_properties");
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session.symbol_facts("SOLUSDT.P").expect_err("not advertised");
    assert_eq!(
        err,
        SymbolPropertiesRequestError::Refused(
            "the gatewayhub does not advertise symbol facts (update it)".into()
        )
    );
    assert!(
        peer.ups().iter().all(|f| !matches!(f, TradeUp::SymbolProperties { .. })),
        "nothing sent"
    );
}

/// An operator's venue read is one request and one answer on the lease: the daemon's answer comes back
/// whole, its refusal comes back as its own words (`not offered by ...`), and nothing else is sent.
#[test]
fn a_venue_read_is_answered_by_the_daemon_and_its_refusal_passes_through_in_words() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let bound = Duration::from_secs(5);
    let balance = session.venue_read(VenueReadKind::Balance, bound).expect("the daemon answered");
    let VenueRead::Balance(balance) = balance else { panic!("a balance read answers a balance") };
    assert_eq!((balance.asset.as_str(), balance.balance), ("pUSD", 20.0));
    let refused = session
        .venue_read(VenueReadKind::OpenOrders, bound)
        .expect_err("the daemon's bridge offers no such read");
    assert_eq!(refused, not_offered_words("binance", VenueReadKind::OpenOrders));
    let asked: Vec<VenueReadKind> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::VenueRead { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(asked, [VenueReadKind::Balance, VenueReadKind::OpenOrders]);
}

/// A daemon that does not advertise `gateway_trade_venue_read` cannot decode the frame, so the client
/// refuses locally, in words, before a byte is written.
#[test]
fn a_venue_read_without_the_feature_is_refused_locally_and_nothing_is_sent() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != FEATURE_GATEWAY_TRADE_VENUE_READ);
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session
        .venue_read(VenueReadKind::Balance, Duration::from_secs(5))
        .expect_err("not advertised");
    assert_eq!(err, "the gatewayhub does not advertise venue reads (update it)");
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::VenueRead { .. })), "nothing sent");
}

/// The third read, the account's positions, is one request and one answer too, and the daemon's answer
/// comes back whole.
#[test]
fn a_positions_read_is_answered_by_the_daemon() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let read = session
        .venue_read(VenueReadKind::Positions, Duration::from_secs(5))
        .expect("the daemon answered");
    let VenueRead::Positions { positions, truncated } = read else {
        panic!("a positions read answers positions")
    };
    assert!(!truncated);
    assert_eq!((positions[0].qty, positions[0].redeemable), (11.11, true));
    let asked: Vec<VenueReadKind> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::VenueRead { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(asked, [VenueReadKind::Positions]);
}

/// ⚠ A daemon from the build that has `gateway_trade_venue_read` but not the positions kind cannot decode the
/// kind's name, and a trade frame the daemon cannot decode ENDS THE LEASE. So against a daemon that does
/// not advertise `gateway_trade_venue_positions` the client refuses positions locally, in words, and writes
/// nothing, while the two older kinds still go to the same daemon.
#[test]
fn a_positions_read_without_its_own_feature_is_refused_locally_and_nothing_is_sent() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != FEATURE_GATEWAY_TRADE_VENUE_POSITIONS);
    assert!(features.iter().any(|f| f == FEATURE_GATEWAY_TRADE_VENUE_READ), "the older word stays");
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session
        .venue_read(VenueReadKind::Positions, Duration::from_secs(5))
        .expect_err("not advertised");
    assert_eq!(err, "the gatewayhub does not advertise position reads (update it)");
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::VenueRead { .. })), "nothing sent");
    let balance = session.venue_read(VenueReadKind::Balance, Duration::from_secs(5));
    assert!(balance.is_ok(), "the older kinds are unaffected: {balance:?}");
}

#[test]
fn the_arming_table_and_the_preflight_legs_are_asked_over_a_fresh_trade_connection() {
    let table = ArmingTableWire {
        properties: [(
            "binance".to_string(),
            VenueAdapterDeclarationView {
                adapter_present: true,
                addresses_accounts: true,
                takes_reconcile_trigger: false,
                grid_source: GridSource::InHand,
                process_exclusive: None,
                clock_declaration_word: vike_model::ClockDeclarationKind::Unknown(
                    "measured".into(),
                ),
            },
        )]
        .into(),
        rows: vec![ArmingRowWire {
            venue: "binance".into(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: "none".into(),
            account_ids: vec![3],
            book: Some("spot".into()),
        }],
        key_changed: Vec::new(),
        start_inputs: Default::default(),
    };
    let legs = vec![
        PreflightLegWire::Clock {
            venue: "binance".into(),
            leg: ServerTimeGapWire::ServerTime { server_ms: 10, local_ms: 12, rtt_ms: 4 },
            reason: None,
            at_stake: None,
        },
        PreflightLegWire::Network {
            reading: NetReadingWire::Alive,
            rounds: Some(7),
            since_ms: None,
        },
    ];
    let (t, l) = (table.clone(), legs.clone());
    let peer = TradePeer::start(TradePeerConfig::new().with_rpc(move |req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(t.clone()))],
        Request::PreflightQuery { .. } => {
            vec![Response::PreflightLegs { legs: l.clone(), plan: None }]
        }
        _ => vec![Response::Error("no".into())],
    }));
    let got = rpc::arming_table(&peer.link()).expect("the table");
    assert_eq!(got, table);
    let got = rpc::preflight(&peer.link(), &["binance".to_string()], PreflightStep::Batch)
        .expect("the legs");
    assert_eq!(got.legs, legs);
    assert_eq!(got.plan, None);
    assert_eq!(
        peer.auths(),
        [(Scope::Trade, true), (Scope::Trade, true)],
        "each dials under Trade"
    );
    assert!(
        matches!(peer.rpcs().as_slice(), [Request::ArmingQuery, Request::PreflightQuery { venues, step: PreflightStep::Batch }] if venues == &["binance"])
    );
}

#[test]
fn the_rpc_one_shots_refuse_locally_without_their_string() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != FEATURE_GATEWAY_ARMING && f != FEATURE_GATEWAY_PREFLIGHT);
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let err = rpc::arming_table(&peer.link()).expect_err("not advertised");
    assert!(err.to_string().contains("gateway_arming"), "{err}");
    let err = rpc::preflight(&peer.link(), &[], PreflightStep::Batch).expect_err("not advertised");
    assert!(err.to_string().contains("gateway_preflight"), "{err}");
    assert!(peer.rpcs().is_empty(), "nothing was asked");
}

#[test]
fn a_daemon_side_error_keeps_the_daemons_words() {
    let peer = TradePeer::start(
        TradePeerConfig::new()
            .with_rpc(|_| vec![Response::Error("gatewayhub: ArmingQuery is not served".into())]),
    );
    let err = rpc::arming_table(&peer.link()).expect_err("an error reply");
    assert_eq!(err.to_string(), "gatewayhub: ArmingQuery is not served");
}

#[test]
fn the_client_pings_at_the_stated_period_and_a_quiet_link_stays_up() {
    let peer = TradePeer::start(TradePeerConfig::new().with_heartbeat_ms(500));
    let (session, _lane) = open(&peer, "tradehub-1");
    let pings = wait_ups(&peer, Duration::from_secs(4), 2, |f| matches!(f, TradeUp::Ping));
    assert!(pings.len() >= 2, "a quiet client owes the server a ping each period");
    assert_eq!(session.link_state(), vike_gatewayhub_client::trade::TradeLinkState::Up);
}

#[test]
fn no_key_reaches_a_debug() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let link = peer.link();
    let (session, _l) = open(&peer, "tradehub-2");
    let shown = format!("{link:?} {session:?} {:?}", session.account());
    for secret in ["trade-key-of-the-tests", "control-key-of-the-tests", "observe-key-of-the-tests"]
    {
        assert!(!shown.contains(secret), "a key reached Debug: {shown}");
    }
}

#[test]
fn detach_flushes_what_is_queued_then_says_goodbye_and_later_calls_are_rejected_not_lost() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("d1"));
    exec.cancel_with_intent("d1", OrderCancelIntent::RiskOff);
    exec.begin_detach();
    exec.detach();
    let finals = wait_ups(&peer, WAIT, 1, |f| {
        matches!(f, TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Final })
    });
    assert_eq!(finals.len(), 1, "the goodbye reached the daemon: {:?}", peer.ups());
    let kinds: Vec<&'static str> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::Submit { .. } => Some("submit"),
            TradeUp::Cancel { .. } => Some("cancel"),
            TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Begin } => {
                Some("begin")
            }
            TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Final } => {
                Some("final")
            }
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["submit", "cancel", "begin", "final"], "the sweep is ahead of the goodbye");
    // The client is detached: an order now is a terminal reject on the core's lane, never silence.
    exec.submit(&order("late"));
    let got = lane.collect_until(WAIT, 1, |i| {
        matches!(i, vike_execution::Ingest::Event(vike_model::events::Event::OrderRejected(_)))
    });
    let rejected = fake_trade_peer::rejects(&got);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, "late");
}

#[test]
fn production_timings_only_ever_raise_the_protocol_floors() {
    use vike_gatewayhub_client::proto::{FETCH_STALL_FLOOR, GATEWAY_TRADE_READ_FLOOR};
    use vike_gatewayhub_client::trade::TradeTimings;
    let floors = TradeTimings::default();
    assert_eq!(floors.read_floor, GATEWAY_TRADE_READ_FLOOR);
    assert_eq!(floors.reconcile_stall_floor, FETCH_STALL_FLOOR);
    let below = TradeTimings::raised(Duration::from_millis(1), Duration::from_millis(1));
    assert_eq!(below, floors, "a setting below a floor is the floor");
    let above = TradeTimings::raised(Duration::from_secs(20), Duration::from_secs(90));
    assert_eq!(above.read_floor, Duration::from_secs(20));
    assert_eq!(above.reconcile_stall_floor, Duration::from_secs(90));
}

fn final_detach(f: &TradeUp) -> bool {
    matches!(f, TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Final })
}

/// **Phase one of a teardown ends the lease on the lease's own thread** (wave 2, W13). The core raises
/// `begin_detach` on EVERY engine before it joins any, so N leases must wind down inside one goodbye
/// wait and not one after another: the final `Detach` goes out after phase one alone, with no `detach`
/// call behind it, the sweep ahead of it, and the caller never waits.
#[test]
fn phase_one_alone_flushes_the_sweep_and_says_the_final_goodbye_without_blocking_the_caller() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("p1"));
    exec.cancel_with_intent("p1", OrderCancelIntent::RiskOff);
    let started = std::time::Instant::now();
    exec.begin_detach();
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "phase one is a channel send, it never waits ({:?})",
        started.elapsed()
    );
    let finals = wait_ups(&peer, WAIT, 1, final_detach);
    assert_eq!(finals.len(), 1, "the goodbye needs no phase two: {:?}", peer.ups());
    let order_of: Vec<&'static str> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::Submit { .. } => Some("submit"),
            TradeUp::Cancel { .. } => Some("cancel"),
            f if final_detach(f) => Some("final"),
            _ => None,
        })
        .collect();
    assert_eq!(order_of, ["submit", "cancel", "final"]);
    // The later `detach` finds the link over and returns.
    let join = std::time::Instant::now();
    exec.detach();
    assert!(join.elapsed() < Duration::from_secs(3), "{:?}", join.elapsed());
}

/// **The ack drain is bounded.** A daemon that reads the sweep but never acknowledges it is waited for
/// the ack bound (2 s) and no longer: the final detach follows on the same stream, so the daemon still
/// reads the sweep first, and nothing blocks forever.
#[test]
fn a_daemon_that_never_acknowledges_is_waited_for_the_bound_and_no_longer() {
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.cancel_with_intent("q1", OrderCancelIntent::RiskOff);
    let started = std::time::Instant::now();
    exec.begin_detach();
    let finals = wait_ups(&peer, Duration::from_secs(8), 1, final_detach);
    let waited = started.elapsed();
    assert_eq!(finals.len(), 1, "the goodbye is sent even when no ack came: {:?}", peer.ups());
    assert!(waited >= Duration::from_millis(1_500), "the sweep's acks were waited for: {waited:?}");
    assert!(waited < Duration::from_secs(6), "…but not for long: {waited:?}");
    exec.detach();
}

/// And the acknowledged case is not delayed by the bound.
#[test]
fn an_acknowledged_sweep_is_not_delayed_by_the_ack_bound() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.cancel_with_intent("r1", OrderCancelIntent::RiskOff);
    let started = std::time::Instant::now();
    exec.begin_detach();
    let finals = wait_ups(&peer, WAIT, 1, final_detach);
    assert_eq!(finals.len(), 1);
    assert!(started.elapsed() < Duration::from_millis(1_500), "{:?}", started.elapsed());
    exec.detach();
}

/// W13b finding 8: a caller that would start something new on a lease (the tradehub's re-teaching
/// worker restarting an adapter) must be able to see that a teardown has begun, from the first moment of
/// it (the ack drain is up to two seconds in which the lease is not yet closed).
#[test]
fn a_teardown_is_visible_to_a_caller_from_the_moment_it_begins() {
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, _lane) = open(&peer, "tradehub-1");
    assert!(!session.is_stopping(), "a fresh lease is not stopping");
    let mut exec = session.execution_client();
    exec.cancel_with_intent("s1", OrderCancelIntent::RiskOff);
    exec.begin_detach();
    assert!(session.is_stopping(), "phase one marks it, before the lease is closed");
    assert_ne!(
        session.link_state(),
        vike_gatewayhub_client::trade::TradeLinkState::Closed,
        "the ack drain is still running: not closed yet, but stopping"
    );
    exec.detach();
    assert!(session.is_stopping());
}

#[test]
fn a_lost_link_during_a_facts_request_is_unavailable_never_a_refusal() {
    let peer = TradePeer::start(TradePeerConfig::new().with_script(|up| match up {
        TradeUp::SymbolProperties { .. } => vec![Step::Close],
        other => default_script()(other),
    }));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session.symbol_facts("SOLUSDT.P").expect_err("the link is cut");
    assert!(matches!(err, SymbolPropertiesRequestError::Unavailable(_)), "{err:?}");
}
