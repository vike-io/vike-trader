//! Shared by the `exec_actor_*` test binaries: wait (bounded) for the next event on an ingest lane.

use std::time::Duration;

use vike_exec::Ingest;
use vike_model::events::Event;

/// Block up to 10s for one `Ingest::Event`; panics on a timeout, a closed lane or a non-event.
pub fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
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
