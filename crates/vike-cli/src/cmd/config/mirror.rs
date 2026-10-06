//! `vike-cli config mirror` — **write one of this box's PROFILE documents (a run profile, a daemon
//! profile or a recorder profile) into the settings database as rows.**
//!
//! ⚠ **This verb used to have a second half — copying the four settings TOMLs
//! (`policy`/`config`/`preferences`/`flags`) into the `setting`/`venue_arming` tables — and that
//! half is DELETED (`docs/decisions/0086`: settings live only in the database, and a write is one
//! row).** There are no settings files any more, as source, fallback, export or way back; a settings
//! key is written one row at a time by `vike-cli config set`, never by mirroring a file. What
//! survives here is the PROFILE half, which 0086 leaves as a going concern for a separate migration
//! (its *Phasing* names `recorder.toml` becoming recorder profile rows as follow-on work, not this
//! record's): a run/daemon/recorder PROFILE is still a document an operator hands this verb, because
//! nothing yet replaces that document with a row-native editor.
//!
//! # `--profile` / `--daemon` / `--recorder` — the PROFILE planes, and THE ROWS BIND
//!
//! ⚠ **This section said the opposite until this landing, and the sentence it printed in `--help`
//! was the whole reason the profile plane never migrated:** *"Nothing reads the rows: the profile
//! FILE still judges every order."* That was true and it was not an accident — the flag wrote to
//! `profile_risk`, a DISCLOSURE mirror of one `[risk]` table with no `active` column, whose reader
//! was forbidden by name (`crates/vike-ops/tests/settings/profile_risk_readers_gate.rs`). Building one was a
//! test failure by design.
//!
//! `--profile` now stores the run profile's whole BODY on the Phase-3 plane
//! (`vike_secrets::profile_store`), `--daemon` does the same for the daemon profile, and
//! **`crates/vike-tradehub/src/tradehub_cli.rs` reads both at boot** — an ACTIVE `run` row is what
//! the daemon builds its pre-trade ceilings from, ahead of `VIKE_RUN_PROFILE`. The lowering, the
//! round-trip fence and the argument for the plane are in `crate::cmd::config::mirror_profile`.
//!
//! **Storing is still not selecting.** Every profile flag here writes a BODY and never an active
//! row: `plan_active_row` is consulted, reported, and answers `Withhold { NothingSelectedToday }`
//! on every box, because this process cannot see what is in force (`VIKE_RUN_PROFILE` reaches the
//! daemon through its unit's `EnvironmentFile=`, `--config` through its `ExecStart=`).
//! `vike-cli config activate <kind> <name> --proves <file>` is the deliberate act, and it has its
//! own fence. That is also why every one of these flags is explicit rather than defaulted: having
//! the mirror pick a profile out of ITS OWN environment would be 0057's Question 3 answered
//! sideways, in the one verb that writes.
//!
//! # Why the writer is a CLI verb and not something a daemon does
//!
//! MEASURED in the tree: **no shipped `.service` under `deploy/` grants `settings/db`**, so the
//! database is READ-ONLY to both daemons inside their own mount namespaces. Reads need no grant and
//! the composition roots already perform one; a WRITE needs the grant 0054 decided and the ceiling
//! above it. So the only process that can write this store today is an operator shell OUTSIDE the
//! daemon's namespace running as the same account — `vike-cli`, or the deploy pipeline.
//!
//! # What it refuses
//!
//! * **A project with no database.** `store_profile` refuses rather than creating one, and the
//!   reason is the live gate rather than tidiness: the mere EXISTENCE of
//!   `<project>/settings/db/vike.db` is the whole of `vike_secrets::Backend`'s per-run choice, so
//!   creating it here would make every credential in `secrets.env` unread in the same act.
//!   `vike-cli secrets migrate` is the one thing that may create it.
//!
//! # ⚠ Not a credential write, and deliberately not journalled as one
//!
//! This verb writes no credential and touches no credential file — it does not appear in
//! `crates/vike-ops/tests/credentials/credential_writer_gate.rs`'s `WRITER_CALLERS` for that reason. It also
//! records no `set_setting` journal row: a mirror states no NEW value, it copies a document an
//! operator already filed, and a journal row per mirrored field would bury the real edits the
//! journal exists to carry. `config set` is the verb that journals, and it writes one settings row.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cmd::args::{self, Flags};

