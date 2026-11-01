//! **The deployment map the trade venue half reads** (wave 2, W9b; the deployment names are W9c's): the
//! store's DEPLOYMENT names a bridge looks up, plus the resolved settings flags that gate code inside a
//! venue adapter, as the `HashMap<String, String>` every `AdapterInputs` carries as `secrets`.
//!
//! This is the tradehub root's map re-homed. In the tradehub it was `workspace_credentials()` (the
//! credential store's whole by-name map, `read_all`) with `fold_flags_into_vars` written over it
//! (`crates/vike-tradehub/src/tradehub_cli/flags.rs`'s `fold_flags_into_vars`); the venue half is the
//! gatewayhub's now, so the gatewayhub's root fills it, from the flags ITS boot resolved.
//!
//! **Two halves, and each is the original's:**
//!
//! * **The deployment names** ([`deployment_names`]): what the bridges still look up BY NAME, which
//!   `AdapterInputs::secrets` is the only door to: the attribution codes (`{VENUE}_BROKER_CODE` /
//!   `{VENUE}_BUILDER_CODE`), the aster and hyperliquid builder fees, dukascopy's `JAVA_HOME` and
//!   `JFOREX_BRIDGE_JAR`, polymarket's geoblock override. They are DEPLOYMENT rows (filed against neither an
//!   account nor a venue: `vike-cli secrets set NAME`), not venue API keys, and they are read here with the
//!   NARROWEST read that yields them: `CredentialStore::read` over a `KeyScope` of exactly these names,
//!   under the handle's own `VIKE_NO_LIVE_KEYS` scope (so an aster or polymarket name is withheld in a
//!   demo-scoped process exactly as `read_all` withheld it). No account's credential VALUE rides in this
//!   map: an account's credentials are its own, by field, in the account directory. Leaving these out
//!   silently stopped every broker/builder attribution (a binance link id, a bybit broker id, an okx broker
//!   code, an aster builder), the builder fees, a named JRE and the geoblock override, with no log line:
//!   `attribution_code_from` answers `None` for an absent key by design.
//! * **The five flags** ([`folded_flag_rows`]): the `"1"`/`"0"` grammar and the OVERWRITE rule are the
//!   original's (every reader compares against `"1"`, and the flag is the SOLE source: no credential-store
//!   line can arm or veto a gate). The tradehub's copy leaves with its last reader (W15).
//!   `flags.record_properties` is NOT folded: the tradehub folded it for a recorder nothing constructs
//!   (`properties_rec: None` since #2093, and decision 0111 gave the recorder its flag as a PARAMETER), and
//!   the gatewayhub passes `None` too.
//!
//! `the_deployment_map_is_the_tradehubs_for_every_name_a_bridge_reads` (in `gatewayhub_cli_tests.rs`) holds
//! the pair equal to the original over one store.
//!
//! **And the source, [`trade_source`]**: the same map, the account directory and the venue settings, read
//! again each time the lane asks (see `crate::trade::adapters::refreshed`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{CredentialStore, KeyScope};

use crate::trade::adapters::refreshed::TradeSource;
use crate::trade::adapters::snapshot::TradeSnapshot;

/// `"1"` / `"0"` — the exact grammar every one of these gates parses.
fn flag_wire_value(on: bool) -> String {
    if on { "1" } else { "0" }.to_string()
}

