//! `vike-cli account copy-credentials --from-id N --to-id M --confirm M [--replace]
//! [--dry-run]` — put the credential fields account row N holds onto a SECOND account row M of the
//! same venue and tier, inside the store, with no value ever leaving this process.
//!
//! # Why it exists
//!
//! Two engines can trade one venue login (bybit's linear and inverse lanes on the same demo keys),
//! and each engine is an account row, so the second row needs the first row's fields. Before this
//! verb the only way to give it them was `secrets set --id M FIELD`, which needs a person at a
//! terminal HOLDING the value — the act the owner's rule (keys live only in SQLite; nobody sees,
//! pastes or files one) exists to refuse. This verb is the copy that touches no human hand: store to
//! store, in-process, the `copy-hub-keys` shape for an account's fields.
//!
//! # The fence, and which test holds each part
//!
//! The tests are `crates/vike-cli/tests/cli_verbs/secrets_cli/copy_credentials.rs`'s, over the
//! shipped binary.
//!
//! * **No VALUE in any form** — none is taken (the operands are two row ids, a typed confirm and two
//!   switches), and none is printed, logged, journalled or errored with: stdout carries field NAMES
//!   and counts, the record carries row NAMES (`the_copy_lands_both_fields_and_no_value_reaches_a_stream_or_the_ledger`).
//! * **Every refusal is decided before a value is read** — the ids, FROM == TO, both rows existing,
//!   the venue, the tier and the typed confirm are all asked of the key-less reads
//!   (`CredentialStore::accounts`, `account_keys`).
//! * **One venue, one tier** — a venue's keys sign only there, and a DEMO key never lands on a LIVE
//!   row, nor the reverse (`a_demo_key_never_lands_on_a_live_row_nor_the_reverse`).
//! * **A field the destination already holds is refused BY NAME** unless `--replace`, and with it
//!   only the source's fields are written: every other field of the destination is untouched
//!   (`a_field_the_destination_holds_is_refused_without_replace_and_replaced_alone_with_it`).
//! * **ONE writer call, ONE transaction** — `vike_secrets::save_owned_credentials`, the call
//!   `secrets set --id` makes (`CredentialUpsertIntent::Add`: a missing field is inserted under the
//!   store's own name for it, a held one replaced in place). It creates no account, chooses no tier
//!   and refuses a row removed in between (`NoSuchAccount`).
//! * **Journalled by that writer** — one `credential_write`, `ChangeActor::cli`, the destination's venue
//!   and tier, the row NAMES written.
//! * **CLI only** — no MCP tool reaches it: `crates/vike-mcp/src/tests/credential_fence.rs`'s
//!   `the_mcp_surface_advertises_no_credential_writer` holds a needle for this module.
//!
//! ⚠ The value copied is the one every reader is handed — `vike_secrets::account_credentials::CredentialFields::get`,
//! TRIMMED — so a value stored with surrounding whitespace lands without it. No reader in the
//! workspace sees a difference: every one trims.
//!
//! The reads are made under the process's declared scope, so under `VIKE_NO_LIVE_KEYS=true` a live
//! row reads as holding nothing and the copy is refused as having nothing to copy.

use std::path::Path;

use vike_change_journal::ChangeActor;

use super::super::*;
use super::{arming_consequence, echo_row};
use crate::exit::{CliError, CommandResult};

/// What `copy-credentials` without both row ids is refused with.
const MISSING_IDS: &str = "`account copy-credentials` needs --from-id N (the account whose \
credential fields are copied) and --to-id M (the account that receives them), and --confirm M:\n  \
vike-cli account copy-credentials --from-id 4 --to-id 9 --confirm 9 --dry-run\nRun \
`vike-cli account ls` for the ids. Nothing was written.";

