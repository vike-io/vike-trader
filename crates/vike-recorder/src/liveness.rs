//! The silence watchdog: which subscribed series have stopped receiving rows.
//!
//! ## The failure this exists for
//!
//! A venue feed that is subscribed and CONNECTED but receiving nothing is invisible from every
//! other vantage point in this daemon. `subscribe_*` returned `Ok`, so the reconcile report says
//! `started=N failed=0`. The socket is `ESTABLISHED`. No error is ever raised, because nothing
//! failed — the venue simply stops sending. The loss counters stay at zero: there are no rows to
//! lose. And the store stops growing, which nothing was watching.
//!
//! the CI box's recorder ran **95 minutes** in exactly that state, writing nothing for binance while
//! logging a clean startup and zero warnings. It was noticed by accident, days later, by reading
//! row counts by hand. That is the gap this closes: rows ARRIVING is the only signal that means
//! "this series is really recording", so it is compared against a threshold every tick.
//!
//! ## Why "expected" comes from the caller
//!
//! [`vike_data::RecorderHandle::liveness`] only knows about series that have received at least one
//! row — so on its own it can never report the worst case, a series that **never started**. The
//! runtime knows what it subscribed, so it passes that set in and the two are diffed here.
//!
//! ## Two watches, one shape
//!
//! [`SilenceWatch`] watches SERIES — rows arriving on something already subscribed. [`ResolveWatch`]
//! watches FEEDS — a venue that has never produced a symbol to subscribe in the first place. They
//! are separate because the first is structurally BLIND to the second: `expected` comes from what
//! the runtime subscribed, and a venue that never resolved subscribed nothing, so the series watch
//! iterates an empty set and reports nothing at 2 seconds or at 2 days. That blindness is why a
//! misconfigured Polymarket proxy recorded nothing for a whole run while every watchdog stayed
//! green.
//!
//! Pure and clock-injected: `now_ms` is a parameter, so the tests own time.

use std::collections::HashMap;

use vike_data::Liveness;

/// One series that is not receiving rows, and which kind of not-receiving it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Silent {
    /// `"{kind}/{venue}/{symbol}"` — the key [`vike_data::RecorderHandle::liveness`] uses.
    pub series: String,
    /// `Some(ms)` = it received rows and then stopped this long ago; `None` = it has NEVER
    /// received one. Distinct on purpose: the first is usually a venue-side stream death, the
    /// second is usually a wrong stream name or an unsupported subscription that reported success.
    pub silent_for_ms: Option<i64>,
    /// Rows this series has received in total — `0` exactly when `silent_for_ms` is `None`.
    pub rows: u64,
}

/// The series in `expected` that are not receiving rows: never-started ones, plus those whose last
/// row is older than `threshold_ms`.
///
/// `expected` is what the runtime believes it subscribed; `live` is
/// [`vike_data::RecorderHandle::liveness`]. A key present in `live` but absent from `expected` is
/// IGNORED rather than reported — it is a series this recorder is no longer subscribed to (a
/// rotated-out Polymarket token, say), and its silence is correct, not a fault.
///
/// Output is sorted by series name so a log line is stable between ticks.
pub fn silent_series(
    expected: &[String],
    live: &HashMap<String, Liveness>,
    now_ms: i64,
    threshold_ms: i64,
) -> Vec<Silent> {
    let mut out: Vec<Silent> = expected
        .iter()
        .filter_map(|series| match live.get(series) {
            None => Some(Silent { series: series.clone(), silent_for_ms: None, rows: 0 }),
            Some(l) => {
                let age = now_ms - l.last_ms;
                (age > threshold_ms).then(|| Silent {
                    series: series.clone(),
                    silent_for_ms: Some(age),
                    rows: l.rows,
                })
            }
        })
        .collect();
    out.sort_by(|a, b| a.series.cmp(&b.series));
    out
}

