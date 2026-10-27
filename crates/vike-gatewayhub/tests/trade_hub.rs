//! **The trade HUB without a socket**: `TradeHub`, `AccountSession` and `Lease` driven through their public
//! surface (`open`, `Lease::handle`, `Lease::next`, `Lease::finish`) over a `ScriptedTrade` lane. What a
//! session is for, and what ends it: the adapter outlives a lease, a final detach tears it down and frees
//! the live lock, an orphan is reaped, a slow lease is told to reconcile, a tier the lane bound wrongly
//! never serves an order, and no frame, however extreme, panics the daemon.
#![cfg(feature = "trade")]

use std::sync::Arc;
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_gatewayhub::budget::ledger::Budget;
use vike_gatewayhub::testkit::trade::{
    ExecCall, ScriptedAdapter, ScriptedStart, ScriptedTrade, TradeCall,
};
use vike_gatewayhub::trade::TradeConfig;
use vike_gatewayhub::trade::lease::{Flow, Lease, Next};
use vike_gatewayhub::trade::session::{OpenRefusal, OpenRequest, Opened, TradeHub};
use vike_gatewayhub_client::proto::{
    AdapterReply, ArmingRow, ArmingTable, DetachPhase, TradeBye, TradeDown, TradeUp,
    WireCancelIntent, WireCursor, WireSymbolFacts,
};
use vike_model::{AccountTier, SymbolProperties};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use trade_rig::{accepted, order, start_args, wait_until};

const WAIT: Duration = Duration::from_secs(5);

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
        account_ccy_retry_after: vike_gatewayhub::trade::ACCOUNT_CCY_RETRY_AFTER,
        state_dir: None,
        halt_path: None,
    }
}

fn hub_with(lane: &ScriptedTrade, cfg: TradeConfig) -> Arc<TradeHub> {
    TradeHub::new(Arc::new(lane.clone()), cfg, Arc::new(Budget::new()))
}

fn request(client_id: &str, tier: AccountTier) -> OpenRequest {
    OpenRequest {
        client_id: client_id.to_string(),
        venue: "binance".to_string(),
        account: None,
        tier,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

fn open(hub: &TradeHub, client_id: &str) -> Opened {
    hub.open(request(client_id, AccountTier::Demo)).expect("the open is accepted")
}

/// The next frame the writer would write, within a short bound.
fn next(lease: &Lease) -> Next {
    lease.next(Duration::from_millis(500))
}

fn expect_frame(lease: &Lease) -> TradeDown {
    match next(lease) {
        Next::Frame(frame) => frame,
        other => panic!("expected a frame, got {other:?}"),
    }
}

fn start(lease: &Arc<Lease>) -> AdapterReply {
    let mut s = start_args();
    s.rid = 7;
    s.route_key = "binance".into();
    assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
    match expect_frame(lease) {
        TradeDown::AdapterReply { rid: 7, reply } => *reply,
        other => panic!("{other:?}"),
    }
}

fn adapter(lane: &ScriptedTrade) -> ScriptedAdapter {
    lane.adapter("binance", None).expect("the adapter was started")
}

#[test]
fn the_adapter_outlives_a_lease_and_the_same_start_is_answered_from_the_running_one() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let first = open(&hub, "tradehub-1");
    assert!(matches!(start(&first.lease), AdapterReply::Live { .. }));
    first.lease.close();
    first.lease.finish();
    assert!(
        !adapter(&lane).is_detached(),
        "a lease ending is not an adapter ending (owner ruling D6)"
    );
    assert_eq!(hub.sessions(), 1);
    assert_eq!(hub.leases(), 0);

    let second = open(&hub, "tradehub-1");
    assert!(matches!(start(&second.lease), AdapterReply::Live { .. }));
    assert_eq!(lane.starts(), 1, "the same start did not start a second adapter");
}

/// **The venue's word on an account's stated leverage rides the start reply** (decision 0181): the
/// lane's answer is in `AdapterReply::Live::leverage_cap`, a lane with none answers `None` (the
/// default every other venue has), and the same start answered from the RUNNING adapter asks the lane
/// afresh instead of replaying the first answer.
#[test]
fn the_start_reply_carries_the_lanes_leverage_confirmation() {
    let cap = |reply: AdapterReply| match reply {
        AdapterReply::Live { leverage_cap, .. } => leverage_cap,
        other => panic!("{other:?}"),
    };
    let plain = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&plain, cfg());
    assert_eq!(cap(start(&open(&hub, "tradehub-1").lease)), None, "nothing to confirm: none");

    let lane = ScriptedTrade::new(&["binance"]).with_leverage_cap("binance", Ok(20.0));
    let hub = hub_with(&lane, cfg());
    let first = open(&hub, "tradehub-1");
    assert_eq!(cap(start(&first.lease)), Some(Ok(20.0)));
    first.lease.close();
    first.lease.finish();
    let lane2 = lane.clone().with_leverage_cap("binance", Err("down".to_string()));
    assert_eq!(lane2.starts(), 1);
    let second = open(&hub, "tradehub-1");
    assert_eq!(
        cap(start(&second.lease)),
        Some(Err("down".to_string())),
        "the running adapter's reply is refreshed with the lane's current answer"
    );
    assert_eq!(lane.starts(), 1, "and it did not start a second adapter");
}

#[test]
fn a_different_start_tears_the_running_adapter_down_first() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let opened = open(&hub, "tradehub-1");
    start(&opened.lease);
    let old = adapter(&lane);
    let mut other = start_args();
    other.rid = 8;
    other.symbol = "ETHUSDT".into();
    other.route_key = "binance".into();
    opened.lease.handle(TradeUp::AdapterStart(other));
    assert!(matches!(expect_frame(&opened.lease), TradeDown::AdapterReply { rid: 8, .. }));
    assert!(old.is_detached(), "the old adapter was detached before the new one started");
    assert_eq!(lane.starts(), 2);
}

