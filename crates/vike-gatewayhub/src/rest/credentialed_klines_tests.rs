//! The engine over a scripted spec: the order of the four steps, one source per (connection, venue), the
//! source's life, the stop probe and the error words. Each venue's own spec is tested in its bridge.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vike_data::source::SourceError;

use super::*;

/// What a scripted source answers.
#[derive(Clone)]
enum Answer {
    /// One bar stamped `start_ms`.
    Bar,
    /// This error.
    Fail(SourceError),
    /// Raise the request's cancel flag, then answer whether the stop probe saw it.
    CancelThenProbe(FetchContext),
}

/// A spec for `venue` whose sources answer `answer`, counting the sources it built and flagging each
/// one's drop; it refuses the interval `1m` in its own words.
struct Scripted {
    venue: &'static str,
    answer: Answer,
    builds: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

struct ScriptedSource {
    answer: Answer,
    dropped: Arc<AtomicBool>,
}

impl Drop for ScriptedSource {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl CredentialedKlinesSource for ScriptedSource {
    fn fetch_until(
        &self,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        _end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        match &self.answer {
            Answer::Bar => Ok(vec![Bar {
                timestamp: start_ms,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }]),
            Answer::Fail(e) => Err(e.clone()),
            Answer::CancelThenProbe(ctx) => {
                let before = stop();
                ctx.cancel();
                Err(SourceError::Fetch(format!("stop before {before}, after {}", stop())))
            }
        }
    }
}

impl CredentialedKlinesSpec for Scripted {
    fn venue(&self) -> &'static str {
        self.venue
    }

    fn admit(&self, interval: &str) -> Result<(), SourceError> {
        if interval == "1m" {
            return Err(SourceError::Refused(format!("{}: `1m` is not served", self.venue)));
        }
        Ok(())
    }

    fn connection_source(&self) -> Box<dyn CredentialedKlinesSource> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        Box::new(ScriptedSource { answer: self.answer.clone(), dropped: Arc::clone(&self.dropped) })
    }

    fn presence(&self) -> CredentialPresence {
        CredentialPresence::Present
    }

    fn startup_line(&self, _settings_dir: Option<&Path>) -> (CredentialPresence, String) {
        (CredentialPresence::Present, format!("{} history lane: scripted", self.venue))
    }
}

/// A lane over a scripted spec for `venue`, and its build count and drop flag.
fn lane(
    venue: &'static str,
    answer: Answer,
) -> (CredentialedKlinesLane, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let builds = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let spec =
        Scripted { venue, answer, builds: Arc::clone(&builds), dropped: Arc::clone(&dropped) };
    (CredentialedKlinesLane::new(Box::new(spec)), builds, dropped)
}

fn fetch(
    lane: &CredentialedKlinesLane,
    scope: &ConnectionMemos,
    interval: &str,
    ctx: &FetchContext,
) -> Result<Vec<Bar>, SourceError> {
    lane.fetch(scope, "EURUSD", interval, 7, 8, ctx)
}

/// **One source per connection**: the connection's first fetch builds it, its later fetches reuse it, and
/// the next connection builds its own (a credential stored, rotated or removed between two requests is
/// what the next one reads).
#[test]
fn a_source_is_built_once_per_connection_and_again_for_the_next() {
    let (lane, builds, _) = lane("oanda", Answer::Bar);
    let ctx = FetchContext::new();
    let first = ConnectionMemos::new();
    for _ in 0..3 {
        let bars = fetch(&lane, &first, "1h", &ctx).expect("a fetch");
        assert_eq!(bars.iter().map(|b| b.timestamp).collect::<Vec<_>>(), [7]);
    }
    assert_eq!(builds.load(Ordering::SeqCst), 1, "three fetches of one connection, ONE source");
    let second = ConnectionMemos::new();
    fetch(&lane, &second, "1h", &ctx).expect("a fetch");
    assert_eq!(builds.load(Ordering::SeqCst), 2, "the next connection builds its own");
}