/// [`silent_series`] plus the one piece of state it needs: when each series was first subscribed.
///
/// **Why the pure function is not enough.** A series that has never received a row is silent by
/// definition the instant it is subscribed — so reporting it immediately would fire on every
/// startup and on every Polymarket token rotation, which is how a warning column becomes noise
/// nobody reads. A never-started series is only a FAULT once it has had the same grace the
/// stopped-series case gets, so this remembers when each key first appeared and withholds the
/// report until `threshold_ms` has passed since then.
///
/// Series that disappear from `expected` are forgotten, so a rotated-out token cannot make the map
/// grow without bound on a long-running daemon.
///
/// It also owns the PER-SERIES notification gate ([`alertable`](Self::alertable)) — a different
/// question from how often a silent series is LOGGED, and one the alerting rule cannot answer.
#[derive(Debug, Default)]
pub struct SilenceWatch {
    first_seen: HashMap<String, i64>,
    /// when each series last produced an ALERT — the per-series repeat gate. Dropped for a series
    /// the moment it stops being silent, so a fresh episode pages immediately.
    last_alerted: HashMap<String, i64>,
}

impl SilenceWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The silent series worth reporting this tick. Same arguments as [`silent_series`], plus the
    /// grace described above.
    pub fn check(
        &mut self,
        expected: &[String],
        live: &HashMap<String, Liveness>,
        now_ms: i64,
        threshold_ms: i64,
    ) -> Vec<Silent> {
        for s in expected {
            self.first_seen.entry(s.clone()).or_insert(now_ms);
        }
        self.first_seen.retain(|k, _| expected.iter().any(|e| e == k));

        silent_series(expected, live, now_ms, threshold_ms)
            .into_iter()
            .filter(|s| match s.silent_for_ms {
                // Already had rows: the age check in `silent_series` is the whole judgment.
                Some(_) => true,
                // Never had one: only a fault once it has been subscribed long enough to have had
                // a fair chance.
                None => self
                    .first_seen
                    .get(&s.series)
                    .is_some_and(|first| now_ms - first > threshold_ms),
            })
            .collect()
    }

    /// Which of `silent` should raise an ALERT right now — [`check`](Self::check)'s output, gated
    /// per SERIES so a page is not the same event as a log line.
    ///
    /// **Why the gate lives here and not on the rule.** `AlertRule::cooldown_ms` is per RULE: with
    /// one rule covering every series (which is the useful shape — a recorder's series set rotates,
    /// so per-series rules are unwritable), the first silent series would consume the cooldown and
    /// the other five would be swallowed. the CI box's watchdog fired for SIX series in one episode;
    /// paging about one of them and silently dropping the rest is worse than not paging at all,
    /// because it reads as a single-series fault.
    ///
    /// `repeat_ms = 0` pages ONCE per silence EPISODE: a series that drops out of `silent` — it
    /// recovered, or it rotated out and is no longer expected — forgets its last alert, so the next
    /// episode pages immediately. A positive `repeat_ms` re-pages a still-silent series that often,
    /// which is what an operator wants for an outage that outlives one shift.
    ///
    /// Clock-injected like the rest of this module, so a test owns time.
    pub fn alertable(&mut self, silent: &[Silent], now_ms: i64, repeat_ms: i64) -> Vec<Silent> {
        // Recovery FIRST: a series absent from this tick's silent set is no longer in an episode,
        // and holding its old timestamp would suppress the next genuine one.
        self.last_alerted.retain(|k, _| silent.iter().any(|s| &s.series == k));

        // A plain loop, not a filter+map chain: the two closures would capture `last_alerted`
        // shared and mutable at once, which does not borrow-check.
        let mut due = Vec::new();
        for s in silent {
            let fire = match self.last_alerted.get(&s.series) {
                None => true, // first tick of this episode
                Some(last) => repeat_ms > 0 && now_ms.saturating_sub(*last) >= repeat_ms,
            };
            if fire {
                self.last_alerted.insert(s.series.clone(), now_ms);
                due.push(s.clone());
            }
        }
        due
    }
}

