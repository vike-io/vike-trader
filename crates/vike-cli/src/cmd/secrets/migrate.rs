//! `vike-cli secrets migrate` — the act that CREATES the settings database and moves the
//! credential store into it, reading the files and writing neither.
//!
//! Split out of `cmd/secrets.rs` (code-layout phase 2, task 10); its module doc ("The MIGRATOR")
//! carries why this verb is the one the credential fence admits as a creator. This file holds the
//! only call site of `vike_secrets::migrate` in the workspace, which is what
//! `crates/vike-ops/tests/credential_writer_gate.rs` pins.

use vike_secrets::SECRETS_FILE;

use super::*;
use crate::exit::{CliError, CmdResult};

/// `migrate` — **CREATE `<project>/settings/db/vike.db` and move the credential store into it.**
///
/// The act `docs/decisions/0036` withheld from the tooling and `docs/decisions/0054` made
/// unavoidable: nobody creates a SQLite database in an editor. This module's *The MIGRATOR* section
/// carries the fence argument; what this function adds is the ORDER and the four judgements the
/// types would not make for it.
///
/// 1. **Resolve the settings DIRECTORY the rest of the program uses**, and hand it over explicitly.
///    `vike_secrets::migrate` takes `Option<&str>` and would happily walk from the working directory
///    on a `None` — a second resolution, which is the defect this command already carries a whole
///    section about for `secrets path`. It gets [`settings_dir_of`]'s answer, the same value
///    [`run_set`]'s writer and [`resolve_store`]'s reader take.
/// 2. **Refuse a non-UTF-8 settings path outright.** That signature has no representation for one,
///    and the two dishonest alternatives are worse than a refusal: `to_string_lossy` names a
///    DIFFERENT directory (and would create a credential database in it), while `None` silently
///    substitutes the walk and could create one somewhere else again. Unreachable on any ordinary
///    box; stated because the failure would be a database in the wrong place.
/// 3. **Preview or apply, through ONE library decision.** `--dry-run` calls
///    `vike_secrets::preview`, which is `vike_secrets::migrate` minus the COMMIT — the same
///    classifier on both paths, not a second opinion (`crates/vike-secrets/src/db.rs`'s `plan` for
///    the table-and-file decisions, and its `fill_into` for the per-row ones, run against an
///    in-memory replica). ⚠ This said *minus step 4*, i.e. minus the write, and that was true of a
///    preview that never ran the row classifier at all — which is what made it possible for the dry
///    run to say *would be UPGRADED* and *every key name would still answer* directly above an
///    apply that failed.
/// 4. **Then record**, and a failure there does NOT fail the call — the rows ARE in the database,
///    and sending a caller down an error path for a write that succeeded is worse than a missing
///    ledger line. The same disposition [`set::record_write`] takes, for the same reason.
///
/// # The exit ladder, argued from this file's own rule
///
/// `crates/vike-cli/src/exit.rs`'s rungs, split the way this module already splits them: USAGE means
/// the command line is wrong and re-running it unchanged cannot succeed; FAILED means the command
/// line was fine and the box is not in a state this verb can act on.
///
/// * **`MigrationOutcome::NothingToMigrate` is a SUCCESS** — exit 0, with a line saying no database
///   was created. `vike_secrets::migrate`'s own doc asks for exactly that, and the alternative is
///   worse than untidy: a non-zero rung on an unconfigured box would make `migrate` the one verb
///   that fails on a fresh install for doing precisely the right thing.
/// * **`MigrateError::Ambiguous` is FAILED, not USAGE.** Nothing was written and the operator has
///   work to do, but the work is in two files on this box — no change to this command line fixes it,
///   which is the test the USAGE rung is defined by.
/// * **A `DbError`/`SecretsError` is FAILED** — the store exists and could not be read or written.
/// * **A run that REFUSED individual keys still exits 0**, and that is the one judgement here worth
///   arguing rather than asserting. It is loud (see below), and it is not a failure: every
///   unambiguous key landed, the database is correct for everything it could decide, and each
///   refusal names a key whose resolution needs an operator to edit a file. A non-zero rung would
///   make a converged box fail this verb forever — a permanently red step for a state somebody chose
///   — and the library reached the same verdict first, returning `Ok` with a report rather than an
///   error. ⚠ It is also, by construction, impossible on the run that CREATES the database: the
///   per-key refusal compares a file value against a STORED row, and on a creating run there are
///   none. So the irreversible run always carries everything the files hold.
pub(super) fn run_migrate(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    // 1. The directory the rest of the program uses — never a second walk, and never `--file`
    //    (`parse` refuses that flag here, with the argument at its site).
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    // 2. …and it must be nameable in the one shape the library takes.
    let Some(dir) = settings.to_str() else {
        return Err(CliError::failed(format!(
            "the settings directory {} is not valid UTF-8, and the migration takes its directory as \
             a string — there is no spelling of that path this command could hand it without naming \
             a DIFFERENT directory, which is where it would then create a credential database. \
             Nothing was read and nothing was written. Name a usable directory with \
             $VIKE_SETTINGS_DIR.",
            settings.display()
        )));
    };

    // 3. The one classification, previewed or applied.
    //
    // ⚠ The predicate is the UNION one — `vike_model::credential_keys::is_platform_key`, all four
    // platform names — and NOT one of the two per-service narrowings beside it. `migrate`'s own doc
    // is the authority: 0051's narrowing decides which FILE answers for ONE service's pair at READ
    // time, whereas this is a CLASSIFICATION of every name in the store, and a per-service predicate
    // here would file the OTHER service's pair as a venue credential — into the wrong table, where
    // nothing looks for it, and where `Ambiguity::WrongTable` then refuses to re-decide it because a
    // name has one home.
    let is_node_key = vike_model::credential_keys::is_platform_key;

    // ⚠ The SECOND injected decision. It arrives the same way and — unlike the predicate above —
    // it still arrives for a LAYERING reason: *which ACCOUNT does this key name belong to* is
    // derived by `vike-bridge-core`, which declares `layer = 25` where tier 15's rule is *nothing
    // above rank 10* (machine-checked by `crates/vike-ops/tests/layer_gate.rs`'s
    // `every_tier_15_crate_names_nothing_above_the_vocabulary`), and which declares `vike-secrets`
    // itself, so the edge is a cycle as well as a band violation. That bound is untouched by
    // decision 0072.
    //
    // ⚠ This comment read *"for the same layering reason: `vike-secrets` declares no `vike-*`
    // dependency, so … which needs `vike_model::account_keys` and the venue roster … is a
    // closure"*, and BOTH halves were false. 0072 (accepted 2026-09-20) declared `vike-model` in
    // that crate — `account_keys` and the roster are reachable from it, and its `ensure_venue_rows`
    // iterates `vike_model::venues::VENUES` outright — so it is the rank-25 half, and only that
    // half, carrying this seam. The two seams therefore stopped being one argument:
    // `vike_secrets::migrate`'s own doc carries the 2026-09-26 ruling that keeps the predicate
    // above on grounds that are not a layer bound at all.
    // `vike_bridge_core::credentials::classify_credential_name` is the ONE implementation, and it
    // is the same one `secrets set` passes, so a key migrated in and a key written afterwards are
    // filed identically rather than by two tables that agree today.
    let classify = vike_bridge_core::credentials::classify_credential_name;

    if args.dry_run {
        // ⚠ The SAME `classify` the apply below is handed, and that is the whole of the fix this
        // argument is: the preview used to take `is_node_key` alone, so it could describe which
        // TABLE every name would land in and could not see a single decision
        // `vike_secrets::schema`'s fill makes per row. The measured consequence was a dry run
        // printing *would be UPGRADED* and *every key name would still answer exactly as it does
        // today* directly above an apply that failed.
        let plan =
            vike_secrets::preview(Some(dir), is_node_key, &classify).map_err(migrate_failure)?;
        println!("{plan}");
        report_refusals(
            &plan.refused,
            "would be REFUSED",
            "this run would not carry everything the files hold, and nothing would be overwritten",
        );
        if !plan.would_create() && plan.keys_read() == 0 {
            report_legacy_store(&settings);
        }
        println!(
            "\nthis was a DRY RUN — nothing was created, nothing was written, and both source \
             files were opened read-only. Apply it with:\n  vike-cli secrets migrate"
        );
        return Ok(());
    }

    let done = vike_secrets::migrate(Some(dir), is_node_key, &classify).map_err(migrate_failure)?;

    // 4. The durable record — and only of a run that actually wrote rows. See `record_migration`.
    //
    // ⚠ RECORD BEFORE REPORTING, which is `run_set`'s order and was NOT this function's. `vike-cli`
    // installs no `SIGPIPE` handler, so `vike-cli secrets migrate | head` panics inside the first
    // `println!` — and with the report first, the irreversible act landed with no ledger record at
    // all and exit 101. This is the one write in this file where that trade is unarguable: the run
    // cannot be repeated to produce the record, because a second run is a no-op by construction.
    record_migration(ctx, &done);

    println!("{done}");
    report_refusals(
        &done.refused,
        "were REFUSED and are NOT in the database",
        "this run did not carry everything the files hold, and nothing was overwritten",
    );

    if !done.database_exists() {
        // `MigrationOutcome::NothingToMigrate`. Said again, plainly, because the operator ran a
        // verb whose name promises a database and there is none — and because "nothing to migrate"
        // read as a failure would send somebody looking for a fault that is not there.
        println!(
            "\nnothing to migrate, so NO database was created — that is the correct outcome for a \
             box with no credentials yet, not a failure. Write the store first (`vike-cli secrets \
             template` prints the empty grid) and run this again."
        );
        report_legacy_store(&settings);
    } else if done.created() {
        // ⚠ **THE FILES HAVE STOPPED BEING READ**, and the report above does not say so: it says
        // they were only READ, which is true and is a different fact. This is
        // `vike_secrets::ShadowedStore`'s finding said at the moment it becomes true, and it is the
        // one thing an operator has to carry away from this run — every runbook, skill and refusal
        // message in this tree says *edit `<project>/settings/secrets.env`*, and from this line on
        // that edit changes nothing while looking exactly like it worked.
        //
        // Said only on the CREATING run: on an `Updated` one the box already answered from the
        // database before the command started, so announcing it would report a change that did not
        // happen here.
        println!(
            "\n⚠ from now on the DATABASE above answers for every process on this box. `{}` and \
             `{}` are still on disk, untouched, and are NO LONGER READ — an edit to either changes \
             nothing. Retiring them is your decision and nothing here will do it for you; \
             `vike-cli secrets path` prints both locations, and `vike-cli secrets set KEY` now \
             writes the database.",
            SECRETS_FILE,
            vike_secrets::NODE_FILE
        );
    }
    Ok(())
}

