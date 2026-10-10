//! `vike-cli secrets init` — the act that CREATES the settings database, EMPTY.
//!
//! Split out of `cmd/secrets.rs` (code-layout phase 2, task 10); its module doc ("The CREATOR")
//! carries why this verb is the one the credential fence admits as a creator. This file holds the
//! only call site of `vike_secrets::create_store` in the workspace, which is what
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins.
//!
//! ⚠ **It journals nothing.** The one run that writes creates the EMPTY store and writes no key, so
//! there is no `credential_write` to record.

use super::*;
use crate::exit::{CliError, CmdResult};

/// `init` — **CREATE `<project>/settings/db/vike.db` EMPTY.**
///
/// The act `docs/decisions/0036` withheld from the tooling and `docs/decisions/0054` made
/// unavoidable: nobody creates a SQLite database in an editor. This module's *The CREATOR* section
/// carries the fence argument; what this function adds is the ORDER and the judgements the types
/// would not make for it.
///
/// 1. **Resolve the settings DIRECTORY the rest of the program uses**, and hand it over explicitly.
///    `vike_secrets::create_store` takes `Option<&str>` and would happily walk from the working
///    directory on a `None` — a second resolution, which is the defect this command already carries
///    a whole section about for `secrets path`. It gets [`settings_dir_of`]'s answer, the same value
///    [`run_set`]'s writer and [`resolve_store`]'s reader take.
/// 2. **Refuse a non-UTF-8 settings path outright.** That signature has no representation for one,
///    and the two dishonest alternatives are worse than a refusal: `to_string_lossy` names a
///    DIFFERENT directory (and would create a credential database in it), while `None` silently
///    substitutes the walk and could create one somewhere else again. Unreachable on any ordinary
///    box; stated because the failure would be a database in the wrong place.
/// 3. **Preview or apply, through ONE library probe.** `--dry-run` calls
///    `vike_secrets::preview_create_store`, which is `vike_secrets::create_store` minus the write —
///    the same probe on both paths, not a second opinion (`crates/vike-secrets/src/db/create.rs`'s
///    `probe`).
///
/// # The exit ladder
///
/// `crates/vike-cli/src/exit.rs`'s rungs: USAGE means the command line is wrong and re-running it
/// unchanged cannot succeed; FAILED means the command line was fine and the box is not in a state
/// this verb can act on.
///
/// * **A created store and an existing one are both a SUCCESS** — exit 0; an existing store is the
///   ordinary no-op.
/// * **A `vike_secrets::DbError` is FAILED** — the store exists and could not be read or written (a
///   store at any schema but the current one included), and no change to this command line fixes it.
pub(super) fn run_init(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    // 1. The directory the rest of the program uses — never a second walk, and never `--file`
    //    (`parse` refuses that flag here, with the argument at its site).
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    // 2. …and it must be nameable in the one shape the library takes.
    let Some(dir) = settings.to_str() else {
        return Err(CliError::failed(format!(
            "the settings directory {} is not valid UTF-8, and the creator takes its directory as \
             a string — there is no spelling of that path this command could hand it without naming \
             a DIFFERENT directory, which is where it would then create a credential database. \
             Nothing was read and nothing was written. Name a usable directory with \
             $VIKE_SETTINGS_DIR.",
            settings.display()
        )));
    };

    // 3. The one decision, previewed or applied.
    if args.dry_run {
        let plan = vike_secrets::preview_create_store(Some(dir)).map_err(create_failure)?;
        println!("{plan}");
        if plan.would_create {
            println!("\n{}", created_consequence(Mood::Would));
        }
        println!(
            "\nthis was a DRY RUN — nothing was created and nothing was written. Apply it with:\n  \
             vike-cli secrets init"
        );
        return Ok(());
    }

    let done = vike_secrets::create_store(Some(dir)).map_err(create_failure)?;
    println!("{done}");
    if done.created {
        // ⚠ Said in full because this is the one creating run whose report has no rows to look at:
        // the consequence is the whole of what it did.
        println!("\n{}", created_consequence(Mood::Did));
    }
    Ok(())
}

/// Which mood [`created_consequence`] speaks in — the rehearsal's or the run's. The same split
/// `vike_secrets::StoreCreationPlan` keeps against `vike_secrets::StoreCreation`, for the same
/// reason: a dry run that described itself in the past tense is the confusion a rehearsal exists to
/// prevent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mood {
    Would,
    Did,
}

/// **What creating the store costs, said at the moment it is paid (or would be).**
///
/// The EMPTY database answers for every credential on the box from then on, and the way a
/// credential gets in is `secrets set` — value on stdin or from a named environment variable, never
/// argv (`docs/decisions/0036`). One function for both moods so the rehearsal and the run cannot
/// describe two different acts.
fn created_consequence(mood: Mood) -> String {
    let (created, answers) = match mood {
        Mood::Would => ("would be created EMPTY", "From then on it would answer"),
        Mood::Did => ("was created EMPTY", "From now on it answers"),
    };
    format!(
        "⚠ the settings database above {created} — no credential, no node key, no account. \
         {answers} for EVERY credential on this box. Add each credential to it with `vike-cli \
         secrets set KEY` — the value on stdin, or from a named environment variable with \
         `--from-env NAME`, never on the command line. Until one is added, no venue on this box \
         has a credential to leave paper with."
    )
}

/// Every [`vike_secrets::DbError`] is the ordinary run failure — see [`run_init`]'s ladder.
///
/// A function rather than a closure at each call site so the two arms of this verb cannot classify
/// the same failure differently.
fn create_failure(e: vike_secrets::DbError) -> CliError {
    CliError::failed(e.to_string())
}
