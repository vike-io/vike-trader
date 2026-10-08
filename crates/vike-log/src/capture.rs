//! **The workspace's ONE scoped `tracing` capture.** `test-support`-gated, so no shipped build
//! compiles it. Two doors over one guarantee:
//!
//! * [`captured`] — run a closure and get back every event it emitted ON THIS THREAD, as
//!   [`CapturedEvent`]s. The recording double every test that asserts a log line reads with, from
//!   a venue mount's announcement to a market maker's HOLD warn.
//! * [`scoped`] — run a closure with a CALLER'S subscriber as this thread's default, for a test
//!   whose subject IS the subscriber: this crate's own layers, a formatter, a filter reload.
//!
//! **The guarantee:** an event the closure emits on this thread is delivered even when its
//! callsite is first hit — before or during the capture — by ANOTHER thread with no subscriber,
//! i.e. by a sibling test calling the same code uncaptured under `cargo test`, which runs a test
//! binary's tests as threads of one process. A bare `tracing::subscriber::with_default` does not
//! hold that, and no answer the capturing subscriber gives can make it hold (this module's private
//! `InterestFloor` carries the tracing-core mechanism and the cure).
//! `crates/vike-log/tests/capture_sees_every_callsite.rs` and
//! `crates/vike-log/tests/scoped_sees_every_callsite.rs` hold it for each door, and
//! `crates/vike-ops/tests/hygiene/tracing_capture_gate.rs` keeps these two doors the only path in the
//! workspace to a scoped default, so a hand-rolled capture cannot come back.
//!
//! ⚠ One interleaving stays open, and no capture can close it: a callsite whose FIRST
//! registration, on another thread, began before the process's first capture had registered its
//! subscriber, and was descheduled inside tracing-core until after that, can still store `never`
//! late. That window is a few instructions wide and opens once per process.
//!
//! It lives HERE because the anchor is a property of `tracing-core`'s process-wide callsite
//! registry and of nothing else, and this is the crate that owns `tracing` setup: a leaf (layer 15)
//! every capturing test reaches with a dev edge, whatever its own layer.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tracing::field::{Field, Visit};
use tracing::subscriber::Interest;
use tracing::{Dispatch, Event, Level, Metadata, Subscriber, span};

/// **One `tracing` event, as a test reads it back.** `fields` holds every structured field except
/// `message`, rendered the way `%value` / `?value` / a plain string render, and `message` is the
/// formatted sentence. `target` is the callsite's target — the emitting module's path unless the
/// macro named one.
#[derive(Debug, Clone)]
pub struct CapturedEvent {
    pub level: Level,
    pub target: String,
    pub fields: BTreeMap<String, String>,
    pub message: String,
}

