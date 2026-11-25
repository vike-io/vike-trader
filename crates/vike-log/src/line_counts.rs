//! The line counts: how many ERROR and WARN lines this process logged since it started, per target.
//!
//! [`LineCounts::layer`] is the one way a line is counted: every ERROR and WARN event it is handed adds one to its
//! TARGET's count (the module path, or a `log` record's own target). Never the message text: the targets are the
//! binary's own callsites, so the key set is small and fixed, and a count can be shown or alerted on per target.
//! `init` installs the process's one under the same subscriber as the console and the file
//! ([`process_line_counts`]), so the panic hook's line (target `panic`) is counted like any other.
//!
//! ⚠ The layer carries a per-layer `WARN` filter, so it adds interest in no INFO, DEBUG or TRACE callsite: a quiet
//! hot path stays exactly as disabled as it was, and a WARN or ERROR line costs one lock and one map lookup more.
//!
//! ⚠ One failure logged twice is counted twice: log once, where the decision is made (the root `CLAUDE.md`'s
//! "Logging (tracing)").

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use tracing::{Event, Level, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// One target's lines since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LevelCounts {
    pub errors: u64,
    pub warnings: u64,
    /// When its newest ERROR line was logged, epoch ms on this process's clock; `0` = none yet. What tells a count
    /// that is still rising from one left over from the start.
    pub latest_error_ms: i64,
}

/// A shared handle on the counts: every clone counts into, and reads, the same map.
#[derive(Clone, Default)]
pub struct LineCounts(Arc<Mutex<BTreeMap<String, LevelCounts>>>);

impl std::fmt::Debug for LineCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineCounts").finish_non_exhaustive()
    }
}

impl LineCounts {
    /// Every target that logged an ERROR or a WARN line, with its counts, in target order. A copy.
    pub fn by_target(&self) -> BTreeMap<String, LevelCounts> {
        self.map().clone()
    }

    /// The layer that counts into this handle, under its `WARN` filter. `init` installs the process's one; a test
    /// installs its own.
    pub fn layer<S>(&self) -> impl Layer<S> + use<S>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        Counting(self.clone()).with_filter(LevelFilter::WARN)
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<String, LevelCounts>> {
        // A panic while the lock was held leaves whole counts behind: keep counting.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The counting half of [`LineCounts::layer`], private so nothing counts without the filter.
struct Counting(LineCounts);

impl<S: Subscriber> Layer<S> for Counting {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or(event.metadata());
        let (target, level) = (metadata.target(), *metadata.level());
        let mut map = self.0.map();
        // A key is allocated only for a target's first line.
        match map.get_mut(target) {
            Some(counts) => counts.add(level),
            None => {
                let mut counts = LevelCounts::default();
                counts.add(level);
                map.insert(target.to_string(), counts);
            }
        }
    }
}

impl LevelCounts {
    fn add(&mut self, level: Level) {
        if level == Level::ERROR {
            self.errors += 1;
            self.latest_error_ms = now_ms();
        } else if level == Level::WARN {
            self.warnings += 1;
        }
    }
}

/// Epoch ms now; `0` on a clock set before 1970 (which then reads as "no ERROR yet", never a wrong age).
fn now_ms() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |span| i64::try_from(span.as_millis()).unwrap_or(i64::MAX))
}

/// The counts `init` installed for this process. Set once, by the `init` whose subscriber became the global one.
pub(crate) static PROCESS_LINE_COUNTS: OnceLock<LineCounts> = OnceLock::new();

/// **This process's counts, or `None` when no `init` installed them** (a test, a tool that logs through
/// `test_init`): not counted, which a reader must never show as "no line logged".
pub fn process_line_counts() -> Option<&'static LineCounts> {
    PROCESS_LINE_COUNTS.get()
}

/// Run `f` on this thread under a subscriber that only counts, and return the counts: how a test fills a
/// [`LineCounts`] without a subscriber of its own (behind `test-support`, through the one scoped door).
#[cfg(any(test, feature = "test-support"))]
pub fn counted(f: impl FnOnce()) -> LineCounts {
    use tracing_subscriber::layer::SubscriberExt;
    let counts = LineCounts::default();
    crate::capture::scoped(tracing_subscriber::registry().with(counts.layer()), f);
    counts
}

#[cfg(test)]
#[path = "line_counts_tests.rs"]
mod line_counts_tests;
