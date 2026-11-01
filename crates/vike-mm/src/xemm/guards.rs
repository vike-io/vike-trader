//! The xEMM's PURE SAFETY VERDICTS — freshness, the naked-inventory bands, and the halt vocabulary.
//!
//! A cross-exchange maker carries a risk a single-venue maker does not: between the maker fill on
//! venue A and the hedge fill on venue B it is DIRECTIONALLY EXPOSED, on a leg it cannot see through
//! any broker read. Every verdict here exists because some way of losing sight of that exposure has
//! a specific, nameable shape:
//!
//! - the REFERENCE feed silently freezes ⇒ the maker quotes off a price that no longer exists;
//! - the maker venue's OWN feed freezes ⇒ [`super::pricing::passive_clamp`] is anchored on a touch
//!   that no longer exists, so "non-marketable" stops being true;
//! - BOTH freeze ⇒ neither age bound can fire from a lane that never runs, hence the emission gap;
//! - the hedge does not fill ⇒ the exposure is real and growing;
//! - the pair decouples ⇒ one side fills forever and the other never does.
//!
//! Each is a PURE function here so it is testable without a runtime or a broker, and so the
//! strategy's fault path is one readable list rather than a chain of inline conditions.
//!
//! House style: naïve f64 folds, pure, in-file `#[cfg(test)]`.

/// WHY the maker stopped quoting. Every variant is an operator-facing fact: it names a specific
/// thing that is wrong, not a generic "error", because the correct response differs (a stale feed
/// is transient; an out-of-band basis means the mount is probably wrong; an unfilled hedge means
/// there is a real open position someone has to close by hand).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum HaltReason {
    /// The REFERENCE venue's touch is older than `max_ref_age_ms` (or has never arrived).
    ReferenceStale,
    /// The MAKER venue's own touch is older than `max_own_touch_age_ms` — the passive clamp's
    /// anchor is gone.
    OwnTouchStale,
    /// Neither venue has produced a tick within `max_emission_gap_ms`, or the maker venue's feed
    /// reported a non-`Live` status.
    FeedImpaired,
    /// The hedge residual survived its full retry budget — there is an OPEN, UNHEDGED position the
    /// taker venue has refused to close.
    HedgeUnfilled,
    /// The unhedged exposure breached `naked_hard_band`.
    NakedHardBand,
    /// The observed maker-vs-reference basis breached `max_basis_bps` — a decoupled pair, a
    /// wrong-instrument mount, or a lying feed.
    BasisOutOfBand,
    /// A fill arrived for a symbol that is NEITHER leg. The strategy cannot attribute it, so it
    /// cannot know its own inventory any more; fail closed.
    UnknownFillSymbol,
}

/// The maker's run state. A halt LATCHES with the event ts it began at, so
/// `resume_after_halt_ms` (default `0` = manual only) is measured in event time like every other
/// window in this crate.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) enum Halt {
    #[default]
    Running,
    Halted {
        reason: HaltReason,
        since: i64,
    },
}

/// `true` when `last_ts` is absent, or older than `max_age_ms` of EVENT time relative to `now`.
///
/// `max_age_ms <= 0` DISABLES the bound for a present timestamp — but an ABSENT one is still stale,
/// because "I have never seen this venue" is not a freshness question and no configuration should
/// be able to say a never-seen feed is fresh.
///
/// A NEGATIVE age (`now < last_ts`, an out-of-order or clock-skewed tick) is treated as FRESH: it
/// is evidence the feed is alive, and the alternative — halting on a single out-of-order tick —
/// would make the guard fire on healthy feeds.
pub(crate) fn stale(last_ts: Option<i64>, now: i64, max_age_ms: i64) -> bool {
    match last_ts {
        None => true,
        Some(ts) => max_age_ms > 0 && now - ts > max_age_ms,
    }
}

