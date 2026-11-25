//! `StreamHealth` — one state machine for transport liveness (one `StreamStatus::GapStart` per
//! outage, `StreamStatus::Live` on recovery) and data freshness (`StreamStatus::Stale` when the
//! newest observed data ts ages past a threshold, `Live` on recovery), plus the rule between them: a
//! freshness check is suppressed while a transport gap is open, so the two halves never
//! double-signal during an outage.
//!
//! Deliberately clock-free (the caller passes `now_ms`/`ts` in, so every transition is unit-tested
//! with zero sleeps). It emits [`vike_model::StreamStatus`] — the ONE stream-health vocabulary,
//! below this crate's pure half as well as `vike-data` — so a producer that owns a `DataClientSink`
//! hands the transition straight to `DataClientSink::stream_status`, and a producer on the core tick
//! lane (the HFT tick-track pumps, which own no sink: `crate::depth::FeedTickSender::link`) pushes its
//! [`vike_model::StreamStatus::feed_status`] instead. There is no per-venue map to re-spell: the
//! field-for-field mirror this crate used to emit, and the sink-side map onto `vike-data`'s copy,
//! were retired when the enum moved below every layer that speaks it.

use vike_model::StreamStatus;

/// Per-stream health state: transport liveness fused with data freshness, plus the gating rule
/// between them (see [`Self::check_freshness`]). Clock-free: the caller passes `now_ms`/`ts` into
/// every method.
#[derive(Debug)]
pub struct StreamHealth {
    /// `Some(ts)` = currently in a transport gap that opened at `ts`; `None` = transport healthy.
    gap_started_ms: Option<i64>,
    /// newest DATA timestamp observed (venue ts, or receive-time fallback); `None` = no data yet.
    newest_timestamp: Option<i64>,
    /// session-start floor for the freshness clock when NO real data has been observed yet — set by
    /// [`Self::reset_freshness`], `None` otherwise. Lets a session that armed but then received zero
    /// data still age toward `Stale` from the arm point (the depth per-session arm-at-start); a feed
    /// that never calls `reset_freshness` (Polymarket's persist mode) leaves this `None`, so a quiet
    /// stream never trips. `newest_timestamp` takes precedence the moment any real data lands.
    armed_at: Option<i64>,
    /// the data-freshness trip threshold in ms — `now_ms - newest_timestamp > threshold_ms` is stale.
    threshold_ms: i64,
    /// `Some(now_ms at trip)` = currently disclosed stale; `None` = fresh.
    stale_since: Option<i64>,
}

impl StreamHealth {
    /// `freshness_threshold_ms` is the data-staleness trip threshold (see
    /// [`Self::check_freshness`]).
    pub fn new(freshness_threshold_ms: i64) -> Self {
        StreamHealth {
            gap_started_ms: None,
            newest_timestamp: None,
            armed_at: None,
            threshold_ms: freshness_threshold_ms,
            stale_since: None,
        }
    }

    // --- transport half ----------------------------------------------------------------------

    /// A transport gap opened at `now_ms` — an idle-watchdog trip or a transport error/close about
    /// to trigger reconnect. Returns `Some(GapStart)` the FIRST time (emit it on the sink); `None` if
    /// already in a gap, so a flapping reconnect never emits nested `GapStart`s.
    pub fn enter_gap(&mut self, now_ms: i64) -> Option<StreamStatus> {
        if self.gap_started_ms.is_none() {
            self.gap_started_ms = Some(now_ms);
            Some(StreamStatus::GapStart { at_timestamp_ms: now_ms })
        } else {
            None
        }
    }

    /// The stream recovered (re-opened + re-seeded / first frame after reconnect). Returns
    /// `Some(Live { gap_started_timestamp_ms })` and clears the gap if one was open; `None` if there was
    /// no gap (e.g. the very first successful connect), so `Live` is emitted only to CLOSE a gap.
    pub fn recover(&mut self) -> Option<StreamStatus> {
        self.gap_started_ms
            .take()
            .map(|ts| StreamStatus::Live { gap_started_timestamp_ms: Some(ts) })
    }

