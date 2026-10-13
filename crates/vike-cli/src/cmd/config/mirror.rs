//! `vike-cli config mirror` — **write one of this box's PROFILE documents (a daemon profile or a
//! recorder profile) into the settings database as rows.**
//!
//! ⚠ **The RUN profile is no longer one of them** (decision 0111, verdict 4): it is rows only and
//! is written from arguments by `vike-cli config bootstrap-run`; no file is imported for it. A
//! `--profile` argument here is REFUSED BY NAME ([`PROFILE_FLAG_REMOVED`]), never read as an
//! unknown word. The sections below that still speak of `--profile` record how the run plane
//! worked while it existed.
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
//! was forbidden by name (`crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs`). Building one was a
//! test failure by design.
//!
//! `--profile` then stored the run profile's whole BODY on the Phase-3 plane
//! (`vike_secrets::profile_store`), `--daemon` does the same for the daemon profile, and
//! **`crates/vike-tradehub/src/tradehub_cli.rs` reads both at boot**. The lowering, the round-trip
//! fence and the argument for the plane are in `crate::cmd::config::mirror_profile`.
//!
//! **Storing is still not selecting.** Every profile flag here writes a BODY and never an active
//! row: `plan_active_row` is consulted, reported, and answers `Withhold { NothingSelectedToday }`
//! on every box, because this process cannot see what is in force (`--config` reaches the daemon
//! through its `ExecStart=`).
//! `vike-cli config activate <kind> <name> --proves <file>` is the deliberate act, and it has its
//! own fence. That is also why every one of these flags is explicit rather than defaulted: having
//! the mirror pick a profile out of ITS OWN environment would be 0057's Question 3 answered
//! sideways, in the one verb that writes.
//!
//! # Why the writer is a CLI verb and not something a daemon does
//!
//! ⚠ **This section argued it from the SANDBOX, and that argument is obsolete for the trading
//! daemon.** It said: *"no shipped `.service` under `deploy/` grants `settings/db`, so the database
//! is READ-ONLY to both daemons inside their own mount namespaces"*, so only an operator shell
//! outside them could write this store. Since the owner's ruling of 2026-09-18
//! `deploy/vike-tradehub.service`'s `ReadWritePaths=` names `<settings>/db`, and since
//! `docs/decisions/0086-settings-live-only-in-the-database.md` point 6 (2026-09-26) that daemon
//! writes `setting` rows itself, over its control channel
//! (`crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting`). It is still true of
//! `deploy/vike-datahub.service`, whose `ReadWritePaths=` names only `settings/state/logs` and its
//! data root.
//!
//! What keeps THIS verb's rows — the profile tables — a CLI act is code, not the kernel: the
//! daemon's settings write plans only the `setting` row shape
//! (`crates/vike-secrets/src/settings/row_write.rs`'s `RowChange`), and every profile writer must
//! take `vike_secrets::profile_store::OperatorWrite`, whose constructing files
//! `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs` pins under `crates/vike-cli/src/`. What
//! guards the `policy` ceilings the daemon CAN write is listed in
//! `crates/vike-secrets/src/settings.rs`'s module doc.
//!
//! # What it refuses
//!
//! * **A project with no database.** `store_profile` refuses rather than creating one, and the
//!   reason is the live gate rather than tidiness: the mere EXISTENCE of
//!   `<project>/settings/db/vike.db` is the whole of `vike_secrets::Backend`'s per-run choice, so
//!   creating it here would hand the box an EMPTY credential store in the same act.
//!   `vike-cli secrets init` is the one thing that may create it.
//!
//! # ⚠ Not a credential write, and deliberately not journalled as one
//!
//! This verb writes no credential — it does not appear in
//! `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`'s `WRITER_CALLERS` for that reason. It also
//! records no `set_setting` journal row: a mirror states no NEW value, it copies a document an
//! operator already filed, and a journal row per mirrored field would bury the real edits the
//! journal exists to carry. `config set` is the verb that journals, and it writes one settings row.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cmd::args;

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
const LONG_ABOUT: &str = "Store one of this box's PROFILE documents (a daemon profile or a recorder
profile) as rows. There are no settings FILES any more (0086) — a settings key is
written one row at a time by `vike-cli config set`, never mirrored here — and
the RUN profile is written by `vike-cli config bootstrap-run` (0111). At least
one profile flag is required.

