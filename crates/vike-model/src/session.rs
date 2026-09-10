//! `SessionCalendar` — the ONE market-hours law (law-map T10; LEAN `SecurityExchangeHours` analog).
//!
//! Before this module nothing in the workspace could answer *"is this venue open at `ts`?"*. The FX
//! week existed as a prose comment in one venue's live-smoke test; live execution discovered a
//! closed market only as a venue REJECTION, and backtests discovered it only as a data GAP. Two
//! different discovery mechanisms for one fact is exactly the divergence class the law-mapping
//! audit exists to kill.
//!
//! ## Policy as DATA, one query
//!
//! The shape is [`crate::venue_caps`]'s: a declarative table plus one pure query, so the backtest
//! fill gate, a future live gate, and the GUI all read the SAME rows. There is deliberately no
//! second session codepath anywhere — a consumer either calls [`SessionCalendar::is_open`] or it is
//! not consulting the law.
//!
//! ## Keyed by (venue, ASSET CLASS) — resolved by the CALLER, never by a model→catalog edge
//!
//! The correct key for a session is **(venue, asset class)**, not the venue alone. Venue-alone
//! keying is a real defect: Alpaca trades cash equities AND 24/7 crypto on one venue, and IBKR is a
//! global multi-asset book — so "what venue is this?" cannot answer "when is it open?". The fill
//! must resolve per SYMBOL.
//!
//! Asset class is a `vike-catalog` concept ([`AssetClass`]), and **`vike-catalog` depends on
//! `vike-model` (down only)** — so this crate must NOT depend on `vike-catalog`, and the
//! `(venue, asset_class) -> SessionCalendar` map cannot live here without inverting that layering.
//! The resolution therefore lives **where both venue and asset class are visible** — in
//! `vike-catalog` (`vike_catalog::session_calendar_for`) or in the consumer — and this module takes
//! the resolved thing as a **caller-supplied parameter**:
//!
//! - the pure query works on a [`SessionCalendar`] the caller hands it (backtest stores one PER
//!   SYMBOL; see `EngineParams::session_calendars`);
//! - [`session_for`] is only a **venue-ONLY convenience default** — see its doc for why it is
//!   deliberately fail-permissive for the mixed-asset venues rather than guessing their class.
//!
//! [`AssetClass`]: https://docs.rs/vike-catalog (the taxonomy lives one layer up, in vike-catalog)
//!
//! ## The time model: week-minutes, no timezone database
//!
//! A calendar is a set of half-open [`SessionSegment`]s over the trading week, measured in
//! **minutes since Monday 00:00 UTC** (`0 .. 10_080`). Timestamps are epoch-ms UTC, the workspace
//! convention, and the only calendar arithmetic is the day division this crate's
//! [`crate::time`] module already owns ([`crate::time::utc_weekday`]) — no `chrono`, no tz
//! database, no hand-rolled civil math.
//!
//! Resolution is ONE MINUTE. Every boundary in every shipped row falls on a minute, so this costs
//! nothing today; a venue needing sub-minute precision would need a wider type, not a workaround.
//!
//! ## The DST rule (read this before adding a row)
//!
//! Several real session boundaries move with daylight-saving time, and without a tz database we
//! cannot know which side of a transition a `ts` falls on. **Where a boundary moves, the declared
//! segment is the UNION of both offsets.** The consequence is stated once and holds for the whole
//! table:
//!
//! > The gate may UNDER-block by up to one hour. It can never OVER-block.
//!
//! That asymmetry is the safe one for a fill gate. Under-blocking leaves an hour of genuinely
//! closed time looking open — the pre-existing behavior, since today nothing blocks at all.
//! Over-blocking would silently DELETE fills that really happened in-session and quietly corrupt a
//! backtest's results. A tz database (LEAN ships a 3.2 MB market-hours JSON for exactly this) is the
//! deferred follow-up that would narrow these rows; see "Deliberately not built" below.
//!
//! ## Fail-PERMISSIVE, unlike `venue_caps`
//!
//! Every resolver here answers [`SessionCalendar::ALWAYS_OPEN`] when it cannot pin a session —
//! [`session_for`] for an unknown venue, and (up one layer) `vike_catalog::session_calendar_for`
//! for an unmodeled (venue, asset-class) pair. That is the opposite of `venue_caps`'s fail-CLOSED
//! [`crate::venue_caps::VenueCaps::UNSUPPORTED`]. The two defaults differ because the two questions
//! differ: a capability we cannot confirm must not be OFFERED, but a session we have no data for
//! must not silently BLOCK a venue that in reality trades fine. A table that halts a venue it simply
//! has no row for is worse than no table. This also drives the DST-union direction above — the whole
//! law only ever under-blocks, never over-blocks.
//!
//! ## Deliberately not built (follow-ups, not oversights)
//!
//! - **Holiday calendars / half-days.** LEAN's per-market holiday + early-close database is the
//!   thing we are explicitly not building yet. Every row here is a plain recurring week: a US
//!   holiday reads as OPEN.
//! - **A tz-database `SessionCalendar`.** The rows here are DST-union approximations (see the DST
//!   rule). Narrowing them to the exact civil-time session per date is the LEAN market-hours-DB
//!   follow-up — it lives above this crate (it needs a tz database), not in the pure model.
//! - **Non-US regional equity sessions and listed-derivative hours.** Only the US regular session
//!   is modeled ([`US_EQUITY_REGULAR`]); LSE/TSE/etc. and CME/CBOE extended hours are resolved
//!   fail-permissive by `vike_catalog::session_calendar_for` until their rows land here.
//! - **Pre/post-market.** [`SessionState`] has no `PreOpen`/`PostClose` variant because no shipped
//!   row carries extended-hours data to distinguish them with. Adding the variant before the data
//!   exists would be a field that is never set.
//! - **Session-aware bar labeling** (LEAN's close-labeled daily bars) is a separate concern.

