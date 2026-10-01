//! `vike-cli backend admin-key` — **MINT the THIRD node key**, the one
//! `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`'s second barrier needs.
//!
//! # Why this is not a flag on `setup`
//!
//! [`super::setup::run_setup`] mints the observe/control PAIR and refuses outright when either is
//! already in the store, because replacing a key some client is signing with has a blast radius
//! rather than being an idempotent re-run. Its escape hatch, `--rotate`, replaces **both**.
//!
//! The box this key is wanted on is, by definition, a box that is already set up — a live daemon
//! with a desktop attached to it. Reaching the admin key through `setup` would therefore mean
//! `setup --rotate --admin` on a production node: the pair changes, and every laptop, container and
//! thin GUI holding the old one fails its next handshake with an `AuthDenied` that reads like a
//! revoked credential. That is a real incident (`docs/ops/tradehub-the CI box.md` records the
//! mirror-image confusion) traded for a key that has nothing to do with the pair.
//!
//! So it is its own verb, it touches exactly one name, and its own `--rotate` costs only what an
//! admin key costs — which today is nothing, because nothing holds one yet.
//!
//! # ⚠ Minting it ARMS NOTHING on its own
//!
//! `crates/vike-tradehub/src/server.rs`'s `account_admission` needs THREE things to hold, and this
//! command supplies one. The other two are the operator's: `config.toml`'s
//! `tradehub_account_admin` declaration, and — under `loopback`, the only value the process can
//! check — a bind that agrees with it. A box that mints this key and declares nothing holds no
//! account writer at all and advertises no `account-verbs` capability, exactly as before.
//!
//! That ordering is deliberate rather than incidental: the key is the part that cannot be undone by
//! editing a file, so it is the part that must not silently turn a surface on.
//!
//! # What never happens here
//!
//! It **never prints the value**, on any path, exactly as `setup` does not — the report carries the
//! key's ID (`super::key_id`) and nothing else. It never accepts one either: there is no `--from`,
//! no stdin read and no environment read, so there is no form in which a key value reaches or
//! leaves this command. And it writes the key NAME to the durable ledger through
//! [`super::record_credential_write`], whose record type takes no value parameter at all.

use crate::exit::{CliError, CmdResult};

use super::{Args, Ctx, key_id, mint_key, open_store, record_credential_write};

/// The name this command mints, taken from the table that VALIDATES it rather than spelled here.
///
/// ⚠ `PLATFORM_KEYS[4]` by index, and the index is safe because that table's own doc pins the order
/// ("ORDER IS LOAD-BEARING … the tradehub pair stays first") and `crates/vike-cli/tests/node_cli.rs`
/// asserts the entries against the server's own constants. A fourth hand copy of the string is the
/// thing this reaches past: `vike_tradehub_client::auth::ADMIN_KEY_ENV` is the server's spelling and
/// `admin_key_name_is_the_servers_own_spelling` below holds this equal to it.
fn admin_key_name() -> &'static str {
    vike_model::credential_keys::PLATFORM_KEYS[4]
}

/// Run `backend admin-key`.
///
/// The same five steps [`super::setup::run_setup`] takes, minus the settings writes it has no
/// business making: validate the name, find the store that ANSWERS, refuse an overwrite, write,
/// journal, report.
pub(super) fn run_admin_key(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let name = admin_key_name();
    // 1. The name, against the table that validates it — the same refusal `validated_names` raises
    //    for the pair, and it is not redundant with the index above: the index says WHICH entry, and
    //    this says the entry is still one a node-key writer may write.
    if !vike_model::credential_keys::is_platform_key(name) {
        return Err(CliError::failed(format!(
            "{name} is not in `vike_model::credential_keys::PLATFORM_KEYS` — this crate's spelling \
             of the admin key name has drifted from the table that validates it. Nothing was \
             written."
        )));
    }

    // 2. The store — the PROJECT's, and the one that ANSWERS. ⚠ The DIRECTORY, not the file: on a
    //    migrated box the settings database's `node_key` table shadows `node.env`, and a refusal
    //    computed against rows nothing reads is a refusal about the wrong store.
    let dir = super::settings_dir(ctx);
    let path = super::landed(ctx, &vike_secrets::backend_in(&dir));
    let existing = open_store(&dir)?;

    // 3. The overwrite refusal.
    if existing.get(name).is_some_and(|v| !v.trim().is_empty()) && !args.rotate {
        return Err(CliError::failed(rotation_refusal(name, &path.display().to_string())));
    }

    // 4. The write. One name, one value, through the one credential writer.
    let minted = mint_key();
    let id = key_id(&minted);
    vike_secrets::save_credentials_to_store(
        &dir,
        vike_secrets::Table::NodeKey,
        &[(name.to_string(), minted)],
        // No account classification, for decision 0051's reason: a node key belongs to no venue and
        // no account, and `node_key` is `(name, value)` in every schema.
        None,
    )
    .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;

    // 5. The durable record — the NAME only.
    record_credential_write(ctx, &path, &[name]);

    for line in report(name, &id, &path.display().to_string()) {
        println!("{line}");
    }
    Ok(())
}