/// `account copy-credentials` itself. The ORDER is the argument: every refusal the key-less reads
/// can decide comes first, the two rows' fields are read only after, and the write is one call.
pub(super) fn run_copy_credentials(
    args: &Args,
    ctx: &Context<'_>,
    rows: &[vike_secrets::AccountRow],
    credentials: &vike_secrets::CredentialStore,
    settings: &Path,
    store: &Path,
) -> CommandResult<()> {
    let (Some(from), Some(to)) = (args.from_id, args.to_id) else {
        return Err(CliError::usage(MISSING_IDS));
    };
    if from == to {
        return Err(CliError::usage(format!(
            "--from-id and --to-id are both {from}: an account already holds its own credentials. \
             Nothing was written."
        )));
    }
    let from_row = row_of(rows, from, "--from-id")?;
    let to_row = row_of(rows, to, "--to-id")?;
    println!("from:");
    echo_row(credentials, from_row);
    println!("to:");
    echo_row(credentials, to_row);

    if from_row.venue != to_row.venue {
        return Err(CliError::usage(format!(
            "account {from} is a {} account and account {to} a {} one: a venue's keys sign only on \
             that venue. Nothing was written.",
            from_row.venue, to_row.venue
        )));
    }
    if from_row.tier != to_row.tier {
        return Err(CliError::usage(format!(
            "account {from} is at tier `{}` and account {to} at tier `{}`: credentials never cross \
             tiers — a demo key must not land on a live row, nor the reverse. Nothing was written.",
            from_row.tier, to_row.tier
        )));
    }
    // ⚠ THE CEREMONY, `account rm`'s shape: the destination is an integer with no roster behind
    // it, and an ACTIVE row that receives a complete key set starts trading within a second on a
    // running tradehub. Missing and mismatched get distinct messages; nothing pre-fills it.
    match args.confirm.as_deref() {
        None => {
            return Err(CliError::usage(format!(
                "`account copy-credentials` writes credentials onto account {to} — a typed confirm \
                 is required: re-send with --confirm {to}. Nothing was written."
            )));
        }
        Some(c) if c.trim() != to.to_string() => {
            return Err(CliError::usage(format!(
                "confirm mismatch: --confirm must equal the exact id being written (--to-id {to}) \
                 — nothing was written."
            )));
        }
        Some(_) => {}
    }

    // The values, read only now: the source's fields, and the destination's (for their NAMES).
    let source = account_fields(credentials, from, &from_row.venue, store)?;
    let names: Vec<&str> = source.fields().collect();
    if names.is_empty() {
        return Err(CliError::failed(format!(
            "account {from} holds no credential field this process may read — nothing to copy (a \
             `live` row reads as empty under VIKE_NO_LIVE_KEYS=true). Nothing was written."
        )));
    }
    let held_by_to = account_fields(credentials, to, &to_row.venue, store)?;
    let held: Vec<&str> = names.iter().copied().filter(|f| held_by_to.get(f).is_some()).collect();

    // A run about to be REFUSED writes nothing, so its lines read as a rehearsal's.
    let refusing = !held.is_empty() && !args.replace;
    let rehearsal = args.dry_run || refusing;
    println!("copy-credentials: account {from} -> account {to} in {}", store.display());
    for field in &names {
        println!("  {field}  {}", fate_word(held.contains(field), args.replace, rehearsal));
    }
    if refusing {
        return Err(CliError::failed(format!(
            "account {to} already holds {} — refusing to replace {} without --replace (the \
             destination's other fields are never touched either way). Nothing was written.",
            held.join(" and "),
            if held.len() == 1 { "it" } else { "them" }
        )));
    }
    let replaced = if args.replace { held.len() } else { 0 };
    println!(
        "{}{} added, {replaced} replaced",
        if args.dry_run { "would copy: " } else { "copying: " },
        names.len() - replaced
    );
    // The consequence that lands ELSEWHERE, said on the rehearsal too.
    if to_row.active {
        println!("{}", arming_consequence(&to_row.venue, to_row.tier, &to.to_string()));
    } else {
        println!(
            "account {to} is INACTIVE, so it trades nothing with these keys until `vike-cli \
             account activate --id {to}`."
        );
    }
    if args.dry_run {
        println!(
            "\nthis was a DRY RUN — nothing was written to {}. Apply it with the same command \
             without --dry-run.",
            store.display()
        );
        return Ok(());
    }

    let pairs: Vec<(String, String)> = names
        .iter()
        .filter_map(|f| source.get(f).map(|v| ((*f).to_string(), v.to_string())))
        .collect();
    let (done, journal_error) = vike_secrets::save_owned_credentials(
        settings,
        &vike_secrets::CredentialOwner::Account(to),
        &pairs,
        // `Add`: a field the destination does not hold is inserted under the store's own name for
        // it; one it holds (only reachable under --replace) is replaced in place, row and name kept.
        vike_secrets::CredentialUpsertIntent::Add,
        account_journal(ctx, ChangeActor::cli("vike-cli")),
    )
    // The store's `Display`: a path, a field or an id, a reason — never a value.
    .map_err(|e| CliError::failed(format!("could not write {}: {e}", store.display())))?;
    report_unrecorded_write(
        &format!("vike-cli account: ⚠ the credentials copied onto account {to} were saved"),
        "it",
        journal_error,
    );
    println!(
        "\ncopied {} field(s) of account {from} onto account {to} in {} (rows {})",
        done.names.len(),
        store.display(),
        done.names.join(", ")
    );
    // Account `to` signs with what it now holds: the running gatewayhub reconnects it (decision 0287).
    super::super::running::tell_each(ctx, [to]);
    Ok(())
}

/// The row `id` names, or the refusal naming which flag carried the id: an id no row carries is a
/// typo, never an instruction to create one.
fn row_of<'a>(
    rows: &'a [vike_secrets::AccountRow],
    id: i64,
    flag: &str,
) -> CommandResult<&'a vike_secrets::AccountRow> {
    rows.iter().find(|a| a.id == id).ok_or_else(|| {
        CliError::usage(format!(
            "{flag} {id}: no account with that id in this store — and none was created. `vike-cli \
             account ls` lists the {} row(s) it holds. Nothing was written.",
            rows.len()
        ))
    })
}

/// ONE account's credential fields, through the handle's one-account read (no other account's row
/// is selected), under the process's scope.
fn account_fields(
    credentials: &vike_secrets::CredentialStore,
    id: i64,
    venue: &str,
    store: &Path,
) -> CommandResult<vike_secrets::account_credentials::CredentialFields> {
    let book = credentials.credential_registry_for(Some(id), venue).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store {} is THERE and could not be read. Nothing was written.",
            store.display()
        ))
    })?;
    Ok(book.account(id).cloned().unwrap_or_default())
}

/// The word printed beside a field NAME. Names only — a fate is a fact about two rows, never a value.
fn fate_word(held: bool, replace: bool, rehearsal: bool) -> &'static str {
    match (held, replace, rehearsal) {
        (false, _, false) => "added",
        (false, _, true) => "would be added",
        (true, true, false) => "REPLACED",
        (true, true, true) => "would be REPLACED",
        (true, false, _) => "HELD by the destination — refused without --replace",
    }
}