use crate::time::utc_weekday;

/// Minutes in a day.
pub const DAY_MINUTES: i64 = 1_440;
/// Minutes in a trading week — the modulus every [`SessionSegment`] lives under.
pub const WEEK_MINUTES: i64 = 7 * DAY_MINUTES;

/// Epoch-ms (UTC) → minutes since **Monday 00:00 UTC**, in `0 .. WEEK_MINUTES`. Total over the
/// whole `i64` range (both divisions are Euclidean, so pre-1970 timestamps floor toward -inf and
/// land in the same buckets as their positive counterparts).
#[inline]
pub fn week_minute(ts_ms: i64) -> i64 {
    let minute_of_day = ts_ms.div_euclid(60_000).rem_euclid(DAY_MINUTES);
    utc_weekday(ts_ms) as i64 * DAY_MINUTES + minute_of_day
}

/// One half-open `[start_min, end_min)` stretch of the trading week, in week-minutes
/// ([`week_minute`]). Segments never wrap: a session spanning Sunday into Monday is declared as TWO
/// segments (see [`FX_WEEK`]), which keeps every query a plain ordered scan with no modular
/// interval arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSegment {
    /// inclusive start, week-minutes
    pub start_min: i64,
    /// exclusive end, week-minutes
    pub end_min: i64,
}

impl SessionSegment {
    /// A segment from `start_min` (inclusive) to `end_min` (exclusive), both week-minutes.
    pub const fn new(start_min: i64, end_min: i64) -> Self {
        SessionSegment { start_min, end_min }
    }

    /// Does this segment cover `week_min`? Half-open, so a segment ending at `end_min` and one
    /// starting at `end_min` are contiguous with no covered-twice minute between them.
    #[inline]
    pub const fn covers(&self, week_min: i64) -> bool {
        week_min >= self.start_min && week_min < self.end_min
    }
}

/// Whether a venue is trading. Two states only — see the module doc's "Deliberately not built" on
/// why there is no `PreOpen`/`PostClose`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The venue accepts and matches orders.
    Open,
    /// The venue is outside its trading session.
    Closed,
}

/// The declared weekly trading session for ONE venue class. `Copy` (a `&'static str` plus a
/// `&'static [_]`) so it is free to hand to a UI frame by value, like [`crate::venue_caps::VenueCaps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCalendar {
    /// The IANA zone the venue itself QUOTES its session in — documentation and UI labeling only.
    /// It is deliberately NOT consulted by any query: `segments` are already resolved to UTC
    /// week-minutes under the module's DST rule. Recording it keeps the provenance of each row
    /// legible and is what a future tz-database implementation would key on.
    pub tz: &'static str,
    /// Ascending, non-overlapping half-open segments in week-minutes. Queries scan them in order.
    pub segments: &'static [SessionSegment],
}

impl SessionCalendar {
    /// Open every minute of every day — the 24/7 calendar AND the fail-permissive fallback for a
    /// venue [`session_for`] has no row for (see the module doc).
    pub const ALWAYS_OPEN: SessionCalendar =
        SessionCalendar { tz: "UTC", segments: &[SessionSegment::new(0, WEEK_MINUTES)] };