/// The verb's own usage, printed by `--help` and by a parse error.
///
/// ⚠ **It said "The files keep winning; nothing this box resolves changes" FLATLY, and on an
/// ADOPTED box that is false in both halves.** After `config adopt` the ROWS are what the box
/// resolves and the four files are inert drafts, so mirroring writes the box's effective values —
/// which is exactly what [`run`]'s own dry-run branch says in as many words, three screens below a
/// page telling the reader the opposite. Measured on the CI box 2026-09-21: the help promised no change
/// while the dry-run of the same invocation warned of one.
///
/// The page now states the RULE WITH ITS CONDITION rather than one side of it as if universal. It
/// cannot branch — it is a `const` — so it names both states and points at the run itself, which
/// can and does.
pub const USAGE: &str = "usage: vike-cli config mirror [--dry-run]\n             [--profile \
                         <file>] [--profile-name <name>]\n             [--daemon <file>] \
                         [--daemon-name <name>]\n             [--recorder <file>] \
                         [--recorder-name <name>]\n\n  Store one of this box's PROFILE documents \
                         (a run profile, a daemon profile or a\n  recorder profile) as rows. There \
                         are no settings FILES any more (0086) — a\n  settings key is written one \
                         row at a time by `vike-cli config set`, never mirrored\n  here. At least \
                         one profile flag is required.\n\n  A PROFILE flag stores that document's \
                         whole BODY as rows and selects nothing.\n  Each is REFUSED unless the \
                         rows render back to the file they came from.\n  `vike-cli config activate \
                         <kind> <name> --proves <file>` is what makes a body\n  the one this box \
                         READS — a separate, deliberate act.\n\n  --profile <file>   the RUN \
                         profile (run-live.toml): mode, [risk], [sinks], [guards].\n              \
                         ⚠ THE ROWS BIND once activated — an active `run` row is what the\n        \
                         daemon builds its pre-trade ceilings from, ahead of \
                         VIKE_RUN_PROFILE.\n  --profile-name <name>   the NAME the run body is \
                         stored under (default: the file stem)\n  --daemon <file>    the DAEMON \
                         profile (tradehub.toml): the mount rows and [daemon].\n  --daemon-name \
                         <name>    the NAME the daemon body is stored under (default: the file \
                         stem)\n  --recorder <file>  the RECORDER profile as `recorder` + \
                         `subscription` rows\n  --recorder-name <name>  the profile NAME the rows \
                         are stored under (default `default`)\n  --dry-run          report what \
                         would be written, and write nothing";

/// Parsed flags. `Debug` so a parser test can `unwrap_err` against it.
#[derive(Debug, Default)]
struct Args {
    dry_run: bool,
    /// The RUN profile FILE whose whole body to store, if any.
    ///
    /// ⚠ **It used to be a `Vec` and it is not one any more**, because what it writes changed: a
    /// `[risk]` DISCLOSURE row keyed by file name could sensibly be mirrored for several profiles
    /// at once, while a BODY that an operator then activates by name is one document at a time. A
    /// second `--profile` is refused rather than resolved, exactly as `--recorder`'s is.
    profile: Option<PathBuf>,
    /// The profile NAME the run body is stored under.
    profile_name: Option<String>,
    /// The DAEMON profile FILE whose whole body to store, if any.
    daemon: Option<PathBuf>,
    /// The profile NAME the daemon body is stored under.
    daemon_name: Option<String>,
    /// The recorder profile FILE to store as rows, if any. **Explicit, never defaulted to
    /// `<settings>/recorder.toml`** — for the same reason every profile flag here is explicit:
    /// having the mirror quietly pick up a file that decides which venue feeds open would be a
    /// migration arming something as a side effect of a tidy-up.
    recorder: Option<PathBuf>,
    /// The profile NAME the recorder rows are stored under.
    recorder_name: Option<String>,
}

impl Args {
    /// Was any profile plane named at all? `--no-settings` alone configures nothing.
    fn any_profile(&self) -> bool {
        self.profile.is_some() || self.daemon.is_some() || self.recorder.is_some()
    }
}

