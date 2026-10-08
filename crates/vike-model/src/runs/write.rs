//! The WRITING half: the persist error, the run id, the directory minter and the writers.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    CONFIG_FILE, ID_FINGERPRINT_LEN, MANIFEST_FILE, MINT_ATTEMPTS, REPORT_FILE, RunDir, RunExtras,
    RunManifest, SERIES_FILE, TRADES_FILE,
};

#[cfg(doc)]
use super::RESERVED_FILES;

/// Why a run could not be persisted. Every variant names the PATH it failed on, because "the run
/// was not saved" without a location is a message an operator cannot act on.
#[derive(Debug)]
pub enum RunPersistError {
    /// A directory could not be created.
    Dir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// A file could not be written.
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// A value could not be turned into JSON.
    Serialize {
        /// Which document it was — one of [`RESERVED_FILES`].
        file: &'static str,
        /// serde_json's own words.
        why: String,
    },
    /// [`MINT_ATTEMPTS`] ids in a row were already taken — in practice a runs directory that
    /// refuses creation while reporting `AlreadyExists`, not a genuine flood of runs.
    IdExhausted {
        /// The runs directory that yielded no free id.
        root: PathBuf,
        /// How many ids were tried.
        attempts: u32,
    },
}

impl fmt::Display for RunPersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dir { path, why } => write!(f, "cannot create {}: {why}", path.display()),
            Self::Write { path, why } => write!(f, "cannot write {}: {why}", path.display()),
            Self::Serialize { file, why } => write!(f, "cannot serialize {file}: {why}"),
            Self::IdExhausted { root, attempts } => {
                write!(f, "no free run id under {} after {attempts} tries", root.display())
            }
        }
    }
}

impl std::error::Error for RunPersistError {}

/// Unix seconds as RFC-3339 UTC to the second — the ONE spelling every manifest timestamp takes, so
/// two producers cannot disagree about the format of a common field.
///
/// ⚠ **Chrono-free, and that is load-bearing rather than tidiness.** This module lives in the crate
/// at the BOTTOM of the dependency graph so that `vike-cli` can read a run without linking the
/// engine (`crates/vike-cli/Cargo.toml`'s `vike-model` rationale argues the same trade for the
/// client-order-id generator). `chrono` may not follow it here, so the calendar math is
/// [`crate::time::civil_from_days`] — Howard Hinnant's proleptic-Gregorian algorithm, integer-exact
/// across the whole `i64` range, which that module was consolidated to be the one home for.
///
/// A second no calendar date can hold falls back to the raw number rather than panicking: a manifest
/// is metadata about a run that already succeeded, and must never be the thing that kills it.
pub fn utc_rfc3339(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let (y, mo, d) = crate::time::civil_from_days(days);
    // The one unrepresentable case: a year outside four digits has no RFC-3339 spelling, so the raw
    // number is the honest answer. `i64::MIN`/`i64::MAX` seconds are both far outside it.
    if !(0..=9999).contains(&y) {
        return unix_secs.to_string();
    }
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// The id a run started at `started_at` takes at attempt `seq`.
///
/// `<unix-seconds>-<address|pid>-<seq>`. Public and pure so a test can plant the id the minter
/// WOULD produce rather than re-spelling the format, and so a reader can recognise one.
///
/// ⚠ **The address is SANITIZED, because this becomes a DIRECTORY NAME.** A fingerprint carrying
/// `/`, `\` or `..` would be a path traversal assembled out of a value some producer computed, and
/// slicing a `&str` by BYTES would panic on a non-char-boundary — so this filters to ASCII
/// alphanumerics, takes at most [`ID_FINGERPRINT_LEN`] CHARACTERS, and falls back to the pid form
/// when nothing usable survives.
pub fn run_id_at(started_at: i64, fingerprint: Option<&str>, seq: u32) -> String {
    let addr: Option<String> = fingerprint.map(|fp| {
        fp.chars().filter(char::is_ascii_alphanumeric).take(ID_FINGERPRINT_LEN).collect()
    });
    match addr {
        Some(a) if !a.is_empty() => format!("{started_at}-{a}-{seq}"),
        // No address, or nothing usable in it: the pre-address form, pid and all. Every producer
        // that cannot content-address its inputs keeps exactly the ids it minted before.
        _ => format!("{started_at}-{}-{seq}", std::process::id()),
    }
}

/// Create a fresh run directory under `runs_root`, minting an id that cannot collide with another
/// run's — [`RunManifest::run_id`] carries the rule and why creation IS the check.
///
/// `started_at` is the run's own clock read in unix seconds, passed IN rather than read here so the
/// id and [`RunManifest::started_at`] name the same instant.
///
/// `fingerprint` is the run's INPUT ADDRESS ([`RunManifest::fingerprint`]) or `None` for a producer
/// that cannot compute one. It rides in the id as a SUFFIX, never as the whole name: the seconds
/// stay in front because `crates/vike-studio-core/src/listing.rs`'s `list_runs` sorts by directory
/// NAME and its module doc calls that chronological for exactly that reason, and
/// `crates/vike-studio/src/panes/research.rs` reverses the same list for "Newest first". A bare content
/// hash would make both arbitrary with nothing going red.
///
/// `runs_root` and its parents are created when absent: a project that has run nothing has no
/// `user_data/runs/`, which is a fresh install rather than an error.
pub fn create_run_dir(
    runs_root: &Path,
    started_at: i64,
    fingerprint: Option<&str>,
) -> Result<RunDir, RunPersistError> {
    std::fs::create_dir_all(runs_root)
        .map_err(|e| RunPersistError::Dir { path: runs_root.to_path_buf(), why: e.to_string() })?;
    for seq in 0..MINT_ATTEMPTS {
        let run_id = run_id_at(started_at, fingerprint, seq);
        let path = runs_root.join(&run_id);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(RunDir { run_id, path }),
            // The whole collision rule, in one arm: somebody else owns that id, so try the next.
            // Reached by a second run in the same second of the same process, by a second PROCESS
            // that shares this one's pid — which is the case a clock-derived discriminator would
            // have called impossible — and now by a second run over the SAME INPUTS in one second,
            // which is two runs and must be two directories.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(RunPersistError::Dir { path, why: e.to_string() }),
        }
    }
    Err(RunPersistError::IdExhausted { root: runs_root.to_path_buf(), attempts: MINT_ATTEMPTS })
}

