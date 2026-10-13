//! The bar-interval VOCABULARY: the strings a feed's bars are keyed by (`"1m"`, `"4h"`, `"1d"`) and
//! the one parser that gives them a width.
//!
//! Three items: [`interval_ms`] (an interval string -> epoch-milliseconds), [`measures_bar_step`]
//! (can the still-forming-bar guard measure this step) and [`KLINE_MENU`] (the venue kline
//! intervals a chart offers, in menu order). Pure `std` and no calendar: they lived in
//! `vike-model`'s `time` module, whose civil-calendar math they never used, and moved here so a crate
//! that only names a bar interval does not link the domain vocabulary. The `[walkforward]` duration
//! grammar (`vike_model::time::parse_span`) is a SEPARATE grammar and deliberately stays there; its
//! own doc argues why the two must not merge.

/// The venue KLINE intervals a chart offers, in menu order: the ONE list behind the desktop
/// title bar's interval menu (`crates/vike-desktop/src/chart_window.rs`'s `title_bar`) and
/// `vike_orderflow::tickvol::BarKind::parse`'s kline-first precedence (a member is a `Kline`
/// passthrough even where it would otherwise parse as `<n>t`/`<x>v`; `"1s"` proves the rule).
///
/// ⚠ `4h` and `1d` joined on 2026-09-15 — the owner's own worked example ("i open same ethusdt
/// daily") named an interval the menu could not produce at all. They need nothing from the
/// venues: [`interval_ms`] parses both, and the collectors already write them. For `BarKind::parse`
/// membership changes nothing for them: neither ends in `t` or `v`, so both already fell through
/// to its `Kline` passthrough.
///
/// ⚠ `1s` STAYS, and it is the one row that cannot paint everywhere: **bybit's kline minimum is
/// 1m**, so no `1s` rows exist in the store for it (or for okx). Whether to keep offering a row
/// two of the three collector venues can never answer is a product call, deliberately not taken
/// here.
pub const KLINE_MENU: [&str; 7] = ["1s", "1m", "5m", "15m", "1h", "4h", "1d"];

/// Parse a vike interval string (`"30s"`/`"1m"`/`"5m"`/`"4h"`/`"1d"`) into epoch-**milliseconds**:
/// an integer count followed by exactly one unit char — `s`/`m`/`h`/`d`. Returns `None` for
/// anything malformed (fewer than 2 chars, empty or non-digit count, unknown unit).
///
/// The single home for the interval vocabulary that was re-parsed — divergently — in three places
/// (`vike-backtest`'s `parse_timeframe` rejected seconds, `vike-data`'s took `s`/`m`/`h`/`d`,
/// `vike-ibkr`'s live feed took a fixed five-word whitelist and silently streamed one-minute bars
/// for any other interval), so a `"30s"` series that loaded fine from the hist store could not be
/// named as a backtest timeframe. Every previously-accepted string maps to the exact same value
/// here, so delegating is a safe monotonic widening; a caller narrows the answer to what it can
/// serve (`vike-ibkr` refuses an interval its 5-second realtime stream cannot build) and refuses
/// the rest, never defaults it.
pub fn interval_ms(interval: &str) -> Option<i64> {
    if interval.len() < 2 {
        return None;
    }
    let (num, unit) = interval.split_at(interval.len() - 1);
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(n * mult)
}

/// **Can the still-forming-bar guard MEASURE this step?** [`interval_ms`] with the value thrown
/// away, given a name — because this question is asked by four separate REFUSALS in this workspace
/// and the answer is the whole of
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s bug B.
///
/// The argument, once, here, rather than re-derived at each caller:
///
/// * A kline collector drops the venue's still-forming last candle by comparing `bar.ts + step`
///   against "now", and the step comes from [`interval_ms`], which reads a count plus ONE of
///   `s`/`m`/`h`/`d`. `1w`, `1M` and `1mo` have no width there.
/// * The guard therefore DECLINES on those spellings rather than acting — deliberately, since a
///   defensive filter must never panic or drop data on a string it does not understand
///   (`crates/vike-backfill/src/klines.rs`'s `unparseable_interval_leaves_bars_untouched` pins
///   exactly that).
/// * So the DECISION belongs to whoever is about to write, and what it costs if nobody makes it is
///   permanent: the venue's open candle is stored as a closed bar AND the window spends its commit
///   key, so `vike_store::DataFusionHist`'s `commit_rows` answers a corrective re-fetch with `Ok(0)`
///   — a silent success over the wrong row, with no verb in this workspace that retires a single
///   key.
///
/// ⚠ **Widening [`interval_ms`] is NOT the fix and is argued against in-tree**: that parser is also
/// a resample step and a page-span estimate (and was a cadence validator for the collector
/// supervisor docs/decisions/0094 deleted), and a calendar month is not a multiple of anything. 0059 records both reasons. This predicate exists so the four
/// callers name one question instead of four spellings of `interval_ms(..).is_none()`, and so a
/// gate can SEE which writers consult it — `crates/vike-ops/tests/venues/kline_ingest_gate.rs` derives the
/// bar producers from `crates/vike-data/src/store/store_kind.rs` and demands every one of them either
/// name this function or carry a written row saying why it need not.
///
/// ⚠ It answers about the STEP VOCABULARY, never about a venue: a step that measures here can still
/// be one a venue does not serve (the deleted one-shot `hyperliquid_backfill` program's own
/// allow-list refused `4h` all the same), and `interval=funding` (the reserved label
/// `crates/vike-backfill/src/funding_rate.rs` writes bars under) does not measure and is not a
/// candle at all.
#[must_use]
pub fn measures_bar_step(interval: &str) -> bool {
    interval_ms(interval).is_some()
}

#[path = "interval_tests.rs"]
#[cfg(test)]
mod interval_tests;
