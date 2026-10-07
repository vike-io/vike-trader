//! The READING half: the read error and one reader per document of a run directory.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    CONFIG_FILE, MANIFEST_FILE, RunManifest, RunSeries, RunTrades, SERIES_FILE, TRADES_FILE,
};

#[cfg(doc)]
use super::{RunPersistError, write_run};

/// Why a run directory's manifest could not be READ back. The counterpart of [`RunPersistError`],
/// and it lives here rather than in whatever crate happens to list runs first: the schema is
/// [`RunManifest`]'s, so the code that turns bytes into one belongs beside the code that turns one
/// into bytes. A reader written up the dependency graph would be a second definition of a common
/// manifest, which this module's doc names as the one outcome that shape cannot survive.
///
/// ⚠ [`RunReadError::Missing`] is deliberately NOT folded into [`RunReadError::Read`], even though
/// both are "no manifest came back". The manifest is written LAST (see this module's doc), so its
/// absence means the run is being written RIGHT NOW or a process died between the two writes —
/// while an unreadable one means a file that exists and cannot be opened. A listing renders those
/// as different rows because they have different fixes: wait, versus go and look at the file.
#[derive(Debug)]
pub enum RunReadError {
    /// The directory holds no such document.
    ///
    /// For [`MANIFEST_FILE`] that is an unfinished run rather than a broken one. For
    /// [`SERIES_FILE`], [`TRADES_FILE`] and [`CONFIG_FILE`] it is ORDINARY: every run written
    /// before those documents existed has none, and a producer that keeps no curve never will.
    Missing {
        /// Where the document was looked for.
        path: PathBuf,
    },
    /// The document is there and could not be read as text — permissions, or not UTF-8.
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// The text is not a [`RunManifest`]: not JSON at all, or JSON missing a COMMON field. Both are
    /// one variant because a listing acts on them identically — the document cannot produce a row,
    /// and `why` carries which of the two it was in the parser's own words.
    Parse {
        /// The file that could not be parsed.
        path: PathBuf,
        /// serde_json's own words.
        why: String,
    },
}

impl fmt::Display for RunReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // ⚠ The FILE comes off the path rather than being the manifest by assumption: this
            // error serves four documents since the run record grew past two, and a message
            // that said `no manifest.json` when a caller asked for `series.json` would send a
            // reader looking for the wrong absence. The half-written-run clause is
            // manifest-ONLY for the same reason: it is true of the completion marker and of
            // nothing else.
            Self::Missing { path } => {
                let file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| MANIFEST_FILE.to_string());
                if file == MANIFEST_FILE {
                    write!(
                        f,
                        "no {MANIFEST_FILE} at {} — the run is still being written, or it \
                         stopped between its report and its manifest",
                        path.display()
                    )
                } else {
                    write!(f, "no {file} at {}", path.display())
                }
            }
            Self::Read { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            Self::Parse { path, why } => write!(f, "cannot parse {}: {why}", path.display()),
        }
    }
}

impl std::error::Error for RunReadError {}

/// Read one run directory's [`MANIFEST_FILE`] — the READING half of this module, and the call any
/// listing of `<project>/user_data/runs/` is written on top of
/// (`crates/vike-studio-core/src/listing.rs`'s `list_runs`).
///
/// Takes the RUN DIRECTORY rather than the manifest path, symmetrically with [`write_run`], so the
/// file name stays this module's business: a caller that spelled `manifest.json` itself would be a
/// second place the name lives.
///
/// Every failure is a value naming the path — nothing here panics, and nothing is silently skipped.
/// A run that vanishes from a listing is worse than one that shows as broken: the first is
/// unanswerable, the second names its own fix.
pub fn read_manifest(dir: &Path) -> Result<RunManifest, RunReadError> {
    read_doc(dir, MANIFEST_FILE)
}

/// Read one JSON document out of a run directory. The three readers below differ only in the file
/// name and the type, so the error mapping — and the `Missing` / `Read` / `Parse` distinction this
/// module's [`RunReadError`] exists for — is written once.
pub fn read_doc<T: serde::de::DeserializeOwned>(dir: &Path, file: &str) -> Result<T, RunReadError> {
    let text = read_run_text(dir, file)?;
    let path = dir.join(file);
    serde_json::from_str(&text).map_err(|e| RunReadError::Parse { path, why: e.to_string() })
}

/// The raw text of one file in a run directory, with the same three-way failure distinction.
fn read_run_text(dir: &Path, file: &str) -> Result<String, RunReadError> {
    let path = dir.join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(RunReadError::Missing { path }),
        Err(e) => Err(RunReadError::Read { path, why: e.to_string() }),
    }
}

/// Read one run directory's [`SERIES_FILE`].
///
/// ⚠ [`RunReadError::Missing`] is an ORDINARY answer here, unlike for the manifest: every run
/// written before this document existed has none, and a producer that keeps no curve never will.
/// A caller renders "no series" rather than "broken run".
pub fn read_series(dir: &Path) -> Result<RunSeries, RunReadError> {
    read_doc(dir, SERIES_FILE)
}

/// Read one run directory's [`TRADES_FILE`]. Same `Missing`-is-ordinary rule as [`read_series`].
pub fn read_trades(dir: &Path) -> Result<RunTrades, RunReadError> {
    read_doc(dir, TRADES_FILE)
}

/// Read one run directory's [`CONFIG_FILE`] — the resolved config as TEXT, unparsed. Parsing it is
/// `crates/vike-backtest/src/harness/profile.rs`'s `BacktestProfile::from_toml_str`'s job and this
/// module does not depend on that tree. Same `Missing`-is-ordinary rule.
pub fn read_config_toml(dir: &Path) -> Result<String, RunReadError> {
    read_run_text(dir, CONFIG_FILE)
}
