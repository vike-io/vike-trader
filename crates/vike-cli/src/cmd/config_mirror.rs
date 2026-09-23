//! `vike-cli config mirror` — **write this box's four settings files, and any of its three PROFILE
//! documents, into the settings database.**
//!
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 1 is MIRRORED:
//! the store is written and **the files still win**. This verb is the WRITE half, and it changes no
//! effective value on the box it runs on — `crates/vike-config/tests/mirror.rs` is where that is
//! proven rather than asserted.
//!
//! # ⚠ This verb IS the migration when the row FORMAT changes
//!
//! The `setting` table's `value` column held a TOML rendering until 2026-09-18 and holds a JSON
//! scalar now (`vike_config::mirror`). There is no second reader and no upgrade step: the rows are
//! a REGENERABLE copy of files that still win, so **running this verb is the whole migration** —
//! `write_settings` is a whole-table REPLACE in one transaction, and this command runs in an
//! operator shell outside both daemons' mount namespaces.
//!
//! The order matters on a deployed box, because the daemon reads the store on EVERY boot: **re-run
//! `vike-cli config mirror` before the new binary's unit restarts.** A row the new reader cannot
//! parse is a hard startup refusal naming the key — deliberately, since the alternative is a
//! mangled ceiling nobody can see — and `crates/vike-config/tests/mirror.rs`'s
//! `no_pre_json_rendering_reads_back_as_a_different_value` is the proof that the only two outcomes
//! for an old row are *refused* and *identical*. MEASURED on both live boxes the day the format
//! changed: every row they carry (`100`, `250`, `"127.0.0.1:7878"`, `"warn"`, `true`, and the
//! arming rows, which were never a rendering) is in the identical column, so neither box needs the
//! re-run to keep booting — it is the ordering rule that makes that true of the NEXT box too.
//!
//! # `--profile` / `--daemon` / `--recorder` — the PROFILE planes, and THE ROWS BIND
//!
//! ⚠ **This section said the opposite until this landing, and the sentence it printed in `--help`
//! was the whole reason the profile plane never migrated:** *"Nothing reads the rows: the profile
//! FILE still judges every order."* That was true and it was not an accident — the flag wrote to
//! `profile_risk`, a DISCLOSURE mirror of one `[risk]` table with no `active` column, whose reader
//! was forbidden by name (`crates/vike-ops/tests/profile_risk_readers_gate.rs`). Building one was a
//! test failure by design.
//!
//! `--profile` now stores the run profile's whole BODY on the Phase-3 plane
//! (`vike_secrets::profile_store`), `--daemon` does the same for the daemon profile, and
//! **`crates/vike-tradehub/src/tradehub_cli.rs` reads both at boot** — an ACTIVE `run` row is what
//! the daemon builds its pre-trade ceilings from, ahead of `VIKE_RUN_PROFILE`. The lowering, the
//! round-trip fence and the argument for the plane are in `crate::cmd::config_mirror_profile`.
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
//! MEASURED in the tree and stated by 0057: **no shipped `.service` under `deploy/` grants
//! `settings/db`**, so the database is READ-ONLY to both daemons inside their own mount namespaces.
//! Reads need no grant and the composition roots already perform one; a WRITE needs the grant 0054
//! decided and has not paid, plus the hard ceiling above it that is not built. So the only process
//! that can write this store today is an operator shell OUTSIDE the daemon's namespace running as
//! the same account — `vike-cli`, or the deploy pipeline. **Nothing in Phase 1 requires a unit
//! change, and widening one is not this verb's business.**
//!
//! # What it refuses
//!
//! * **A project with no database.** `vike_secrets::write_settings` returns
//!   `DbErrorKind::NoSettingsDatabase` rather than creating one, and the reason is the live gate
//!   rather than tidiness: the mere EXISTENCE of `<project>/settings/db/vike.db` is the whole of
//!   `vike_secrets::Backend`'s per-run choice, so creating it here would make every credential in
//!   `secrets.env` unread in the same act. `vike-cli secrets migrate` is the one thing that may
//!   create it.
//! * **A settings tree a boot would refuse.** The mirror loads the four files first, with the same
//!   loader and the same errors, so a store is never written from a file that would fail to parse
//!   at the next start.
//!
//! # ⚠ Not a credential write, and deliberately not journalled as one
//!
//! This verb writes no credential and touches no credential file — it does not appear in
//! `crates/vike-ops/tests/credential_writer_gate.rs`'s `WRITER_CALLERS` for that reason. It also
//! records no `set_setting` journal row: a mirror states no NEW value, it copies values an operator
//! already filed, and a journal row per mirrored key would bury the real edits the journal exists to
//! carry. `config set` is the verb that journals, and it still writes the FILE.

