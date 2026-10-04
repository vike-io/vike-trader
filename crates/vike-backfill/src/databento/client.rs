//! Databento historical HTTP transport: build the `timeseries.get_range` URL, authenticate with
//! HTTP Basic (API key as username, empty password), and stream the CSV response body to a temp
//! file (never buffered — tick/book ranges are large, same discipline as `pmxt::download_hour`).
//!
//! **Env boundary (settings STEP 2).** The API key arrives as a `&str` PARAMETER; this module
//! reads neither the process environment nor the workspace `.env`. It used to own an
//! `api_key_from_env()` (a `Layer::Library` row on the STEP-2 work-list) while its sibling
//! `tardis` adapter — same vendor shape, same crate — already had its `bin` do the read. The
//! `databento_backfill` bin now does the same, so the two premium-vendor adapters are symmetric.

use std::path::{Path, PathBuf};

use crate::error::CollectError;

pub const HIST_BASE: &str = "https://hist.databento.com";

/// A `timeseries.get_range` request. `encoding=csv` and `compression=none` are fixed; symbology is
/// `raw_symbol` in and out (the store keys on the raw vendor symbol).
pub struct GetRange<'a> {
    pub dataset: &'a str,
    pub symbols: &'a str,
    pub schema: &'a str,
    pub start: &'a str,
    pub end: &'a str,
}

/// Build the full GET URL (query only — auth is a header, not in the URL).
pub fn build_url(base: &str, req: &GetRange) -> String {
    let enc = |s: &str| url_encode(s);
    format!(
        "{base}/v0/timeseries.get_range?dataset={}&symbols={}&schema={}&start={}&end={}\
         &encoding=csv&compression=none&stype_in=raw_symbol&stype_out=raw_symbol",
        enc(req.dataset),
        enc(req.symbols),
        enc(req.schema),
        enc(req.start),
        enc(req.end),
    )
}

/// Minimal percent-encoding for the characters that appear in Databento args (`:` in timestamps,
/// spaces, `,` between symbols). Alphanumerics and `-._~` pass through.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Stream a `get_range` CSV response to a fresh file under `out_dir` (streamed, never buffered).
/// HTTP Basic: API key as username, empty password. Non-2xx (incl. 404) → `CollectError::Fetch`
/// with the body head. Returns the written file path. Uses the shared [`crate::http::get_to_file`]
/// (no global timeout — the ranges are large).
pub fn fetch_to_file(
    base: &str,
    api_key: &str,
    req: &GetRange,
    out_dir: &Path,
) -> Result<PathBuf, CollectError> {
    let url = build_url(base, req);
    // HTTP Basic "key:" (empty password) → base64.
    let token = base64_std(format!("{api_key}:").as_bytes());
    let auth = format!("Basic {token}");
    let opts = crate::http::GetOptions { authorization: Some(&auth), ..Default::default() };
    let dest = out_dir.join(format!("databento_{}_{}.csv", req.schema, sanitize(req.symbols)));
    // `treat_404_as_none = false` → a 404 (like any non-2xx) surfaces as an Err above, so `get_to_file`
    // never returns None on the Ok path; the `unwrap_or(dest)` just re-yields the written path.
    let written = crate::http::get_to_file(&url, &dest, &opts, "databento", false)?;
    Ok(written.unwrap_or(dest))
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

/// Standard-alphabet base64 (no padding omitted) for the Basic auth token. Hand-rolled to avoid a
/// new dep — the input is a short API key, not hot-path data.
fn base64_std(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_has_endpoint_fixed_encoding_and_encoded_args() {
        let req = GetRange {
            dataset: "GLBX.MDP3",
            symbols: "ESZ4",
            schema: "trades",
            start: "2024-01-02T00:00:00",
            end: "2024-01-02T00:01:00",
        };
        let url = build_url(HIST_BASE, &req);
        assert!(url.starts_with("https://hist.databento.com/v0/timeseries.get_range?"));
        assert!(url.contains("dataset=GLBX.MDP3"));
        assert!(url.contains("schema=trades"));
        assert!(url.contains("encoding=csv"));
        assert!(url.contains("compression=none"));
        assert!(url.contains("stype_in=raw_symbol"));
        assert!(url.contains("start=2024-01-02T00%3A00%3A00"), "colons percent-encoded");
    }

    #[test]
    fn base64_matches_known_vector() {
        assert_eq!(base64_std(b"db-key:"), "ZGIta2V5Og==");
    }
}
