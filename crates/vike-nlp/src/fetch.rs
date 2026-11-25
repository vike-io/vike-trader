//! Download a pinned model into the models folder, and read its files back VERIFIED.
//!
//! [`fetch`] is the only writer of `<models_dir>/<name>/<revision>/`: for each pinned file it keeps a
//! file already there whose size and sha256 match the pin, and otherwise downloads it at the pinned
//! commit, checks the bytes against the pin BEFORE writing, and replaces the file atomically
//! (`vike_utility::atomic_file::write_atomic`), so the folder never holds a partial or unverified
//! file under a pinned name. A mismatch writes nothing and names both digests.
//!
//! [`read_verified`] is what a loader reads through: the bytes of one pinned file, refused unless
//! they match the pin. Weights are read whole into memory (the crate forbids `unsafe`, so no mapping);
//! the loader needs them in memory anyway.
//!
//! The download is a plain `GET` of the hub's `resolve` address with the workspace's honest
//! `User-Agent` and no credential: the pinned repositories are public. The body is read up to one
//! byte past the pinned size, so a wrong file never costs more memory than the right one. It has no
//! ceiling, only an idle bound: a read that waits `vike_transport::http::DEFAULT_TIMEOUT` for one byte fails.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::NlpError;
use crate::pins::{PinnedFile, PinnedModel};

/// How long a connection may take to open. The body has no ceiling, only an idle bound
/// ([`download_agent`]): a 440 MB file on a slow line is a slow download, not a failure.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The download agent: the connect bound, a PER-READ idle bound (never a total: a 440 MB file on a slow line is
/// slow, a line silent for `idle` is dead), a 4xx/5xx as an error, the network layer's stated public-GET redirect
/// limit and no proxy (`vike_transport::http::public_get_agent_bounded`; the download is keyless).
fn download_agent(idle: Duration) -> ureq::Agent {
    use vike_transport::http::{CallBound, StatusPolicy, public_get_agent_bounded};
    public_get_agent_bounded(Some(CONNECT_TIMEOUT), CallBound::Idle(idle), StatusPolicy::Error)
}

/// Make `model`'s folder under `models_dir` hold every pinned file, verified; returns the folder.
/// Files already present and matching are not downloaded again, so a second run is the cheap way
/// to re-verify a folder.
pub fn fetch(model: &PinnedModel, models_dir: &Path) -> Result<PathBuf, NlpError> {
    let folder = model.folder_in(models_dir);
    std::fs::create_dir_all(&folder)
        .map_err(|source| NlpError::Io { path: folder.clone(), source })?;
    // The shared idle bound: thirty seconds without one byte is a dead line.
    let agent = download_agent(vike_transport::http::DEFAULT_TIMEOUT);
    for file in model.files {
        let path = folder.join(file.path);
        if read_checked(&path, file, model.name).is_ok() {
            continue;
        }
        let url = model.file_url(file);
        let bytes = download(&agent, &url, file.size)?;
        check_bytes(&url, &bytes, file)?;
        vike_utility::atomic_file::write_atomic(&path, &bytes)
            .map_err(|source| NlpError::Io { path: path.clone(), source })?;
    }
    Ok(folder)
}

/// The bytes of `model`'s pinned file named `name` under `models_dir`, refused unless its size and
/// sha256 match the pin ([`NlpError::NotFetched`] when absent, [`NlpError::Digest`] when different).
pub fn read_verified(
    model: &PinnedModel,
    models_dir: &Path,
    name: &str,
) -> Result<Vec<u8>, NlpError> {
    let file = model
        .files
        .iter()
        .find(|f| f.path == name)
        .ok_or_else(|| NlpError::Model(format!("{} pins no file named {name}", model.name)))?;
    read_checked(&model.folder_in(models_dir).join(file.path), file, model.name)
}

/// Read `path` and check it against `file`'s pin.
fn read_checked(path: &Path, file: &PinnedFile, model: &'static str) -> Result<Vec<u8>, NlpError> {
    let bytes = std::fs::read(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => NlpError::NotFetched { path: path.to_path_buf(), model },
        _ => NlpError::Io { path: path.to_path_buf(), source },
    })?;
    check_bytes(&path.display().to_string(), &bytes, file)?;
    Ok(bytes)
}

/// Refuse `bytes` unless they are `file`'s pinned size and sha256. `what` names them in the error.
pub(crate) fn check_bytes(what: &str, bytes: &[u8], file: &PinnedFile) -> Result<(), NlpError> {
    let size = bytes.len() as u64;
    if size != file.size {
        return Err(NlpError::Digest {
            what: what.to_string(),
            expected: format!("size {}", file.size),
            actual: format!("size {size}"),
        });
    }
    let digest = vike_hub_protocol::hash::sha256_hex(bytes);
    if digest != file.sha256 {
        return Err(NlpError::Digest {
            what: what.to_string(),
            expected: format!("sha256 {}", file.sha256),
            actual: format!("sha256 {digest}"),
        });
    }
    Ok(())
}

/// `GET url`, the body read up to one byte past `size` (the check then refuses a longer one).
fn download(agent: &ureq::Agent, url: &str, size: u64) -> Result<Vec<u8>, NlpError> {
    let mut response =
        agent.get(url).call().map_err(|e| NlpError::Fetch(format!("GET {url}: {e}")))?;
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    response
        .body_mut()
        .as_reader()
        .take(size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| NlpError::Fetch(format!("read {url}: {e}")))?;
    Ok(bytes)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