/// One feed's resolve state, as [`ResolveWatch`] sees it — [`crate::runtime::FeedTick`] narrowed to
/// the two things the judgment needs.
///
/// Deliberately NOT the `FeedTick` itself: this module is the pure judgment layer and takes plain
/// data, exactly as [`silent_series`] takes `expected`/`live` rather than a runtime handle.
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

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn live(pairs: &[(&str, u64, i64)]) -> HashMap<String, Liveness> {
        pairs
            .iter()
            .map(|(k, rows, last_ms)| (k.to_string(), Liveness { rows: *rows, last_ms: *last_ms }))
            .collect()
    }
    pub(super) fn expect(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_flowing_series_is_not_reported() {
        let got = silent_series(
            &expect(&["trade/binance/BTC"]),
            &live(&[("trade/binance/BTC", 9, 990)]),
            1_000,
            60_000,
        );
        assert!(got.is_empty(), "{got:?}");
    }

    /// **The 95-minute case.** It received rows, then the venue stream went quiet — the loss
    /// counters stay zero and nothing errors, so this is the only place it shows.
    #[test]
    fn a_series_that_stopped_is_reported_with_its_age() {
        let got = silent_series(
            &expect(&["trade/binance/BTC"]),
            &live(&[("trade/binance/BTC", 6_000, 1_000)]),
            1_000 + 95 * 60_000,
            60_000,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].silent_for_ms, Some(95 * 60_000));
        assert_eq!(got[0].rows, 6_000, "it DID receive rows once — that is the diagnosis");
    }

    /// **The worse case**, and the one the handle alone can never report: a series with no entry at
    /// all. `RecorderHandle::liveness` only knows series that received something, so "never
    /// started" is only visible by diffing against what was subscribed.
    #[test]
    fn a_series_that_never_started_is_reported_distinctly() {
        let got = silent_series(&expect(&["depth/binance/BTC"]), &live(&[]), 5_000, 60_000);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].silent_for_ms, None, "never-started is not 'stale for 5s'");
        assert_eq!(got[0].rows, 0);
    }

    /// A series still inside the threshold is quiet, not silent — a 5-minute-window family can go
    /// a minute between rows without being a fault.
    #[test]
    fn a_pause_shorter_than_the_threshold_is_not_a_fault() {
        let got = silent_series(
            &expect(&["trade/poly/T"]),
            &live(&[("trade/poly/T", 3, 0)]),
            59_000,
            60_000,
        );
        assert!(got.is_empty());
    }

    /// A series the recorder is no longer subscribed to is IGNORED — a rotated-out Polymarket
    /// token stops receiving rows by design, and reporting it would bury the real faults.
    #[test]
    fn an_unsubscribed_series_is_not_reported() {
        let got = silent_series(
            &expect(&["trade/poly/NEW"]),
            &live(&[("trade/poly/OLD", 500, 0), ("trade/poly/NEW", 1, 9_000)]),
            10_000,
            60_000,
        );
        assert!(got.is_empty(), "only OLD is stale, and OLD is no longer expected: {got:?}");
    }

    #[test]
    fn output_is_sorted_so_a_log_line_is_stable() {
        let got = silent_series(&expect(&["b/v/s", "a/v/s", "c/v/s"]), &live(&[]), 0, 0);
        let names: Vec<&str> = got.iter().map(|s| s.series.as_str()).collect();
        assert_eq!(names, vec!["a/v/s", "b/v/s", "c/v/s"]);
    }
}

#[cfg(test)]
mod watch_tests {
    use super::tests::{expect, live};
    use super::*;

