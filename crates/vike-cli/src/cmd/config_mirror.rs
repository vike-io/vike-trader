//! `vike-cli config mirror` — **write this box's four settings files, and optionally a run
//! profile's `[risk]` table, into the settings database.**
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
//! # `--profile <file>` — Phase 2, and it is a COPY rather than a second authority
//!
//! `run-live.toml`'s `[risk]` table holds the pre-trade ceilings that judge every order the core
//! admits, and no panel can read their values (`vike_config::profile_risk`'s module doc carries the
//! whole argument, including the measurement that `policy.toml`'s same-named ceiling is NOT the one
//! that reaches `vike_exec::RiskLimits`). `--profile` mirrors that table so `vike-cli config show`
//! can print the numbers with the daemon down.
//!
//! **What the flag does not do is the load-bearing half.** Nothing on the mount path reads a
//! mirrored row — the file `vike_core::resolve_profile` opens is still the only thing that builds a
//! `vike_exec::ProfileRisk` — so mirroring cannot arm a venue, raise a ceiling or satisfy
//! `vike_mount::require_live_risk_budget`. And it is a NAME, not a selection: 0057's Question 3
//! (profile SELECTION — environment or row) is open, so this verb writes no active-profile row and
//! `VIKE_RUN_PROFILE` / `--profile` on the DAEMON keeps deciding which file is live, exactly as
//! before. That is also why this flag is explicit rather than defaulting to `VIKE_RUN_PROFILE`:
//! having the mirror quietly pick a profile out of the environment would be that question answered
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
pub const USAGE: &str = "usage: vike-cli config mirror [--dry-run] [--profile <file>]... \
                         [--recorder <file>] [--recorder-name <name>]\n\n  Copy \
                         <project>/settings/{policy,config,preferences,flags}.toml into the \
                         settings\n  database beside them. The files keep winning; nothing this \
                         box resolves changes.\n\n  --profile <file>   ALSO mirror that run \
                         profile's [risk] table, so `config show` can\n                     print \
                         the live pre-trade ceilings with the daemon down. May repeat.\n         \
                         Nothing reads the rows: the profile FILE still judges every order.\n  \
                         --recorder <file>  ALSO store that recorder profile as `recorder` + \
                         `subscription`\n                     rows, so a daemon can be started \
                         with --record-profile <name>.\n                     REFUSED unless the \
                         rows render back to the file they came from.\n  --recorder-name <name>  \
                         the profile NAME the rows are stored under (default \
                         `default`)\n  --dry-run          report what would be written, and write \
                         nothing";

