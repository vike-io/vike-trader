//! `vike-cli secrets delete KEY --confirm KEY` — take ONE credential out of the store.
//!
//! The owner asked for it in so many words (*"I want to add, edit, delete and save"*), and
//! `docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md` is the
//! record that re-decides `docs/decisions/0036` for this one act. What it does is a SOFT delete: the
//! row the readers answer for is RETIRED (`vike_secrets::retire_credential_in_store`),
//! so the name reads as ABSENT — absent credentials are the live gate, and the account the key
//! belonged to stops being configured at the next restart — while the row, value and all, stays in
//! the database as the evidence of the act and the one way back. Nothing here, or beneath it,
//! erases a row.
//!
//! The fence, one line per reason of 0036's four:
//!
//! * **No VALUE in any form** (reason 2). There is nothing to take: no stdin, no `--from-env`, no
//!   argv. A second positional, an inline `=`, or any unrecognised token is refused with
//!   `DELETE_TAKES_NO_VALUE`, which quotes NOTHING — on this verb the likeliest such token is a
//!   secret typed out of `set`'s habit.
//! * **ONE name the store already holds, refused by name otherwise** (reason 3). A typo cannot
//!   retire a key nobody named: the name must be in the store this verb resolved, and the store
//!   itself refuses again inside its transaction. A hub key is refused before the store is
//!   opened — those are MINTED (`backend setup` / `datahub setup`), and every client of the service
//!   signs with the pair (decision 0051).
//! * **Never a wholesale act** (reason 1). One row, by id, marked; no other row moves, and the
//!   retired row is the evidence. A spelling of a credential that ANOTHER spelling still answers for
//!   is refused, naming it, rather than retired alone or retired together.
//! * **A typed confirm**, `account remove`'s ceremony: `--confirm` must equal the key exactly, and
//!   nothing pre-fills it. `--dry-run` echoes the store, the key and its account, and changes
//!   nothing — it still asks for the confirm, so the rehearsal is the same command minus the write.
//! * **Journalled** — one `credential_retire` record, `Actor::cli`, the key NAME only.
//! * **Not reachable from `mcp`** (reason 4) —
//!   `crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`'s
//!   `the_mcp_surface_advertises_no_credential_writer` carries a needle for the retire writer.
//!
//! **Recovery** is not a verb (decision 0144 records why none was built): the retired row is the
//! newest row of that name whose `superseded_at` is `retired-by-the-operator`, and setting the
//! credential again with `secrets set` is the supported way back — `set --account ID FIELD` for an
//! account's, `set NAME` for a deployment name.
//!
//! **Whose a name is** is read off the STORE, never out of the name (decision 0156 item 6): a name
//! filed against an account row (`vike_secrets::CredentialStore::account_keys`) is that account's,
//! and the echo and the ledger name the row's venue and its tier; any other name belongs to no
//! account — an attribution code is its venue's, everything else the deployment's — and its ledger
//! tier cell is `vike_change_journal::TIER_UNTIERED`.

use std::path::Path;

use vike_change_journal::{Actor, Proc, TIER_UNTIERED};
use vike_secrets::Source;

use super::*;
use crate::exit::{CliError, CmdResult};