use std::collections::HashMap;
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
pub const USAGE: &str = "usage: vike-cli config mirror [--dry-run] [--no-settings]\n             \
                         [--profile <file>] [--profile-name <name>]\n             [--daemon \
                         <file>] [--daemon-name <name>]\n             [--recorder <file>] \
                         [--recorder-name <name>]\n\n  Copy \
                         <project>/settings/{policy,config,preferences,flags}.toml into the \
                         settings\n  database beside them.\n  ⚠ What that CHANGES depends on \
                         whether this box was adopted:\n      UNADOPTED — the files keep winning; \
                         no value this box resolves changes.\n      ADOPTED   — the ROWS are what \
                         it resolves, so mirroring writes its EFFECTIVE\n                  values. \
                         Run --dry-run first: it says which of the two you are on.\n\n  A PROFILE \
                         flag stores that document's whole BODY as rows and selects nothing.\n  \
                         Each is REFUSED unless the rows render back to the file they came from.\n \
                         `vike-cli config activate <kind> <name> --proves <file>` is what makes a \
                         body\n  the one this box READS — a separate, deliberate act.\n\n  \
                         --profile <file>   the RUN profile (run-live.toml): mode, [risk], \
                         [sinks], [guards].\n                     ⚠ THE ROWS BIND once activated — \
                         an active `run` row is what the\n                     daemon builds its \
                         pre-trade ceilings from, ahead of VIKE_RUN_PROFILE.\n  --profile-name \
                         <name>   the NAME the run body is stored under (default: the file \
                         stem)\n  --daemon <file>    the DAEMON profile (tradehub.toml): the mount \
                         rows and [daemon].\n  --daemon-name <name>    the NAME the daemon body is \
                         stored under (default: the file stem)\n  --recorder <file>  the RECORDER \
                         profile as `recorder` + `subscription` rows\n  --recorder-name <name>  \
                         the profile NAME the rows are stored under (default \
                         `default`)\n  --no-settings      skip the four-file half entirely and \
                         mirror only the profiles.\n                     ⚠ THE WAY IN ON AN \
                         ADOPTED BOX: the four-file half is refused there\n                     \
                         (its rows are the only copy), and a profile write touches neither\n       \
                         the `setting` nor the `venue_arming` table, so that refusal does not \
                         apply.\n  --dry-run          report what would be written, and write \
                         nothing";

