//! The MARK store: a NAME that points at a run, its move history, its name rules, read and write.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{META_SCHEMA, utc_rfc3339};

#[cfg(doc)]
use super::RunManifest;

/// How many previous pointers a mark keeps.
///
/// A mark moved on every CI run would otherwise grow one file without bound — the same failure the
/// log retention in this workspace exists for, on a smaller scale. Most-recent-first, oldest
/// dropped.
pub const MARK_HISTORY_MAX: usize = 32;

/// The deepest a mark name may nest. A mark is a LABEL, not a tree: `baseline/momentum` and
/// `nightly/eu/open` are names; anything deeper is somebody using the marks root as a filesystem.
const MARK_MAX_DEPTH: usize = 3;

/// The Windows RESERVED DEVICE NAMES, which are unopenable there under ANY extension.
///
/// ⚠ No test in this workspace runs on Windows (`CLAUDE.md`: "No Windows TEST runs anywhere"), so
/// this list has to be right by construction rather than by measurement. A mark called `con` would
/// write here, resolve here, and be unopenable on the one platform nothing would catch it on.
const WINDOWS_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Where a mark pointed before it was moved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarkMove {
    /// The run it pointed at.
    pub run_id: String,
    /// When it was set, RFC-3339 UTC.
    pub marked_at: String,
    /// The note it carried, if any.
    pub note: Option<String>,
}

/// A NAME that points at a run, and the moves it has made.
///
/// # Why a mark exists at all
///
/// A run id moves every time you run. A gate written against one is a gate that passes once and then
/// names a run nobody is comparing to. §7.2 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`: "A **mark** is what makes
/// `gate` and `diff` usable: a stable second operand that does not move when you run again."
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mark {
    /// [`META_SCHEMA`] at write time.
    #[serde(default)]
    pub schema: u32,
    /// The mark's own name, duplicated into the file for the same reason [`RunManifest::run_id`] is:
    /// a document copied out of its directory should still say what it is. The FILE NAME decides,
    /// exactly as the run DIRECTORY does.
    pub name: String,
    /// The run it points at now.
    pub run_id: String,
    /// When it was pointed there, RFC-3339 UTC.
    pub marked_at: String,
    /// The note given when it was set.
    #[serde(default)]
    pub note: Option<String>,
    /// Where it pointed before, MOST RECENT FIRST, capped at [`MARK_HISTORY_MAX`].
    #[serde(default)]
    pub history: Vec<MarkMove>,
}

/// Refuse a mark name that could not be, or should not be, a file path.
///
/// The name becomes `<marks_root>/<name>.json`, so this is a SECURITY boundary and a PORTABILITY one
/// at once rather than tidiness:
///
/// * traversal (`..`, a leading or doubled `/`, an absolute path) would write OUTSIDE the marks
///   root, from a string somebody typed on a command line;
/// * a leading `.` collides with the dot-entry skip every directory scan in this workspace applies,
///   so such a mark would be written and then be invisible;
/// * `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|`, whitespace and control bytes are unwritable or
///   ambiguous on Windows;
/// * the Windows device names (`con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`,
///   case-insensitive, and under ANY extension) are unopenable there;
/// * depth is capped at three `/`-separated parts, because a mark is a label and not a tree.
///
/// The `Err` is the SENTENCE a caller prints, so it names what was wrong rather than saying
/// "invalid".
pub fn valid_mark_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a mark name is required (for example `baseline/momentum`, or `prod`)".into());
    }
    if name != name.trim() {
        return Err(format!("'{name}': a mark name may not begin or end with whitespace"));
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() > MARK_MAX_DEPTH {
        return Err(format!(
            "'{name}': a mark name may have at most {MARK_MAX_DEPTH} '/'-separated parts — a mark \
             is a label, not a directory tree"
        ));
    }
    // ⚠ `.copied()` gives a `&str` rather than the `&&str` a plain `&segments` loop yields — the
    // device-name check below hands one to `unwrap_or`, which needs the same type the split returns.
    for segment in segments.iter().copied() {
        if segment.is_empty() {
            return Err(format!(
                "'{name}': an empty part — a mark name may not start or end with '/' and may not \
                 contain '//'"
            ));
        }
        if segment.starts_with('.') {
            return Err(format!(
                "'{name}': the part '{segment}' starts with '.' — that is a traversal ('..'), or a \
                 dot-entry every directory scan in this workspace skips, so the mark would be \
                 written and then be invisible"
            ));
        }
        if let Some(bad) =
            segment.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        {
            return Err(format!(
                "'{name}': the part '{segment}' contains '{bad}' — a mark name takes ASCII letters, \
                 digits, '.', '_', '-' and '/' only, because it becomes a file path on every \
                 platform this ships to"
            ));
        }
        // Windows refuses a device name under ANY extension, so the STEM is what matters: `con` and
        // `con.baseline` are both unopenable there once `.json` is appended.
        let stem = segment.split('.').next().unwrap_or(segment).to_ascii_lowercase();
        if WINDOWS_DEVICE_NAMES.contains(&stem.as_str()) {
            return Err(format!(
                "'{name}': the part '{segment}' is a reserved Windows device name — a file called \
                 that cannot be opened on Windows under any extension"
            ));
        }
    }
    Ok(())
}