/// **The account's asked leverage is part of "the same start"** (decision 0182): a start that now asks
/// a venue that posts a leverage to hold N is a different start from the one the running adapter was
/// built for, so the old adapter is torn down and a fresh one is started (its bridge posts N), and
/// the SAME ask answered again is answered from the running adapter.
#[test]
fn a_changed_asked_leverage_is_a_different_start() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let opened = open(&hub, "tradehub-1");
    start(&opened.lease);
    let old = adapter(&lane);
    let mut asks = start_args();
    asks.rid = 8;
    asks.route_key = "binance".into();
    asks.account_leverage = Some(10.0);
    opened.lease.handle(TradeUp::AdapterStart(asks.clone()));
    assert!(matches!(expect_frame(&opened.lease), TradeDown::AdapterReply { rid: 8, .. }));
    assert!(old.is_detached(), "the adapter built for no ask is gone");
    assert_eq!(lane.starts(), 2);
    // The same ask again (a reconnecting tradehub): the running adapter answers.
    asks.rid = 9;
    opened.lease.handle(TradeUp::AdapterStart(asks));
    assert!(matches!(expect_frame(&opened.lease), TradeDown::AdapterReply { rid: 9, .. }));
    assert_eq!(lane.starts(), 2, "the same ask did not start a third adapter");
}

#[test]
fn a_final_detach_tears_the_adapter_down_and_retires_the_session() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let opened = open(&hub, "tradehub-1");
    start(&opened.lease);
    let adapter = adapter(&lane);
    assert_eq!(opened.lease.handle(TradeUp::Detach { phase: DetachPhase::Begin }), Flow::Continue);
    assert!(!adapter.is_detached(), "phase one raises nothing on the daemon's adapter");
    assert_eq!(opened.lease.handle(TradeUp::Detach { phase: DetachPhase::Final }), Flow::End);
    assert!(matches!(next(&opened.lease), Next::Bye(TradeBye::ClientClosed)));
    opened.lease.close();
    opened.lease.finish();
    assert!(adapter.is_detached());
    let calls = adapter.calls();
    let begin = calls.iter().position(|c| *c == ExecCall::BeginDetach).expect("begin_detach");
    let end = calls.iter().position(|c| *c == ExecCall::Detach).expect("detach");
    assert!(begin < end, "phase one before phase two: {calls:?}");
    assert_eq!(hub.sessions(), 0);
    // The next open starts from nothing: a new adapter, a new ring.
    let again = open(&hub, "tradehub-1");
    start(&again.lease);
    assert_eq!(lane.starts(), 2);
    assert_eq!(again.reply.event_ring_floor, Some(0));
}

#[test]
fn an_orphaned_session_is_reaped_after_the_ttl() {
    let lane = ScriptedTrade::new(&["binance"]);
    let mut config = cfg();
    config.orphan_ttl = Duration::from_millis(200);
    let hub = hub_with(&lane, config);
    let opened = open(&hub, "tradehub-1");
    start(&opened.lease);
    let adapter = adapter(&lane);
    opened.lease.close();
    opened.lease.finish();
    assert!(wait_until(WAIT, || adapter.is_detached()), "nobody came back and the adapter stayed");
    assert!(wait_until(WAIT, || hub.sessions() == 0));
}

#[test]
fn a_session_with_a_lease_is_never_reaped() {
    let lane = ScriptedTrade::new(&["binance"]);
    let mut config = cfg();
    config.orphan_ttl = Duration::from_millis(50);
    let hub = hub_with(&lane, config);
    let opened = open(&hub, "tradehub-1");
    start(&opened.lease);
    std::thread::sleep(Duration::from_millis(1_300));
    assert!(!adapter(&lane).is_detached());
    assert_eq!(hub.sessions(), 1);
    drop(opened);
}

#[test]
fn a_lease_whose_writer_falls_behind_the_ring_gets_replay_unavailable_with_the_floor() {
    let lane = ScriptedTrade::new(&["binance"]);
    let mut config = cfg();
    config.ring_events = 3;
    let hub = hub_with(&lane, config);
    let opened = open(&hub, "tradehub-1");
    start(&opened.lease);
    let adapter = adapter(&lane);
    for n in 1..=10 {
        assert!(adapter.emit(accepted(&format!("c{n}"))));
    }
    // The forwarder is a thread: wait until the ring has all ten.
    assert!(wait_until(WAIT, || hub
        .session_status("binance", None)
        .is_some_and(|s| s.last_seq == 10)));
    // The writer now asks for its next frame: seq 1 is gone (the floor is 8).
    match expect_frame(&opened.lease) {
        TradeDown::ReplayUnavailable { floor } => assert_eq!(floor, 8),
        other => panic!("{other:?}"),
    }
    let mut seqs = Vec::new();
    while let Next::Frame(f) = opened.lease.next(Duration::from_millis(50)) {
        if let TradeDown::Event { seq, replayed, .. } = f {
            assert!(!replayed, "a live event is not a replay");
            seqs.push(seq);
        }
    }
    assert_eq!(seqs, [8, 9, 10]);
}