/// Every key the fold writes, with the resolved value: the table both the fold and its test iterate.
pub(super) fn folded_flag_rows(flags: &vike_config::Flags) -> [(&'static str, bool); 5] {
    use vike_config::flags as f;
    [
        // Polymarket's two mount gates (map-only readers since decision 0095).
        (f::POLY_EXEC_ENV, flags.poly_exec),
        (f::POLY_RECONCILE_ENV, flags.poly_reconcile),
        // Hyperliquid's symbology universe.
        (f::HYPERLIQUID_HIP3_ENV, flags.hyperliquid_hip3),
        // The two SAFETY OVERRIDES, where `false` is the guarded state.
        (f::ALLOW_WITHDRAW_KEYS_ENV, flags.allow_withdraw_keys),
        (f::PREFLIGHT_SKIP_ENV, flags.preflight_skip),
    ]
}

/// **The deployment names a bridge looks up out of `AdapterInputs::secrets`**, sorted: every attribution
/// key the shared grid enumerates, and every row of `STORE_KEYS_OUTSIDE_THE_GRID` whose reader is a
/// bridge crate (`bridges/<venue>`). Derived from the two tables the writer (`vike-cli secrets set`)
/// validates against, so a deployment name a bridge gains is read here the day it is settable.
pub(super) fn deployment_names() -> Vec<String> {
    let mut names = vike_venues::credential_keys::attribution_keys();
    names.extend(
        vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
            .iter()
            .filter(|(_, reader)| reader.starts_with("bridges/"))
            .map(|(name, _)| (*name).to_string()),
    );
    names.sort_unstable();
    names.dedup();
    names
}

/// The store's deployment names the bridges read, by NAME and nothing else. A store that exists and
/// cannot be read is an ERROR, logged, and reads empty (the live gate: no attribution, no builder fee), as
/// the tradehub's whole-map read did.
fn deployment_vars(store: &CredentialStore) -> HashMap<String, String> {
    match store.read(&KeyScope::of(deployment_names())) {
        Ok(read) => read.into_map(),
        Err(e) => {
            tracing::error!(
                error = %e,
                "vike-gatewayhub: the credential store could not be read for the deployment names \
                 the venue bridges look up (the attribution codes, the builder fees, the JForex tool \
                 paths, the geoblock override); they read as absent"
            );
            HashMap::new()
        }
    }
}

/// The map the venue half reads: the store's deployment names, then the resolved `flags` folded over
/// them (the flag wins for its five keys, whatever a store row of that name holds).
pub(super) fn trade_vars(
    store: &CredentialStore,
    flags: &vike_config::Flags,
) -> HashMap<String, String> {
    let mut vars = deployment_vars(store);
    for (name, on) in folded_flag_rows(flags) {
        vars.insert(name.to_string(), flag_wire_value(on));
    }
    vars
}

/// The flags as the settings database holds them NOW, or **the guarded defaults when it cannot be
/// trusted**: the load failed, the store would not open, or the loader marked a refusal (an illegal row
/// degrades every flag to its default anyway). A read that cannot be trusted FAILS CLOSED; it does not
/// keep the values the previous read gave, because the operator may have switched one off in the very edit
/// that broke the store, and an adapter restarting on the old, more permissive value would be arming what
/// the rows now forbid (the BEFORE tree refused to boot on such a row).
///
/// Each of the five folded flags ([`folded_flag_rows`]) is restrictive OFF, and `Flags::default()` has all
/// five off (`the_defaults_the_fail_closed_read_falls_to_are_the_guarded_states` holds that):
///
/// * `poly_exec` off: no Polymarket exec plane (`ExecFlagUnset`, the account NOT RUNNING);
/// * `poly_reconcile` off: no Polymarket reconcile client, so no mainnet reconcile pass starts;
/// * `allow_withdraw_keys` off: a withdraw-capable key is refused at arming (the guarded state);
/// * `preflight_skip` off: the preflight runs (the guarded state);
/// * `hyperliquid_hip3` off: the narrower symbology universe (the default), a read-only choice.
///
/// `boot_flags` is only the answer when there is no settings directory at all (no store to be wrong
/// about); the edit takes effect once the store is sound again, and the error says so.
fn fresh_flags(settings_dir: Option<&Path>, boot_flags: vike_config::Flags) -> vike_config::Flags {
    let Some(dir) = settings_dir else { return boot_flags };
    let read = vike_secrets::read_settings_in(dir);
    let mut refusal = String::new();
    let source = vike_config::StoreLayer::of(Some(&read), &mut refusal);
    match vike_config::load_with_source(Some(dir), source, &vike_config::CliOverrides::default()) {
        Ok(settings) if settings.store_refusal.is_none() && settings.seal_refusal.is_none() => {
            settings.flags
        }
        Ok(settings) => {
            tracing::error!(
                refusal = ?settings.store_refusal.as_deref().or(settings.seal_refusal.as_deref()),
                "vike-gatewayhub: the settings store did not read cleanly; the venue half runs on the \
                 GUARDED flags (poly_exec, poly_reconcile, allow_withdraw_keys, preflight_skip and \
                 hyperliquid_hip3 all off) until it does"
            );
            vike_config::Flags::default()
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "vike-gatewayhub: the settings could not be loaded; the venue half runs on the GUARDED \
                 flags (poly_exec, poly_reconcile, allow_withdraw_keys, preflight_skip and \
                 hyperliquid_hip3 all off) until they can"
            );
            vike_config::Flags::default()
        }
    }
}

