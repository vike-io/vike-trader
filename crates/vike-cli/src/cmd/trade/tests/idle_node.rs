//! The `trade` reads against a REAL idle paper node: they answer at once, and a key the node does not
//! hold lands on the failed rung. The only node in this crate's unit tests.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::time::{Duration, Instant};

use vike_client::hubkeys;
use vike_hub_protocol::auth::HubKeys;
use vike_tradehub::{publish, server};
use vike_tradehub_client::wire::HubIdentityWire;
use vike_tradehub_core::StrategyCorePaper;

/// Far-future resolution so the A-S horizon is positive.
const RESOLUTION_TIMESTAMP: i64 = 3_000_000_000;
/// Obviously-fake HMAC keys: any bytes work as long as both sides agree.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-trade-idle-node-tests";
const CONTROL_KEY: &str = "DUMMY-control-key-for-the-trade-idle-node-tests";
/// What a test reads back to prove an answer is the node's frame and not the client's placeholder,
/// which carries no identity at all.
const STAMPED_NAME: &str = "trade-idle-node-stamped";

/// A PAPER node on an ephemeral loopback port, its publisher STAMPING an identity into every frame,
/// the `seq: 0` frame of a node that has folded nothing included: what the shipped daemon does.
fn spawn_stamped_hub() -> (StrategyCorePaper, SocketAddr) {
    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategy_lab::makers::spread_maker::SpreadMaker::from_params,
        "polymarket",
        "TRADE_IDLE_NODE",
        Some(RESOLUTION_TIMESTAMP),
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let identity = HubIdentityWire {
        name: STAMPED_NAME.to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "vike-cli idle_node".to_string(),
        advertise_addr: String::new(),
        priced_venues: None,
    };
    let publisher = publish::spawn(mount.handle.snapshot_cell(), Some(identity));
    let commands = Some(mount.handle.command_sink());
    let keys = HubKeys::new(OBSERVE_KEY.as_bytes().to_vec(), CONTROL_KEY.as_bytes().to_vec());
    std::thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });
    (mount, addr)
}

/// **THE `trade` READS ANSWER AN IDLE NODE AT ONCE.** `crate::cmd::trade`'s `connect_observe` is the
/// subscribe-and-wait every display read of the `trade` plane goes through (`trade order ls`, `trade
/// position ls`; the REPL's own reads share the same wait). It waited for `seq != 0`, which an idle
/// node never reaches, so every such read against one sat out the whole ~2s deadline and then printed
/// the very frame it had held since the first round trip.
///
/// The CONDITION is asserted first: the node's stamped frame, from a node whose own cell never left
/// `seq: 0`. The time bound is the symptom, at half the deadline.
#[test]
fn a_trade_read_answers_an_idle_stamped_node_without_waiting_out_the_deadline() {
    let (mount, node) = spawn_stamped_hub();

    let started = Instant::now();
    let (handle, snap) = crate::cmd::trade::connect_observe(
        &node.to_string(),
        OBSERVE_KEY.as_bytes().into(),
        vike_tradehub_client::liveness::LinkPatience::default().observe_stall,
    )
    .expect("a live node accepts the observe handshake");
    let took = started.elapsed();

    assert_eq!(
        snap.identity.as_ref().map(|i| i.name.as_str()),
        Some(STAMPED_NAME),
        "the node's stamped frame, never the client's placeholder"
    );
    assert_eq!(snap.sequence, 0, "an unseeded paper node never folds, so it stays at seq 0");
    assert_eq!(mount.handle.snapshot_cell().load().sequence, 0, "…and the node's own cell agrees");
    assert!(
        took < Duration::from_secs(1),
        "the node's frame is in hand after one round trip, so the read must not wait out the \
         first-frame deadline — it took {took:?}"
    );
    drop(handle);
}

/// `crate::cmd::trade`'s `observe_read`, the one OBSERVE read `order ls`, `position ls`, `recon
/// ls` and `order stops` open with, against a real node: the right key reads the node's frame, a
/// key the node does not hold is the refused handshake on `Exit::Failed` (never `Exit::Connect`:
/// the node answered). The no-key and dead-socket arms are in `crate::cmd::trade`'s own tests.
#[test]
fn the_ls_verbs_observe_read_reads_a_real_node_and_puts_a_refused_key_on_the_failed_rung() {
    let (_mount, node) = spawn_stamped_hub();
    let keyring = |key: &str| {
        let keyed: HashMap<String, String> =
            [(hubkeys::OBSERVE_KEY_ENV.to_string(), key.to_string())].into_iter().collect();
        hubkeys::resolve(&keyed, None)
    };

    let snap =
        crate::cmd::trade::observe_read("trade order ls", &node.to_string(), &keyring(OBSERVE_KEY))
            .expect("the node's own observe key reads its frame");
    assert_eq!(snap.identity.as_ref().map(|i| i.name.as_str()), Some(STAMPED_NAME));

    let refused = crate::cmd::trade::observe_read(
        "trade order ls",
        &node.to_string(),
        &keyring("not-the-key"),
    )
    .expect_err("a key the node does not hold is refused");
    assert_eq!(
        format!("{refused:?}"),
        format!("{:?}", std::process::ExitCode::from(crate::exit::Exit::Failed))
    );
}