/// The refusal for an admin key already in the store.
///
/// PURE, so the wording is unit-tested rather than reached only by arranging a populated store —
/// the same shape `super::setup`'s own `rotation_refusal` has, and deliberately a SEPARATE sentence:
/// the pair's refusal talks about every client that would stop working, and this one must not, since
/// the blast radius here is one operator's own admin access and not a fleet's.
fn rotation_refusal(name: &str, store: &str) -> String {
    format!(
        "{name} is already in {store}, and this command will not silently replace it.\n\
         ⚠ Re-run with --rotate to mint a new one. What that costs is narrow — the admin key is \
         used by the account verbs alone, so nothing that PLACES ORDERS is affected and no client \
         loses its session. What does break is any desktop already configured with the old admin \
         key: re-run `vike-cli backend connect` there with the new one."
    )
}

/// The report, PURE so its wording is tested without a store.
///
/// ⚠ It states what has NOT happened, and that is the load-bearing half. An operator who has just
/// minted a key called `ADMIN` will reasonably believe the surface is now on; it is not, and the
/// thing that would tell them otherwise is a refusal at a moment they are no longer watching.
fn report(name: &str, id: &str, store: &str) -> Vec<String> {
    vec![
        format!("minted {name} into {store}"),
        format!("  key id: {id}   (the VALUE is not printed, here or anywhere)"),
        String::new(),
        "⚠ THIS ARMS NOTHING BY ITSELF. The account verbs need all three of:".to_string(),
        "     1. this key                                              ✓ done".to_string(),
        "     2. config.toml `tradehub_account_admin` = \"loopback\" or \"contained\"".to_string(),
        "     3. under `loopback`, a bind that agrees — checked at BOOT, and a wide".to_string(),
        "        bind REFUSES the capability rather than arming it".to_string(),
        String::new(),
        "Until 2 is declared the daemon holds no account writer, advertises no".to_string(),
        "`account-verbs` capability, and refuses every account frame. Restart the".to_string(),
        "daemon after declaring it — the declaration is read at boot.".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ **THE SPELLING, against the SERVER's own constant.** This crate mints the key; the daemon
    /// authenticates against it. If the two names drift, the mint writes a row the server never
    /// reads and every account frame is refused with `account_admission`'s "no admin key" arm —
    /// which reads like a missing declaration rather than like a typo.
    #[test]
    fn admin_key_name_is_the_servers_own_spelling() {
        assert_eq!(admin_key_name(), vike_tradehub_client::auth::ADMIN_KEY_ENV);
    }

    /// …and it is a name a node-key writer is allowed to write. `is_platform_key` is what
    /// `save_credentials_to_store` is trusted against; a name outside that table would be refused
    /// at the write, after the settings half of a setup had already happened.
    #[test]
    fn the_admin_key_is_a_validated_platform_name() {
        assert!(vike_model::credential_keys::is_platform_key(admin_key_name()));
    }

    /// ⚠ **…and it is NOT part of the PAIR predicate**, which is the whole reason
    /// `vike_model::credential_keys::is_tradehub_node_key` stopped being
    /// `platform_key_service(k) == Some(TRADEHUB_SERVICE)`. A `node.env` holding only this key must
    /// not become *the answer* for where the observe/control pair is read from — that is the
    /// measured `bad mac` generator that predicate's own doc describes.
    #[test]
    fn the_admin_key_does_not_decide_where_the_pair_is_read_from() {
        assert!(!vike_model::credential_keys::is_tradehub_node_key(admin_key_name()));
        // …while still routing an operator to the right command, which is the other consumer.
        assert_eq!(
            vike_model::credential_keys::platform_key_service(admin_key_name()),
            Some(vike_model::credential_keys::TRADEHUB_SERVICE)
        );
    }

    /// The report says what has NOT happened. An operator who mints a key called ADMIN and is not
    /// told the surface is still closed will believe it is open.
    #[test]
    fn the_report_says_the_surface_is_still_closed() {
        let lines = report("VIKE_TRADEHUB_ADMIN_KEY", "abc123", "/x/vike.db").join("\n");
        assert!(lines.contains("ARMS NOTHING"), "{lines}");
        assert!(lines.contains("tradehub_account_admin"), "names the declaration: {lines}");
        assert!(lines.contains("Restart"), "names the restart: {lines}");
        assert!(!lines.contains("key id: VIKE"), "the id is not the name: {lines}");
    }

    /// The refusal names `--rotate` and, unlike the PAIR's, does NOT threaten a fleet — the blast
    /// radius really is narrower and a message that overstated it would buy a needless hesitation.
    #[test]
    fn the_rotation_refusal_names_the_flag_and_the_narrow_cost() {
        let msg = rotation_refusal("VIKE_TRADEHUB_ADMIN_KEY", "/x/vike.db");
        assert!(msg.contains("--rotate"), "{msg}");
        assert!(msg.contains("nothing that PLACES ORDERS is affected"), "{msg}");
    }
}
