//! `StreamHealth` — the unified transport-liveness + data-freshness stream-health state machine
//! (net-hardening §B). Fuses vike-data's retired `GapTracker` (transport: one
//! [`HealthEvent::Gap`] per outage, [`HealthEvent::Live`] on recovery) and `FreshnessTracker`
//! (data: [`HealthEvent::Stale`] when the newest observed data ts ages past a threshold, `Live` on
//! recovery) into ONE type, plus the one new rule both feeds currently hand-wire: a freshness check
//! is suppressed while a transport gap is open, so the two halves never double-signal during an
//! outage.
//!
//! Deliberately clock-free (the caller passes `now_ms`/`ts` in, so every transition is unit-tested
//! with zero sleeps) and deliberately `vike-data`-free: this crate must not depend on `vike-data`,
//! so `StreamHealth` emits the neutral [`HealthEvent`] rather than `vike_data::StreamStatus`. Each
//! producer (the crypto depth feeds, Polymarket) maps `HealthEvent` onto `StreamStatus` 1:1 at its
//! own sink boundary.

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
    /// ⚠ **Why this exists beside the per-venue `health_to_stream_status` copies rather than
    /// replacing them.** A feed that owns a `LiveDataSink` maps onto `vike_data::StreamStatus` at
    /// its sink boundary, and this crate must not name `vike-data` (module doc). The HFT tick-track
    /// pumps (`crates/bridges/binance/src/family/depth.rs`'s `md_main` and its bybit/okx twins)
    /// have no sink at all: they take a bare `vike_exec::TickSender` and push
    /// `vike_model::FeedStatus` straight onto it, which is the type `vike_core`'s `CoreLaneSink`
    /// converts to anyway. So those three producers need THIS map, and they need it in one place
    /// for the same reason the sink-side one is a per-venue 1:1: a second spelling is a second
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