    /// Is this venue trading at `ts_ms` (epoch-ms UTC)? The ONE query every consumer calls; O(number
    /// of segments), which is ≤ 5 for every shipped row, so it is cheap enough for a fill path.
    #[inline]
    pub fn is_open(&self, ts_ms: i64) -> bool {
        let w = week_minute(ts_ms);
        self.segments.iter().any(|s| s.covers(w))
    }

    /// [`SessionState`] form of [`Self::is_open`].
    #[inline]
    pub fn state(&self, ts_ms: i64) -> SessionState {
        if self.is_open(ts_ms) { SessionState::Open } else { SessionState::Closed }
    }

    /// The earliest instant `>= ts_ms` at which this venue is open, or `None` when it never opens
    /// (a calendar with no segments). **`Some(ts_ms)` when already open** — the query is "from when
    /// may I trade?", so an open venue answers "now" rather than skipping to the next week's open.
    ///
    /// Minute-aligned when it moves: the returned instant is a whole-minute boundary. Costs at most
    /// one week of minute steps, which merges contiguous and adjacent segments for free — this is an
    /// ops/GUI surface, not a fill-path call.
    pub fn next_open(&self, ts_ms: i64) -> Option<i64> {
        self.scan_to(ts_ms, true)
    }

    /// The earliest instant `>= ts_ms` at which this venue is closed, or `None` when it never closes
    /// (a 24/7 calendar). `Some(ts_ms)` when already closed, mirroring [`Self::next_open`].
    pub fn next_close(&self, ts_ms: i64) -> Option<i64> {
        self.scan_to(ts_ms, false)
    }

    /// Shared body of [`Self::next_open`]/[`Self::next_close`]: the first instant `>= ts_ms` whose
    /// openness equals `want`. Returns `ts_ms` verbatim when it already matches; otherwise walks
    /// whole minutes for at most one full week (after which the weekly pattern has repeated, so no
    /// such instant exists).
    fn scan_to(&self, ts_ms: i64, want: bool) -> Option<i64> {
        if self.is_open(ts_ms) == want {
            return Some(ts_ms);
        }
        // `ts_ms` is in a minute that does NOT match, and `week_minute` is minute-granular, so the
        // whole minute does not match — the answer therefore falls on a later minute boundary.
        let base = ts_ms.div_euclid(60_000) * 60_000;
        let w = week_minute(ts_ms);
        for k in 1..=WEEK_MINUTES {
            let m = (w + k).rem_euclid(WEEK_MINUTES);
            if self.segments.iter().any(|s| s.covers(m)) == want {
                return Some(base + k * 60_000);
            }
        }
        None
    }
}

impl Default for SessionCalendar {
    fn default() -> Self {
        SessionCalendar::ALWAYS_OPEN
    }
}

// ---------------------------------------------------------------------------------------------
// The rows. Three classes cover the whole `crate::venues::VENUES` roster; each states its source.
// ---------------------------------------------------------------------------------------------

/// **Crypto CEX/DEX + prediction markets — 24/7/365.** binance, bybit, okx, deribit, aster,
/// hyperliquid, polymarket.
///
/// These venues have no weekly close at all: matching runs continuously, and the only interruptions
/// are unscheduled maintenance windows (which a static calendar cannot predict and which surface as
/// venue rejections / feed gaps regardless). No DST exposure — the row is offset-free.
pub const CRYPTO_24_7: SessionCalendar =
    SessionCalendar { tz: "UTC", segments: &[SessionSegment::new(0, WEEK_MINUTES)] };

/// **FX / CFD — the FX week.** dukascopy, oanda, ig, fxcm, ctrader.
///
/// The documented law (`crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`, until now the
/// workspace's ONLY statement of it) is *Sunday 22:00 GMT → Friday 22:00 GMT*. The true boundary is
/// the New York 17:00 close, which is **21:00 UTC under EDT and 22:00 UTC under EST** — so the
/// nominal "22:00 GMT" is the standard-time value and the boundary really does move by an hour
/// twice a year.
///
/// Per the module's DST rule this row declares the UNION: it OPENS at the earlier of the two
/// (Sunday 21:00 UTC) and CLOSES at the later of the two (Friday 22:00 UTC). The weekend gap it
/// blocks is therefore Friday 22:00 → Sunday 21:00 UTC — always a true closure, never a false one,
/// with up to one hour of genuinely closed Sunday evening left permissively open.
///
/// Declared as two segments because the week wraps: Monday 00:00 through Friday 22:00, then Sunday
/// 21:00 to the end of the week.
pub const FX_WEEK: SessionCalendar = SessionCalendar {
    tz: "America/New_York", // the 17:00 NY close is what the week boundary tracks
    segments: &[
        // Mon 00:00 UTC → Fri 22:00 UTC
        SessionSegment::new(0, 4 * DAY_MINUTES + 22 * 60),
        // Sun 21:00 UTC → end of week
        SessionSegment::new(6 * DAY_MINUTES + 21 * 60, WEEK_MINUTES),
    ],
};

