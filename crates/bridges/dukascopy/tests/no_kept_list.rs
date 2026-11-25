//! **A held-session venue with NO kept list** (decision 0290): this binary holds no dukascopy list, as a box before the
//! datahub has kept what the account's first login stated. Over the scripted fake (`src/bin/fake_jforex_bridge.rs`):
//! the sidecar starts and its login states its list all the same (its `ready` needs no row), the reconcile pass reads the
//! venue and books each row under the venue's own name, and what needs a row refuses in words naming the fix
//! (`vike_catalog::no_kept_list_refusal`): an order, a price subscription and the facts door (the last as "not now").
//! Nothing here may hold the list: `vike_dukascopy::test_support::hold_kept_list_for_tests` is the other binaries'.

use std::sync::Arc;

use vike_bridge_core::data_client::bus::DataClientBus;
use vike_dukascopy::feed::{FEATURE_TICKS, LinePort, QuoteBus};
use vike_dukascopy::test_support::{Wire, order};
use vike_dukascopy::{DukascopyConfig, ExecutionClientDukascopy};
use vike_execution::recon::ReconcileClient;
use vike_execution::testing::recv_event;
use vike_execution::{ExecutionClient, event_channel};
use vike_model::events::Event;

const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: vike_model::secret::Secret::new("test-password".into()),
        server: String::new(),
        ledger_mount: None,
        fx_clock: None,
    }
}

/// The fix the refusals name.
fn the_fix() -> String {
    vike_catalog::no_kept_list_refusal("dukascopy")
}

/// **The bridge starts, lists and reconciles with no kept list; an order needing a row is refused in words**: the start
/// is told it succeeded, the list its login stated is on the bus, a venue row is reported under the venue's own name,
/// and an order on `EURUSD` is a terminal `OrderRejected` naming the fix, before anything is sent.
#[test]
fn the_bridge_starts_with_no_kept_list_and_an_order_needing_a_row_is_refused_in_words() {
    assert!(vike_catalog::kept_list("dukascopy").is_none(), "this binary holds no dukascopy list");
    let dir =
        vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-duka-no-list")
            .expect("scratch dir");
    let body = dir.path().join("recon-body.json");
    std::fs::write(
        &body,
        r#"{"timestamp":5000,"orders":[],"positions":[{"symbol":"EURUSD","size":1000.0,"average_price":1.1}]}"#,
    )
    .expect("the body file");
    let bus = QuoteBus::new();
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_program_quoting(
        BRIDGE,
        &["recon".into(), body.display().to_string()],
        &config(),
        events,
        Arc::clone(&bus),
    )
    .expect("the sidecar starts with no kept list");
    assert!(bus.instrument_list().is_ok(), "its login stated the list all the same");

    let positions =
        client.recon_client().fetch_position_status_reports().expect("the venue is read");
    let rows: Vec<(&str, f64)> = positions.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    assert_eq!(rows, [("EURUSD", 1000.0)], "booked under the venue's own name, never dropped");

    client.submit(&order("needs-a-row"));
    match recv_event(&mut rx) {
        Event::OrderRejected(e) => {
            assert_eq!(e.order_client_id, "needs-a-row");
            assert!(e.reason.contains(&the_fix()), "the fix is named: {}", e.reason);
            assert!(e.reason.contains("NOT sent"), "{}", e.reason);
            assert_eq!(e.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        }
        other => panic!("expected the refusal before anything is sent, got {other:?}"),
    }
    client.detach();
}

/// **A price subscription and the facts door need the row too**: the stream refuses in words naming the fix and writes
/// nothing to the sidecar; the facts door answers "not now" (`ListUnavailable`), which the node asks again once the
/// datahub has kept the list, never a verdict it would remember.
#[test]
fn a_price_subscription_and_the_facts_door_need_the_kept_list() {
    use vike_data::{DataClient, DataClientSink, RecordingSink};
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
    let sink = Arc::new(RecordingSink::default());
    let mut stream = DataClientBus::on(&bus, Arc::clone(&sink) as Arc<dyn DataClientSink>);
    let why = stream.subscribe_quotes("EURUSD").expect_err("no kept list").to_string();
    assert!(why.contains(&the_fix()), "{why}");
    assert!(wire.lines().is_empty(), "nothing is subscribed: {:?}", wire.lines());

    let refusal =
        vike_dukascopy::instrument::names::facts_wire_symbol("EURUSD").expect_err("no kept list");
    assert!(refusal.is_transient(), "not now, never a verdict: {refusal:?}");
    assert!(refusal.text().contains(&the_fix()), "{refusal:?}");
}
