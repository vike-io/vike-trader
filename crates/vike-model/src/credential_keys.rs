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

/// **The PLATFORM keys — names this store holds that belong to no venue at all.**
///
/// The two `vike-tradehub` node keys: the observe (read) HMAC key and the control (write) one, in
/// the order a reader wants them. They live in the same `<project>/settings/secrets.env` as every
/// venue credential and are read from it by a completely different loader
/// (`vike_tradehub_client::auth`'s `from_vars`, which the daemon calls through
/// `start_observe_server`), so the grid above has never covered them and must not start.
///
/// # What this table is FOR, and the three unions it deliberately is not part of
///
/// It exists so a WRITER can validate the names it is about to write against a fixed enumeration —
/// `crates/vike-cli/src/cmd/node/setup.rs` mints both keys and asks this table what to call them,
/// which is the answer `crates/vike-ops/tests/credential_writer_gate.rs`'s third `GROWTH_GUIDANCE`
/// question demands of every credential writer. It is a NAME table and nothing else: no value, no
/// tier, no venue, and no read.
///
/// - **NOT unioned into [`credential_keys`].** That function is asserted by
///   `crates/vike-bridge-core/src/credentials.rs`'s
///   `the_loader_reads_exactly_the_enumerated_credential_grid` to BE what `load_credentials_from`
///   reads, folded over the roster. These names are read by a different loader entirely, so
///   unioning them would make that gate assert something false.
/// - **NOT unioned into [`lookup_keys`], and therefore not into [`key_owner`].** `key_owner` is
///   `vike-cli secrets set`'s membership test, and that verb still REFUSES both names — the measured
///   pain was never "I could not type my key", it was "I had to invent one and nothing told me how".
///   A verb that accepts a hand-typed 256-bit HMAC key preserves the invention step, preserves the
///   copy-paste step, and adds a way to paste a truncated key that fails as an opaque auth denial.
///   Generating is the fix; accepting is not. `credential_keys.rs`'s own
///   `key_owner_classifies_exactly_the_lookup_grid` still lists the control key among its
///   `outsiders`, and that assertion is deliberately unchanged.
/// - **NOT unioned into [`starter_keys`].** That function is per-venue and exists to be shown to a
///   human writing their first venue store; there is no venue to show these under, and a store
///   template offering a key nobody should type by hand is the opposite of the point.
///
/// # ⚠ These spellings are a DUPLICATION, and the duplication is paid for
///
/// `vike_tradehub_client::auth`'s `OBSERVE_KEY_ENV` / `CONTROL_KEY_ENV` are the REFERENCE spelling —
/// that crate's doc carries the table of every copy and warns that a copy without an equality
/// assertion re-opens the gap it exists to close. This is such a copy, and its assertion is
/// `crates/vike-cli/tests/node_cli.rs`'s `the_platform_key_table_is_the_servers_own_spelling`,
/// which lives there because `vike-cli` is the lowest crate that can see BOTH this table (layer 10)
/// and the tradehub client (layer 50) — this crate cannot see that one, and must not.
///
/// ⚠ **`concat!` rather than whole literals, and it is load-bearing.**
/// `crates/vike-ops/tests/settings_registry.rs`'s literal harvest reads any string literal with
/// env-var shape and a known prefix as evidence that the containing crate READS that variable, and
/// then demands a `vike_ops::settings::SETTINGS` row for it. `vike-model` reads neither of these
/// names — it only names them — so a whole spelling here would make the registry assert something
/// false about this crate. Splitting the prefix off leaves two fragments neither of which is
/// env-shaped (one has no known prefix, the other does not begin with an uppercase letter) while the
/// compiled constant is byte-identical. It is the same move
/// `key_owner_classifies_exactly_the_lookup_grid` already makes with `format!` for its fixture.
/// ⚠ **FOUR names, not two, and the two that arrived late are the interesting half.** This table
/// held the TRADEHUB pair alone until 2026-09-08, while `vike-datahub` had grown an identical pair
/// of its own — same shape, same scopes, same HMAC handshake. Two consequences, both measured:
/// `vike-cli secrets set VIKE_DATAHUB_OBSERVE_KEY` fell through to the generic "edit it in by hand"
/// refusal instead of naming a command, and NOTHING in this tree could mint that pair, so the
/// datahub deployed on 2026-09-08 had its keys generated with `openssl` at a shell.
///
/// ORDER IS LOAD-BEARING and the tradehub pair stays first:
/// `crates/vike-cli/tests/node_cli.rs`'s `the_platform_key_table_is_the_servers_own_spelling`
/// asserts `[0]`/`[1]` against `vike_tradehub_client::auth`'s constants BY INDEX. The datahub pair
/// is APPENDED, and pays the same equality assertion in that file against
/// `vike_datahub_client::node_auth`'s own spellings — the duplication rule below applies to it
/// identically.
pub const PLATFORM_KEYS: [&str; 4] = [
    concat!("VIKE", "_TRADEHUB_OBSERVE_KEY"),
    concat!("VIKE", "_TRADEHUB_CONTROL_KEY"),
    concat!("VIKE", "_DATAHUB_OBSERVE_KEY"),
    concat!("VIKE", "_DATAHUB_CONTROL_KEY"),
];

