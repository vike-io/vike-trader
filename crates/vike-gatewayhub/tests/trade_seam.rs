//! The TRADE SEAM (`crate::trade::lane::TradeLane`) and its scripted lane (`testkit::trade`): the
//! contract W9b's real lane (`BridgeTrade`) implements and every later session test stands on.
//!
//! Compiled with feature `trade` (this crate's self dev-dependency turns it on for its own tests; a
//! build without it compiles this file to nothing).
#![cfg(feature = "trade")]

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_exec::{CancelIntent, Ingest};
use vike_gatewayhub::testkit::trade::{ExecCall, ScriptedStart, ScriptedTrade, TradeCall};
use vike_gatewayhub::trade::lane::{TradeLane, TradeTarget};
use vike_gatewayhub_client::proto::{AdapterStart, LegResult, NetReading, PreflightStep};
use vike_model::AccountTier;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted};

fn target(tier: AccountTier) -> TradeTarget {
    TradeTarget { venue: "binance".to_string(), account: None, tier }
}

fn start() -> AdapterStart {
    AdapterStart {
        rid: 1,
        symbol: "BTCUSDT".to_string(),
        declared_legs: Vec::new(),
        recon_enabled: true,
        takes_trigger: true,
        risk: None,
        market_slippage: None,
        halt_admit: Default::default(),
        withhold: None,
        account_leverage: None,
        read_account_ccy: false,
        route_key: "binance".to_string(),
    }
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

/// The lane is object-safe: the server holds it as `Arc<dyn TradeLane>`.
#[test]
fn the_lane_is_a_trait_object_and_its_venues_are_a_build_fact() {
    let lane: Arc<dyn TradeLane> = Arc::new(ScriptedTrade::new(&["binance", "bybit"]));
    assert_eq!(lane.venues(), vec!["binance".to_string(), "bybit".to_string()]);
    // A venue that is down is a refused start, never a missing advertisement.
    let down = ScriptedTrade::new(&["binance"]).with_start(
        "binance",
        None,
        ScriptedStart::refuse(ArmingBlock::AdapterFailed, "the venue login failed"),
    );
    assert_eq!(down.venues(), vec!["binance".to_string()]);
    let (events, _rx) = vike_exec::event_channel(4);
    let refused = down
        .start_adapter(&target(AccountTier::Demo), &start(), events, None)
        .expect_err("scripted refusal");
    assert_eq!(refused.block_word, ArmingBlock::AdapterFailed.word());
    assert_eq!(refused.refusal_text, "the venue login failed");
    assert_eq!(down.venues(), vec!["binance".to_string()]);
}

/// A started adapter hands back its facts, its exec client and its reconcile client; the lane keeps the
/// sender the bridge pumps push into, and a test plays the venue through it.
#[test]
fn a_started_adapter_carries_its_facts_and_its_event_lane() {
    let lane = ScriptedTrade::new(&["binance"]).with_start(
        "binance",
        None,
        ScriptedStart::live().contract_value(vike_model::ContractValue::inverse(1.0)),
    );
    let (events, mut rx) = vike_exec::event_channel(8);
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let started = lane
        .start_adapter(
            &target(AccountTier::Demo),
            &start(),
            events.routed("binance"),
            Some(trigger_tx),
        )
        .expect("scripted start");
    assert_eq!(started.bound_tier, AccountTier::Demo);
    assert_eq!(started.contract_value, Some(vike_model::ContractValue::inverse(1.0)));
    assert!(started.recon.is_some());

    let adapter = lane.adapter("binance", None).expect("the adapter handle");
    assert!(adapter.emit(Event::OrderAccepted(OrderAccepted {
        client_order_id: "c1".to_string(),
        venue_order_id: None,
        ts: 5,
    })));
    match rx.try_recv().expect("the event reached the lane's receiver") {
        Ingest::Event(Event::OrderAccepted(a)) => assert_eq!(a.client_order_id, "c1"),
        other => panic!("unexpected ingest: {other:?}"),
    }
    assert!(adapter.fire_trigger());
    assert!(trigger_rx.recv_timeout(Duration::from_secs(1)).is_ok());

    // The exec client records every call, in order; `cancel` and `cancel_with_intent` are one door.
    let mut client = started.client;
    client.submit(&order("c1"));
    client.cancel("c1");
    client.cancel_with_intent("c2", CancelIntent::RiskOff);
    client.cancel_batch_with_intent(&["c3".to_string(), "c4".to_string()], CancelIntent::Routine);
    client.confirm("c1");
    client.begin_detach();
    client.detach();
    client.detach();
    let calls = adapter.calls();
    assert_eq!(calls[0], ExecCall::Submit(order("c1")));
    assert_eq!(calls[1], ExecCall::Cancel { coid: "c1".into(), intent: CancelIntent::Unspecified });
    assert_eq!(calls[2], ExecCall::Cancel { coid: "c2".into(), intent: CancelIntent::RiskOff });
    assert_eq!(
        calls[3],
        ExecCall::CancelBatch {
            coids: vec!["c3".into(), "c4".into()],
            intent: CancelIntent::Routine
        }
    );
    assert_eq!(calls[4], ExecCall::Confirm("c1".into()));
    assert!(adapter.is_detached());
    // A detached adapter has dropped its lane: nothing more can be played into it.
    assert!(!adapter.emit(Event::OrderAccepted(OrderAccepted {
        client_order_id: "late".to_string(),
        venue_order_id: None,
        ts: 6,
    })));
}

/// The lane records what it was asked, a start included, before it answers.
#[test]
fn every_lane_call_is_recorded_in_arrival_order() {
    let lane = ScriptedTrade::new(&["binance"])
        .with_tier("binance", Some("ALT"), Ok(AccountTier::Live))
        .with_legs(vec![LegResult::Network {
            reading: NetReading::Alive,
            rounds: Some(3),
            since_ms: None,
        }]);
    assert_eq!(lane.account_tier("binance", None), Ok(AccountTier::Demo));
    assert_eq!(lane.account_tier("binance", Some("ALT")), Ok(AccountTier::Live));
    let _ = lane.arming_table();
    let answer = lane.preflight(&["binance".to_string()], PreflightStep::Batch);
    assert_eq!(answer.legs.len(), 1);
    let (events, _rx) = vike_exec::event_channel(4);
    let started = lane
        .start_adapter(&target(AccountTier::Demo), &start(), events, None)
        .expect("a default start is live");
    assert!(lane.record_identity(&target(AccountTier::Demo), started.bound_tier, None).is_ok());
    let calls = lane.calls();
    assert!(matches!(calls[0], TradeCall::AccountTier { .. }));
    assert!(matches!(calls[2], TradeCall::ArmingTable));
    assert_eq!(
        calls[3],
        TradeCall::Preflight { venues: vec!["binance".to_string()], step: PreflightStep::Batch }
    );
    assert!(matches!(calls[4], TradeCall::Start { trigger: false, .. }));
    assert!(matches!(calls[5], TradeCall::RecordIdentity { had_recon: false, .. }));
    assert_eq!(lane.starts(), 1);
}

/// An account that does not run is NOT RUNNING with the cause as a word, never a paper stand-in; and the
/// by-coid lookup is a per-venue fact the lane states.
#[test]
fn a_tier_that_cannot_be_stated_is_a_block_and_a_resolver_is_a_per_venue_fact() {
    let lane = ScriptedTrade::new(&["binance", "okx"])
        .with_tier("okx", None, Err(ArmingBlock::AccountInactive))
        .with_resolver("binance");
    assert_eq!(lane.account_tier("okx", None), Err(ArmingBlock::AccountInactive));
    assert!(lane.resolves_by_coid("binance"));
    assert!(!lane.resolves_by_coid("okx"));
}
