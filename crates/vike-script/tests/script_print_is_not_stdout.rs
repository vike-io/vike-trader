//! ⚠ **A user script's `print`/`debug` must never reach the HOST PROCESS'S STDOUT** — for both of
//! this crate's two engines.
//!
//! `rhai::Engine::new()` wires both hooks to `println!` (rhai-1.25.1 `src/engine.rs`'s `new`:
//! `engine.print = Some(Box::new(|s| println!("{s}")))`). This crate is embedded in binaries whose
//! stdout is a PROTOCOL: `vike-cli mcp` speaks newline-JSON-RPC over `io::stdout()` and compiles a
//! caller-supplied script in-process (`crates/vike-cli/src/cmd/mcp.rs`'s `tool_validate_strategy`
//! → [`vike_script::discover_params`]), and `vike-tradehub` — the daemon that signs real orders —
//! mounts a `RhaiStrategy` while keeping its own stdout for a newline-JSON control channel. So a
//! stray `print` is a malformed frame, not console noise.
//! `crates/vike-script/src/engine.rs`'s `redirect_script_output` is the fix and carries the full
//! argument; the study tier decided the same question first
//! (`crates/vike-studio-core/src/rhai_study/bind.rs`'s `build_engine`).
//!
//! ⚠ **The obvious version of this test is VACUOUS, twice over, and every assertion below is
//! paired against one of the two.**
//!
//! 1. *The harness never captures anything.* A subscriber that is not actually installed, a
//!    callsite whose interest was cached before it, a field name that does not match — all of them
//!    produce an empty capture, and "assert the capture is non-empty" would then be the only thing
//!    that ever fails. [`the_capture_harness_sees_an_event_emitted_directly`] plants a plain
//!    `tracing::info!` and proves the harness sees it.
//! 2. *The harness captures everything.* An assertion that "some event was recorded" would pass on
//!    a crate that logs anything at all for its own reasons.
//!    [`a_script_that_prints_nothing_records_nothing`] runs the same shape with the `print` removed
//!    and proves the capture is EMPTY, so a hit in the tests above is attributable to the `print`.
//!
//! Deliberately NOT asserted: that stdout stayed silent. Nothing in-process can observe the
//! `println!` a broken build would emit (libtest's capture is not readable from inside the test),
//! so the property is proven from the other side — the string arrived where it was routed — and
//! MUTATION is what closes the gap: delete either `redirect_script_output` call and these tests go
//! red.
//!
//! No `tracing-subscriber` dev-dependency: the collector below is ~40 lines against the `tracing`
//! facade this crate already depends on, and adding a dev-dep to prove a four-line routing change
//! would cost the workspace a lockfile edit for nothing.

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use vike_model::{Bar, Broker, Strategy};
use vike_script::{compile_indicator, Indicator, RhaiStrategy};

/// The target `redirect_script_output` routes both hooks to. Spelled here rather than imported —
/// the constant is `pub(crate)`, and a test that read the value out of the code it is checking
/// would agree with any typo.
const TARGET: &str = "vike_script";

// -------------------------------------------------------------------------------------------
// the capture harness
// -------------------------------------------------------------------------------------------

/// One captured `tracing` event, flattened to what these tests assert on.
#[derive(Clone, Debug)]
struct Ev {
    target: String,
    level: Level,
    message: String,
    tier: String,
}

type Log = Arc<Mutex<Vec<Ev>>>;

/// A minimal `Subscriber` that keeps every event it is handed. Spans are accepted and discarded —
/// nothing here opens one, but the trait requires the methods.
struct Collector(Log);

impl Subscriber for Collector {
    fn enabled(&self, _m: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _a: &Attributes<'_>) -> Id {
        Id::from_u64(1) // `from_u64` panics on 0; no span is ever entered here
    }
    fn record(&self, _s: &Id, _v: &Record<'_>) {}
    fn record_follows_from(&self, _s: &Id, _f: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut v = Fields::default();
        event.record(&mut v);
        self.0.lock().unwrap().push(Ev {
            target: event.metadata().target().to_string(),
            level: *event.metadata().level(),
            message: v.message,
            tier: v.tier,
        });
    }
    fn enter(&self, _s: &Id) {}
    fn exit(&self, _s: &Id) {}
}

/// Pulls the two fields these tests care about out of an event.
///
/// `message` arrives as `format_args!` through `record_debug` (a `&dyn Debug` whose `Debug` is its
/// `Display`), while `tier` is a `&'static str` and takes `record_str`. Both are implemented
/// because relying on one would silently record nothing if tracing routed the other.
#[derive(Default)]
struct Fields {
    message: String,
    tier: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "tier" => self.tier = format!("{value:?}"),
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "tier" => self.tier = value.to_string(),
            _ => {}
        }
    }
}

