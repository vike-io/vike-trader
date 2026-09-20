//! **The bounds on [`Request::SeedSeries`](crate::proto::Request::SeedSeries) that BOTH ends need**
//! — the window the server fetches, the intervals it will fetch, and the two validators that stand
//! between a client-named string and a venue URL.
//!
//! # Why the constants live HERE and not in `vike-datahub`
//!
//! Same rule as [`crate::market`]'s `MD_MAX_SYMBOL_BYTES` / `validate_md_symbol`, and for the same
//! reason: a client cannot guard a rule the server does not know, nor the reverse. The server's door
//! is `crates/vike-datahub/src/server.rs`'s `seed_series_verb` and the client's is
//! [`crate::DatahubClient::seed_series`]; both call the functions below, so a refusal the operator
//! sees locally is the refusal the server would have given. The constants that are the SERVER's
//! alone — the token bucket, the per-process series cap — stay in `crates/vike-datahub/src/seed.rs`,
//! because a client that knew them could only ever mis-predict them.
//!
//! # ⚠ The validators are a SECURITY boundary, not hygiene
//!
//! `crates/bridges/binance/src/family/klines.rs`'s `klines_url` builds its request as
//! `format!("{base}?symbol={symbol}&interval={interval}&limit={limit}")` — **no allowlist, no
//! percent-encoding**, on both fields. Bybit and OKX validate in their own code tables
//! (`crates/bridges/bybit/src/data.rs`'s `interval_code`, `crates/bridges/okx/src/data.rs`'s
//! `bar_code`) and binance does not, and the owner has deferred the per-venue interval table that
//! would own this properly. Until this verb existed every interval and symbol reaching that line
//! came from an operator's argv or from a `VerbScope::Control` client. They now arrive from an
//! OBSERVE client, which is exactly the untrusted path that hole was waiting for — so
//! [`validate_seed_interval`] and [`validate_seed_symbol`] run at the server's door, before the
//! venue is dispatched to, and `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` records
//! that they are a STOPGAP the per-venue table replaces rather than a permanent home.

use std::fmt::Write as _;

/// How many bars ONE seed fetches — the whole of the "how much history" question, and the client
/// names no part of it (`docs/decisions/0057`'s decision 1 rests on that).
///
/// **600 is ONE page on most of the table venues at ANY interval**, which is what makes opening a
/// `1m` chart and a `1d` chart cost the same: `crates/bridges/binance/src/family/klines.rs`'s
/// `MAX_LIMIT` and `crates/bridges/bybit/src/data.rs`'s `MAX_LIMIT` are both 1000, and
/// `crates/vike-model/src/rate_limits.rs` records `/klines?limit=1000` at weight 2 on binance spot
/// and 5 on `fapi`.
///
/// ⚠ **It is SIX pages on OKX, and that correction is carried rather than designed around.**
/// `crates/bridges/okx/src/data.rs`'s `MAX_LIMIT` is **100**, not 1000, and its own doc calls that
/// "the one figure that makes OKX's pace unlike its siblings'". Six paged requests with that pager's
/// `PAGE_DELAY` between them is the real cost there. It is accepted because OKX publishes no weight
/// counter at all (`vike_model::rate_limits::PaceSample::per_request_weight`'s doc says so), its
/// pager already paces itself, and a per-venue window would make "what did the server fetch" a
/// per-venue answer for a difference no chart can see.
///
/// ⚠ **The table grew from three venues to six on 2026-09-16** (0059 Phase 2), and the paging
/// figures were never re-derived for the three that joined. The reasoning is unchanged in SHAPE —
/// a per-venue window is still the deferred per-venue table rather than this constant — but the
/// arithmetic above is now a statement about binance and bybit, with okx measured against them.
/// Aster pages through the same binance family pager and its `MAX_LIMIT`, so one page is very
/// likely; deribit and hyperliquid are UNMEASURED here and this doc does not guess at them.
///
/// Comfortably above a chart's visible width so scrolling left finds history, and far below
/// `vike_app_core::store_bars::STORE_READ_BARS` — the read that follows a seed asks for more than
/// a seed writes, so nothing is trimmed on the way back.
pub const SEED_BARS: u32 = 600;

