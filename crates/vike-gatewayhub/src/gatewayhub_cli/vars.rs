//! **The deployment map the trade venue half reads** (wave 2, W9b; the deployment names are W9c's): the
//! store's DEPLOYMENT names a bridge looks up, as the `HashMap<String, String>` every `AdapterInputs` carries
//! as `secrets`, and nothing else.
//!
//! This is the tradehub root's map re-homed, minus its flags. In the tradehub it was `workspace_credentials()`
//! (the credential store's whole by-name map, `read_all`) with the resolved flags written over it as `"1"`/`"0"`
//! strings; the venue half is the gatewayhub's now, and the flags reach it TYPED.
//!
//! * **The deployment names** ([`deployment_names`]): what the bridges still look up BY NAME, which
//!   `AdapterInputs::secrets` is the only door to: the attribution codes (`{VENUE}_BROKER_CODE` /
//!   `{VENUE}_BUILDER_CODE`), the aster and hyperliquid builder fees, dukascopy's `JAVA_HOME` and
//!   `JFOREX_BRIDGE_JAR`, polymarket's geoblock override. They are DEPLOYMENT rows (filed against neither an
//!   account nor a venue: `vike-cli secrets set NAME`), not venue API keys, and they are read here with the
//!   NARROWEST read that yields them: `CredentialStore::read` over a `CredentialKeyScope` of exactly these names,
//!   under the handle's own `VIKE_NO_LIVE_KEYS` scope (so an aster or polymarket name is withheld in a
//!   demo-scoped process exactly as `read_all` withheld it). No account's credential VALUE rides in this
//!   map: an account's credentials are its own, by field, in the account directory. Leaving these out
//!   silently stopped every broker/builder attribution (a binance link id, a bybit broker id, an okx broker
//!   code, an aster builder), the builder fees, a named JRE and the geoblock override, with no log line:
//!   `attribution_code_from` answers `None` for an absent key by design.
//! * **The flags are not in it**: [`trade_source`] hands the resolved `vike_config::Flags` in
//!   `TradeLaneSnapshot::flags`, and a start reads them as `AdapterFlags`
//!   (`crate::trade::adapters::contract`'s `adapter_flags`). The flag row is the SOLE source: a credential-store
//!   row spelled like a flag's retired name is no deployment name, so nothing reads it.
//!
//! `the_deployment_map_is_the_tradehubs_for_every_name_a_bridge_reads` (in `gatewayhub_cli_tests.rs`) holds
//! the names equal to the original over one store.
//!
//! **And the source, [`trade_source`]**: the same map, the account directory, the venue settings and the
//! flags, read again each time the lane asks (see `crate::trade::adapters::refreshed`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{CredentialKeyScope, CredentialStore};

use crate::trade::adapters::refreshed::TradeLaneSource;
use crate::trade::adapters::snapshot::{IbkrCashSettledRoots, TradeLaneSnapshot};

/// **The deployment names a bridge looks up out of `AdapterInputs::secrets`**, sorted: every attribution
/// key the shared grid enumerates, and every row of `STORE_KEYS_OUTSIDE_THE_GRID` whose reader is a
/// bridge crate (`bridges/<venue>`). Derived from the two tables the writer (`vike-cli secrets set`)
/// validates against, so a deployment name a bridge gains is read here the day it is settable.
pub(super) fn deployment_names() -> Vec<String> {
    let mut names = vike_venues::credential_keys::attribution_keys();
    names.extend(
        vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
            .iter()
            .filter(|(_, reader, _)| reader.starts_with("bridges/"))
            .map(|(name, ..)| (*name).to_string()),
    );
    names.sort_unstable();
    names.dedup();
    names
}

