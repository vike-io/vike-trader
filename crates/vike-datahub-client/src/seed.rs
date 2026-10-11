//! **The bounds on [`Request::SeedSeries`](crate::proto::Request::SeedSeries) that BOTH ends need**
//! — the window the server fetches, the intervals it will fetch, and the two validators that stand
//! between a client-named string and a venue URL. Declared ungated: a default `vike-datahub` build
//! must DECODE the verb in order to answer or refuse it cleanly.
//!
//! # Why the constants live HERE and not in `vike-datahub`
//!
//! A client cannot guard a rule the server does not know, nor the reverse (as with
//! [`crate::market`]'s `MD_MAX_SYMBOL_BYTES` / `validate_md_symbol`).
//! `crates/vike-datahub/src/server/seed_series.rs`'s `seed_series_verb` and
//! [`crate::DatahubClient::seed_series`] both call the functions below, so a refusal seen locally is
//! the one the server would give: the local check is for the MESSAGE, the server's re-check at its
//! own door, before it dispatches to a venue, is the enforcement. The SERVER's own constants (the
//! token bucket, the per-process series cap) stay in `crates/vike-datahub/src/seed.rs`: a client
//! that knew them could only mis-predict them.
//!
//! # ⚠ The validators are a SECURITY boundary, not hygiene
//!
//! `crates/bridges/binance/src/family/klines.rs`'s `klines_url` interpolates `symbol` and `interval`
//! into its query string with **no allowlist and no percent-encoding** (bybit and okx validate in
//! their own code tables: `crates/bridges/bybit/src/data.rs`'s `interval_code`,
//! `crates/bridges/okx/src/data.rs`'s `bar_code`). Those strings now arrive from an OBSERVE client,
//! so [`validate_seed_interval`] and [`validate_seed_symbol`] run at the server's door before a venue
//! is dispatched to. `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` records them as a
//! STOPGAP that the deferred per-venue interval table replaces.

use std::fmt::Write as _;

/// How many bars ONE seed fetches — the whole of the "how much history" question; the client names
/// no part of it (the cost rule
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`'s decision 1 rests on).
///
/// **600 is ONE page on binance and bybit at ANY interval**, so a `1m` chart and a `1d` chart cost
/// the same: `crates/bridges/binance/src/family/klines.rs`'s `MAX_LIMIT` and
/// `crates/bridges/bybit/src/data.rs`'s `MAX_LIMIT` are both 1000 (`/klines?limit=1000` is weight 2
/// on binance spot and 5 on `fapi`, per `crates/vike-model/src/rate_limits.rs`).
///
/// ⚠ **SIX paced pages on OKX** (`crates/bridges/okx/src/data.rs`'s `MAX_LIMIT` is 100), accepted:
/// OKX publishes no weight counter, its pager paces itself, and a per-venue window is the deferred
/// per-venue table rather than this constant. Aster pages through the binance family pager (one
/// page, very likely); deribit and hyperliquid are UNMEASURED here.
///
/// Comfortably above a chart's visible width, and far below
/// `vike_app_core::data::store_bars::STORE_READ_BARS`, so the read after a seed trims nothing.
pub const SEED_BARS: u32 = 600;

/// The intervals a seed will fetch. **The client's string is checked against THIS and nothing else
/// before a venue is dispatched to** (see the module doc).
///
/// The rule: (1) what every venue in `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table`
/// serves (the code tables are bybit's `interval_code` and okx's `bar_code`), intersected with
/// (2) what `vike_model::time::interval_ms` can parse — an interval with no bar width has no window
/// and no partition.
///
/// ⚠ **`4h` stays although deribit refuses it** (`crates/bridges/deribit/src/data.rs`'s
/// `resolution_code` has no 4h): removing it would take a step from the five venues that serve it
/// to spare deribit an error it answers cleanly from its OWN table before a request leaves the box
/// (a [`crate::proto::Response::Error`] with nothing written), not the silent wrong bucket rule 2
/// prevents.
///
/// ⚠ **`1s` is EXCLUDED though binance serves it:** bybit and okx refuse it from their own tables,
/// so admitting it would make this set a per-venue answer (and 600 bars of `1s` is ten minutes).
/// `1w`/`1M` have no width in `interval_ms`'s grammar (rule 2);
/// `crates/vike-datahub/src/server/backfill.rs`'s `backfill_verb` refuses the same family on the
/// Control-scoped sibling, and this set stays the narrower allowlist because an OBSERVE client names
/// the string.
///
/// A STOPGAP: the per-venue interval table
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` defers replaces it.
pub const SEED_INTERVALS: [&str; 11] =
    ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d"];

