#![cfg(feature = "trade")]
//! **The trade lease and the `ExecutionClient` contract, against the fake gatewayhub.**
//!
//! What a tradehub relies on before it ever loses a link: the lease is signed under `Scope::Trade` and
//! only then, a daemon that does not serve the class is refused BEFORE the client signs, every
//! `ExecutionClient` call is exactly one frame with the next `cseq` (and none is lost between a boxed
//! client and the wire), the HALT-free happy path acknowledges, the request/answer plumbing works for
//! the adapter start, the arming table and the preflight legs, and no key reaches a `Debug`.

use std::time::Duration;

use vike_exec::{CancelIntent, ExecutionClient};
use vike_gatewayhub_client::client::TradeOpenError;
use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterRisk, AdapterStart, ArmingRow, ArmingTable, ClockLeg, FACTS_BUSY,
    FEATURE_GW_ARMING, FEATURE_GW_PREFLIGHT, GridSource, LegResult, NetReading, PreflightStep,
    Request, Response, TradeDown, TradeUp, VenueFacts, WireCancelIntent, trade_venue_feature,
};
use vike_gatewayhub_client::trade::{SymbolFactsError, TradeError, TradeLink, rpc};
use vike_hub_proto::auth::Scope;
use vike_model::AccountTier;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{
    Step, TradePeer, TradePeerConfig, account, dead_addr, default_script, fast_trade_timings,
    keys_without_trade, lane, link_to, open, order, wait_ups,
};

const WAIT: Duration = Duration::from_secs(5);

fn start_adapter_args() -> AdapterStart {
    AdapterStart {
        rid: 0,
        symbol: "BTCUSDT".into(),
        declared_legs: vec!["ETHUSDT".into()],
        recon_enabled: true,
        takes_trigger: false,
        risk: Some(AdapterRisk { max_leverage: Some(3.0) }),
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_ccy: false,
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
    assert_eq!(opened.gw_epoch, peer.epoch());
    assert_eq!(opened.heartbeat_ms, 500);
    assert_eq!(opened.project_token, "project-token-of-the-tests");
}

#[test]
fn a_link_without_a_trade_key_dials_nothing() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (events, _lane) = lane();
    let link =
        vike_gatewayhub_client::client::GatewayLink::new(peer.addr_string(), keys_without_trade());
    let err = TradeLink::new(link, account("tradehub-1"), events).open().expect_err("no key");
    let TradeOpenError::Link(io) = err else { panic!("a missing key is a link error") };
    assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(io.to_string().contains("trade"), "{io}");
    assert_eq!(peer.hellos(), 0, "nothing was dialled");
}

#[test]
fn a_daemon_that_does_not_serve_the_class_is_refused_before_the_client_signs() {
    let features: Vec<String> = vec!["gatewayhub".into(), "gw_data".into(), "gw_status".into()];
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (events, _lane) = lane();
    let err =
        TradeLink::new(peer.link(), account("tradehub-1"), events).open().expect_err("no class");
    let text = err.to_string();
    assert!(text.contains("gw_trade"), "the refusal names the string: {text}");
    assert_eq!(peer.hellos(), 1, "the client heard the Welcome");
    assert!(peer.auths().is_empty(), "and signed NOTHING: the ORDER key never left");
}

#[test]
fn a_missing_venue_or_lease_string_is_refused_naming_it_and_nothing_is_opened() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| *f != trade_venue_feature("binance"));
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (events, _lane) = lane();
    let err =
        TradeLink::new(peer.link(), account("tradehub-1"), events).open().expect_err("no venue");
    assert!(err.to_string().contains("trade_venue=binance"), "{err}");
    assert!(peer.opens().is_empty(), "no TradeOpen was sent");
}

