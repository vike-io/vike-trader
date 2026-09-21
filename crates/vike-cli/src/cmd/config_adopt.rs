//! **`vike-cli config compare` and `vike-cli config adopt`** — the crossing, as two operator acts.
//!
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md` ends with the settings
//! DATABASE answering for every settings key and the four files retired. The act that crosses that
//! line is `config adopt`, and everything about how it is shaped follows from one requirement:
//! **the daemon this touches signs real orders and is running right now.**
//!
//! # Why the crossing is an OPERATOR ACT and not a release
//!
//! The alternative was a probe the new binary evaluates on its own — a row count, the tables'
//! existence — which would fire the instant the release lands, on every box at once, with no
//! rehearsal and with a redeploy as the only rollback. This shape instead gives:
//!
//! * a release that **changes nothing on any box**, because no box carries a seal on the day it
//!   ships (so the risky deploy contains no behaviour change, and the deploy pre-flight proves it);
//! * a flip performed **with the daemon still running on the old resolution**, whose result
//!   `vike-cli config show` proves BEFORE any restart;
//! * a rollback that is **one row deleted**, not a redeploy of a trading daemon.
//!
//! # The two verbs
//!
//! * **`config compare`** resolves this box twice — the four files alone, then the rows alone —
//!   through the same loader, and prints a per-key diff. Exit 0 identical, 1 different. It is the
//!   pre-flight the crossing turns on, and it is worth running on its own long before anybody
//!   intends to adopt.
//! * **`config adopt`** re-runs that comparison internally and **refuses unless it is identical**,
//!   re-reads every row through the boot's own reader, and only then writes the seal — LAST, in one
//!   transaction. `--undo` deletes it; the files answer again; no redeploy.
//!
//! ⚠ **The re-read is what LICENSES the dispositions on the far side**, and above all
//! `vike_config::mirror::apply_adopted_rows` inverting an unreadable row from a degrade into a
//! refusal. That inversion is not a preference about strictness: it rests on this command having
//! read every row successfully at a moment somebody chose. An adopted store holding an unreadable
//! row therefore means the rows changed after adoption by some route other than the validated
//! writer — which is the hand-`INSERT` class the illegal-row refusal already covers.
//!
//! # ⚠ What neither verb may ever say
//!
//! **No message here may name `rm`, and none may name the database FILE as something to delete.**
//! Deleting `settings/db/vike.db` makes `vike_secrets::Backend` answer `Files` for CREDENTIALS on
//! that box — every venue silently on paper, and the only copy of the box's venue keys gone. The
//! repair for a database that will not open at all is a restore, out of band. That sentence is here
//! because the JSON incident's refusal named a command that did not work; this one must not name a
//! command that works and is catastrophic.

use std::path::Path;
use std::process::ExitCode;

use crate::cmd::args::{self, Flags};

pub const COMPARE_USAGE: &str = "\
usage: vike-cli config compare

Resolve this box's settings TWICE — from the four files alone, then from the settings
database's rows alone — and print every key the two disagree about.

Neither half applies the environment or the CLI layer: both sit ABOVE the two sources, so
including them could only hide a disagreement. The comparison is over RESOLVED VALUES rather
than row bytes, because a key stated at exactly its default is a real row that changes nothing.

exit 0  the two resolutions are identical (the state `config adopt` requires)
exit 1  they differ, or a settings file could not be read
";

pub const ADOPT_USAGE: &str = "\
usage: vike-cli config adopt [--undo] [--dry-run]

Make the settings DATABASE answer for every settings key on this box. The four settings files
are then not opened for resolution at all — they become stale drafts, and editing one changes
nothing until `vike-cli config mirror` files it back into the rows.

It REFUSES unless `vike-cli config compare` reports identical, and unless every row reads back
through the loader a daemon boots with. Nothing is written until both pass.

  --undo      delete the seal: the four files answer again, immediately, with the rows left
              exactly where they are. This is the rollback, and it needs no redeploy.
  --dry-run   report the verdict and write nothing.

⚠ Adopting changes NO value (that is what `compare` proves). What it changes is WHICH ARTIFACT
  those values come from — so after adopting, a running daemon still holds what it BOOTED with
  until it is restarted. Prove the new resolution with `vike-cli config show` first.
";

/// `vike-cli config compare`.
pub fn run_compare(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    // `config compare` takes no options beyond help — it resolves this box twice and prints the
    // difference, and there is nothing to configure about that. An `if let` rather than a `while`
    // because one unknown flag already returns.
    let mut flags = Flags::new(args);
    if let Some((flag, _)) = flags.next_flag() {
        if flag == "-h" || flag == "--help" {
            println!("{COMPARE_USAGE}");
            return ExitCode::SUCCESS;
        }
        return args::exit_for_parse_error(
            "config compare",
            COMPARE_USAGE,
            &format!("unknown option '{flag}'"),
        );
    }

    match compare(settings_dir) {
        Ok((report, identical)) => {
            println!("{report}");
            if identical { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(msg) => {
            eprintln!("vike-cli config compare: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// The comparison and its rendering. `bool` is *identical*, which is the exit code.
fn compare(settings_dir: Option<&Path>) -> Result<(String, bool), String> {
    let dir = settings_dir.ok_or(NO_DIRECTORY)?;
    let source = vike_secrets::read_settings_in(dir).map_err(|e| e.to_string())?;
    let rows = source.rows().ok_or_else(|| {
        format!("{source}. There are no settings rows to compare against — run `vike-cli config mirror` first.")
    })?;

    let drift = vike_config::compare_sources(dir, rows);
    let mut out = String::new();
    out.push_str(&format!("settings directory: {}\nstore:              {source}\n", dir.display()));
    out.push_str(&format!(
        "files on disk:      {}\n\n",
        if drift.files_present.is_empty() {
            "(none)".to_string()
        } else {
            drift.files_present.join(", ")
        }
    ));
    for why in &drift.unreadable {
        out.push_str(&format!("⚠ {why}\n"));
    }
    if drift.keys.is_empty() && drift.unreadable.is_empty() {
        out.push_str(
            "IDENTICAL — the rows resolve to exactly what the files resolve to, `policy.venues`' \
             stated-at-all flag included.\n",
        );
        return Ok((out, true));
    }
    if !drift.keys.is_empty() {
        let wk = drift.keys.iter().map(|k| k.key.len()).max().unwrap_or(0).max("SETTING".len());
        let wf = drift.keys.iter().map(|k| k.file.len()).max().unwrap_or(0).max("FILES".len());
        out.push_str(&format!("{:<wk$}  {:<wf$}  {}\n", "SETTING", "FILES", "STORE"));
        for k in &drift.keys {
            out.push_str(&format!("{:<wk$}  {:<wf$}  {}\n", k.key, k.file, k.store));
        }
        out.push('\n');
    }
    out.push_str(
        "DIFFERENT — `vike-cli config adopt` refuses in this state. Run `vike-cli config mirror` \
         to re-derive every row from the files (the files are the source), then compare again.\n",
    );
    Ok((out, false))
}

/// `vike-cli config adopt`.
pub fn run_adopt(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let (mut undo, mut dry_run) = (false, false);
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => {
                println!("{ADOPT_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--undo" => {
                if let Err(e) = args::no_value(&flag, inline) {
                    return args::exit_for_parse_error("config adopt", ADOPT_USAGE, &e);
                }
                undo = true;
            }
            "--dry-run" => {
                if let Err(e) = args::no_value(&flag, inline) {
                    return args::exit_for_parse_error("config adopt", ADOPT_USAGE, &e);
                }
                dry_run = true;
            }
            other => {
                return args::exit_for_parse_error(
                    "config adopt",
                    ADOPT_USAGE,
                    &format!("unknown option '{other}'"),
                );
            }
        }
    }
    let result =
        if undo { undo_adoption(settings_dir, dry_run) } else { adopt(settings_dir, dry_run) };
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config adopt: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// **Write the seal** — after both preconditions, and only then.
fn adopt(settings_dir: Option<&Path>, dry_run: bool) -> Result<String, String> {
    let dir = settings_dir.ok_or(NO_DIRECTORY)?;
    let source = vike_secrets::read_settings_in(dir).map_err(|e| e.to_string())?;
    let rows = source.rows().ok_or_else(|| {
        format!(
            "{source}. Adopting a store with no settings rows would seal this box into resolving \
             from NOTHING — no ceiling, no dead-man, and every venue capped `paper` with nothing \
             recording that an arming was ever stated. Run `vike-cli config mirror` first."
        )
    })?;

    if let Some(existing) = source.adoption() {
        return Ok(format!(
            "already adopted on {} by {} — the settings database already answers for every key on \
             this box. `vike-cli config mirror` files a changed file into the rows; `vike-cli \
             config adopt --undo` hands the decision back to the files.",
            existing.adopted_at, existing.tool_version
        ));
    }

    // PRECONDITION 1 — the two resolutions must be IDENTICAL. This is what makes the crossing a
    // change of ARTIFACT rather than a change of VALUE, and it is also what closes the
    // empty-tables cell with no special case: a store `secrets migrate` created and nobody
    // mirrored fails here, naming every ceiling that would have vanished.
    let drift = vike_config::compare_sources(dir, rows);
    if !drift.is_identical() {
        let named: Vec<String> = drift
            .keys
            .iter()
            .map(|k| format!("  {} — files {}, store {}", k.key, k.file, k.store))
            .collect();
        return Err(format!(
            "the files and the rows do not resolve to the same thing, so adopting would CHANGE \
             what this box runs on rather than only changing where it reads it from:\n{}{}\n\
             Run `vike-cli config mirror` to re-derive every row from the files, then retry. \
             NOTHING WAS WRITTEN.",
            named.join("\n"),
            if drift.unreadable.is_empty() {
                String::new()
            } else {
                format!("\n{}", drift.unreadable.join("\n"))
            }
        ));
    }

    // PRECONDITION 2 — every row must read back through the loader a daemon boots with. This is
    // the act that LICENSES `apply_adopted_rows` treating an unreadable row as a FAULT on the far
    // side rather than degrading past it: the seal is positive evidence that the rows were all
    // readable at a moment somebody chose, so a row that will not read now arrived by some route
    // other than the validated writer.
    let mut probe = vike_config::Settings::default();
    vike_config::apply_rows(&mut probe, rows)
        .map_err(|e| format!("a settings row does not resolve: {e}\nNOTHING WAS WRITTEN."))?;
    if let Some(why) = probe.warnings.iter().find(|w| w.contains("settings database was NOT read"))
    {
        return Err(format!(
            "a settings row cannot be READ at all: {why}\nOn an unadopted box that degrades and \
             the files answer. Adopting would leave this box resolving that key from the \
             compiled-in default with no file underneath it — which for a ceiling means NO \
             CEILING — and would put it in the state where `vike-cli trade` and `vike-cli mcp` \
             refuse. Run `vike-cli config mirror` to re-derive the rows from the files, then \
             retry. NOTHING WAS WRITTEN."
        ));
    }

    // ...and the two facts the seal has to carry that the rows cannot: which files existed when
    // this box crossed, and whether an arming had ever been STATED. See `vike_secrets::Adoption`.
    let venues_declared = probe.policy.venues.is_declared();
    let files_present = drift.files_present.join(",");

    if dry_run {
        return Ok(format!(
            "WOULD ADOPT (nothing written)\n  settings directory: {}\n  rows:               \
             {source}\n  files present:      {}\n  arming stated:      {venues_declared}\n\n\
             The two resolutions are identical and every row reads back. `vike-cli config adopt` \
             would write the seal.",
            dir.display(),
            if files_present.is_empty() { "(none)" } else { &files_present }
        ));
    }

    let db = dir.join("db").join("vike.db");
    let written = vike_secrets::write_adoption(
        &db,
        &now_utc_rfc3339(),
        concat!("vike-cli ", env!("CARGO_PKG_VERSION")),
        &files_present,
        venues_declared,
    )
    .map_err(|e| format!("{e}"))?;

    Ok(format!(
        "ADOPTED — the settings database now answers for every settings key on this box.\n  \
         sealed on:      {}\n  by:             {}\n  setting rows:   {}\n  arming rows:    {}\n  \
         arming stated:  {}\n  files present:  {}\n\n\
         ⚠ A RUNNING daemon still holds what it BOOTED with. Prove the new resolution with \
         `vike-cli config show` — its ORIGIN column should read `db` and its header should say \
         `precedence: env > the settings database > default   (policy: STORE ONLY)` — and only \
         then restart.\n\
         ⚠ The settings files are still on disk and are now INERT drafts. `vike-cli config mirror` \
         files a change from a file into the rows; `vike-cli config adopt --undo` hands the \
         decision back to the files, with no redeploy.",
        written.adopted_at,
        written.tool_version,
        written.setting_rows,
        written.arming_rows,
        written.venues_declared,
        if files_present.is_empty() { "(none)" } else { &files_present }
    ))
}

/// **Delete the seal** — the rollback, and the whole of it.
///
/// The rows are left exactly where they are, which is why this needs no redeploy and why the
/// crossing is safe to rehearse: nothing about the data is undone, because nothing about the data
/// was changed.
fn undo_adoption(settings_dir: Option<&Path>, dry_run: bool) -> Result<String, String> {
    let dir = settings_dir.ok_or(NO_DIRECTORY)?;
    let db = dir.join("db").join("vike.db");
    if dry_run {
        let source = vike_secrets::read_settings_in(dir).map_err(|e| e.to_string())?;
        return Ok(match source.adoption() {
            Some(a) => format!(
                "WOULD UN-ADOPT (nothing written) — the seal written on {} would be deleted and \
                 the four settings files would answer again immediately. The rows stay where they \
                 are.",
                a.adopted_at
            ),
            None => "not adopted — the settings files already answer on this box.".to_string(),
        });
    }
    let removed = vike_secrets::clear_adoption(&db).map_err(|e| format!("{e}"))?;
    Ok(if removed {
        "UN-ADOPTED — the four settings files answer again. The settings rows are untouched, so \
         `vike-cli config adopt` can re-seal this box once `vike-cli config compare` reports \
         identical.\n\
         ⚠ A RUNNING daemon still holds what it BOOTED with; restart it to pick this up."
            .to_string()
    } else {
        "not adopted — the settings files already answer on this box. Nothing was written."
            .to_string()
    })
}

/// The refusal both verbs give when no project was resolved.
const NO_DIRECTORY: &str = "no settings directory resolved, so there is nothing to compare or adopt. Set \
     VIKE_SETTINGS_DIR, or run from a project that has one.";

/// The current time as RFC 3339 UTC, for the seal's disclosure column.
///
/// Hand-rolled from `SystemTime` for the reason the rest of this crate's timestamps are: no `chrono`
/// in the CLI's dependency closure, and a disclosure column does not justify adding one.
fn now_utc_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm), the same one `vike_model`'s renderers use.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", tod / 3600, (tod % 3600) / 60, tod % 60)
}
