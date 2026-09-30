//! A ~50-line `tracing` capture harness, shared by the two test binaries that assert on what an
//! OPERATOR sees at a mount.
//!
//! ⚠ **It installs a GLOBAL default, not a thread-local one**, and that is deliberate rather than
//! convenient. `tracing` caches a callsite's `Interest` the first time the callsite is reached, and
//! a thread-local `with_default` leaves that first evaluation racing whatever else the test binary
//! is doing — the shape this workspace has already been bitten by (a capture that comes back empty
//! while the code under test is emitting perfectly well). One global collector, installed before
//! anything under test runs, takes the question away. The cost is that a binary using this may
//! install it ONCE, which is why the two cases live in two files: the report itself is
//! `Once`-latched per PROCESS, so "it fires" and "it stays silent" cannot be observed in one.
//!
//! No `tracing-subscriber` dev-dependency: this is the `tracing` facade only, the same choice
//! `crates/vike-script/tests/script_print_is_not_stdout.rs` made and for the same reason.

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Every event the process emitted, newest last, each rendered `[LEVEL] target: message`.
pub type Log = Arc<Mutex<Vec<String>>>;

struct Collector(Log);

/// Pulls the `message` field out of an event. Both visit methods are implemented because
/// `format_args!` arrives through `record_debug` while a `&'static str` takes `record_str`, and
/// relying on one would silently record nothing if tracing routed the other.
#[derive(Default)]
struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        if f.name() == "message" {
            self.0 = format!("{v:?}");
        }
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        if f.name() == "message" {
            self.0 = v.to_string();
        }
    }
}

impl Subscriber for Collector {
    fn enabled(&self, _m: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _a: &Attributes<'_>) -> Id {
        Id::from_u64(1) // `from_u64` panics on 0; no span is ever entered here
    }
    fn record(&self, _s: &Id, _v: &Record<'_>) {}
    fn record_follows_from(&self, _s: &Id, _f: &Id) {}
    fn event(&self, e: &Event<'_>) {
        let mut m = Message::default();
        e.record(&mut m);
        self.0.lock().unwrap().push(format!(
            "[{}] {}: {}",
            e.metadata().level(),
            e.metadata().target(),
            m.0
        ));
    }
    fn enter(&self, _s: &Id) {}
    fn exit(&self, _s: &Id) {}
}

/// Install the collector as this PROCESS's subscriber. Call it once, first thing.
#[must_use]
pub fn install() -> Log {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(Collector(Arc::clone(&log)))
        .expect("this test binary installs exactly one subscriber");
    log
}

/// Everything recorded so far.
///
/// ⚠ Two statements rather than a tail `log.lock().unwrap().clone()` — that spelling is E0597: a
/// `MutexGuard` temporary in a block's TAIL expression outlives the block's own locals.
#[must_use]
pub fn lines(log: &Log) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(log.lock().unwrap().iter().cloned());
    out
}