#[test]
fn a_resume_inside_the_ring_replays_in_order_and_marks_the_replay() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let first = open(&hub, "tradehub-1");
    start(&first.lease);
    let adapter = adapter(&lane);
    let epoch = first.reply.gw_epoch;
    for n in 1..=3 {
        adapter.emit(accepted(&format!("c{n}")));
    }
    first.lease.close();
    first.lease.finish();
    for n in 4..=6 {
        adapter.emit(accepted(&format!("c{n}")));
    }
    let mut req = request("tradehub-1", AccountTier::Demo);
    req.resume = Some(WireCursor { gw_epoch: epoch, seq: 3, ts_ms: 0 });
    // The forwarder is a thread: let it stamp all six before the client comes back.
    assert!(wait_until(WAIT, || hub
        .session_status("binance", None)
        .is_some_and(|s| s.last_seq == 6)));
    let second = hub.open(req).expect("accepted");
    let mut got = Vec::new();
    while let Next::Frame(f) = second.lease.next(Duration::from_millis(100)) {
        if let TradeDown::Event { seq, replayed, .. } = f {
            got.push((seq, replayed));
        }
    }
    assert_eq!(got, [(4, true), (5, true), (6, true)]);
    adapter.emit(accepted("c7"));
    match expect_frame(&second.lease) {
        TradeDown::Event { seq: 7, replayed: false, .. } => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_full_control_queue_ends_the_lease_too_slow_after_what_it_holds() {
    let lane = ScriptedTrade::new(&["binance"]);
    let mut config = cfg();
    config.control_queue = 2;
    let hub = hub_with(&lane, config);
    let opened = open(&hub, "tradehub-1");
    for _ in 0..5 {
        opened.lease.handle(TradeUp::Ping);
    }
    assert!(matches!(expect_frame(&opened.lease), TradeDown::Pong));
    assert!(matches!(expect_frame(&opened.lease), TradeDown::Pong));
    assert!(matches!(next(&opened.lease), Next::Bye(TradeBye::TooSlow)));
}

#[test]
fn the_live_lock_is_claimed_at_open_held_for_the_session_and_freed_at_the_final_detach() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let lane = ScriptedTrade::new(&["binance"])
        .with_tier("binance", None, Ok(AccountTier::Live))
        .with_arming(live_table());
    let mut config = cfg();
    config.state_dir = Some(dir.path().to_path_buf());
    let hub = hub_with(&lane, config);
    let opened = hub.open(request("tradehub-1", AccountTier::Live)).expect("live opens");
    assert_eq!(
        opened.reply.project_token.len(),
        32,
        "the project token was made in the state directory"
    );
    // Another process on the account (stood in for by a second claim in this one) is refused.
    let refused = vike_boot::live_lock::LiveLock::acquire(dir.path(), "binance")
        .expect_err("the daemon holds the account");
    assert_eq!(refused.kind(), std::io::ErrorKind::AddrInUse);
    // A lease flap keeps the claim: the account is the session's, not the connection's.
    opened.lease.close();
    opened.lease.finish();
    assert!(vike_boot::live_lock::LiveLock::acquire(dir.path(), "binance").is_err());
    // A final detach frees it.
    let again = hub.open(request("tradehub-1", AccountTier::Live)).expect("reopens");
    again.lease.handle(TradeUp::Detach { phase: DetachPhase::Final });
    again.lease.close();
    again.lease.finish();
    assert!(
        vike_boot::live_lock::LiveLock::acquire(dir.path(), "binance").is_ok(),
        "the claim was not released at the final detach"
    );
}

#[test]
fn a_live_account_another_process_holds_is_a_lease_refusal_with_the_locks_own_words() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let held =
        vike_boot::live_lock::LiveLock::acquire(dir.path(), "binance").expect("held elsewhere");
    let lane = ScriptedTrade::new(&["binance"])
        .with_tier("binance", None, Ok(AccountTier::Live))
        .with_arming(live_table());
    let mut config = cfg();
    config.state_dir = Some(dir.path().to_path_buf());
    let hub = hub_with(&lane, config);
    match hub.open(request("tradehub-1", AccountTier::Live)) {
        // A LEASE refusal (the whole live mount is refused), not an account's `TradeRefused`.
        Err(OpenRefusal::LeaseHeld { holder_key_id, .. }) => {
            assert!(
                holder_key_id.contains("another live process already trades the binance account"),
                "{holder_key_id}"
            );
        }
        other => panic!("{:?}", other.map(|o| o.reply)),
    }
    assert_eq!(hub.sessions(), 0, "a refused open leaves no session behind");
    drop(held);
    assert!(hub.open(request("tradehub-1", AccountTier::Live)).is_ok());
}

#[test]
fn a_demo_account_claims_no_live_lock() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let lane = ScriptedTrade::new(&["binance"]).with_arming(live_table());
    let mut config = cfg();
    config.state_dir = Some(dir.path().to_path_buf());
    let hub = hub_with(&lane, config);
    let _opened = open(&hub, "tradehub-1");
    assert!(vike_boot::live_lock::LiveLock::acquire(dir.path(), "binance").is_ok());
}

fn live_table() -> ArmingTable {
    ArmingTable {
        facts: Default::default(),
        rows: vec![ArmingRow {
            venue: "binance".into(),
            label: None,
            tier: Some(AccountTier::Live),
            effective: Some(AccountTier::Live),
            block_word: ArmingBlock::None.word().to_string(),
            account_ids: vec![1],
            book: None,
        }],
        key_changed: Vec::new(),
    }
}

#[test]
fn a_client_bound_at_another_tier_never_serves_an_order() {
    let lane = ScriptedTrade::new(&["binance"]).with_start(
        "binance",
        None,
        ScriptedStart::live().bound_tier(AccountTier::Live),
    );
    let hub = hub_with(&lane, cfg());
    let opened = open(&hub, "tradehub-1");
    match start(&opened.lease) {
        AdapterReply::NotRunning { block_word, refusal_text } => {
            assert_eq!(block_word, ArmingBlock::AdapterFailed.word());
            assert!(refusal_text.contains("tier interlock"), "{refusal_text}");
        }
        other => panic!("{other:?}"),
    }
    let adapter = adapter(&lane);
    assert!(adapter.is_detached(), "the wrongly bound client was detached at once");
    // An order now has no adapter to reach: rejected, not sent.
    opened.lease.handle(TradeUp::Submit { cseq: 1, order: order("c1") });
    assert!(adapter.submitted().is_empty());
}