A PROFILE flag stores that document's whole BODY as rows and selects nothing.
Each is REFUSED unless the rows render back to the file they came from.
`vike-cli config activate <kind> <name> --proves <file>` is what makes a body
the one this box READS — a separate, deliberate act.";

/// The refusal `--profile` / `--profile-name` earn (decision 0111): the run profile is no longer a
/// document this verb imports, so the flag is refused BY NAME, naming its replacement, rather than
/// falling into the unknown-option arm as if it had never shipped.
pub(super) const PROFILE_FLAG_REMOVED: &str = "--profile is REMOVED (decision 0111): the run \
     profile is rows only and no file is imported for it. Write it with `vike-cli config \
     bootstrap-run <name> --risk.<key> <value> ...` — every key of the old file is a \
     flag spelled as its dotted path — which stores the body and makes it the ACTIVE run profile.";

/// Parsed flags. `Debug` so a parser test can `unwrap_err` against it.
#[derive(Debug, Default)]
struct Args {
    dry_run: bool,
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
        self.daemon.is_some() || self.recorder.is_some()
    }
}

/// `config mirror`'s arguments, as `clap` reads them. At least one profile flag is required, and
/// each file flag is given ONCE — both rules keep their own wording, in [`MirrorCmd::into_args`].
#[derive(clap::Args, Debug)]
#[command(
    about = "Store a daemon/recorder PROFILE document as rows",
    long_about = LONG_ABOUT
)]
pub(crate) struct MirrorCmd {
    #[arg(long, help = "report what would be written, and write nothing")]
    dry_run: bool,
    // Repeatable ONLY so that a second occurrence is refused in the words this verb always used,
    // naming the `-name` flag that is the real answer (see [`once`]).
    #[arg(
        long,
        value_name = "FILE",
        action = clap::ArgAction::Append,
        help = "the DAEMON profile (tradehub.toml): the mount rows and [daemon]"
    )]
    daemon: Vec<String>,
    #[arg(
        long,
        value_name = "NAME",
        help = "the NAME the daemon body is stored under (default: the file stem)"
    )]
    daemon_name: Option<String>,
    #[arg(
        long,
        value_name = "FILE",
        action = clap::ArgAction::Append,
        help = "the RECORDER profile as `recorder` + `subscription` rows"
    )]
    recorder: Vec<String>,
    #[arg(
        long,
        value_name = "NAME",
        help = "the profile NAME the rows are stored under (default `default`)"
    )]
    recorder_name: Option<String>,
    // ⚠ Removed (decision 0111), and answered BY NAME with the writer that replaced them.
    #[arg(long, hide = true, num_args = 0..=1)]
    profile: Option<Option<String>>,
    #[arg(long, hide = true, num_args = 0..=1)]
    profile_name: Option<Option<String>>,
}

/// Refuse a second occurrence of a one-at-a-time file flag, naming the `-name` flag that is the
/// real answer. Spelled once so the two read identically.
fn once(values: Vec<String>, flag: &str) -> Result<Option<PathBuf>, String> {
    if values.len() > 1 {
        return Err(format!(
            "{flag} given twice. Each profile is stored under a NAME, so mirror them one at a time \
             with {flag}-name."
        ));
    }
    Ok(values.into_iter().next().map(PathBuf::from))
}

