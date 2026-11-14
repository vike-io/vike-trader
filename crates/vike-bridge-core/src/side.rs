//! A venue's wire side WORD → the `i32` side sign (`+1` buy / `-1` sell) a recon report carries.
//!
//! The venues that spell a side as a `buy`/`sell` word do not agree on what an UNKNOWN word means,
//! and that default is a per-venue decision, so each venue picks one of the two families here:
//! [`sign_default_buy`] (`sell` → `-1`, everything else → `+1`) is the recon read of alpaca, IG and
//! polymarket, each the inverse of its own outbound `side >= 0 → buy` spelling;
//! [`sign_default_sell`] (`buy` → `+1`, everything else → `-1`) is the binance-family, bybit and okx
//! recon read. Hyperliquid and IBKR read their own words (`"A"`, a leading `B`); those stay with their
//! venues.

/// `sell` (ASCII case-insensitive) → `-1`; ANY other word, the empty string included, → `+1`.
/// No trim: `" sell"` is `+1`.
#[must_use]
pub fn sign_default_buy(word: &str) -> i32 {
    if word.eq_ignore_ascii_case("sell") { -1 } else { 1 }
}

/// `buy` (ASCII case-insensitive) → `+1`; ANY other word, the empty string included, → `-1`.
/// No trim: `" buy"` is `-1`.
#[must_use]
pub fn sign_default_sell(word: &str) -> i32 {
    if word.eq_ignore_ascii_case("buy") { 1 } else { -1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden outputs of the three copies this replaced (alpaca `recon_client.rs`, IG
    /// `recon_client.rs`, polymarket `exec_plane/recon_client.rs`, each a private `side_sign`).
    #[test]
    fn sell_is_minus_one_and_every_other_word_is_plus_one() {
        for (word, sign) in [
            ("sell", -1),
            ("SELL", -1),
            ("Sell", -1),
            ("sElL", -1),
            ("buy", 1),
            ("BUY", 1),
            ("", 1),      // the conservative default, never panics
            (" sell", 1), // no trim
            ("sell ", 1),
            ("s", 1),
            ("short", 1),
            ("A", 1),
            ("selling", 1),
        ] {
            assert_eq!(sign_default_buy(word), sign, "{word:?}");
        }
    }

    /// Golden outputs of the three copies this replaces (the binance family `family/recon.rs`, bybit
    /// and okx `recon_client.rs`, each a private `side_sign` comparing `BUY`, `Buy` and `buy` without
    /// case).
    #[test]
    fn buy_is_plus_one_and_every_other_word_is_minus_one() {
        for (word, sign) in [
            ("buy", 1),
            ("BUY", 1),
            ("Buy", 1),
            ("bUy", 1),
            ("sell", -1),
            ("SELL", -1),
            ("Sell", -1),
            ("", -1),     // the other default, never panics
            (" buy", -1), // no trim
            ("buy ", -1),
            ("b", -1),
            ("long", -1),
            ("B", -1),
            ("buying", -1),
        ] {
            assert_eq!(sign_default_sell(word), sign, "{word:?}");
        }
    }
}
