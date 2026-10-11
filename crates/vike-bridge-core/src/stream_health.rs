//! `StreamHealth` — one state machine for transport liveness (one [`HealthEvent::Gap`] per outage,
//! [`HealthEvent::Live`] on recovery) and data freshness ([`HealthEvent::Stale`] when the newest
//! observed data ts ages past a threshold, `Live` on recovery), plus the rule between them: a
//! freshness check is suppressed while a transport gap is open, so the two halves never
//! double-signal during an outage.
//!
//! Deliberately clock-free (the caller passes `now_ms`/`ts` in, so every transition is unit-tested
//! with zero sleeps) and deliberately `vike-data`-free in its state machine: the pure half of this
//! crate (`--no-default-features`) carries no `vike-data`, so `StreamHealth` emits the neutral
//! [`HealthEvent`] rather than `vike_data::StreamStatus`. Each producer that owns a
//! `LiveDataSink` (the crypto depth feeds, the bar/quote feeds, Polymarket, OANDA) maps
//! `HealthEvent` onto `StreamStatus` 1:1 at its own sink boundary through
//! `health_to_stream_status` — the ONE map, behind `full` where `vike-data` is already a
//! dependency (the venue mount contract names it). Never re-spell it per venue.

/// A stream-health transition [`StreamHealth`] asks its caller to disclose. Mirrors
/// `vike_data::StreamStatus`'s three cases field-for-field (`GapStart` -> `Gap`, everything else
/// name-for-name); a producer maps this 1:1 onto `StreamStatus` at its `LiveDataSink` boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthEvent {
    /// The stream can no longer be trusted from `at_ts_ms`: transport error, server close, or
    /// idle-watchdog trip.
    Gap { at_ts_ms: i64 },
    /// The stream is live again, closing either a transport `Gap` or a data `Stale` episode.
    /// `gap_started_ts_ms` echoes whichever episode's start/trip time it closes.
    Live { gap_started_ts_ms: Option<i64> },
    /// The transport is alive but no fresh DATA has arrived: `now_ms - newest_data_ts_ms` exceeded
    /// the freshness threshold. Recovery is the shared `Live`.
    Stale { newest_data_ts_ms: i64, now_ms: i64 },
}

impl HealthEvent {
    /// The [`vike_model::FeedStatus`] this transition discloses — the 1:1 map for a producer that
    /// reaches the core through the **tick lane** (`vike_exec::StreamStatusUpdate`) instead of
    /// through a `LiveDataSink`.
    ///
    /// ⚠ **Why this exists beside `health_to_stream_status` rather than replacing it.** A feed
    /// that owns a `LiveDataSink` maps onto `vike_data::StreamStatus` at its sink boundary through
    /// that fn, which is `full`-gated because it names `vike-data` (module doc); this method is
    /// not, because the pure half of the crate carries no `vike-data`. The HFT tick-track
    /// pumps (`crates/bridges/binance/src/family/depth.rs`'s `md_main` and its bybit/okx twins)
    /// have no sink at all: they take a bare `vike_exec::TickSender` and push
    /// `vike_model::FeedStatus` straight onto it, which is the type `vike_core`'s `CoreLaneSink`
    /// converts to anyway. So those three producers need THIS map, and they need it in one place
    /// for the same reason the sink-side one is a single fn: a second spelling is a second
    /// answer to "is a `Gap` a `Disconnected`".
    ///
    /// `Gap` -> `Disconnected` and `Stale` -> `Stale` are the two that matter to the CONNECTION-state
    /// dead-man (`vike_core::LinkDeadManConfig`): the first opens its grace window, the second moves
    /// nothing at all, which is the whole content of decision 0038.
    #[must_use]
    pub const fn feed_status(self) -> vike_model::FeedStatus {
        match self {
            HealthEvent::Gap { .. } => vike_model::FeedStatus::Disconnected,
            HealthEvent::Live { .. } => vike_model::FeedStatus::Live,
            HealthEvent::Stale { .. } => vike_model::FeedStatus::Stale,
        }
    }
}