/// Which sides the SOFT naked band suppresses, as `(suppress_bid, suppress_ask)`.
///
/// While `|naked| > band` the side that would GROW the exposure is withheld and the side that would
/// REDUCE it keeps quoting — so the maker trades its way back toward flat instead of stopping dead.
/// Net LONG (`naked > band`) suppresses the BID; net SHORT suppresses the ASK. This is the
/// mirror-image of the fill-rate breaker: that one reacts to one-sided FLOW, this one to the
/// resulting POSITION.
///
/// A negative `band` is clamped to `0` (suppress on any exposure at all) rather than widening the
/// trigger — the same discipline `taker_flatten` applies to its band.
pub(crate) fn soft_band_suppression(naked: f64, band: f64) -> (bool, bool) {
    let band = band.max(0.0);
    if !naked.is_finite() {
        // An unknowable exposure suppresses BOTH sides: the conservative reading.
        return (true, true);
    }
    (naked > band, naked < -band)
}

/// `true` when the unhedged exposure breached the HARD band — halt, pull both sides, and fire the
/// `taker_flatten` impulse for the excess.
///
/// STRICTLY over (`>`), matching `crate::taker_flatten`'s own "exactly at the band is not over"
/// convention, so the two agree about when the impulse leg engages. `hard <= 0` disables the band
/// (there is no meaningful "halt on any exposure at all" — that is what the soft band is for), and
/// a non-finite exposure trips it (unknowable is never safe).
pub(crate) fn over_hard_band(naked: f64, hard: f64) -> bool {
    if !naked.is_finite() {
        return true;
    }
    hard > 0.0 && naked.abs() > hard
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ABSENT timestamp is stale under EVERY configuration — no knob may declare a feed the
    /// maker has never seen to be fresh.
    #[test]
    fn a_never_seen_feed_is_stale_under_every_configuration() {
        assert!(stale(None, 1_000, 2_000));
        assert!(stale(None, 1_000, 0), "even with the bound disabled");
        assert!(stale(None, 1_000, -1));
    }

    /// The bound is on EVENT age, inclusive at the boundary (exactly at the bound is still fresh).
    #[test]
    fn the_age_bound_is_event_time_and_inclusive_at_the_boundary() {
        assert!(!stale(Some(0), 2_000, 2_000), "exactly at the bound is fresh");
        assert!(stale(Some(0), 2_001, 2_000), "one ms past is stale");
        assert!(!stale(Some(0), 1_000_000, 0), "a disabled bound never ages a PRESENT ts");
    }

    /// An out-of-order tick is evidence of LIFE, not of staleness — halting on one would fire the
    /// guard on healthy feeds.
    #[test]
    fn an_out_of_order_tick_is_fresh() {
        assert!(!stale(Some(5_000), 1_000, 2_000));
    }

    /// The soft band withholds the GROWING side only, so the maker keeps trading back toward flat.
    #[test]
    fn the_soft_band_suppresses_only_the_growing_side() {
        assert_eq!(soft_band_suppression(3.0, 2.0), (true, false), "long ⇒ stop buying");
        assert_eq!(soft_band_suppression(-3.0, 2.0), (false, true), "short ⇒ stop selling");
        assert_eq!(soft_band_suppression(1.0, 2.0), (false, false), "inside the band ⇒ both quote");
        assert_eq!(soft_band_suppression(2.0, 2.0), (false, false), "exactly at the band holds");
        assert_eq!(soft_band_suppression(0.5, -1.0), (true, false), "a negative band clamps to 0");
        assert_eq!(soft_band_suppression(f64::NAN, 2.0), (true, true), "unknowable ⇒ both off");
    }

    /// The hard band is STRICTLY over and agrees with `taker_flatten`'s convention, so the halt and
    /// the impulse leg engage on the same inventories.
    #[test]
    fn the_hard_band_matches_taker_flattens_convention() {
        assert!(over_hard_band(11.0, 10.0));
        assert!(over_hard_band(-11.0, 10.0));
        assert!(!over_hard_band(10.0, 10.0), "exactly at the band holds, as taker_flatten does");
        assert_eq!(
            over_hard_band(10.0, 10.0),
            crate::taker_flatten::taker_flatten(10.0, 10.0).is_some(),
            "the two must not disagree about when the impulse leg engages"
        );
        assert!(!over_hard_band(1e9, 0.0), "a non-positive hard band is disabled");
        assert!(over_hard_band(f64::NAN, 10.0), "unknowable exposure trips it");
    }
}
