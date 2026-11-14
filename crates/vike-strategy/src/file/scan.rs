//! The scan of `<project>/strategies/` (`vike_paths::state_path::PROJECT_STRATEGIES_DIR`): every
//! `.rs` and `.rhai` file below it, subfolders included, each with its path relative to the folder
//! (its identity, `/`-separated), its sha256, its parsed header or the reason it has none, and for
//! a `.rs` file its build state. It runs on every list request (no watcher).
//!
//! A broken file is listed with its reason and never hides or blocks another; a folder that cannot
//! be read is reported in [`StrategyFileScan::unreadable`] and the rest is still listed. A missing
//! root is an empty list. A symbolic link is listed, unread, with its reason, and a linked folder
//! is not entered (a link cannot loop the walk or reach outside it). The extension is compared
//! case-insensitively, as every reader of `vike_paths::state_path::RUST_EXT` / `RHAI_EXT` does.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use vike_paths::state_path::{RHAI_EXT, RUST_EXT};

use super::StrategyFileHeader;

/// The lowercase hex sha256 of `bytes`: 64 characters, `[0-9a-f]`, no prefix, the spelling
/// `vike_hub_protocol::hash::sha256_hex` writes (that crate is not named here: its tree carries an RNG,
/// which this determinism-critical crate may not).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    hex::encode(digest)
}

/// The extension of a built artifact in a build folder: `<build folder>/<sha256>.so`.
pub const BUILT_EXTENSION: &str = "so";

/// The extension of a failed build's compiler message in a build folder: `<build folder>/<sha256>.err`.
pub const BUILD_FAILED_EXTENSION: &str = "err";

/// The two languages a strategy file is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyFileLanguage {
    /// `.rs`: built by the builder, run in a sandboxed process.
    Rust,
    /// `.rhai`: run in the host process.
    Rhai,
}

/// Where a `.rs` file stands with the builder on this box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyFileBuildState {
    /// No artifact and no failure for this sha256.
    NotBuilt,
    /// `<sha256>.so` is in the build folder.
    Built {
        /// The file's sha256 (shown by its first 8 characters).
        sha256: String,
    },
    /// `<sha256>.err` is in the build folder, holding the compiler's message.
    BuildFailed {
        /// The compiler's message.
        message: String,
    },
    /// This box cannot run a Rust file (no sandbox here).
    CannotRunHere {
        /// Why.
        why: String,
    },
}

impl fmt::Display for StrategyFileBuildState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBuilt => f.write_str("not built"),
            Self::Built { sha256 } => write!(f, "built {}", sha256.get(..8).unwrap_or(sha256)),
            Self::BuildFailed { message } => write!(f, "build failed: {message}"),
            Self::CannotRunHere { why } => write!(f, "cannot run here: {why}"),
        }
    }
}

/// One file of the scan.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyFileEntry {
    /// The path relative to the strategies folder, `/`-separated: the file's identity.
    pub path: String,
    /// From the extension.
    pub language: StrategyFileLanguage,
    /// The lowercase hex sha256 of the file's bytes; `None` when it could not be read.
    pub sha256: Option<String>,
    /// The parsed header, or the one-line reason the file is listed broken.
    pub header: Result<StrategyFileHeader, String>,
    /// A readable `.rs` file's build state; `None` for a `.rhai` file (the hosts that link the Rhai
    /// engine parse-check it) and for a file that could not be read.
    pub build: Option<StrategyFileBuildState>,
}

/// The whole scan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StrategyFileScan {
    /// Every `.rs` and `.rhai` file, sorted by path.
    pub files: Vec<StrategyFileEntry>,
    /// Each folder (or folder entry) that could not be read, as `<path>: <error>`.
    pub unreadable: Vec<String>,
}

