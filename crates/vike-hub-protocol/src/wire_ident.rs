//! The spelling rules of the two untrusted names the hub protocols carry, a venue slug and a symbol:
//! length, charset, and a refusal that names what was wrong. Here because both hub clients refuse by
//! them and neither may name the other (`vike-datahub-client` for the chart seed and the catalog,
//! `vike-backtesthub-client` for the named run, decision 0064's bound 4), so the rule lives BELOW both.
//! It holds no roster and knows no venue: bytes and a length, nothing else. The roster's own test is
//! `crates/vike-datahub-client/src/catalog.rs`'s `every_canonical_roster_slug_is_accepted`.
//!
//! The error text says "a seed symbol" and "a catalog venue" because those are the verbs the rules were
//! written for; a named run has always been refused in those words.

/// The longest symbol a seed will carry. The worst real case is deribit's option names
/// (`BTC-28MAR25-100000-C`, 20 bytes); 32 clears it with room, and a venue with longer names would
/// reopen it.
///
/// ⚠ NOT `vike_datahub_client::market::MARKET_DATA_MAX_SYMBOL_BYTES`, and must not become it: that 96 is DERIVED
/// from a market-data control-frame ceiling and a 78-digit polymarket token id, and no polymarket token
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
/// HIP-3 perp (`test:BTC`). `vike_hyperliquid::history::klines::identity_coin_for` refuses the `/` case at
/// the collector for a second, independent reason, so it is refused twice over rather than guessed
/// at.
fn is_seed_symbol_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_')
}

/// The symbol rules, cheapest first, each naming what was wrong.
///
/// ⚠ **The message never ECHOES the symbol** (`vike_datahub_client::market::validate_market_data_symbol`'s rule):
/// it names the LENGTH, the CAP and the OFFSET, never the untrusted string, which would ride into a
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

/// The longest venue slug this verb will carry: `hyperliquid` (11 bytes) is the roster's longest,
/// and 16 is the next power of two. It bounds the ALLOCATION a hostile frame can ask for; the
/// charset and the server's own table lookup are the real bound.
pub const CATALOG_MAX_VENUE_BYTES: usize = 16;

/// The bytes a venue slug may be built from: ASCII lowercase letters and digits only — narrower
/// than the seed symbol's, because every `vike_model::VENUES` entry is lowercase alphanumeric. The
/// value never reaches a URL (it selects a row in the server's own table), so this is a cheap early
/// refusal rather than a security boundary; still an ALLOWLIST, because the dangerous set is open.
fn is_catalog_venue_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// The venue rules, cheapest first, each naming what was wrong.
///
/// ⚠ **The message never ECHOES the venue** ([`validate_seed_symbol`]'s rule): it names the LENGTH, the
/// CAP and the OFFSET, never the untrusted string, which would ride into a log whose file layer
/// defaults to `trace`.
///
/// ⚠ **Not lower-cased or trimmed:** the server looks the slug up by exact match, so coercing it
/// would make the request and the answer disagree about which venue was asked for.
pub fn validate_catalog_venue(venue: &str) -> Result<(), String> {
    if venue.is_empty() {
        return Err(
            "a catalog venue is EMPTY, so it names no venue. Pass the roster slug (`binance`, \
             `okx`, `polymarket`)."
                .to_string(),
        );
    }
    if venue.len() > CATALOG_MAX_VENUE_BYTES {
        return Err(format!(
            "a catalog venue of {} bytes exceeds CATALOG_MAX_VENUE_BYTES = \
             {CATALOG_MAX_VENUE_BYTES}. The longest slug on the canonical roster is \
             `hyperliquid` at 11 bytes.",
            venue.len()
        ));
    }
    if let Some(at) = venue.bytes().position(|b| !is_catalog_venue_byte(b)) {
        return Err(format!(
            "a catalog venue carries a byte outside the permitted set at offset {at}. A venue slug \
             may hold ASCII lowercase letters and digits only — every entry on the canonical \
             `vike_model::VENUES` roster does, so anything else names no venue this server could \
             serve."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_metacharacter_or_an_upper_case_slug_is_refused_and_never_echoed() {
        for bad in ["BINANCE", "bin ance", "binance/../x", "bin-ance", "bin_ance", "binance\n"] {
            let err = validate_catalog_venue(bad).expect_err("must be refused");
            assert!(err.contains("outside the permitted set"), "{bad}: {err}");
            assert!(!err.contains(bad), "the refusal echoed the venue back: {err}");
        }
    }

    #[test]
    fn an_empty_or_over_long_venue_is_refused_by_name() {
        assert!(validate_catalog_venue("").unwrap_err().contains("EMPTY"));
        let over = "x".repeat(CATALOG_MAX_VENUE_BYTES + 1);
        let err = validate_catalog_venue(&over).expect_err("over-long must be refused");
        assert!(err.contains("CATALOG_MAX_VENUE_BYTES"), "{err}");
        assert!(!err.contains(&over), "the refusal echoed the venue back");
    }
}