/// **US equities — the regular session.** The row `vike_catalog::session_calendar_for` returns for
/// `(alpaca | ibkr, Equity | Etf)`. It is reached by the (venue, asset-class) resolver, NOT by
/// venue alone — see [`session_for`], which deliberately does not hand it back for those venues.
///
/// 09:30–16:00 `America/New_York`, Monday through Friday. In UTC that is 13:30–20:00 under EDT and
/// 14:30–21:00 under EST, so per the module's DST rule the declared segment is the UNION —
/// **13:30–21:00 UTC** — leaving one hour permissively open at whichever end the season is not
/// currently using.
///
/// Limitations, deliberate and listed as module-doc follow-ups:
/// - **No holidays.** Thanksgiving reads as a normal open Thursday. LEAN's holiday/early-close
///   database is the thing this row deliberately does not build.
/// - **US region only.** IBKR is a global book; `session_calendar_for` returns this row only for
///   the venues it is confident trade the US session, and answers `ALWAYS_OPEN` (never over-block)
///   for a region it cannot infer from (venue, asset-class) — a per-symbol override supplies the
///   real session. See that resolver's doc.
pub const US_EQUITY_REGULAR: SessionCalendar = SessionCalendar {
    tz: "America/New_York",
    segments: &[
        SessionSegment::new(13 * 60 + 30, 21 * 60), // Mon
        SessionSegment::new(DAY_MINUTES + 13 * 60 + 30, DAY_MINUTES + 21 * 60), // Tue
        SessionSegment::new(2 * DAY_MINUTES + 13 * 60 + 30, 2 * DAY_MINUTES + 21 * 60), // Wed
        SessionSegment::new(3 * DAY_MINUTES + 13 * 60 + 30, 3 * DAY_MINUTES + 21 * 60), // Thu
        SessionSegment::new(4 * DAY_MINUTES + 13 * 60 + 30, 4 * DAY_MINUTES + 21 * 60), // Fri
    ],
};

/// The **venue-only convenience default** — the [`SessionCalendar`] you get when the venue is all
/// you know. Correct for the venues whose whole book shares ONE session (a crypto CEX is 24/7; a
/// spot-FX broker runs the FX week), and the fallback the backtest gate lands on when a symbol has
/// no per-symbol override.
///
/// It is NOT the keying authority. The correct key is (venue, asset class), and for a **mixed-asset
/// venue this function deliberately answers [`SessionCalendar::ALWAYS_OPEN`]** rather than guess:
/// Alpaca runs equities AND 24/7 crypto, IBKR is global multi-asset, so a venue-only answer that
/// restricted their hours would wrongly block whichever asset it guessed wrong (over-blocking =
/// deleting real fills, the one direction the whole law forbids). To get a restricting session for
/// those venues, resolve `(venue, asset_class)` through `vike_catalog::session_calendar_for`, or
/// pin the exact row per symbol via `EngineParams::session_calendars`.
///
/// An unknown venue answers `ALWAYS_OPEN` too — **fail-PERMISSIVE**, deliberately the opposite of
/// [`crate::venue_caps::caps_for`]'s fail-closed fallback (module doc explains why the two differ).
pub fn session_for(venue: &str) -> SessionCalendar {
    match venue {
        // crypto CEX/DEX + prediction markets — the whole book has no weekly close
        "binance" | "bybit" | "okx" | "deribit" | "aster" | "hyperliquid" | "polymarket" => {
            CRYPTO_24_7
        }
        // FX / CFD brokers — the whole book runs the FX week
        "dukascopy" | "oanda" | "ig" | "fxcm" | "ctrader" => FX_WEEK,
        // vike:new-venue:row // TODO(new-venue: {venue}): pick CRYPTO_24_7 (continuous book) / FX_WEEK (FX or CFD) /
        // vike:new-venue:row // SessionCalendar::ALWAYS_OPEN (MIXED-asset — venue alone cannot say, so fail-permissive
        // vike:new-venue:row // and let vike-catalog's (venue, asset-class) resolver restrict it).
        // vike:new-venue:row "{venue}" => SessionCalendar::ALWAYS_OPEN,
        // MIXED-asset venues (equities + 24/7 crypto, or a global book): venue alone cannot pick a
        // session, so fail-permissive here and let the (venue, asset_class) resolver / per-symbol
        // override restrict them. `alpaca`/`ibkr` fall through to the arm below deliberately.
        _ => SessionCalendar::ALWAYS_OPEN,
    }
}

