//! `vike-cli backend mint-missing` — on the SERVER's box: mint every hub's FULL key this project
//! LACKS, and never touch one it already holds
//! (`docs/decisions/0280-every-hub-mints-its-full-keys-at-first-start-and-a-client-takes-them-over-ssh.md`).
//!
//! The owner's ruling of 2026-11-12: *"Can I have full keys by default for any user at start? We
//! just launched and I don't want this nightmare for users with keys."* Before it, a fresh install
//! needed four setup verbs (three of them retired since by decision 0288) typed in the right order (the gatewayhub parked itself on exit 78 until the last one ran), and
//! the strategy builder was skipped until somebody invented a key for `secrets set`. The server
//! install (`deploy/docker/entrypoint.sh`'s `start_server`) now runs this verb at EVERY start,
//! before the first daemon, so a fresh folder starts keyed and an upgraded one gains only what it
//! lacked.
//!
//! # What it mints, and what it deliberately does not
//!
//! [`hub_key_names`]: the DAEMONS' keys only (decision 0164's class, which no person holds): the gatewayhub's
//! observe and control key and the two order-scope keys the daemons hold to reach each other (the gatewayhub's
//! `Trade` key, the datahub's strategy-lane key), plus the strategy builder's one key ([`builder_key_name`], a
//! deployment row of the `credential` table, a daemon key since decision 0288's part 5). It mints NO tradehub or
//! datahub pair any more (decision 0288 item 8: the hubs read none, a person signs with a device key) and never an
//! admin key.
//!
//! It also mints this box's own DEVICE keys (decision 0288 items 6 and 8, [`SERVER_DEVICES`]): `owner/server`
//! (`admin`), `server/backtesthub` (`observe`) and `owner/telegram` (`control`), each an Ed25519 pair stored in
//! `device_key` with its public half granted on this box's `access_key` list in the same transaction. A held key
//! stays; a revoked one is never re-granted.
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
//! any other value stays byte for byte. Nothing here rotates: replacing a held key is an operator's
//! word, never a start's.
//!
//! The tradehub's DOOR: with no `config.tradehub_addr` row the tradehub serves nothing, and a key
//! with no door is no use to the desktop beside it. So a box with NO such row gets the loopback
//! default ([`vike_config::config::DEFAULT_TRADEHUB_ADDRESS`]); a row an operator wrote (`vike-cli config set
//! config.tradehub_addr`) is never touched. Loopback only: the remote route stays the ssh tunnel
//! (decision 0280's safety argument).

use vike_hub_protocol::access::{SERVER_DEVICES, ServerDevice};

use super::{Context, hub_key_journal, key_id, mint_key, open_store, report_unrecorded_hub_keys};
use crate::exit::{CliError, CommandResult};

/// The `config` section's bind key, spelled once, in the dotted form `vike_config::write_setting_row`
/// takes: its first segment names the section, the remainder is the key inside it.
const BIND_ADDRESS_KEY: &str = "config.tradehub_addr";