/// **Whose the name being deleted is**, as the store files it — the echo's `belongs:` line, the
/// ledger's venue and tier cells, and the way back.
enum Owner {
    /// Filed against this account row: its venue and tier are the row's.
    Account(vike_secrets::Account),
    /// A venue's application row, under the store's own name for it
    /// (`vike_secrets::venue_credential_name`): the venue, and the venue-wide FIELD.
    Venue(&'static str, &'static str),
    /// Filed against no account: the ledger venue cell (an attribution code's venue, or the
    /// deployment).
    NoAccount(&'static str),
}

/// `delete KEY --confirm KEY` — retire ONE credential, journalled. The ORDER is the argument, as
/// it is for [`super::set::run_set`]: every refusal that can be decided from the key alone comes
/// first, then the store, then the echo, then the ceremony, then the act.
pub(super) fn run_delete(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let key = args.key.as_deref().expect("parse refuses `delete` with no key");

    // 1. A hub key is never deleted by name — decided from the name alone, before the store.
    if let Some(service) = vike_venues::credential_keys::platform_key_service(key) {
        return Err(CliError::usage(hub_key_message(key, service)));
    }

    // 2. The store — the PROJECT's, the same resolution `set` and `list` make. None is refused and
    //    nothing is created; an unreadable one is an ERROR, never "nothing to delete".
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let db = vike_secrets::db_path_in(&dir);
    let store = credential_store(ctx.settings_dir, ctx.settings_dir_override, ctx.scope);
    let resolved = store.read_all().map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem \
                 from having none. Nothing was changed."
        ))
    })?;
    if matches!(resolved.source, Source::None) {
        return Err(CliError::failed(format!(
            "no credential store at {} — there is nothing to delete a key from, and this command \
             creates no store. Nothing was changed.",
            db.display()
        )));
    }
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }

    // 3. The name must be one this store holds. Names only: `SecretMap` exposes no value.
    if !resolved.secrets.keys().any(|k| k == key) {
        return Err(CliError::usage(format!(
            "'{key}' is not a key this store holds ({}), so there is nothing to delete. Nothing \
             was changed. `vike-cli secrets list` prints the names it holds.",
            db.display()
        )));
    }

    // 4. The ECHO — the store, the key and whose it is — before anything else is asked, so the
    //    operator checks what they are about to take away against what they meant. WHOSE is the
    //    store's answer (the account row the name is filed against), never a reading of the name.
    let owner = owner_of(&store, key)?;
    let (venue, tier, belongs) = match &owner {
        Owner::Account(row) => (
            row.venue.as_str(),
            row.tier.as_str(),
            format!(
                "{} / {} (account {}{})",
                row.venue,
                row.tier,
                row.id,
                row.label.as_deref().map(|l| format!(", label {l}")).unwrap_or_default()
            ),
        ),
        Owner::Venue(venue, _) => {
            (*venue, TIER_UNTIERED, format!("{venue} (venue-wide: every {venue} account)"))
        }
        Owner::NoAccount(venue) => (*venue, TIER_UNTIERED, format!("{venue} (no account)")),
    };
    println!("store:   {}", db.display());
    println!("key:     {key}");
    println!("belongs: {belongs}");
    println!(
        "⚠ from the next restart this key reads as ABSENT: whatever it configured stops being \
         configured (absent credentials are paper). A RUNNING daemon keeps it until it restarts."
    );

    // 5. THE CEREMONY — `account remove`'s, keyed on the NAME. Missing and mismatched are distinct
    //    messages; neither quotes what was typed, because on this verb a mistyped confirm may be a
    //    pasted secret.
    match args.confirm.as_deref() {
        None => {
            return Err(CliError::usage(format!(
                "`delete` takes a key away — a typed confirm is required: re-send with \
                 --confirm {key}. Nothing was changed."
            )));
        }
        Some(c) if c.trim() != key => {
            return Err(CliError::usage(format!(
                "confirm mismatch: --confirm must equal the key being deleted ({key}) exactly — \
                 nothing was changed."
            )));
        }
        Some(_) => {}
    }

    // 6. The rehearsal stops here, having asked every question the act would.
    if args.dry_run {
        println!(
            "\nthe apply would RETIRE {key}: its row stays in the database as evidence, and every \
             reader reads the key as absent.\nthis was a DRY RUN — nothing was changed in {}.",
            db.display()
        );
        return Ok(());
    }

    // 7. The act and its record, ONE call.
    let (landed, journal_error) = vike_secrets::retire_credential_in_store(
        &dir,
        vike_secrets::Table::Credential,
        key,
        vike_secrets::CredentialJournal {
            actor: Actor::cli("vike-cli"),
            venue,
            tier,
            proc: Proc::current(env!("CARGO_PKG_VERSION")),
            now_ms: ctx.now_ms,
        },
    )
    .map_err(|e| CliError::failed(format!("could not delete {key}: {e}")))?;
    let landed_in = match landed {
        vike_secrets::Backend::Database(db) => db,
        vike_secrets::Backend::Absent => db.clone(),
    };
    if let Some(e) = journal_error {
        eprintln!("vike-cli secrets: ⚠ {key} was deleted, but {e}");
    }
    let way_back = match &owner {
        Owner::Account(row) => format!("vike-cli secrets set --account {} <FIELD>", row.id),
        Owner::Venue(venue, field) => format!("vike-cli secrets set --venue {venue} {field}"),
        Owner::NoAccount(_) => format!("vike-cli secrets set {key}"),
    };
    report(key, &landed_in, &way_back);
    Ok(())
}