/// The longest symbol a seed will carry. The worst real case is deribit's option names
/// (`BTC-28MAR25-100000-C`, 20 bytes); 32 clears it with room, and a venue with longer names would
/// reopen it.
///
/// ⚠ NOT [`crate::market::MD_MAX_SYMBOL_BYTES`], and must not become it: that 96 is DERIVED from a
/// market-data control-frame ceiling and a 78-digit polymarket token id, and no polymarket token
/// reaches a kline collector, so borrowing it would widen what reaches a URL threefold for no venue
/// that exists.
pub const SEED_MAX_SYMBOL_BYTES: usize = 32;

/// The bytes a seed symbol may be built from: ASCII letters, digits, and the three separators the
/// table's venues spell (`-` for `BTC-USDT-SWAP` and `BTC-PERPETUAL`, `.` for the `.P` perp suffix,
/// `_` for the store's partition vocabulary).
///
/// An ALLOWLIST, and that direction is the point: `klines_url` interpolates this string unencoded,
/// and the dangerous set (`&`, `#`, `?`, `%`, a newline, a space, …) is open.
///
/// ⚠ Two real spellings fall outside it, correctly: a hyperliquid SPOT pair (`HYPE/USDC`) and a
/// HIP-3 perp (`test:BTC`). `vike_hyperliquid::history::identity_coin_for` refuses the `/` case at
/// the collector for a second, independent reason, so it is refused twice over rather than guessed
/// at.
fn is_seed_symbol_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_')
}

/// The interval rule: membership in [`SEED_INTERVALS`], exact and case-SENSITIVE — `1M` and `1m`
/// are a calendar month and a minute at every venue, so a case-insensitive match would silently turn
/// one into the other. The message names the whole permitted set, so an operator who sent `1s` sees
/// that the SET refused it rather than the venue.
pub fn validate_seed_interval(interval: &str) -> Result<(), String> {
    if SEED_INTERVALS.contains(&interval) {
        return Ok(());
    }
    let mut msg = format!(
        "interval {interval:?} is not one this server will seed. It is refused HERE, before any \
         venue is dispatched to. Permitted: ["
    );
    for (i, iv) in SEED_INTERVALS.iter().enumerate() {
        if i > 0 {
            msg.push_str(", ");
        }
        let _ = write!(msg, "{iv}");
    }
    msg.push_str(
        "]. The set is the intersection of what every venue in this server's collector table \
         serves and what the store's own interval vocabulary can parse, so it is narrower than any \
         one venue — `1s` is served by binance alone and is excluded for that reason.",
    );
    Err(msg)
}

/// The symbol rules, cheapest first, each naming what was wrong.
///
/// ⚠ **The message never ECHOES the symbol** ([`crate::market::validate_md_symbol`]'s rule): it
/// names the LENGTH, the CAP and the OFFSET, never the untrusted string, which would ride into a
/// log whose file layer defaults to `trace`.
///
/// ⚠ **Not trimmed, not upper-cased:** the venue is sent the spelling verbatim, so altering it here
/// would make the store's partition and the venue's answer disagree about what was asked for.
pub fn validate_seed_symbol(symbol: &str) -> Result<(), String> {
    if symbol.is_empty() {
        return Err(
            "a seed symbol is EMPTY, so it names no instrument. Pass the venue's own spelling \
             (`BTCUSDT`, `BTCUSDT.P`, `BTC-USDT-SWAP`)."
                .to_string(),
        );
    }
    if symbol.len() > SEED_MAX_SYMBOL_BYTES {
        return Err(format!(
            "a seed symbol of {} bytes exceeds SEED_MAX_SYMBOL_BYTES = {SEED_MAX_SYMBOL_BYTES}. \
             The longest spelling any venue in this server's collector table uses is 20 bytes \
             (`BTC-28MAR25-100000-C`, a deribit option).",
            symbol.len()
        ));
    }
    if let Some(at) = symbol.bytes().position(|b| !is_seed_symbol_byte(b)) {
        return Err(format!(
            "a seed symbol carries a byte outside the permitted set at offset {at}. A seed symbol \
             may hold ASCII letters, digits, `-`, `.` and `_` only — it is interpolated into a \
             venue REST query string unencoded, so this is an ALLOWLIST rather than a check for \
             characters known to be dangerous."
        ));
    }
    Ok(())
}

