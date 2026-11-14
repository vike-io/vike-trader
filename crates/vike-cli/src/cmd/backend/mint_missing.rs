//! `vike-cli backend mint-missing` — on the SERVER's box: mint every hub's FULL key this project
//! LACKS, and never touch one it already holds
//! (`docs/decisions/0280-every-hub-mints-its-full-keys-at-first-start-and-a-client-takes-them-over-ssh.md`).
//!
//! The owner's ruling of 2026-11-12: *"Can I have full keys by default for any user at start? We
//! just launched and I don't want this nightmare for users with keys."* Before it, a fresh install
//! needed `backend setup`, `datahub setup`, `datahub setup --strategy-lane` and `gatewayhub setup`
//! typed in the right order (the gatewayhub parked itself on exit 78 until the last one ran), and
//! the strategy builder was skipped until somebody invented a key for `secrets set`. The server
//! install (`deploy/docker/entrypoint.sh`'s `start_server`) now runs this verb at EVERY start,
//! before the first daemon, so a fresh folder starts keyed and an upgraded one gains only what it
//! lacked.
//!
//! # What it mints, and what it deliberately does not
//!
//! [`hub_key_names`]: each hub's observe and control key (the tradehub, the datahub — which the
//! backtesthub verifies with too — and the gatewayhub) and the two order-scope keys the daemons
//! hold to reach each other (the gatewayhub's `Trade` key, the datahub's strategy-lane key), plus the
//! strategy builder's one key ([`builder_key_name`], a deployment row of the `credential` table,
//! where its service and Studio read it). It does NOT mint the two admin keys: the tradehub's is
//! decision 0065's second barrier and stays `backend admin-key`'s, and the gatewayhub's is minted by
//! nothing until its scope is served.
//!
//! # The properties every hub-key writer here keeps, unchanged
//!
//! CSPRNG values handed straight to `vike_secrets::save_credentials_to_store` in-process — no argv,
//! stdin or environment form, nothing printed but each key's `key_id`, every name validated against
//! [`vike_venues::credential_keys::PLATFORM_KEYS`], an ABSENT store refused (the supervisor runs
//! `secrets init` first), one journal record per write with key NAMES only, and no MCP reach.
//!
//! ⚠ **"Missing" is decided from ONE read before the write, and a key the read saw is never in the
//! write.** A blank row is no key (every reader trims and treats it as absent), so it is minted over;
//! any other value stays byte for byte. Nothing here rotates: that is `setup --rotate`'s, by the
//! operator's word.
//!
//! The tradehub's DOOR: with no `config.tradehub_addr` row the tradehub serves nothing, and a key
//! with no door is no use to the desktop beside it. So a box with NO such row gets the loopback
//! default ([`super::DEFAULT_BIND_ADDRESS`]); a row an operator wrote is never touched. Loopback only:
//! the remote route stays the ssh tunnel (decision 0280's safety argument).

use super::setup::BIND_ADDRESS_KEY;
use super::{
    Context, DEFAULT_BIND_ADDRESS, hub_key_journal, key_id, mint_key, open_store,
    report_unrecorded_hub_keys,
};
use crate::exit::{CliError, CommandResult};

/// The hub-key names a first start mints, in the order the report prints them: the tradehub pair,
/// the datahub pair, the gatewayhub pair, then the two order-scope keys one daemon holds to reach
/// another. Every one is validated against `PLATFORM_KEYS` before a byte is written ([`super::validated`]).
fn hub_key_names() -> [&'static str; 8] {
    let [tradehub_observe, tradehub_control] = super::key_names();
    let [gatewayhub_observe, gatewayhub_control] =
        vike_venues::credential_keys::gatewayhub_key_pair();
    [
        tradehub_observe,
        tradehub_control,
        vike_hub_protocol::auth::DATAHUB_OBSERVE_KEY_ENV,
        vike_hub_protocol::auth::DATAHUB_CONTROL_KEY_ENV,
        gatewayhub_observe,
        gatewayhub_control,
        vike_venues::credential_keys::gatewayhub_trade_key(),
        vike_venues::credential_keys::datahub_trade_key(),
    ]
}

/// The strategy builder's key NAME: the deployment row `vike-strategy-builder` reads, taken from the
/// table that declares it (`vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID`, the row whose
/// reader is that crate) rather than spelled here, so this binary names no key it does not validate.
fn builder_key_name() -> CommandResult<&'static str> {
    vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
        .iter()
        .find(|(_, reader, _)| *reader == "vike-strategy-builder")
        .map(|(name, ..)| *name)
        .ok_or_else(|| {
            CliError::failed(
                "the strategy builder's key is not in `STORE_KEYS_OUTSIDE_THE_GRID` any more, so \
                 this binary cannot name it. Nothing was written."
                    .to_string(),
            )
        })
}

/// The names of `names` the store does not hold as a key: absent, or a blank row. PURE.
fn missing<'a>(
    names: &[&'a str],
    held: &std::collections::HashMap<String, String>,
) -> Vec<&'a str> {
    names.iter().copied().filter(|n| held.get(*n).is_none_or(|v| v.trim().is_empty())).collect()
}