/// The store's deployment names the bridges read, by NAME and nothing else. A store that exists and
/// cannot be read is an ERROR, logged, and reads empty (the live gate: no attribution, no builder fee), as
/// the tradehub's whole-map read did. This is the whole map the venue half reads.
pub(super) fn deployment_vars(store: &CredentialStore) -> HashMap<String, String> {
    match store.read(&CredentialKeyScope::of(deployment_names())) {
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

/// The flags as the settings database holds them NOW, or **the guarded defaults when it cannot be
/// trusted**: the load failed, the store would not open, or the loader marked a refusal (an illegal row
/// degrades every flag to its default anyway). A read that cannot be trusted FAILS CLOSED; it does not
/// keep the values the previous read gave, because the operator may have switched one off in the very edit
/// that broke the store, and an adapter restarting on the old, more permissive value would be arming what
/// the rows now forbid (the BEFORE tree refused to boot on such a row).
///
/// Each of the four flags a start reads (`AdapterFlags`) is restrictive OFF, and `Flags::default()` has all
/// four off (`crates/vike-gatewayhub/src/trade/adapters/contract_tests/process_facts.rs`'s
/// `the_venue_gates_a_start_is_handed_are_the_resolved_flags` holds that):
///
/// * `poly_exec` off: no Polymarket exec plane (`ExecFlagUnset`, the account NOT RUNNING);
/// * `poly_reconcile` off: no Polymarket reconcile client, so no mainnet reconcile pass starts;
/// * `allow_withdraw_keys` off: a withdraw-capable key is refused at arming (the guarded state);
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
/// (`TradeLaneSnapshot::venue_settings_unread`), which refuses the start.
///
/// This replaces a read taken once at the daemon's start. In the tradehub those reads were once per
/// TRADEHUB boot, so a credential rotation or a `venue_setting` edit took effect on a tradehub restart; the
/// gatewayhub outlives every tradehub, so the same reads are repeated at each step a tradehub restart
/// drives.
pub(super) fn trade_source(
    store: CredentialStore,
    settings_dir: Option<PathBuf>,
    boot_flags: vike_config::Flags,
) -> TradeLaneSource {
    Box::new(move || {
        let flags = fresh_flags(settings_dir.as_deref(), boot_flags);
        let accounts = AccountDirectory::read(
            store.accounts(),
            store.account_keys(),
            store.credential_registry(),
        );
        let loaded = settings_dir.as_deref().map(vike_secrets::venue_setting::load_venue_settings);
        let venue_settings = super::boot::venue_settings_of(loaded.as_ref());
        let venue_settings_unread =
            loaded.as_ref().and_then(|read| read.as_ref().err()).map(ToString::to_string);
        (
            deployment_vars(&store),
            TradeLaneSnapshot {
                accounts,
                venue_settings,
                venue_settings_unread,
                preflight_failed: std::collections::BTreeSet::new(),
                ibkr_cash_settled_roots: ibkr_cash_settled_roots(settings_dir.as_deref()),
                flags,
            },
        )
    })
}

/// **The `ibkr_cash_settled_root` rows**, read afresh with the rest of the snapshot, or why there are none: the words
/// every IBKR future is then refused with. READ ONLY, on every box: a store with no table answers its directory, and
/// only IBKR's first future admission creates the table (`crate::trade::adapters::ibkr_roots`), so a box that trades
/// no IBKR future never holds one (the owner, 2026-11-16: one venue's step never sits in the shared start).
fn ibkr_cash_settled_roots(settings_dir: Option<&Path>) -> IbkrCashSettledRoots {
    use vike_secrets::ibkr_cash_settled_root::read_ibkr_cash_settled_roots;
    let Some(dir) = settings_dir else {
        return IbkrCashSettledRoots::Unread(
            crate::trade::adapters::snapshot::ROOTS_NOT_READ.to_string(),
        );
    };
    match read_ibkr_cash_settled_roots(dir) {
        Ok(Some(roots)) => IbkrCashSettledRoots::Read(roots),
        Ok(None) => IbkrCashSettledRoots::NotCreated(dir.to_path_buf()),
        Err(e) => IbkrCashSettledRoots::Unread(format!(
            "the `ibkr_cash_settled_root` rows could not be read, so no IBKR future is admitted: {e}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No settings directory holds no root and says why (the words every IBKR future is refused with). A store with no
    /// table answers its directory and is left WITHOUT one: the snapshot is read at every start of every box, IBKR or
    /// not, so a start that trades no IBKR future creates no IBKR table (the seeded rows' read is `vike-secrets`' own
    /// test, the creation `crate::trade::adapters::ibkr_roots`').
    #[test]
    fn the_snapshot_read_creates_no_table_and_says_why_it_has_no_root() {
        use vike_secrets::ibkr_cash_settled_root::read_ibkr_cash_settled_roots;
        assert_eq!(
            ibkr_cash_settled_roots(None),
            IbkrCashSettledRoots::Unread(
                crate::trade::adapters::snapshot::ROOTS_NOT_READ.to_string()
            )
        );
        let dir = tempfile::TempDir::new().expect("a temp settings dir");
        vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
            .expect("a store with no root table");
        for _ in 0..2 {
            assert_eq!(
                ibkr_cash_settled_roots(Some(dir.path())),
                IbkrCashSettledRoots::NotCreated(dir.path().to_path_buf())
            );
        }
        assert_eq!(
            read_ibkr_cash_settled_roots(dir.path()).expect("read"),
            None,
            "a read created it"
        );
    }

    /// The flags ride the snapshot TYPED, and the map holds no flag name at all: over a store with no
    /// deployment row and no settings directory (the boot's flags are the answer), the map is empty.
    #[test]
    fn the_source_hands_the_flags_typed_and_the_map_holds_no_flag_name() {
        let store = CredentialStore::open(None, vike_secrets::CredentialScope::Full);
        let flags = vike_config::Flags { poly_exec: true, ..vike_config::Flags::default() };
        let (vars, snapshot) = trade_source(store, None, flags)();
        assert_eq!(snapshot.flags, flags);
        assert!(vars.is_empty(), "no flag name rides the deployment map: {vars:?}");
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
            .filter(|(_, reader, _)| reader.starts_with("bridges/"))
            .count();
        assert_eq!(
            names.len(),
            vike_venues::credential_keys::attribution_keys().len() + bridge_rows,
            "nothing else is asked of the store"
        );
        assert_eq!(bridge_rows, 5, "the bridge crates read five deployment names today");
    }
}
