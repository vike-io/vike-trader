//! The READING half: the read error and one reader per document of a run directory.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::{
    CONFIG_FILE, DAY_STAMPS_FILE, EQUITY_CURVE_FILE, EQUITY_LOG_FILE, FORECASTS_FILE,
    KEPT_ANSWER_FILE, MANIFEST_FILE, MODEL_ARTIFACT, MODEL_ONNX_ARTIFACT, RunEquityCurve,
    RunForecasts, RunManifest, RunSeries, RunTrades, SERIES_FILE, TRADES_FILE,
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
/// (`crates/vike-runs/src/scan.rs`'s `scan_runs`).
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
pub(crate) fn read_doc<T: serde::de::DeserializeOwned>(
    dir: &Path,
    file: &str,
) -> Result<T, RunReadError> {
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

/// Read one run directory's [`EQUITY_CURVE_FILE`] ([`RunEquityCurve::from_bytes`]). `Missing` is ORDINARY: only a
/// run the backtesthub answered as a summary keeps one.
pub fn read_equity_curve(dir: &Path) -> Result<RunEquityCurve, RunReadError> {
    let path = dir.join(EQUITY_CURVE_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => {
            RunEquityCurve::from_bytes(&bytes).map_err(|why| RunReadError::Parse { path, why })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(RunReadError::Missing { path }),
        Err(e) => Err(RunReadError::Read { path, why: e.to_string() }),
    }
}

/// Read one run directory's [`EQUITY_LOG_FILE`], its bytes unread (`vike_simulator::decode_ledger_events` reads them).
/// `Missing` is ORDINARY: only a summarized single bar run saved since the log existed keeps one.
pub fn read_equity_log(dir: &Path) -> Result<Vec<u8>, RunReadError> {
    read_run_bytes(dir, EQUITY_LOG_FILE)
}

/// Read one run directory's [`DAY_STAMPS_FILE`], its bytes unread (the backtesthub's saver wrote them, and its curve
/// rebuild reads them). `Missing` is ORDINARY: only a run saved with an event log since the file existed keeps one.
pub fn read_day_stamps(dir: &Path) -> Result<Vec<u8>, RunReadError> {
    read_run_bytes(dir, DAY_STAMPS_FILE)
}

/// Read one run directory's [`KEPT_ANSWER_FILE`], its bytes unread (the backtesthub wrote them, and reads them back to
/// answer an identical request). `Missing` is ORDINARY: only a run that may answer again keeps one.
pub fn read_kept_answer(dir: &Path) -> Result<Vec<u8>, RunReadError> {
    read_run_bytes(dir, KEPT_ANSWER_FILE)
}

/// The bytes of one file in a run directory, with [`read_run_text`]'s three-way failure distinction.
fn read_run_bytes(dir: &Path, file: &str) -> Result<Vec<u8>, RunReadError> {
    let path = dir.join(file);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(RunReadError::Missing { path }),
        Err(e) => Err(RunReadError::Read { path, why: e.to_string() }),
    }
}

/// Read one run directory's [`FORECASTS_FILE`]. `Missing` is ORDINARY, and more so than for the trades: a run
/// whose strategy published no forecast writes none.
pub fn read_forecasts(dir: &Path) -> Result<RunForecasts, RunReadError> {
    read_doc(dir, FORECASTS_FILE)
}

/// Read one run directory's [`CONFIG_FILE`] — the resolved config as TEXT, unparsed. Parsing it is
/// `crates/vike-backtest/src/run/profile.rs`'s `ProfileBacktest::from_toml_str`'s job and this
/// module does not depend on that tree. Same `Missing`-is-ordinary rule.
pub fn read_config_toml(dir: &Path) -> Result<String, RunReadError> {
    read_run_text(dir, CONFIG_FILE)
}

/// The text of run `run_id`'s [`MODEL_ARTIFACT`] under `runs_root`: the model a study fitted, which a host
/// hands a strategy file for its `type = "model"` key (`vike_strategy::file`'s format note; the file reads
/// no file). A run whose model is a network, [`MODEL_ONNX_ARTIFACT`], answers the base64 (STANDARD engine)
/// of its bytes instead, because the key crosses as text; a run holding both files is refused, never
/// resolved by a preference.
///
/// `run_id` comes from a profile or a mount row, so it is held to ONE directory name of
/// [`super::run_id_at`]'s alphabet (ASCII letters, digits and `-`): a value can name no path outside
/// `runs_root`. Only a FINISHED run answers: its [`MANIFEST_FILE`] is read first, because a run with none is
/// still being written or stopped before it finished, and its model may be cut short.
pub fn read_model_text(runs_root: &Path, run_id: &str) -> Result<String, RunReadError> {
    let dir = finished_run_dir(runs_root, run_id)?;
    let onnx = dir.join(MODEL_ONNX_ARTIFACT);
    match std::fs::read(&onnx) {
        Ok(_) if dir.join(MODEL_ARTIFACT).exists() => Err(RunReadError::Read {
            path: dir,
            why: format!(
                "the run holds both {MODEL_ARTIFACT} and {MODEL_ONNX_ARTIFACT}: one run, one model"
            ),
        }),
        Ok(bytes) => Ok(STANDARD.encode(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => read_run_text(&dir, MODEL_ARTIFACT),
        Err(e) => Err(RunReadError::Read { path: onnx, why: e.to_string() }),
    }
}

/// Whether `run_id` is ONE directory name of [`super::run_id_at`]'s alphabet (ASCII letters, digits and `-`), so it
/// can name no path outside the runs root: [`finished_run_dir`]'s rule and the removers' (`super::retention`).
pub(crate) fn is_one_run_id(run_id: &str) -> bool {
    !run_id.is_empty() && run_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The directory of the FINISHED run `run_id` under `runs_root`, for a reader handed a run id rather than a
/// path ([`read_model_text`], a study's read of a run's forecasts): the id is held to ONE directory name of
/// [`super::run_id_at`]'s alphabet (ASCII letters, digits and `-`), so a value can name no path outside
/// `runs_root`, and its [`MANIFEST_FILE`] must read, because a run with none is still being written or stopped
/// before it finished, and what it holds may be cut short.
pub fn finished_run_dir(runs_root: &Path, run_id: &str) -> Result<PathBuf, RunReadError> {
    if !is_one_run_id(run_id) {
        return Err(RunReadError::Read {
            path: runs_root.to_path_buf(),
            why: format!("{run_id:?} is not a run id (ASCII letters, digits and `-` only)"),
        });
    }
    let dir = runs_root.join(run_id);
    if !dir.is_dir() {
        return Err(RunReadError::Missing { path: dir });
    }
    read_manifest(&dir)?;
    Ok(dir)
}
