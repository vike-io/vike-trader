//! Pure Polymarket display-label helpers for the scalp cockpit (`WinKind::Polymarket`): the
//! token-id elision ([`poly_short_label`]) and the Gamma market → short readable name derivation
//! ([`poly_market_short_name`]). String in, string out — no I/O, no vike-polymarket dependency
//! (the feature-gated `GammaMarket`-typed selector `pick_updown_token` stays in `vike-app`, a thin
//! fat-only adapter over these). Moved down from `vike-app`'s CI-excluded `main.rs` (audit F4) so
//! the `poly_market_short_name` unit tests finally run in a gate.

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

/// Derive a SHORT, human-readable market name (≤ 18 chars) for the cockpit chain-rail + ladder
/// header from a Gamma market's `question` (preferred) and `slug` (fallback) — the readable label
/// that replaces the elided token-id. Polymarket's rolling crypto markets carry a DATED question
/// like `"Bitcoin Up or Down - July 25, 3PM ET"`; the date/time tail is noise for a header that
/// already shows a live countdown, so it is stripped and the verbose "Up or Down" phrase is
/// compacted to "Up/Down". Pure / no-I/O (kept ASCII on purpose — the ↑↓ glyphs would risk tofu in
/// egui's bundled fonts). Derivation:
///   1. base = trimmed `question`, else the de-slugified `slug` (dashes/underscores → spaces, each
///      word Title-Cased) so a sparse-question window still yields a name;
///   2. drop a trailing " - <when>" clause (Polymarket's "<Name> - <date>" shape);
///   3. if the head contains "up or down" / "up/down" (case-insensitive), render "<Asset> Up/Down"
///      keeping the asset's ORIGINAL casing; else keep the head verbatim;
///   4. hard-cap at 18 chars on a char boundary, trimming any truncation-edge whitespace.
///
/// Examples:
///
/// - `"Bitcoin Up or Down - July 25, 3PM ET"` → `"Bitcoin Up/Down"`
/// - `"Ethereum Up or Down on July 25?"` → `"Ethereum Up/Down"`
/// - slug `"solana-up-or-down-2026-07-25"` (empty question) → `"Solana Up/Down"`
/// - `"New Rihanna Album before GTA VI?"` → `"New Rihanna Album"` (generic, capped at 18)
pub fn poly_market_short_name(question: &str, slug: &str) -> String {
    // 1) base text: the question when present, else a de-slugified slug.
    let base: String = if !question.trim().is_empty() {
        question.trim().to_string()
    } else {
        slug.split(['-', '_'])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut ch = w.chars();
                match ch.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    // 2) strip a trailing " - <date/time>" clause.
    let head = base.split(" - ").next().unwrap_or(&base).trim();
    // 3) compact "<Asset> up or down …" → "<Asset> Up/Down". Match on the lowercased text but slice
    //    the ORIGINAL head (ASCII questions ⇒ byte offsets align; `get` degrades safely otherwise).
    let lower = head.to_lowercase();
    let compact = match lower.find("up or down").or_else(|| lower.find("up/down")) {
        Some(idx) => {
            let asset = head.get(..idx).unwrap_or("").trim().trim_end_matches([':', '-']).trim();
            if asset.is_empty() {
                "Up/Down".to_string()
            } else {
                format!("{asset} Up/Down")
            }
        }
        None => head.to_string(),
    };
    // 4) hard cap at 18 chars on a char boundary.
    compact.chars().take(18).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod poly_name_tests {
    use super::poly_market_short_name;

    #[test]
    fn strips_date_tail_and_compacts_up_or_down() {
        assert_eq!(
            poly_market_short_name("Bitcoin Up or Down - July 25, 3PM ET", ""),
            "Bitcoin Up/Down"
        );
        assert_eq!(
            poly_market_short_name("Ethereum Up or Down on July 25?", ""),
            "Ethereum Up/Down"
        );
    }

    #[test]
    fn falls_back_to_deslugified_slug_when_question_empty() {
        assert_eq!(
            poly_market_short_name("", "solana-up-or-down-2026-07-25-1200"),
            "Solana Up/Down"
        );
    }

    #[test]
    fn generic_market_is_capped_at_18_chars() {
        let out = poly_market_short_name("New Rihanna Album before GTA VI?", "");
        assert!(out.chars().count() <= 18, "got {out:?}");
        assert_eq!(out, "New Rihanna Album");
    }

    #[test]
    fn empty_inputs_do_not_panic() {
        assert_eq!(poly_market_short_name("", ""), "");
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