#[test]
fn a_refused_start_is_not_running_with_the_bridges_own_words() {
    let lane = ScriptedTrade::new(&["binance"]).with_start(
        "binance",
        None,
        ScriptedStart::refuse(ArmingBlock::NoCredentials, "binance: no demo key set is stored"),
    );
    let hub = hub_with(&lane, cfg());
    let opened = open(&hub, "tradehub-1");
    match start(&opened.lease) {
        AdapterReply::NotRunning { block_word, refusal_text } => {
            assert_eq!(block_word, "no_credentials");
            assert_eq!(refusal_text, "binance: no demo key set is stored");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_lease_table_has_a_bound() {
    let lane = ScriptedTrade::new(&["binance", "okx"]);
    let mut config = cfg();
    config.max_leases = 1;
    let hub = hub_with(&lane, config);
    let _first = open(&hub, "tradehub-1");
    let mut second = request("tradehub-1", AccountTier::Demo);
    second.venue = "okx".to_string();
    match hub.open(second) {
        Err(OpenRefusal::Error(text)) => assert!(text.contains("trade leases"), "{text}"),
        other => panic!("{:?}", other.map(|o| o.reply)),
    }
}

#[test]
fn the_daemons_stop_ends_every_lease_and_detaches_every_adapter() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let opened = open(&hub, "tradehub-1");
    start(&opened.lease);
    let adapter = adapter(&lane);
    let lease = Arc::clone(&opened.lease);
    let finisher = std::thread::Builder::new()
        .name("test-finisher".into())
        .spawn(move || {
            // The connection's thread: sees the Bye, ends, finishes.
            while !matches!(
                lease.next(Duration::from_millis(50)),
                Next::Bye(TradeBye::ServerStopping)
            ) {}
            lease.close();
            lease.finish();
        })
        .expect("spawn");
    hub.shutdown();
    finisher.join().expect("joined");
    assert!(adapter.is_detached());
    assert_eq!(hub.sessions(), 0);
    assert!(matches!(
        hub.open(request("late", AccountTier::Demo)),
        Err(OpenRefusal::Error(text)) if text.contains("stopping")
    ));
}

/// `SymbolFacts` over the real lease path: the lane's answer comes back as `SymbolFactsReply` with the
/// request's `rid`, for the lease's own venue, with no adapter started; a refusal keeps its words.
#[test]
fn a_symbol_facts_request_is_answered_by_the_lane_for_the_leases_venue() {
    let facts = WireSymbolFacts {
        properties: SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() },
        ..WireSymbolFacts::default()
    };
    let lane = ScriptedTrade::new(&["binance"])
        .with_symbol_facts("SOLUSDT.P", Ok(facts.clone()))
        .with_symbol_facts("NOPE", Err("binance: no instrument NOPE in the venue's list".into()));
    let hub = hub_with(&lane, cfg());
    let lease = open(&hub, "tradehub-1").lease;
    let ask = |rid, symbol: &str| {
        assert_eq!(
            lease.handle(TradeUp::SymbolFacts {
                rid,
                symbol: symbol.to_string(),
                probe: true,
                quoted: false
            }),
            Flow::Continue
        );
        match expect_frame(&lease) {
            TradeDown::SymbolFactsReply { rid: echoed, facts } => {
                assert_eq!(echoed, rid);
                facts
            }
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(ask(11, "SOLUSDT.P"), Ok(facts));
    assert_eq!(ask(12, "NOPE"), Err("binance: no instrument NOPE in the venue's list".to_string()));
    let asked: Vec<_> = lane
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            TradeCall::SymbolFacts { target, symbol } => Some((target.venue, symbol)),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked,
        [
            ("binance".to_string(), "SOLUSDT.P".to_string()),
            ("binance".to_string(), "NOPE".to_string())
        ]
    );
}

/// **A committed admission carries the leverage the lane confirmed for the symbol, and nothing else does**
/// (decision 0182's per-symbol twin). A look (`probe`) never asks the lane about leverage, so a Preview
/// changes nothing at the venue; neither does a commit before any adapter runs (there is no figure the
/// node asked); a commit with a running adapter asks the lane with THAT adapter's own start, and its
/// `Ok` rides the reply while a refusal, or a lane with no answer, leaves the facts exactly as the lookup
/// made them.
#[test]
fn a_committed_symbol_carries_the_leverage_the_lane_confirmed_and_a_look_never_does() {
    let facts = WireSymbolFacts {
        properties: SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() },
        ..WireSymbolFacts::default()
    };
    let lane = ScriptedTrade::new(&["binance"])
        .with_symbol_facts("SOLUSDT.P", Ok(facts.clone()))
        .with_symbol_facts("ETHUSDT.P", Ok(facts.clone()))
        .with_symbol_facts("BNBUSDT.P", Ok(facts.clone()))
        .with_symbol_leverage("binance", "SOLUSDT.P", Ok(10.0))
        .with_symbol_leverage("binance", "ETHUSDT.P", Err("an open position is held at 3x".into()));
    let hub = hub_with(&lane, cfg());
    let lease = open(&hub, "tradehub-1").lease;
    let ask = |rid, symbol: &str, probe| {
        let frame = TradeUp::SymbolFacts { rid, symbol: symbol.to_string(), probe, quoted: false };
        assert_eq!(lease.handle(frame), Flow::Continue);
        match expect_frame(&lease) {
            TradeDown::SymbolFactsReply { rid: echoed, facts } => {
                assert_eq!(echoed, rid);
                facts
            }
            other => panic!("{other:?}"),
        }
    };
    let leverage_asks = || {
        lane.calls()
            .into_iter()
            .filter_map(|c| match c {
                TradeCall::SymbolLeverage { symbol, start, .. } => {
                    Some((symbol, start.account_leverage))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    // No adapter yet: a commit is answered with the facts alone and the lane is not asked.
    assert_eq!(ask(1, "SOLUSDT.P", false), Ok(facts.clone()));
    assert!(leverage_asks().is_empty(), "no adapter, no figure the node asked");

    // The account's adapter comes up having asked for 10x.
    let mut s = start_args();
    s.rid = 7;
    s.route_key = "binance".into();
    s.account_leverage = Some(10.0);
    assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
    assert!(matches!(expect_frame(&lease), TradeDown::AdapterReply { rid: 7, .. }));

    // A LOOK: the same facts, and the lane is not asked a thing about leverage.
    assert_eq!(ask(2, "SOLUSDT.P", true), Ok(facts.clone()));
    assert!(leverage_asks().is_empty(), "a look must change nothing at the venue");

    // The COMMIT: the lane's confirmation rides the reply, next to the unchanged facts.
    let committed = ask(3, "SOLUSDT.P", false).expect("granted");
    assert_eq!(committed.leverage, Some(10.0));
    assert_eq!(committed.properties, facts.properties);

    // A refusal and a lane with no answer for the symbol: the facts as the lookup made them, no figure.
    assert_eq!(ask(4, "ETHUSDT.P", false), Ok(facts.clone()));
    assert_eq!(ask(5, "BNBUSDT.P", false), Ok(facts.clone()));
    assert_eq!(
        leverage_asks(),
        [
            ("SOLUSDT.P".to_string(), Some(10.0)),
            ("ETHUSDT.P".to_string(), Some(10.0)),
            ("BNBUSDT.P".to_string(), Some(10.0)),
        ],
        "every commit with a running adapter asked, with that adapter's own start"
    );
}

/// **A symbol quoted in another currency than the account's is answered only to a node that asked to
/// convert** (decision 0183, phase 1). A plain look goes to the lane's `symbol_facts` (whose fence refuses
/// it); the same look with `quoted` goes to `symbol_facts_quoted` and carries the quote currency and the
/// rate; a lane with no quoted answer scripted answers what the plain one does, and the commit follows the
/// same door (a daemon that would refuse the commit would leave the reconcile client untold).
#[test]
fn a_quoted_look_reaches_the_quoted_lane_and_a_plain_one_does_not() {
    let plain = WireSymbolFacts {
        properties: SymbolProperties { tick_size: 0.00001, step_size: 1.0, ..Default::default() },
        ..WireSymbolFacts::default()
    };
    let cross = WireSymbolFacts {
        quote_ccy: Some("GBP".into()),
        quote_rate: Some(1.35),
        rate_as_of_ms: Some(1_700_000_000_000),
        ..plain.clone()
    };
    let lane = ScriptedTrade::new(&["binance"])
        .with_symbol_facts("EURGBP", Err("oanda: EUR_GBP is quoted in GBP ... ".into()))
        .with_quoted_symbol_facts("EURGBP", Ok(cross.clone()))
        .with_symbol_facts("GBPUSD", Ok(plain.clone()));
    let hub = hub_with(&lane, cfg());
    let lease = open(&hub, "tradehub-1").lease;
    let ask = |rid, symbol: &str, probe, quoted| {
        let frame = TradeUp::SymbolFacts { rid, symbol: symbol.to_string(), probe, quoted };
        assert_eq!(lease.handle(frame), Flow::Continue);
        match expect_frame(&lease) {
            TradeDown::SymbolFactsReply { rid: echoed, facts } => {
                assert_eq!(echoed, rid);
                facts
            }
            other => panic!("{other:?}"),
        }
    };

    assert!(ask(1, "EURGBP", true, false).expect_err("the fence").contains("quoted in GBP"));
    assert_eq!(ask(2, "EURGBP", true, true), Ok(cross.clone()), "a look from a node that converts");
    assert_eq!(ask(3, "EURGBP", false, true), Ok(cross), "the commit goes the same way");
    assert_eq!(ask(4, "GBPUSD", true, true), Ok(plain), "no quoted answer scripted: the plain one");
    let routed: Vec<(&str, bool)> = lane
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            TradeCall::SymbolFacts { .. } => Some(("plain", false)),
            TradeCall::SymbolFactsQuoted { .. } => Some(("quoted", true)),
            _ => None,
        })
        .collect();
    assert_eq!(routed, [("plain", false), ("quoted", true), ("quoted", true), ("quoted", true)]);
}

/// **The account's currency rides the start reply** (decision 0183, item 1): the lane's answer is in
/// `AdapterReply::Live::account_ccy`, and a lane that states none leaves it absent (the engine keeps its label).
#[test]
fn the_start_reply_carries_the_currency_the_venue_states() {
    let ccy = |reply: AdapterReply| match reply {
        AdapterReply::Live { account_ccy, .. } => account_ccy,
        other => panic!("{other:?}"),
    };
    let plain = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&plain, cfg());
    assert_eq!(ccy(start(&open(&hub, "tradehub-1").lease)), None, "no currency stated: none");

    let lane = ScriptedTrade::new(&["binance"]).with_account_currency("binance", "USD");
    let hub = hub_with(&lane, cfg());
    assert_eq!(ccy(start(&open(&hub, "tradehub-1").lease)), Some("USD".to_string()));
}

/// **A missed account currency is not kept for the adapter's life**: the first ask came back empty (a stalled
/// host, a dropped packet), and the node would refuse every symbol quoted in another currency until the daemon
/// restarted. The same start answered from the RUNNING adapter asks the lane again, but never sooner than the
/// lane's retry window after the last ask (a flapping link is not a flood of venue sessions); an answer is then
/// kept in the adapter's reply, so the starts after it ask nothing more.
#[test]
fn a_missed_account_currency_is_asked_again_at_a_later_start_and_not_cached_for_the_adapters_life()
{
    let ccy = |reply: AdapterReply| match reply {
        AdapterReply::Live { account_ccy, .. } => account_ccy,
        other => panic!("{other:?}"),
    };
    // Long enough that the in-window start below cannot fall outside it on a loaded machine.
    let retry = Duration::from_secs(2);
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, TradeConfig { account_ccy_retry_after: retry, ..cfg() });
    let first = open(&hub, "tradehub-1");
    assert_eq!(ccy(start(&first.lease)), None, "the first ask missed");
    assert_eq!(lane.account_currency_asks(), 1);
    first.lease.close();
    first.lease.finish();

    // The venue recovers. A start INSIDE the window asks nothing and is answered as before.
    let _ = lane.clone().with_account_currency("binance", "USD");
    let second = open(&hub, "tradehub-1");
    assert_eq!(ccy(start(&second.lease)), None, "inside the retry window: not asked");
    assert_eq!(lane.account_currency_asks(), 1);
    second.lease.close();
    second.lease.finish();

    // After the window the next start asks again, gets the currency, and the adapter keeps it.
    std::thread::sleep(retry + Duration::from_millis(200));
    let third = open(&hub, "tradehub-1");
    assert_eq!(ccy(start(&third.lease)), Some("USD".to_string()), "healed by the retry");
    assert_eq!(lane.account_currency_asks(), 2);
    third.lease.close();
    third.lease.finish();
    let fourth = open(&hub, "tradehub-1");
    assert_eq!(ccy(start(&fourth.lease)), Some("USD".to_string()), "kept in the adapter's reply");
    assert_eq!(lane.account_currency_asks(), 2, "a currency in hand is never asked again");
    assert_eq!(lane.starts(), 1, "and no start started a second adapter");
}

/// [`start`] with `read_account_ccy` as the node sets it, and the currency the reply carries. A different
/// `reads` is a different start (`same_start`), so a test keeps one value for a whole run of repeats.
fn start_reading_currency(lease: &Arc<Lease>, rid: u64, reads: bool) -> Option<String> {
    let mut s = start_args();
    s.rid = rid;
    s.route_key = "binance".into();
    s.read_account_ccy = reads;
    assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
    match expect_frame(lease) {
        TradeDown::AdapterReply { reply, .. } => match *reply {
            AdapterReply::Live { account_ccy, .. } => account_ccy,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

/// **A venue whose currency read is BUDGETED (ibkr, ig: `currency_read_is_budgeted`) is re-asked after a first
/// read that missed only by a start that asks for it, and never sooner than the retry window.** The ig branch of
/// the review re-asked at every start; the shared throttle (`account_ccy_retry_after`) is now what bounds it, for
/// the venue whose ask costs a login and a read of the account allowance. Inside the window: nothing; after it, one
/// ask (`Some` when the venue recovered); then the answer is kept. The scripted lane stands for either venue.
#[test]
fn a_budgeted_venue_re_asks_a_missed_currency_only_after_the_window() {
    let retry = Duration::from_secs(2);
    let lane = ScriptedTrade::new(&["binance"]).with_budgeted_currency_read("binance");
    let hub = hub_with(&lane, TradeConfig { account_ccy_retry_after: retry, ..cfg() });
    let first = open(&hub, "tradehub-1");
    assert_eq!(start_reading_currency(&first.lease, 7, true), None, "the first read missed");
    assert_eq!(lane.account_currency_asks(), 1);

    // The venue recovers; repeats of the same start INSIDE the window ask nothing, however many.
    let _ = lane.clone().with_account_currency("binance", "USD");
    for rid in 8..11 {
        assert_eq!(start_reading_currency(&first.lease, rid, true), None, "inside the window");
    }
    assert_eq!(
        lane.account_currency_asks(),
        1,
        "a budgeted venue is not re-asked within the window"
    );

    // AFTER the window the next repeat asks once, heals, and the adapter keeps the answer.
    std::thread::sleep(retry + Duration::from_millis(200));
    assert_eq!(start_reading_currency(&first.lease, 11, true), Some("USD".to_string()));
    assert_eq!(lane.account_currency_asks(), 2);
    assert_eq!(start_reading_currency(&first.lease, 12, true), Some("USD".to_string()));
    assert_eq!(lane.account_currency_asks(), 2, "a currency in hand is never asked again");
    assert_eq!(lane.starts(), 1, "no repeat started a second adapter");
}

/// **A start that did NOT ask for the currency of a budgeted venue reads nothing, at any later start, however old
/// the first ask** (an ig or ibkr account with no exposure ceiling pays the venue nothing for it).
#[test]
fn a_budgeted_venue_is_never_re_asked_by_a_start_that_did_not_ask() {
    let retry = Duration::from_millis(300);
    let lane = ScriptedTrade::new(&["binance"]).with_budgeted_currency_read("binance");
    let hub = hub_with(&lane, TradeConfig { account_ccy_retry_after: retry, ..cfg() });
    let first = open(&hub, "tradehub-1");
    // (The scripted lane counts the first start's ask whatever the start says; the real one reads only on `reads`.)
    assert_eq!(start_reading_currency(&first.lease, 7, false), None);
    let asks = lane.account_currency_asks();
    let _ = lane.clone().with_account_currency("binance", "USD");
    std::thread::sleep(retry + Duration::from_millis(200));
    for rid in 8..11 {
        assert_eq!(
            start_reading_currency(&first.lease, rid, false),
            None,
            "a start that did not ask"
        );
    }
    assert_eq!(lane.account_currency_asks(), asks, "nothing was read for a start that did not ask");
}

/// **An UNBUDGETED venue (ctrader, dukascopy, oanda: a free read) is re-asked after the window whatever the start
/// says**, because the field is ignored for it: the retry that heals a missed first read still happens.
#[test]
fn an_unbudgeted_venue_is_re_asked_after_the_window_even_by_a_start_that_did_not_ask() {
    let retry = Duration::from_millis(300);
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, TradeConfig { account_ccy_retry_after: retry, ..cfg() });
    let first = open(&hub, "tradehub-1");
    assert_eq!(start_reading_currency(&first.lease, 7, false), None, "the first read missed");
    assert_eq!(lane.account_currency_asks(), 1);
    let _ = lane.clone().with_account_currency("binance", "USD");
    std::thread::sleep(retry + Duration::from_millis(200));
    assert_eq!(start_reading_currency(&first.lease, 8, false), Some("USD".to_string()));
    assert_eq!(lane.account_currency_asks(), 2, "one re-ask healed it");
}

/// A venue that states no currency at all (every non-converting venue) is asked at most once per window by a
/// later start, never per start, and the answer stays absent.
#[test]
fn a_venue_with_no_currency_is_retried_at_most_once_per_window() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let first = open(&hub, "tradehub-1");
    start(&first.lease);
    first.lease.close();
    first.lease.finish();
    for _ in 0..3 {
        let again = open(&hub, "tradehub-1");
        start(&again.lease);
        again.lease.close();
        again.lease.finish();
    }
    assert_eq!(lane.account_currency_asks(), 1, "the default window is thirty seconds");
}

/// No frame, however extreme, panics the daemon: every one is answered, ignored or a protocol `Bye`.
#[test]
fn extreme_frames_never_panic_the_daemon_and_an_over_bound_one_is_a_protocol_bye() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let huge = "x".repeat(10_000);
    // (frame, whether the daemon ends the lease with a protocol violation)
    let frames: Vec<(TradeUp, bool)> = vec![
        (TradeUp::Submit { cseq: u64::MAX, order: order("c1") }, false),
        (TradeUp::Submit { cseq: 0, order: order("") }, true),
        (TradeUp::Cancel { cseq: 1, coid: huge.clone(), intent: WireCancelIntent::RiskOff }, true),
        (
            TradeUp::CancelBatch {
                cseq: 2,
                coids: vec![String::new()],
                intent: WireCancelIntent::Routine,
            },
            true,
        ),
        (
            TradeUp::Modify {
                cseq: 3,
                order: order("c1"),
                new_qty: Some(f64::MAX),
                new_price: Some(f64::MIN_POSITIVE),
            },
            false,
        ),
        (TradeUp::AdoptRestored { cseq: 4, pairs: vec![(huge.clone(), huge.clone())] }, true),
        (TradeUp::WireIds { rid: u64::MAX, coids: vec![huge.clone()] }, true),
        (TradeUp::ResolveOrders { rid: 0, coids: vec![String::new()] }, true),
        (TradeUp::SubmitBatch { cseq: 5, orders: vec![order("a"); 9000] }, true),
        (TradeUp::Confirm { cseq: 6, coid: String::new() }, true),
        (TradeUp::IdentityRecord { rid: u64::MAX }, false),
        (TradeUp::TradeStatus { rid: 0 }, false),
        (TradeUp::SymbolFacts { rid: 0, symbol: huge.clone(), probe: true, quoted: false }, true),
        (
            TradeUp::SymbolFacts { rid: 1, symbol: "BTCUSDT.P".into(), probe: true, quoted: false },
            false,
        ),
    ];
    for (frame, violation) in frames {
        let label = format!("{frame:?}");
        let label = label.chars().take(60).collect::<String>();
        let lease = open(&hub, "tradehub-1").lease;
        let flow = lease.handle(frame);
        if violation {
            assert_eq!(flow, Flow::End, "{label}");
            let mut protocol = false;
            while let Next::Frame(_) | Next::Bye(_) = lease.next(Duration::from_millis(20)) {
                protocol = true;
            }
            assert!(protocol, "{label}: a violation is answered with a Bye");
        } else {
            assert_eq!(flow, Flow::Continue, "{label}");
        }
        lease.close();
        lease.finish();
    }
    // The hub is alive and serving after all of it.
    assert!(hub.open(request("probe", AccountTier::Demo)).is_ok());
}

#[test]
fn a_reconcile_trigger_with_no_client_attached_waits_for_the_next_lease() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let first = open(&hub, "tradehub-1");
    let mut s = start_args();
    s.rid = 7;
    s.route_key = "binance".into();
    s.takes_trigger = true;
    first.lease.handle(TradeUp::AdapterStart(s));
    assert!(matches!(expect_frame(&first.lease), TradeDown::AdapterReply { .. }));
    let adapter = adapter(&lane);
    // While attached the trigger is forwarded at once.
    assert!(adapter.fire_trigger());
    assert!(matches!(expect_frame(&first.lease), TradeDown::ReconcileNow { .. }));
    first.lease.close();
    first.lease.finish();
    // While detached it is remembered.
    assert!(adapter.fire_trigger());
    std::thread::sleep(Duration::from_millis(100));
    let second = open(&hub, "tradehub-1");
    assert!(
        matches!(expect_frame(&second.lease), TradeDown::ReconcileNow { .. }),
        "the owed reconcile is the first thing the next client hears"
    );
}