/// **The one finding `nothing to migrate` would otherwise swallow.**
///
/// `vike_secrets::legacy_store_warning` fires for exactly one box: an upgrade from before the
/// one-store rule, whose credentials are still in `<project>/.env` and whose
/// `settings/secrets.env` never appeared. `run_list` already prints it, and says why at the call:
/// *"`no store found — every venue stays paper` is the right answer for a fresh install and a badly
/// misleading one for an upgrade whose `.env` never moved."*
///
/// ⚠ **`migrate` is the verb most likely to BE that upgrade**, and without this it printed the
/// fresh-install sentence — *write the store first* — to the one operator whose store is already
/// written, just in the old place. The library computes the finding either way; this is the call
/// that stops throwing it away. stderr and `⚠`, the same stream and shape as `run_list`'s.
fn report_legacy_store(settings: &std::path::Path) {
    if let Some(w) = vike_secrets::legacy_store_warning(&settings.join(SECRETS_FILE)) {
        eprintln!("⚠ {w}");
    }
}

/// Every [`vike_secrets::MigrateError`] is the ordinary run failure — see [`run_migrate`]'s ladder.
///
/// A function rather than a closure at each call site so the two arms of this verb cannot classify
/// the same failure differently.
fn migrate_failure(e: vike_secrets::MigrateError) -> CliError {
    CliError::failed(e.to_string())
}