    /// Whether a transport gap is currently open (the feed uses this to emit `Live` only on the
    /// first frame after a reconnect, not on every frame).
    pub fn in_gap(&self) -> bool {
        self.gap_started_ms.is_some()
    }

    // --- freshness half ----------------------------------------------------------------------

    /// Record a DATA frame's timestamp. Call on EVERY applied data frame (book/quote/trade/mark) —
    /// NOT on keepalives (that's transport liveness, above). Monotonic-max so an out-of-order
    /// older frame can't drag freshness backward.
    pub fn observe_data(&mut self, ts: i64) {
        self.newest_timestamp = Some(self.newest_timestamp.map_or(ts, |n| n.max(ts)));
    }

    /// Evaluate freshness at `now_ms`. Call periodically (the transport-alive watchdog tick).
    /// Returns `Some(Stale)` the first time data ages past the threshold, `Some(Live)` when fresh
    /// data resumes, `None` otherwise (including: no data observed yet AND never armed).
    ///
    /// The reference timestamp aged against `now_ms` is `newest_timestamp` (real observed data, venue-ts,
    /// monotonic-max) when present, else the [`Self::reset_freshness`] `armed_at` session-start
    /// floor — so a session that armed but received ZERO data still ages toward `Stale` from the arm
    /// point, while an unarmed no-data stream (persist mode) never trips. Once any data lands,
    /// `newest_timestamp` supersedes `armed_at`.
    ///
    /// Recovery (`Live`) fires ONLY when the reference data is genuinely fresh again (`now_ms -
    /// ref_timestamp <= threshold`) — an update whose OWN stamp is still past the threshold keeps the
    /// episode open and never flaps Live/Stale (the depth test
    /// `a_stale_stamped_update_while_stale_does_not_falsely_recover`).
    ///
    /// **Gated:** returns `None` immediately while [`Self::in_gap`] is true — a transport gap
    /// already discloses unhealthiness, so freshness never double-signals during an outage.
    pub fn check_freshness(&mut self, now_ms: i64) -> Option<StreamStatus> {
        if self.in_gap() {
            return None;
        }
        // Real observed data (`newest_timestamp`) takes precedence; else the armed session-start floor; else
        // nothing observed and unarmed → no judgement (`None`, unchanged for the persist mode).
        let ref_timestamp = self.newest_timestamp.or(self.armed_at)?;
        let stale_now = now_ms.saturating_sub(ref_timestamp) > self.threshold_ms;
        match self.stale_since {
            None if stale_now => {
                self.stale_since = Some(now_ms);
                Some(StreamStatus::Stale { newest_data_timestamp_ms: ref_timestamp, now_ms })
            }
            Some(started) if !stale_now => {
                self.stale_since = None;
                Some(StreamStatus::Live { gap_started_timestamp_ms: Some(started) })
            }
            _ => None,
        }
    }

    /// Whether data is currently disclosed stale.
    pub fn is_stale(&self) -> bool {
        self.stale_since.is_some()
    }

    /// Clear any observed data + open stale episode (`newest_timestamp` = `None`, `stale_since` = `None`)
    /// WITHOUT emitting anything, and ARM the freshness clock at `now_ms` — so a session that then
    /// receives no data still ages toward `Stale` from this point (the depth per-session
    /// arm-at-start; see `armed_session_with_no_data_goes_stale`). Once any real data lands,
    /// `newest_timestamp` supersedes this `armed_at` floor. Callers that want freshness armed only by real
    /// data (e.g. a persist-across-reconnect feed like Polymarket) simply never call this — leaving
    /// `armed_at` `None`, so a quiet-but-connected stream never trips. Does NOT touch transport gap
    /// state (see `reset_freshness_does_not_touch_transport_gap_state`).
    pub fn reset_freshness(&mut self, now_ms: i64) {
        self.newest_timestamp = None;
        self.stale_since = None;
        self.armed_at = Some(now_ms);
    }
}

#[path = "stream_health_tests.rs"]
#[cfg(test)]
mod stream_health_tests;
