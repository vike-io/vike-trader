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
//! The shape is [`crate::venues::venue_caps`]'s: a declarative table plus one pure query, so the backtest
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
//! The resolution lives **where both venue and asset class are visible** — in `vike-catalog`
//! (`vike_catalog::session_calendar_for`) or in the consumer — and this module takes the resolved
//! thing as a **caller-supplied parameter**:
//!
//! - the pure query works on a [`SessionCalendar`] the caller hands it (backtest stores one PER
//!   SYMBOL; see `EngineParams::session_calendars`);
//! - [`session_for`] is only a **venue-ONLY convenience default** — see its doc for why it is
//!   deliberately fail-permissive for the mixed-asset venues rather than guessing their class.
//!
//! ⚠ **This paragraph used to say the map CANNOT live here** — that [`crate::AssetClass`] was a
//! `vike-catalog` concept and owning the map would invert a down-only edge. **That reason is
//! DEAD**: the taxonomy moved INTO this crate (see [`crate::asset_class`]) so that the hist store
//! could name it, and both halves of the key are visible here now. The split above is preserved
//! deliberately rather than by necessity — the rows a resolver would consult are a per-venue
//! CATALOG table, and `docs/decisions/0061`'s second reason (nothing forces a per-venue table into
//! the vocabulary crate) is the one still standing. A future PR that moves the resolver down has to
//! argue it on those terms, not on a layering refusal that no longer exists.
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
//! [`crate::venues::venue_caps::VenueCaps::UNSUPPORTED`]. The two defaults differ because the two questions
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
/// `&'static [_]`) so it is free to hand to a UI frame by value, like [`crate::venues::venue_caps::VenueCaps`].
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
/// [`crate::venues::venue_caps::caps_for`]'s fail-closed fallback (module doc explains why the two differ).
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

#[path = "session_tests.rs"]
#[cfg(test)]
mod session_tests;