/// Refuse a second occurrence of a one-at-a-time file flag, naming the `-name` flag that is the
/// real answer. Spelled once so the three read identically.
fn once(slot: &mut Option<PathBuf>, flag: &str, value: String) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!(
            "{flag} given twice. Each profile is stored under a NAME, so mirror them one at a time \
             with {flag}-name."
        ));
    }
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--dry-run" => {
                args::no_value(&flag, inline)?;
                out.dry_run = true;
            }
            "--profile" => {
                let v = flags.value(&flag, inline)?;
                once(&mut out.profile, "--profile", v)?;
            }
            "--profile-name" => out.profile_name = Some(flags.value(&flag, inline)?),
            "--daemon" => {
                let v = flags.value(&flag, inline)?;
                once(&mut out.daemon, "--daemon", v)?;
            }
            "--daemon-name" => out.daemon_name = Some(flags.value(&flag, inline)?),
            "--recorder" => {
                let v = flags.value(&flag, inline)?;
                once(&mut out.recorder, "--recorder", v)?;
            }
            "--recorder-name" => out.recorder_name = Some(flags.value(&flag, inline)?),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    for (name, name_flag, file, file_flag) in [
        (&out.profile_name, "--profile-name", &out.profile, "--profile"),
        (&out.daemon_name, "--daemon-name", &out.daemon, "--daemon"),
        (&out.recorder_name, "--recorder-name", &out.recorder, "--recorder"),
    ] {
        if name.is_some() && file.is_none() {
            return Err(format!(
                "{name_flag} names the profile {file_flag} stores, so it needs {file_flag} <file>. \
                 Alone it configures nothing."
            ));
        }
    }
    if !out.any_profile() {
        return Err(
            "nothing named to mirror — there are no settings FILES any more (0086), so at least \
             one of --profile / --daemon / --recorder is required."
                .to_string(),
        );
    }
    Ok(out)
}

/// **The NAME a profile body is stored under when none is given: the file's STEM.**
///
/// ⚠ NOT the file NAME, which is what the retired `[risk]` mirror keyed on. A body's name is what
/// an operator types into `vike-cli config activate <kind> <name>`, and a `.toml` inside it reads
/// as a path — which is precisely the path-versus-name confusion 0057's Question 3 warns about, in
/// the one verb where getting it wrong selects the wrong document. A path with no stem renders as
/// whatever was typed, so the operator sees their own argument rather than an empty string.
fn default_name(file: &Path) -> String {
    file.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.display().to_string())
}

