//! **An `ExecActor` that was never handed a sentinel path watches NO file** — in particular not a
//! process-wide one (decision 0099).
//!
//! A bridge reads no process-global state (decision 0096, rule 1): the path arrives with
//! `ExecActor::with_halt_path`, called by each bridge's mount from `MountInputs::process.halt_path`
//! (`crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs`'s
//! `every_live_mount_hands_its_client_the_halt_path` holds that call), and an actor built without it
//! is a plain scaffold with nothing to refuse on.
//!
//! # Why a file (and a process) of its own
//!
//! The proof has to ENGAGE a process-wide sentinel, and the only way to do that without
//! `std::env::set_var` is to declare the project (`declare_project_state_dir`) and put a `HALT`
//! file in it — memoized for the life of the process, so it must not share a binary with other
//! tests. One test per binary also keeps the order of declaration, resolution and spawn under this
//! test's control: declaring after anything resolved is rejected as "too late".

use vike_bridge_core::exec_actor::ExecActor;
use vike_bridge_core::halt::{HALT_FILE, declare_project_state_dir, halt_path_from_env};
use vike_exec::{ExecutionClient, event_channel};
use vike_model::OrderRequest;
use vike_model::events::Event;

#[path = "support/fake_venue.rs"]
mod fake_venue;
#[path = "support/ingest.rs"]
mod ingest;

use fake_venue::fake_run;
use ingest::recv_event;

#[test]
fn an_actor_with_no_sentinel_path_does_not_read_a_process_wide_one() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let state = scratch.path().join("project").join("settings").join("state");
    std::fs::create_dir_all(&state).expect("the declared state directory");
    let sentinel = state.join(HALT_FILE);
    std::fs::write(&sentinel, b"").expect("engage the PROCESS-WIDE sentinel");

    // Declare BEFORE anything resolves, then prove the wiring before the property: the memoized
    // resolver now names the file this test engaged, so a client that consulted it would be halted.
    declare_project_state_dir(Some(state)).expect("the first declaration is accepted");
    assert_eq!(
        halt_path_from_env(),
        sentinel,
        "the process-wide sentinel must be the engaged file, or the assertion below passes for the \
         wrong reason — the defect this file exists to prevent"
    );
    assert!(sentinel.exists(), "the sentinel must be on disk");

    // No `with_halt_path`: the actor was handed nothing.
    let (events, mut rx) = event_channel(16);
    let mut client =
        ExecActor::spawn("halt-unwired", events.clone(), move |rx| fake_run(events, rx));
    client.submit(&OrderRequest {
        client_order_id: "no-sentinel-path".into(),
        venue: "test".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        ts: 5,
        ..Default::default()
    });

    match recv_event(&mut rx) {
        Event::OrderSubmitted(s) => assert_eq!(s.client_order_id, "no-sentinel-path"),
        other => panic!(
            "an actor handed NO sentinel path must not consult a process-wide one (decision 0099) \
             — its order should have reached the venue thread, got {other:?}"
        ),
    }
}