/// Is `venue` trading at `ts_ms`, by its **venue-only** default session? The one-call read surface
/// for the GUI / ops ("venue closed" badge) and any future live gate — [`session_for`] composed
/// with [`SessionCalendar::is_open`].
///
/// Carries [`session_for`]'s caveat: for a mixed-asset venue (alpaca/ibkr) this is always `true`,
/// because venue alone cannot say. A per-instrument badge must resolve `(venue, asset_class)` via
/// `vike_catalog::session_calendar_for` and call [`SessionCalendar::is_open`] directly.
///
/// READ-ONLY: nothing in the live order path consults it today, and adding a live block is a
/// separate change with its own venue smokes.
#[inline]
pub fn venue_is_open(venue: &str, ts_ms: i64) -> bool {
    session_for(venue).is_open(ts_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::days_from_civil;

    /// Epoch-ms for a UTC instant, built from the crate's own civil-calendar math.
    fn ms(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000 + mi * 60_000
    }

    const MIN: i64 = 60_000;

    #[test]
    fn week_minute_anchors_monday_at_zero() {
        // 2024-01-01 was a Monday.
        assert_eq!(week_minute(ms(2024, 1, 1, 0, 0)), 0);
        assert_eq!(week_minute(ms(2024, 1, 1, 0, 1)), 1);
        assert_eq!(week_minute(ms(2024, 1, 2, 0, 0)), DAY_MINUTES, "Tuesday");
        assert_eq!(week_minute(ms(2024, 1, 7, 0, 0)), 6 * DAY_MINUTES, "Sunday");
        // last minute of the week, then the wrap back to 0
        assert_eq!(week_minute(ms(2024, 1, 7, 23, 59)), WEEK_MINUTES - 1);
        assert_eq!(week_minute(ms(2024, 1, 8, 0, 0)), 0, "next Monday wraps to 0");
    }

    #[test]
    fn week_minute_is_total_including_pre_epoch() {
        for t in [i64::MIN / 2, -86_400_000 - 1, -1, 0, 1, i64::MAX / 2] {
            let w = week_minute(t);
            assert!((0..WEEK_MINUTES).contains(&w), "week_minute({t}) = {w} out of range");
        }
        // 1970-01-01 was a Thursday (index 3), so epoch 0 sits at 3 whole days in.
        assert_eq!(week_minute(0), 3 * DAY_MINUTES);
    }

    #[test]
    fn segment_is_half_open() {
        let s = SessionSegment::new(100, 200);
        assert!(!s.covers(99));
        assert!(s.covers(100), "start is inclusive");
        assert!(s.covers(199));
        assert!(!s.covers(200), "end is exclusive");
    }

    // --- the three shipped rows ---

    #[test]
    fn crypto_is_open_every_minute_of_the_week() {
        for wm in [0, 1, DAY_MINUTES, 4 * DAY_MINUTES + 1320, WEEK_MINUTES - 1] {
            // reconstruct an instant with that week-minute off a known Monday
            let t = ms(2024, 1, 1, 0, 0) + wm * MIN;
            assert!(CRYPTO_24_7.is_open(t), "crypto closed at week-minute {wm}");
        }
        assert_eq!(CRYPTO_24_7, SessionCalendar::ALWAYS_OPEN);
        // never closes → no next_close; already open → next_open is now
        let t = ms(2024, 1, 4, 12, 0);
        assert_eq!(CRYPTO_24_7.next_close(t), None);
        assert_eq!(CRYPTO_24_7.next_open(t), Some(t));
    }

    /// The FX week: open Mon 00:00 → Fri 22:00 UTC, closed until Sun 21:00 UTC. 2024-01-01 is a
    /// Monday, so 2024-01-05 is that week's Friday and 2024-01-07 its Sunday.
    #[test]
    fn fx_week_weekend_gap_is_friday_2200_to_sunday_2100() {
        // mid-week: open
        assert!(FX_WEEK.is_open(ms(2024, 1, 3, 12, 0)));
        // Friday, either side of the 22:00 UTC close
        assert!(FX_WEEK.is_open(ms(2024, 1, 5, 21, 59)));
        assert!(!FX_WEEK.is_open(ms(2024, 1, 5, 22, 0)), "Fri 22:00 UTC closes the week");
        // the weekend
        assert!(!FX_WEEK.is_open(ms(2024, 1, 6, 12, 0)), "Saturday");
        assert!(!FX_WEEK.is_open(ms(2024, 1, 7, 20, 59)), "Sunday, pre-open");
        // Sunday 21:00 UTC reopens (the DST-union early edge)
        assert!(FX_WEEK.is_open(ms(2024, 1, 7, 21, 0)));
        assert!(FX_WEEK.is_open(ms(2024, 1, 7, 23, 59)));
        // straight into Monday
        assert!(FX_WEEK.is_open(ms(2024, 1, 8, 0, 0)));
    }

    /// The FX-week boundaries to the MILLISECOND, and the DST stance stated in assertions.
    ///
    /// DST stance: we do NOT model daylight-saving transitions. The true week boundary is New
    /// York 17:00, which is 21:00 UTC under EDT and 22:00 UTC under EST — so the row declares the
    /// **union**: it CLOSES at the later edge (Fri 22:00 UTC) and REOPENS at the earlier edge
    /// (Sun 21:00 UTC). Consequence, asserted below: the gate under-blocks by up to one hour and
    /// never over-blocks. `week_minute` is minute-granular, so a boundary that falls exactly on a
    /// minute (all of ours do) is respected to the millisecond.
    #[test]
    fn fx_week_boundaries_to_the_millisecond() {
        // --- Friday close: 22:00:00.000 UTC (the later, EST edge — DST-union close) ---
        // 21:59:59.999 is the same minute as 21:59, still inside the open segment
        assert!(FX_WEEK.is_open(ms(2024, 1, 5, 21, 59) + 59_999), "Fri 21:59:59.999 — open");
        assert!(!FX_WEEK.is_open(ms(2024, 1, 5, 22, 0)), "Fri 22:00:00.000 — closed");
        // one ms before the close minute is still the previous (open) minute
        assert!(FX_WEEK.is_open(ms(2024, 1, 5, 22, 0) - 1), "Fri 21:59:59.999 (as 22:00 - 1ms)");

        // --- Sunday reopen: 21:00:00.000 UTC (the earlier, EDT edge — DST-union open) ---
        assert!(!FX_WEEK.is_open(ms(2024, 1, 7, 20, 59) + 59_999), "Sun 20:59:59.999 — closed");
        assert!(FX_WEEK.is_open(ms(2024, 1, 7, 21, 0)), "Sun 21:00:00.000 — reopen");
        // The nominal "Sunday 22:00 GMT" is the LATE (EST) edge; we open an hour early and both
        // 21:59 and 22:00 are open — the deliberate under-block, never an over-block.
        assert!(FX_WEEK.is_open(ms(2024, 1, 7, 21, 59)), "Sun 21:59 — open (past the union edge)");
        assert!(FX_WEEK.is_open(ms(2024, 1, 7, 22, 0)), "Sun 22:00 — open (nominal GMT open)");

        // --- mid-week: open across a whole ordinary weekday ---
        assert!(FX_WEEK.is_open(ms(2024, 1, 3, 0, 0)), "Wed 00:00 — open");
        assert!(FX_WEEK.is_open(ms(2024, 1, 3, 12, 0)), "Wed 12:00 — open");
        assert!(FX_WEEK.is_open(ms(2024, 1, 3, 23, 59) + 59_999), "Wed 23:59:59.999 — open");
    }

    #[test]
    fn fx_week_next_open_and_close_land_on_the_boundaries() {
        // from inside Saturday, the next open is Sunday 21:00 UTC
        assert_eq!(FX_WEEK.next_open(ms(2024, 1, 6, 12, 0)), Some(ms(2024, 1, 7, 21, 0)));
        // from mid-week, the next close is Friday 22:00 UTC
        assert_eq!(FX_WEEK.next_close(ms(2024, 1, 3, 12, 0)), Some(ms(2024, 1, 5, 22, 0)));
        // already open / already closed answer "now"
        let open_now = ms(2024, 1, 3, 12, 0);
        assert_eq!(FX_WEEK.next_open(open_now), Some(open_now));
        let closed_now = ms(2024, 1, 6, 12, 0);
        assert_eq!(FX_WEEK.next_close(closed_now), Some(closed_now));
        // the Sunday-open segment and Monday's segment are CONTIGUOUS across the week wrap, so a
        // Sunday-evening close scan runs through Monday to the following Friday rather than
        // stopping at the week boundary.
        assert_eq!(FX_WEEK.next_close(ms(2024, 1, 7, 22, 0)), Some(ms(2024, 1, 12, 22, 0)));
    }

    /// US equities: 13:30–21:00 UTC (the EDT∪EST union), weekdays only.
    #[test]
    fn us_equity_regular_session_bounds_and_weekend() {
        assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 13, 29)));
        assert!(US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 13, 30)), "union open edge (EDT 09:30)");
        assert!(US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 20, 59)));
        assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 21, 0)), "union close edge (EST 16:00)");
        // overnight
        assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 2, 0)));
        // weekend
        assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 6, 15, 0)), "Saturday");
        assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 7, 15, 0)), "Sunday");
        // all five weekdays are open at 15:00 UTC
        for d in 1..=5 {
            assert!(US_EQUITY_REGULAR.is_open(ms(2024, 1, d, 15, 0)), "weekday {d}");
        }
    }

    #[test]
    fn us_equity_next_open_skips_the_weekend() {
        // Friday after the close → Monday's open
        assert_eq!(
            US_EQUITY_REGULAR.next_open(ms(2024, 1, 5, 22, 0)),
            Some(ms(2024, 1, 8, 13, 30))
        );
        // Thursday after the close → Friday's open
        assert_eq!(
            US_EQUITY_REGULAR.next_open(ms(2024, 1, 4, 22, 0)),
            Some(ms(2024, 1, 5, 13, 30))
        );
        // inside the session → this session's close
        assert_eq!(
            US_EQUITY_REGULAR.next_close(ms(2024, 1, 3, 15, 0)),
            Some(ms(2024, 1, 3, 21, 0))
        );
    }

    /// The known limitation, pinned so it cannot be forgotten: no holiday database, so a US market
    /// holiday reads OPEN. 2024-12-25 (Christmas) was a Wednesday; NYSE was closed.
    #[test]
    fn holidays_are_not_modeled_and_read_as_open() {
        assert!(
            US_EQUITY_REGULAR.is_open(ms(2024, 12, 25, 15, 0)),
            "holiday calendars are a deliberate deferral (see the module doc)"
        );
    }

    // --- the registry ---

    /// [`session_for`] is the VENUE-ONLY default: single-session venues get their row; the
    /// mixed-asset venues (alpaca/ibkr) get `ALWAYS_OPEN` on purpose (venue alone can't pick a
    /// session — the (venue, asset-class) resolver does, tested in `vike-catalog`). One arm per
    /// roster venue, so a new venue cannot slip in unclassified.
    #[test]
    fn venue_only_default_classes_every_roster_venue() {
        // `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
        // re-indented by rustfmt once a row ending in a trailing `//` comment is generated above
        // it, which defeats `--remove`. Gated by `crates/vike-ops/tests/new_venue_gate.rs`'s
        // `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
        #[rustfmt::skip]
        let rows: &[(&str, SessionCalendar)] = &[
            ("binance", CRYPTO_24_7),
            ("bybit", CRYPTO_24_7),
            ("okx", CRYPTO_24_7),
            ("deribit", CRYPTO_24_7),
            ("aster", CRYPTO_24_7),
            ("hyperliquid", CRYPTO_24_7),
            ("polymarket", CRYPTO_24_7),
            ("oanda", FX_WEEK),
            ("ig", FX_WEEK),
            ("fxcm", FX_WEEK),
            ("dukascopy", FX_WEEK),
            ("ctrader", FX_WEEK),
            // mixed-asset venues: venue alone is insufficient ⇒ fail-permissive
            ("alpaca", SessionCalendar::ALWAYS_OPEN),
            ("ibkr", SessionCalendar::ALWAYS_OPEN),
            // vike:new-venue:row ("{venue}", SessionCalendar::ALWAYS_OPEN), // TODO(new-venue: {venue}): match the arm above
        ];
        assert_eq!(rows.len(), crate::venues::VENUES.len(), "one declared row per roster venue");
        for &v in crate::venues::VENUES {
            let (_, want) = rows
                .iter()
                .find(|(rv, _)| *rv == v)
                .unwrap_or_else(|| panic!("no SessionCalendar row declared for roster venue {v}"));
            assert_eq!(
                session_for(v),
                *want,
                "{v}: venue-only default must serve its declared row"
            );
        }
        // the restricting equity session is NOT reachable by venue alone — that is the fix
        assert_ne!(session_for("alpaca"), US_EQUITY_REGULAR);
        assert_ne!(session_for("ibkr"), US_EQUITY_REGULAR);
    }

    /// The fail-PERMISSIVE fallback — the deliberate inversion of `venue_caps`'s fail-closed one.
    #[test]
    fn unknown_venue_is_always_open() {
        assert_eq!(session_for("nasdaq"), SessionCalendar::ALWAYS_OPEN);
        // ...at every minute, including the FX weekend and a US holiday
        for t in [ms(2024, 1, 6, 12, 0), ms(2024, 12, 25, 15, 0), ms(2024, 1, 3, 3, 0)] {
            assert!(venue_is_open("nasdaq", t), "an unknown venue must never be blocked");
        }
        assert_eq!(SessionCalendar::default(), SessionCalendar::ALWAYS_OPEN);
    }

    #[test]
    fn venue_is_open_matches_the_calendar_query() {
        let weekend = ms(2024, 1, 6, 12, 0);
        assert!(!venue_is_open("dukascopy", weekend));
        assert!(venue_is_open("binance", weekend));
        // alpaca is a mixed-asset venue: venue alone can't restrict it, so the venue-only surface
        // reports open even on a Saturday. The (venue, asset-class) resolver is what closes it.
        assert!(venue_is_open("alpaca", weekend));
        for v in crate::venues::VENUES {
            assert_eq!(venue_is_open(v, weekend), session_for(v).is_open(weekend), "{v}");
        }
    }

    #[test]
    fn state_mirrors_is_open() {
        let weekend = ms(2024, 1, 6, 12, 0);
        assert_eq!(FX_WEEK.state(weekend), SessionState::Closed);
        assert_eq!(CRYPTO_24_7.state(weekend), SessionState::Open);
    }

    // --- structural invariants every row must hold ---

    #[test]
    fn every_row_has_ascending_non_overlapping_in_range_segments() {
        for (name, cal) in [
            ("ALWAYS_OPEN", SessionCalendar::ALWAYS_OPEN),
            ("CRYPTO_24_7", CRYPTO_24_7),
            ("FX_WEEK", FX_WEEK),
            ("US_EQUITY_REGULAR", US_EQUITY_REGULAR),
        ] {
            let mut prev_end = 0;
            for s in cal.segments {
                assert!(s.start_min < s.end_min, "{name}: empty/inverted segment {s:?}");
                assert!(s.start_min >= prev_end, "{name}: segments must ascend, got {s:?}");
                assert!(
                    (0..=WEEK_MINUTES).contains(&s.end_min) && s.start_min >= 0,
                    "{name}: segment {s:?} out of the week"
                );
                prev_end = s.end_min;
            }
            assert!(!cal.tz.is_empty(), "{name}: tz provenance must be recorded");
        }
    }

    /// `next_open`/`next_close` agree with `is_open` at and just before every boundary they
    /// report, for every row — the property that keeps the scan and the predicate one law.
    #[test]
    fn scan_results_agree_with_is_open() {
        let base = ms(2024, 1, 1, 0, 0);
        for cal in [CRYPTO_24_7, FX_WEEK, US_EQUITY_REGULAR] {
            for k in (0..WEEK_MINUTES).step_by(37) {
                let t = base + k * MIN;
                if let Some(o) = cal.next_open(t) {
                    assert!(cal.is_open(o), "next_open({t}) = {o} is not open");
                    assert!(o >= t);
                    if o > t {
                        assert!(
                            !cal.is_open(o - MIN),
                            "next_open({t}) = {o} is not the FIRST open"
                        );
                    }
                }
                if let Some(c) = cal.next_close(t) {
                    assert!(!cal.is_open(c), "next_close({t}) = {c} is not closed");
                    assert!(c >= t);
                    if c > t {
                        assert!(
                            cal.is_open(c - MIN),
                            "next_close({t}) = {c} is not the FIRST close"
                        );
                    }
                }
            }
        }
    }
}