/// Entry point the `config` dispatcher routes to.
pub fn run(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("config mirror", USAGE, &msg),
    };
    match execute(&args, settings_dir) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config mirror: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn execute(args: &Args, settings_dir: Option<&Path>) -> Result<String, String> {
    let Some(dir) = settings_dir else {
        return Err(
            "no settings directory resolved — there is nothing to mirror. Set VIKE_SETTINGS_DIR, \
             or run from a project that has one."
                .to_string(),
        );
    };

    // ── THE NAMES, AND THE ONE COLLISION A PLAN CANNOT SEE ──────────────────────────────────
    //
    // Resolved FIRST — above the settings half, above every plan, above every write — because the
    // refusal below is a pure function of the command line and must cost nothing when it fires.
    let run_name = args
        .profile
        .as_deref()
        .map(|f| args.profile_name.clone().unwrap_or_else(|| default_name(f)));
    let daemon_name =
        args.daemon.as_deref().map(|f| args.daemon_name.clone().unwrap_or_else(|| default_name(f)));
    // ⚠ Resolved even when `--recorder` is absent, because the report below names it either way —
    // so `refuse_one_name_under_two_kinds` takes the FLAG's presence rather than the name's.
    let recorder_name = args
        .recorder_name
        .clone()
        .unwrap_or_else(|| crate::cmd::config::mirror_recorder::DEFAULT_PROFILE_NAME.to_string());
    refuse_one_name_under_two_kinds(
        run_name.as_deref(),
        daemon_name.as_deref(),
        args.recorder.as_ref().map(|_| recorder_name.as_str()),
    )?;

    // ── THE PROFILE HALVES, ALL PLANNED BEFORE ANYTHING IS WRITTEN ──────────────────────────
    //
    // Each plan runs its own ROUND-TRIP FENCE and its own CROSS-KIND pre-check, so every refusal
    // this verb can FORESEE is decided here, with nothing written. The write block below is still
    // one transaction PER PROFILE, so a store that goes away between two of them — a full disk, a
    // read-only remount, a concurrent writer holding the lock past the busy timeout — leaves the
    // earlier ones on disk; [`write_phase_error`] is what makes that state SAY so instead of
    // inheriting a store-level "nothing was written" that is true of one transaction and false of
    // the run.
    let run = match (&args.profile, &run_name) {
        (Some(file), Some(name)) => Some(
            crate::cmd::config::mirror_profile::plan(
                crate::cmd::config::mirror_profile::Plane::Run,
                dir,
                file,
                name,
            )
            .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    let daemon = match (&args.daemon, &daemon_name) {
        (Some(file), Some(name)) => Some(
            crate::cmd::config::mirror_profile::plan(
                crate::cmd::config::mirror_profile::Plane::Daemon,
                dir,
                file,
                name,
            )
            .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    // ...and the recorder profile. `crate::cmd::config::mirror_recorder`'s module doc carries why
    // its fence is a round-trip on the BODY rather than `plan_active_row` alone: a recorder profile
    // decides which venue feeds open, so a body that is not what the box runs today is a daemon
    // subscribing to markets nobody asked for.
    let recorder = match &args.recorder {
        Some(file) => Some(
            crate::cmd::config::mirror_recorder::plan(dir, file, &recorder_name)
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };

    let mut profile_counts = String::new();
    for (label, name, m) in [("run", &run_name, &run), ("daemon", &daemon_name, &daemon)] {
        if let (Some(name), Some(m)) = (name, m) {
            profile_counts.push_str(&format!(
                ", {label} `{name}` ({} mount(s), {} setting(s): {})",
                m.stored.mounts.len(),
                m.stored.settings.len(),
                m.summary.join("; ")
            ));
        }
    }
    if let Some(m) = &recorder {
        profile_counts.push_str(&format!(
            ", recorder `{recorder_name}` (1 body, {} subscription(s): {})",
            m.subscriptions.len(),
            m.subscriptions.join("; ")
        ));
    }

    let profile_note = profile_notes(&run, &daemon, &recorder);

    if args.dry_run {
        return Ok(format!(
            "would mirror{profile_counts} into {} — NOTHING WAS WRITTEN.{profile_note}",
            vike_secrets::db_path_in(dir).display(),
        ));
    }

    // ── THE WRITE PHASE, AND THE RUN KNOWS WHICH PROFILES LANDED ────────────────────────────
    //
    // `landed` is not bookkeeping for a report that reads nicely: it is the only thing on this box
    // that can tell an operator the truth after a fault between two of these calls. Every store
    // refusal is written from INSIDE one transaction and says so in the store's own words; the run
    // scope is this function's to state. See [`write_phase_error`].
    let mut landed: Vec<String> = Vec::new();
    for (plane, name, m) in [
        (crate::cmd::config::mirror_profile::Plane::Run, &run_name, &run),
        (crate::cmd::config::mirror_profile::Plane::Daemon, &daemon_name, &daemon),
    ] {
        if let (Some(name), Some(m)) = (name, m) {
            let what = format!("the {} profile `{name}`", plane.kind().sql_word());
            crate::cmd::config::mirror_profile::write(
                plane,
                dir,
                m,
                now_utc(),
                vike_model::AssetClass::SQL_WORDS,
            )
            .map_err(|e| write_phase_error(&landed, &what, &e))?;
            landed.push(what);
        }
    }
    if let Some(m) = &recorder {
        let what = format!("the recorder profile `{recorder_name}`");
        crate::cmd::config::mirror_recorder::write(
            dir,
            m,
            now_utc(),
            vike_model::AssetClass::SQL_WORDS,
        )
        .map_err(|e| write_phase_error(&landed, &what, &e))?;
        landed.push(what);
    }
    Ok(format!(
        "mirrored{profile_counts} into {}.{profile_note}",
        vike_secrets::db_path_in(dir).display()
    ))
}

/// **ONE name under TWO kinds in ONE command — refused before any half is planned.**
///
/// ⚠ **This is the collision no `plan` can see, and it was reachable from the fix's own motivating
/// flag.** Each plan's cross-kind pre-check calls `read_profiles`, which answers for the store as
/// it was when the run STARTED; the halves are then written one at a time. So on a store holding
/// neither name,
///
/// ```text
/// vike-cli config mirror --no-settings --profile run-live.toml --profile-name default \
///                        --recorder rec.toml
/// ```
///
/// planned clean, reported *would mirror … recorder `default`* and exited 0 in `--dry-run`, then
/// wrote the `run` body and had `store_profile` refuse the recorder half — printing
/// `NOTHING WAS WRITTEN` with the run body already on disk. MEASURED at `bab92f0ee`. `--recorder`
/// supplies `default` from [`crate::cmd::config::mirror_recorder::DEFAULT_PROFILE_NAME`] with no
/// name typed at all, so the operator's command line does not look like a collision.
///
/// # Why comparing THREE FLAGS is the whole of it, not a special case
///
/// The general cure would be to plan each half against a PROJECTED store — the rows earlier halves
/// will have written. Here the two collapse, and the reason is structural rather than lucky: the
/// only thing one half of this run can change that another half READS is
/// `vike_secrets::profile_store::Profiles::by_name`, the `profile.name` namespace, and the only
/// names this run adds to it are these three. The other read a plan performs —
/// `Profiles::active(kind)` — cannot move either, because `store_profile` PRESERVES the `active`
/// bit it finds and sets no other row's, and each kind is named by at most one flag (`--profile`
/// and `--daemon` are refused twice over by [`once`], and they are different kinds by
/// construction). So every ordering of these three writes is legal exactly when these three names
/// are pairwise distinct.
///
/// What it leaves open is a FAULT rather than a refusal, and [`write_phase_error`] is where that is
/// handled: two `config mirror` processes racing on one store can still have the second refused by
/// `store_profile` after this one's earlier halves have landed.
///
/// # Errors
///
/// The store's own [`vike_secrets::profile_store::ProfileError::NameWantedByTwoKinds`], constructed
/// rather than re-spelled, so this rung and the two in-store ones share the rule and the repair.
/// ⚠ NOT `NameHeldByAnotherKind`, which the reviewer's proposed cure would have reused: its first
/// sentence is *"the settings store already holds a `{held}` profile called `{name}`"*, and at this
/// rung the store holds nothing of the sort — the refusal would have been a second false statement
/// about a write, inside the fix for a false statement about a write.
fn refuse_one_name_under_two_kinds(
    run: Option<&str>,
    daemon: Option<&str>,
    recorder: Option<&str>,
) -> Result<(), String> {
    use vike_secrets::profile_store::ProfileKind;
    // In WRITE ORDER, which is what makes `first`/`second` name the body that would have been
    // destroyed and the body that would have destroyed it.
    let planned =
        [(ProfileKind::Run, run), (ProfileKind::Daemon, daemon), (ProfileKind::Recorder, recorder)];
    for (i, (first, a)) in planned.iter().enumerate() {
        for (second, b) in planned.iter().skip(i + 1) {
            if let (Some(a), Some(b)) = (a, b)
                && a == b
            {
                return Err(vike_secrets::profile_store::ProfileError::NameWantedByTwoKinds {
                    name: (*a).to_string(),
                    first: *first,
                    second: *second,
                }
                .to_string());
            }
        }
    }
    Ok(())
}

/// **What a failure DURING the write phase says — and why this verb owns the run-scope sentence
/// rather than inheriting one.**
///
/// A store refusal is written from inside ONE transaction and is true about that transaction:
/// `NOTHING WAS WRITTEN` in `vike_secrets::profile_store::ProfileError` or
/// `vike_secrets::DbError` means *this write stored nothing*, which is the only scope a library
/// can see. `config mirror` is FOUR transactions, so once one has committed that sentence is false
/// about the run — and a message that is false about a write is the exact defect this whole verb
/// was audited for.
///
/// So: while nothing has landed the store's words pass through UNCHANGED (they are true, and they
/// are better than anything this function could write). Once something has landed, the run-scope
/// claim is re-scoped to the write it is actually about and the report names what is on disk. The
/// rewrite is deliberately a phrase substitution rather than a rewording — the store's argument,
/// its repair and its evidence are what the operator needs, and only the SCOPE of one sentence is
/// wrong.
fn write_phase_error(landed: &[String], failed: &str, err: &str) -> String {
    if landed.is_empty() {
        return err.to_string();
    }
    format!(
        "{}\n\n⚠ PART OF THIS RUN WAS ALREADY WRITTEN. {failed} is what failed; {} \
         {} already committed, and re-reading the refusal above as if the store were untouched \
         would be wrong. `config mirror` is one transaction PER HALF, not one for the run — the \
         halves that committed are on disk and this failure did not roll them back.\n\nEvery half \
         is a REPLACE by name, so re-running the same command once the failure above is dealt \
         with is the repair: the halves that landed are written again identically.",
        rescope_store_claim(err),
        landed.join(", "),
        if landed.len() == 1 { "has" } else { "have" },
    )
}

/// **The run-scope claim a library message makes about itself, re-scoped to what it can see.**
///
/// `NOTHING WAS WRITTEN` is this tree's term of art for *this command changed nothing*, and a
/// store error says it about its own transaction. [`write_phase_error`] calls this only when that
/// reading is false at run scope. A phrase that is absent is left alone, so this cannot be wrong
/// about a message that never made the claim — it can only fail to catch a spelling nobody uses
/// yet, which is why the three the tree actually writes are each named here.
fn rescope_store_claim(err: &str) -> String {
    err.replace("NOTHING WAS WRITTEN", "NOTHING WAS WRITTEN BY THAT ONE WRITE")
        .replace("Nothing was written", "Nothing was written by that one write")
        .replace("nothing was written", "nothing was written by that one write")
}

/// **The sentence an operator must not have to infer, once per mirrored profile: the BODY was
/// stored and NOTHING was selected.**
///
/// Said on every profile mirror, dry run included. Storing a body is not selecting one, and on
/// these documents the difference is what a restart does — which venue sockets open, which risk
/// ceilings judge an order, whether a live mount starts at all. The report names the second,
/// deliberate act rather than leaving the operator to discover it.
fn profile_notes(
    run: &Option<crate::cmd::config::mirror_profile::Mirrored>,
    daemon: &Option<crate::cmd::config::mirror_profile::Mirrored>,
    recorder: &Option<crate::cmd::config::mirror_recorder::Mirrored>,
) -> String {
    let mut out = String::new();
    for (plane, m) in [
        (crate::cmd::config::mirror_profile::Plane::Run, run),
        (crate::cmd::config::mirror_profile::Plane::Daemon, daemon),
    ] {
        if let Some(m) = m {
            out.push_str(&crate::cmd::config::mirror_profile::active_row_note(plane, m));
        }
    }
    if let Some(m) = recorder {
        out.push_str(&active_row_note(m));
    }
    out
}

/// Seconds since the Unix epoch, for the row's `updated_utc` stamp. A wall clock is the right one
/// here — the column answers "when did an operator last write this", not an interval.
fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// **The sentence an operator must not have to infer: the body was stored and NOTHING was
/// selected.**
///
/// Said on every recorder mirror, dry run included. Storing a body is not selecting one, and on
/// this document the difference is which venue sockets a restart opens — so the report names the
/// second, deliberate act rather than leaving the operator to discover that the daemon still needs
/// `--record-profile` on its `ExecStart=`.
fn active_row_note(m: &crate::cmd::config::mirror_recorder::Mirrored) -> String {
    match &m.active {
        vike_secrets::profile_store::ActivePlan::Write { name } => format!(
            " The recorder profile `{name}` was ALREADY the active row for its kind, so that row \
             is unchanged."
        ),
        vike_secrets::profile_store::ActivePlan::Withhold { reason } => format!(
            " No active recorder row was written — {reason}. That is the expected answer on every \
             box: nothing SELECTS a recorder profile, because --record/--record-profile are \
             explicit arguments with no default. Point the unit's ExecStart= at \
             `--record-profile <name>` when you are ready, which is a deliberate act."
        ),
    }
}

#[path = "tests/mirror.rs"]
#[cfg(test)]
mod config_mirror_tests;
