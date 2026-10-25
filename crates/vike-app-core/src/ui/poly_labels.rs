//! Pure Polymarket display-label helper for the scalp cockpit (`WinKind::Polymarket`): the
//! token-id elision ([`poly_short_label`]). String in, string out — no I/O, no vike-polymarket
//! dependency. ⚠ The Gamma market → short-name derivation (`poly_market_short_name`) that stood
//! beside it was deleted with its only caller, `pick_updown_token`, which went with the `fat` build
//! (its tombstone is in `crates/vike-desktop/src/main.rs`): the desktop opens no venue socket and
//! makes no Gamma call, so there is no market name to derive.

/// A short, human-readable label for a Polymarket YES token-id. The raw id is a 78-digit uint256,
/// which is unreadable as the chain-rail / ladder header, so elide it to `first6…last4` (e.g.
/// `107156…2933`). Already-short ids (the placeholder) pass through verbatim. Purely cosmetic — the
/// full token-id stays the routing key everywhere else.
pub fn poly_short_label(token: &str) -> String {
    if token.len() <= 12 {
        token.to_string()
    } else {
        format!("{}…{}", &token[..6], &token[token.len() - 4..])
    }
}

#[cfg(test)]
mod short_label_tests {
    use super::poly_short_label;

    #[test]
    fn elides_long_token_ids_and_passes_short_ones_through() {
        // 78-digit uint256-style id → first6…last4
        let long = "1071563829465012877364528374650192837465019283746501928374650192837465012933";
        assert_eq!(poly_short_label(long), "107156…2933");
        // the placeholder / short ids pass through verbatim
        assert_eq!(poly_short_label("POLY-DEMO"), "POLY-DEMO");
        assert_eq!(poly_short_label(""), "");
    }
}
