//! Shared blocking HTTP GET helpers for the collectors (finding F21). Each vendor module had
//! rebuilt the same ureq skeleton — build agent, optional auth/UA header, manual `(200..300)`
//! status check, `CollectError::Fetch` wrapping — in three subtly different shapes, plus a drift
//! bug: eod used a plain `ureq::agent()` (which errors on 4xx/5xx inside `call()`), so its manual
//! status check + body-head branch was dead code. This module gives all four a single
//! `http_status_as_error(false)` agent and three body sinks: [`get_to_string`], [`get_to_bytes`]
//! (404 → `Ok(None)`), and [`get_to_file`] (streamed, 404 → `Ok(None)` when asked).
//!
//! Auth genuinely differs per vendor (Basic / optional-Bearer / none), so the `Authorization`
//! header stays a parameter rather than being abstracted away. No global timeout is set on purpose
//! — the archive downloads run 130-400 MB and `vike_bridge_core::http::blocking_agent`'s 30s
//! `timeout_global` would abort them mid-stream, so that shared agent is deliberately NOT reused.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::CollectError;

/// Per-request options common to the collector GETs.
#[derive(Clone, Copy, Default)]
pub struct GetOptions<'a> {
    /// Full `Authorization` header value, e.g. `"Basic <b64>"` or `"Bearer <key>"`. `None` = no auth.
    pub authorization: Option<&'a str>,
    /// `User-Agent` header value. Some sources 403 without one (Yahoo); pmxt sends an identifying UA.
    pub user_agent: Option<&'a str>,
    /// Extra request headers, applied verbatim after the two named above.
    ///
    /// The escape hatch for a vendor whose auth is neither Basic nor Bearer, and it exists for
    /// exactly one: `data.vike.io` authenticates with an `X-API-KEY` header, which has no
    /// `Authorization` spelling at all (`crates/vike-backfill/src/vikedata/client.rs`'s
    /// `API_KEY_HEADER`). Empty for every other caller — the module doc's point stands, that auth
    /// genuinely differs per vendor and stays a parameter rather than being abstracted away.
    ///
    /// ⚠ A header VALUE here can be a credential, so nothing in this module logs `opts`: the error
    /// messages below carry the URL and the status, never the request headers.
    pub headers: &'a [(&'a str, &'a str)],
}

/// Build the request, apply the optional headers, and send it. `ctx` is a short vendor prefix used
/// in error messages (e.g. `"tardis"`).
fn send(
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<ureq::http::Response<ureq::Body>, CollectError> {
    // Read statuses ourselves (a 404 is often expected, not a transport error). NO global timeout —
    // large archive streams must not be capped (see module doc).
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let mut req = agent.get(url);
    if let Some(auth) = opts.authorization {
        req = req.header("Authorization", auth);
    }
    if let Some(ua) = opts.user_agent {
        req = req.header("User-Agent", ua);
    }
    for (name, value) in opts.headers {
        req = req.header(*name, *value);
    }
    req.call().map_err(|e| CollectError::Fetch(format!("{ctx} GET {url}: {e}")))
}

/// First 200 chars of a body — the error-message head shared by the status-failure branches.
fn head(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Blocking GET → the response body as a `String`. Non-2xx → `CollectError::Fetch` with the body
/// head (the body is read first so it can be surfaced in the error).
pub fn get_to_string(url: &str, opts: &GetOptions, ctx: &str) -> Result<String, CollectError> {
    let mut resp = send(url, opts, ctx)?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| CollectError::Fetch(format!("read {ctx} {url}: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(CollectError::Fetch(format!(
            "{ctx} HTTP {status} from {url}: {}",
            head(&body)
        )));
    }
    Ok(body)
}

/// Blocking GET → the response body as raw bytes, or `Ok(None)` on HTTP 404 (the "not published"
/// skip). Other non-2xx → `Err`. For callers that post-process the bytes (e.g. gzip-decompress).
pub fn get_to_bytes(
    url: &str,
    opts: &GetOptions,
    ctx: &str,
) -> Result<Option<Vec<u8>>, CollectError> {
    let mut resp = send(url, opts, ctx)?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(CollectError::Fetch(format!("{ctx} HTTP {status} from {url}")));
    }
    let mut buf = Vec::new();
    resp.body_mut()
        .as_reader()
        .read_to_end(&mut buf)
        .map_err(|e| CollectError::Fetch(format!("read {ctx} body {url}: {e}")))?;
    Ok(Some(buf))
}

/// Blocking GET streamed straight to a fresh file at `dest` (never buffered — archive parts run
/// 130-400 MB). `Ok(Some(dest))` on success; `Ok(None)` on HTTP 404 when `treat_404_as_none`
/// (otherwise a 404, like any non-2xx, is an `Err` carrying the body head). Creates `dest`'s
/// parent directory.
pub fn get_to_file(
    url: &str,
    dest: &Path,
    opts: &GetOptions,
    ctx: &str,
    treat_404_as_none: bool,
) -> Result<Option<PathBuf>, CollectError> {
    let mut resp = send(url, opts, ctx)?;
    let status = resp.status().as_u16();
    if treat_404_as_none && status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(CollectError::Fetch(format!(
            "{ctx} HTTP {status} from {url}: {}",
            head(&body)
        )));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CollectError::Fetch(format!("create {}: {e}", parent.display())))?;
    }
    let mut out = File::create(dest)
        .map_err(|e| CollectError::Fetch(format!("create {}: {e}", dest.display())))?;
    let mut reader = resp.body_mut().as_reader();
    std::io::copy(&mut reader, &mut out).map_err(|e| {
        CollectError::Fetch(format!("stream {ctx} body to {}: {e}", dest.display()))
    })?;
    out.flush().map_err(|e| CollectError::Fetch(format!("flush {}: {e}", dest.display())))?;
    Ok(Some(dest.to_path_buf()))
}
