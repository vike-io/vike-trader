//! `vike-cli backtest path <id> [FILE]` — **print one absolute path on stdout and nothing else**, so
//! `$(…)` substitution works (spec §6.5).
//!
//! Deliberately not a `cat` sub-verb: the registry's job is to hand over a path, and the shell
//! already has `cat`, `head` and `jq`. `jq .sharpe "$(vike-cli backtest path @last report)"` is the
//! shape this exists for.
//!
//! ⚠ **Nothing else goes on stdout — not a header, not a trailing note, not the scan's problems.**
//! A `problems` row from `crate::cmd::runs::scan` is printed to STDERR, because a run directory that
//! would not parse must not end up inside somebody's `$(…)`.

use std::path::{Path, PathBuf};

use vike_model::runs::{MANIFEST_FILE, REPORT_FILE};

use crate::cmd::runs::{Ctx, scan::scan_runs, selector::resolve_one};
use crate::exit::{CliError, CmdResult};

pub(crate) fn run_path(ctx: &Ctx<'_>, selector: &str, file: Option<&str>) -> CmdResult<()> {
    let root = ctx.runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory to \
             look in. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = scan_runs(root);
    for problem in &scan.problems {
        eprintln!("vike-cli backtest path: {problem}");
    }
    let run = resolve_one(&scan, ctx.marks_root, selector)?;

    let target = match file {
        // No FILE: the run DIRECTORY itself. That is the useful default — it is what a person pastes
        // into a file manager and what a script `cd`s into.
        None => run.dir.clone(),
        Some(name) => resolve_file(&run.dir, name)?,
    };
    if !target.exists() {
        return Err(CliError::failed(format!(
            "{} does not exist — the run is there but that file is not (an unfinished run has no \
             {MANIFEST_FILE}; a run that computed nothing has no {REPORT_FILE})",
            target.display()
        )));
    }
    // `std::path::absolute` rather than `canonicalize`: it needs no symlink resolution and, on
    // Windows, does not hand back a `\\?\` prefix that half the shell world cannot use. A failure
    // falls back to the joined path, which is still correct when the runs root is already absolute.
    let shown = std::path::absolute(&target).unwrap_or(target);
    println!("{}", shown.display());
    Ok(())
}

/// Resolve the optional FILE argument INSIDE the run directory.
///
/// Two names are sugar for the two files a run directory is defined to hold, so nobody has to
/// remember the extension; anything else is joined verbatim, which is what makes this survive the
/// run record growing more files. **A name that would escape the run directory is refused** — `path`
/// hands its answer to `$(…)`, and a `..` that walked out of the tree would be a path the operator
/// did not ask for, substituted into a command they did.
fn resolve_file(dir: &Path, name: &str) -> Result<PathBuf, CliError> {
    let leaf = match name {
        "manifest" | "manifest.json" => MANIFEST_FILE,
        "report" | "report.json" => REPORT_FILE,
        other => other,
    };
    let p = Path::new(leaf);
    // ⚠ **`is_absolute()` is NOT the whole test, and the gap is Windows-shaped.** A drive-qualified
    // RELATIVE path — `C:secrets.txt` — is neither absolute nor a `ParentDir`, so it passed both
    // halves of the old guard; and `PathBuf::push` with a prefixed path REPLACES the receiver rather
    // than appending to it, so the join handed back a path outside the run directory entirely. A
    // bare `RootDir` (`\file`) is the same class one step along. Both are matched by COMPONENT here
    // rather than by string inspection, which is what makes this a property of the path type instead
    // of a list of spellings somebody has to keep current.
    let escapes = p.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        )
    });
    if p.is_absolute() || escapes {
        return Err(CliError::usage(format!(
            "'{name}' is not a file inside a run directory — `path` names one of that run's own \
             files ({MANIFEST_FILE}, {REPORT_FILE}, or another the producer wrote)"
        )));
    }
    Ok(dir.join(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `..` that walked out of the run directory would be a path the operator did not ask for,
    /// substituted into a command they did — so it is a USAGE refusal rather than a join.
    #[test]
    fn a_file_argument_cannot_escape_the_run_directory() {
        let dir = Path::new("/runs/1789213143-8821-00");
        for escape in ["../../etc/passwd", "..", "a/../../b", "/etc/passwd"] {
            let e = resolve_file(dir, escape).expect_err(escape);
            assert_eq!(e.exit, crate::exit::Exit::Usage, "{escape}: {}", e.msg);
        }
    }

    /// ⚠ **The Windows shapes, which `is_absolute()` does not catch.** `C:secrets.txt` is
    /// drive-qualified and RELATIVE — neither absolute nor a `ParentDir` — and `PathBuf::push` with
    /// a prefixed path REPLACES the receiver, so the join used to hand back a path outside the run
    /// directory entirely. `path` substitutes its answer into somebody's `$(…)`, so that is a file
    /// they did not name reaching a command they did write.
    ///
    /// The parse is platform-dependent (`Component::Prefix` only exists on Windows), so this
    /// asserts the two DIRECTIONS rather than a verdict per spelling: nothing that parses with a
    /// prefix or a root may pass, and the ordinary relative name must still resolve everywhere.
    #[test]
    fn a_drive_qualified_or_rooted_name_cannot_escape_either() {
        let dir = Path::new("/runs/1789213143-8821-00");
        for name in ["C:secrets.txt", r"C:\Windows\system32\config\SAM", r"\etc\passwd"] {
            let parsed = Path::new(name);
            let prefixed = parsed.components().any(|c| {
                matches!(c, std::path::Component::Prefix(_) | std::path::Component::RootDir)
            });
            match resolve_file(dir, name) {
                Err(e) => assert_eq!(e.exit, crate::exit::Exit::Usage, "{name}: {}", e.msg),
                // It parsed as an ordinary relative name on this platform (a unix `C:secrets.txt`
                // is a plain filename), so it must have stayed INSIDE the run directory.
                Ok(p) => {
                    assert!(!prefixed, "{name} parsed with a prefix/root and was still accepted");
                    assert!(p.starts_with(dir), "{name} resolved to {} — outside", p.display());
                }
            }
        }
        // The negative control: an ordinary name still resolves, on every platform.
        assert_eq!(resolve_file(dir, "series.json").unwrap(), dir.join("series.json"));
    }

    /// The two sugar names resolve to the two files a run directory is DEFINED to hold, and
    /// anything else is joined verbatim so this survives the record growing more documents.
    #[test]
    fn the_two_sugar_names_resolve_and_anything_else_joins_verbatim() {
        let dir = Path::new("/runs/1789213143-8821-00");
        assert_eq!(resolve_file(dir, "report").unwrap(), dir.join(REPORT_FILE));
        assert_eq!(resolve_file(dir, "report.json").unwrap(), dir.join(REPORT_FILE));
        assert_eq!(resolve_file(dir, "manifest").unwrap(), dir.join(MANIFEST_FILE));
        assert_eq!(resolve_file(dir, "series.json").unwrap(), dir.join("series.json"));
    }
}
