//! The FEED-level watch: which venues have never resolved a symbol since startup.

use std::collections::HashMap;

/// One feed's resolve state, as [`ResolveWatch`] sees it — [`crate::runtime::FeedTick`] narrowed to
/// the two things the judgment needs.
///
/// Deliberately NOT the `FeedTick` itself: this module is the pure judgment layer and takes plain
/// data, exactly as [`silent_series`] takes `expected`/`live` rather than a runtime handle.
///
/// [`silent_series`]: super::silent_series
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedResolve {
    pub venue: String,
    /// See [`crate::runtime::FeedTick::last_nonempty_ms`]. `None` = this venue has never produced a
    /// symbol since startup.
    pub last_nonempty_ms: Option<i64>,
}

/// [`SilenceWatch`]'s FEED-level twin: which venues have NEVER resolved a symbol since startup.
///
/// ## The gap it closes
///
/// [`SilenceWatch`] can only report a series the runtime believes it SUBSCRIBED
/// (`RecorderRuntime::expected_series`). A venue whose resolve fails has zero subscriptions, so
/// that set is empty, so [`silent_series`] iterates nothing and returns nothing — at 2 seconds or
/// at 2 days. **The silence watchdog watches SERIES; nothing watched FEEDS.** That is why a
/// misconfigured Polymarket proxy produced one `warn!` a tick, forever, and no alert: measured on
/// the CI box, `could not resolve the desired set — subscriptions unchanged … Connection refused`.
///
/// ## What it does NOT report, deliberately
///
/// A venue that resolved before and is failing NOW (`last_nonempty_ms: Some(_)`). That is a retry —
/// a Gamma blip, a DNS wobble — the daemon leaves its live books alone on purpose
/// (`crate::runtime`'s module doc), and the per-tick `warn!` is the right and unchanged reaction.
/// Escalating it would page for every transient the daemon already handles correctly.
///
/// Same three properties as its sibling, for the same reasons: a per-VENUE `first_seen` grace (a
/// startup resolve is allowed one threshold to succeed, or every start pages), a per-VENUE repeat
/// gate with a recovery re-arm (a five-venue outage must page five times, not once — see
/// [`SilenceWatch::alertable`]), and departed-venue forgetting. Pure and clock-injected.
///
/// [`SilenceWatch`]: super::SilenceWatch
/// [`silent_series`]: super::silent_series
/// [`SilenceWatch::alertable`]: super::SilenceWatch::alertable
#[derive(Debug, Default)]
pub struct ResolveWatch {
    first_seen: HashMap<String, i64>,
    last_alerted: HashMap<String, i64>,
}

impl ResolveWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The venues that have never resolved a symbol and have had `grace_ms` to do it. Sorted, so a
    /// log line is stable between ticks.
    pub fn check(&mut self, feeds: &[FeedResolve], now_ms: i64, grace_ms: i64) -> Vec<String> {
        for f in feeds {
            self.first_seen.entry(f.venue.clone()).or_insert(now_ms);
        }
        self.first_seen.retain(|k, _| feeds.iter().any(|f| &f.venue == k));

        let mut out: Vec<String> = feeds
            .iter()
            .filter(|f| f.last_nonempty_ms.is_none())
            .filter(|f| {
                self.first_seen.get(&f.venue).is_some_and(|first| now_ms - first > grace_ms)
            })
            .map(|f| f.venue.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Which of `unresolved` should ALERT right now — the per-VENUE repeat gate, with a recovery
    /// re-arm. Verbatim [`SilenceWatch::alertable`]'s contract, keyed on venue instead of series,
    /// and it is NOT `AlertRule::cooldown_ms` for that method's documented reason: one rule covers
    /// every venue, so a per-rule cooldown would page for one venue of a three-venue outage and
    /// swallow the rest, which reads as a single-venue fault.
    ///
    /// [`SilenceWatch::alertable`]: super::SilenceWatch::alertable
    pub fn alertable(&mut self, unresolved: &[String], now_ms: i64, repeat_ms: i64) -> Vec<String> {
        self.last_alerted.retain(|k, _| unresolved.iter().any(|v| v == k));

        let mut due = Vec::new();
        for venue in unresolved {
            let fire = match self.last_alerted.get(venue) {
                None => true,
                Some(last) => repeat_ms > 0 && now_ms.saturating_sub(*last) >= repeat_ms,
            };
            if fire {
                self.last_alerted.insert(venue.clone(), now_ms);
                due.push(venue.clone());
            }
        }
        due
    }
}

/// The FEED-level watch: never-resolved vs resolved-then-failed, its grace, and its per-venue pager
/// gate.
#[path = "resolve_watch_tests.rs"]
#[cfg(test)]
mod resolve_watch_tests;
