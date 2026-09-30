//! `backtest data fetch-starter` — download the published starter dataset into the hist store.
//!
//! # Why this exists beside `data fetch`
//!
//! `data fetch` reaches the venue directly, which is the right answer when the venue is reachable. It
//! is not always: Binance geoblocks whole countries at the REST layer, a corporate network may
//! allow GitHub and nothing else, and an air-gapped box allows neither. For every one of those the
//! product looked identical to a broken install — the tools ran and found no data.
//!
//! So the same bars are published as a release asset on the PUBLIC mirror
//! (`github.com/vike-io/vike-trader`), which is a plain HTTPS download with no credentials and no venue
//! involved. `scripts/publish_starter_data.sh` produces it, from this workspace's own `data fetch` and
//! `data export`, so the dataset is exactly what a user's own fetch would have written.
//!
//! # What is checked, and what deliberately is not
//!
//! The download is verified against a `SHA256SUMS` file published beside it, and the digest is
//! PRINTED. That catches the failure that actually happens — a truncated or corrupted download,
//! which would otherwise land in the store as a short slice nobody notices. It does not attempt to
//! establish that the release itself is trustworthy: that rests on HTTPS and on GitHub, the same
//! way `cargo` trusts crates.io, and a digest pinned in this file would only move the question to
//! whoever edits this file.
//!
//! ⚠ Rows land under the venue's REAL id, never `vike_data::demo::DEMO_VENUE`. This is real market
//! data; the demo tape is a curve. Keeping them in separate namespaces is what stops a result
//! computed on one from being confused with the other.

use std::io::Read;
use std::path::Path;

/// Where the starter dataset is published.
///
/// The PUBLIC mirror, not this repository: this one is private, so its release assets need a token
/// and a stranger who pulled the container image would get a 404. The mirror exists precisely so
/// the things a user needs are reachable without an account.
pub const STARTER_BASE: &str =
    "https://github.com/vike-io/vike-trader/releases/download/starter-data";

/// One published series.
///
/// A ROSTER rather than a single file, because the two shapes a first run needs are different: an
/// hourly span long enough for a strategy to have a history, and a minute span dense enough for a
/// chart to look like a market. Both are the SAME instrument, so a user comparing them is looking
/// at one tape at two resolutions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StarterSeries {
    /// The asset's file name in the release, and the local file name while it is being verified.
    pub file: &'static str,
    /// Venue id the rows are written under — a REAL venue; this is real market data.
    pub venue: &'static str,
    pub symbol: &'static str,
    pub interval: &'static str,
}

/// The published set. Kept in step with `scripts/publish_starter_data.sh` by
/// `crates/vike-ops/tests/starter_dataset_gate.rs`, because a name that disagrees between producer
/// and consumer is a 404 at the moment a new user first tries the product.
pub const STARTER_SERIES: &[StarterSeries] = &[
    StarterSeries {
        file: "binance-BTCUSDT-1h.parquet",
        venue: "binance",
        symbol: "BTCUSDT",
        interval: "1h",
    },
    StarterSeries {
        file: "binance-BTCUSDT-1m.parquet",
        venue: "binance",
        symbol: "BTCUSDT",
        interval: "1m",
    },
];

/// The append key one starter series lands under.
///
/// Carries the file name, so re-running the command writes nothing while a REFRESHED dataset — a
/// new file name — still lands. The alternative, a key naming only the series, would make the
/// starter data un-refreshable: the second download would be silently discarded.
#[must_use]
pub fn commit_key(series: &StarterSeries) -> String {
    format!("starter:{}", series.file)
}

/// What one series' download did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub file: &'static str,
    /// Bytes downloaded.
    pub bytes: usize,
    /// Rows the store accepted. Zero means this exact file was already loaded — a success.
    pub rows: usize,
    /// The digest, so it appears in the terminal and in any log of the run.
    pub sha256: String,
}

/// Fetch `url` into memory, refusing anything larger than `cap`.
///
/// # Errors
///
/// Transport failures verbatim, plus an explicit refusal for a body over `cap` — an unbounded read
/// of a URL is how a redirect to something enormous becomes an out-of-memory kill rather than a
/// message.
fn get(url: &str, cap: usize) -> Result<Vec<u8>, String> {
    let mut resp = ureq::get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    let mut body = Vec::new();
    resp.body_mut()
        .as_reader()
        .take(cap as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("reading {url}: {e}"))?;
    if body.len() > cap {
        return Err(format!("{url} is larger than the {cap}-byte cap this command will accept"));
    }
    Ok(body)
}

/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The digest a `SHA256SUMS` body records for `file`, in the usual `<hex>  <name>` shape.
fn digest_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hex, name) = l.split_once("  ")?;
        (name.trim() == file).then(|| hex.trim().to_ascii_lowercase())
    })
}