/// Run `backend mint-missing`. The order: the store (refused when absent), the hub keys, the
/// builder's key, the tradehub's door.
pub(super) fn run_mint_missing(ctx: &Context<'_>) -> CommandResult<()> {
    let names = super::validated(hub_key_names())?;
    let builder = builder_key_name()?;
    let dir = super::settings_dir(ctx);
    let path = super::landed(ctx, &vike_secrets::backend_in(&dir));
    let held = open_store(&dir, ctx.scope)?;

    let mut minted: Vec<(String, String)> =
        missing(&names, &held).into_iter().map(|n| (n.to_string(), mint_key())).collect();
    if !minted.is_empty() {
        let (_landed, journal_error) = vike_secrets::save_credentials_to_store(
            &dir,
            vike_secrets::StoreTable::HubKey,
            &minted,
            // `Add`: a missing name is a row the `hub_key` table does not hold yet.
            vike_secrets::CredentialUpsertIntent::Add,
            hub_key_journal(ctx.now_ms),
        )
        .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;
        report_unrecorded_hub_keys(journal_error);
    }

    // The builder's key is a DEPLOYMENT row of the `credential` table, so its presence is asked of
    // that table: a boolean SQLite computes, no value read.
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(&dir)),
        ctx.scope,
    );
    let builder_held = store
        .present_names(&vike_secrets::CredentialKeyScope::of([builder]))
        .map_err(|e| {
            CliError::failed(format!(
                "{e} — the store is THERE and could not be read. The hub keys above are written; \
                 the builder's key was not."
            ))
        })?
        .contains(builder);
    if !builder_held {
        let row = [(builder.to_string(), mint_key())];
        let (_landed, journal_error) = vike_secrets::save_credentials_to_store(
            &dir,
            vike_secrets::StoreTable::Credential,
            &row,
            // `Add`: a deployment row (no account, no venue), the shape `secrets set` files it in.
            vike_secrets::CredentialUpsertIntent::Add,
            hub_key_journal(ctx.now_ms),
        )
        .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;
        report_unrecorded_hub_keys(journal_error);
        minted.extend(row);
    }

    let door = door_missing(&dir)?;
    if door {
        println!("settings:");
        super::set_setting_journalled(ctx, BIND_ADDRESS_KEY, DEFAULT_BIND_ADDRESS)?;
    }
    for line in report(&minted, &path.display().to_string(), door) {
        println!("{line}");
    }
    Ok(())
}

/// Whether the settings database holds NO `config.tradehub_addr` row: the one case this verb writes
/// it. A store that cannot be read is an error, never "no row".
fn door_missing(dir: &std::path::Path) -> CommandResult<bool> {
    let (section, key) = BIND_ADDRESS_KEY.split_once('.').unwrap_or_default();
    let source = vike_secrets::read_settings_in(dir).map_err(|e| {
        CliError::failed(format!(
            "{e} — the settings database could not be read to look for the {BIND_ADDRESS_KEY} row. \
             The keys above are written."
        ))
    })?;
    Ok(!source
        .rows()
        .is_some_and(|rows| rows.settings.iter().any(|r| r.section == section && r.key == key)))
}

/// What the verb prints: one line per minted key (its NAME and `key_id`, never the value), or that
/// nothing was missing. PURE, so the no-value property is unit-tested.
fn report(minted: &[(String, String)], store: &str, door: bool) -> Vec<String> {
    if minted.is_empty() {
        let mut lines = vec![format!("every hub key is already in {store}; nothing was minted.")];
        if door {
            lines
                .push("the tradehub's door was unset and is now its loopback default.".to_string());
        }
        return lines;
    }
    let width = minted.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut lines = vec![format!(
        "minted {} missing key(s) into {store} (the VALUES were never printed):",
        minted.len()
    )];
    lines.extend(minted.iter().map(|(name, value)| format!("  {name:<width$}  {}", key_id(value))));
    lines.push(
        "a daemon reads its keys once, at start: one already running loads these at its next \
         restart (`bin/vikectl restart <daemon>`). A client on THIS box takes them from this store \
         by itself; another box runs `vike-cli backend connect <this host> --server-project \
         <this project>`."
            .to_string(),
    );
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minted_names_are_platform_keys_and_never_an_admin_key() {
        let names = super::super::validated(hub_key_names()).expect("every name validates");
        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "a name twice: {names:?}");
        let table = vike_venues::credential_keys::PLATFORM_KEYS;
        // `[4]` and `[8]` are the two admin keys: decision 0065's barrier, and a scope nothing serves.
        assert!(!names.contains(&table[4]) && !names.contains(&table[8]), "{names:?}");
        for (i, name) in table.iter().enumerate() {
            assert_eq!(names.contains(name), i != 4 && i != 8, "{name}");
        }
    }

    #[test]
    fn the_builder_key_is_the_row_its_service_reads() {
        let name = builder_key_name().expect("the table names it");
        assert!(vike_venues::credential_keys::is_store_key_outside_the_grid(name));
        assert!(!vike_venues::credential_keys::is_platform_key(name), "a deployment row: {name}");
    }

    #[test]
    fn only_an_absent_or_blank_key_is_missing() {
        let held = std::collections::HashMap::from(
            [("A", "kept"), ("B", "  "), ("C", "")].map(|(k, v)| (k.to_string(), v.to_string())),
        );
        assert_eq!(missing(&["A", "B", "C", "D"], &held), ["B", "C", "D"]);
        assert!(missing(&["A"], &held).is_empty());
    }

    #[test]
    fn the_report_names_and_ids_and_never_a_value() {
        let minted = vec![("NAME_ONE".to_string(), "secret-one".to_string())];
        let text = report(&minted, "/p/settings/db/vike.db", true).join("\n");
        assert!(text.contains("NAME_ONE") && text.contains(&key_id("secret-one")), "{text}");
        assert!(!text.contains("secret-one"), "a value leaked: {text}");
        assert!(text.contains("restart"), "{text}");
        let nothing = report(&[], "/p/x", false).join("\n");
        assert!(nothing.contains("nothing was minted"), "{nothing}");
        assert!(report(&[], "/p/x", true).join("\n").contains("loopback default"));
    }
}
