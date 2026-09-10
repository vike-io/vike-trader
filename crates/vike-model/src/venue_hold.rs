//! `venue_hold` — the VENUE-DECLARED order holds, as venue FACTS rather than backtest constants.
//!
//! A venue hold is how long the EXCHANGE deliberately sits on an incoming order before matching or
//! booking it (see [`crate::SymbolProperties::taker_hold_ms`] for the full mechanism write-up and
//! the live measurements). It is observed on the wire by a bridge crate and consumed as entry-leg
//! delay by the backtest engine — two crates that cannot see each other, and both of which depend
//! on vike-model. So the numbers live HERE, once.
//!
//! ⚠ **Why this is two constants and not a `venue_caps`-style capability-map row.** The repo's
//! per-venue capability-map convention (CLAUDE.md) is for a knob whose value is a property of the
//! VENUE — one row per venue, iterated by a `VENUES`-completeness test. A taker hold is not that
//! shape: Polymarket's own hold is `250` on a crypto up/down market, `3000` on a sports game
//! market, and `0` on a politics market, so no per-venue row can state it. That is exactly why the
//! value is carried PER MARKET on [`crate::SymbolProperties::taker_hold_ms`] and recorded on the
//! point-in-time `kind=properties` tape. What lives here is only the *catalogue of the distinct
//! holds a venue is known to declare* — the literals a parser resolves to and an engine models —
//! and a per-venue table of those would be a table with one populated row.
//!
//! **These values change.** Polymarket has already moved this one once (500 ms → 250 ms), which is
//! the entire reason for a single home: before this module the `250` and the `3000` each existed
//! twice, in `vike-backtest` and `vike-polymarket`, with a comment in the latter explaining that it
//! could not reference the former.
//!
//! Units are milliseconds and the type is [`u32`], matching
//! [`crate::SymbolProperties::taker_hold_ms`]; `vike-backtest` re-exports them under its historical
//! `i64` names for its nanosecond arithmetic.

/// Polymarket's **`itode` hold: 250 ms**, on the CRYPTO/finance up/down markets — mechanism (1) of
/// the two described on [`crate::SymbolProperties::taker_hold_ms`].
///
/// Declared ONLY by the boolean `itode` on `GET https://clob.polymarket.com/clob-markets/
/// {condition_id}` (terse-keyed payload). Verified `itode: true` on 4/4 live btc/eth/sol/xrp
/// `updown-5m` markets, measured live 2026-07-23; live since 2026-06-05. Semantics
/// (docs.polymarket.com/concepts/order-lifecycle): the order is held 250 ms, a CANCEL IS REJECTED
/// while it is pending, it survives a dropped connection, and it is then re-validated and either
/// matched or placed on the book.
///
/// Resolved from the wire by `vike_polymarket::taker_hold::resolve_taker_hold_ms`; modelled as
/// entry-leg delay by `vike_backtest::latency` (which re-exports this as
/// `VENUE_HOLD_POLYMARKET_UPDOWN_MS: i64`).
pub const POLYMARKET_ITODE_HOLD_MS: u32 = 250;

/// Polymarket's **sports GAME-market delay: 3000 ms** — mechanism (2), entirely INDEPENDENT of
/// [`POLYMARKET_ITODE_HOLD_MS`]. The two are declared by different fields, on different endpoints,
/// over disjoint market sets, and have never been observed co-occurring.
///
/// Declared as `seconds_delay` on the CLOB `/markets/{condition_id}` payload (`sd` on the terse
/// `/clob-markets/{condition_id}` one, `secondsDelay` on Gamma) — i.e. the venue states it in
/// SECONDS and this constant is that value times 1000. Measured live 2026-07-23 over every open
/// sports market: **400 of 400 markets carrying a `game_start_time` report `seconds_delay: 3`**,
/// while 191 of 192 futures/props report `0`; the discriminator is exactly "has a game start". NBA
/// (370) plus EPL/NFL/CBB/IPL. Politics: 0 of 30.
///
/// Twelve times the crypto hold, and long enough that the book a strategy decided on is routinely
/// gone by delivery — a sports taker backtest run without it is not merely slightly optimistic.
///
/// ⚠ **This is the value for TRADITIONAL sports games, not a universal one — never assume it.**
/// A later live sweep (2026-07-23, over Gamma's whole open tradeable listing) found the entire
/// esports book — CS2 futures/handicaps/totals — declaring `secondsDelay: 1`, i.e. a 1000 ms hold,
/// alongside the `3` on traditional game markets. The venue states the number PER MARKET, which is
/// exactly why the value that reaches a replay comes off
/// [`crate::SymbolProperties::taker_hold_ms`] (recorded from the wire at token resolution) and not
/// from this constant. This constant exists to NAME the traditional-game figure for docs and for
/// the "prefer a 3 s market" arm of the live smoke — nothing resolves a hold by reading it.
pub const POLYMARKET_SPORTS_GAME_HOLD_MS: u32 = 3_000;

/// The venue's stated unit for [`POLYMARKET_SPORTS_GAME_HOLD_MS`]: `seconds_delay` is in SECONDS,
/// so a parser multiplies by this. Kept beside the constant so the `3` on the wire and the `3000`
/// here can never drift into disagreeing about the unit.
pub const MS_PER_SECOND: u32 = 1_000;

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire→constant relationship, pinned: the venue says `3` seconds, we model 3000 ms.
    #[test]
    fn the_sports_hold_is_the_wire_seconds_times_the_unit() {
        assert_eq!(3 * MS_PER_SECOND, POLYMARKET_SPORTS_GAME_HOLD_MS);
    }

    /// The two mechanisms are different numbers — a regression that collapsed them would silently
    /// model a sports market at the crypto hold (or vice versa), which is the exact conflation the
    /// `taker_hold_ms` docs exist to prevent.
    #[test]
    fn the_two_polymarket_mechanisms_are_distinct() {
        assert_ne!(POLYMARKET_ITODE_HOLD_MS, POLYMARKET_SPORTS_GAME_HOLD_MS);
        assert_eq!(POLYMARKET_ITODE_HOLD_MS, 250);
        assert_eq!(POLYMARKET_SPORTS_GAME_HOLD_MS, 3_000);
    }
}