#[test]
fn the_daemons_refusals_keep_their_shape_and_words() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let try_open = || {
        let (events, _lane) = lane();
        TradeLink::new(peer.link(), account("tradehub-1"), events)
            .with_timings(fast_trade_timings())
            .open()
            .expect_err("refused")
    };
    peer.refuse_open(Some(Response::LeaseHeld { holder_key_id: "nk-abc".into(), since_ms: 77 }));
    match try_open() {
        TradeOpenError::LeaseHeld { holder_key_id, since_ms } => {
            assert_eq!((holder_key_id.as_str(), since_ms), ("nk-abc", 77));
        }
        other => panic!("{other:?}"),
    }
    peer.refuse_open(Some(Response::TradeRefused {
        block_word: "tier_conflict".into(),
        text: "the account runs at live".into(),
    }));
    match try_open() {
        TradeOpenError::Refused { block_word, text } => {
            assert_eq!(block_word, "tier_conflict");
            assert_eq!(text, "the account runs at live");
        }
        other => panic!("{other:?}"),
    }
    peer.refuse_open(Some(Response::Error("gatewayhub: not today".into())));
    match try_open() {
        TradeOpenError::Daemon(text) => assert_eq!(text, "gatewayhub: not today"),
        other => panic!("{other:?}"),
    }
    assert_eq!(peer.leases_opened(), 0);
}

#[test]
fn a_dead_gatewayhub_is_a_link_error_naming_the_address() {
    let addr = dead_addr();
    let (events, _lane) = lane();
    let err = TradeLink::new(link_to(&addr), account("tradehub-1"), events)
        .open()
        .expect_err("nothing listens");
    assert!(matches!(err, TradeOpenError::Link(_)));
    assert!(err.to_string().contains(&addr), "{err}");
}