/// Is `key` one of the [`PLATFORM_KEYS`]? The membership half of that table, so a caller asks a
/// question rather than reaching into an array.
///
/// ⚠ **This is NOT a second [`key_owner`], and must never be folded into one.** `key_owner` answers
/// *which venue and tier owns this name*, and its totality over [`lookup_keys`] — in BOTH directions
/// — is what `vike-cli secrets set`'s refusal rests on. This answers a disjoint question about a
/// disjoint name set, and the two tables' emptiness of intersection is asserted by
/// `platform_keys_are_outside_the_venue_grid` below.
#[must_use]
pub fn is_platform_key(key: &str) -> bool {
    PLATFORM_KEYS.contains(&key)
}

/// WHICH service's node keys `key` belongs to, as that service's binary name.
///
/// ⚠ **A caller that routes an operator somewhere needs this, and [`is_platform_key`] cannot give
/// it.** `vike-cli secrets set`'s refusal names the COMMAND that owns the key it is refusing, and
/// while there was one pair that command was a constant. With two services there are two commands,
/// and a refusal that named the wrong one would be the failure class that arm was built to end —
/// correct about the refusal, wrong about the route.
///
/// Returns the BINARY name rather than a bespoke enum on purpose: every caller is composing a
/// sentence for a human or picking a verb prefix, both of which want the name the operator already
/// knows, and an enum here would be a second vocabulary for a fact the string already carries.
/// `None` for anything outside the table, so this is safe to call before [`is_platform_key`].
#[must_use]
pub fn platform_key_service(key: &str) -> Option<&'static str> {
    match key {
        k if k == PLATFORM_KEYS[0] || k == PLATFORM_KEYS[1] => Some("vike-tradehub"),
        k if k == PLATFORM_KEYS[2] || k == PLATFORM_KEYS[3] => Some("vike-datahub"),
        _ => None,
    }
}

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

