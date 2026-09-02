//! `credential_keys` — every credential/attribution key name this workspace can ever look up, as
//! DATA rather than as a format string.
//!
//! # The blind spot this closes
//!
//! `vike_bridge_core::credentials::load_credentials_from` builds each key with a `format!` over a
//! `{VENUE}_{TIER}` prefix and then asks a caller-supplied map for it; `attribution_code_from` does
//! the same with `{VENUE}_BROKER_CODE` / `{VENUE}_BUILDER_CODE`. The settings registry's scanner
//! (`crates/vike-ops/src/scan.rs`) resolves string LITERALS and `const`s, so a key that never
//! appears as a literal anywhere had no `vike_ops::settings::SETTINGS` row **and no sighting** —
//! undetectably. `OKX_LIVE_API_SECRET` was read on every credential probe and was in neither place,
//! while its `OKX_DEMO_API_SECRET` sibling had a row only because a test fixture happened to spell
//! it; `BYBIT_BROKER_CODE` and three siblings had no fixture and so had no row at all. That is not
//! a gap `crates/vike-ops/tests/settings_registry.rs`'s `DYNAMIC_ALLOWLIST` can cover: it
//! allowlists a call site the scanner FOUND and could not resolve, and a computed map `get` is
//! never recognised as a candidate site to begin with.
//!
//! # The shape of the fix — the roster precedent
//!
//! One const table, plus completeness tests that iterate it. [`crate::venues::VENUES`] is the
//! exemplar every per-venue capability table already follows, and this module is the same move for
//! the key NAMES: [`CREDENTIAL_SUFFIXES`], [`ATTRIBUTION_SUFFIXES`], [`CREDENTIAL_TIERS`] and
//! [`LEGACY_CREDENTIAL_TIERS`] are the only place those spellings exist, and [`credential_keys`] /
//! [`attribution_keys`] fold them over the roster into the whole enumerable grid.
//!
//! Three gates hold the two sides together, each comparing evidence of a DIFFERENT provenance:
//!
//! - `crates/vike-bridge-core/src/credentials.rs`'s
//!   `the_loader_reads_exactly_the_enumerated_credential_grid` folds the REAL loader's own
//!   `names_for_prefix` over the roster and every [`crate::venues::VENUES`] tier, and asserts the
//!   resulting name set IS [`credential_keys`] — so the enumeration cannot drift from the read.
//! - its `the_attribution_grid_is_exactly_what_the_reader_looks_up` twin drives the real
//!   `attribution_code_from` with a map holding one enumerated key at a time, and asserts BOTH
//!   ways: a mechanised venue's keys are enumerated and read, an unmechanised venue's are neither.
//! - `crates/vike-ops/tests/settings_registry.rs`'s `every_generated_key_is_declared` demands a
//!   registry row for every key this module can produce.
//!
//! So adding a venue to [`crate::venues::VENUES`], a tier here, or a suffix here reddens the
//! registry until the rows exist — which is exactly what the roster contract promises for every
//! other per-venue table.
//!
//! # Key NAMES only
//!
//! Nothing here reads, holds, forwards or logs a credential VALUE: every function returns a
//! `String` that is a variable NAME, built from the caller's venue id and this module's own
//! constants. A credential never enters this file.
//!
//! # What the grid deliberately does NOT cover
//!
//! The BESPOKE per-venue shapes — `FXCM_{TIER}_USER`/`_PASSWORD`, `DUKASCOPY_DEMO1_LOGIN`,
//! `OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, `ASTER_{TIER}_PRIVATE_KEY`,
//! `HYPERLIQUID_{TIER}_PRIVATE_KEY`, the `POLY_*` L2 trio — are read by each bridge's own
//! `config.rs` loader with LITERAL keys, so the scanner has always seen them and they have always
//! had rows. This table is only for the family that is COMPUTED, which is the only family the
//! scanner is structurally blind to.
//!
//! ⚠ **The grid is an OVER-approximation over the roster, on purpose.**
//! `load_credentials_from` is generic over the venue string, and WHICH venues reach it is a `match`
//! arm rather than data: `vike_connections::status`'s `venue_env_configured` routes the venues whose
//! loaders read a bespoke key shape to those loaders, and everything else to the generic one. ⚠ That
//! set is deliberately not counted here — it was written as "six venues" and was wrong within the
//! month, when the four venues whose status had been read from a grid they never used grew arms of
//! their own. The `match` in `venue_env_configured` is the authority. So a roster venue's keys are
//! declared even where nothing asks for them today — the same rule the capability tables follow,
//! where a roster venue's row is always NAMED even when its value equals the fallback. The
//! alternative is a hand-kept "these venues use the generic loader" list, which is precisely the
//! copy that rots.