#[test]
fn the_words_under_which_an_account_runs_are_exactly_the_blocks_that_run() {
    use vike_gatewayhub::trade::session::block_runs;
    for block in ArmingBlock::ALL {
        assert_eq!(block_runs(block.word()), block.runs(), "{block:?}");
    }
    assert!(block_runs("none"), "`none` is `ArmingBlock::None`: nothing refuses the account");
    assert!(
        !block_runs(ArmingBlock::None.as_str()),
        "the badge spelling is not a wire word: the empty string names no block"
    );
    assert!(!block_runs("a word from a newer daemon"), "an unknown word never runs");
}

#[test]
fn a_labelled_live_account_claims_its_own_route_keys_lock_and_leaves_the_default_one_free() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let lane = ScriptedTrade::new(&["binance"])
        .with_tier("binance", Some("ALT"), Ok(AccountTier::Live))
        .with_arming(ArmingTable {
            facts: Default::default(),
            rows: vec![ArmingRow {
                venue: "binance".into(),
                label: Some("ALT".into()),
                tier: Some(AccountTier::Live),
                effective: Some(AccountTier::Live),
                block_word: "none".to_string(),
                account_ids: vec![2],
                book: None,
            }],
            key_changed: Vec::new(),
        });
    let mut config = cfg();
    config.state_dir = Some(dir.path().to_path_buf());
    let hub = hub_with(&lane, config);
    let mut req = request("tradehub-1", AccountTier::Live);
    req.account = Some("ALT".to_string());
    let _opened = hub.open(req).expect("the labelled live account opens");
    assert!(
        vike_boot::live_lock::LiveLock::acquire(dir.path(), "binance#ALT").is_err(),
        "LIVE-binance#ALT.lock is held: the route key names the account"
    );
    assert!(
        vike_boot::live_lock::LiveLock::acquire(dir.path(), "binance").is_ok(),
        "the default account of the venue is another account"
    );
}

