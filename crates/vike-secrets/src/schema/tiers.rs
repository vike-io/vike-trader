//! The tier vocabulary at the store's edge: the column conversions of `account.tier`, and the
//! stored word for a machine-scoped `venue_setting` row.
//!
//! **The tier itself is `vike_model::AccountTier`** (decision 0129) — `paper < demo < live`, its one
//! parser (`FromStr`: `paper`, `demo` or `live`, case-insensitive and trimmed) and its one spelling
//! (`as_str`). This crate owns no tier word of its own, and since decision 0156 item 6 no key-name
//! token maps onto one either: the tier is written in ONE place, `account.tier`. What stays here is
//! what only the store needs: a `rusqlite` conversion on each side of a `tier` column
//! (`vike-model` carries no `rusqlite`).

use vike_model::AccountTier;

/// **An `account.tier` value read out of a row** — the column's text through `AccountTier`'s one
/// parser. `idx` is the column, for the error.
///
/// The column's `CHECK (tier IN ('paper', 'demo', 'live'))` makes a refusal unreachable on a store
/// this crate wrote; a store that holds anything else is not on the shipped shape, and the read
/// FAILS (a `FromSqlConversionFailure` naming the word) rather than guessing a tier — a guessed tier
/// would be a guessed amount of real money.
pub(crate) fn tier_from_column(idx: usize, text: &str) -> rusqlite::Result<AccountTier> {
    text.parse::<AccountTier>().map_err(|refused| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            Box::new(refused),
        )
    })
}

/// **The `venue_setting.tier` word for a MACHINE-SCOPED row** — "applies to any tier", which SQL
/// NULL spelled until §5.2 step 7 of
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` (owner ruling 2: a NULL
/// may not decide what KIND of row this is).
///
/// ⚠ **It is a STORED word and never a tier.** `AccountTier`'s parser refuses it and an account can
/// never carry it (`account`'s `CHECK` refuses it), so a dotted key an operator types,
/// `venue.polymarket.any.proxy_host`, does NOT address the `(polymarket, None, PROXY_HOST)` row:
/// `crate::venue_setting::parse_venue_setting_key` keeps the `any` as part of the FIELD. Nothing on
/// the Rust side spells "no tier" as anything but `None`. The two functions below are the ONLY
/// places the word meets that `None`, one per direction, and every Rust value bound to or read from
/// `venue_setting.tier` goes through one of them.
pub(crate) const ANY_TIER: &str = "any";

/// **The WRITE boundary** — the value a `venue_setting.tier` column stores for a row whose tier is
/// `tier` (`None` for a machine-scoped row).
///
/// ⚠ Every writer binds THIS rather than the `Option` itself. Binding the `Option` wrote SQL NULL,
/// which the shipped `tier TEXT NOT NULL` refuses outright — the loud half of forgetting it. The
/// quiet half is a LOOKUP (`WHERE … tier IS ?`) that stays well-formed and simply finds nothing for
/// a row that plainly exists; `crate::settings::set_venue_setting_in`'s previous-value read is that
/// shape, and it binds this function too.
///
/// ⚠ It cannot be handed [`ANY_TIER`] as a tier: since decision 0129 the tier is an `AccountTier`, so
/// the `Some("any")` that once overwrote the machine-scoped row (and was refused here by hand) no
/// longer has a spelling.
pub(crate) fn stored_venue_setting_tier(tier: Option<AccountTier>) -> &'static str {
    tier.map_or(ANY_TIER, AccountTier::as_str)
}

/// **The READ boundary** — the tier a stored `venue_setting.tier` value means, `None` for a
/// machine-scoped row. `idx` is the column, for the error.
///
/// ⚠ **BOTH spellings of "no tier" read as `None`.** The shipped column is `NOT NULL`, so a NULL
/// reads as `None` for free rather than by design. The value that must never get PAST this function
/// is `'any'`: that is the trap step 7 was split off stage 4a for.
/// a renderer would spell `{HEAD}_ANY_{FIELD}` from an unmapped
/// `'any'`, a name no store holds, and every legacy machine-scoped row would go UNREACHABLE — not
/// wrong, invisible. `crates/vike-secrets/tests/store/venue_setting_any_tier.rs` proves it THROUGH
/// the public readers rather than through a `SELECT`, which is the only way it can be proved. Any
/// other word goes through `AccountTier`'s one parser, and a word it refuses fails the read.
pub(crate) fn venue_setting_tier_of_stored(
    idx: usize,
    stored: Option<String>,
) -> rusqlite::Result<Option<AccountTier>> {
    match stored.as_deref() {
        None | Some(ANY_TIER) => Ok(None),
        Some(word) => tier_from_column(idx, word).map(Some),
    }
}