/// Write the documents of a run directory, the manifest LAST — this module's doc carries the
/// argument for that order, and [`write_run_with`] is where it is enforced.
///
/// Kept as the two-document door because that is what a producer with nothing else to write wants,
/// and because its two callers should not have to say "no extras" to mean it.
///
/// `report` is generic rather than a named type because the report is the KIND-SPECIFIC half: this
/// function's contract is the file names and the ordering, never the schema of what a particular
/// producer computed.
pub fn write_run<R>(dir: &Path, manifest: &RunManifest, report: &R) -> Result<(), RunPersistError>
where
    R: Serialize + ?Sized,
{
    write_run_with(dir, manifest, report, &RunExtras::default())
}

/// [`write_run`] plus the optional documents in [`RunExtras`].
///
/// ORDER: config, series, trades, report, manifest — every irreplaceable document before the
/// COMPLETION MARKER, so a disk that fills part-way through costs the metadata rather than the
/// result, and a listing can still tell a half-written run from a broken one with no lock file.
pub fn write_run_with<R>(
    dir: &Path,
    manifest: &RunManifest,
    report: &R,
    extras: &RunExtras<'_>,
) -> Result<(), RunPersistError>
where
    R: Serialize + ?Sized,
{
    if let Some(toml) = extras.config_toml {
        write_text(&dir.join(CONFIG_FILE), toml)?;
    }
    if let Some(series) = extras.series {
        write_json(&dir.join(SERIES_FILE), SERIES_FILE, series)?;
    }
    if let Some(trades) = extras.trades {
        write_json(&dir.join(TRADES_FILE), TRADES_FILE, trades)?;
    }
    write_json(&dir.join(REPORT_FILE), REPORT_FILE, report)?;
    write_json(&dir.join(MANIFEST_FILE), MANIFEST_FILE, manifest)
}

/// The config is TEXT, not JSON — it is the operator's own file, byte for byte.
fn write_text(path: &Path, value: &str) -> Result<(), RunPersistError> {
    std::fs::write(path, value)
        .map_err(|e| RunPersistError::Write { path: path.to_path_buf(), why: e.to_string() })
}

/// Pretty JSON plus a trailing newline, written to `path`. `file` names the document in a
/// [`RunPersistError::Serialize`]. `pub` because a run directory's sidecar documents written by a
/// crate above this one (`vike_backtest::trial_ledger`'s search header) take the same shape, and
/// a private twin of this body is what that crate used to carry. ⚠ Not for a COMPACT one-line
/// JSON file such as an append-only ledger.
pub fn write_json<T>(path: &Path, file: &'static str, value: &T) -> Result<(), RunPersistError>
where
    T: Serialize + ?Sized,
{
    let mut json = serde_json::to_string_pretty(value)
        .map_err(|e| RunPersistError::Serialize { file, why: e.to_string() })?;
    json.push('\n');
    std::fs::write(path, json)
        .map_err(|e| RunPersistError::Write { path: path.to_path_buf(), why: e.to_string() })
}