/// Map the driver-neutral [`HealthEvent`] onto the `vike_data::StreamStatus` disclosure vocabulary
/// (1:1: `Gap` -> `GapStart`, `Live` and `Stale` name-for-name, every field carried verbatim). A pure
/// translation at the `LiveDataSink` boundary: the [`StreamHealth`] the feed's driver owns across
/// reconnects already dedups transport gaps and gates freshness during an open gap, so nothing is
/// decided here.
///
/// Used by every venue feed that discloses through a `LiveDataSink` — the binance/aster family's
/// DOM depth lane, bybit's and okx's depth lanes, polymarket's pump and oanda's quote/bar lanes.
/// `full`-gated because it names `vike_data::StreamStatus` (the module doc argues why the state
/// machine itself does not).
#[cfg(feature = "full")]
#[must_use]
pub fn health_to_stream_status(ev: HealthEvent) -> vike_data::StreamStatus {
    use vike_data::StreamStatus;
    match ev {
        HealthEvent::Gap { at_ts_ms } => StreamStatus::GapStart { at_ts_ms },
        HealthEvent::Live { gap_started_ts_ms } => StreamStatus::Live { gap_started_ts_ms },
        HealthEvent::Stale { newest_data_ts_ms, now_ms } => {
            StreamStatus::Stale { newest_data_ts_ms, now_ms }
        }
    }
}

