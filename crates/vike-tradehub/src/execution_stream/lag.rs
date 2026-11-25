//! **The stream's LAG, said and never acted on**: while the datahub has not stored what the journal holds, the journal
//! keeps every segment past its floor, so a lag is a disk-growth signal as much as a freshness one. One WARN when the
//! datahub trails the journal's durable head by more than [`LAG_WARN_RECORDS`] or for longer than [`LAG_WARN_AFTER`],
//! again at most every [`LAG_REWARN_EVERY`] while it lasts, and one INFO when it has caught up. Pure over its inputs
//! (the clock is a parameter). The retired journal materializer's `LagWatch` "behind" half, moved here with its numbers.

use std::path::Path;
use std::time::{Duration, Instant};

/// The lag bound in journal RECORDS: days of a quiet box's traffic and minutes of a busy one's, every one a record the
/// journal keeps on disk until the datahub stores it.
pub(super) const LAG_WARN_RECORDS: u64 = 100_000;

/// The lag bound in TIME: trailing the head at all for longer than this is a lag, however few records.
pub(super) const LAG_WARN_AFTER: Duration = Duration::from_secs(5 * 60);

/// How often a lag that persists is said again.
pub(super) const LAG_REWARN_EVERY: Duration = Duration::from_secs(15 * 60);

/// What one [`ExecutionStreamLag::observe`] has to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LagEvent {
    /// The datahub trails the journal by `lag` records, and has for `behind_for`.
    Behind { lag: u64, behind_for: Duration },
    /// It was said to be behind and has caught up.
    CaughtUp,
}

#[derive(Debug, Default)]
pub(super) struct ExecutionStreamLag {
    /// When the datahub first trailed the head in this episode; `None` while caught up.
    behind_since: Option<Instant>,
    /// When this episode was last said; `None` until it is.
    last_warn: Option<Instant>,
}

impl ExecutionStreamLag {
    /// Fold the journal's durable `head` and the `position` the datahub holds everything up to (`None`: nothing).
    pub(super) fn observe(
        &mut self,
        head: Option<u64>,
        position: Option<u64>,
        now: Instant,
    ) -> Option<LagEvent> {
        let count = |s: Option<u64>| s.map_or(0, |s| s.saturating_add(1));
        let lag = count(head).saturating_sub(count(position));
        if lag == 0 {
            self.behind_since = None;
            return self.last_warn.take().map(|_| LagEvent::CaughtUp);
        }
        let behind_for = now.saturating_duration_since(*self.behind_since.get_or_insert(now));
        if lag <= LAG_WARN_RECORDS && behind_for <= LAG_WARN_AFTER {
            return None;
        }
        if self.last_warn.is_some_and(|t| now.saturating_duration_since(t) < LAG_REWARN_EVERY) {
            return None;
        }
        self.last_warn = Some(now);
        Some(LagEvent::Behind { lag, behind_for })
    }
}

/// The one place a [`LagEvent`] becomes a log line.
pub(super) fn log_lag(dir: &Path, event: LagEvent) {
    match event {
        LagEvent::Behind { lag, behind_for } => tracing::warn!(
            dir = %dir.display(),
            lag_records = lag,
            behind_secs = behind_for.as_secs(),
            "execution stream: the datahub has not stored this journal's executions for a while: the journal keeps \
             them and its directory GROWS (read the stream's failure lines: the datahub, its address, its key)"
        ),
        LagEvent::CaughtUp => tracing::info!(
            dir = %dir.display(),
            "execution stream: the datahub has caught up with the journal"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lag is said once past a bound, not again within [`LAG_REWARN_EVERY`], again after it, and its end once.
    #[test]
    fn a_lag_is_said_past_its_bound_again_only_after_the_rewarn_and_its_end_once() {
        let mut watch = ExecutionStreamLag::default();
        let t0 = Instant::now();
        assert_eq!(watch.observe(Some(9), Some(9), t0), None, "caught up: nothing");
        assert_eq!(watch.observe(Some(20), Some(9), t0), None, "behind, under both bounds");
        let late = t0 + LAG_WARN_AFTER + Duration::from_secs(1);
        assert_eq!(
            watch.observe(Some(20), Some(9), late),
            Some(LagEvent::Behind { lag: 11, behind_for: LAG_WARN_AFTER + Duration::from_secs(1) })
        );
        assert_eq!(
            watch.observe(Some(30), Some(9), late + Duration::from_secs(60)),
            None,
            "said already"
        );
        assert!(matches!(
            watch.observe(Some(30), Some(9), late + LAG_REWARN_EVERY),
            Some(LagEvent::Behind { lag: 21, .. })
        ));
        assert_eq!(
            watch.observe(Some(30), Some(30), late + LAG_REWARN_EVERY),
            Some(LagEvent::CaughtUp)
        );
        assert_eq!(
            watch.observe(Some(30), Some(30), late + LAG_REWARN_EVERY),
            None,
            "its end is said once"
        );
    }

    /// Past the record bound a lag is said at once, and a journal nothing was ever acknowledged of counts from its
    /// first record.
    #[test]
    fn a_lag_past_the_record_bound_is_said_at_once() {
        let mut watch = ExecutionStreamLag::default();
        let t0 = Instant::now();
        assert!(matches!(
            watch.observe(Some(LAG_WARN_RECORDS), None, t0),
            Some(LagEvent::Behind { lag, .. }) if lag == LAG_WARN_RECORDS + 1
        ));
        assert_eq!(
            ExecutionStreamLag::default().observe(None, None, t0),
            None,
            "an empty journal lags nothing"
        );
    }
}