/// The inclusive epoch-ms window one seed fetches: the last [`SEED_BARS`] bars ending `now_ms`.
///
/// **The SERVER computes this** from its own clock and constant — the request carries no range (the
/// term 0058 turns on). Exposed so a client can report what it is about to be given, and so the two
/// ends cannot disagree about what "600 bars" meant.
///
/// `None` for an interval outside [`SEED_INTERVALS`] or one `vike_model::time::interval_ms` cannot
/// parse — the same set by construction; the redundant check stays because this is the function
/// that would otherwise multiply by a garbage width.
pub fn seed_range(interval: &str, now_ms: i64) -> Option<(i64, i64)> {
    if !SEED_INTERVALS.contains(&interval) {
        return None;
    }
    let step = vike_model::time::interval_ms(interval).filter(|&ms| ms > 0)?;
    Some((now_ms.saturating_sub(step.saturating_mul(i64::from(SEED_BARS))), now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn every_permitted_interval_has_a_bar_width_the_store_can_parse() {
        // Rule 2 of `SEED_INTERVALS`' derivation, asserted: otherwise `seed_range` would answer
        // `None` for an interval `validate_seed_interval` had just accepted.
        for iv in SEED_INTERVALS {
            let ms = vike_model::time::interval_ms(iv);
            assert_matches!(ms, Some(n) if n > 0, "{iv} has no positive bar width: {ms:?}");
            assert!(seed_range(iv, 1_700_000_000_000).is_some(), "{iv} has no window");
        }
    }

    #[test]
    fn the_permitted_set_is_sorted_by_bar_width_and_free_of_duplicates() {
        // Not cosmetic: the set is rendered into a refusal an operator reads.
        let widths: Vec<i64> =
            SEED_INTERVALS.iter().map(|iv| vike_model::time::interval_ms(iv).unwrap()).collect();
        assert!(widths.windows(2).all(|w| w[0] < w[1]), "not strictly increasing: {widths:?}");
    }

    #[test]
    fn the_intervals_no_venue_or_no_store_can_serve_are_refused_by_name() {
        // `1s`: binance serves it, bybit and okx refuse it in their own code tables (MEASURED).
        // `1w`/`1M`: no bar width in `interval_ms`'s grammar. `1M` also proves case sensitivity.
        for bad in ["1s", "1w", "1M", "1mo", "60", "", "1h ", " 1h", "1H"] {
            let err = validate_seed_interval(bad).expect_err("{bad} must be refused");
            assert!(err.contains("before any venue"), "{bad}: {err}");
            assert!(err.contains("Permitted"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_url_metacharacter_in_a_symbol_is_refused_and_the_symbol_is_never_echoed() {
        // The whole reason `validate_seed_symbol` exists: `klines_url` interpolates unencoded, so
        // each of these would otherwise become an extra query parameter on a venue REST call.
        for bad in ["BTC&limit=9999", "BTC USDT", "BTC#frag", "BTC?x=1", "BTC%2F", "BTC\nUSDT"] {
            let err = validate_seed_symbol(bad).expect_err("{bad} must be refused");
            assert!(err.contains("outside the permitted set"), "{bad}: {err}");
            assert!(!err.contains(bad), "the refusal echoed the symbol back: {err}");
        }
    }

    #[test]
    fn every_real_venue_spelling_in_the_table_is_accepted() {
        for good in ["BTCUSDT", "BTCUSDT.P", "BTC-USDT", "BTC-USDT-SWAP", "ETHUSDT", "1000PEPEUSDT"]
        {
            validate_seed_symbol(good).unwrap_or_else(|e| panic!("{good} must be accepted: {e}"));
        }
    }

    #[test]
    fn the_longest_real_spelling_is_comfortably_inside_the_cap() {
        // The claim `SEED_MAX_SYMBOL_BYTES`' doc makes, held rather than asserted in prose.
        assert!(SEED_MAX_SYMBOL_BYTES >= 2 * "BTC-USDT-SWAP".len());
        let over = "X".repeat(SEED_MAX_SYMBOL_BYTES + 1);
        let err = validate_seed_symbol(&over).expect_err("over-long must be refused");
        assert!(err.contains("SEED_MAX_SYMBOL_BYTES"), "{err}");
        assert!(!err.contains(&over), "the refusal echoed the symbol back");
    }

    #[test]
    fn the_window_is_exactly_seed_bars_wide_whatever_the_interval() {
        // The property that makes opening a 1m chart and a 1d chart cost the same request count on
        // binance and bybit — the reasoning `SEED_BARS`' doc carries.
        const NOW: i64 = 1_700_000_000_000;
        for iv in SEED_INTERVALS {
            let (start, end) = seed_range(iv, NOW).expect("permitted");
            let step = vike_model::time::interval_ms(iv).unwrap();
            assert_eq!(end, NOW, "{iv}");
            assert_eq!(end - start, step * i64::from(SEED_BARS), "{iv}");
        }
    }

    #[test]
    fn a_refused_interval_has_no_window() {
        assert_eq!(seed_range("1s", 1_700_000_000_000), None);
        assert_eq!(seed_range("not-an-interval", 1_700_000_000_000), None);
    }
}