/// **The trade lane's source** (`crate::trade::adapters::refreshed::RefreshedTrade`): each call reads the
/// deployment map, the `account` directory (every account's credentials by field), the `venue_setting`
/// rows and the flags afresh, from the store the boot opened and the settings directory it resolved.
/// `boot_flags` is what the boot resolved; a later read that cannot be trusted falls to the guarded
/// defaults ([`fresh_flags`]), and a `venue_setting` table that cannot be read is carried in the snapshot
/// (`TradeSnapshot::venue_settings_unread`), which refuses the start.
///
/// This replaces a read taken once at the daemon's start. In the tradehub those reads were once per
/// TRADEHUB boot, so a credential rotation or a `venue_setting` edit took effect on a tradehub restart; the
/// gatewayhub outlives every tradehub, so the same reads are repeated at each step a tradehub restart
/// drives.
pub(super) fn trade_source(
    store: CredentialStore,
    settings_dir: Option<PathBuf>,
    boot_flags: vike_config::Flags,
) -> TradeSource {
    Box::new(move || {
        let flags = fresh_flags(settings_dir.as_deref(), boot_flags);
        let accounts =
            AccountDirectory::read(store.accounts(), store.account_keys(), store.credential_book());
        let loaded = settings_dir.as_deref().map(vike_secrets::venue_setting::load_venue_settings);
        let venue_settings = super::boot::venue_settings_of(loaded.as_ref());
        let venue_settings_unread =
            loaded.as_ref().and_then(|read| read.as_ref().err()).map(ToString::to_string);
        (
            trade_vars(&store, &flags),
            TradeSnapshot {
                accounts,
                venue_settings,
                venue_settings_unread,
                preflight_failed: std::collections::BTreeSet::new(),
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag half of the map: the five keys, from a store with no deployment row.
    #[test]
    fn the_fold_writes_exactly_the_five_flag_keys_over_an_empty_store() {
        let store = CredentialStore::open(None, vike_secrets::CredentialScope::Full);
        let vars = trade_vars(&store, &vike_config::Flags::default());
        let mut names: Vec<&str> = vars.keys().map(String::as_str).collect();
        names.sort_unstable();
        let mut want = vec![
            vike_config::flags::POLY_EXEC_ENV,
            vike_config::flags::POLY_RECONCILE_ENV,
            vike_config::flags::HYPERLIQUID_HIP3_ENV,
            vike_config::flags::ALLOW_WITHDRAW_KEYS_ENV,
            vike_config::flags::PREFLIGHT_SKIP_ENV,
        ];
        want.sort_unstable();
        assert_eq!(names, want);
    }

    #[test]
    fn a_true_flag_is_one_and_a_false_flag_is_zero() {
        let store = CredentialStore::open(None, vike_secrets::CredentialScope::Full);
        let flags = vike_config::Flags { poly_exec: true, ..vike_config::Flags::default() };
        let vars = trade_vars(&store, &flags);
        assert_eq!(vars[vike_config::flags::POLY_EXEC_ENV], "1");
        assert_eq!(vars[vike_config::flags::POLY_RECONCILE_ENV], "0");
    }

    /// The names are the closed set the bridges read, derived: the attribution grid plus the five
    /// `bridges/*` rows. The five are spelled out here on purpose: a name a bridge reads and the table
    /// lost would be silent.
    #[test]
    fn the_deployment_names_are_the_attribution_grid_and_the_five_bridge_rows() {
        let names = deployment_names();
        for key in vike_venues::credential_keys::attribution_keys() {
            assert!(names.contains(&key), "{key} is an attribution key a bridge reads");
        }
        for fixed in [
            "ASTER_BUILDER_FEE_RATE",
            "HYPERLIQUID_BUILDER_FEE_TENTHS_BP",
            "JAVA_HOME",
            "JFOREX_BRIDGE_JAR",
            "POLY_GEOBLOCK_OVERRIDE",
        ] {
            assert!(
                names.iter().any(|n| n == fixed),
                "{fixed} is a deployment name a bridge reads"
            );
        }
        let bridge_rows = vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
            .iter()
            .filter(|(_, reader)| reader.starts_with("bridges/"))
            .count();
        assert_eq!(
            names.len(),
            vike_venues::credential_keys::attribution_keys().len() + bridge_rows,
            "nothing else is asked of the store"
        );
        assert_eq!(bridge_rows, 5, "the bridge crates read five deployment names today");
    }
}
