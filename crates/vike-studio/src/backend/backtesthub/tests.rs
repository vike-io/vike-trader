use super::*;
use std::assert_matches;
use vike_data::TimestampRange;

#[test]
fn backend_defaults_to_local() {
    // The default is the DAEMON now; `Local` is gone. See `impl Default for StudioRunBackend`.
    assert_eq!(
        StudioRunBackend::default(),
        StudioRunBackend::Source { address: DEFAULT_BACKTEST_ADDRESS.to_string() }
    );
}

/// This PC's device key as every hub dial of Studio carries it (decision 0288): an empty shared pair
/// around the device.
fn device_keys() -> HubKeys {
    let (device, _) = vike_hub_protocol::access::DeviceKey::generate().expect("a device key");
    HubKeys::of_device(device)
}

/// The refusal a keyed backtesthub hands Studio names how this PC's device gets on the server's list
/// as the desktop around Studio offers it (the Server group's Connect and Join, the Access group's Set
/// role at `control`), never a `vike-cli` verb a desktop user does not have, whether the PC holds no
/// device key or the server refused the one it holds, and keeps the server's own word. No restart is
/// asked for (the desktop seeds Studio's key again when a connect moves it). It names no retired
/// shared key: no hub reads one since decision 0288's cutover.
#[test]
fn a_refused_studio_is_told_how_to_register_its_device() {
    for held in [false, true] {
        let message = device_refusal_message("127.0.0.1:17880", held, "access denied");
        for needle in [
            "Data Manager → Backend → Server",
            "Connect over ssh",
            "Connect the server on this computer",
            "Join",
            "Data Manager → Backend → Access",
            "the role `control`",
            "Set role",
            "access denied",
            "127.0.0.1:17880",
        ] {
            assert!(message.contains(needle), "held={held}: missing {needle:?}: {message}");
        }
        assert!(
            !message.contains("vike-cli"),
            "held={held}: a verb the desktop does not offer: {message}"
        );
        assert!(!message.contains("secrets set"), "held={held}: a retired key route: {message}");
        assert!(
            !message.contains("restart"),
            "held={held}: a restart Studio does not need: {message}"
        );
    }
    assert!(device_refusal_message("a", false, "x").contains("holds no device key"));
    assert!(device_refusal_message("a", true, "x").contains("refused this PC's device key"));
}

/// ⚠ **STUDIO'S OWN RUN PATH, holding this PC's device key, pointed at a DATA-plane server, sends
/// nothing after its `Hello`.** This is the production call `StudioState::start_run` makes, not
/// the client crate's constructor on its own — so it also fails if [`connect`] ever signs the
/// key through the UNGUARDED `connect_authed`, which the client crate's own tests cannot see. A device
/// listed at `control` on the datahub too would otherwise open a Write session there.
#[test]
fn a_run_holding_the_device_key_sends_nothing_to_a_data_plane_server() {
    use std::io::Read;
    // ⚠ The fake answers at the DATAHUB's own version, as a real one does: since backtesthub
    // protocol 9 the two planes' numbers differ, and the dial must still read "the DATA daemon"
    // (`vike_hub_protocol::conn::HubConnection` names the daemon before it compares the versions).
    use vike_datahub_client::PROTO_VERSION;
    use vike_hub_protocol::auth::FEATURE_AUTH;
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    use vike_hub_protocol::plane::DATA_PLANE_SENTINEL;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("local_addr").to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        assert_matches!(
            read_frame::<_, HandshakeFrame>(&mut stream),
            Ok(HandshakeFrame::Hello { .. })
        );
        write_frame(
            &mut stream,
            &HandshakeReply::Welcome {
                proto_version: PROTO_VERSION,
                features: vec![DATA_PLANE_SENTINEL.to_string(), FEATURE_AUTH.to_string()],
                nonce: Some([9u8; 32]),
            },
        )
        .expect("welcome");
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).expect("timeout");
        let mut after_hello = Vec::new();
        let _ = stream.read_to_end(&mut after_hello);
        after_hello
    });

    let key = device_keys();
    let slice = DataSlice::bars(
        "binance",
        "BTCUSDT",
        "1h",
        TimestampRange { start: Some(0), end: Some(1) },
    );
    let rhai = StrategySpec::rhai("fn on_bar(){}");
    let engine = StudioRunEngine::default();
    let err = run_backtesthub_slice_summary(&address, Some(&key), &rhai, &slice, engine)
        .map(|_| ())
        .expect_err("a DATA-plane server must be refused for Studio's Write dial");
    assert!(err.to_string().contains("DATA daemon"), "{err}");
    let after_hello = server.join().expect("server thread");
    assert!(
        after_hello.is_empty(),
        "Studio's Run wrote {} byte(s) after its Hello to the DATA daemon while holding a \
             device key",
        after_hello.len()
    );
}