#[test]
fn the_adapter_start_round_trips_and_the_session_names_the_accounts_route_key() {
    let peer = TradePeer::start(TradePeerConfig::new().with_script(|frame| match frame {
        TradeUp::AdapterStart(start) => {
            // The reply echoes what the daemon was told, so the test can read it back.
            let reply = if start.symbol == "BTCUSDT" {
                AdapterReply::Live {
                    bound_tier: AccountTier::Demo,
                    grid: None,
                    contract_value: Some(vike_model::ContractValue::inverse(1.0)),
                    margin_mode: None,
                    leg_grids: Vec::new(),
                    recon: true,
                    leverage_cap: None,
                    account_ccy: None,
                    account_ccy_assumed: false,
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
        AdapterReply::Live { bound_tier, contract_value, recon, .. } => {
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
    features.retain(|f| f != "gw_adapter");
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session.start_adapter(start_adapter_args()).expect_err("not advertised");
    assert!(matches!(err, TradeError::Unsupported(_)));
    assert!(err.text().contains("gw_adapter"), "{err}");
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::AdapterStart(_))), "nothing was sent");
}

#[test]
fn every_execution_call_is_one_frame_with_the_next_cseq() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.exec_client();

    exec.submit(&order("c1"));
    exec.cancel("c1");
    exec.cancel_with_intent("c2", CancelIntent::RiskOff);
    exec.cancel_batch_with_intent(&["c3".to_string(), "c4".to_string()], CancelIntent::Routine);
    exec.modify(&order("c5"), Some(2.0), None);
    exec.confirm("c6");
    exec.adopt_restored(&[("c7".to_string(), "BTCUSDT".to_string())]);
    exec.submit_batch(&[order("b1"), order("b2")]);

    let frames = wait_ups(&peer, WAIT, 8, |f| fake_trade_peer::cseq_of(f).is_some());
    assert_eq!(frames.len(), 8, "{frames:?}");
    let cseqs: Vec<u64> = frames.iter().filter_map(fake_trade_peer::cseq_of).collect();
    assert_eq!(cseqs, [1, 2, 3, 4, 5, 6, 7, 8], "per client, +1 per frame, in call order");

    assert!(matches!(&frames[0], TradeUp::Submit { order, .. } if order.client_order_id == "c1"));
    assert!(matches!(
        &frames[1],
        TradeUp::Cancel { coid, intent: WireCancelIntent::Unspecified, .. } if coid == "c1"
    ));
    assert!(matches!(
        &frames[2],
        TradeUp::Cancel { coid, intent: WireCancelIntent::RiskOff, .. } if coid == "c2"
    ));
    assert!(matches!(
        &frames[3],
        TradeUp::CancelBatch { coids, intent: WireCancelIntent::Routine, .. } if coids == &["c3", "c4"]
    ));
    assert!(matches!(
        &frames[4],
        TradeUp::Modify { order, new_qty: Some(q), new_price: None, .. }
            if order.client_order_id == "c5" && (*q - 2.0).abs() < f64::EPSILON
    ));
    assert!(matches!(&frames[5], TradeUp::Confirm { coid, .. } if coid == "c6"));
    assert!(matches!(&frames[6], TradeUp::AdoptRestored { pairs, .. } if pairs.len() == 1));
    assert!(matches!(&frames[7], TradeUp::SubmitBatch { orders, .. } if orders.len() == 2));
}

#[test]
fn the_default_cancel_door_is_the_flatten_safe_one() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.exec_client();
    exec.cancel("k1");
    exec.cancel_batch(&["k2".to_string(), "k3".to_string()]);
    let frames = wait_ups(&peer, WAIT, 2, |f| fake_trade_peer::cseq_of(f).is_some());
    assert!(matches!(&frames[0], TradeUp::Cancel { intent: WireCancelIntent::Unspecified, .. }));
    assert!(matches!(
        &frames[1],
        TradeUp::CancelBatch { intent: WireCancelIntent::Unspecified, .. }
    ));
    assert!(!CancelIntent::from(WireCancelIntent::default()).may_be_shed());
}

#[test]
fn a_boxed_client_forwards_the_wire_id_and_the_adoption() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    session.prefetch_wire_ids(&["hl-1".to_string(), "bn-2".to_string()]).expect("prefetched");
    let mut boxed: Box<dyn ExecutionClient + Send> = Box::new(session.exec_client());
    assert_eq!(boxed.wire_id_for("hl-1").as_deref(), Some("0xhl-1"), "the keccak-shaped answer");
    assert_eq!(boxed.wire_id_for("bn-2"), None, "asked about, and the venue echoes the coid");
    assert_eq!(boxed.wire_id_for("never-asked"), None, "a miss is not an echo, and does not panic");
    boxed.adopt_restored(&[("hl-1".to_string(), "BTC".to_string())]);
    let frames = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::AdoptRestored { .. }));
    assert_eq!(frames.len(), 1, "the boxed impl delegated adopt_restored to the wire");
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
        SymbolFactsError::Refused(
            "binance: no instrument NOPEUSDT in the venue's instrument list".into()
        )
    );
    let busy = session.symbol_facts("BUSYUSDT").expect_err("a busy daemon");
    assert_eq!(
        busy,
        SymbolFactsError::Unavailable(FACTS_BUSY.into()),
        "not a verdict on the symbol"
    );
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
            TradeUp::SymbolFacts { probe, .. } => Some(*probe),
            _ => None,
        })
        .collect();
    assert_eq!(asked, [true, false], "the lookup probes, the admission commits");
}

/// An old node sends no `probe` field: the daemon reads it as a COMMIT (the pre-field behaviour).
#[test]
fn a_symbol_facts_frame_without_probe_reads_as_a_commit() {
    let frame: TradeUp =
        serde_json::from_str(r#"{"SymbolFacts":{"rid":3,"symbol":"SOLUSDT.P"}}"#).expect("parses");
    assert!(matches!(frame, TradeUp::SymbolFacts { probe: false, rid: 3, .. }), "{frame:?}");
}

#[test]
fn symbol_facts_without_the_feature_are_refused_locally_and_nothing_is_sent() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != "gw_trade_symbol_facts");
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session.symbol_facts("SOLUSDT.P").expect_err("not advertised");
    assert_eq!(
        err,
        SymbolFactsError::Refused(
            "the gatewayhub does not advertise symbol facts (update it)".into()
        )
    );
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::SymbolFacts { .. })), "nothing sent");
}

