//! **The versioned strategy SDK's source, as this daemon serves it to the Studio** (decision 0296,
//! verdict 3: "Studio gets two read verbs that serve an SDK's source"): `ListSdks` ([`listing`]) and
//! `ReadSdkFile` ([`read`]), negotiated by `vike_backtesthub_client::proto::FEATURE_SDK_SOURCE`.
//!
//! **Which SDKs there are is not this crate's to decide.** The SDK vocabulary (where the snapshots are
//! installed, their marker, "SDK dev" and the ONE rule saying whether an SDK is usable here) is
//! `crates/vike-rust-strategy/src/sdk.rs`'s, and no hub may link that crate
//! (`crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs`'s `STRATEGY_FILE_LOADER_FENCE`).
//! So the composition root, which does link it, hands this process a reader ([`use_reader`], from
//! `crates/vike-backend/src/main.rs`'s `backtesthub_main`), as it hands the builder's client
//! (`crate::strategy_sandbox::use_builder`). A daemon started outside `vike-backend` reads no SDK and
//! answers both verbs with a refusal in words. The reader is asked at every request: a deploy that
//! installs or retires an SDK is seen at once.
//!
//! ⚠ **A path a client sends is a TRUST BOUNDARY** (a Read-scope client, the Named backend's included,
//! may send any text). [`read_under`] serves only a file or folder under the SDK's `crates/`
//! (`vike_backtesthub_client::sdk_source::SDK_CRATES_FOLDER`), and on a dev checkout that matters: its
//! root also holds `settings/` (the credential store), `target/` and `vendor/`.
//! - The SDK is one the reader LISTS, matched by name; its folder is the reader's, never built from the
//!   client's text.
//! - The path is relative, `/`-separated plain names: a NUL, a `\`, a `:`, a leading `/`, and an empty,
//!   `.` or `..` component are refused before the filesystem is touched.
//! - Every component is looked at with `symlink_metadata`, so a link anywhere on the way (pointing in or
//!   out of the SDK) is refused, never followed; a folder's entries list no link either.
//! - The path is then canonicalized and must stay under the canonical `crates/` (what a platform
//!   normalizes behind the first checks, such as a trailing dot or space on Windows, still cannot leave).
//! - A file above `vike_backtesthub_client::sdk_source::SDK_FILE_MAX_BYTES`, or one that is not UTF-8, is
//!   refused in words; the read itself stops one byte past the cap.
//!
//! An installed SDK is immutable and read-only (decision 0296, verdict 1), so the walk does not guard a
//! link swapped in between its check and the open: that needs write access to the server's snapshot.

use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vike_backtesthub_client::sdk_source::{
    SDK_CRATES_FOLDER, SDK_FILE_MAX_BYTES, WireSdk, WireSdkEntry, WireSdkFile, WireSdkListing,
};

/// One SDK of this server as the reader found it: its listing row and its root folder (`sdk-N/`, or
/// the checkout for `sdk-dev`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkOnServer {
    /// What `ListSdks` lists for it.
    pub sdk: WireSdk,
    /// Its root folder, the one [`read`] serves `crates/` of.
    pub dir: PathBuf,
}

/// What the reader answers for a project: where its SDKs are, and each one, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSdks {
    /// The folder the SDKs were read from (`<project>/vendor/vike-strategy-source`, or the checkout).
    pub root: PathBuf,
    /// Every SDK, newest first.
    pub sdks: Vec<SdkOnServer>,
}

/// How this process reads the SDKs under a project root: the composition root's function over the SDK
/// vocabulary, `Err` in words for a folder that cannot be read.
pub type SdkReader = Box<dyn Fn(&Path) -> Result<ServerSdks, String> + Send + Sync>;

/// The reader this process asks, set once by the composition root ([`use_reader`]).
static SDK_READER: OnceLock<SdkReader> = OnceLock::new();

/// Read this process's SDKs with `reader` (the first call wins).
pub fn use_reader(reader: SdkReader) {
    let _ = SDK_READER.set(reader);
}

/// The refusal of a daemon that was handed no reader.
const NO_READER: &str = "this backtest server reads no strategy SDK: it was started outside \
     `vike-backend`, which hands it the SDK reader. Run `vike-backend backtesthub --addr`.";

/// `ListSdks`: the SDKs under `project` (`StudyDirs::project_root`), newest first.
pub(crate) fn listing(project: Option<&Path>) -> Result<WireSdkListing, String> {
    listing_with(SDK_READER.get(), project)
}

/// `ReadSdkFile`: the file or folder `path` of the SDK named `sdk` under `project`.
pub(crate) fn read(project: Option<&Path>, sdk: &str, path: &str) -> Result<WireSdkFile, String> {
    read_with(SDK_READER.get(), project, sdk, path)
}