// The encode/decode tests that stood here (`to_wire_spec`, `to_wire_slice`, the result, sweep,
// walk-forward and error-line decoders) moved WITH their functions, beside the server halves they
// invert: `crates/vike-studio-core/src/wire_run_tests.rs` and
// `crates/vike-backtesthub-client/src/wire_record_tests.rs`, each now a round trip.

/// "Compare all" keeps every row, in order, a failed run as its sentence rather than dropped.
#[test]
fn compare_each_keeps_a_failed_row_as_its_sentence() {
    let rows = vec![
        ("ok".to_string(), StrategySpec::rhai("fn on_bar() {}")),
        ("broken".to_string(), StrategySpec::rhai("fn on_bar( {")),
    ];
    let out = compare_each(rows, |spec| match spec {
        StrategySpec::Rhai(src) if src.contains("( {") => {
            Err(RunError::Compile("unexpected `{`".to_string()))
        }
        _ => Ok(RunSummaryWire::default()),
    });
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].0, "ok");
    assert!(out[0].1.is_ok());
    assert_eq!(out[1].0, "broken");
    assert!(out[1].1.as_ref().is_err_and(|e| e.contains("unexpected")), "{:?}", out[1].1);
}

// ⚠ `remote_store_tick_refusal` STOOD HERE AND IS DELETED, not ported.
//
// It walked the split-plane tick rule exhaustively, and the ONE refused cell it asserted
// was (remote store, tick slice, LOCAL backend) - a refusal that exists only because a
// local path was the thing being refused IN FAVOUR OF. `StudioRunBackend::Local` is gone
// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`), so the
// cell is unreachable and a test asserting it would pass for a reason that no longer
// exists - which this tree treats as worse than no test at all.

/// **The funding switch is never dropped when OFF** (the owner's ruling of 2026-11-20: no funding unless he asks).
/// A server that serves it gets OFF as `false`; one that predates it would CHARGE a perpetual on an absent switch, so
/// OFF there is refused in words with nothing sent, never a funded run. ON is left out there (its default charges).
#[test]
fn the_funding_switch_off_is_refused_on_a_server_that_cannot_honour_it() {
    let new = vec![FEATURE_ATTACH_FUNDING.to_string()];
    let switch = |engine: StudioRunEngine, features: &[String]| {
        engine.wire(features).ok().map(|wire| wire.and_then(|w| w.attach_funding))
    };
    let off = StudioRunEngine::default();
    assert_eq!(switch(off, &new), Some(Some(false)), "OFF is sent as false");
    let on = StudioRunEngine { funding: true, ..off };
    assert_eq!(switch(on, &new), Some(Some(true)));
    let refused = off.wire(&[]).expect_err("OFF on an older server is refused");
    assert!(refused.to_string().contains(FUNDING_OFF_NOT_SERVED), "{refused}");
    assert_eq!(on.wire(&[]).ok(), Some(None), "ON on an older server: its default, so nothing");
    let capital = StudioRunEngine { cash: Some(25_000.0), funding: true };
    let old = capital.wire(&[]).expect("sent").expect("the capital rides alone");
    assert_eq!((old.cash, old.attach_funding), (Some(25_000.0), None));
}

/// **A Rhai buffer's parameters reach a server that reads them.** An older one is refused them in words for a plain
/// Run or a Compare-all row (they would be dropped), and gets the bare buffer for a scan or a walk-forward, whose
/// grid holds every value as an axis.
#[test]
fn a_rhai_buffers_parameters_are_never_dropped_silently() {
    let with = StrategySpecWire::RhaiWithParameters {
        source: "fn on_bar() {}".to_string(),
        parameters_toml: "fast = 7\n".to_string(),
    };
    let new = vec![FEATURE_RHAI_PARAMETERS.to_string()];
    assert_eq!(served_spec(with.clone(), &new, false).ok(), Some(with.clone()));
    let refused = served_spec(with.clone(), &[], false).expect_err("a plain Run is refused");
    assert!(refused.to_string().contains(RHAI_VALUES_NOT_SERVED), "{refused}");
    let bare = StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    assert_eq!(served_spec(with, &[], true).ok(), Some(bare.clone()), "a scan's axes carry them");
    assert_eq!(served_spec(bare.clone(), &[], false).ok(), Some(bare), "a bare buffer runs");
}
