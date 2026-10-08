//! **An `ExecActor` that was never handed a sentinel path watches NO file** — in particular not a
//! process-wide one (decision 0099).
//!
//! Until that decision the actor's default (`halt_path: None`) fell back to the memoized
//! process-wide resolver, so ten venues' kill switches worked by the actor reaching into process
//! state at the moment an order arrived. The mount contract (decision 0096, rule 1) says a bridge
//! reads no process-global state and is handed what it needs as `MountInputs::process`; the actor's
//! default was the one place that rule did not hold, and it held for every venue at once. The path
//! now arrives with `ExecActor::with_halt_path`, called by each bridge's mount from
//! `MountInputs::process.halt_path` (`crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs`'s
//! `every_live_mount_hands_its_client_the_halt_path` holds that call), and an actor built without it
//! is a plain scaffold with nothing to refuse on.
//!
//! # Why a file (and a process) of its own
//!
//! The proof has to ENGAGE a process-wide sentinel, and the only way to do that without
//! `std::env::set_var` is to declare the project (`declare_project_state_dir`) and put a `HALT`
//! file in it — which is memoized for the life of the process and would turn every sibling test in
//! `exec_actor_halt.rs` that still spawns without a path into a halted one. One test per binary
//! also keeps the order of declaration, resolution and spawn under this test's control: declaring
//! after anything resolved is rejected as "too late".

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use vike_bridge_core::exec_actor::{ExecActor, ExecCommand, cancel_batch_undeclared};
use vike_bridge_core::halt::{HALT_FILE, declare_project_state_dir, halt_path_from_env};
use vike_exec::{EventSender, ExecutionClient, Ingest, event_channel};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCanceled, OrderSubmitted};

/// A private scratch directory whose `Drop` removes it (`halt_declaration.rs`'s `Scratch`: this
/// crate has no `tempfile` dev-dependency).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-halt-unwired-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `exec_actor_halt.rs`'s fake venue loop: a submit that REACHES the venue thread answers
/// `OrderSubmitted`, which is how this test tells "refused at the boundary" from "passed through".
fn fake_run(events: EventSender, rx: Receiver<ExecCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                let _ = events.blocking_send(Event::OrderCanceled(OrderCanceled {
                    client_order_id: coid,
                    reason: String::new().into(),
                    ts: 0,
                }));
            }
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
}

fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

#[test]
fn an_actor_with_no_sentinel_path_does_not_read_a_process_wide_one() {
    let scratch = Scratch::new("procwide");
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