/// What `reader` finds under `project`, or why nothing was read.
fn server_sdks(reader: Option<&SdkReader>, project: Option<&Path>) -> Result<ServerSdks, String> {
    let reader = reader.ok_or_else(|| NO_READER.to_string())?;
    let project = project.ok_or_else(|| {
        "this backtest server found no project (no settings folder above its working directory), \
         so it holds no strategy SDK"
            .to_string()
    })?;
    reader(project)
}

/// [`listing`] with the reader a parameter.
fn listing_with(
    reader: Option<&SdkReader>,
    project: Option<&Path>,
) -> Result<WireSdkListing, String> {
    let found = server_sdks(reader, project)?;
    Ok(WireSdkListing {
        root: found.root.display().to_string(),
        sdks: found.sdks.into_iter().map(|s| s.sdk).collect(),
    })
}

/// [`read`] with the reader a parameter: the SDK is matched by its LISTED name, then [`read_under`].
fn read_with(
    reader: Option<&SdkReader>,
    project: Option<&Path>,
    sdk: &str,
    path: &str,
) -> Result<WireSdkFile, String> {
    let found = server_sdks(reader, project)?;
    let Some(listed) = found.sdks.iter().find(|s| s.sdk.name == sdk) else {
        let names: Vec<&str> = found.sdks.iter().map(|s| s.sdk.name.as_str()).collect();
        return Err(format!("this server holds no SDK {sdk:?} (it holds {names:?})"));
    };
    read_under(&listed.dir, sdk, path)
}

/// **The trust-boundary walk** (this module's doc): the file or folder `path` under the SDK root `root`,
/// served only inside its `crates/`.
pub(crate) fn read_under(root: &Path, sdk: &str, path: &str) -> Result<WireSdkFile, String> {
    let refuse = |why: &str| -> Result<WireSdkFile, String> {
        Err(format!("{sdk} {path:?}: {why}; nothing was read"))
    };
    if path.contains('\0') {
        return refuse("a path holds no NUL byte");
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return refuse("a path is relative to the SDK and `/`-separated");
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|part| matches!(*part, "" | "." | "..")) {
        return refuse("no part of a path may be empty, `.` or `..`");
    }
    if parts[0] != SDK_CRATES_FOLDER {
        return refuse("only the SDK's `crates/` folder is served");
    }
    let unreadable = |e: std::io::Error| format!("{sdk} {path:?}: {e}");
    let mut at = root.to_path_buf();
    let mut meta = None;
    for part in &parts {
        at.push(part);
        let here = std::fs::symlink_metadata(&at).map_err(unreadable)?;
        if here.file_type().is_symlink() {
            return refuse("it passes through a link, and a link is never followed");
        }
        meta = Some(here);
    }
    let Some(meta) = meta else { return refuse("an empty path") };
    let crates = root.join(SDK_CRATES_FOLDER).canonicalize().map_err(unreadable)?;
    if !at.canonicalize().map_err(unreadable)?.starts_with(&crates) {
        return refuse("it leaves the SDK's `crates/` folder");
    }
    let answer = |folder: bool, text: String, entries: Vec<WireSdkEntry>| WireSdkFile {
        sdk: sdk.to_string(),
        path: path.to_string(),
        folder,
        text,
        entries,
    };
    if meta.is_dir() {
        return Ok(answer(true, String::new(), folder_entries(&at).map_err(unreadable)?));
    }
    if !meta.is_file() {
        return refuse("not a regular file or a folder");
    }
    let too_big = || format!("larger than the {SDK_FILE_MAX_BYTES} bytes one read answers");
    if meta.len() > SDK_FILE_MAX_BYTES {
        return refuse(&too_big());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&at)
        .and_then(|file| file.take(SDK_FILE_MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(unreadable)?;
    if bytes.len() as u64 > SDK_FILE_MAX_BYTES {
        return refuse(&too_big());
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(answer(false, text, Vec::new())),
        Err(_) => refuse("not UTF-8 text (the SDK view shows source)"),
    }
}

/// A folder's entries, folders first, each group in name order: regular files and folders only (a link
/// and anything else is left out), and only a name a path can spell (UTF-8).
fn folder_entries(dir: &Path) -> std::io::Result<Vec<WireSdkEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if kind.is_dir() || kind.is_file() {
            entries.push(WireSdkEntry { name, folder: kind.is_dir() });
        }
    }
    entries.sort_by(|a, b| match (a.folder, b.folder) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    Ok(entries)
}

#[path = "sdk_source_tests.rs"]
#[cfg(test)]
mod sdk_source_tests;