/// The intervals a seed will fetch. **The client's string is checked against THIS and nothing else
/// before a venue is dispatched to** — see the module doc for why that is load-bearing.
///
/// It was the INTERSECTION of two sets, and deliberately narrower than any one venue:
///
/// 1. what every venue in `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table` serves —
///    read from `crates/bridges/bybit/src/data.rs`'s `interval_code` and
///    `crates/bridges/okx/src/data.rs`'s `bar_code`, the two that carry code tables at all;
/// 2. what `vike_model::time::interval_ms` can parse, since an interval with no bar width has no
///    window to ask for and no partition the store can reason about.
///
/// ⚠ **Rule 1 stopped holding on 2026-09-16 and the set did not move.** 0059 Phase 2 widened
/// `real_backfill_table` from three venues to six, and `crates/bridges/deribit/src/data.rs`'s
/// `resolution_code` — whose own doc says "Note the gaps — there is **no 4h**" — refuses `4h`.
/// So `4h` is in this set and outside the new intersection. It is left in DELIBERATELY: removing
/// it would take a step away from the five venues that do serve it, on a wire surface both ends
/// predict with, to spare deribit an error it answers correctly from its OWN table before a
/// request leaves the box. What a `4h` deribit seed gets is `deribit: unsupported interval "4h"`,
/// a clean [`crate::proto::Response::Error`] with nothing written — the same shape as any other
/// venue refusal, and NOT the silent wrong-bucket this constant's rule 2 exists to prevent. The
/// per-venue interval table that `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`
/// defers is what makes this answerable per venue; until it exists, this set is what every venue
/// is asked in common, and one venue declines a member of it.
///
/// ⚠ **`1s` IS EXCLUDED and binance serves it.** MEASURED 2026-09-15 against the live collectors:
/// binance wrote 10 rows for a `1s` window; bybit and okx answered an error from their OWN code
/// tables, so the request never left the box. Admitting it would make this set a per-venue answer,
/// which is precisely the table the owner deferred — and 600 bars of `1s` is ten minutes of chart.
/// `1w` and `1M` are excluded by rule 2: `interval_ms` splits on a single trailing character over
/// `s`/`m`/`h`/`d`, so neither has a width. ⚠ That exclusion is no longer this verb's alone —
/// `crates/vike-datahub/src/server.rs`'s `backfill_verb` now refuses the same family on the
/// Control-scoped sibling, for the forming-bar reason its doc argues. Two gates, one parser; this
/// one stays the narrower allowlist because an OBSERVE client names the string.
///
/// A STOPGAP. `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` says so in its
/// consequences, and the per-venue interval table is what replaces it.
pub const SEED_INTERVALS: [&str; 11] =
    ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d"];

/// The longest symbol a seed will carry.
///
/// ⚠ **The longest spelling moved when the collector table grew from three venues to six** (0059
/// Phase 2). It was OKX's `BTC-USDT-SWAP` at 13 bytes; deribit's option names are the new worst
/// case — `BTC-28MAR25-100000-C` is 20. Binance and bybit spell `BTCUSDT`, aster's perp suffix
/// makes `BTCUSDT.P` at 9, and a hyperliquid perp is a bare coin. 32 still clears the real worst
/// case with room, so **the constant does not move** — but the sentence that justified it had to,
/// and a seventh venue with longer names is what would reopen it.
///
/// ⚠ It is NOT [`crate::market::MD_MAX_SYMBOL_BYTES`], and must not become it: 96 is DERIVED there
/// from a control-frame ceiling a market-data memory assertion rests on, and a 78-digit polymarket
/// token id is what sets it. No polymarket token reaches a kline collector — that venue has no
/// collector in the table at all — so borrowing that bound would import a derivation this verb does
/// not participate in and would widen what reaches a URL by a factor of three for no venue that
/// exists.
pub const SEED_MAX_SYMBOL_BYTES: usize = 32;

