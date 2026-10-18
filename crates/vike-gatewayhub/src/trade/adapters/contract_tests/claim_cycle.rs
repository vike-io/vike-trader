//! **The process-exclusive claim's whole life, through the lane and through the session** (W9c).
//!
//! The claim used to be committed "for the life of the process", and that process was the tradehub: a
//! graceful stop, an orphan TTL or a bridge-panic restart killed the JForex child together with the
//! claim. The process is the gatewayhub now and it outlives every tradehub, so a claim kept past its
//! adapter refuses the next `AdapterStart` (`already claimed`) until the daemon restarts. Each test
//! below starts an exclusive adapter, ends it the way one of those paths does, and starts it again; the
//! double-start refusal while the adapter is alive keeps its text.
//!
//! The resource is this file's own (the claim set is process-global and shared by every test of the
//! binary), and a probe asks for THAT name.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{AdapterReply, AdapterStart, DetachPhase, TradeDown, TradeUp};

use super::*;
use crate::budget::ledger::Budget;
use crate::trade::TradeConfig;
use crate::trade::adapters::bridge_trade::BridgeTrade;
use crate::trade::lane::{TradeLane, TradeTarget};
use crate::trade::lease::{Flow, Lease, Next};
use crate::trade::session::{OpenRequest, TradeHub};

const RESOURCE: &str = "the claim-cycle test's resource";

static CYCLE: Planted = Planted {
    exclusive: true,
    resource: Some(RESOURCE),
    ..Planted::new("ig", Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }, true)
};
static CYCLE_REG: [VenueRow; 1] = [VenueRow::Adapter(&CYCLE)];

// The panicking twin: its bound client panics in `submit`.
const PANIC_RESOURCE: &str = "the claim-cycle panic test's resource";
static PANICS: Planted = Planted {
    exclusive: true,
    resource: Some(PANIC_RESOURCE),
    client_panics: true,
    ..Planted::new("ig", Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }, true)
};
static PANICS_REG: [VenueRow; 1] = [VenueRow::Adapter(&PANICS)];

fn lane_over(registry: &'static [VenueRow]) -> Arc<BridgeTrade> {
    let policy = policy("ig", AccountTier::Demo);
    Arc::new(BridgeTrade::with_snapshot(registry, HashMap::new(), snapshot_of(&policy)))
}

fn target() -> TradeTarget {
    TradeTarget { venue: "ig".to_string(), account: None, tier: AccountTier::Demo }
}

fn start_args() -> AdapterStart {
    AdapterStart {
        rid: 7,
        symbol: "EURUSD".into(),
        declared_legs: Vec::new(),
        recon_enabled: false,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        route_key: "ig".into(),
    }
}

fn is_free(resource: &str) -> bool {
    crate::trade::adapters::exclusive::claim(resource).is_some()
}

/// One lane start of the account, over a fresh event channel.
fn lane_start(
    lane: &BridgeTrade,
) -> Result<crate::trade::lane::StartedAdapter, crate::trade::lane::AdapterRefusal> {
    let (tx, _rx) = vike_exec::event_channel(8);
    lane.start_adapter(&target(), &start_args(), tx, None)
}

// ---- the lane --------------------------------------------------------------------------------------

/// Start, end, start again: the adapter's end (its `StartedAdapter` dropped, which is what a session's
/// teardown does) hands the resource back, and while it is alive a second start is refused with the
/// bridge's own `already claimed` sentence before the bridge is asked.
#[test]
fn a_started_adapter_holds_the_claim_until_it_ends_and_the_next_start_runs() {
    let lane = lane_over(&CYCLE_REG);
    let first = lane_start(&lane).expect("the first start runs");
    assert!(!is_free(RESOURCE), "a running resource holds its claim");

    let refused = lane_start(&lane).expect_err("a second start while the adapter is alive");
    assert_eq!(
        refused.block_word,
        crate::trade::adapters::arming::wire_word(ArmingBlock::AdapterFailed)
    );
    assert!(
        refused.refusal_text.ends_with(": already claimed"),
        "the bridge's own sentence: {}",
        refused.refusal_text
    );
    assert_eq!(CYCLE.mounts.load(Ordering::SeqCst), 1, "the refusal came before the bridge");

    drop(first);
    assert!(is_free(RESOURCE), "the adapter ended, so the claim ended with it");
    let again = lane_start(&lane).expect("the next start after the adapter ended runs");
    assert_eq!(CYCLE.mounts.load(Ordering::SeqCst), 2);
    drop(again);
}