/// **The refusals, on STDERR, where a redirected stdout cannot hide them.**
///
/// ⚠ Deliberately duplicating what `Migration`'s own `Display` already printed, and the duplication
/// is the point: a non-empty refusal list means the run did NOT carry everything the files hold
/// while still returning success, and the report it sits inside is a document operators pipe into a
/// file or a ticket. Repeating it on the stream this command already uses for every other finding —
/// stderr, `⚠`, the type's own `Display` — is what keeps it from being a footnote in a page nobody
/// re-reads. Key NAMES only: `Ambiguity`'s `Display` formats a key, a table and a file, never a
/// value.
/// ⚠ **The WHOLE clause is the caller's, not just the verb phrase, and the reason is this verb.**
/// It used to take only `what` and hard-code *"this run did not carry everything the files hold,
/// and nothing was overwritten"* — so a `--dry-run` printed the indicative past tense about a run
/// that had not happened and an overwrite that could not have happened. On the one command whose
/// entire design is *rehearse before the irreversible act*, a rehearsal describing itself as a run
/// is the exact confusion the branch exists to prevent, and `MigrationPlan`'s own `Display` ten
/// lines away keeps the conditional mood rigorously. The stderr copy now matches it.
fn report_refusals(refused: &[vike_secrets::Ambiguity], what: &str, clause: &str) {
    if refused.is_empty() {
        return;
    }
    eprintln!("⚠ {} key(s) {what} — {clause}:", refused.len());
    for a in refused {
        eprintln!("    - {a}");
    }
}