/// The hub-key names a first start mints, in the order the report prints them: the DAEMONS' keys alone
/// (decision 0164's class, which no person holds) — the gatewayhub pair the tradehub and the datahub dial it with,
/// then the two order-scope keys one daemon holds to reach another. ⚠ No tradehub or datahub pair any more
/// (decision 0288 item 8): a person and a daemon dialling a person's hub sign with a DEVICE key
/// ([`SERVER_DEVICES`]), and the hubs read no pair. Every one is validated against `PLATFORM_KEYS` before a byte is
/// written ([`super::validated`]).
fn hub_key_names() -> [&'static str; 4] {
    let [gatewayhub_observe, gatewayhub_control] =
        vike_venues::credential_keys::gatewayhub_key_pair();
    [
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

    let devices = mint_server_devices(&dir, &path, ctx.now_ms, &|row| {
        crate::cmd::access::journal(
            ctx,
            "add",
            &row.person,
            Some(&row.device),
            Some(&row.role),
            Some(&row.key_id),
        );
    })?;

    let door = door_missing(&dir)?;
    if door {
        println!("settings:");
        super::set_setting_journalled(
            ctx,
            BIND_ADDRESS_KEY,
            vike_config::config::DEFAULT_TRADEHUB_ADDRESS,
        )?;
    }
    for line in report(&minted, &path.display().to_string(), door) {
        println!("{line}");
    }
    for line in devices {
        println!("{line}");
    }
    Ok(())
}

/// Mint each [`SERVER_DEVICES`] key this box lacks and grant it on this box's own list, one transaction each
/// (`vike_secrets::access::device_key_grant`): a held key stays, and a revoked one is never re-granted; `granted` is
/// told each row this call granted, for its change-journal record (decision 0288 item 5). Returns one
/// line per device, its key id and what happened; never a private byte. Its first row is the one
/// `vike_secrets::access::device_key_load` answers; each daemon reads its own row back by
/// `vike_secrets::access::device_key_named`.
fn mint_server_devices(
    dir: &std::path::Path,
    path: &std::path::Path,
    now_ms: i64,
    granted: &dyn Fn(&vike_secrets::access::AccessKeyRow),
) -> CommandResult<Vec<String>> {
    use vike_secrets::access::{AccessKeyRow, DeviceAdded, device_key_grant};
    let failed = |e: &dyn std::fmt::Display| {
        CliError::failed(format!(
            "could not write {}: {e} — the hub keys above are written; the device keys were not",
            path.display()
        ))
    };
    let mut lines = Vec::new();
    for ServerDevice { person, device, role } in SERVER_DEVICES {
        let role = role.name();
        let (key, pkcs8) =
            vike_hub_protocol::access::DeviceKey::generate().map_err(|e| failed(&e))?;
        let row = AccessKeyRow {
            key_id: key.key_id(),
            person: person.to_string(),
            device: device.to_string(),
            role: role.to_string(),
            public_key: key.public_key().to_vec(),
            granted_ms: now_ms,
            revoked_ms: None,
        };
        let said = match device_key_grant(dir, &pkcs8, &row).map_err(|e| failed(&e))? {
            DeviceAdded::Added => {
                granted(&row);
                format!("minted, key {}", row.key_id)
            }
            DeviceAdded::Held => "held; nothing minted".to_string(),
            DeviceAdded::Revoked => "REVOKED on the access list; not re-granted".to_string(),
            DeviceAdded::DeviceTaken => {
                "the access list holds it under another key; nothing minted".to_string()
            }
        };
        lines.push(format!("device key {person}/{device} ({role}): {said}"));
    }
    Ok(lines)
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

    /// Only the daemons' keys are minted: never an admin key, and never the tradehub or datahub pair a person once
    /// held (decision 0288 item 8: `[0..4]` are those pairs, `[4]` and `[8]` the two admin keys).
    #[test]
    fn the_minted_names_are_the_daemons_platform_keys_and_never_a_persons() {
        let names = super::super::validated(hub_key_names()).expect("every name validates");
        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "a name twice: {names:?}");
        let table = vike_venues::credential_keys::PLATFORM_KEYS;
        for (i, name) in table.iter().enumerate() {
            assert_eq!(names.contains(name), matches!(i, 5 | 6 | 7 | 9), "{name}");
        }
    }

    /// The door's key is the dotted spelling `vike_config::write_setting_row` takes (a first segment
    /// that is no section is refused with `BadKey`, after the keys were written), and a real setting.
    #[test]
    fn the_door_key_is_sectioned_and_a_real_setting() {
        assert_eq!(
            BIND_ADDRESS_KEY.split('.').next(),
            Some(vike_config::SettingsSection::Config.section())
        );
        let keys = vike_config::provenance::setting_keys();
        assert!(keys.iter().any(|k| k.key == BIND_ADDRESS_KEY), "{BIND_ADDRESS_KEY}");
    }

    #[test]
    fn the_builder_key_is_the_row_its_service_reads() {
        let name = builder_key_name().expect("the table names it");
        assert!(vike_venues::credential_keys::is_store_key_outside_the_grid(name));
        assert!(!vike_venues::credential_keys::is_platform_key(name), "a deployment row: {name}");
    }

    /// The server's device keys: minted once, held on every later start, a revoked one never re-granted, and the
    /// report names key ids only.
    #[test]
    fn the_server_device_keys_are_minted_once_and_never_regranted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        vike_secrets::create_store(settings.to_str()).expect("store");
        let db = vike_secrets::db_path_in(&settings);

        let first = mint_server_devices(&settings, &db, 1, &|_| {}).expect("mint").join("\n");
        assert!(first.contains("owner/server (admin): minted, key nk-"), "{first}");
        assert!(first.contains("server/backtesthub (observe): minted, key nk-"), "{first}");
        assert!(first.contains("owner/telegram (control): minted, key nk-"), "{first}");
        let telegram = vike_secrets::access::device_key_named(&settings, "owner", "telegram")
            .expect("load")
            .expect("the Telegram channel's own key");
        let listed = vike_secrets::access::find_access_key(&settings, &telegram.key_id);
        assert_eq!(listed.expect("find").expect("granted").role, "control");
        let own = vike_secrets::access::device_key_load(&settings).expect("load").expect("a key");
        assert_eq!((own.person.as_str(), own.device.as_str()), ("owner", "server"));
        let listed = vike_secrets::access::find_access_key(&settings, &own.key_id).expect("find");
        assert_eq!(listed.expect("granted").role, "admin");

        let again = mint_server_devices(&settings, &db, 2, &|_| {}).expect("again").join("\n");
        assert_eq!(again.matches("held; nothing minted").count(), 3, "{again}");

        vike_secrets::access::revoke_access(&settings, "owner", Some("server"), 3).expect("revoke");
        let revoked = mint_server_devices(&settings, &db, 4, &|_| {}).expect("start").join("\n");
        assert!(revoked.contains("owner/server (admin): REVOKED"), "{revoked}");
        assert_eq!(vike_secrets::access::list_access_keys(&settings).expect("list").len(), 3);
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
