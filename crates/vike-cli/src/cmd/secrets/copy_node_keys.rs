//! `vike-cli secrets copy-node-keys --from-settings-dir DIR [--only tradehub|datahub] [--dry-run]
//! [--replace]` — COPY the node keys out of ANOTHER project's settings database into this one.
//!
//! # Why it exists
//!
//! A node key (`vike_model::credential_keys::PLATFORM_KEYS`: the tradehub and datahub HMAC pairs
//! and the tradehub ADMIN key) authenticates every client of a service by being held IDENTICALLY on
//! both sides. Until this verb there was one way to put one into a box's store: `backend setup` /
//! `datahub setup` (and `backend admin-key`), each of which MINTS a fresh key — so re-homing a
//! service onto a new project, or giving a second box on the same host the pair the first already
//! serves with, broke every client holding the old key. `secrets set` refuses node names on
//! purpose (`docs/decisions/0051-node-keys-live-in-their-own-store.md`; a hand-pasted 256-bit key
//! that is truncated fails as an opaque auth denial), and the credential FILE store a key used to be
//! carried in by hand was removed on 2026-10-07. This verb is the copy that touches no human hand:
//! database to database, in-process.
//!
//! # The fence, and which test holds each part
//!
//! * **NO FILES.** The input is a settings DIRECTORY holding `db/vike.db`, and nothing else is
//!   read; no key file, export or dump is written anywhere, and stdout carries names and counts.
//! * **The SOURCE is opened READ-ONLY** — `vike_secrets::read_table_scoped`, whose open is
//!   `SQLITE_OPEN_READ_ONLY` and creates nothing. ⚠ The ABSENT-source refusal is derived from that
//!   open FAILING, never from a probe ahead of it, so "this verb cannot create the source" is
//!   witnessed by the same call that reads it: `crates/vike-cli/tests/secrets_cli/copy_node_keys.rs`'s
//!   `a_missing_source_is_refused_loudly_and_nothing_is_created_there` goes red the moment that
//!   open could write.
//! * **It reads ONLY node-key rows, and only the names it may copy** — a scoped read of the
//!   `node_key` table over [`selected_names`], so a venue credential never enters this process and
//!   neither does a node-key row this verb has no name for. Decision 0051's NAMESPACE holds on both
//!   sides: node table in, node table out.
//! * **A source it cannot read is an ERROR, never "no keys"** — a missing database, a foreign
//!   schema, a database without a `node_key` table each refuse, naming the path.
//! * **The source cannot be the destination** ([`same_store`]).
//! * **The write is the node-key writer `backend setup` / `datahub setup` already use** —
//!   `vike_secrets::save_credentials_to_store` into `Table::NodeKey`, one call, one transaction, so
//!   a pair is never half-copied. An ABSENT destination store is refused (the shared `open_store`).
//! * **A DIFFERENT value already held is REFUSED, naming the names** ([`Fate::Differs`]); `--replace`
//!   is the operator saying the ROTATION is intended. An identical value is a no-op.
//! * **No VALUE is printed, logged, journalled or errored with** — not even a prefix, and not the
//!   non-reversible `key_id` either: the owner's order for this verb is names and counts only.
//! * **Journalled** — one `credential_write`, `Actor::cli`, the key NAMES written.
//! * **CLI only**: no MCP tool reaches it (`crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs`'s
//!   `the_mcp_surface_advertises_no_credential_writer` holds a needle for this module's entry).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::{Args, Ctx, settings_dir_of};
use crate::exit::{CliError, CmdResult};

/// What happens to ONE node key name. PURE data, so the whole decision is unit-tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fate {
    /// The destination holds no value under this name: the source's is written.
    Add,
    /// The destination holds the SAME value (compared trimmed — the servers trim what they read):
    /// nothing is written.
    Same,
    /// The destination holds a DIFFERENT value. Refused without `--replace`; with it, written.
    Differs,
    /// The source holds no value under this name: nothing to copy.
    AbsentInSource,
}