/// **A cancelled fetch builds nothing and creates no slot**, and says so in the daemon's words.
#[test]
fn a_cancelled_fetch_builds_nothing_and_creates_no_slot() {
    let (lane, builds, _) = lane("oanda", Answer::Bar);
    let scope = ConnectionMemos::new();
    let ctx = FetchContext::new();
    ctx.cancel();
    let out = fetch(&lane, &scope, "1h", &ctx);
    assert_eq!(
        out,
        Err(SourceError::Fetch(
            "gatewayhub fetch cancelled before the venue was asked".to_string()
        ))
    );
    assert_eq!((builds.load(Ordering::SeqCst), scope.is_empty()), (0, true));
}

/// **A request the spec does not admit is refused in the spec's words** before the slot, the source or
/// any read; the cancel flag is asked first.
#[test]
fn a_refused_admission_builds_nothing_and_keeps_its_words() {
    let (lane, builds, _) = lane("ig", Answer::Bar);
    let scope = ConnectionMemos::new();
    let out = fetch(&lane, &scope, "1m", &FetchContext::new());
    assert_eq!(out, Err(SourceError::Refused("ig: `1m` is not served".to_string())));
    assert_eq!((builds.load(Ordering::SeqCst), scope.is_empty()), (0, true));

    let cancelled = FetchContext::new();
    cancelled.cancel();
    assert!(matches!(fetch(&lane, &scope, "1m", &cancelled), Err(SourceError::Fetch(_))));
}

/// **A source's error keeps its variant and its words** across the engine.
#[test]
fn a_sources_error_keeps_its_variant_and_its_words() {
    for (error, wire) in [
        (SourceError::Fetch("venue said no".into()), SourceError::Fetch("venue said no".into())),
        (SourceError::Refused("cannot ask".into()), SourceError::Refused("cannot ask".into())),
    ] {
        let (lane, _, _) = lane("ctrader", Answer::Fail(error));
        assert_eq!(fetch(&lane, &ConnectionMemos::new(), "1h", &FetchContext::new()), Err(wire));
    }
}

/// **The stop probe a source is handed reads the request's cancel flag**, live.
#[test]
fn the_stop_probe_reads_the_requests_cancel_flag() {
    let ctx = FetchContext::new();
    let (lane, _, _) = lane("ig", Answer::CancelThenProbe(ctx.clone()));
    let out = fetch(&lane, &ConnectionMemos::new(), "1h", &ctx);
    assert_eq!(out, Err(SourceError::Fetch("stop before false, after true".to_string())));
}

/// **A connection's source lives as long as the connection and no longer**: what it read goes with it.
#[test]
fn a_source_is_dropped_with_its_connection() {
    let (lane, _, dropped) = lane("oanda", Answer::Bar);
    let scope = ConnectionMemos::new();
    fetch(&lane, &scope, "1h", &FetchContext::new()).expect("a fetch");
    assert!(!dropped.load(Ordering::SeqCst), "dropped while the connection was open");
    drop(scope);
    assert!(dropped.load(Ordering::SeqCst), "the source outlived its connection");
}

/// **Two venues on one connection keep a source each**: one does not answer for the other.
#[test]
fn two_venues_on_one_connection_keep_a_source_each() {
    let (oanda, oanda_builds, _) = lane("oanda", Answer::Bar);
    let (ig, ig_builds, _) = lane("ig", Answer::Fail(SourceError::Fetch("ig".into())));
    let scope = ConnectionMemos::new();
    let ctx = FetchContext::new();
    for _ in 0..2 {
        fetch(&oanda, &scope, "1h", &ctx).expect("oanda's source");
        assert_eq!(fetch(&ig, &scope, "1h", &ctx), Err(SourceError::Fetch("ig".into())));
    }
    assert_eq!((oanda_builds.load(Ordering::SeqCst), ig_builds.load(Ordering::SeqCst)), (1, 1));
}
