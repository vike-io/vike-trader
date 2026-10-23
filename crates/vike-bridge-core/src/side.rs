//! A venue's wire side WORD → the `i32` side sign (`+1` buy / `-1` sell) a recon report carries.
//!
//! The venues that spell a side as a `buy`/`sell` word do not agree on what an UNKNOWN word means,
//! and that default is a per-venue decision, so only the one family that shares it reads here:
//! [`sign_default_buy`] (`sell` → `-1`, everything else → `+1`) is the recon read of alpaca, IG and
//! polymarket, each the inverse of its own outbound `side >= 0 → buy` spelling. The binance-family,
//! bybit and okx recon clients default the OTHER way (`buy` → `+1`, everything else → `-1`), and
//! hyperliquid and IBKR read their own words (`"A"`, a leading `B`); those stay with their venues.

/// `sell` (ASCII case-insensitive) → `-1`; ANY other word, the empty string included, → `+1`.
/// No trim: `" sell"` is `+1`.
#[must_use]
pub fn sign_default_buy(word: &str) -> i32 {
    if word.eq_ignore_ascii_case("sell") { -1 } else { 1 }
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
}