/// Runs `f` with a fresh capturing subscriber installed as the THREAD's default, and returns
/// everything it recorded.
fn capture(f: impl FnOnce()) -> Vec<Ev> {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::with_default(Collector(Arc::clone(&log)), f);
    // Cloned out rather than unwrapped from the `Arc`: `with_default` holds the collector inside a
    // `Dispatch`, so nothing here may assume this is the last handle.
    // ⚠ Two statements rather than a tail `log.lock().unwrap().clone()` — that spelling is E0597,
    // because a `MutexGuard` temporary in a block's TAIL expression is dropped after the block's
    // own locals, `log` included. Measured on the CI box, not guessed.
    let mut events = Vec::new();
    events.extend(log.lock().unwrap().iter().cloned());
    events
}

/// The one event a test expects, or a panic naming everything that WAS recorded — an empty capture
/// and a capture full of the wrong thing are different bugs and must not print the same way.
fn only(events: &[Ev]) -> &Ev {
    assert_eq!(
        events.len(),
        1,
        "expected exactly one recorded event, got {}: {events:?}",
        events.len()
    );
    &events[0]
}

// -------------------------------------------------------------------------------------------
// the two anti-vacuity controls
// -------------------------------------------------------------------------------------------

/// Control 1: the harness can see an event at all. Without this, every assertion below could be
/// satisfied only by the harness being broken in the same direction as the bug.
#[test]
fn the_capture_harness_sees_an_event_emitted_directly() {
    let events = capture(|| {
        tracing::info!(target: TARGET, tier = "control", "planted");
    });
    let e = only(&events);
    assert_eq!(e.target, TARGET);
    assert_eq!(e.level, Level::INFO);
    assert_eq!(e.message, "planted");
    assert_eq!(e.tier, "control");
}

/// Control 2: the harness is not simply recording whatever the crate does. The same compile with
/// no `print` in it records NOTHING, so a hit in the tests below is attributable to the `print`.
#[test]
fn a_script_that_prints_nothing_records_nothing() {
    let events = capture(|| {
        RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(1.0); }").expect("compiles");
        compile_indicator("quiet", "fn on_bar(bar) { bar.close }").expect("compiles");
    });
    assert!(events.is_empty(), "nothing printed, so nothing should have been logged: {events:?}");
}

// -------------------------------------------------------------------------------------------
// the strategy engine (`engine.rs`'s `build_engine`)
// -------------------------------------------------------------------------------------------