/// Why a mark could not be read or written. Every variant names the PATH or the NAME, because "no
/// such mark" without one is a message an operator cannot act on.
#[derive(Debug)]
pub enum MarkError {
    /// The name is not usable as a path — [`valid_mark_name`] says why, verbatim.
    BadName {
        /// The name as it was typed.
        name: String,
        /// [`valid_mark_name`]'s own sentence.
        why: String,
    },
    /// No such mark.
    Missing {
        /// The name that was looked up.
        name: String,
        /// Where it was looked for.
        path: PathBuf,
    },
    /// It is there and could not be read, or will not parse.
    Read {
        /// The file.
        path: PathBuf,
        /// The operating system's or the parser's own words.
        why: String,
    },
    /// It could not be written.
    Write {
        /// The file.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
}

impl fmt::Display for MarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadName { why, .. } => write!(f, "{why}"),
            Self::Missing { name, path } => write!(
                f,
                "no mark named '{name}' — nothing at {}. Set one with \
                 `vike-cli backtest tag <run> --as {name}`.",
                path.display()
            ),
            Self::Read { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            Self::Write { path, why } => write!(f, "cannot write {}: {why}", path.display()),
        }
    }
}

impl std::error::Error for MarkError {}

/// The file one mark lives in: `<marks_root>/<name>.json`.
///
/// ⚠ Joined SEGMENT BY SEGMENT rather than as one string, so a name that somehow reached here
/// absolute could not replace the root — `Path::join` with an absolute argument DISCARDS the base.
/// [`valid_mark_name`] already refuses one; this is the belt beside that brace, because the cost of
/// being wrong is a write outside the marks root.
fn mark_path(marks_root: &Path, name: &str) -> PathBuf {
    let mut path = marks_root.to_path_buf();
    let segments: Vec<&str> = name.split('/').collect();
    for (i, segment) in segments.iter().enumerate() {
        if i + 1 == segments.len() {
            path.push(format!("{segment}.json"));
        } else {
            path.push(segment);
        }
    }
    path
}

/// Read one mark by name.
pub fn read_mark(marks_root: &Path, name: &str) -> Result<Mark, MarkError> {
    valid_mark_name(name).map_err(|why| MarkError::BadName { name: name.to_string(), why })?;
    let path = mark_path(marks_root, name);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(MarkError::Missing { name: name.to_string(), path });
        }
        Err(e) => return Err(MarkError::Read { path, why: e.to_string() }),
    };
    serde_json::from_str(&text).map_err(|e| MarkError::Read { path, why: e.to_string() })
}

/// Point a mark at a run, creating it or MOVING it.
///
/// Moving RECORDS the previous pointer in [`Mark::history`] — §7.2's "Re-marking is explicit and
/// recorded", which is only true if the move is kept somewhere a person can see it.
///
/// The write is atomic (temp file beside the target, then rename), so a CI job interrupted mid-write
/// leaves the OLD mark rather than a truncated file — a half-written pointer is worse than a stale
/// one, because a gate would then judge against nothing.
///
/// ⚠ On Windows `rename` FAILS when the destination exists, unlike POSIX, so the replace is
/// `remove_file`-then-`rename` with the removal's `NotFound` treated as success. That is the one
/// platform difference in this module and no test in this workspace can execute it — nothing here
/// runs on Windows — so it is written from the rule rather than from a measurement.
pub fn write_mark(
    marks_root: &Path,
    name: &str,
    run_id: &str,
    note: Option<&str>,
    at: i64,
) -> Result<Mark, MarkError> {
    valid_mark_name(name).map_err(|why| MarkError::BadName { name: name.to_string(), why })?;
    let path = mark_path(marks_root, name);

    // The PREVIOUS pointer, if there is one. A mark that is there and will not parse is a REFUSAL
    // rather than a silent re-mint: the history in it is the evidence §7.2 asks for.
    let previous = match read_mark(marks_root, name) {
        Ok(m) => Some(m),
        Err(MarkError::Missing { .. }) => None,
        Err(other) => return Err(other),
    };

    let mut history = Vec::new();
    if let Some(prev) = previous {
        history.push(MarkMove { run_id: prev.run_id, marked_at: prev.marked_at, note: prev.note });
        history.extend(prev.history);
        history.truncate(MARK_HISTORY_MAX);
    }

    let mark = Mark {
        schema: META_SCHEMA,
        name: name.to_string(),
        run_id: run_id.to_string(),
        marked_at: utc_rfc3339(at),
        note: note.map(str::to_string),
        history,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| MarkError::Write { path: path.clone(), why: e.to_string() })?;
    }
    let mut json = serde_json::to_string_pretty(&mark)
        .map_err(|e| MarkError::Write { path: path.clone(), why: e.to_string() })?;
    json.push('\n');

    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)
        .map_err(|e| MarkError::Write { path: tmp.clone(), why: e.to_string() })?;
    // ⚠ Windows `rename` refuses an existing destination; POSIX replaces it. Removing first is
    // correct on both, and a `NotFound` here is the ordinary first-write case.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(MarkError::Write { path: path.clone(), why: e.to_string() }),
    }
    std::fs::rename(&tmp, &path)
        .map_err(|e| MarkError::Write { path: path.clone(), why: e.to_string() })?;
    Ok(mark)
}