#[test]
fn a_session_recreated_after_a_final_detach_names_a_new_epoch_and_the_old_cursor_is_unavailable() {
    // W8b finding 1: the epoch names a RING, so a session built again must not reuse the old one. A
    // cursor `(E, N)` from the retired session would otherwise be read as "I am up to date" by a client
    // whose guard drops every `seq <= N` of the new ring as a duplicate.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let first = open(&hub, "tradehub-1");
    start(&first.lease);
    for n in 1..=3 {
        adapter(&lane).emit(accepted(&format!("c{n}")));
    }
    assert!(wait_until(WAIT, || hub
        .session_status("binance", None)
        .is_some_and(|s| s.last_seq == 3)));
    let old_epoch = first.reply.gw_epoch;
    assert_eq!(first.lease.handle(TradeUp::Detach { phase: DetachPhase::Final }), Flow::End);
    first.lease.close();
    first.lease.finish();
    assert_eq!(hub.sessions(), 0, "the final detach retired the session");

    let mut req = request("tradehub-1", AccountTier::Demo);
    req.resume = Some(WireCursor { gw_epoch: old_epoch, seq: 3, ts_ms: 0 });
    let second = hub.open(req).expect("accepted");
    assert_ne!(
        second.reply.gw_epoch, old_epoch,
        "a new session is a new ring: it must not wear the retired ring's epoch"
    );
    match expect_frame(&second.lease) {
        TradeDown::ReplayUnavailable { floor } => assert_eq!(floor, 0, "an empty ring"),
        other => panic!("a retired session's cursor must be unavailable, got {other:?}"),
    }
}