/// A strategy's TOP LEVEL is where an author writes a diagnostic, and `compile` runs it exactly
/// once (`run_ast_with_scope`) — which is also the path `vike-cli mcp`'s `validate_strategy` tool
/// takes with its stdout mid-JSON-RPC-frame.
#[test]
fn a_strategy_top_level_print_goes_to_tracing() {
    let events = capture(|| {
        RhaiStrategy::<MockBroker>::compile(r#"print("top-level-hello"); fn on_bar() {}"#)
            .expect("compiles");
    });
    let e = only(&events);
    assert_eq!(e.target, TARGET, "the whole surface lands on one target: {e:?}");
    assert_eq!(e.level, Level::INFO, "`print` is INFO; `debug` is DEBUG: {e:?}");
    assert_eq!(e.message, "top-level-hello");
    assert_eq!(e.tier, "strategy", "the tier field names which engine produced it: {e:?}");
}

/// ...and from inside a HOOK, which is the path that runs on a live venue: `vike-tradehub` mounts a
/// `RhaiStrategy` on the production core, and `on_bar` is called for every bar it trades.
///
/// The order is asserted too. Without it, a strategy that failed to run at all would produce the
/// same empty-stdout evidence this test is really about.
#[test]
fn a_strategy_hook_print_goes_to_tracing() {
    let mut strat =
        RhaiStrategy::<MockBroker>::compile(r#"fn on_bar() { print("bar-hello"); buy(1.0); }"#)
            .expect("compiles");
    let mut b = MockBroker::default();
    let events = capture(|| strat.on_bar(&mut b, &bar(100.0)));

    let e = only(&events);
    assert_eq!(e.target, TARGET);
    assert_eq!(e.message, "bar-hello");
    assert_eq!(e.tier, "strategy");
    assert_eq!(b.submits, vec![(1, 1.0)], "the hook really ran — the print is not from a failure");
}

/// `debug()` is rhai's SECOND stdout hook and a separate `Box` on the engine — a fix that set only
/// `on_print` would leave this one on `println!`. It lands at DEBUG, so an operator can keep
/// `print` visible without the firehose.
#[test]
fn a_strategy_debug_goes_to_tracing_at_debug_level() {
    let events = capture(|| {
        RhaiStrategy::<MockBroker>::compile(r#"debug("dbg-hello"); fn on_bar() {}"#)
            .expect("compiles");
    });
    let e = only(&events);
    assert_eq!(e.target, TARGET);
    assert_eq!(e.level, Level::DEBUG, "`debug` must not be logged at INFO: {e:?}");
    assert!(e.message.contains("dbg-hello"), "the debugged value survives: {e:?}");
    assert_eq!(e.tier, "strategy");
}

// -------------------------------------------------------------------------------------------
// the user-indicator engine (`indicator.rs`'s `indicator_engine`)
// -------------------------------------------------------------------------------------------

/// ⚠ **The engine that is easy to miss.** This crate builds TWO, and #1520 (the module-resolver
/// fix) is the precedent: the same default-engine capability had to be closed in both, separately.
/// A user indicator is also the LEAST reviewed script the crate compiles — it is picked up from
/// `user_data/indicators/`, not named in a profile.
#[test]
fn a_user_indicator_top_level_print_goes_to_tracing() {
    let events = capture(|| {
        compile_indicator("chatty", r#"print("ind-hello"); fn on_bar(bar) { bar.close }"#)
            .expect("compiles");
    });
    let e = only(&events);
    assert_eq!(e.target, TARGET);
    assert_eq!(e.level, Level::INFO);
    assert_eq!(e.message, "ind-hello");
    assert_eq!(e.tier, "indicator", "the tier names the OTHER engine: {e:?}");
}

/// ...and per bar, where a chatty indicator would otherwise write one stdout line for every bar of
/// every backtest. The returned value is asserted so an empty capture cannot be explained by the
/// indicator having faulted before reaching its `print`.
#[test]
fn a_user_indicator_on_bar_print_goes_to_tracing() {
    let mut ind = compile_indicator("chatty", r#"fn on_bar(bar) { print("ind-bar"); 42.0 }"#)
        .expect("compiles");
    let events = capture(|| {
        assert_eq!(ind.on_bar(&bar(100.0)), vec![42.0]);
    });
    let e = only(&events);
    assert_eq!(e.target, TARGET);
    assert_eq!(e.message, "ind-bar");
    assert_eq!(e.tier, "indicator");
    assert!(ind.fault().is_none(), "the bar was clean: {:?}", ind.fault());
}

// -------------------------------------------------------------------------------------------
// fixtures — duplicated from `module_import_refused.rs` because cargo compiles each `tests/*.rs`
// as its own crate.
// -------------------------------------------------------------------------------------------

fn bar(c: f64) -> Bar {
    Bar {
        ts: 0,
        open: c,
        high: c,
        low: c,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("X".into()),
    }
}

#[derive(Default)]
struct MockBroker {
    submits: Vec<(i32, f64)>,
    bars: Vec<Bar>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _s: &str, side: i32, qty: f64) {
        self.submits.push((side, qty));
    }
    fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        0.0
    }
    fn price(&self, _s: &str) -> f64 {
        self.bars.last().map(|b| b.close).unwrap_or(0.0)
    }
    fn equity(&self) -> f64 {
        10_000.0
    }
    fn bars(&self, _s: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn now(&self) -> i64 {
        0
    }
}
