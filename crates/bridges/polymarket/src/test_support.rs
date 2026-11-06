//! Shared by this crate's `#[ignore]`d live smokes that read an ingest lane — a started client's or a
//! user-channel pump's: wait for one event without blocking past a deadline. Compiled ONCE here behind
//! `test-support` (only the self dev-dependency turns it on) instead of once per smoke that
//! `#[path]`-included `tests/support/ingest.rs`.

use std::time::{Duration, Instant};

use vike_exec::Ingest;
use vike_model::events::Event;

/// Poll `rx` for an `Event` matching `pred` until `deadline`, sleeping `poll` whenever the lane is
/// empty; `None` at the deadline or once every sender is gone. `poll` is the caller's own interval:
/// the smokes that share this poll at different rates, and each keeps its own.
pub fn wait_event(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    deadline: Instant,
    poll: Duration,
    mut pred: impl FnMut(&Event) -> bool,
) -> Option<Event> {
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(e)) => {
                if pred(&e) {
                    return Some(e);
                }
            }
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(poll);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
        }
    }
    None
}
