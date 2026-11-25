//! The bounded failure line of a window that will not land: the retired journal materializer's PassFailed /
//! Recovered half, now about a window. A flush that keeps failing at one stage is logged on its first failure, when
//! the failing stage changes, and otherwise at most every [`FAILURE_REWARN_EVERY`] with the count it did not log;
//! the first flush that lands again says so once. Pure over its inputs (the clock is a parameter).

use std::time::{Duration, Instant};

use super::limits::FAILURE_REWARN_EVERY;

/// Where a flush stopped: what the bounded line compares, never the error TEXT (an error that embeds a number would
/// be "new" on every retry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlushStage {
    /// Writing the cursor row of `vike.db` (`writing_through`, `stored_through`, a gap).
    Cursor,
    /// Appending a series of the window to the store.
    Append,
}

impl FlushStage {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            FlushStage::Cursor => "cursor",
            FlushStage::Append => "append",
        }
    }
}

/// What one call has to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureEvent {
    /// A flush failed at `stage`, and `unlogged` failures since the previous line were counted, not logged.
    Failed { stage: FlushStage, unlogged: u64 },
    /// The first flush that landed after `failed` failing ones over `failing_for`.
    Recovered { failed: u64, failing_for: Duration },
}

#[derive(Debug, Default)]
pub(super) struct FlushFailures {
    failing_since: Option<Instant>,
    failed: u64,
    last_line: Option<(FlushStage, Instant)>,
    unlogged: u64,
}

impl FlushFailures {
    /// A flush failed at `stage`: the line is due on the first failure, when the stage changes, and otherwise once
    /// per [`FAILURE_REWARN_EVERY`].
    pub(super) fn failed(&mut self, stage: FlushStage, now: Instant) -> Option<FailureEvent> {
        self.failing_since.get_or_insert(now);
        self.failed = self.failed.saturating_add(1);
        let due = self.last_line.is_none_or(|(last, at)| {
            last != stage || now.saturating_duration_since(at) >= FAILURE_REWARN_EVERY
        });
        if !due {
            self.unlogged = self.unlogged.saturating_add(1);
            return None;
        }
        self.last_line = Some((stage, now));
        Some(FailureEvent::Failed { stage, unlogged: std::mem::take(&mut self.unlogged) })
    }

    /// A flush landed: the recovery line, once, if flushes had been failing.
    pub(super) fn landed(&mut self, now: Instant) -> Option<FailureEvent> {
        let since = self.failing_since.take()?;
        self.last_line = None;
        self.unlogged = 0;
        Some(FailureEvent::Recovered {
            failed: std::mem::take(&mut self.failed),
            failing_for: now.saturating_duration_since(since),
        })
    }
}

/// The one place a [`FailureEvent`] becomes a log line.
pub(super) fn log(journal_id: &str, event: FailureEvent, error: &str) {
    match event {
        FailureEvent::Failed { stage, unlogged } => tracing::warn!(
            journal_id,
            stage = stage.as_str(),
            error,
            unlogged_failures = unlogged,
            "vike-datahub: an execution-stream window could not be stored; the SAME window is retried, and the \
             tradehub keeps every record it holds past the datahub's cursor (repeated only when the failing stage \
             changes or once per FAILURE_REWARN_EVERY; unlogged_failures counts the retries between)"
        ),
        FailureEvent::Recovered { failed, failing_for } => tracing::info!(
            journal_id,
            failed_retries = failed,
            failing_secs = failing_for.as_secs(),
            "vike-datahub: execution-stream windows are stored again"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first failure is logged, a repeat at the same stage is counted until the rewarn bound, a new stage is
    /// logged at once with the count, and the first landing says so once.
    #[test]
    fn a_failing_flush_is_logged_bounded_and_its_recovery_once() {
        let t0 = Instant::now();
        let mut f = FlushFailures::default();
        assert_eq!(
            f.failed(FlushStage::Append, t0),
            Some(FailureEvent::Failed { stage: FlushStage::Append, unlogged: 0 })
        );
        assert_eq!(f.failed(FlushStage::Append, t0 + Duration::from_secs(1)), None);
        assert_eq!(f.failed(FlushStage::Append, t0 + Duration::from_secs(2)), None);
        assert_eq!(
            f.failed(FlushStage::Cursor, t0 + Duration::from_secs(3)),
            Some(FailureEvent::Failed { stage: FlushStage::Cursor, unlogged: 2 })
        );
        assert_eq!(
            f.failed(FlushStage::Cursor, t0 + Duration::from_secs(3) + FAILURE_REWARN_EVERY),
            Some(FailureEvent::Failed { stage: FlushStage::Cursor, unlogged: 0 })
        );
        let back = t0 + Duration::from_secs(10) + FAILURE_REWARN_EVERY;
        assert_eq!(
            f.landed(back),
            Some(FailureEvent::Recovered { failed: 5, failing_for: back - t0 })
        );
        assert_eq!(f.landed(back), None, "said once");
    }
}
