//! **A feed thread's status must reach the JOURNAL, on a transition, once** —
//! `vike_binance::family::market_feed`'s `FeedCtx::set_status`.
//!
//! # The hole
//!
//! `set_status` wrote a `String` into an `Arc<Mutex<String>>` the desktop's status bar reads, and
//! emitted nothing else. In a headless process — the recorder, the datahub daemon — nobody holds
//! that mutex, so every `seed error` and every `ws error (reconnecting)` a feed thread produced on
//! those boxes was written to memory nothing would ever read. That is one half of why a binance perp
//! depth lane could reconnect-loop for forty days leaving a journal with nothing in it (the other
//! half was `crates/vike-data/src/live_rec.rs`'s `RecorderSink::stream_status`, which recorded
//! stream-health markers for the `book` lane only — fixed in the same change).
//!
//! # Why a TRANSITION and not every call
//!
//! These strings are produced on reconnect loops. A log line per call would put a per-cycle event
//! into a file layer that defaults to `trace`, which is the shape that once wrote 341 GB and nearly
//! filled the disk hosting a live trading node (root `CLAUDE.md`, Logging). So the emit is gated on
//! the status TEXT CHANGING: entering a degraded state speaks once, a venue repeating the identical
//! error every three seconds speaks once, and coming back `LIVE ·` speaks once at `info!`. The
//! `warn!`/`info!` split is what makes `journalctl -p warning` show a feed that stopped working.
//!
//! # Why its own binary, and why the capture is hand-rolled
//!
//! `tracing` caches an `Interest` verdict PER CALLSITE, process-globally, written by whichever
//! thread reaches it first — and this crate's other test binaries drive feed bodies (and therefore
//! this callsite) on threads with no subscriber. One test file is one binary; the subscriber is
//! installed once, before the first emit, and `tracing::callsite::rebuild_interest_cache()` runs
//! after the install regardless of whether anything is believed to have poisoned it.
//! `tracing-subscriber` is not a dependency here, so the capture is a minimal [`Subscriber`]
//! enabled only for this module's target.
//!
//! # Kill proof
//!
//! Remove the two `tracing` calls from `FeedCtx::set_status` (leaving the mutex write and the wake)
//! and every assertion below that counts lines reads zero. Remove the `changed` guard so it speaks
//! unconditionally and the line counts in `a_status_transition_reaches_the_journal_and_a_repeated_status_speaks_once` fail.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber, span};
use vike_binance::family::market_feed::FeedCtx;
use vike_binance::market_feed::BINANCE_URLS;

/// The tracing target both callsites in `set_status` carry.
const TARGET: &str = "vike_binance::family::market_feed";

fn captured() -> &'static Mutex<Vec<String>> {
    static LOG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

fn test_init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(Capture { next_span: AtomicU64::new(1) });
        tracing::callsite::rebuild_interest_cache();
    });
}

fn lines() -> Vec<String> {
    captured().lock().expect("capture poisoned").clone()
}

struct Capture {
    next_span: AtomicU64,
}

impl Subscriber for Capture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == TARGET
            && (*metadata.level() == Level::WARN || *metadata.level() == Level::INFO)
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut v = FieldVisitor { out: format!("{} ", event.metadata().level()) };
        event.record(&mut v);
        captured().lock().expect("capture poisoned").push(v.out);
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

struct FieldVisitor {
    out: String,
}

impl FieldVisitor {
    fn push(&mut self, field: &Field, value: &str) {
        self.out.push_str(field.name());
        self.out.push('=');
        self.out.push_str(value);
        self.out.push(' ');
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        self.push(field, &rendered);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value);
    }
}

/// A `FeedCtx` with the network-free stand-ins the family's own bodies accept: the shared
/// `NoopSink` double, a wake counter, and the real `BINANCE_URLS` spec so the venue field is the
/// one an operator would read.
fn ctx(wakes: Arc<AtomicU64>) -> FeedCtx {
    FeedCtx {
        sink: Arc::new(vike_data::test_support::NoopSink),
        status: Arc::new(Mutex::new(String::new())),
        wake: Arc::new(move || {
            wakes.fetch_add(1, Ordering::Relaxed);
        }),
        stop: Arc::new(AtomicBool::new(false)),
        spec: vike_binance::family::FamilySpec {
            venue: "binance",
            display: "Binance",
            urls: BINANCE_URLS,
        },
    }
}