/// `copy-node-keys` itself. The ORDER is the argument: every refusal that can be decided before
/// the source is read is, the source is read before anything is written, and the write is one call.
pub(super) fn run_copy_node_keys(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let from = args
        .from_settings_dir
        .as_deref()
        .expect("parse refuses `copy-node-keys` with no --from-settings-dir");
    let names = selected_names(args.only.as_deref()).map_err(CliError::usage)?;

    // 1. The DESTINATION — this project's store, which must exist: absent is refused, unreadable is
    //    an error, by the same `open_store` every node-key writer asks.
    let dir = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let dest_db = vike_secrets::db_path_in(&dir);
    let held = crate::cmd::node::open_store(&dir)?;

    // 2. The SOURCE — a directory, never a file, and never this project's own store.
    let src_dir = PathBuf::from(from);
    if !src_dir.is_dir() {
        return Err(CliError::failed(format!(
            "--from-settings-dir {} is not a directory. It names the SOURCE project's settings \
             DIRECTORY (`<project>/settings`, the one holding db/vike.db) — a database is read \
             from it and nothing else; no key file is ever an input. Nothing was written.",
            src_dir.display()
        )));
    }
    let src_db = vike_secrets::db_path_in(&src_dir);
    if same_store(&src_db, &dest_db) {
        return Err(CliError::failed(format!(
            "--from-settings-dir names THIS project's own store ({}) — the source and the \
             destination are the same database, so there is nothing to copy. Nothing was written.",
            dest_db.display()
        )));
    }

    // 3. Read the source: read-only, node_key table only, the selected names only.
    let source = read_source(&src_db, &names)?;
    if source.values().all(|v| v.trim().is_empty()) {
        return Err(CliError::failed(format!(
            "the source store {} holds none of the node keys asked for ({}) — nothing to copy. \
             Nothing was written.",
            src_db.display(),
            names.join(", ")
        )));
    }

    // 4. Decide, purely.
    let steps = plan(&names, &source, &held);
    let differs: Vec<&str> =
        steps.iter().filter(|(_, f)| *f == Fate::Differs).map(|(n, _)| *n).collect();
    let would_write = |f: Fate| f == Fate::Add || (f == Fate::Differs && args.replace);

    // A run about to be REFUSED writes nothing, so its per-name lines are worded as a rehearsal's
    // ("would be added") — a line reading "added" above a refusal would claim a write that did not
    // happen.
    let refusing = !differs.is_empty() && !args.replace;
    println!("copy-node-keys: {} -> {}", src_db.display(), dest_db.display());
    for (name, fate) in &steps {
        println!("  {name}  {}", fate_word(*fate, args.replace, args.dry_run || refusing));
    }
    if !differs.is_empty() && !args.replace {
        return Err(CliError::failed(differs_refusal(&differs, &dest_db)));
    }
    println!("{}", summary(&steps, args.replace, args.dry_run));
    if args.dry_run {
        println!("--dry-run: nothing was written");
        return Ok(());
    }

    let updates: Vec<(String, String)> = steps
        .iter()
        .filter(|(_, f)| would_write(*f))
        .filter_map(|(n, _)| source.get(*n).map(|v| ((*n).to_string(), v.clone())))
        .collect();
    if updates.is_empty() {
        // Every key the source holds is already here, identically. Nothing changed, so nothing is
        // journalled — the ledger records changes, not visits.
        return Ok(());
    }

    // 5. The write — the node-key writer `backend setup` and `datahub setup` call, one call so the
    //    copy lands whole or not at all.
    let landed = vike_secrets::save_credentials_to_store(
        &dir,
        vike_secrets::Table::NodeKey,
        &updates,
        // No account classification: a node key belongs to no venue and no account, and
        // `node_key` is `(name, value)` in every schema — the same `None` `datahub setup` passes.
        None,
    )
    .map_err(|e| CliError::failed(format!("could not write {}: {e}", dest_db.display())))?;
    let store = match landed {
        vike_secrets::Backend::Database(db) => db,
        vike_secrets::Backend::Absent => dest_db.clone(),
    };
    let written: Vec<&str> = updates.iter().map(|(n, _)| n.as_str()).collect();
    record_copy(ctx, &store, &written);
    println!(
        "restart the service whose keys moved: a running vike-tradehub / vike-datahub keeps the \
         keys it booted with"
    );
    Ok(())
}

/// The node key NAMES this run may copy: every [`vike_model::credential_keys::PLATFORM_KEYS`] entry,
/// or one service's when `--only` names it. Taken from the table, never spelled here — the table is
/// what every node-key writer validates against.
pub(super) fn selected_names(only: Option<&str>) -> Result<Vec<&'static str>, String> {
    use vike_model::credential_keys::{
        DATAHUB_SERVICE, PLATFORM_KEYS, TRADEHUB_SERVICE, platform_key_service,
    };
    let service = match only {
        None => None,
        Some("tradehub") => Some(TRADEHUB_SERVICE),
        Some("datahub") => Some(DATAHUB_SERVICE),
        Some(other) => {
            return Err(format!(
                "--only takes `tradehub` or `datahub` (the service whose node keys to copy), not \
                 '{other}'"
            ));
        }
    };
    Ok(PLATFORM_KEYS
        .iter()
        .copied()
        .filter(|k| service.is_none_or(|s| platform_key_service(k) == Some(s)))
        .collect())
}

