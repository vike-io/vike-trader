//! **The trade class on the wire, from a hostile or confused peer, and the two stateless queries.**
//!
//! A raw peer sends what a client library never would: a frame that is not a verb, a value over its
//! bound, a length prefix over the ceiling. The daemon answers each with a protocol `Bye` or a closed
//! connection, never a panic and never a hang, and a well-behaved client opens fine afterwards.
//! The arming table and the preflight legs are asked through the client crate's real one-shots.
#![cfg(feature = "trade")]

use std::time::Duration;

use vike_gatewayhub::testkit::gateway::trade_keys;
use vike_gatewayhub::testkit::trade::TradeLaneScript;
use vike_gatewayhub_client::proto::{
    ArmingRowWire, ArmingTableWire, ClockRiskInfoWire, NetReadingWire, PreflightAnswer,
    PreflightLegWire, PreflightPlanWire, PreflightStep, Request, Response, ServerTimeGapWire,
    TradeBye, TradeDown, TradeUp,
};
use vike_gatewayhub_client::trade::rpc;
use vike_hub_protocol::auth::Scope;
use vike_model::AccountTier;
use vike_model::{ClockRisk, GridSource, VenueAdapterDeclarationView};

use vike_gatewayhub::testkit::{raw_peer, trade_rig};

use raw_peer::Raw;
use trade_rig::{Rig, open, order, quick_config, rig, rig_with, start_args};
use vike_gatewayhub::testkit::gateway::TradeKit;

fn raw_lease(rig: &Rig) -> Raw {
    let mut raw = Raw::open(rig.gw.addr(), &trade_keys(), Scope::Trade);
    raw.send(&Request::TradeOpen {
        client_id: "hostile".to_string(),
        venue: "binance".to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
        row: None,
    });
    assert!(matches!(raw.recv::<Response>(), Response::TradeOpened(_)));
    raw
}

/// The next frame that is not a heartbeat.
fn next_down(raw: &mut Raw) -> TradeDown {
    crate::next_down(raw, Duration::from_secs(5), |f| matches!(f, TradeDown::Heartbeat))
}

#[test]
fn a_frame_that_is_not_a_verb_ends_the_lease_with_a_protocol_bye() {
    let rig = rig();
    let mut raw = raw_lease(&rig);
    raw.send(&serde_json::json!({ "NoSuchVerb": { "x": 1 } }));
    match next_down(&mut raw) {
        TradeDown::Bye(TradeBye::Protocol(words)) => {
            assert!(words.contains("undecodable"), "{words}");
        }
        other => panic!("{other:?}"),
    }
    assert!(raw.closed_within(Duration::from_secs(3)), "the connection stays open after a Bye");
}

/// An older tradehub's `Submit` still naming the order client id under its retired key ends the lease
/// BY NAME: decoding it would default the id away and send the order unnamed.
#[test]
fn a_retired_order_client_id_key_ends_the_lease_by_name() {
    let rig = rig();
    let mut raw = raw_lease(&rig);
    let (old, new) = vike_model::orders::client_id::RETIRED_ORDER_CLIENT_ID_KEYS[0];
    let mut submit =
        serde_json::to_value(TradeUp::Submit { command_sequence: 1, order: order("c1") }).unwrap();
    let request = submit["Submit"]["order"].as_object_mut().expect("the order is an object");
    let id = request.remove(new).expect("the order names its id");
    request.insert(old.to_string(), id);
    raw.send(&submit);
    match next_down(&mut raw) {
        TradeDown::Bye(TradeBye::Protocol(words)) => {
            assert!(words.contains(&format!("`{old}` is now `{new}`")), "{words}");
        }
        other => panic!("{other:?}"),
    }
    assert!(raw.closed_within(Duration::from_secs(3)), "the connection stays open after a Bye");
}

#[test]
fn a_value_over_its_bound_ends_the_lease_with_a_protocol_bye() {
    let rig = rig();
    for hostile in [
        TradeUp::Submit { command_sequence: 1, order: order(&"x".repeat(300)) },
        TradeUp::Cancel {
            command_sequence: 1,
            order_client_id: String::new(),
            intent: vike_model::OrderCancelIntent::Unspecified,
        },
        TradeUp::WireIds { rid: 1, order_client_ids: vec!["y".repeat(1000)] },
    ] {
        let mut raw = raw_lease(&rig);
        raw.send(&hostile);
        assert!(
            matches!(next_down(&mut raw), TradeDown::Bye(TradeBye::Protocol(_))),
            "{hostile:?}"
        );
        assert!(raw.closed_within(Duration::from_secs(3)));
    }
}

#[test]
fn a_length_prefix_over_the_up_ceiling_closes_the_connection_and_the_daemon_survives() {
    let rig = rig();
    let mut raw = raw_lease(&rig);
    // 1 MiB declared, over the 256 KiB trade ceiling: a framing fault, the stream is desynced.
    raw.send_bytes(&(1_048_576u32).to_be_bytes());
    assert!(raw.closed_within(Duration::from_secs(3)));
    // And an ordinary client still opens afterwards.
    let (session, _lane) = open(&rig, "tradehub-1");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        vike_gatewayhub_client::proto::AdapterReply::Running { .. }
    ));
}