/// Several starts racing for the one resource: exactly one runs, the others are refused with the
/// existing text, and once the winner is dropped the resource is free again.
#[test]
fn concurrent_starts_of_one_exclusive_resource_admit_exactly_one() {
    const RACERS: usize = 6;
    static RACE: Planted = Planted {
        exclusive: true,
        resource: Some("the claim-race test's resource"),
        ..Planted::new(
            "ig",
            Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            true,
        )
    };
    static RACE_REG: [VenueRow; 1] = [VenueRow::Adapter(&RACE)];
    let lane = lane_over(&RACE_REG);
    let barrier = std::sync::Barrier::new(RACERS);
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                s.spawn(|| {
                    barrier.wait();
                    lane_start(&lane)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("a racer ended without a panic")).collect()
    });
    let running = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(running, 1, "one resource, one runner");
    for refusal in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(refusal.refusal_text.ends_with(": already claimed"), "{}", refusal.refusal_text);
    }
    assert_eq!(RACE.mounts.load(Ordering::SeqCst), 1, "only the winner reached the bridge");
    drop(results);
    assert!(is_free("the claim-race test's resource"));
}

// ---- the session -----------------------------------------------------------------------------------

fn cfg() -> TradeConfig {
    TradeConfig {
        heartbeat: Duration::from_millis(100),
        read_floor: Duration::from_millis(300),
        ring_events: 64,
        dedup_coids: 64,
        command_queue: 64,
        control_queue: 64,
        event_channel: 64,
        max_leases: 8,
        orphan_ttl: Duration::from_secs(600),
        recon_progress: Duration::from_millis(100),
        recon_page_bytes: 4096,
        fence_drain: Duration::from_millis(500),
        state_dir: None,
        halt_path: None,
    }
}

fn hub_over(lane: Arc<BridgeTrade>, cfg: TradeConfig) -> Arc<TradeHub> {
    TradeHub::new(lane, cfg, Arc::new(Budget::new()))
}

fn open(hub: &TradeHub) -> crate::trade::session::Opened {
    hub.open(OpenRequest {
        client_id: "tradehub-1".to_string(),
        venue: "ig".to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    })
    .unwrap_or_else(|e| panic!("the open is accepted: {e:?}"))
}

fn start_over(lease: &Arc<Lease>) -> AdapterReply {
    assert_eq!(lease.handle(TradeUp::AdapterStart(start_args())), Flow::Continue);
    // Acks of earlier commands may precede the reply.
    loop {
        match lease.next(Duration::from_secs(5)) {
            Next::Frame(TradeDown::AdapterReply { rid: 7, reply }) => return *reply,
            // Acks and the reconcile request a panicked bridge raises.
            Next::Frame(TradeDown::CmdAck { .. } | TradeDown::ReconcileNow { .. }) => {}
            other => panic!("expected the adapter reply, got {other:?}"),
        }
    }
}