/// **Which venue and tier a [`lookup_keys`] name belongs to** — the classification a WRITER needs
/// and a reader never did.
///
/// `Some((venue, Some(tier)))` for a credential key, `Some((venue, None))` for an attribution key
/// (a broker/builder code is per-venue and has no tier), and `None` for any name outside
/// [`lookup_keys`] — so this is also the membership test, answered once instead of by building the
/// whole grid and searching it.
///
/// ⚠ **It lives HERE for the reason [`starter_keys`] gives**, and the reason is a gate rather than
/// taste: `crates/vike-ops/tests/settings_registry.rs`'s `generated_key_sites` reads a call to
/// [`credential_key`] as *this crate READS these variables* and would then demand the whole grid's
/// worth of `SETTINGS` rows for whichever crate composed the names. `vike-cli secrets set`
/// validates a key name and reads none of them, so the composition belongs in the table's own
/// module and the caller just asks.
///
/// The LEGACY `MAINNET` tier answers with itself rather than with `LIVE`, unlike
/// [`crate::account_keys::AccountRef`]'s normalization: this function's job is to say what the name
/// IS, and a caller writing that key is writing the legacy spelling whatever it means downstream.
#[must_use]
pub fn key_owner(key: &str) -> Option<(&'static str, Option<&'static str>)> {
    for venue in VENUES {
        for tier in CREDENTIAL_TIERS.iter().chain(LEGACY_CREDENTIAL_TIERS.iter()) {
            for sfx in CREDENTIAL_SUFFIXES {
                if credential_key(venue, tier, sfx) == key {
                    return Some((venue, Some(tier)));
                }
            }
        }
        // Same narrowing `attribution_keys` applies: a venue with no order-level mechanic produces
        // no attribution key, so one spelled for it belongs to nobody.
        if !attribution_for(venue).is_none() {
            for sfx in ATTRIBUTION_SUFFIXES {
                if attribution_key(venue, sfx) == key {
                    return Some((venue, None));
                }
            }
        }
    }
    None
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

    /// **`key_owner` is TOTAL over the grid and answers for nothing else** — both directions, so the
    /// membership half of `vike-cli secrets set`'s validation cannot go quietly wrong in either
    /// one. A key the workspace can read that this classifier rejects is a key an operator would be
    /// refused by name; a name outside the grid that it accepts is a key nothing will ever read,
    /// written into the store with a success message.
    #[test]
    fn key_owner_classifies_exactly_the_lookup_grid() {
        for key in lookup_keys() {
            let (venue, tier) = key_owner(&key)
                .unwrap_or_else(|| panic!("{key} is in lookup_keys() and must be classified"));
            assert!(VENUES.contains(&venue), "{key}: {venue} is not a roster venue");
            assert!(key.starts_with(&venue.to_uppercase()), "{key} must be {venue}'s");
            match tier {
                // A credential key names a tier, and the name really carries that spelling —
                // including the legacy one, which is NOT normalized here (see the doc).
                Some(t) => assert!(
                    CREDENTIAL_TIERS.contains(&t) || LEGACY_CREDENTIAL_TIERS.contains(&t),
                    "{key}: {t} is no tier"
                ),
                // …and the tier-less answer is exactly the attribution family.
                None => assert!(
                    key.ends_with(BROKER_CODE_SUFFIX) || key.ends_with(BUILDER_CODE_SUFFIX),
                    "{key} answered with no tier but is not an attribution key"
                ),
            }
        }
        // The other direction: near-misses, a bespoke key the grid deliberately excludes, and an
        // unmechanised venue's attribution code all answer `None`.
        //
        // ⚠ **The near-misses are COMPOSED, never spelled**, and that is not style.
        // `crates/vike-ops/tests/settings_registry.rs`'s literal harvest reads ANY string literal
        // with env-var shape and a known prefix as evidence that this crate READS that variable,
        // and then demands a `vike_ops::settings::SETTINGS` row for it — so spelling
        // `{VENUE}_{TIER}_API_KEYS` here as test data would demand a registry row for a key nothing
        // reads, which is precisely what that registry exists to refuse. Composing them keeps the
        // fixtures out of the harvest while leaving them exactly as near a miss.
        let real = credential_key(VENUES[0], CREDENTIAL_TIERS[0], API_KEY_SUFFIX);
        let outsiders = [
            format!("{real}_"),
            format!("_{real}"),
            format!("{real}S"),
            real.replace(CREDENTIAL_TIERS[0], "PROD"),
            format!("NOTAVENUE_{}", &real[real.find('_').unwrap() + 1..]),
            // Two BESPOKE shapes, likewise composed: real keys their own bridge's `config.rs`
            // reads with a literal, and which this grid deliberately does not cover.
            format!("FXCM_{}", "DEMO_USER"),
            format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"),
            String::new(),
        ];
        for outsider in &outsiders {
            assert!(key_owner(outsider).is_none(), "{outsider} must not be classified");
        }
        // …and an attribution code for a venue with NO order-level mechanic is nobody's key, the
        // same narrowing `attribution_keys` applies.
        let unmechanised = VENUES
            .iter()
            .find(|v| attribution_for(v).is_none())
            .expect("some roster venue has no attribution mechanic");
        assert!(key_owner(&attribution_key(unmechanised, BROKER_CODE_SUFFIX)).is_none());
    }
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

    /// **The platform table and the venue grid do not intersect, in either direction** — the
    /// property every one of [`PLATFORM_KEYS`]' three "NOT unioned into" clauses rests on.
    ///
    /// A platform key that `key_owner` classified would become settable through
    /// `vike-cli secrets set` the moment somebody widened one function, silently reversing the
    /// "generating is the fix; accepting is not" verdict; a grid key that answered `true` here
    /// would let a node-key writer overwrite a venue's live signing credential.
    #[test]
    fn platform_keys_are_outside_the_venue_grid() {
        for key in PLATFORM_KEYS {
            assert!(
                key_owner(key).is_none(),
                "{key} is a PLATFORM key and must belong to no venue — `secrets set` refuses it"
            );
            assert!(!lookup_keys().contains(&key.to_string()), "{key} must not be in the grid");
        }
        for key in lookup_keys() {
            assert!(!is_platform_key(&key), "{key} is a venue key and is not a platform key");
        }
    }

    /// The table is two DISTINCT, env-shaped names, and the membership test answers for them and
    /// for nothing near them.
    ///
    /// ⚠ The near-misses are COMPOSED for the reason [`PLATFORM_KEYS`]' own doc gives and
    /// `key_owner_classifies_exactly_the_lookup_grid` gives above: a whole env-shaped literal in
    /// this file is read by the settings registry's harvest as a READ, and demands a row for a
    /// variable this crate does not read.
    #[test]
    fn the_platform_table_is_two_distinct_env_shaped_names() {
        assert_ne!(
            PLATFORM_KEYS[0], PLATFORM_KEYS[1],
            "the two node keys are separate credentials"
        );
        for key in PLATFORM_KEYS {
            assert!(
                key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "{key} is not env-var shaped"
            );
            assert!(is_platform_key(key), "{key} must answer its own membership test");
        }
        for outsider in
            [format!("{}_", PLATFORM_KEYS[0]), PLATFORM_KEYS[0].to_lowercase(), String::new()]
        {
            assert!(!is_platform_key(&outsider), "{outsider:?} must not be a platform key");
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