use crate::attribution::attribution_for;
use crate::venues::VENUES;

/// The API-key suffix appended to a `{VENUE}_{TIER}` prefix.
pub const API_KEY_SUFFIX: &str = "_API_KEY";
/// The API-secret suffix appended to a `{VENUE}_{TIER}` prefix.
pub const API_SECRET_SUFFIX: &str = "_API_SECRET";
/// The API-passphrase suffix appended to a `{VENUE}_{TIER}` prefix. Required only where
/// `vike_bridge_core::venue_passphrase::venue_passphrase` says so, but LOOKED UP for every venue —
/// which is what puts it in the grid.
pub const API_PASSPHRASE_SUFFIX: &str = "_API_PASSPHRASE";

/// The three suffixes one `{VENUE}_{TIER}` prefix yields, in the order
/// `vike_bridge_core::credentials`' `names_for_prefix` returns them.
pub const CREDENTIAL_SUFFIXES: [&str; 3] =
    [API_KEY_SUFFIX, API_SECRET_SUFFIX, API_PASSPHRASE_SUFFIX];

/// The broker-code attribution suffix — tried FIRST by `attribution_code_from`.
pub const BROKER_CODE_SUFFIX: &str = "_BROKER_CODE";
/// The builder-code attribution suffix — the fallback `attribution_code_from` tries second.
pub const BUILDER_CODE_SUFFIX: &str = "_BUILDER_CODE";

/// Both attribution suffixes, in the order `attribution_code_from` tries them.
pub const ATTRIBUTION_SUFFIXES: [&str; 2] = [BROKER_CODE_SUFFIX, BUILDER_CODE_SUFFIX];

/// The environment tiers, spelled exactly as `vike_bridge_core::credentials::Environment::as_str`
/// spells them. Pinned equal to that enum by
/// `crates/vike-bridge-core/src/credentials.rs`'s `the_environment_tiers_are_the_shared_table` —
/// neither crate can be the sole authority here (this one cannot see the enum, and the enum's crate
/// must not re-spell the grid), so the two are held equal by a gate.
pub const CREDENTIAL_TIERS: [&str; 3] = ["SIM", "DEMO", "LIVE"];

/// The LEGACY tier spellings still accepted as a fallback — pre-rename credential files wrote
/// `MAINNET` where `LIVE` is written now, and `Environment::legacy_str` still tries it, so those
/// keys are genuinely looked up and genuinely belong in the grid.
pub const LEGACY_CREDENTIAL_TIERS: [&str; 1] = ["MAINNET"];

/// One credential key: `{VENUE}_{TIER}{SUFFIX}`, e.g. `BINANCE_DEMO_API_KEY`.
///
/// `venue` is a canonical lowercase roster id ([`crate::venues::VENUES`]); the uppercasing is the
/// loader's own (`load_credentials_from` builds its prefix the same way), so this and the read can
/// only agree.
#[must_use]
pub fn credential_key(venue: &str, tier: &str, suffix: &str) -> String {
    format!("{}_{tier}{suffix}", venue.to_uppercase())
}