impl MirrorCmd {
    fn into_args(self) -> Result<Args, String> {
        if self.profile.is_some() || self.profile_name.is_some() {
            return Err(PROFILE_FLAG_REMOVED.to_string());
        }
        let out = Args {
            dry_run: self.dry_run,
            daemon: once(self.daemon, "--daemon")?,
            daemon_name: self.daemon_name,
            recorder: once(self.recorder, "--recorder")?,
            recorder_name: self.recorder_name,
        };
        for (name, name_flag, file, file_flag) in [
            (&out.daemon_name, "--daemon-name", &out.daemon, "--daemon"),
            (&out.recorder_name, "--recorder-name", &out.recorder, "--recorder"),
        ] {
            if name.is_some() && file.is_none() {
                return Err(format!(
                    "{name_flag} names the profile {file_flag} stores, so it needs {file_flag} \
                     <file>. Alone it configures nothing."
                ));
            }
        }
        if !out.any_profile() {
            return Err(
                "nothing named to mirror — there are no settings FILES any more (0086), so at \
                 least one of --daemon / --recorder is required (the run profile is written by \
                 `vike-cli config bootstrap-run`, 0111)."
                    .to_string(),
            );
        }
        Ok(out)
    }
}

/// Read `config mirror`'s argv tail (the arguments after `config mirror`) through the plane's own
/// parser, so a test meets the grammar an operator does.
#[cfg(test)]
fn parse_args(argv: impl Iterator<Item = String>) -> Result<Args, args::Stop> {
    let argv = std::iter::once("mirror".to_string()).chain(argv);
    match crate::cmd::config::parse(argv)? {
        crate::cmd::config::Sub::Mirror(cmd) => {
            cmd.into_args().map_err(|msg| args::Stop::message("vike-cli config mirror", msg))
        }
        other => unreachable!("`mirror` parsed as {other:?}"),
    }
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
pub(crate) fn run(cmd: MirrorCmd, settings_dir: Option<&Path>) -> ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config mirror", msg),
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
    // Resolved FIRST — above every plan, above every write — because the refusal below is a pure
    // function of the command line and must cost nothing when it fires.
    let daemon_name =
        args.daemon.as_deref().map(|f| args.daemon_name.clone().unwrap_or_else(|| default_name(f)));
    // ⚠ Resolved even when `--recorder` is absent, because the report below names it either way —
    // so `refuse_one_name_under_two_kinds` takes the FLAG's presence rather than the name's.
    let recorder_name = args
        .recorder_name
        .clone()
        .unwrap_or_else(|| crate::cmd::config::mirror_recorder::DEFAULT_PROFILE_NAME.to_string());
    refuse_one_name_under_two_kinds(
        daemon_name.as_deref(),
        args.recorder.as_ref().map(|_| recorder_name.as_str()),
    )?;

    // ── THE PROFILE HALVES, ALL PLANNED BEFORE ANYTHING IS WRITTEN ──────────────────────────
    //
    // Each plan runs its own ROUND-TRIP FENCE and its own CROSS-KIND pre-check, so every refusal
    // this verb can FORESEE is decided here, with nothing written. The write block below is still
    // one transaction PER PROFILE, so a store that goes away between the two — a full disk, a
    // read-only remount, a concurrent writer holding the lock past the busy timeout — leaves the
    // earlier one on disk; [`write_phase_error`] is what makes that state SAY so instead of
    // inheriting a store-level "nothing was written" that is true of one transaction and false of
    // the run.
    let daemon = match (&args.daemon, &daemon_name) {
        (Some(file), Some(name)) => Some(
            crate::cmd::config::mirror_profile::plan(dir, file, name).map_err(|e| e.to_string())?,
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
    if let (Some(name), Some(m)) = (&daemon_name, &daemon) {
        profile_counts.push_str(&format!(
            ", daemon `{name}` ({} mount(s), {} setting(s): {})",
            m.stored.mounts.len(),
            m.stored.settings.len(),
            m.summary.join("; ")
        ));
    }
    if let Some(m) = &recorder {
        profile_counts.push_str(&format!(
            ", recorder `{recorder_name}` (1 body, {} subscription(s): {})",
            m.subscriptions.len(),
            m.subscriptions.join("; ")
        ));
    }

    let profile_note = profile_notes(&daemon, &recorder);

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
    if let (Some(name), Some(m)) = (&daemon_name, &daemon) {
        let what = format!("the daemon profile `{name}`");
        crate::cmd::config::mirror_profile::write(
            dir,
            m,
            vike_model::time::clock::now_secs(),
            vike_model::AssetClass::SQL_WORDS,
        )
        .map_err(|e| write_phase_error(&landed, &what, &e))?;
        landed.push(what);
    }
    if let Some(m) = &recorder {
        let what = format!("the recorder profile `{recorder_name}`");
        crate::cmd::config::mirror_recorder::write(
            dir,
            m,
            vike_model::time::clock::now_secs(),
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
/// name typed at all, so the operator's command line does not look like a collision. The `run` half
/// is gone (decision 0111); `--daemon-name default --recorder rec.toml` is the same collision
/// between the two halves that are left.
///
/// # Why comparing the FLAGS is the whole of it, not a special case
///
/// The general cure would be to plan each half against a PROJECTED store — the rows earlier halves
/// will have written. Here the two collapse, and the reason is structural rather than lucky: the
/// only thing one half of this run can change that another half READS is
/// `vike_secrets::profile_store::Profiles::by_name`, the `profile.name` namespace, and the only
/// names this run adds to it are these. The other read a plan performs — `Profiles::active(kind)`
/// — cannot move either, because `store_profile` PRESERVES the `active` bit it finds and sets no
/// other row's, and each kind is named by at most one flag (a second `--daemon` or `--recorder` is
/// refused by [`once`]). So every ordering of these writes is legal exactly when the names are
/// pairwise distinct.
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
    daemon: Option<&str>,
    recorder: Option<&str>,
) -> Result<(), String> {
    use vike_secrets::profile_store::ProfileKind;
    // In WRITE ORDER, which is what makes `first`/`second` name the body that would have been
    // destroyed and the body that would have destroyed it.
    let planned = [(ProfileKind::Daemon, daemon), (ProfileKind::Recorder, recorder)];
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
/// these documents the difference is what a restart does — which venue sockets open, which account
/// trades. The report names the second, deliberate act rather than leaving the operator to discover
/// it.
fn profile_notes(
    daemon: &Option<crate::cmd::config::mirror_profile::Mirrored>,
    recorder: &Option<crate::cmd::config::mirror_recorder::Mirrored>,
) -> String {
    let mut out = String::new();
    if let Some(m) = daemon {
        out.push_str(&crate::cmd::config::mirror_profile::active_row_note(m));
    }
    if let Some(m) = recorder {
        out.push_str(&active_row_note(m));
    }
    out
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

/// An OPTIONAL string key of a profile document being lowered to rows: absent is `None`, a TOML
/// string is its text, anything else refuses naming `key`. The one reader both mirror halves
/// (`crate::cmd::config::mirror_profile` and `crate::cmd::config::mirror_recorder`) lower their
/// string keys through.
pub(super) fn opt_str(v: Option<&toml::Value>, key: &str) -> Result<Option<String>, String> {
    match v {
        None => Ok(None),
        Some(v) => Ok(Some(v.as_str().ok_or(format!("`{key}` must be a string"))?.to_string())),
    }
}

/// An OPTIONAL integer key, [`opt_str`]'s twin: absent is `None`, a TOML integer is its value,
/// anything else (a float included) refuses naming `key`.
pub(super) fn opt_int(v: Option<&toml::Value>, key: &str) -> Result<Option<i64>, String> {
    match v {
        None => Ok(None),
        Some(v) => Ok(Some(v.as_integer().ok_or(format!("`{key}` must be an integer"))?)),
    }
}

#[path = "tests/mirror.rs"]
#[cfg(test)]
mod config_mirror_tests;