/// Scan `root` (`<project>/strategies`).
///
/// `build_folder` is the builder's folder for this release (`<project>/user_data/strategy-builds/
/// <fingerprint8>`), `None` when it is unknown: every `.rs` file is then `not built`.
/// `cannot_run_here` is the reason this box cannot run a Rust file, and wins over every other
/// state.
pub fn scan(
    root: &Path,
    build_folder: Option<&Path>,
    cannot_run_here: Option<&str>,
) -> StrategyFileScan {
    let mut out = StrategyFileScan::default();
    walk(root, root, build_folder, cannot_run_here, &mut out);
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The build state of a `.rs` file whose bytes hash to `sha256`, under the same two inputs as
/// [`scan`].
pub fn build_state(
    sha256: &str,
    build_folder: Option<&Path>,
    cannot_run_here: Option<&str>,
) -> StrategyFileBuildState {
    if let Some(why) = cannot_run_here {
        return StrategyFileBuildState::CannotRunHere { why: why.to_string() };
    }
    let Some(folder) = build_folder else {
        return StrategyFileBuildState::NotBuilt;
    };
    if folder.join(format!("{sha256}.{BUILT_EXTENSION}")).is_file() {
        return StrategyFileBuildState::Built { sha256: sha256.to_string() };
    }
    match fs::read_to_string(folder.join(format!("{sha256}.{BUILD_FAILED_EXTENSION}"))) {
        Ok(message) => {
            StrategyFileBuildState::BuildFailed { message: message.trim_end().to_string() }
        }
        Err(_) => StrategyFileBuildState::NotBuilt,
    }
}

/// The file a profile's `[strategy] file = "<relative>"` names, under `root`. Refused when the
/// path is empty, absolute, climbs out (`..`), or is not a `.rs` / `.rhai` file. Whether the file
/// exists is the reader's question.
pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    let inside = !relative.is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(format!(
            "strategy file {relative:?} is not a path inside the strategies folder: name it \
             relative to that folder, without `..` (for example \"trend/momentum.rs\")"
        ));
    }
    if language_of(path).is_none() {
        return Err(format!("strategy file {relative:?} is neither a .rs nor a .rhai file"));
    }
    Ok(root.join(path))
}

/// The language of `path` by its extension, compared case-insensitively.
pub fn language_of(path: &Path) -> Option<StrategyFileLanguage> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case(RUST_EXT) {
        Some(StrategyFileLanguage::Rust)
    } else if extension.eq_ignore_ascii_case(RHAI_EXT) {
        Some(StrategyFileLanguage::Rhai)
    } else {
        None
    }
}

fn walk(
    root: &Path,
    folder: &Path,
    build_folder: Option<&Path>,
    cannot_run_here: Option<&str>,
    out: &mut StrategyFileScan,
) {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if folder == root && e.kind() == ErrorKind::NotFound => return,
        Err(e) => {
            out.unreadable.push(format!("{}: {e}", folder.display()));
            return;
        }
    };
    for entry in entries {
        let (entry, file_type) = match entry.and_then(|e| e.file_type().map(|t| (e, t))) {
            Ok(pair) => pair,
            Err(e) => {
                out.unreadable.push(format!("{}: {e}", folder.display()));
                continue;
            }
        };
        let path = entry.path();
        if file_type.is_dir() {
            walk(root, &path, build_folder, cannot_run_here, out);
            continue;
        }
        let Some(language) = language_of(&path) else {
            continue;
        };
        let relative = relative_path(root, &path);
        if !file_type.is_file() {
            out.files.push(StrategyFileEntry {
                path: relative,
                language,
                sha256: None,
                header: Err("a symbolic link or special file is not read: copy the file itself \
                             into the folder"
                    .to_string()),
                build: None,
            });
            continue;
        }
        out.files.push(match fs::read(&path) {
            Ok(bytes) => {
                let sha256 = sha256_hex(&bytes);
                let header = std::str::from_utf8(&bytes)
                    .map_err(|_| "the file is not UTF-8 text".to_string())
                    .and_then(StrategyFileHeader::parse);
                let build = (language == StrategyFileLanguage::Rust)
                    .then(|| build_state(&sha256, build_folder, cannot_run_here));
                StrategyFileEntry { path: relative, language, sha256: Some(sha256), header, build }
            }
            Err(e) => StrategyFileEntry {
                path: relative,
                language,
                sha256: None,
                header: Err(format!("cannot read the file: {e}")),
                build: None,
            },
        });
    }
}

/// `path` relative to `root`, its components joined by `/` on every platform.
fn relative_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}