/// The success line: what happened, where, and the way back.
fn report(key: &str, store: &Path, way_back: &str) {
    println!(
        "\n{key} deleted from {} — its row is RETIRED, not erased: the value stays in the database \
         as evidence, and `{way_back}` adds the credential again.",
        store.display()
    );
}

/// **Whose `key` is, by the store's filing** — the account row whose credential names include it
/// (names only: `vike_secrets::CredentialStore::account_keys` selects no value); else a venue's
/// application row, when `key` is the store's own name for one (`vike_secrets::venue_credential_name`
/// of a venue-wide field of `vike_venues::credential_schema`); else no account: an attribution code's
/// venue, or the deployment. No tier is read out of a name. A store that answered `read_all` a
/// moment ago and now will not answer this is an error, never a guess.
fn owner_of(store: &vike_secrets::CredentialStore, key: &str) -> CmdResult<Owner> {
    let failed = |e: vike_secrets::SecretsError| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. Nothing was changed."
        ))
    };
    let filed_under = store.account_keys().map_err(failed)?.and_then(|keys| {
        keys.into_iter().find(|(_, k)| k.names.iter().any(|n| n == key)).map(|(id, _)| id)
    });
    if let Some(id) = filed_under {
        let row = match store.accounts().map_err(failed)? {
            vike_secrets::Accounts::Known(rows) => rows.into_iter().find(|a| a.id == id),
            vike_secrets::Accounts::Unanswerable(_) => None,
        };
        if let Some(row) = row {
            return Ok(Owner::Account(row));
        }
    }
    for schema in vike_venues::credential_schema::VENUE_KEY_SCHEMAS {
        if let Some(field) = schema
            .venue_fields()
            .find(|f| vike_secrets::venue_credential_name(schema.venue, f.field) == key)
        {
            return Ok(Owner::Venue(schema.venue, field.field));
        }
    }
    Ok(Owner::NoAccount(
        vike_venues::credential_keys::attribution_key_owner(key)
            .unwrap_or(super::set::DEPLOYMENT_OWNER),
    ))
}

/// The refusal for a hub key: what it is, and the verb that replaces one. Names only.
pub(super) fn hub_key_message(key: &str, service: &str) -> String {
    // ⚠ The verb is chosen per SERVICE, and the gatewayhub is a third one: a two-way `if` would
    // send an operator at the gatewayhub's pair to `backend setup`, the TRADEHUB's minter.
    let verb = match service {
        vike_venues::credential_keys::DATAHUB_SERVICE => "datahub",
        vike_venues::credential_keys::GATEWAYHUB_SERVICE => "gatewayhub",
        _ => "backend",
    };
    format!(
        "'{key}' is a hub key of {service}, and a hub key is not deleted by name: every client \
         of that service signs with the pair, so removing one locks them all out. On the {service} \
         box, `vike-cli {verb} setup` MINTS a new pair, which is how a hub key is replaced. \
         Nothing was changed."
    )
}