fn wait_until(bound: Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// A graceful tradehub stop: its final detach retires the session and ends the adapter. The next
/// tradehub's `AdapterStart` runs.
///
/// Its own resource, `Planted` and registry, as the other session tests have: the claim set and the
/// `mounts` counter are process-global, so a threaded `cargo test` run (the CI lines that use plain
/// `cargo test`) interleaved this test with the lane test above on the one resource and one counter, which
/// process-per-test nextest hides.
#[test]
fn a_final_detach_frees_the_claim_and_the_next_tradehub_starts_the_adapter() {
    const DETACH_RESOURCE: &str = "the claim-final-detach test's resource";
    static DETACHED: Planted = Planted {
        exclusive: true,
        resource: Some(DETACH_RESOURCE),
        ..Planted::new(
            "ig",
            Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            true,
        )
    };
    static DETACHED_REG: [VenueRow; 1] = [VenueRow::Adapter(&DETACHED)];
    let hub = hub_over(lane_over(&DETACHED_REG), cfg());
    let first = open(&hub);
    assert!(matches!(start_over(&first.lease), AdapterReply::Live { .. }));
    assert!(!is_free(DETACH_RESOURCE), "the session's adapter holds the resource");

    assert_eq!(first.lease.handle(TradeUp::Detach { phase: DetachPhase::Final }), Flow::End);
    first.lease.close();
    first.lease.finish();
    assert_eq!(hub.sessions(), 0, "the session is retired");
    assert!(is_free(DETACH_RESOURCE), "…and its claim with it");

    let second = open(&hub);
    assert!(
        matches!(start_over(&second.lease), AdapterReply::Live { .. }),
        "the next tradehub's start is not refused `already claimed`"
    );
    assert_eq!(DETACHED.mounts.load(Ordering::SeqCst), 2);
}

/// The orphan TTL: nobody came back, the reaper retires the session, the claim goes with the adapter.
#[test]
fn an_orphan_ttl_frees_the_claim() {
    static ORPHAN: Planted = Planted {
        exclusive: true,
        resource: Some("the claim-orphan test's resource"),
        ..Planted::new(
            "ig",
            Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            true,
        )
    };
    static ORPHAN_REG: [VenueRow; 1] = [VenueRow::Adapter(&ORPHAN)];
    let mut config = cfg();
    config.orphan_ttl = Duration::from_millis(200);
    let hub = hub_over(lane_over(&ORPHAN_REG), config);
    let opened = open(&hub);
    assert!(matches!(start_over(&opened.lease), AdapterReply::Live { .. }));
    assert!(!is_free("the claim-orphan test's resource"));
    opened.lease.close();
    opened.lease.finish();
    assert!(wait_until(Duration::from_secs(5), || hub.sessions() == 0), "the orphan was reaped");
    assert!(is_free("the claim-orphan test's resource"), "the reaped adapter handed it back");
    let again = open(&hub);
    assert!(matches!(start_over(&again.lease), AdapterReply::Live { .. }));
}

/// A bridge panic: the adapter is dead, and the same `AdapterStart` builds a fresh one, which must
/// win the claim the dead one held.
#[test]
fn a_bridge_panic_restart_wins_the_claim_the_dead_adapter_held() {
    let hub = hub_over(lane_over(&PANICS_REG), cfg());
    let opened = open(&hub);
    assert!(matches!(start_over(&opened.lease), AdapterReply::Live { .. }));
    assert_eq!(PANICS.mounts.load(Ordering::SeqCst), 1);
    assert!(!is_free(PANIC_RESOURCE));

    let order = vike_model::OrderRequest {
        client_order_id: "boom".to_string(),
        venue: "ig".to_string(),
        symbol: "EURUSD".to_string(),
        side: 1,
        qty: 1.0,
        price: Some(1.0),
        ts: 1_700_000_000_000,
        ..Default::default()
    };
    assert_eq!(opened.lease.handle(TradeUp::Submit { cseq: 1, order }), Flow::Continue);
    // The panic marks the adapter dead; the restart tears it down (releasing the claim) and starts a
    // fresh one, so the answer is `Live` again and the bridge was asked a second time.
    assert!(wait_until(Duration::from_secs(5), || {
        hub.session_status("ig", None).is_some_and(|s| !s.adapter_up)
    }));
    assert!(
        matches!(start_over(&opened.lease), AdapterReply::Live { .. }),
        "the restart after a bridge panic is not refused `already claimed`"
    );
    assert_eq!(PANICS.mounts.load(Ordering::SeqCst), 2, "a fresh adapter was started");
}