/// Read the selected node keys out of the SOURCE database — read-only, `node_key` only, scoped.
///
/// ⚠ An ABSENT source is detected by the read-only open FAILING, and only then named as absent:
/// there is deliberately no `is_file` probe ahead of the read, because the property worth having is
/// that the read itself cannot bring a database into existence, and a probe in front of it would
/// hide a reader that could.
fn read_source(src_db: &Path, names: &[&'static str]) -> CmdResult<BTreeMap<String, String>> {
    let scope = vike_secrets::KeyScope::of(names.iter().copied());
    match vike_secrets::read_table_scoped(src_db, vike_secrets::Table::NodeKey, &scope) {
        Ok(map) => Ok(map.into_map().into_iter().collect()),
        Err(_) if !src_db.exists() => Err(CliError::failed(format!(
            "no settings database at {} — --from-settings-dir must name a settings directory \
             holding db/vike.db (a migrated project). Nothing was read and nothing was written.",
            src_db.display()
        ))),
        Err(e) => Err(CliError::failed(format!(
            "the source {} could not be read as a settings store with a node_key table: {e}. That \
             is a store this verb cannot read, not a store with no keys. Nothing was written.",
            src_db.display()
        ))),
    }
}

/// Is `a` the same database file as `b`? Both are canonicalized, so a relative path, a `..` or a
/// symlink into this project's own settings directory is still recognised. A path that does not
/// resolve is not the destination (which exists — `open_store` already said so).
pub(super) fn same_store(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// What happens to each selected name. PURE — values are compared here and never leave.
pub(super) fn plan(
    names: &[&'static str],
    source: &BTreeMap<String, String>,
    held: &HashMap<String, String>,
) -> Vec<(&'static str, Fate)> {
    names
        .iter()
        .map(|name| {
            let src = source.get(*name).map(|v| v.trim()).filter(|v| !v.is_empty());
            let dst = held.get(*name).map(|v| v.trim()).filter(|v| !v.is_empty());
            let fate = match (src, dst) {
                (None, _) => Fate::AbsentInSource,
                (Some(_), None) => Fate::Add,
                (Some(s), Some(d)) if s == d => Fate::Same,
                (Some(_), Some(_)) => Fate::Differs,
            };
            (*name, fate)
        })
        .collect()
}

/// The word printed beside a name. Names only — a fate is a fact about two values, never one.
fn fate_word(fate: Fate, replace: bool, dry_run: bool) -> &'static str {
    match (fate, replace, dry_run) {
        (Fate::Add, _, false) => "added",
        (Fate::Add, _, true) => "would be added",
        (Fate::Same, _, _) => "unchanged (already identical)",
        (Fate::Differs, true, false) => "REPLACED (rotated for every client)",
        (Fate::Differs, true, true) => "would be REPLACED (rotated for every client)",
        (Fate::Differs, false, _) => "DIFFERS — refused without --replace",
        (Fate::AbsentInSource, _, _) => "not in the source (skipped)",
    }
}

/// The count line. Counts only.
fn summary(steps: &[(&'static str, Fate)], replace: bool, dry_run: bool) -> String {
    let count = |f: Fate| steps.iter().filter(|(_, x)| *x == f).count();
    let replaced = if replace { count(Fate::Differs) } else { 0 };
    format!(
        "{}{} added, {replaced} replaced, {} unchanged, {} not in the source",
        if dry_run { "would copy: " } else { "copied: " },
        count(Fate::Add),
        count(Fate::Same),
        count(Fate::AbsentInSource)
    )
}

/// The refusal for a destination holding a DIFFERENT value. PURE; names only.
pub(super) fn differs_refusal(differs: &[&str], dest_db: &Path) -> String {
    format!(
        "{} already in {} with a DIFFERENT value than the source — refusing to replace {} \
         without --replace. Replacing a node key ROTATES it: every client of that service still \
         holding the old key stops authenticating once the server restarts. Nothing was written.",
        differs.join(" and "),
        dest_db.display(),
        if differs.len() == 1 { "it" } else { "them" }
    )
}

/// ONE `credential_write` record for the keys just written — key NAMES only; the ledger's
/// `credential_write` takes no value parameter at all. `venue` and `tier` are empty for the reason
/// `crate::cmd::node`'s `record_credential_write` gives: a node key belongs to no venue. A ledger
/// failure does not fail the call — the keys ARE written.
fn record_copy(ctx: &Ctx<'_>, store: &Path, keys: &[&str]) {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::DB_FILE);
    let change =
        Change::credential_write(Outcome::Applied, Actor::cli("vike-cli"), file, "", "", keys);
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        eprintln!(
            "vike-cli secrets: ⚠ the node keys were copied, but the change journal in {} could not \
             record the write: {e}",
            journal.dir().display()
        );
    }
}