/// Parsed flags. `Debug` so a parser test can `unwrap_err` against it.
#[derive(Debug, Default)]
struct Args {
    dry_run: bool,
    /// Skip the four-file half. ⚠ **The way in on an ADOPTED box** — see [`execute`]'s guard.
    no_settings: bool,
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
            "--no-settings" => {
                args::no_value(&flag, inline)?;
                out.no_settings = true;
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
    if out.no_settings && !out.any_profile() {
        return Err(
            "--no-settings skips the only half this run would have done. Name a profile as well \
             (--profile / --daemon / --recorder), or drop --no-settings."
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
        .unwrap_or_else(|| crate::cmd::config_mirror_recorder::DEFAULT_PROFILE_NAME.to_string());
    refuse_one_name_under_two_kinds(
        run_name.as_deref(),
        daemon_name.as_deref(),
        args.recorder.as_ref().map(|_| recorder_name.as_str()),
    )?;

    // ── THE FOUR-FILE HALF ──────────────────────────────────────────────────────────────────
    //
    // ⚠ **IT IS NOW CONDITIONAL, AND THAT IS THE FIX THAT MAKES THIS VERB USABLE ON THE BOX THAT
    // NEEDS IT.** Everything from here to the guard below used to run unconditionally, ABOVE every
    // profile flag — so on an ADOPTED box the guard refused the whole run with a message about
    // `setting` rows even when the operator had asked only for a profile body, and a profile write
    // touches neither `setting` nor `venue_arming`. Measured on the CI box 2026-09-21: the box that
    // carries the two un-migrated profiles is exactly the box where this verb could not be used.
    //
    // The guard itself is NOT weakened and must not be: it is correct for the settings plane for
    // reasons that do not hold for a profile body (see its own comment). It is SCOPED.
    let settings_half = !args.no_settings;
    let counts;
    let mut rows = vike_secrets::StoredSettings::default();
    if settings_half {
        // The files, validated exactly as a boot validates them. A refusal here names the file and
        // the key, and nothing has been written.
        rows = vike_config::rows_from_files(dir).map_err(|e| e.to_string())?;
        let mut by_section: HashMap<&str, usize> = HashMap::new();
        for row in &rows.settings {
            *by_section.entry(row.section.as_str()).or_default() += 1;
        }
        let mut shown: Vec<String> = vike_config::SECTION_FILES
            .iter()
            .map(|(section, _)| {
                format!("{section} {}", by_section.get(section).copied().unwrap_or(0))
            })
            .collect();
        shown.push(format!("venue-arming {}", rows.arming.len()));
        counts = shown.join(", ");
    } else {
        counts = "no settings (--no-settings)".to_string();
    }

    // ── THE PROFILE HALVES, ALL PLANNED BEFORE ANYTHING IS WRITTEN ──────────────────────────
    //
    // Each plan runs its own ROUND-TRIP FENCE and its own CROSS-KIND pre-check, so every refusal
    // this verb can FORESEE is decided here, with nothing written — including the one the settings
    // half would otherwise have landed ahead of.
    //
    // ⚠ **That is a claim about REFUSALS, not about FAULTS, and this comment used to make the wider
    // one.** It read *"refuses the whole run rather than leaving the settings tables mirrored and
    // the profile half not"*, which reads as atomicity and is not: the write block below is FOUR
    // transactions (the settings tables, then one per profile body), so a store that goes away
    // between them — a full disk, a read-only remount, a concurrent writer holding the lock past
    // the busy timeout — leaves the earlier halves on disk. [`write_phase_error`] is what makes
    // that state SAY so instead of inheriting a store-level "nothing was written" that is true of
    // one transaction and false of the run.
    let run = match (&args.profile, &run_name) {
        (Some(file), Some(name)) => Some(
            crate::cmd::config_mirror_profile::plan(
                crate::cmd::config_mirror_profile::Plane::Run,
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
            crate::cmd::config_mirror_profile::plan(
                crate::cmd::config_mirror_profile::Plane::Daemon,
                dir,
                file,
                name,
            )
            .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    // ...and the recorder profile. `crate::cmd::config_mirror_recorder`'s module doc carries why
    // its fence is a round-trip on the BODY rather than `plan_active_row` alone: a recorder profile
    // decides which venue feeds open, so a body that is not what the box runs today is a daemon
    // subscribing to markets nobody asked for.
    let recorder = match &args.recorder {
        Some(file) => Some(
            crate::cmd::config_mirror_recorder::plan(dir, file, &recorder_name)
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

    // ⚠ **THE ADOPTED-BOX GUARD — this command was adoption-BLIND and that made it a way to take
    // the ceiling off a live box with a success message on it.** It re-derives every row from the
    // four files and `write_settings_in` replaces the tables wholesale. On an UNADOPTED box that is
    // harmless by construction: the files win, so the rows are a copy and a copy of the truth
    // cannot be wrong. On an ADOPTED box the rows ARE the truth and the files are inert drafts —
    // `drift.rs` tells the operator so in as many words — so a `policy.toml` that has been deleted
    // or trimmed since adoption re-derives to FEWER rows, and the values it no longer names fall
    // back to compiled-in defaults. Measured at `650907a37`: `max_notional_per_order` went from
    // `250.0` to unset — no size cap on the edge every remote control command funnels through —
    // and the command printed *"The files still win — nothing this box resolves has changed"* and
    // exited 0, which on an adopted box is false in both halves.
    //
    // It is also the disarm for the erase detector: `write_settings` updates the seal's counts in
    // the SAME transaction, so the shrunk table matches its own new seal and the next boot's
    // integrity check sees nothing.
    //
    // REFUSE rather than warn, and rather than confirm: this is the one shape where the operator's
    // intent is genuinely ambiguous (they may be mirroring a deliberately-trimmed file), and
    // `--undo` is the verb that means *I want the files to answer again*. Naming it is what makes
    // the refusal actionable; it runs in this state, like every repair verb.
    //
    // ⚠ **It is evaluated ONLY for the settings half, and the reason it does not reach a profile
    // body is measurable rather than a judgement call.** Its evidence is the four settings files'
    // ROW COUNTS against the seal, which say nothing about a run or daemon profile; there is no
    // seal on the profile plane and so no erase detector to disarm; and `store_profile` replaces
    // ONE body keyed by name inside one transaction and PRESERVES the `active` bit it finds, so it
    // can neither erase a plane nor silently deselect one.
    let seal = if settings_half {
        vike_secrets::read_settings_in(dir).ok().and_then(|s| {
            s.adoption().map(|a| (a.setting_rows, a.arming_rows, a.adopted_at.clone()))
        })
    } else {
        None
    };
    let adopted_box = seal.is_some();
    if let Some((sealed_settings, sealed_arming, adopted_at)) = &seal {
        let (now_settings, now_arming) = (rows.settings.len(), rows.arming.len());
        let (sealed_settings, sealed_arming) = (*sealed_settings, *sealed_arming);
        if now_settings < sealed_settings || now_arming < sealed_arming {
            return Err(format!(
                "REFUSED: this box was ADOPTED on {adopted_at}, so its settings ROWS are what it \
                 resolves and the four files are inert drafts. Re-deriving from the files now \
                 would write {now_settings} setting row(s) and {now_arming} arming row(s) where \
                 the seal records {sealed_settings} and {sealed_arming} — so {} would stop being \
                 named by any row and fall back to the compiled-in defaults, which for \
                 `policy.max_notional_per_order` means NO CEILING on this box.\n\nThis is a \
                 one-way loss: the rows are the only copy. If the files are what you want this box \
                 to resolve from, step back to them with `vike-cli config adopt --undo` — that is \
                 the verb that means it, and it keeps the rows. `vike-cli config compare` shows \
                 what the two sources disagree about first.\n\nIf you came here to mirror a \
                 PROFILE, re-run with --no-settings: a profile body is written to the `profile` / \
                 `mount` / `profile_setting` tables, it carries no seal, it replaces one body by \
                 NAME and it preserves whatever the `active` column already held — so none of the \
                 above applies to it and this refusal does not reach it.",
                if now_settings < sealed_settings { "settings keys" } else { "venue armings" }
            ));
        }
    }

    let profile_note = profile_notes(&run, &daemon, &recorder);

    if args.dry_run {
        return Ok(format!(
            "would mirror {counts}{profile_counts} into {} — NOTHING WAS WRITTEN.{}{profile_note}",
            vike_secrets::db_path_in(dir).display(),
            if !settings_half {
                " The four settings files were not read (--no-settings), so the `setting` and \
                 `venue_arming` tables are untouched by this run."
            } else if adopted_box {
                " ⚠ This box is ADOPTED, so these rows are what it RESOLVES: mirroring would \
                 change its effective values."
            } else {
                " The files keep winning either way; this changes no value this box resolves."
            },
        ));
    }

    // ── THE WRITE PHASE — FOUR TRANSACTIONS, AND THE RUN KNOWS WHICH ONES LANDED ────────────
    //
    // `landed` is not bookkeeping for a report that reads nicely: it is the only thing on this box
    // that can tell an operator the truth after a fault between two of these calls. Every store
    // refusal is written from INSIDE one transaction and says so in the store's own words; the run
    // scope is this function's to state. See [`write_phase_error`].
    let mut landed: Vec<String> = Vec::new();
    let mut created = "";
    if settings_half {
        let written = vike_secrets::write_settings_in(dir, &rows)
            .map_err(|e| write_phase_error(&landed, "the four settings files", &e.to_string()))?;
        if written.tables_created {
            created = " (this store was migrated before the settings tables existed; they were \
                       created)";
        }
        landed.push("the `setting` and `venue_arming` tables".to_string());
    }
    for (plane, name, m) in [
        (crate::cmd::config_mirror_profile::Plane::Run, &run_name, &run),
        (crate::cmd::config_mirror_profile::Plane::Daemon, &daemon_name, &daemon),
    ] {
        if let (Some(name), Some(m)) = (name, m) {
            let what = format!("the {} profile `{name}`", plane.kind().sql_word());
            crate::cmd::config_mirror_profile::write(
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
        crate::cmd::config_mirror_recorder::write(
            dir,
            m,
            now_utc(),
            vike_model::AssetClass::SQL_WORDS,
        )
        .map_err(|e| write_phase_error(&landed, &what, &e))?;
        landed.push(what);
    }
    // ⚠ **BRANCHED ON `adopted_box`, like the dry-run above — it was not, and that is a defect this
    // file already documents about ITSELF.** The ADOPTED-BOX GUARD's comment quotes *"The files
    // still win — nothing this box resolves has changed"* as the sentence the `650907a37` incident
    // printed while taking the ceiling off a live box. The guard that stops the LOSS landed; the
    // sentence that misreported it did not, so this command went on asserting the false half on
    // every adopted box. Measured again on the CI box 2026-09-21: the dry-run warned that effective
    // values would change and the apply, on the same box in the same minute, said none had.
    //
    // ⚠ The adopted wording deliberately does NOT claim the guard makes this safe. That guard
    // compares ROW COUNTS against the seal, so a file whose VALUE changed at an unchanged count
    // passes it and is written with nothing said. Naming `config compare` is what makes the
    // sentence actionable instead of merely hedged.
    let resolve_note = if !settings_half {
        " The four settings files were not read (--no-settings), so the `setting` and \
         `venue_arming` tables are exactly as they were."
    } else if adopted_box {
        " ⚠ This box is ADOPTED, so these rows ARE what it resolves — what was just written is now \
         its EFFECTIVE configuration. The refusal above compares row COUNTS only, so a value that \
         changed at the same count was written without comment; `vike-cli config compare` shows \
         what the two sources disagree about."
    } else {
        " The files still win — nothing this box resolves has changed."
    };
    Ok(format!(
        "mirrored {counts}{profile_counts} into {}{created}.{resolve_note}{profile_note}",
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
/// supplies `default` from [`crate::cmd::config_mirror_recorder::DEFAULT_PROFILE_NAME`] with no
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
    run: &Option<crate::cmd::config_mirror_profile::Mirrored>,
    daemon: &Option<crate::cmd::config_mirror_profile::Mirrored>,
    recorder: &Option<crate::cmd::config_mirror_recorder::Mirrored>,
) -> String {
    let mut out = String::new();
    for (plane, m) in [
        (crate::cmd::config_mirror_profile::Plane::Run, run),
        (crate::cmd::config_mirror_profile::Plane::Daemon, daemon),
    ] {
        if let Some(m) = m {
            out.push_str(&crate::cmd::config_mirror_profile::active_row_note(plane, m));
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
fn active_row_note(m: &crate::cmd::config_mirror_recorder::Mirrored) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn the_flag_parser_takes_dry_run_and_refuses_anything_else() {
        assert!(!parse_of(&[]).unwrap().dry_run);
        assert!(parse_of(&["--dry-run"]).unwrap().dry_run);
        let err = parse_of(&["--force"]).unwrap_err();
        assert!(err.contains("--force"), "{err}");
        // `--help` travels back through the `Err` channel as the shared sentinel, which
        // `args::exit_for_parse_error` turns into a SUCCESS printing the usage to stdout.
        assert!(parse_of(&["--help"]).is_err());
        assert!(parse_of(&["--dry-run=1"]).is_err(), "a bare boolean takes no inline value");
    }

    /// Each profile flag takes a value, may NOT repeat, and its `-name` sibling needs it.
    ///
    /// ⚠ `--profile` used to repeat, and it does not any more: what it writes changed from a
    /// `[risk]` disclosure row keyed by file name to a BODY an operator then activates BY NAME, and
    /// one document at a time is the honest shape of that.
    #[test]
    fn each_profile_flag_takes_one_value_and_its_name_flag_needs_it() {
        for (flag, name_flag) in [
            ("--profile", "--profile-name"),
            ("--daemon", "--daemon-name"),
            ("--recorder", "--recorder-name"),
        ] {
            assert!(parse_of(&[flag, "a.toml"]).is_ok(), "{flag}");
            let err = parse_of(&[flag, "a.toml", flag, "b.toml"]).unwrap_err();
            assert!(err.contains("given twice"), "{flag}: {err}");
            assert!(err.contains(name_flag), "the refusal names the real answer: {err}");
            let err = parse_of(&[name_flag, "x"]).unwrap_err();
            assert!(err.contains(flag), "{name_flag} alone configures nothing: {err}");
            // The shared rule: a valued flag may not eat a following flag.
            assert!(parse_of(&[flag, "--dry-run"]).is_err(), "{flag}");
            assert!(parse_of(&[flag]).is_err(), "{flag}");
        }
    }

    /// **The sentence the help used to print is GONE, and the one that replaced it is there.**
    ///
    /// ⚠ The old page said, of `--profile`: *"Nothing reads the rows: the profile FILE still judges
    /// every order."* It was TRUE — that flag wrote a disclosure mirror with no `active` column
    /// whose reader was forbidden by name — and it is the sentence an operator would have read
    /// before deciding this migration was pointless. It must not survive the change that makes it
    /// false, and a `const` cannot be checked by reading it, so it is checked here.
    #[test]
    fn the_help_page_names_every_flag_and_no_longer_says_the_rows_are_inert() {
        for flag in [
            "--dry-run",
            "--no-settings",
            "--profile",
            "--profile-name",
            "--daemon",
            "--daemon-name",
            "--recorder",
            "--recorder-name",
        ] {
            assert!(USAGE.contains(flag), "the help page does not name `{flag}`:\n{USAGE}");
        }
        assert!(
            !USAGE.contains("Nothing reads the rows"),
            "the retired sentence survived the change that made it false:\n{USAGE}"
        );
        assert!(
            USAGE.contains("THE ROWS BIND"),
            "the help must say what the rows now DO, not merely stop saying what they did \
             not:\n{USAGE}"
        );
        assert!(
            USAGE.contains("config activate"),
            "…and name the separate act that makes a body bind:\n{USAGE}"
        );
    }

    /// `--no-settings` alone would skip the only half the run would have done, so it is refused.
    #[test]
    fn no_settings_alone_configures_nothing_and_is_refused() {
        let err = parse_of(&["--no-settings"]).unwrap_err();
        assert!(err.contains("--no-settings"), "{err}");
        assert!(parse_of(&["--no-settings", "--profile", "p.toml"]).unwrap().no_settings);
    }

    /// **The NAME defaults to the file STEM, not the file name** — a name is what an operator types
    /// into `config activate`, and a `.toml` inside one reads as a path.
    #[test]
    fn the_default_profile_name_is_the_file_stem() {
        assert_eq!(default_name(Path::new("/srv/x/settings/run-live.toml")), "run-live");
        assert_eq!(default_name(Path::new("tradehub.toml")), "tradehub");
        // A path with no stem renders as whatever was typed rather than as an empty name.
        assert_eq!(default_name(Path::new("..")), "..");
    }

    #[test]
    fn a_run_with_no_settings_directory_refuses_rather_than_guessing_one() {
        let err = execute(&Args::default(), None).unwrap_err();
        assert!(err.contains("VIKE_SETTINGS_DIR"), "{err}");
    }

    /// ⚠ The refusal that matters: a project with NO database is not served by creating one. The
    /// existence of that file is what makes the database — rather than `secrets.env` — answer for
    /// every credential on the box.
    #[test]
    fn a_project_with_no_database_is_refused_and_none_is_created() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("policy.toml"), "max_leverage = 2.0\n").unwrap();

        let err = execute(&Args::default(), Some(tmp.path())).unwrap_err();
        assert!(err.contains("secrets migrate"), "the refusal must name the repair: {err}");
        assert!(
            !vike_secrets::db_path_in(tmp.path()).exists(),
            "no database may be left behind by a refused mirror"
        );
    }

    /// A dry run reports the shape and writes nothing — including against a project with no
    /// database, where it is the safe way to see what a mirror WOULD carry.
    #[test]
    fn a_dry_run_reports_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("policy.toml"), "max_leverage = 2.0\n").unwrap();
        std::fs::write(tmp.path().join("flags.toml"), "reconcile = true\n").unwrap();

        let out = execute(&Args { dry_run: true, ..Args::default() }, Some(tmp.path())).unwrap();
        assert!(out.contains("policy 1"), "{out}");
        assert!(out.contains("flags 1"), "{out}");
        assert!(out.contains("NOTHING WAS WRITTEN"), "{out}");
        assert!(!vike_secrets::db_path_in(tmp.path()).exists());
    }

    /// A settings tree a BOOT would refuse is refused here, naming the file and the key — never
    /// half-written into a store that would then refuse every later read.
    #[test]
    fn an_invalid_settings_tree_is_refused_naming_the_file_and_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("policy.toml"), "market_slippage = 0.9\n").unwrap();
        let err =
            execute(&Args { dry_run: true, ..Args::default() }, Some(tmp.path())).unwrap_err();
        assert!(err.contains("market_slippage"), "{err}");
        assert!(err.contains("policy.toml"), "{err}");
    }

    /// **`--no-settings` does not read the four files at all**, which is what makes the verb usable
    /// on an adopted box. Proven by planting a `policy.toml` a BOOT would refuse: with the settings
    /// half on it is a refusal, with `--no-settings` it is never opened.
    #[test]
    fn no_settings_does_not_read_the_four_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("policy.toml"), "market_slippage = 0.9\n").unwrap();
        let profile = tmp.path().join("run-live.toml");
        std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 100.0\n")
            .unwrap();

        let refused = execute(
            &Args { dry_run: true, profile: Some(profile.clone()), ..Args::default() },
            Some(tmp.path()),
        )
        .unwrap_err();
        assert!(
            refused.contains("market_slippage"),
            "the settings half opens the files: {refused}"
        );

        let out = execute(
            &Args { dry_run: true, no_settings: true, profile: Some(profile), ..Args::default() },
            Some(tmp.path()),
        )
        .unwrap();
        assert!(out.contains("no settings (--no-settings)"), "{out}");
        assert!(out.contains("run `run-live`"), "the body is still planned and named: {out}");
        assert!(out.contains("NOTHING WAS WRITTEN"), "{out}");
    }

    /// A dry run names the profile's rows and still writes nothing.
    #[test]
    fn a_dry_run_reports_the_profile_rows_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("run-live.toml");
        std::fs::write(
            &profile,
            "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 250.0\n\
             max_total_exposure = 1000.0\n",
        )
        .unwrap();

        let out = execute(
            &Args { dry_run: true, no_settings: true, profile: Some(profile), ..Args::default() },
            Some(tmp.path()),
        )
        .unwrap();
        assert!(out.contains("run `run-live` (0 mount(s), 3 setting(s)"), "{out}");
        assert!(out.contains("risk.max_notional_per_order = 250.0"), "{out}");
        assert!(out.contains("No active run row was written"), "storing is not selecting: {out}");
        assert!(out.contains("config activate run"), "the report names the deliberate act: {out}");
        assert!(out.contains("NOTHING WAS WRITTEN"), "{out}");
        assert!(!vike_secrets::db_path_in(tmp.path()).exists());
    }

    /// A typo'd `[risk]` key refuses the WHOLE run — before anything is written, so a half-mirrored
    /// store is not a state this verb can produce.
    #[test]
    fn a_bad_risk_key_refuses_the_run_by_name_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("run-live.toml");
        std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_levrage = 3.0\n").unwrap();

        let err = execute(
            &Args { no_settings: true, profile: Some(profile), ..Args::default() },
            Some(tmp.path()),
        )
        .unwrap_err();
        assert!(err.contains("max_levrage"), "{err}");
        assert!(
            !vike_secrets::db_path_in(tmp.path()).exists(),
            "the profile is lowered BEFORE any write, so nothing was created"
        );
    }

    /// **The three flags are compared pairwise, in WRITE ORDER, and every pair is covered.**
    ///
    /// A loop that compared only neighbours would miss `run` against `recorder`, which is the pair
    /// the measured defect actually used (`--profile-name default` plus `--recorder`'s own default
    /// name), so the middle flag is left out of two of the three cases deliberately.
    #[test]
    fn every_pair_of_profile_names_is_compared_and_a_distinct_set_is_allowed() {
        for (run, daemon, recorder, first, second) in [
            (Some("x"), Some("x"), None, "run", "daemon"),
            (Some("x"), None, Some("x"), "run", "recorder"),
            (None, Some("x"), Some("x"), "daemon", "recorder"),
            (Some("x"), Some("x"), Some("x"), "run", "daemon"),
        ] {
            let err = refuse_one_name_under_two_kinds(run, daemon, recorder)
                .expect_err("one name under two kinds must be refused");
            assert!(err.contains("`x`"), "the refusal must name the name: {err}");
            assert!(err.contains(&format!("`{first}` profile")), "names the first kind: {err}");
            assert!(err.contains(&format!("`{second}` profile")), "names the second kind: {err}");
            assert!(err.contains("NOTHING WAS WRITTEN"), "and it is TRUE here: {err}");
        }
        // ...and three distinct names, or fewer than two flags, are not a collision.
        for (run, daemon, recorder) in [
            (Some("a"), Some("b"), Some("c")),
            (Some("a"), None, None),
            (None, None, Some("a")),
            (None, None, None),
        ] {
            assert!(refuse_one_name_under_two_kinds(run, daemon, recorder).is_ok());
        }
    }

    /// **A `--recorder` that is not given may not collide**, which is the one case the report's
    /// unconditional `recorder_name` makes easy to get wrong: that name is resolved for every run
    /// so the dry-run can print it, and comparing it without the FLAG would refuse
    /// `--profile-name default` on a box that never asked for a recorder body.
    #[test]
    fn a_recorder_name_with_no_recorder_flag_is_not_a_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("run-live.toml");
        std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 10.0\n")
            .unwrap();
        let out = execute(
            &Args {
                dry_run: true,
                no_settings: true,
                profile: Some(profile),
                profile_name: Some(crate::cmd::config_mirror_recorder::DEFAULT_PROFILE_NAME.into()),
                ..Args::default()
            },
            Some(tmp.path()),
        )
        .expect("no recorder was asked for, so `default` is free");
        assert!(out.contains("run `default`"), "{out}");
    }

    /// **THE MEASURED DEFECT, at the rung that decides it.** `--profile-name default` beside a
    /// `--recorder` whose own default name is `default` refused NOTHING at plan time, reported
    /// *would mirror* in `--dry-run` and exited 0, then wrote the run body and had the store refuse
    /// the recorder half — printing `NOTHING WAS WRITTEN` over a committed row.
    ///
    /// Asserted through [`execute`] rather than through the helper so the DRY RUN is covered: it is
    /// the half that reported a write the store was going to refuse, and a check placed one line
    /// lower (after the plans, before the writes) would pass the helper's own test and still
    /// promise it.
    #[test]
    fn one_name_under_two_kinds_is_refused_before_any_plan_dry_run_included() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("run-live.toml");
        std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 10.0\n")
            .unwrap();
        let rec = tmp.path().join("rec.toml");
        std::fs::write(&rec, "store = \"market_data/hist\"\n").unwrap();

        for dry_run in [true, false] {
            let outcome = execute(
                &Args {
                    dry_run,
                    no_settings: true,
                    profile: Some(profile.clone()),
                    profile_name: Some("default".to_string()),
                    recorder: Some(rec.clone()),
                    ..Args::default()
                },
                Some(tmp.path()),
            );
            let Err(err) = outcome else {
                panic!("one name under two kinds must be refused (dry_run: {dry_run})")
            };
            assert!(err.contains("`run` profile") && err.contains("`recorder` profile"), "{err}");
            assert!(err.contains("NOTHING WAS WRITTEN"), "{err}");
            assert!(!err.contains("would mirror"), "a rehearsal may not promise it: {err}");
            assert!(
                !vike_secrets::db_path_in(tmp.path()).exists(),
                "refused ABOVE the write phase, so not even a store was reached"
            );
        }

        // ⚠ …and ABOVE THE PLANS, which the assertions so far cannot see: a plan writes nothing
        // either, so "no database exists" is true of both placements. What distinguishes them is
        // WHICH refusal an operator reads when there is also something wrong with the file. With
        // the check above the plans it is the collision; one line lower it is whatever
        // `config_mirror_profile::plan` says about a file it cannot open — and the operator then
        // fixes the file and meets the collision on the next run instead of this one.
        let err = execute(
            &Args {
                no_settings: true,
                profile: Some(tmp.path().join("not-here.toml")),
                profile_name: Some("default".to_string()),
                recorder: Some(rec),
                ..Args::default()
            },
            Some(tmp.path()),
        )
        .expect_err("the collision is decided before the file is opened");
        assert!(err.contains("`recorder` profile"), "the COLLISION, not the file: {err}");
        assert!(!err.contains("not-here.toml"), "the file was never opened: {err}");
    }

    /// **A failure with nothing on disk passes the store's words through UNCHANGED** — they are
    /// true at that point, and re-scoping a true sentence would be its own small lie.
    #[test]
    fn a_failure_before_any_write_inherits_the_stores_own_words() {
        let err = write_phase_error(&[], "the four settings files", "boom. NOTHING WAS WRITTEN.");
        assert_eq!(err, "boom. NOTHING WAS WRITTEN.");
    }

    /// **…and a failure AFTER a half has committed may not print the run-scope claim.**
    ///
    /// Every spelling the tree actually writes is covered, because the store messages reachable
    /// from this verb use all three: `vike_secrets::DbError`'s `NoSettingsDatabase` shouts it,
    /// `ForeignKeys` capitalises the sentence, `Unclassified` lower-cases it.
    #[test]
    fn a_failure_after_a_half_committed_rescopes_the_claim_and_names_what_landed() {
        for raw in [
            "refused. NOTHING WAS WRITTEN.",
            "refused. Nothing was written.",
            "refused. nothing was written.",
        ] {
            let err = write_phase_error(
                &["the `setting` and `venue_arming` tables".to_string()],
                "the run profile `x`",
                raw,
            );
            assert!(
                !err.contains("NOTHING WAS WRITTEN.")
                    && !err.contains("Nothing was written.")
                    && !err.contains("nothing was written."),
                "the run-scope claim is FALSE here and may not print: {err}"
            );
            assert!(
                err.contains("BY THAT ONE WRITE") || err.contains("by that one write"),
                "{err}"
            );
            assert!(err.contains("PART OF THIS RUN WAS ALREADY WRITTEN"), "{err}");
            assert!(err.contains("`setting` and `venue_arming`"), "it names what landed: {err}");
            assert!(err.contains("the run profile `x`"), "…and what failed: {err}");
        }
        // Two landings read as English, which is the only thing the plural arm decides.
        let two = write_phase_error(&["a".to_string(), "b".to_string()], "c", "boom");
        assert!(two.contains("a, b have already committed"), "{two}");
    }
}
