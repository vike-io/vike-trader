//! The STRATEGY verbs (split-plane B4) over the wire, and the delayed-ACK pin.

use super::*;
use std::time::Duration;
use vike_log::poll::wait_until;

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

impl vike_execution::ExecutionClient for ModifiableClient {
    fn submit(&mut self, request: &vike_model::OrderRequest) {
        use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            order_client_id: request.order_client_id.clone(),
            venue_order_id: None,
            timestamp: request.timestamp,
        }));
    }
    fn cancel(&mut self, _order_client_id: &str) {}
    fn modify(
        &mut self,
        order: &vike_model::OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) {
        use vike_model::events::{Event, OrderModified};
        self.events.push_back(Event::OrderModified(OrderModified {
            order_client_id: order.order_client_id.clone(),
            venue_order_id: None,
            new_qty,
            new_price,
            new_trigger_price: None,
            timestamp: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.events.pop_front()
    }
}

/// ⚠ THE B4 WRITE GATE, end to end: a `CommandWire::UpdateParams` sent over the REAL control
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
    use vike_core::{CoreConfig, Mount, spawn_core};
    use vike_execution::{Account, BalanceMode, Engine, QuoteUpdate, RiskGate};
    use vike_marketdata::TickQuote;
    use vike_model::OrderStatus;
    use vike_model::RiskLimits;
    use vike_strategy_lab::makers::spread_maker::SpreadMaker;

    test_init();

    // A FIXED-SPREAD maker on the production runtime (mid ± 0.5, qty 1.0) over the echoing client.
    let engine = Engine::new(
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
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(SpreadMaker::new(1.0, 0.5)),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    // The REAL server over this core, control enabled — the same serve() the daemon runs.
    let addr = serve_core(&handle, HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let quote = |ts: i64| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: Default::default(),
        },
    };

    // Rest bid/ask off the first quotes (mid 100.1, half_spread 0.5, qty 1.0), settle in the cell.
    ticks.quote(quote(1)).expect("quote 1");
    ticks.quote(quote(2)).expect("quote 2");
    assert!(
        wait_until(Duration::from_secs(5), || cell.load_full().orders.len() == 2),
        "the fixed-spread maker rests exactly two orders off the first quotes"
    );

    // The B4 write, OVER THE WIRE: widen half_spread 0.5 → 1.0 and grow qty 1.0 → 2.0. The params
    // payload is built from the REAL core type — the wire carries the core's own serde JSON.
    let retune = vike_model::StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
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
    let mut control = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut control,
        &Request::Command {
            command: CommandWire::UpdateParams {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                mount_id: None,
                params: serde_json::to_value(&retune).expect("core params serialize"),
            },
            reason: Some("widen for the event".into()),
        },
    )
    .expect("update_params command");
    match read_frame::<_, Response>(&mut control).expect("ack") {
        Response::Ack { order_client_id, .. } => {
            assert_eq!(order_client_id, "", "account-wide-verb convention: empty order client id")
        }
        other => panic!("expected Ack for update_params, got {other:?}"),
    }

    // The FIRST quote after the Ack: the maker re-prices/re-sizes its resting orders in place.
    ticks.quote(quote(3)).expect("quote 3");
    let mid = 0.5 * (100.0_f64 + 100.2);
    assert!(
        wait_until(Duration::from_secs(5), || {
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

    // …and the ONE acceptance path recorded it: kind "update_params", empty order client id, the rationale.
    let entry = audit_entry_with_kind("update_params").expect("the accepted re-tune was audited");
    assert_eq!(entry.order_client_id, "", "account-wide-verb convention in the audit record too");
    assert_eq!(entry.reason.as_deref(), Some("widen for the event"));

    handle.shutdown_and_join();
}

/// An UNDECODABLE params payload is refused at the lowering edge — `Response::Error` naming the
/// verb, nothing reaching the core — over the REAL wire (the unit half lives in `server.rs`'s
/// `server::tests::lower_command`; this pins the operator-visible reply shape).
#[test]
fn an_undecodable_update_params_payload_is_refused_over_the_wire() {
    test_init();
    let (_mount, addr) = spawn_hub(HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut control = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut control,
        &command(CommandWire::UpdateParams {
            venue: "polymarket".into(),
            symbol: TOKEN.into(),
            interval: "1m".into(),
            mount_id: None,
            params: serde_json::json!({"NoSuchStrategyParams": {"qty": 1.0}}),
        }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut control).expect("response") {
        Response::Error(msg) => assert!(msg.contains("update_params"), "names the verb: {msg}"),
        other => panic!("an undecodable params payload must be Error, got {other:?}"),
    }
}

/// The strategies folder of the project this test PROCESS declares (one per process,
/// `vike_boot::halt::declare_project_state_dir`): the wire door reads a mount's file from the
/// declared project's `strategies/`, as the daemon's does. A FIXED folder under the target's test
/// scratch (`CARGO_TARGET_TMPDIR`), so a run reuses it rather than leaving a temp folder behind;
/// `buy_hold.rhai` is written only when it is not already there with the same text. Every
/// `spawn_hub` calls it before its paper core resolves the HALT path, so the declaration is the
/// process's first whichever test runs first.
pub(crate) fn declared_strategies() -> std::path::PathBuf {
    static DECLARED: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DECLARED
        .get_or_init(|| {
            let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join("vike-tradehub-control-roundtrip-project");
            let state = root.join("settings").join("state");
            std::fs::create_dir_all(&state).expect("the project's state dir");
            vike_boot::halt::declare_project_state_dir(Some(state))
                .expect("this process's one declaration");
            let strategies = root.join(vike_paths::state_path::PROJECT_STRATEGIES_DIR);
            let file = strategies.join("buy_hold.rhai");
            if std::fs::read_to_string(&file).ok().as_deref()
                != Some(crate::strategy_files::BUY_HOLD)
            {
                crate::strategy_files::write_into(
                    &strategies,
                    &[("buy_hold.rhai", crate::strategy_files::BUY_HOLD)],
                );
            }
            strategies
        })
        .clone()
}

/// ⚠ THE B5 WRITE GATE, end to end: a `CommandWire::MountStrategy` sent over the REAL control
/// server lands in a core that spawned with NO mount at all and adds one at runtime — resolved
/// through the daemon's REAL `mount_factory` (the `buy_hold.rhai` file) — asserted on what the core
/// EXPOSES (the published `mounts` view gains the row); the matching `UnmountStrategy` removes it
/// again. Both leave their audit records (`"mount_strategy"` / `"unmount_strategy"`) carrying the
/// rationale. A DUPLICATE mount of the same live id is Ack'd at the edge (the command is
/// well-formed) and refused BY THE CORE as a recent-events note — the `UpdateParams`
/// unknown-target contract, pinned here so the wire behaviour cannot drift from the white-box
/// vike-core tests (`runtime_mount_tests`), which own the cancel/state-sidecar half.
#[test]
fn mount_and_unmount_over_the_wire_change_the_live_mount_set() {
    use vike_core::{CoreConfig, spawn_core};
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, RiskGate};
    use vike_model::RiskLimits;

    test_init();

    let engine = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let config = CoreConfig {
        seed_cash: 1.0,
        // The daemon's REAL resolver — the same one both `main.rs` arms inject, over the folder the
        // wire door reads too.
        strategy_factory: Some(vike_tradehub::mount_factory::strategy_factory(
            Some(declared_strategies()),
            false,
        )),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    assert!(cell.load_full().mounts.is_empty(), "spawned with no mount");

    let addr = serve_core(&handle, HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mount_command = CommandWire::MountStrategy {
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        controller_id: Some("rt-hold".into()),
        file: Some("buy_hold.rhai".into()),
        name: None,
        rhai: None,
        params: serde_json::json!({"size": 1.0}),
    };

    // (1) MOUNT over the wire: Ack, the published mounts view gains the row (+ the residual row),
    // and the audit trail records the verb with its rationale.
    let mut control = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut control,
        &Request::Command {
            command: mount_command.clone(),
            reason: Some("second strategy for the session".into()),
        },
    )
    .expect("mount command");
    match read_frame::<_, Response>(&mut control).expect("ack") {
        Response::Ack { order_client_id, .. } => {
            assert_eq!(order_client_id, "", "account-wide-verb convention")
        }
        other => panic!("expected Ack for mount_strategy, got {other:?}"),
    }
    assert!(
        wait_until(Duration::from_secs(5), || cell
            .load_full()
            .mounts
            .iter()
            .any(|m| m.venue == "binance" && m.symbol == "BTCUSDT")),
        "the runtime mount never appeared in the published mounts view: {:?}",
        cell.load_full().mounts
    );
    let entry = audit_entry_with_kind("mount_strategy").expect("the accepted mount was audited");
    assert_eq!(entry.order_client_id, "");
    assert_eq!(entry.reason.as_deref(), Some("second strategy for the session"));

    // (2) A DUPLICATE of the LIVE id: well-formed, so the edge Acks — the CORE refuses, surfaced
    // in recent-events, and the mount set does not grow.
    write_frame(&mut control, &command(mount_command)).expect("duplicate mount command");
    match read_frame::<_, Response>(&mut control).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("a well-formed duplicate is edge-Ack'd, got {other:?}"),
    }
    assert!(
        wait_until(Duration::from_secs(5), || cell
            .load_full()
            .recent_events
            .iter()
            .any(|l| l.contains("MOUNT REFUSED: duplicate strategy-mount id `rt_hold`"))),
        "the core's duplicate refusal never surfaced: {:?}",
        cell.load_full().recent_events
    );

    // (3) UNMOUNT over the wire: Ack, the row disappears, the verb is audited.
    write_frame(
        &mut control,
        &Request::Command {
            command: CommandWire::UnmountStrategy { controller_id: "rt-hold".into() },
            reason: Some("done for the day".into()),
        },
    )
    .expect("unmount command");
    match read_frame::<_, Response>(&mut control).expect("ack") {
        Response::Ack { order_client_id, .. } => assert_eq!(order_client_id, ""),
        other => panic!("expected Ack for unmount_strategy, got {other:?}"),
    }
    assert!(
        wait_until(Duration::from_secs(5), || cell.load_full().mounts.is_empty()),
        "the unmounted row never left the published mounts view: {:?}",
        cell.load_full().mounts
    );
    let entry =
        audit_entry_with_kind("unmount_strategy").expect("the accepted unmount was audited");
    assert_eq!(entry.reason.as_deref(), Some("done for the day"));

    handle.shutdown_and_join();
}

/// A mount spec the daemon's profile machinery refuses — a strategy NAME, the retired selection
/// (a strategy is a file; nothing is mounted by name) — is refused AT THE EDGE with a
/// `Response::Error` carrying the profile vocabulary's own message and its fix; nothing reaches the
/// core. (The unit half lives in `mount_factory`'s tests; this pins the operator-visible reply
/// shape, the `an_undecodable_update_params_payload…` twin.)
#[test]
fn a_strategy_name_mount_is_refused_over_the_wire() {
    test_init();
    let (_mount, addr) = spawn_hub(HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut control = control_stream(addr, CONTROL_KEY);
    write_frame(
        &mut control,
        &command(CommandWire::MountStrategy {
            venue: "polymarket".into(),
            account: None,
            symbol: TOKEN.into(),
            interval: "1m".into(),
            controller_id: None,
            file: None,
            name: Some("no_such_strategy".into()),
            rhai: None,
            params: serde_json::json!({}),
        }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut control).expect("response") {
        Response::Error(msg) => {
            assert!(msg.contains("mount_strategy"), "names the verb: {msg}");
            assert!(msg.contains("is retired"), "carries the profile refusal: {msg}");
            assert!(msg.contains("[strategy] file = "), "…and its fix: {msg}");
        }
        other => panic!("a strategy name must be Error, got {other:?}"),
    }
}

/// A node whose publisher carries NO identity block (`publish::spawn(.., None)` — the shape this
/// file's `spawn_hub` builds) answers `StrategyStatus` with an HONEST error, never a fabricated
/// empty status. (The mounted-truth answer is `observe_roundtrip`'s
/// `observe_scope_can_strategy_status_but_not_update_params`, whose node publishes an identity.)
#[test]
fn strategy_status_without_an_identity_block_is_an_error() {
    test_init();
    let (_mount, addr) = spawn_hub(HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut obs = observe_stream(addr, OBSERVE_KEY);
    write_frame(&mut obs, &Request::StrategyStatus).expect("status request");
    match read_frame::<_, Response>(&mut obs).expect("response") {
        Response::Error(msg) => assert!(msg.contains("identity"), "names the gap: {msg}"),
        other => panic!("an identity-less node must answer Error, got {other:?}"),
    }
}

/// Two `Ping`s PIPELINED in one write on an authenticated connection: the node's second `Pong` is
/// written while its first is still unacknowledged, and must not wait for this client's delayed
/// ACK — the accepted socket carries `TCP_NODELAY` (`vike_hub_protocol::frame::configure_hub_stream`),
/// which is also what sends an observe stream's consecutive snapshots without that wait.
/// `crates/vike-datahub/tests/default_build/wire_latency.rs`'s `back_to_back_answers_cost_no_delayed_ack` argues
/// the shape and the budget (half of Linux's 40 ms delayed-ACK floor per pair).
#[test]
fn back_to_back_answers_cost_no_delayed_ack() {
    use std::io::Write;

    const PAIRS: u32 = 200;
    const BUDGET: Duration = Duration::from_millis(20 * PAIRS as u64);
    test_init();
    let (_mount, addr) = spawn_hub(HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), false);
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
         TCP_NODELAY (vike_hub_protocol::frame::configure_hub_stream)"
    );
}