/// Per-stream health state: transport liveness fused with data freshness, plus the gating rule
/// between them (see [`Self::check_freshness`]). Clock-free: the caller passes `now_ms`/`ts` into
/// every method.
#[derive(Debug)]
pub struct StreamHealth {
    /// `Some(ts)` = currently in a transport gap that opened at `ts`; `None` = transport healthy.
    gap_started_ms: Option<i64>,
    /// newest DATA timestamp observed (venue ts, or receive-time fallback); `None` = no data yet.
    newest_ts: Option<i64>,
    /// session-start floor for the freshness clock when NO real data has been observed yet — set by
    /// [`Self::reset_freshness`], `None` otherwise. Lets a session that armed but then received zero
    /// data still age toward `Stale` from the arm point (the depth per-session arm-at-start); a feed
    /// that never calls `reset_freshness` (Polymarket's persist mode) leaves this `None`, so a quiet
    /// stream never trips. `newest_ts` takes precedence the moment any real data lands.
    armed_at: Option<i64>,
    /// the data-freshness trip threshold in ms — `now_ms - newest_ts > threshold_ms` is stale.
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
            newest_ts: None,
            armed_at: None,
            threshold_ms: freshness_threshold_ms,
            stale_since: None,
        }
    }

    /// For a producer that uses the TRANSPORT half only — [`Self::enter_gap`] / [`Self::recover`] /
    /// [`Self::in_gap`] — and never calls [`Self::check_freshness`] or [`Self::reset_freshness`].
    ///
    /// It exists so such a caller does not have to invent a freshness threshold that means nothing:
    /// a number written there would read as a measurement and would be the first thing a later
    /// reader "fixed". The threshold is [`i64::MAX`], so even a mistaken `check_freshness` call can
    /// only ever answer "fresh" — a wrong threshold cannot become a spurious `Stale`.
    ///
    /// The callers today are the three HFT tick-track market-data pumps
    /// (`crates/bridges/binance/src/family/depth.rs`'s `md_main` and its bybit/okx twins), which
    /// run NO freshness watchdog: their read loop treats a socket timeout as a stop-poll tick and
    /// nothing on them judges data age. `Stale` would be inert to the connection-state dead-man in
    /// any case (decision 0038), so this is a disclosure they must not invent rather than one they
    /// merely skip.
    pub fn transport_only() -> Self {
        StreamHealth::new(i64::MAX)
    }

    // --- transport half ----------------------------------------------------------------------

    /// A transport gap opened at `now_ms` — an idle-watchdog trip or a transport error/close about
    /// to trigger reconnect. Returns `Some(Gap)` the FIRST time (emit it on the sink); `None` if
    /// already in a gap, so a flapping reconnect never emits nested `Gap`s.
    pub fn enter_gap(&mut self, now_ms: i64) -> Option<HealthEvent> {
        if self.gap_started_ms.is_none() {
            self.gap_started_ms = Some(now_ms);
            Some(HealthEvent::Gap { at_ts_ms: now_ms })
        } else {
            None
        }
    }

    /// The stream recovered (re-opened + re-seeded / first frame after reconnect). Returns
    /// `Some(Live { gap_started_ts_ms })` and clears the gap if one was open; `None` if there was
    /// no gap (e.g. the very first successful connect), so `Live` is emitted only to CLOSE a gap.
    pub fn recover(&mut self) -> Option<HealthEvent> {
        self.gap_started_ms.take().map(|ts| HealthEvent::Live { gap_started_ts_ms: Some(ts) })
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
        self.newest_ts = Some(self.newest_ts.map_or(ts, |n| n.max(ts)));
    }

    /// Evaluate freshness at `now_ms`. Call periodically (the transport-alive watchdog tick).
    /// Returns `Some(Stale)` the first time data ages past the threshold, `Some(Live)` when fresh
    /// data resumes, `None` otherwise (including: no data observed yet AND never armed).
    ///
    /// The reference timestamp aged against `now_ms` is `newest_ts` (real observed data, venue-ts,
    /// monotonic-max) when present, else the [`Self::reset_freshness`] `armed_at` session-start
    /// floor — so a session that armed but received ZERO data still ages toward `Stale` from the arm
    /// point, while an unarmed no-data stream (persist mode) never trips. Once any data lands,
    /// `newest_ts` supersedes `armed_at`.
    ///
    /// Recovery (`Live`) fires ONLY when the reference data is genuinely fresh again (`now_ms -
    /// ref_ts <= threshold`) — an update whose OWN stamp is still past the threshold keeps the
    /// episode open and never flaps Live/Stale (the depth test
    /// `a_stale_stamped_update_while_stale_does_not_falsely_recover`).
    ///
    /// **Gated:** returns `None` immediately while [`Self::in_gap`] is true — a transport gap
    /// already discloses unhealthiness, so freshness never double-signals during an outage.
    pub fn check_freshness(&mut self, now_ms: i64) -> Option<HealthEvent> {
        if self.in_gap() {
            return None;
        }
        // Real observed data (`newest_ts`) takes precedence; else the armed session-start floor; else
        // nothing observed and unarmed → no judgement (`None`, unchanged for the persist mode).
        let ref_ts = self.newest_ts.or(self.armed_at)?;
        let stale_now = now_ms.saturating_sub(ref_ts) > self.threshold_ms;
        match self.stale_since {
            None if stale_now => {
                self.stale_since = Some(now_ms);
                Some(HealthEvent::Stale { newest_data_ts_ms: ref_ts, now_ms })
            }
            Some(started) if !stale_now => {
                self.stale_since = None;
                Some(HealthEvent::Live { gap_started_ts_ms: Some(started) })
            }
            _ => None,
        }
    }

    /// Whether data is currently disclosed stale.
    pub fn is_stale(&self) -> bool {
        self.stale_since.is_some()
    }

    /// Clear any observed data + open stale episode (`newest_ts` = `None`, `stale_since` = `None`)
    /// WITHOUT emitting anything, and ARM the freshness clock at `now_ms` — so a session that then
    /// receives no data still ages toward `Stale` from this point (the depth per-session
    /// arm-at-start; see `armed_session_with_no_data_goes_stale`). Once any real data lands,
    /// `newest_ts` supersedes this `armed_at` floor. Callers that want freshness armed only by real
    /// data (e.g. a persist-across-reconnect feed like Polymarket) simply never call this — leaving
    /// `armed_at` `None`, so a quiet-but-connected stream never trips. Does NOT touch transport gap
    /// state (see `reset_freshness_does_not_touch_transport_gap_state`).
    pub fn reset_freshness(&mut self, now_ms: i64) {
        self.newest_ts = None;
        self.stale_since = None;
        self.armed_at = Some(now_ms);
    }
}

#[path = "stream_health_tests.rs"]
#[cfg(test)]
mod stream_health_tests;