    /// **The false positive that would make this unreadable.** A just-subscribed series has
    /// received nothing BY DEFINITION; reporting it immediately fires on every startup and every
    /// Polymarket token rotation.
    #[test]
    fn a_just_subscribed_series_is_not_reported_yet() {
        let mut w = SilenceWatch::new();
        let e = expect(&["trade/binance/BTC"]);
        assert!(w.check(&e, &live(&[]), 0, 60_000).is_empty(), "t=0");
        assert!(w.check(&e, &live(&[]), 59_000, 60_000).is_empty(), "still inside the grace");
    }

    /// …but once it HAS had its grace and still never received a row, it is a real fault — a wrong
    /// stream name the venue accepted anyway.
    #[test]
    fn a_series_that_never_starts_is_reported_after_the_grace() {
        let mut w = SilenceWatch::new();
        let e = expect(&["trade/binance/BTC"]);
        let _ = w.check(&e, &live(&[]), 0, 60_000);
        let got = w.check(&e, &live(&[]), 61_000, 60_000);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].silent_for_ms, None);
    }

    /// The grace applies ONLY to never-started series. One that received rows and then stopped is
    /// judged on its own age, so a feed dying right after startup is still caught immediately.
    #[test]
    fn the_grace_does_not_delay_a_series_that_stopped() {
        let mut w = SilenceWatch::new();
        let e = expect(&["trade/binance/BTC"]);
        let got = w.check(&e, &live(&[("trade/binance/BTC", 10, 0)]), 61_000, 60_000);
        assert_eq!(got.len(), 1, "first tick ever, and it is already reported: {got:?}");
        assert_eq!(got[0].silent_for_ms, Some(61_000));
    }

    /// A rotated-out token is forgotten, so the map cannot grow without bound on a daemon that
    /// rotates every 5 minutes forever.
    #[test]
    fn departed_series_are_forgotten() {
        let mut w = SilenceWatch::new();
        let _ = w.check(&expect(&["trade/poly/OLD"]), &live(&[]), 0, 60_000);
        let _ = w.check(&expect(&["trade/poly/NEW"]), &live(&[]), 1_000, 60_000);
        assert_eq!(w.first_seen.len(), 1);
        assert!(w.first_seen.contains_key("trade/poly/NEW"));
    }

    /// A re-subscribed token gets a FRESH grace rather than inheriting the old one — it is a new
    /// subscription to a new stream, and judging it by when its predecessor appeared would report
    /// it instantly.
    #[test]
    fn a_returning_series_gets_a_fresh_grace() {
        let mut w = SilenceWatch::new();
        let a = expect(&["trade/poly/A"]);
        let _ = w.check(&a, &live(&[]), 0, 60_000);
        let _ = w.check(&expect(&["trade/poly/B"]), &live(&[]), 10_000, 60_000);
        assert!(w.check(&a, &live(&[]), 70_000, 60_000).is_empty(), "re-subscribed at t=70_000");
    }
}

/// The PER-SERIES notification gate: how often a silent series may PAGE, as opposed to how often it
/// is logged (every tick, unchanged).
#[cfg(test)]
mod alertable_tests {
    use super::*;

    fn silent(names: &[&str]) -> Vec<Silent> {
        names
            .iter()
            .map(|n| Silent { series: n.to_string(), silent_for_ms: Some(600_000), rows: 7 })
            .collect()
    }

    /// **The reason this gate is not `AlertRule::cooldown_ms`.** the CI box's watchdog fired for SIX
    /// series in one episode; a per-RULE cooldown would page for one of them and swallow five,
    /// which reads to an operator as a single-series fault.
    #[test]
    fn every_series_of_one_episode_alerts_not_just_the_first() {
        let mut w = SilenceWatch::new();
        let got = w.alertable(&silent(&["a/v/s", "b/v/s", "c/v/s"]), 1_000, 3_600_000);
        assert_eq!(got.len(), 3, "all three, on one tick, under one rule: {got:?}");
    }

