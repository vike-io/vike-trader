//! `settings_grid` — THE GENERATED KEY GRID: the second table of the settings registry.
//!
//! [`crate::settings::SETTINGS`] holds every hand-written row. The rows here are the family it
//! structurally could not see — the ATTRIBUTION-code keys `vike_secrets::credentials`'
//! `attribution_code_from` COMPOSES with a `format!` (`vike_model::credential_keys`'
//! `attribution_key`) and then asks the credential map for, which appear as a string literal at no
//! read site. They are one family with one argument, so they live in a file of their own rather than
//! at the bottom of the other table. The gate that checks them is
//! `crates/vike-ops/tests/settings_secrets/settings_registry/walk_and_grid.rs`'s
//! `every_generated_key_is_declared`.
//!
//! ⚠ **The grid used to be three times thirteen times three rows bigger, and that half is GONE, not
//! moved.** It also held every `{VENUE}_{SIM|DEMO|LIVE}_API_{KEY,SECRET,PASSPHRASE}` name the
//! credential loader composed. Decision 0156 item 6 deleted that composition: a credential belongs
//! to its ACCOUNT, a bridge reads it by FIELD (`API_KEY`) through
//! `vike_secrets::account_credentials::AccountCredentials`, and no reader builds a credential NAME or
//! reads a tier out of one. A field word is not a name looked up in a map, so it has no row here;
//! the per-venue field words are `crates/vike-model/src/credential_schema.rs`'s `VENUE_KEY_SCHEMAS`.
//!
//! ⚠ **Two tables, one registry, and ONE way to iterate it.** [`crate::settings::all_settings`]
//! chains [`crate::settings::SETTINGS`] and the table below, and every consumer of the registry
//! reads that. A reader of `SETTINGS` alone has silently dropped every attribution-code row —
//! `vike-cli config show` would stop listing them — with nothing failing to compile. The table is
//! `pub(crate)` for that reason: this module is reachable only through `all_settings`.
//!
//! The new-venue scaffold's marker at the bottom of the table is a NOTE, not a row
//! (`crates/vike-ops/tests/venues/new_venue_gate/site_rules.rs`'s `STRUCTURAL_SITES` is keyed on this
//! file): a scaffolded venue's attribution arm is `AttributionMechanic::None`, which composes no key.

use crate::settings::{Layer, Medium, Naming, Scope, Setting};

/// One row of `GENERATED_KEY_GRID`. The rows differ in nothing but `name`, so the six fields they
/// share are spelled ONCE, here, and the table below is one call per key. Why the crate is
/// `vike-secrets` and why the default is empty is argued at the head of the table; the medium
/// is the credential map because that is the map `vike_secrets::credentials` asks for every one.
const fn key(name: &'static str) -> Setting {
    Setting {
        name,
        krate: "vike-secrets",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::CredentialMap,
        default: "",
    }
}

/// The generated attribution-code key rows, sorted. Read through
/// [`crate::settings::all_settings`], never alone and never from outside this crate.
pub(crate) const GENERATED_KEY_GRID: &[Setting] = &[
    // -----------------------------------------------------------------------------------------
    // THE GENERATED KEY GRID — the family this table structurally could not see.
    //
    // Every row below is a key `vike_secrets::credentials`' `attribution_code_from` BUILDS with a
    // `format!` (`{VENUE}_BROKER_CODE`, then `{VENUE}_BUILDER_CODE`) and then asks the credential
    // map for. None of them appears as a string literal at its read site, so
    // `crates/vike-model/src/scan.rs`'s `find_map_lookups` could not observe one and this table
    // could not declare one. `DYNAMIC_ALLOWLIST` could never have covered them — that table
    // allowlists a call site the scanner FOUND and could not resolve, and a computed map `get` is
    // not recognised as a candidate site at all.
    //
    // These rows are HAND-WRITTEN DATA, deliberately, and the gate that checks them derives its
    // expectation from somewhere else entirely: `vike_model::credential_keys`' `attribution_keys`
    // folds the two suffixes over the roster venues whose `vike_model::venues::attribution` arm
    // names a mechanic, and `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
    // `every_generated_key_is_declared` demands a row here for each. GENERATING these rows from that
    // enumeration would make the gate compare the table to itself — the
    // `roster_matches_the_bridge_crates` failure this repo has already shipped once, green through a
    // mutation that added a bridge crate. So giving a venue a mechanic, or adding a suffix, reddens
    // CI until this block is extended, which is the whole point.
    //
    // ⚠ A venue joining the ROSTER adds nothing here, and that is not a gap. `just new-venue NAME`
    // writes its attribution arm as `AttributionMechanic::None`, which `attribution_code_from`
    // returns on before it builds a key — so `attribution_keys` holds none of its names and nothing
    // looks them up. The scaffold's marker below is therefore a NOTE for the human who later names
    // a mechanic, and `every_generated_key_is_declared` prints the two rows that step then needs.
    //
    // Kept as ONE contiguous sorted block, in a file of its own, rather than interleaved into
    // `SETTINGS`' alphabetical body: it is a family with one shared argument. `vike_config::settings::
    // all_settings` chains the two tables and `vike-cli config show` sorts its own output, so the
    // operator view is unaffected by the position.
    //
    // Every row is ONE call to `key`, the constructor above this table: the rows differ in nothing
    // but `name`, so the fields they share are spelled once, there, and no row can drift from them.
    // The grid's size is written down nowhere: `every_generated_key_is_declared` demands a row for
    // every key the enumeration composes, and `every_declared_variable_is_read` refuses a row
    // nothing reads.
    //
    // ⚠ BOTH suffixes for every mechanised venue, though a writer is taught one
    // (`vike_model::credential_keys`' `attribution_var_for`): `attribution_code_from` tries the
    // broker name and falls back to the builder one, so both are looked up.
    //
    // ⚠ `krate` is `vike-secrets` because that is where the map `get` happens, whatever crate
    // calls the reader — and the gate derives the SAME crate from the path of the file that composes
    // the keys. If that file ever moves crates, `every_generated_key_is_declared` and
    // `every_declared_variable_is_read` go red TOGETHER: that signature means RE-KEY these rows, not
    // delete them. (It did once: the reader moved from `vike-bridge-core` to `vike-secrets` by
    // docs/decisions/0135, and every row here was re-keyed.)
    //
    // ⚠ `default: ""` is not a placeholder. Unset means the venue's orders carry no attribution and
    // the wire is byte-identical to an unattributed one.
    // -----------------------------------------------------------------------------------------
    key("ASTER_BROKER_CODE"),
    key("ASTER_BUILDER_CODE"),
    key("BINANCE_BROKER_CODE"),
    key("BINANCE_BUILDER_CODE"),
    key("BYBIT_BROKER_CODE"),
    key("BYBIT_BUILDER_CODE"),
    key("HYPERLIQUID_BROKER_CODE"),
    key("HYPERLIQUID_BUILDER_CODE"),
    key("OKX_BROKER_CODE"),
    key("OKX_BUILDER_CODE"),
    key("POLYMARKET_BROKER_CODE"),
    key("POLYMARKET_BUILDER_CODE"),
];
// vike:new-venue:note `{venue}` gets NO row here while its `crates/vike-model/src/venues/attribution.rs` `attribution_for` arm is `AttributionMechanic::None` (the scaffold's answer): nothing composes or looks up its codes. If you name a mechanic there, add `key("{VENUE}_BROKER_CODE")` and `key("{VENUE}_BUILDER_CODE")` to `GENERATED_KEY_GRID` in sorted position — `every_generated_key_is_declared` demands exactly those two.