fn table() -> ArmingTableWire {
    ArmingTableWire {
        properties: [(
            "binance".to_string(),
            VenueAdapterDeclarationView {
                adapter_present: true,
                addresses_accounts: true,
                takes_reconcile_trigger: false,
                grid_source: GridSource::InHand,
                process_exclusive: None,
                clock_declaration_word: vike_model::ClockDeclarationKind::Wired,
            },
        )]
        .into(),
        rows: vec![ArmingRowWire {
            venue: "binance".into(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: "none".to_string(),
            account_ids: vec![3],
            book: Some("spot".into()),
        }],
        key_changed: Vec::new(),
        start_inputs: Default::default(),
    }
}

#[test]
fn the_arming_table_and_the_preflight_legs_are_the_lanes_and_bounded() {
    let legs = vec![
        PreflightLegWire::Clock {
            venue: "binance".into(),
            leg: ServerTimeGapWire::ServerTime { server_ms: 10, local_ms: 11, rtt_ms: 2 },
            reason: None,
            at_stake: None,
        },
        PreflightLegWire::Network {
            reading: NetReadingWire::Alive,
            rounds: Some(7),
            since_ms: None,
        },
    ];
    let rig = rig_with(
        TradeLaneScript::new(&["binance"]).with_arming(table()).with_legs(legs.clone()),
        quick_config(),
        TradeKit::default(),
    );
    let link = rig.gw.trade_link();
    assert_eq!(rpc::arming_table(&link).expect("the table"), table());
    let batch =
        rpc::preflight(&link, &["binance".to_string()], PreflightStep::Batch).expect("legs");
    assert_eq!(batch.legs, legs);
    assert_eq!(batch.plan, None);

    // A venue this build has no adapter for is refused in words, and nothing is probed.
    let err = rpc::preflight(&link, &["bybit".to_string()], PreflightStep::Batch)
        .expect_err("not served");
    assert!(err.to_string().contains("bybit") && err.to_string().contains("binance"), "{err}");
    let many: Vec<String> = (0..100).map(|n| format!("v{n}")).collect();
    let err = rpc::preflight(&link, &many, PreflightStep::Batch).expect_err("too many");
    assert!(err.to_string().contains("100 venues"), "{err}");
    let err =
        rpc::preflight(&link, &["v".repeat(500)], PreflightStep::Batch).expect_err("a long name");
    assert!(err.to_string().contains("500 bytes"), "{err}");
    let probes = rig
        .lane
        .calls()
        .iter()
        .filter(|c| matches!(c, vike_gatewayhub::testkit::trade::TradeCall::Preflight { .. }))
        .count();
    assert_eq!(probes, 1, "only the served query reached the lane");
}

/// **Every preflight question reaches the lane with its step and its venues, and its answer comes back
/// whole**: the plan travels in its own field, the legs of a read in `legs`. The daemon holds no schedule
/// of its own: a resample is the CALLER's second question, and the lane sees it as one.
#[test]
fn each_preflight_step_reaches_the_lane_and_its_answer_comes_back() {
    let lists = PreflightPlanWire {
        clock_risks: [(
            "binance".to_string(),
            ClockRiskInfoWire { class: ClockRisk::NonceWindow, remedy: "sync it".to_string() },
        )]
        .into(),
        wired_clock_reads: 1,
        credential_venues: vec!["binance".to_string()],
    };
    let answer = lists.clone();
    let rig = rig_with(
        TradeLaneScript::new(&["binance"]).with_arming(table()).with_preflight(
            move |venues, step| match step {
                PreflightStep::Plan => {
                    PreflightAnswer { legs: Vec::new(), plan: Some(answer.clone()) }
                }
                other => PreflightAnswer {
                    legs: vec![PreflightLegWire::Network {
                        reading: NetReadingWire::Dead(format!("{other:?} {venues:?}")),
                        rounds: None,
                        since_ms: Some(5),
                    }],
                    plan: None,
                },
            },
        ),
        quick_config(),
        TradeKit::default(),
    );
    let link = rig.gw.trade_link();
    let venues = ["binance".to_string()];
    let plan = rpc::preflight(&link, &venues, PreflightStep::Plan).expect("the plan");
    assert_eq!(plan.plan, Some(lists), "the plan crosses in its own field");
    assert!(plan.legs.is_empty(), "a plan measures nothing");
    for step in [PreflightStep::Clock, PreflightStep::Credential, PreflightStep::Network] {
        let got = rpc::preflight(&link, &venues, step).expect("a read");
        assert_eq!(got.plan, None, "{step:?}");
        assert_eq!(
            got.legs,
            vec![PreflightLegWire::Network {
                reading: NetReadingWire::Dead(format!("{step:?} [\"binance\"]")),
                rounds: None,
                since_ms: Some(5),
            }],
            "{step:?}: the lane saw the step and the venues"
        );
    }
    let asked: Vec<PreflightStep> = rig
        .lane
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            vike_gatewayhub::testkit::trade::TradeCall::Preflight { step, .. } => Some(step),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked,
        [
            PreflightStep::Plan,
            PreflightStep::Clock,
            PreflightStep::Credential,
            PreflightStep::Network
        ]
    );
}