/// Parsed flags. `Debug` so a parser test can `unwrap_err` against it.
#[derive(Debug)]
struct Args {
    dry_run: bool,
    /// Run profiles whose `[risk]` table to mirror, in the order given. Empty is the Phase-1
    /// behaviour, byte-identically: the four files and nothing else.
    profiles: Vec<PathBuf>,
    /// The recorder profile FILE to store as rows, if any. **Explicit, never defaulted to
    /// `<settings>/recorder.toml`** — for the same reason `--profile` is explicit: having the
    /// mirror quietly pick up a file that decides which venue feeds open would be a migration
    /// arming something as a side effect of a tidy-up.
    recorder: Option<PathBuf>,
    /// The profile NAME the recorder rows are stored under.
    recorder_name: Option<String>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out =
        Args { dry_run: false, profiles: Vec::new(), recorder: None, recorder_name: None };
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--dry-run" => {
                args::no_value(&flag, inline)?;
                out.dry_run = true;
            }
            "--profile" => out.profiles.push(PathBuf::from(flags.value(&flag, inline)?)),
            "--recorder" => {
                if out.recorder.is_some() {
                    // One box, one recording daemon, one profile row per name — and a second
                    // `--recorder` would silently overwrite the first's rows under the same
                    // default name. Refuse rather than resolve.
                    return Err(
                        "--recorder given twice. Each recorder profile is stored under a NAME, so \
                         mirror them one at a time with --recorder-name."
                            .to_string(),
                    );
                }
                out.recorder = Some(PathBuf::from(flags.value(&flag, inline)?));
            }
            "--recorder-name" => out.recorder_name = Some(flags.value(&flag, inline)?),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    if out.recorder_name.is_some() && out.recorder.is_none() {
        return Err(
            "--recorder-name names the profile --recorder stores, so it needs --recorder <file>. \
             Alone it configures nothing."
                .to_string(),
        );
    }
    Ok(out)
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

    // The files, validated exactly as a boot validates them. A refusal here names the file and the
    // key, and nothing has been written.
    let rows = vike_config::rows_from_files(dir).map_err(|e| e.to_string())?;

    // ...and the run profiles named on the command line, rendered BEFORE anything is written, so a
    // typo'd `[risk]` key refuses the whole run rather than leaving the settings tables mirrored
    // and the profile half not. Each is validated against `vike_config::PROFILE_RISK_KEYS`, which
    // is gated against `vike_exec::ProfileRisk`'s own fields — see that module's doc for what this
    // does NOT validate and where the profile's whole validation happens.
    let mut profiles = Vec::new();
    for path in &args.profiles {
        profiles.push(vike_config::risk_rows_from_profile(path).map_err(|e| e.to_string())?);
    }
    // Two `--profile` flags naming files of the same NAME would write one row set and report two.
    // The store keys on the name, so the collision is real and is refused rather than resolved.
    for (i, p) in profiles.iter().enumerate() {
        if let Some(j) = profiles.iter().take(i).position(|q| q.profile == p.profile) {
            return Err(format!(
                "two --profile arguments name a file called `{}` ({} and {}), and the store keys a \
                 run profile by its NAME. Mirror them one at a time from directories whose files \
                 are named differently.",
                p.profile,
                args.profiles[j].display(),
                args.profiles[i].display()
            ));
        }
    }

    let counts = {
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
        for p in &profiles {
            shown.push(format!("{} [risk] {}", p.profile, p.rows.len()));
        }
        shown.join(", ")
    };

    // ...and the recorder profile, PLANNED before anything is written — which is where its own
    // fence fires. `crate::cmd::config_mirror_recorder`'s module doc carries why that fence is a
    // round-trip on the BODY rather than `plan_active_row` alone: a recorder profile decides which
    // venue feeds open, so a body that is not what the box runs today is a daemon subscribing to
    // markets nobody asked for.
    let recorder_name = args
        .recorder_name
        .clone()
        .unwrap_or_else(|| crate::cmd::config_mirror_recorder::DEFAULT_PROFILE_NAME.to_string());
    let recorder = match &args.recorder {
        Some(file) => Some(
            crate::cmd::config_mirror_recorder::plan(dir, file, &recorder_name)
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    let recorder_counts = recorder.as_ref().map(|m| {
        format!(
            ", recorder `{recorder_name}` (1 body, {} subscription(s): {})",
            m.subscriptions.len(),
            m.subscriptions.join("; ")
        )
    });
    let recorder_counts = recorder_counts.unwrap_or_default();

    if args.dry_run {
        return Ok(format!(
            "would mirror {counts}{recorder_counts} into {} — NOTHING WAS WRITTEN. The files keep \
             winning either way; this changes no value this box resolves.{}",
            vike_secrets::db_path_in(dir).display(),
            recorder.as_ref().map(active_row_note).unwrap_or_default()
        ));
    }

    let written = vike_secrets::write_settings_in(dir, &rows).map_err(|e| e.to_string())?;
    let mut created = if written.tables_created {
        " (this store was migrated before the settings tables existed; they were created)"
    } else {
        ""
    };
    for p in &profiles {
        let done = vike_secrets::write_profile_risk_in(dir, p).map_err(|e| e.to_string())?;
        if done.table_created {
            created = " (this store was migrated before the settings tables existed; they were \
                       created)";
        }
    }
    let profile_note = if profiles.is_empty() {
        String::new()
    } else {
        // ⚠ Said EVERY time a profile is mirrored, because this is the sentence an operator must
        // not have to infer: a mirrored ceiling is readable, not enforceable.
        " The `[risk]` rows are a DISCLOSURE copy — nothing on the mount path reads them, so the \
         profile FILE is still the only thing that judges an order or satisfies a live mount's \
         risk-budget refusal."
            .to_string()
    };
    let recorder_note = match &recorder {
        None => String::new(),
        Some(m) => {
            crate::cmd::config_mirror_recorder::write(
                dir,
                m,
                now_utc(),
                vike_catalog::AssetClass::SQL_WORDS,
            )?;
            active_row_note(m)
        }
    };
    Ok(format!(
        "mirrored {counts}{recorder_counts} into {}{created}. The files still win — nothing this \
         box resolves has changed.{profile_note}{recorder_note}",
        vike_secrets::db_path_in(dir).display()
    ))
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

    #[test]
    fn a_run_with_no_settings_directory_refuses_rather_than_guessing_one() {
        let err = execute(
            &Args { dry_run: false, profiles: Vec::new(), recorder: None, recorder_name: None },
            None,
        )
        .unwrap_err();
        assert!(err.contains("VIKE_SETTINGS_DIR"), "{err}");
    }

    /// ⚠ The refusal that matters: a project with NO database is not served by creating one. The
    /// existence of that file is what makes the database — rather than `secrets.env` — answer for
    /// every credential on the box.
    #[test]
    fn a_project_with_no_database_is_refused_and_none_is_created() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("policy.toml"), "max_leverage = 2.0\n").unwrap();

        let err = execute(
            &Args { dry_run: false, profiles: Vec::new(), recorder: None, recorder_name: None },
            Some(tmp.path()),
        )
        .unwrap_err();
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

        let out = execute(
            &Args { dry_run: true, profiles: Vec::new(), recorder: None, recorder_name: None },
            Some(tmp.path()),
        )
        .unwrap();
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
        let err = execute(
            &Args { dry_run: true, profiles: Vec::new(), recorder: None, recorder_name: None },
            Some(tmp.path()),
        )
        .unwrap_err();
        assert!(err.contains("market_slippage"), "{err}");
        assert!(err.contains("policy.toml"), "{err}");
    }

    // -----------------------------------------------------------------------------------------
    // `--profile` — 0057 Phase 2
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_profile_flag_takes_a_value_and_may_repeat() {
        assert!(parse_of(&[]).unwrap().profiles.is_empty());
        let both = parse_of(&["--profile", "a.toml", "--profile=b.toml"]).unwrap();
        assert_eq!(
            both.profiles,
            vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")],
            "order is the operator's, and each one is written separately"
        );
        // The shared rule: a valued flag may not eat a following flag.
        assert!(parse_of(&["--profile", "--dry-run"]).is_err());
        assert!(parse_of(&["--profile"]).is_err());
    }

    /// A dry run names the profile and its row count, and still writes nothing.
    #[test]
    fn a_dry_run_reports_the_profile_rows_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("run-live.toml");
        std::fs::write(
            &profile,
            "[risk]\nmax_notional_per_order = 250.0\nmax_total_exposure = 1000.0\n",
        )
        .unwrap();

        let out = execute(
            &Args { dry_run: true, profiles: vec![profile], recorder: None, recorder_name: None },
            Some(tmp.path()),
        )
        .unwrap();
        assert!(out.contains("run-live.toml [risk] 2"), "{out}");
        assert!(out.contains("NOTHING WAS WRITTEN"), "{out}");
        assert!(!vike_secrets::db_path_in(tmp.path()).exists());
    }

    /// A typo'd `[risk]` key refuses the WHOLE run — before the settings tables are written, so a
    /// half-mirrored store is not a state this verb can produce.
    #[test]
    fn a_bad_risk_key_refuses_the_run_by_name_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("run-live.toml");
        std::fs::write(&profile, "[risk]\nmax_levrage = 3.0\n").unwrap();

        let err = execute(
            &Args { dry_run: false, profiles: vec![profile], recorder: None, recorder_name: None },
            Some(tmp.path()),
        )
        .unwrap_err();
        assert!(err.contains("max_levrage"), "{err}");
        assert!(
            !vike_secrets::db_path_in(tmp.path()).exists(),
            "the profile is rendered BEFORE the settings write, so nothing was created"
        );
    }

    /// The store keys a profile by NAME, so two files of the same name are a real collision and
    /// are refused rather than silently resolved to whichever was written last.
    #[test]
    fn two_profiles_of_the_same_file_name_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        for dir in [&a, &b] {
            std::fs::write(dir.join("run-live.toml"), "[risk]\nmax_leverage = 3.0\n").unwrap();
        }
        let err = execute(
            &Args {
                dry_run: true,
                profiles: vec![a.join("run-live.toml"), b.join("run-live.toml")],
                recorder: None,
                recorder_name: None,
            },
            Some(tmp.path()),
        )
        .unwrap_err();
        assert!(err.contains("run-live.toml"), "{err}");
        assert!(err.contains("NAME"), "{err}");
    }
}