/// ONE test: the capture buffer is process-global, so assertions about how many lines exist after
/// a given sequence cannot be split into `#[test]`s that would race inside one binary.
#[test]
fn a_status_transition_reaches_the_journal_and_a_repeated_status_speaks_once() {
    test_init();
    let wakes = Arc::new(AtomicU64::new(0));
    let ctx = ctx(Arc::clone(&wakes));

    // --- entering a degraded state SPEAKS, at WARN, naming the venue and the status text.
    ctx.set_status("BTCUSDT.P@1m ws error (reconnecting): connection reset".to_string());
    let after_first = lines();
    assert_eq!(after_first.len(), 1, "a degradation speaks exactly once: {after_first:?}");
    assert!(after_first[0].starts_with("WARN "), "a broken feed is a WARN: {}", after_first[0]);
    assert!(after_first[0].contains("venue=binance"), "…naming the venue: {}", after_first[0]);
    assert!(
        after_first[0].contains("ws error (reconnecting): connection reset"),
        "…and carrying the status an operator would have read in the GUI: {}",
        after_first[0]
    );

    // --- THE RECONNECT LOOP. The identical error, over and over, is the shape that produced ~800
    // --- cycles an hour on the broken depth lane. It must not produce ~800 journal lines.
    for _ in 0..200 {
        ctx.set_status("BTCUSDT.P@1m ws error (reconnecting): connection reset".to_string());
    }
    assert_eq!(
        lines().len(),
        1,
        "a repeated status is not a transition and must stay quiet: {:?}",
        lines()
    );

    // --- …while the mutex and the repaint keep working exactly as before: the GUI channel is
    // --- unchanged, the journal is the ADDITION. 201 calls, 201 wakes.
    assert_eq!(wakes.load(Ordering::Relaxed), 201, "every call still wakes the UI");

    // --- a DIFFERENT error is a different fact and speaks again.
    ctx.set_status("BTCUSDT.P@1m seed error: 451 Unavailable For Legal Reasons".to_string());
    assert_eq!(lines().len(), 2, "a changed status is a transition: {:?}", lines());

    // --- and RECOVERY speaks at INFO, so an operator can tell a resolved incident from a live one.
    ctx.set_status("LIVE · Binance".to_string());
    let after_recovery = lines();
    assert_eq!(after_recovery.len(), 3, "{after_recovery:?}");
    assert!(
        after_recovery[2].starts_with("INFO "),
        "a healthy feed is not a warning: {}",
        after_recovery[2]
    );
    assert!(after_recovery[2].contains("LIVE · Binance"), "{}", after_recovery[2]);

    // --- …and a repeated healthy status is silent too (a feed that re-seeds hourly must not
    // --- narrate it).
    ctx.set_status("LIVE · Binance".to_string());
    assert_eq!(lines().len(), 3, "a repeated LIVE is not a transition: {:?}", lines());

    // --- THE SUCCESS DISCLOSURE (2026-09-11). Every lane's `SessionStatus::Live` arm now writes
    // --- `live_status(&ctx.spec)` on the first confirmed frame of EVERY session, where the
    // --- healthy write used to happen once per feed and never again. That turns this file's
    // --- dedup from a nicety into the thing that bounds the new write rate: a venue reconnecting
    // --- on its 3 s ladder now writes a healthy string per recovery, and without the transition
    // --- gate each one would be a journal line. Pin that the producer's text IS the text asserted
    // --- above — a drift between them would silently move every recovery from INFO to WARN (the
    // --- `HEALTHY_STATUS_PREFIX` classification) and re-open the repeat.
    let produced = vike_binance::family::market_feed::live_status(&ctx.spec);
    assert_eq!(
        produced, "LIVE · Binance",
        "the `Live` arm's text is what this test asserts the journal level on"
    );
    ctx.set_status(produced);
    assert_eq!(
        lines().len(),
        3,
        "the producer's own healthy text must dedup against the one already published — otherwise \
         every reconnect is a line: {:?}",
        lines()
    );
}