/// Per-stream health state: transport liveness (mirrors the retired `GapTracker`) fused with data
/// freshness (mirrors the retired `FreshnessTracker`), plus the gating rule between them (see
/// [`Self::check_freshness`]). Clock-free: the caller passes `now_ms`/`ts` into every method.
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

    // --- transport half (mirrors the retired `GapTracker`) ---------------------------------

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

    // --- freshness half (mirrors the retired `FreshnessTracker`) ---------------------------

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
    /// episode open and never falsely recovers (an intentional improvement over the old inline
    /// freshness, which flapped Live/Stale on a stale-stamped update; see the depth test
    /// `a_stale_stamped_update_while_stale_does_not_falsely_recover`).
    ///
    /// **Gated:** returns `None` immediately while [`Self::in_gap`] is true — a transport gap
    /// already discloses unhealthiness, so freshness never double-signals during an outage. This
    /// is the one rule the fusion adds on top of the two former standalone trackers.
    pub fn check_freshness(&mut self, now_ms: i64) -> Option<HealthEvent> {
        if self.in_gap() {
            return None;
        }
        // Real observed data (`newest_ts`) takes precedence; else the armed session-start floor; else
        // nothing observed and unarmed → no judgement (`None`, unchanged for the persist mode).
        let ref_ts = self.newest_ts.or(self.armed_at)?;
        let stale_now = now_ms - ref_ts > self.threshold_ms;
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- transport half (ported from vike-data/src/gap.rs's `mod tests`, GapTracker) -------

    #[test]
    fn enter_gap_emits_once_then_guards() {
        let mut h = StreamHealth::new(1000);
        assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));
        assert!(h.in_gap());
        // a flapping reconnect calls enter_gap again — no nested Gap
        assert_eq!(h.enter_gap(150), None);
        assert_eq!(h.enter_gap(200), None);
    }

    #[test]
    fn recover_closes_the_gap_with_the_matching_start() {
        let mut h = StreamHealth::new(1000);
        h.enter_gap(100);
        assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
        assert!(!h.in_gap());
        // recover again with no open gap → nothing (Live only closes a gap)
        assert_eq!(h.recover(), None);
    }

    #[test]
    fn recover_without_a_gap_is_none() {
        // first successful connect: no prior gap → no Live
        let mut h = StreamHealth::new(1000);
        assert_eq!(h.recover(), None);
    }

    #[test]
    fn second_outage_gets_a_fresh_gap() {
        let mut h = StreamHealth::new(1000);
        h.enter_gap(100);
        h.recover();
        // a NEW outage later → a fresh Gap at the new ts (not guarded by the first)
        assert_eq!(h.enter_gap(500), Some(HealthEvent::Gap { at_ts_ms: 500 }));
        assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(500) }));
    }

    // --- freshness half (ported from vike-data/src/gap.rs's `mod freshness_tests`,
    // FreshnessTracker) --------------------------------------------------------------------

    // threshold 1000ms. Data ts advances with now → never stale.
    #[test]
    fn advancing_data_never_trips() {
        let mut h = StreamHealth::new(1000);
        for t in [0i64, 500, 1000, 1500, 2000] {
            h.observe_data(t);
            assert_eq!(h.check_freshness(t), None, "fresh data at t={t} must not trip");
        }
        assert!(!h.is_stale());
    }

    // transport alive (check keeps being called) but data ts frozen past threshold → exactly one
    // Stale.
    #[test]
    fn frozen_data_trips_once_then_recovers_once() {
        let mut h = StreamHealth::new(1000);
        h.observe_data(0); // newest data ts = 0
        assert_eq!(h.check_freshness(500), None); // lag 500 <= 1000, fresh
        // data freezes; wall clock advances past threshold
        assert_eq!(
            h.check_freshness(1500),
            Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1500 }),
            "lag 1500 > 1000 → Stale"
        );
        assert!(h.is_stale());
        // still frozen, later checks do NOT re-emit
        assert_eq!(h.check_freshness(2000), None);
        assert_eq!(h.check_freshness(3000), None);
        // fresh data resumes
        h.observe_data(3100);
        assert_eq!(
            h.check_freshness(3100),
            Some(HealthEvent::Live { gap_started_ts_ms: Some(1500) }),
            "data fresh again → one Live closing the episode at the trip ts"
        );
        assert!(!h.is_stale());
        assert_eq!(h.check_freshness(3200), None); // no double recovery
    }

    // a sparse-but-CURRENT market: data arrives infrequently but each frame's ts is recent → no
    // trip.
    #[test]
    fn sparse_but_current_does_not_trip() {
        let mut h = StreamHealth::new(1000);
        // one frame every 800ms (< threshold), ts == arrival — always fresh
        for t in [0i64, 800, 1600, 2400, 3200] {
            h.observe_data(t);
            assert_eq!(h.check_freshness(t), None);
            assert_eq!(
                h.check_freshness(t + 700),
                None,
                "still within threshold before the next frame"
            );
        }
        assert!(!h.is_stale());
    }

    // no data observed yet AND never armed (a fresh `new()`, `reset_freshness` never called) →
    // nothing to judge, never trips on an unstarted stream. This is exactly Polymarket's
    // persist-across-reconnect mode: it never calls `reset_freshness`, so `armed_at` stays `None`
    // and a quiet-but-connected stream is never falsely declared stale — only real observed data
    // arms the freshness clock there.
    #[test]
    fn no_data_yet_never_trips() {
        let mut h = StreamHealth::new(1000);
        assert_eq!(h.check_freshness(999_999), None);
        assert!(!h.is_stale());
    }

    // The boundary is strict `>`: lag == threshold is NOT yet stale; lag == threshold+1 is.
    #[test]
    fn check_at_exact_threshold_is_not_yet_stale() {
        let mut h = StreamHealth::new(1000);
        h.observe_data(0);
        assert_eq!(h.check_freshness(1000), None, "lag == threshold is fresh (strict >)");
        assert_eq!(
            h.check_freshness(1001),
            Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1001 }),
            "one ms past threshold → Stale"
        );
    }

    // observe_data() is monotonic-max: an out-of-order OLDER frame must not drag newest_ts
    // backward.
    #[test]
    fn observe_ignores_out_of_order_older_frames() {
        let mut h = StreamHealth::new(1000);
        h.observe_data(2000);
        h.observe_data(500); // older, out of order — must be ignored
        // freshness is judged from 2000, not 500:
        assert_eq!(h.check_freshness(2500), None, "lag from newest (2000) is 500 <= 1000 → fresh");
        assert_eq!(
            h.check_freshness(3001),
            Some(HealthEvent::Stale { newest_data_ts_ms: 2000, now_ms: 3001 }),
            "newest_ts stayed at 2000, never regressed to 500"
        );
    }

    // --- NEW: gating + reset (the fusion's one new rule) ------------------------------------

    #[test]
    fn freshness_check_is_suppressed_while_in_a_transport_gap() {
        let mut h = StreamHealth::new(1000);
        assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));

        // data is badly stale, but a transport gap is open — check_freshness must stay silent
        h.observe_data(0);
        assert_eq!(h.check_freshness(1_000_000), None, "gated while in_gap()");
        assert!(!h.is_stale(), "gating must not mark stale internally either");

        assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
        // gate lifted — freshness resumes normal operation and trips as expected
        assert_eq!(
            h.check_freshness(1_000_000),
            Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1_000_000 }),
            "after recover(), check_freshness works again"
        );
        assert!(h.is_stale());
    }

    #[test]
    fn reset_freshness_clears_an_open_stale_episode() {
        let mut h = StreamHealth::new(1000);
        h.observe_data(0);
        assert_eq!(
            h.check_freshness(1500),
            Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1500 })
        );
        assert!(h.is_stale());

        // reset clears the open stale episode (and re-arms the clock at now = 1500) WITHOUT emitting.
        h.reset_freshness(1500);
        assert!(!h.is_stale(), "reset clears the open stale episode without emitting anything");

        // Checked at / near the 1500 arm floor → within threshold, so no immediate re-trip and — the
        // key guard — no spurious `Live` (the cleared `stale_since` cannot recover): stays silent.
        assert_eq!(h.check_freshness(1500), None);
        assert_eq!(h.check_freshness(2000), None, "age 500 from the 1500 arm floor is still fresh");
        assert!(!h.is_stale());
    }

    #[test]
    fn reset_freshness_does_not_touch_transport_gap_state() {
        let mut h = StreamHealth::new(1000);
        assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));
        h.reset_freshness(100);
        assert!(h.in_gap(), "reset_freshness must not clear transport gap state");
        // the gap can still be closed normally afterward
        assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
    }

    // A session armed at start (the depth per-session `reset_freshness(now)`) that then receives
    // ZERO data still ages toward `Stale` from the arm floor — the §B capability the unified
    // `StreamHealth` must preserve (a subscribe that silently succeeds but sends nothing behind a
    // live socket). Without the `armed_at` floor a dataless session would trip NOTHING.
    #[test]
    fn armed_session_with_no_data_goes_stale() {
        let mut h = StreamHealth::new(1000);
        h.reset_freshness(0); // armed at session start; observe_data is NEVER called below
        assert_eq!(h.check_freshness(500), None, "within threshold of the arm floor → still fresh");
        assert_eq!(
            h.check_freshness(1001),
            Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1001 }),
            "past threshold with no data → the armed floor trips Stale from the session start"
        );
        assert!(h.is_stale());
    }

    // Once real data lands, `newest_ts` supersedes the `armed_at` floor: freshness ages from the
    // DATA ts, not the (older) arm time — so an armed session that DOES receive data is judged
    // exactly as it would be without the floor (preserving the #118 stale-stamped-first-frame path,
    // which prefers the real data ts via `newest_ts.or(armed_at)`).
    #[test]
    fn armed_then_real_data_uses_the_data_ts() {
        let mut h = StreamHealth::new(1000);
        h.reset_freshness(0); // armed at 0…
        h.observe_data(50_000); // …but real data at 50_000 supersedes the floor
        assert_eq!(
            h.check_freshness(50_500),
            None,
            "ages from the data ts (50_000), not the arm floor (0) — else 50_500 would trip"
        );
        assert!(!h.is_stale());
    }

    // --- the tick-lane map + the transport-only constructor ---------------------------------

    /// The 1:1 `HealthEvent` -> `FeedStatus` map, pinned in all three directions. ⚠ The `Gap` row
    /// is the load-bearing one: `Disconnected` is the ONE status that opens the connection-state
    /// dead-man's grace window, so mapping a gap to anything else would leave the tick-track pumps
    /// disclosing a link death the switch cannot act on — the exact failure this map exists to end.
    #[test]
    fn a_health_event_maps_onto_the_feed_status_the_core_acts_on() {
        use vike_model::FeedStatus;
        assert_eq!(HealthEvent::Gap { at_ts_ms: 7 }.feed_status(), FeedStatus::Disconnected);
        assert_eq!(
            HealthEvent::Live { gap_started_ts_ms: Some(7) }.feed_status(),
            FeedStatus::Live
        );
        assert_eq!(
            HealthEvent::Stale { newest_data_ts_ms: 1, now_ms: 9 }.feed_status(),
            FeedStatus::Stale,
        );
    }

    /// [`StreamHealth::transport_only`] is the SAME transport machine — one gap per outage, `Live`
    /// only to close one — and its freshness half can never speak, however far the clock is
    /// advanced. Both halves asserted: a constructor that silently disabled the gap tracking too
    /// would pass a test that only checked the second.
    #[test]
    fn transport_only_keeps_the_gap_half_and_can_never_go_stale() {
        let mut h = StreamHealth::transport_only();
        assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));
        assert_eq!(h.enter_gap(200), None, "still one gap per outage");
        assert!(h.in_gap());
        assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
        assert_eq!(h.recover(), None, "Live still only ever CLOSES a gap");

        // …and the freshness half is inert even if somebody calls it: armed at 0, judged a century
        // later, with the threshold at i64::MAX.
        h.reset_freshness(0);
        assert_eq!(h.check_freshness(3_155_760_000_000), None, "no threshold can trip");
        assert!(!h.is_stale());
    }
}