/// One attribution key: `{VENUE}{SUFFIX}`, e.g. `OKX_BROKER_CODE`.
///
/// ⚠ `attribution_code_from` used to uppercase with `to_ascii_uppercase` here while the credential
/// loader used `to_uppercase`; both call this now. The difference is unreachable, not merely
/// unlikely — that reader returns `None` for a venue with no [`crate::attribution`] mechanic
/// BEFORE it builds a key, and every mechanic arm is matched on a lowercase ASCII roster id, so no
/// string whose two uppercasings differ can reach this function through it. This file's
/// `the_two_uppercasings_agree_on_every_roster_venue` pins that for the roster.
#[must_use]
pub fn attribution_key(venue: &str, suffix: &str) -> String {
    format!("{}{suffix}", venue.to_uppercase())
}

/// The WHOLE credential grid: every roster venue × every tier (current AND legacy) × every
/// suffix, sorted and deduplicated.
///
/// Allocates, and is meant to: the callers are the registry gate and the loader's own equivalence
/// test, never a hot path. `load_credentials_from` builds the two or three names it needs and does
/// not walk this.
#[must_use]
pub fn credential_keys() -> Vec<String> {
    let mut out: Vec<String> = VENUES
        .iter()
        .flat_map(|venue| {
            CREDENTIAL_TIERS.iter().chain(LEGACY_CREDENTIAL_TIERS.iter()).flat_map(move |tier| {
                CREDENTIAL_SUFFIXES.iter().map(move |sfx| credential_key(venue, tier, sfx))
            })
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every attribution key that can be looked up, sorted and deduplicated.
///
/// Only venues with an order-level [`crate::attribution::AttributionMechanic`] appear: a venue
/// classified `None` makes `attribution_code_from` return before any key is built, so declaring one
/// would claim a read that provably cannot happen. That is the one place this module is NARROWER
/// than the roster, and it is derived from the capability table rather than hand-listed.
#[must_use]
pub fn attribution_keys() -> Vec<String> {
    let mut out: Vec<String> = VENUES
        .iter()
        .filter(|venue| !attribution_for(venue).is_none())
        .flat_map(|venue| ATTRIBUTION_SUFFIXES.iter().map(move |sfx| attribution_key(venue, sfx)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// [`credential_keys`] ∪ [`attribution_keys`] — the whole set of names a computed map `get` in this
/// workspace can ask for, sorted and deduplicated. The registry gate's input.
#[must_use]
pub fn lookup_keys() -> Vec<String> {
    let mut out = credential_keys();
    out.extend(attribution_keys());
    out.sort();
    out.dedup();
    out
}

/// The keys a FIRST store should carry for ONE venue, in the order a human wants to read them:
/// every current tier × every credential suffix, then the attribution keys this venue's mechanic
/// can actually produce.
///
/// ⚠ **Current tiers ONLY — the legacy `MAINNET` spelling is deliberately excluded**, unlike
/// [`credential_keys`]. That grid exists so the registry gate can prove every key a computed lookup
/// might ASK for is declared, and the loader genuinely still reads `MAINNET`; this one exists to be
/// shown to a person writing their first store, and teaching them a spelling we are trying to
/// retire would be a different kind of wrong. The two are allowed to disagree for that reason and
/// no other.
///
/// ⚠ **This lives HERE rather than in the CLI that prints it, and that placement is load-bearing.**
/// `crates/vike-ops/tests/settings_registry.rs`'s `generated_key_sites` treats any file CALLING
/// [`credential_key`] as a site whose crate must then declare the whole grid in `SETTINGS` — the
/// gate's meaning is *this crate READS these variables*. A command that merely prints key NAMES
/// reads none of them, so composing them at the call site would have made the registry assert
/// something false about `vike-cli`. This module is the table's own definition and is excluded from
/// that set by construction, so the composition belongs here and the caller just renders.
#[must_use]
pub fn starter_keys(venue: &str) -> Vec<String> {
    let mut out: Vec<String> = CREDENTIAL_TIERS
        .iter()
        .flat_map(|tier| {
            CREDENTIAL_SUFFIXES.iter().map(move |sfx| credential_key(venue, tier, sfx))
        })
        .collect();
    // Only venues with an order-level mechanic produce attribution keys; reuse that classification
    // rather than restating it, exactly as `attribution_keys` does.
    if !attribution_for(venue).is_none() {
        out.extend(ATTRIBUTION_SUFFIXES.iter().map(|sfx| attribution_key(venue, sfx)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid is exactly the product it claims to be — DERIVED from the roster's length, never a
    /// pinned count. A count checked against itself is the failure
    /// `crate::venues`' `roster_matches_the_bridge_crates` was rewritten to stop shipping.
    #[test]
    fn the_credential_grid_is_the_roster_times_the_tiers_times_the_suffixes() {
        let tiers = CREDENTIAL_TIERS.len() + LEGACY_CREDENTIAL_TIERS.len();
        assert_eq!(credential_keys().len(), VENUES.len() * tiers * CREDENTIAL_SUFFIXES.len());
    }

    /// …and the attribution half is the MECHANIC venues times the suffixes, with the roster's
    /// unmechanised venues genuinely absent (not merely unlisted).
    #[test]
    fn the_attribution_grid_covers_exactly_the_mechanised_venues() {
        let mechanised = VENUES.iter().filter(|v| !attribution_for(v).is_none()).count();
        assert_eq!(attribution_keys().len(), mechanised * ATTRIBUTION_SUFFIXES.len());
        assert!(mechanised > 0 && mechanised < VENUES.len(), "both arms must be non-empty");
        let keys = attribution_keys();
        for venue in VENUES.iter().filter(|v| attribution_for(v).is_none()) {
            for sfx in ATTRIBUTION_SUFFIXES {
                let key = attribution_key(venue, sfx);
                assert!(!keys.contains(&key), "{key} is never looked up — it must not be declared");
            }
        }
    }

    /// The SPELLING, pinned against a name written out in full. Everything else in this file
    /// composes the same constants the code under test composes, which would keep agreeing after a
    /// change to either; this one assertion is the anchor that says WHICH strings those are.
    ///
    /// The literal is a declared grid key, so it is safe for
    /// `crates/vike-ops/tests/settings_registry.rs`'s literal sweep to observe. Do not spell a name
    /// here that the grid does not contain — an env-shaped literal with no registry row fails that
    /// gate's direction 1, wherever in the tree it sits.
    #[test]
    fn a_credential_key_is_venue_then_tier_then_suffix() {
        assert_eq!(credential_key("binance", "DEMO", API_KEY_SUFFIX), "BINANCE_DEMO_API_KEY");
        assert_eq!(attribution_key("okx", BROKER_CODE_SUFFIX), "OKX_BROKER_CODE");
    }

    /// Every key is uppercase/underscore/digit shaped and carries one of the declared suffixes —
    /// the property the registry's own name predicate (`vike_ops::scan`'s `is_env_name`) demands
    /// before it will even consider a string an environment variable.
    #[test]
    fn every_key_is_env_shaped_and_carries_a_declared_suffix() {
        for key in lookup_keys() {
            assert!(
                key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "{key} is not env-var shaped"
            );
            assert!(
                CREDENTIAL_SUFFIXES
                    .iter()
                    .chain(ATTRIBUTION_SUFFIXES.iter())
                    .any(|s| key.ends_with(s)),
                "{key} ends in no declared suffix"
            );
        }
    }

    /// Sorted and deduplicated, so a gate diffing this against a committed table prints one line
    /// per change rather than a reordering.
    #[test]
    fn the_grid_is_sorted_and_unique() {
        for grid in [credential_keys(), attribution_keys(), lookup_keys()] {
            let mut sorted = grid.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(grid, sorted);
        }
    }

    /// The two uppercasings the workspace used before this table existed agree on every roster id,
    /// which is what makes [`attribution_key`]'s switch from `to_ascii_uppercase` a no-op.
    #[test]
    fn the_two_uppercasings_agree_on_every_roster_venue() {
        for venue in VENUES {
            assert_eq!(venue.to_uppercase(), venue.to_ascii_uppercase(), "{venue}");
        }
    }
}
