//! The tier vocabulary at the store's edge: the `SIM`/`DEMO`/`LIVE` key-token map, the column
//! conversions of `account.tier`, and the stored word for a machine-scoped `venue_setting` row.
//!
//! **The tier itself is `vike_model::VenueMode`** (decision 0129) — `paper < demo < live`, its one
//! parser (`FromStr`: `paper`, `demo` or `live`, case-insensitive and trimmed) and its one spelling
//! (`as_str`). This crate owns no tier word of its own any more: `ACCOUNT_TIERS` became
//! `VenueMode::ALL`, `PAPER_TIER` became `VenueMode::Paper`, and `account_tier_named`, which also
//! took the pre-rename `sim`, became that parser — `sim` is no tier input anywhere
//! (decision 0129; there are no legacy fallbacks, decision 0117). What stays here is what only the
//! store needs: a `rusqlite` conversion on each side of a `tier` column (`vike-model` carries no
//! `rusqlite`), and the map between a credential KEY's tier token and a tier.

#[cfg(doc)]
use super::*;

use vike_model::VenueMode;

/// **The account tier a credential key's TIER TOKEN names** — `SIM` → `paper`, `DEMO` → `demo`,
/// `LIVE` → `live`; `None` for anything else.
///
/// # ⚠ The KEY TOKEN is a different alphabet, and it must not be "finished"
///
/// `vike_model::credential_keys::CREDENTIAL_TIERS` is still `["SIM", "DEMO", "LIVE"]`, because
/// those are the words an OPERATOR TYPED into a credential key name: renaming them would make every
/// `{VENUE}_SIM_*` key on an existing box unreadable, which is a data migration wearing a rename.
/// So the `SIM` token MAPS onto the `paper` tier, here and only here — the token is NEVER handed to
/// `VenueMode`'s parser (which refuses `sim` and every uppercase word). [`key_token_of_account_tier`]
/// is the inverse, for the renderer that has to go back.
///
/// `token` arrives as `vike_model::accounts::account_keys::AccountRef::tier` parsed it — one of the
/// three `CREDENTIAL_TIERS` spellings, exactly. Aster's `TESTNET` and dukascopy's `DEMO1` are the
/// two live families whose token is none of them; both are classified by
/// `crate::venue_setting::HAND_MAPPED_ACCOUNTS`, which states their tier as a `VenueMode`, before
/// the grammar is consulted, so neither reaches this map.
#[must_use]
pub fn account_tier_of_key_token(token: &str) -> Option<VenueMode> {
    VenueMode::ALL.into_iter().find(|tier| key_token_of_account_tier(*tier) == token)
}

/// **The credential-key TIER TOKEN an account tier is spelled with** — the inverse of
/// [`account_tier_of_key_token`], and the reason it has to exist.
///
/// [`crate::venue_setting::venue_setting_names`] composes a LEGACY CREDENTIAL NAME out of a
/// `(venue, tier, field)` row — `{HEAD}_{TOKEN}_{FIELD}` — so it needs the token, not the tier. A
/// renderer that uppercased the tier word instead would emit `{HEAD}_PAPER_{FIELD}`, a name no
/// store has ever held, and the row it was rendering would become UNREACHABLE rather than wrong:
/// nothing errors, the old key simply stops answering. That is the silent half of the `sim` ->
/// `paper` rename and the reason the map is a pair rather than a single direction.
#[must_use]
pub fn key_token_of_account_tier(tier: VenueMode) -> &'static str {
    match tier {
        VenueMode::Paper => SIM_KEY_TOKEN,
        VenueMode::Demo => "DEMO",
        VenueMode::Live => "LIVE",
    }
}

/// The credential-key tier token [`VenueMode::Paper`] is spelled with.
///
/// ⚠ Spelled out rather than taken as `vike_model::credential_keys::CREDENTIAL_TIERS[0]` — the
/// index would resolve, but it would read as *the first tier*, which is not what this is. The two
/// are held equal by `schema::tests::the_paper_tier_is_spelled_sim_in_a_credential_key` instead,
/// which asserts MEMBERSHIP rather than position, so reordering that table cannot silently re-point
/// this constant at `DEMO`.
pub const SIM_KEY_TOKEN: &str = "SIM";

/// **An `account.tier` value read out of a row** — the column's text through `VenueMode`'s one
/// parser. `idx` is the column, for the error.
///
/// The column's `CHECK (tier IN ('paper', 'demo', 'live'))` makes a refusal unreachable on a store
/// this crate wrote; a store that holds anything else is not on the shipped shape, and the read
/// FAILS (a `FromSqlConversionFailure` naming the word) rather than guessing a tier — a guessed tier
/// would be a guessed amount of real money.
pub(crate) fn tier_from_column(idx: usize, text: &str) -> rusqlite::Result<VenueMode> {
    text.parse::<VenueMode>().map_err(|refused| {
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
/// ⚠ **It is a STORED word and never a tier.** `VenueMode`'s parser refuses it and an account can
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
/// ⚠ It cannot be handed [`ANY_TIER`] as a tier: since decision 0129 the tier is a `VenueMode`, so
/// the `Some("any")` that once overwrote the machine-scoped row (and was refused here by hand) no
/// longer has a spelling.
pub(crate) fn stored_venue_setting_tier(tier: Option<VenueMode>) -> &'static str {
    tier.map_or(ANY_TIER, VenueMode::as_str)
}

/// **The READ boundary** — the tier a stored `venue_setting.tier` value means, `None` for a
/// machine-scoped row. `idx` is the column, for the error.
///
/// ⚠ **BOTH spellings of "no tier" read as `None`.** The shipped column is `NOT NULL`, so a NULL
/// reads as `None` for free rather than by design. The value that must never get PAST this function
/// is `'any'`: that is the trap step 7 was split off stage 4a for.
/// `crate::venue_setting::venue_setting_names` would render `{HEAD}_ANY_{FIELD}` from an unmapped
/// `'any'`, a name no store holds, and every legacy machine-scoped row would go UNREACHABLE — not
/// wrong, invisible. `crates/vike-secrets/tests/store/venue_setting_any_tier.rs` proves it THROUGH
/// the public readers rather than through a `SELECT`, which is the only way it can be proved. Any
/// other word goes through `VenueMode`'s one parser, and a word it refuses fails the read.
pub(crate) fn venue_setting_tier_of_stored(
    idx: usize,
    stored: Option<String>,
) -> rusqlite::Result<Option<VenueMode>> {
    match stored.as_deref() {
        None | Some(ANY_TIER) => Ok(None),
        Some(word) => tier_from_column(idx, word).map(Some),
    }
}