    /// A still-silent series does not re-page every tick — the watchdog runs every 30s and an
    /// outage lasts hours.
    #[test]
    fn a_still_silent_series_does_not_repage_within_the_repeat_window() {
        let mut w = SilenceWatch::new();
        let s = silent(&["a/v/s"]);
        assert_eq!(w.alertable(&s, 0, 3_600_000).len(), 1, "first tick pages");
        assert!(w.alertable(&s, 30_000, 3_600_000).is_empty(), "30s later: suppressed");
        assert!(w.alertable(&s, 3_599_999, 3_600_000).is_empty(), "just inside the window");
        assert_eq!(w.alertable(&s, 3_600_000, 3_600_000).len(), 1, "the window elapsed → re-pages");
    }

    /// `repeat_ms = 0` means ONCE per episode: never re-page while it stays silent.
    #[test]
    fn a_zero_repeat_pages_once_per_episode_and_never_again_while_it_lasts() {
        let mut w = SilenceWatch::new();
        let s = silent(&["a/v/s"]);
        assert_eq!(w.alertable(&s, 0, 0).len(), 1);
        assert!(w.alertable(&s, 86_400_000, 0).is_empty(), "a day later, still one episode");
    }

    /// …but a NEW episode pages immediately, however the last one ended. Without the recovery
    /// sweep, a series that came back and died again would be silently suppressed for a whole
    /// repeat window — the failure this whole file exists to make impossible.
    #[test]
    fn recovery_rearms_so_the_next_episode_pages_at_once() {
        let mut w = SilenceWatch::new();
        let s = silent(&["a/v/s"]);
        assert_eq!(w.alertable(&s, 0, 3_600_000).len(), 1);
        // it recovered: this tick's silent set no longer names it.
        assert!(w.alertable(&[], 1_000, 3_600_000).is_empty());
        // …and it dies again, well inside the repeat window.
        assert_eq!(w.alertable(&s, 2_000, 3_600_000).len(), 1, "a fresh episode is a fresh page");
    }

    /// A rotated-out Polymarket token leaves the silent set forever; its bookkeeping must go with
    /// it or the map grows without bound on a daemon that rotates every 5 minutes.
    #[test]
    fn departed_series_do_not_accumulate() {
        let mut w = SilenceWatch::new();
        let _ = w.alertable(&silent(&["trade/poly/OLD"]), 0, 0);
        let _ = w.alertable(&silent(&["trade/poly/NEW"]), 1_000, 0);
        assert_eq!(w.last_alerted.len(), 1);
        assert!(w.last_alerted.contains_key("trade/poly/NEW"));
    }
}

/// The FEED-level watch: never-resolved vs resolved-then-failed, its grace, and its per-venue pager
/// gate.
#[cfg(test)]
mod resolve_watch_tests {
    use super::*;

    fn never(venue: &str) -> FeedResolve {
        FeedResolve { venue: venue.into(), last_nonempty_ms: None }
    }
    fn worked_at(venue: &str, ms: i64) -> FeedResolve {
        FeedResolve { venue: venue.into(), last_nonempty_ms: Some(ms) }
    }

    /// **The false positive that would make this unusable.** A feed is unresolved BY DEFINITION on
    /// the tick it is mounted; reporting it at once fires on every startup of every box.
    #[test]
    fn a_just_mounted_venue_is_not_reported_yet() {
        let mut w = ResolveWatch::new();
        assert!(w.check(&[never("polymarket")], 0, 300_000).is_empty(), "t=0");
        assert!(w.check(&[never("polymarket")], 299_000, 300_000).is_empty(), "inside the grace");
    }

    /// **The measured failure.** Past the grace and it has still never produced a symbol: a proxy
    /// nothing is listening behind, a family name that matches no market. It will not fix itself.
    #[test]
    fn a_venue_that_never_resolves_is_reported_after_the_grace() {
        let mut w = ResolveWatch::new();
        let _ = w.check(&[never("polymarket")], 0, 300_000);
        assert_eq!(w.check(&[never("polymarket")], 301_000, 300_000), vec!["polymarket"]);
    }