impl CapturedEvent {
    /// A structured field's text, or `None` when the event carries no such field.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

/// Run `f` and return every `tracing` event it emitted ON THIS THREAD, in order. Code that spawns a
/// thread of its own logs from that thread, which this does not see: the default it installs is
/// this thread's.
///
/// Holds against a sibling test running the same code uncaptured — the module doc's guarantee, and
/// its one residual.
pub fn captured<R>(f: impl FnOnce() -> R) -> (R, Vec<CapturedEvent>) {
    let recorder = Recorder::default();
    let out = scoped(recorder.clone(), f);
    let events = std::mem::take(&mut *recorder.0.lock().unwrap_or_else(PoisonError::into_inner));
    (out, events)
}

/// Run `f` with `subscriber` as this thread's default — `tracing::subscriber::with_default`, after
/// the process's interest floor exists, so the module doc's guarantee holds for a subscriber the
/// caller built (a formatter writing into a buffer, a layered registry under test).
pub fn scoped<S, R>(subscriber: S, f: impl FnOnce() -> R) -> R
where
    S: Subscriber + Send + Sync + 'static,
{
    // BEFORE `with_default`'s own `Dispatch::new`, so that registration already counts two.
    INTEREST_FLOOR.get_or_init(|| Dispatch::new(InterestFloor));
    tracing::subscriber::with_default(subscriber, f)
}

/// The recording subscriber behind [`captured`]. Hand-rolled over `tracing`'s own `Subscriber`
/// trait. Its `register_callsite` answers `sometimes`, so `enabled` is asked on every hit — but ⚠
/// that answer is NOT what keeps the per-callsite `Interest` cache from emptying a capture: while a
/// recorder is the only live dispatcher, tracing-core never ASKS it. [`InterestFloor`] is.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<CapturedEvent>>>);

/// **A dispatcher that exists only to be REGISTERED**, once per process and never dropped, so a
/// scoped subscriber is never the only live one. It records nothing and is nobody's default; its
/// one job is the answer it gives `register_callsite`.
///
/// Why a capture needs it — tracing-core 0.1.36, `callsite.rs`, read rather than guessed:
///
/// * a callsite caches ONE process-wide `Interest`, computed when it is first hit, and an event
///   whose callsite cached `never` is dropped inside the macro, before any dispatcher sees it;
/// * that interest is the `and` of every live dispatcher's `register_callsite` — EXCEPT while
///   `Dispatchers::register_dispatch` last counted at most one live dispatcher
///   (`has_just_one`). Then `Rebuilder::JustOne` asks only `dispatcher::get_default` — the default
///   of the thread that HAPPENS to hit the callsite first;
/// * so with a capture's subscriber the only live dispatcher, a sibling test that calls the same
///   code WITHOUT capturing it, on its own thread, registers the callsite against
///   `NoSubscriber`: `never`, cached for every thread, and the capture's own hit is dropped.
///   That was the `vike-fxcm` mount-test flake (#2406): `a_loadable_shim_mounts_a_demo_bound_…`
///   hit the live mount line uncaptured while `a_live_login_beside_the_demo_one_…` was
///   mid-capture, and the capture came back without it.
///
/// Neither cure a capture can apply to ITSELF works: answering `sometimes` or `always` is never
/// consulted on the JustOne path, and `tracing::callsite::rebuild_interest_cache()` at capture
/// entry re-computes only the callsites ALREADY registered, not one a sibling registers after it.
///
/// With this registered first, every later `register_dispatch` counts at least two, so from the
/// first capture's registration on the JustOne path is never taken again in the process, and every
/// interest is computed over a list holding this — whose `sometimes` makes the cached answer at
/// least `sometimes` (`and` of two different answers is `sometimes`), whichever thread computes
/// it. A capture's own `register_dispatch` also re-computes every callsite already registered, so
/// a `never` cached before the floor existed does not survive it either. Its `max_level_hint` is
/// the default `None` (read as TRACE), never `Some(OFF)`, so it never lowers the global level
/// filter under a capture.
///
/// NOT a global default, deliberately: that slot is `crate::test_init`'s, which test files across
/// the workspace call, and a process can hold only one.
struct InterestFloor;

impl Subscriber for InterestFloor {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        false
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

/// The process's one [`InterestFloor`], registered by the first [`scoped`] call (every
/// [`captured`] is one) and alive until exit.
static INTEREST_FLOOR: OnceLock<Dispatch> = OnceLock::new();

struct FieldReader<'a>(&'a mut CapturedEvent);

impl Visit for FieldReader<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.message = format!("{value:?}");
        } else {
            self.0.fields.insert(field.name().to_string(), format!("{value:?}"));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.message = value.to_string();
        } else {
            self.0.fields.insert(field.name().to_string(), value.to_string());
        }
    }
}

impl Subscriber for Recorder {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let mut seen = CapturedEvent {
            level: *meta.level(),
            target: meta.target().to_string(),
            fields: BTreeMap::new(),
            message: String::new(),
        };
        event.record(&mut FieldReader(&mut seen));
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(seen);
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}