/// **ONE `credential_write` record for the whole migration, and the shape is a judgement.**
///
/// `vike_model::change_journal::Change::credential_write` takes a `store`, a `venue`, a `tier` and a
/// list of key NAMES, and a migration spans every venue and every tier at once. Two shapes were
/// available — one record per `(venue, tier)` group, or one record for the ACT — and this is the
/// second. Why:
///
/// * **The act IS one act.** Every pending row lands in ONE transaction
///   (`crates/vike-secrets/src/db.rs`'s `migrate`), so the write is all-or-nothing. N grouped records
///   would assert N separate changes, and a ledger append that failed partway through them would
///   leave the journal claiming a half-migration that the database cannot be in.
/// * **The grouping vocabulary does not fit this store.** 57 of the 67 key names on the live box are
///   OUTSIDE `vike_model::credential_keys`' enumerable `VENUE × TIER × SUFFIX` grid — the bespoke FX
///   and on-chain shapes, the data-API keys, the platform pair — so `key_owner` answers `None` for
///   most of them and grouping would file the majority under an invented or empty venue.
///   [`run_set`] can group because it REFUSES any name outside the grid first; a migration must not,
///   and carrying the bespoke names is the whole point of it.
/// * **The type already has the vocabulary for one record.** `change_journal::VENUE_MULTI` is the
///   cell for a save that spanned several venues, and `change_journal::TIER_UNTIERED` the cell for a
///   write that governs no single tier. Both are constants on the type that owns the cell so a
///   `jq` reader can learn the field's domain, which is exactly the question a migration record
///   raises.
/// * **The cap stays honest.** `MAX_CREDENTIAL_KEYS` bounds the NAMES at 16 while `count` carries the
///   true total, so a 71-key migration records "16 of 71" rather than claiming 16 was the whole
///   write. One large record says that; N small ones would each look complete.
///
/// The counter-argument, so it is not rediscovered: grouped records would let
/// `jq 'select(.target.venue=="okx")'` find okx's migration. That query cannot work for the 57
/// bespoke names anyway, and the key NAMES in this record carry their venue prefix.
///
/// # What is NOT relaxed
///
/// * **Key NAMES only.** There is no value parameter and none is added —
///   `crates/vike-model/tests/change_journal_credential_values.rs` gates that from the other side.
/// * **`ctx.state_dir == None` means nothing is journalled and the migration still happened.** No
///   project above the working directory means no ledger home, and an append-only record in a guessed
///   directory is worse than none — `vike_boot::journal_boot_settings`' rule, and [`Ctx::state_dir`]
///   states it.
/// * **The `store` cell names the store the write LANDED in**, which here is the database
///   (`vike.db`), taken from `Migration::db` rather than from a constant. The defect that rule comes
///   from is in [`set::record_write`]: a ledger stamped with `secrets.env` for a key that went into the
///   database is an append-only record asserting the wrong store.
/// * **It appends DIRECTLY**, mirroring [`set::record_write`] rather than routing through
///   `vike_connections::save_credentials_journalled` — the layering verdict `docs/decisions/0036`
///   records: the narrowest crate that could see both `vike_model::change_journal` and
///   `vike_secrets` was `vike-bridge-core`, and `vike-app-core` (the wrapper's other caller) had no
///   edge to it, so hoisting the wrapper would have added an edge to a crate that was not asking
///   for one. (That wrapper was deleted on 2026-09-26; its successor is
///   `vike_secrets::save_credentials_to_store_journalled`.)
///
/// # Only a run that WROTE is recorded
///
/// `AlreadyComplete` and `NothingToMigrate` insert nothing, so there is no change to record and a
/// record would assert one. A whole-run REFUSAL never reaches here at all — it is an `Err`, and
/// [`run_set`] does not journal its refusals either.
fn record_migration(ctx: &Ctx<'_>, done: &vike_secrets::Migration) {
    use vike_model::change_journal::{
        Actor, Change, ChangeJournal, Outcome, Proc, TIER_UNTIERED, VENUE_MULTI,
    };

    // ⚠ `inserted()` counts the rows the FILES contributed, and a SCHEMA UPGRADE contributes none
    // of those while rewriting every row in the table — an irreversible act on a store holding
    // live venue keys. `inserted_keys` carries the names either way (`vike_secrets::migrate` folds
    // the reshape's in), so the one thing this guard must not do is return early on the upgrade.
    if done.inserted_keys.is_empty() {
        return;
    }
    // No project above the working directory ⇒ NO ledger, and the migration still happened.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    // The FILE NAME of the store the rows LANDED in — `vike.db` — never a constant and never the
    // file this run merely read.
    let store = done.db.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::DB_FILE);
    // ⚠ The names THIS RUN INSERTED, carried on the report — never a read-back of the table, which
    // on a run that added one key to a sixty-seven-key store would name all sixty-eight and claim
    // this run wrote them. `vike_secrets::Migration::inserted_keys` carries that argument at the
    // field. The cap is the ledger's: `MAX_CREDENTIAL_KEYS` trims the names and `count` keeps the
    // true total, so a large migration records "16 of 71" rather than claiming 16 was the whole
    // write.
    let refs: Vec<&str> = done.inserted_keys.iter().map(String::as_str).collect();
    let change = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        store,
        VENUE_MULTI,
        TIER_UNTIERED,
        &refs,
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The rows ARE in the
        // database, so this is a finding about the ledger and not about the migration.
        eprintln!(
            "vike-cli secrets: ⚠ the migration landed, but the change journal in {} could not \
             record it: {e}",
            journal.dir().display()
        );
    }
}