#[test]
fn a_session_recreated_after_the_orphan_ttl_names_a_new_epoch() {
    let lane = ScriptedTrade::new(&["binance"]);
    let mut config = cfg();
    config.orphan_ttl = Duration::from_millis(200);
    let hub = hub_with(&lane, config);
    let first = open(&hub, "tradehub-1");
    start(&first.lease);
    adapter(&lane).emit(accepted("c1"));
    first.lease.close();
    first.lease.finish();
    assert!(wait_until(WAIT, || hub.sessions() == 0), "the orphan was reaped");

    let second = open(&hub, "tradehub-1");
    assert_ne!(second.reply.gw_epoch, first.reply.gw_epoch);
    assert_eq!(
        second.reply.received_through_cseq, 0,
        "a new session has no watermark, and the epoch change is what tells the client so"
    );
}

#[test]
fn a_link_flap_inside_one_session_keeps_its_epoch() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let first = open(&hub, "tradehub-1");
    start(&first.lease);
    first.lease.close();
    first.lease.finish();
    let second = open(&hub, "tradehub-1");
    assert_eq!(second.reply.gw_epoch, first.reply.gw_epoch, "the same ring, the same epoch");
}

#[test]
fn an_order_with_unbounded_contingency_or_combo_fields_is_a_protocol_bye() {
    // W8b finding 6: only the coid, symbol and venue were bounded; the other client-named strings and
    // lists were limited by the 256 KiB frame alone.
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub_with(&lane, cfg());
    let mut crowded = vec![];
    let mut o = order("c1");
    o.linked_order_ids = vec!["l".to_string(); 5_000];
    crowded.push(("linked ids", o));
    let mut o = order("c1");
    o.linked_order_ids = vec!["l".repeat(10_000)];
    crowded.push(("a long linked id", o));
    let mut o = order("c1");
    o.parent_order_id = Some("p".repeat(10_000));
    crowded.push(("a long parent id", o));
    let mut o = order("c1");
    o.order_list_id = Some("p".repeat(10_000));
    crowded.push(("a long list id", o));
    let mut o = order("c1");
    o.contingency_type = Some("OCO".repeat(1_000));
    crowded.push(("a long contingency type", o));
    let mut o = order("c1");
    o.combo_legs = vec![combo_leg(); 5_000];
    crowded.push(("combo legs", o));
    for (what, o) in crowded {
        let lease = open(&hub, "tradehub-1").lease;
        assert_eq!(lease.handle(TradeUp::Submit { cseq: 1, order: o }), Flow::End, "{what}");
        assert!(matches!(next(&lease), Next::Bye(TradeBye::Protocol(_))), "{what}");
        lease.close();
        lease.finish();
    }
    // The ordinary shapes still pass.
    let lease = open(&hub, "tradehub-1").lease;
    let mut ok = order("c2");
    ok.linked_order_ids = vec!["a".into(), "b".into()];
    ok.parent_order_id = Some("parent".into());
    ok.contingency_type = Some("OCO".into());
    assert_eq!(lease.handle(TradeUp::Submit { cseq: 1, order: ok }), Flow::Continue);
}

fn combo_leg() -> vike_model::ComboLeg {
    vike_model::ComboLeg { symbol: "BTC-C".into(), ratio: 1 }
}