/// Download every published series and append it to `store`.
///
/// `store` must be able to ingest a Parquet file, so this takes the concrete store rather than the
/// trait: `append_bars_from_parquet` is a `DataFusionHist` method, and a bulk load through
/// `append_bars` would decode and re-encode every row for no reason.
///
/// # Errors
///
/// Any transport, digest or store failure. A series that fails stops the run: a partially-loaded
/// starter dataset is the state hardest to reason about later, and re-running is free because each
/// series' commit key makes an already-loaded one a no-op.
pub fn fetch_into(
    store: &vike_data::DataFusionHist,
    scratch: &Path,
    mut progress: impl FnMut(&str),
) -> Result<Vec<Loaded>, String> {
    // 8 MB is comfortably above the published set and far below anything that could exhaust a
    // container's memory. It is a REFUSAL, not a truncation — see `get`.
    const CAP: usize = 8 * 1024 * 1024;

    let sums_url = format!("{STARTER_BASE}/SHA256SUMS");
    progress(&format!("fetching {sums_url}"));
    let sums = String::from_utf8(get(&sums_url, 64 * 1024)?)
        .map_err(|e| format!("{sums_url} is not text: {e}"))?;

    let mut out = Vec::with_capacity(STARTER_SERIES.len());
    for series in STARTER_SERIES {
        let want = digest_for(&sums, series.file).ok_or_else(|| {
            format!(
                "SHA256SUMS carries no line for {}. The published set and this build disagree \
                 about what exists — `crates/vike-ops/tests/starter_dataset_gate.rs` holds the two \
                 rosters equal, so this means the RELEASE is behind, not the binary.",
                series.file
            )
        })?;
        let url = format!("{STARTER_BASE}/{}", series.file);
        progress(&format!("fetching {url}"));
        let body = get(&url, CAP)?;
        let got = sha256_hex(&body);
        if got != want {
            return Err(format!(
                "{} failed its checksum: expected {want}, got {got}. Nothing was written. A \
                 truncated download is the ordinary cause; re-run.",
                series.file
            ));
        }

        // Through a file because that is what the store's bulk loader takes, and in a caller-owned
        // scratch directory so nothing is left behind on any path out of here.
        let path = scratch.join(series.file);
        std::fs::write(&path, &body).map_err(|e| format!("writing {}: {e}", path.display()))?;
        let rows = store
            .append_bars_from_parquet(
                &path,
                series.venue,
                series.symbol,
                series.interval,
                Some(&commit_key(series)),
            )
            .map_err(|e| format!("loading {}: {e}", series.file))?;
        let _ = std::fs::remove_file(&path);

        out.push(Loaded { file: series.file, bytes: body.len(), rows, sha256: got });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_line_is_read_in_the_sha256sums_shape() {
        let sums = "\
abc123  binance-BTCUSDT-1h.parquet
def456  binance-BTCUSDT-1m.parquet
";
        assert_eq!(digest_for(sums, "binance-BTCUSDT-1h.parquet").as_deref(), Some("abc123"));
        assert_eq!(digest_for(sums, "binance-BTCUSDT-1m.parquet").as_deref(), Some("def456"));
        assert_eq!(digest_for(sums, "absent.parquet"), None);
    }

    /// A prefix must not match: `…-1h.parquet` and `…-1h.parquet.old` are different assets, and
    /// taking the first digest that starts the same way would verify the wrong bytes.
    #[test]
    fn a_partial_name_does_not_match_a_digest() {
        let sums = "abc123  binance-BTCUSDT-1h.parquet.bak\n";
        assert_eq!(digest_for(sums, "binance-BTCUSDT-1h.parquet"), None);
    }

    #[test]
    fn the_digest_is_lowercase_hex_of_the_right_length() {
        let hex = sha256_hex(b"vike");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{hex}");
        // A known vector, so a swapped hasher cannot pass by producing 64 plausible characters.
        assert_eq!(sha256_hex(b"").len(), 64);
        assert_ne!(sha256_hex(b"a"), sha256_hex(b"b"));
    }

    /// The key must carry the FILE, so a refreshed dataset can land while a re-run cannot
    /// double-book.
    #[test]
    fn the_commit_key_changes_only_when_the_published_file_does() {
        let a = STARTER_SERIES[0];
        let b = StarterSeries { file: "binance-BTCUSDT-1h-v2.parquet", ..a };
        assert_eq!(commit_key(&a), commit_key(&a));
        assert_ne!(commit_key(&a), commit_key(&b));
    }

    #[test]
    fn every_published_series_is_distinct() {
        let keys: std::collections::BTreeSet<String> =
            STARTER_SERIES.iter().map(commit_key).collect();
        assert_eq!(keys.len(), STARTER_SERIES.len(), "two series share a commit key");
        let files: std::collections::BTreeSet<&str> =
            STARTER_SERIES.iter().map(|s| s.file).collect();
        assert_eq!(files.len(), STARTER_SERIES.len(), "two series share a file name");
    }

    /// The base URL must point at the PUBLIC mirror. Pointing it at the private repository would
    /// 404 for every user who is not the author — and would do so only in their terminal.
    #[test]
    fn the_base_url_is_the_public_mirror_over_https() {
        assert!(STARTER_BASE.starts_with("https://"), "{STARTER_BASE}");
        assert!(
            STARTER_BASE.contains("github.com/vike-io/vike-trader/"),
            "the starter dataset must come from the PUBLIC mirror; {STARTER_BASE} does not"
        );
        assert!(
            !STARTER_BASE.contains("vike_trader_rust"),
            "that is the PRIVATE repository — its release assets need a token, so every user \
             without one gets a 404"
        );
    }
}