    /// ⚠ **The retry case, which must NEVER escalate.** `crate::runtime`'s module doc is why: a
    /// resolution failure changes nothing and is retried next tick, and paging for a Gamma blip is
    /// how a pager gets muted.
    #[test]
    fn a_venue_that_resolved_once_is_never_reported_however_long_it_has_been_failing() {
        let mut w = ResolveWatch::new();
        let _ = w.check(&[worked_at("polymarket", 1_000)], 1_000, 300_000);
        assert!(
            w.check(&[worked_at("polymarket", 1_000)], 86_400_000, 300_000).is_empty(),
            "a day of failing resolves, after ONE success, is still a retry — not a page"
        );
    }

    /// A three-venue box with one dead venue must name exactly that one, and name it.
    #[test]
    fn only_the_never_resolved_venues_are_reported_and_the_output_is_sorted() {
        let mut w = ResolveWatch::new();
        let feeds =
            [never("polymarket"), worked_at("binance", 500), never("aster"), worked_at("okx", 500)];
        let _ = w.check(&feeds, 0, 300_000);
        assert_eq!(w.check(&feeds, 301_000, 300_000), vec!["aster", "polymarket"]);
    }

    /// A venue dropped from the profile is forgotten, so a long-running daemon's map cannot grow
    /// without bound — and a re-added venue gets a FRESH grace rather than inheriting the old one.
    #[test]
    fn departed_venues_are_forgotten_and_a_returning_one_gets_a_fresh_grace() {
        let mut w = ResolveWatch::new();
        let _ = w.check(&[never("polymarket")], 0, 300_000);
        let _ = w.check(&[never("binance")], 1_000, 300_000);
        assert_eq!(w.first_seen.len(), 1);
        assert!(w.first_seen.contains_key("binance"));
        // polymarket comes back at t=1_000's successor: it must get its whole grace again.
        assert!(w.check(&[never("polymarket")], 2_000, 300_000).is_empty());
        assert!(w.check(&[never("polymarket")], 301_000, 300_000).is_empty(), "grace from t=2_000");
        assert_eq!(w.check(&[never("polymarket")], 303_000, 300_000), vec!["polymarket"]);
    }

    /// Every venue of one episode pages — the reason this gate is not `AlertRule::cooldown_ms`.
    #[test]
    fn every_unresolved_venue_of_one_episode_alerts_not_just_the_first() {
        let mut w = ResolveWatch::new();
        let all = ["aster".to_string(), "binance".to_string(), "polymarket".to_string()];
        assert_eq!(w.alertable(&all, 1_000, 3_600_000).len(), 3, "all three, on one tick");
    }

    /// A still-unresolved venue does not re-page every tick — the daemon ticks every 30 s.
    #[test]
    fn a_still_unresolved_venue_does_not_repage_within_the_repeat_window() {
        let mut w = ResolveWatch::new();
        let one = ["polymarket".to_string()];
        assert_eq!(w.alertable(&one, 0, 3_600_000).len(), 1, "first tick pages");
        assert!(w.alertable(&one, 30_000, 3_600_000).is_empty(), "30s later: suppressed");
        assert_eq!(w.alertable(&one, 3_600_000, 3_600_000).len(), 1, "the window elapsed");
    }

    /// …and a recovery re-arms, so the NEXT episode pages at once rather than waiting out a window.
    #[test]
    fn a_recovered_venue_rearms_and_a_fresh_episode_pages_immediately() {
        let mut w = ResolveWatch::new();
        let one = ["polymarket".to_string()];
        assert_eq!(w.alertable(&one, 0, 3_600_000).len(), 1);
        assert!(w.alertable(&[], 1_000, 3_600_000).is_empty(), "it resolved: nothing to page");
        assert_eq!(w.alertable(&one, 2_000, 3_600_000).len(), 1, "a fresh episode is a fresh page");
        assert_eq!(w.last_alerted.len(), 1, "…and the map does not accumulate");
    }
}