/// The bytes a seed symbol may be built from: ASCII letters, digits, and the three separators the
/// table's venues actually spell (`-` for OKX's `BTC-USDT-SWAP` and deribit's `BTC-PERPETUAL`, `.`
/// for this workspace's `.P` perp suffix, `_` for symmetry with the store's own partition
/// vocabulary).
///
/// An ALLOWLIST rather than a deny-list, and that direction is the point: `klines_url` interpolates
/// this string into a query string unencoded, so the question that matters is not "which characters
/// are dangerous" (`&`, `#`, `?`, `%`, a raw newline, a space — the list is open) but "which are
/// known-safe". Everything outside this set is refused.
///
/// ⚠ **Two spellings the six-venue table can produce are outside it, and both refusals are
/// correct.** A hyperliquid SPOT pair is `BASE/QUOTE` (`HYPE/USDC`) and a HIP-3 perp is
/// `{dex}:{coin}` (`test:BTC`); neither `/` nor `:` is admitted here. The `/` case is the one that
/// matters — `vike_backfill::hyperliquid::identity_coin_for` refuses it at the collector for a
/// second, independent reason (the seam carries one symbol and cannot resolve a spot pair's
/// `@<pairIndex>` coin), so a seed for one is refused twice over rather than guessed at.
fn is_seed_symbol_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_')
}

/// The interval rule: membership in [`SEED_INTERVALS`], exact and case-sensitive.
///
/// Case-SENSITIVE deliberately. `1M` and `1m` are different intervals at every venue in the table
/// (a calendar month and a minute), so a case-insensitive match would silently turn a monthly
/// request into a minute one — and `1M` is not in the set, so the refusal is the correct answer
/// rather than a coercion.
///
/// The message names the whole permitted set, because an operator who sent `1s` needs to see that
/// the SET refused it rather than the venue.
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
/// ⚠ **The message never ECHOES the symbol back.** Same rule and same reason as
/// [`crate::market::validate_md_symbol`]: the field being refused is the untrusted one, a refusal
/// quoting it is that same untrusted string wearing a log line, and the file layer defaults to
/// `trace`. It names the LENGTH, the CAP and the OFFSET, which is what an operator can act on.
///
/// ⚠ **A symbol is NOT trimmed and NOT upper-cased.** The venue is sent the spelling verbatim, so
/// altering it here would make the store's partition and the venue's answer disagree about what was
/// asked for — the identical rule `validate_md_symbol` states for the subscription key.
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
             The longest spelling any venue in this server's collector table uses is 13 bytes \
             (`BTC-USDT-SWAP`).",
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
/// **The SERVER computes this**, from its own clock and its own constant — the request carries no
/// range at all, which is the term `docs/decisions/0057` turns on. Exposed here so a client can
/// report what it is about to be given rather than guess, and so the two ends cannot disagree about
/// what "600 bars" meant.
///
/// `None` for an interval outside [`SEED_INTERVALS`] or one `vike_model::time::interval_ms` cannot
/// parse — the two are the same set by construction, and the redundant second check is kept because
/// this function is the one that would otherwise multiply by a garbage width.
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

    #[test]
    fn every_permitted_interval_has_a_bar_width_the_store_can_parse() {
        // Rule 2 of `SEED_INTERVALS`' derivation, asserted rather than trusted: an entry the store
        // vocabulary cannot parse would have no window and no partition, and `seed_range` would
        // answer `None` for an interval `validate_seed_interval` had just accepted.
        for iv in SEED_INTERVALS {
            let ms = vike_model::time::interval_ms(iv);
            assert!(matches!(ms, Some(n) if n > 0), "{iv} has no positive bar width: {ms:?}");
            assert!(seed_range(iv, 1_700_000_000_000).is_some(), "{iv} has no window");
        }
    }

    #[test]
    fn the_permitted_set_is_sorted_by_bar_width_and_free_of_duplicates() {
        // Not cosmetic: the set is rendered into a refusal an operator reads, and a duplicate would
        // mean two rows of the derivation collapsed without anyone noticing.
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
