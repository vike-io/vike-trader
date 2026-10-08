//! The STRATEGY verbs (split-plane B4) over the wire, and the delayed-ACK pin.

use super::*;

// ---------------------------------------------------------------------------------------------
// The STRATEGY verbs (split-plane B4)
// ---------------------------------------------------------------------------------------------

/// Accepts every order (so it rests, modifiable) and echoes `OrderModified` on modify — the same
/// shape as vike-core's `wiring/live_params.rs` harness client, standing in for a venue so the
/// maker's re-quote is observable in the snapshot. Used ONLY by the B4 update-params test below,
/// which mounts a FIXED-SPREAD `SpreadMaker` (deterministic quotes; the A-S paper mount the other
/// tests use never quotes without a feed, which is exactly why it cannot show a re-tune landing).
#[derive(Default)]
struct ModifiableClient {
    events: std::collections::VecDeque<vike_model::events::Event>,
}

impl vike_exec::ExecutionClient for ModifiableClient {
    fn submit(&mut self, request: &vike_model::OrderRequest) {
        use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn modify(
        &mut self,
        order: &vike_model::OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) {
        use vike_model::events::{Event, OrderModified};
        self.events.push_back(Event::OrderModified(OrderModified {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: None,
            new_qty,
            new_price,
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.events.pop_front()
    }
}

/// ⚠ THE B4 WRITE GATE, end to end: a `WireCommand::UpdateParams` sent over the REAL control
/// server lands in the core as the REAL `Command::UpdateParams` — asserted on the effect the core
/// EXPOSES (the mounted `SpreadMaker`'s next quote re-prices/re-sizes at the NEW knobs, in place),
/// never on internals — and it leaves an `"update_params"` audit record carrying the rationale.
///
/// The choreography mirrors vike-core's `mounted_spreadmaker_retunes_live_without_unmount_or_cancel`
/// (the in-process proof); what THIS test adds is the wire: server edge limits → `accept_command` →
/// `lower_command`'s serde of the core's own `StrategyParams` JSON → `CommandSink`. Ordering is
/// guaranteed because the server enqueues the command BEFORE writing its Ack, and the third quote
/// is only sent after the Ack is read — commands and ticks share the one ordered ingest lane.
#[test]
fn update_params_over_the_wire_retunes_the_mounted_strategy() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicI64;
    use vike_core::{CoreConfig, StrategyMount, spawn_core};
    use vike_exec::{
        Account, BalanceMode, ExecutionEngine, OrderStatus, QuoteUpdate, RiskGate, RiskLimits,
    };
    use vike_model::QuoteTick;

    test_init();

    // A FIXED-SPREAD maker on the production runtime (mid ± 0.5, qty 1.0) over the echoing client.
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ModifiableClient::default(),
        "binance",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1.0,
        clock: Box::new(move || t.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(vike_mm::SpreadMaker::new(1.0, 0.5)),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    // The REAL server over this core, control enabled — the same serve() the daemon runs.
    let addr = serve_core(&handle, NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let quote = |ts: i64| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };

    // Rest bid/ask off the first quotes (mid 100.1, half_spread 0.5, qty 1.0), settle in the cell.
    ticks.quote(quote(1)).expect("quote 1");
    ticks.quote(quote(2)).expect("quote 2");
    assert!(
        wait_until(5, || cell.load_full().orders.len() == 2),
        "the fixed-spread maker rests exactly two orders off the first quotes"
    );

    // The B4 write, OVER THE WIRE: widen half_spread 0.5 → 1.0 and grow qty 1.0 → 2.0. The params
    // payload is built from the REAL core type — the wire carries the core's own serde JSON.
    let retune = vike_core::StrategyParams::SpreadMaker(vike_core::SpreadMakerParams {
        qty: 2.0,
        half_spread: 1.0,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: vike_model::QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: None,
    });
    let mut ctl = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut ctl,
        &Request::Command {
            cmd: WireCommand::UpdateParams {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                params: serde_json::to_value(&retune).expect("core params serialize"),
            },
            reason: Some("widen for the event".into()),
        },
    )
    .expect("update_params command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "", "account-wide-verb convention: empty coid"),
        other => panic!("expected Ack for update_params, got {other:?}"),
    }

    // The FIRST quote after the Ack: the maker re-prices/re-sizes its resting orders in place.
    ticks.quote(quote(3)).expect("quote 3");
    let mid = 0.5 * (100.0_f64 + 100.2);
    assert!(
        wait_until(5, || {
            let snap = cell.load_full();
            snap.orders
                .iter()
                .any(|o| o.side == 1 && o.price.map(f64::to_bits) == Some((mid - 1.0).to_bits()))
        }),
        "the resting bid never re-priced at the NEW half_spread — the wire UpdateParams did not \
         reach the mounted strategy"
    );
    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "no fault: {:?}", snap.fault);
    assert_eq!(snap.orders.len(), 2, "re-tuned IN PLACE — no cancel + resubmit");
    assert!(
        snap.orders.iter().all(|o| o.status == OrderStatus::Accepted),
        "both sides still resting after the re-tune"
    );
    let bid = snap.orders.iter().find(|o| o.side == 1).expect("resting bid");
    let ask = snap.orders.iter().find(|o| o.side == -1).expect("resting ask");
    assert_eq!(bid.price.unwrap().to_bits(), (mid - 1.0).to_bits(), "bid at the NEW half_spread");
    assert_eq!(ask.price.unwrap().to_bits(), (mid + 1.0).to_bits(), "ask at the NEW half_spread");
    assert_eq!(bid.qty.to_bits(), 2.0_f64.to_bits(), "bid re-sized to the NEW qty");
    assert_eq!(ask.qty.to_bits(), 2.0_f64.to_bits(), "ask re-sized to the NEW qty");

    // …and the ONE acceptance path recorded it: kind "update_params", empty coid, the rationale.
    let entry = audit_entry_with_kind("update_params").expect("the accepted re-tune was audited");
    assert_eq!(entry.coid, "", "account-wide-verb convention in the audit record too");
    assert_eq!(entry.reason.as_deref(), Some("widen for the event"));

    handle.shutdown_and_join();
}

/// An UNDECODABLE params payload is refused at the lowering edge — `Response::Error` naming the
/// verb, nothing reaching the core — over the REAL wire (the unit half lives in `server.rs`'s
/// `server::tests::lower_command`; this pins the operator-visible reply shape).
#[test]
fn an_undecodable_update_params_payload_is_refused_over_the_wire() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut ctl = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut ctl,
        &command(WireCommand::UpdateParams {
            venue: "polymarket".into(),
            symbol: TOKEN.into(),
            interval: "1m".into(),
            params: serde_json::json!({"NoSuchStrategyParams": {"qty": 1.0}}),
        }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("response") {
        Response::Error(msg) => assert!(msg.contains("update_params"), "names the verb: {msg}"),
        other => panic!("an undecodable params payload must be Error, got {other:?}"),
    }
}

/// ⚠ THE B5 WRITE GATE, end to end: a `WireCommand::MountStrategy` sent over the REAL control
/// server lands in a core that spawned with NO mount at all and adds one at runtime — resolved
/// through the daemon's REAL `mount_factory` (registry `buy_hold`) — asserted on what the core
/// EXPOSES (the published `mounts` view gains the row); the matching `UnmountStrategy` removes it
/// again. Both leave their audit records (`"mount_strategy"` / `"unmount_strategy"`) carrying the
/// rationale. A DUPLICATE mount of the same live id is Ack'd at the edge (the command is
/// well-formed) and refused BY THE CORE as a recent-events note — the `UpdateParams`
/// unknown-target contract, pinned here so the wire behaviour cannot drift from the white-box
/// vike-core tests (`runtime_mount_tests`), which own the cancel/state-sidecar half.
#[test]
fn mount_and_unmount_over_the_wire_change_the_live_mount_set() {
    use vike_core::{CoreConfig, spawn_core};
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};

    test_init();

    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let config = CoreConfig {
        seed_cash: 1.0,
        // The daemon's REAL resolver — the same one both `main.rs` arms inject.
        strategy_factory: Some(vike_tradehub::mount_factory::strategy_factory()),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    assert!(cell.load_full().mounts.is_empty(), "spawned with no mount");

    let addr = serve_core(&handle, NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mount_cmd = WireCommand::MountStrategy {
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        controller_id: Some("rt-hold".into()),
        name: Some("buy_hold".into()),
        rhai: None,
        params: serde_json::json!({"size": 1.0}),
    };

    // (1) MOUNT over the wire: Ack, the published mounts view gains the row (+ the residual row),
    // and the audit trail records the verb with its rationale.
    let mut ctl = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut ctl,
        &Request::Command {
            cmd: mount_cmd.clone(),
            reason: Some("second strategy for the session".into()),
        },
    )
    .expect("mount command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "", "account-wide-verb convention"),
        other => panic!("expected Ack for mount_strategy, got {other:?}"),
    }
    assert!(
        wait_until(5, || cell
            .load_full()
            .mounts
            .iter()
            .any(|m| m.venue == "binance" && m.symbol == "BTCUSDT")),
        "the runtime mount never appeared in the published mounts view: {:?}",
        cell.load_full().mounts
    );
    let entry = audit_entry_with_kind("mount_strategy").expect("the accepted mount was audited");
    assert_eq!(entry.coid, "");
    assert_eq!(entry.reason.as_deref(), Some("second strategy for the session"));

    // (2) A DUPLICATE of the LIVE id: well-formed, so the edge Acks — the CORE refuses, surfaced
    // in recent-events, and the mount set does not grow.
    write_frame(&mut ctl, &command(mount_cmd)).expect("duplicate mount command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("a well-formed duplicate is edge-Ack'd, got {other:?}"),
    }
    assert!(
        wait_until(5, || cell
            .load_full()
            .recent_events
            .iter()
            .any(|l| l.contains("MOUNT REFUSED: duplicate strategy-mount id `rt_hold`"))),
        "the core's duplicate refusal never surfaced: {:?}",
        cell.load_full().recent_events
    );

    // (3) UNMOUNT over the wire: Ack, the row disappears, the verb is audited.
    write_frame(
        &mut ctl,
        &Request::Command {
            cmd: WireCommand::UnmountStrategy { controller_id: "rt-hold".into() },
            reason: Some("done for the day".into()),
        },
    )
    .expect("unmount command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, ""),
        other => panic!("expected Ack for unmount_strategy, got {other:?}"),
    }
    assert!(
        wait_until(5, || cell.load_full().mounts.is_empty()),
        "the unmounted row never left the published mounts view: {:?}",
        cell.load_full().mounts
    );
    let entry =
        audit_entry_with_kind("unmount_strategy").expect("the accepted unmount was audited");
    assert_eq!(entry.reason.as_deref(), Some("done for the day"));

    handle.shutdown_and_join();
}

/// A mount spec the daemon's profile machinery refuses — an unknown strategy name — is refused AT
/// THE EDGE with a `Response::Error` carrying the profile vocabulary's own message; nothing
/// reaches the core. (The unit half lives in `mount_factory`'s tests; this pins the
/// operator-visible reply shape, the `an_undecodable_update_params_payload…` twin.)
#[test]
fn an_unknown_strategy_mount_is_refused_over_the_wire() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut ctl = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut ctl,
        &command(WireCommand::MountStrategy {
            venue: "polymarket".into(),
            account: None,
            symbol: TOKEN.into(),
            interval: "1m".into(),
            controller_id: None,
            name: Some("no_such_strategy".into()),
            rhai: None,
            params: serde_json::json!({}),
        }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("response") {
        Response::Error(msg) => {
            assert!(msg.contains("mount_strategy"), "names the verb: {msg}");
            assert!(msg.contains("unknown strategy"), "carries the profile refusal: {msg}");
        }
        other => panic!("an unknown strategy name must be Error, got {other:?}"),
    }
}

/// A node whose publisher carries NO identity block (`publish::spawn(.., None)` — the shape this
/// file's `spawn_node` builds) answers `StrategyStatus` with an HONEST error, never a fabricated
/// empty status. (The mounted-truth answer is `observe_roundtrip`'s
/// `observe_scope_can_strategy_status_but_not_update_params`, whose node publishes an identity.)
#[test]
fn strategy_status_without_an_identity_block_is_an_error() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut obs = observe_stream(addr, OBSERVE_KEY);
    write_frame(&mut obs, &Request::StrategyStatus).expect("status request");
    match read_frame::<_, Response>(&mut obs).expect("response") {
        Response::Error(msg) => assert!(msg.contains("identity"), "names the gap: {msg}"),
        other => panic!("an identity-less node must answer Error, got {other:?}"),
    }
}

/// Two `Ping`s PIPELINED in one write on an authenticated connection: the node's second `Pong` is
/// written while its first is still unacknowledged, and must not wait for this client's delayed
/// ACK — the accepted socket carries `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`),
/// which is also what sends an observe stream's consecutive snapshots without that wait.
/// `crates/vike-datahub/tests/wire_latency.rs`'s `back_to_back_answers_cost_no_delayed_ack` argues
/// the shape and the budget (half of Linux's 40 ms delayed-ACK floor per pair).
#[test]
fn back_to_back_answers_cost_no_delayed_ack() {
    use std::io::Write;

    const PAIRS: u32 = 200;
    const BUDGET: Duration = Duration::from_millis(20 * PAIRS as u64);
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), false);
    let mut s = observe_stream(addr, OBSERVE_KEY);
    let pong = |s: &mut TcpStream| match read_frame::<_, Response>(s).expect("read") {
        Response::Pong => {}
        other => panic!("a Ping must be answered with Pong, got {other:?}"),
    };
    let mut two = Vec::new();
    write_frame(&mut two, &Request::Ping).expect("frame the first Ping");
    write_frame(&mut two, &Request::Ping).expect("frame the second Ping");
    let started = Instant::now();
    for _ in 0..PAIRS {
        s.write_all(&two).expect("write both requests in ONE write");
        pong(&mut s);
        pong(&mut s);
    }
    let took = started.elapsed();
    assert!(
        took < BUDGET,
        "{PAIRS} pipelined pairs took {took:?} (budget {BUDGET:?}): the node's second answer is \
         waiting for the client's delayed ACK of its first — the accepted socket is missing \
         TCP_NODELAY (vike_node_proto::frame::configure_node_stream)"
    );
}
