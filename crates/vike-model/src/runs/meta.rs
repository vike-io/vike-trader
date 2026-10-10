//! The per-run TAG SIDECAR: its schema, its document, and its one reader and one writer.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::read::read_doc;
use super::{META_FILE, RunPersistError, RunReadError, utc_rfc3339, write_json};

#[cfg(doc)]
use super::{Mark, RunManifest, SERIES_SCHEMA, create_run_dir};

// ─── the per-run TAG SIDECAR, and the MARK store beside the runs root ──────────────────────────
//
// Two documents, because they answer two different questions and one store could not answer both. A
// TAG is a LABEL that belongs to one run and travels with it, so it lives INSIDE the run directory.
// A MARK is a POINTER — a name that must resolve to a run without opening every run directory in
// the tree — so it lives in a file NAMED for the mark, under a root that is a SIBLING of the runs
// root (`crate::paths::state_path::MARKS_SUBDIR` carries why a child would be wrong).
//
// ⚠ **Neither is a field on [`RunManifest`], and that is a decision rather than an omission.**
// [`write_run_with`] is write-once and there is no rewrite entry point: the manifest IS the
// completion marker a listing relies on, so rewriting it to add a tag would mean a run momentarily
// has none. And `RunManifest`'s fields are public with no constructor precisely so that a new COMMON
// field stops every producer compiling until it decides what to put there — which is right for a
// field every producer must answer, and wrong for one only a later verb ever writes.

/// The schema version [`RunMeta`] and [`Mark`] open with.
///
/// Shipped WITH the documents rather than after them — §13 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`: a schema tag retrofitted onto
/// documents already in people's scripts is the one item that gets strictly more expensive every
/// week. [`SERIES_SCHEMA`] carries the argument for versioning a run directory's documents at all.
pub const META_SCHEMA: u32 = 1;

/// A note somebody attached to a run, with when.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunNote {
    /// RFC-3339 UTC to the second — [`utc_rfc3339`], the one spelling every timestamp in this module
    /// takes.
    pub at: String,
    /// The note, verbatim.
    pub text: String,
}

/// What a user attached to a run after it finished: labels and notes. The content of [`META_FILE`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunMeta {
    /// [`META_SCHEMA`] at write time; `0` in a document written before this field existed, carried
    /// for the same reason [`RunManifest::schema`] carries it.
    #[serde(default)]
    pub schema: u32,
    /// Labels, deduped, in FIRST-INSERT order so a rendered row does not shuffle between calls.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Notes, APPEND-ONLY. A note is evidence; the second one must not delete the first.
    #[serde(default)]
    pub notes: Vec<RunNote>,
}

impl Default for RunMeta {
    fn default() -> Self {
        Self { schema: META_SCHEMA, tags: Vec::new(), notes: Vec::new() }
    }
}

/// Read a run's tag sidecar.
///
/// An ABSENT file is an EMPTY [`RunMeta`], never an error — see [`META_FILE`]: a run nobody has
/// tagged, and every run minted before tagging existed, is the ordinary case. A file that EXISTS and
/// will not parse IS an error, which is the line every reader in this workspace draws between "not
/// configured" and "broken".
pub fn read_meta(dir: &Path) -> Result<RunMeta, RunReadError> {
    match read_doc::<RunMeta>(dir, META_FILE) {
        Ok(meta) => Ok(meta),
        Err(RunReadError::Missing { .. }) => Ok(RunMeta::default()),
        Err(other) => Err(other),
    }
}

/// Add tags and/or a note to a run, in place. The ONE writer of [`META_FILE`].
///
/// ⚠ **Both `vike-cli backtest run --tag/--note` and `vike-cli backtest tag --add/--note` call
/// this**, and that is a decision rather than a coincidence: they write the same document, and two
/// writers with two formats is how a sidecar comes to mean different things depending on which verb
/// made it. §5.8 and §7.2 of the CLI-surface design each give a verb that flag pair and neither says
/// who owns the file; this function is the answer.
///
/// `at` is the clock second, passed IN — this crate contains no ambient clock read, the same rule
/// [`create_run_dir`] follows and for the same reason.
///
/// Tags DEDUPE and keep first-insert order; notes APPEND. Passing neither is not an error here —
/// "you asked for no change" is a question about the command line, and belongs where the command
/// line is parsed.
///
/// ⚠ **An existing sidecar that will not parse is a REFUSAL, not an overwrite.** The file is the
/// only copy of somebody's notes; re-minting it from an empty document would delete them silently,
/// which is the one outcome a metadata write may not have.
pub fn add_tags(
    dir: &Path,
    tags: &[String],
    note: Option<&str>,
    at: i64,
) -> Result<RunMeta, RunPersistError> {
    let path = dir.join(META_FILE);
    let mut meta = read_meta(dir).map_err(|e| RunPersistError::Write {
        path: path.clone(),
        why: format!(
            "the sidecar already there could not be read ({e}) — refusing to overwrite it, because \
             it is the only copy of whatever is in it"
        ),
    })?;
    meta.schema = META_SCHEMA;
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || meta.tags.iter().any(|t| t == tag) {
            continue;
        }
        meta.tags.push(tag.to_string());
    }
    if let Some(text) = note {
        meta.notes.push(RunNote { at: utc_rfc3339(at), text: text.to_string() });
    }
    write_json(&path, META_FILE, &meta)?;
    Ok(meta)
}
