//! `vike-cli secrets delete KEY --confirm KEY` — take ONE credential out of the store.
//!
//! The owner asked for it in so many words (*"I want to add, edit, delete and save"*), and
//! `docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md` is the
//! record that re-decides `docs/decisions/0036` for this one act. What it does is a SOFT delete: the
//! row the readers answer for is RETIRED (`vike_secrets::retire_credential_in_store_journalled`),
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
//!   itself refuses again inside its transaction. A NODE key is refused before the store is
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
//! key again with `secrets set KEY` is the supported way back.

use std::path::Path;

use vike_secrets::Source;
use vike_secrets::change_journal::{Actor, Proc};

use super::set::owner_of;
use super::*;
use crate::exit::{CliError, CmdResult};

/// `delete KEY --confirm KEY` — retire ONE credential, journalled. The ORDER is the argument, as
/// it is for [`super::set::run_set`]: every refusal that can be decided from the key alone comes
/// first, then the store, then the echo, then the ceremony, then the act.
pub(super) fn run_delete(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let key = args.key.as_deref().expect("parse refuses `delete` with no key");

    // 1. A NODE key is never deleted by name — decided from the name alone, before the store.
    if let Some(service) = vike_model::credential_keys::platform_key_service(key) {
        return Err(CliError::usage(node_key_message(key, service)));
    }

    // 2. The store — the PROJECT's, the same resolution `set` and `list` make. None is refused and
    //    nothing is created; an unreadable one is an ERROR, never "nothing to delete".
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let db = vike_secrets::db_path_in(&dir);
    let resolved = resolve_store(ctx.settings_dir, ctx.settings_dir_override).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. Nothing was changed."
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
    //    operator checks what they are about to take away against what they meant.
    let (venue, tier) = owner_of(&vike_secrets::credentials::classify_credential_name(key))
        .unwrap_or_else(|| (super::set::DEPLOYMENT_OWNER.to_string(), None));
    println!("store:   {}", db.display());
    println!("key:     {key}");
    println!("belongs: {venue}{}", tier.as_deref().map(|t| format!(" / {t}")).unwrap_or_default());
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
    let (landed, journal_error) = vike_secrets::retire_credential_in_store_journalled(
        &dir,
        vike_secrets::Table::Credential,
        key,
        vike_secrets::CredentialJournal {
            actor: Actor::cli("vike-cli"),
            venue: &venue,
            tier: tier.as_deref().unwrap_or(""),
            proc: Proc::current(env!("CARGO_PKG_VERSION")),
            now_ms: ctx.now_ms,
        },
    )
    .map_err(|e| CliError::failed(format!("could not delete {key}: {e}")))?;
    let store = match landed {
        vike_secrets::Backend::Database(db) => db,
        vike_secrets::Backend::Absent => db.clone(),
    };
    if let Some(e) = journal_error {
        eprintln!("vike-cli secrets: ⚠ {key} was deleted, but {e}");
    }
    report(key, &store);
    Ok(())
}

/// The success line: what happened, where, and the way back.
fn report(key: &str, store: &Path) {
    println!(
        "\n{key} deleted from {} — its row is RETIRED, not erased: the value stays in the database \
         as evidence, and `vike-cli secrets set {key}` adds the key again.",
        store.display()
    );
}

/// The refusal for a NODE key: what it is, and the verb that replaces one. Names only.
pub(super) fn node_key_message(key: &str, service: &str) -> String {
    let verb = if service == "vike-datahub" { "datahub" } else { "backend" };
    format!(
        "'{key}' is a NODE key of {service}, and a node key is not deleted by name: every client \
         of that service signs with the pair, so removing one locks them all out. On the {service} \
         box, `vike-cli {verb} setup` MINTS a new pair, which is how a node key is replaced. \
         Nothing was changed."
    )
}