#[test]
fn the_arming_table_and_the_preflight_legs_are_asked_over_a_fresh_trade_connection() {
    let table = ArmingTable {
        facts: [(
            "binance".to_string(),
            VenueFacts {
                adapter_present: true,
                addresses_accounts: true,
                takes_recon_trigger: false,
                grid_source: GridSource::InHand,
                process_exclusive: None,
                clock_decl_word: "measured".into(),
            },
        )]
        .into(),
        rows: vec![ArmingRow {
            venue: "binance".into(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: "none".into(),
            account_ids: vec![3],
            book: Some("spot".into()),
        }],
        key_changed: Vec::new(),
    };
    let legs = vec![
        LegResult::Clock {
            venue: "binance".into(),
            leg: ClockLeg::ServerTime { server_ms: 10, local_ms: 12, rtt_ms: 4 },
            reason: None,
            at_stake: None,
        },
        LegResult::Network { reading: NetReading::Alive, rounds: Some(7), since_ms: None },
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
    features.retain(|f| f != FEATURE_GW_ARMING && f != FEATURE_GW_PREFLIGHT);
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features));
    let err = rpc::arming_table(&peer.link()).expect_err("not advertised");
    assert!(err.to_string().contains("gw_arming"), "{err}");
    let err = rpc::preflight(&peer.link(), &[], PreflightStep::Batch).expect_err("not advertised");
    assert!(err.to_string().contains("gw_preflight"), "{err}");
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
    assert_eq!(session.link_state(), vike_gatewayhub_client::trade::LinkState::Up);
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
    let mut exec = session.exec_client();
    exec.submit(&order("d1"));
    exec.cancel_with_intent("d1", CancelIntent::RiskOff);
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
        matches!(i, vike_exec::Ingest::Event(vike_model::events::Event::OrderRejected(_)))
    });
    let rejected = fake_trade_peer::rejects(&got);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, "late");
}

#[test]
fn production_timings_only_ever_raise_the_protocol_floors() {
    use vike_gatewayhub_client::proto::{FETCH_STALL_FLOOR, GW_TRADE_READ_FLOOR};
    use vike_gatewayhub_client::trade::TradeTimings;
    let floors = TradeTimings::default();
    assert_eq!(floors.read_floor, GW_TRADE_READ_FLOOR);
    assert_eq!(floors.recon_stall_floor, FETCH_STALL_FLOOR);
    let below = TradeTimings::raised(Duration::from_millis(1), Duration::from_millis(1));
    assert_eq!(below, floors, "a setting below a floor is the floor");
    let above = TradeTimings::raised(Duration::from_secs(20), Duration::from_secs(90));
    assert_eq!(above.read_floor, Duration::from_secs(20));
    assert_eq!(above.recon_stall_floor, Duration::from_secs(90));
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
    let mut exec = session.exec_client();
    exec.submit(&order("p1"));
    exec.cancel_with_intent("p1", CancelIntent::RiskOff);
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
    let mut exec = session.exec_client();
    exec.cancel_with_intent("q1", CancelIntent::RiskOff);
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
    let mut exec = session.exec_client();
    exec.cancel_with_intent("r1", CancelIntent::RiskOff);
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
    let mut exec = session.exec_client();
    exec.cancel_with_intent("s1", CancelIntent::RiskOff);
    exec.begin_detach();
    assert!(session.is_stopping(), "phase one marks it, before the lease is closed");
    assert_ne!(
        session.link_state(),
        vike_gatewayhub_client::trade::LinkState::Closed,
        "the ack drain is still running: not closed yet, but stopping"
    );
    exec.detach();
    assert!(session.is_stopping());
}

#[test]
fn a_lost_link_during_a_facts_request_is_unavailable_never_a_refusal() {
    let peer = TradePeer::start(TradePeerConfig::new().with_script(|up| match up {
        TradeUp::SymbolFacts { .. } => vec![Step::Close],
        other => default_script()(other),
    }));
    let (session, _lane) = open(&peer, "tradehub-1");
    let err = session.symbol_facts("SOLUSDT.P").expect_err("the link is cut");
    assert!(matches!(err, SymbolFactsError::Unavailable(_)), "{err:?}");
}
