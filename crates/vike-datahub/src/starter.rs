//! `store fetch-starter` — download the published starter dataset into the hist store. Behind the
//! `fetch-starter` feature, which is what carries `ureq` here.
//!
//! # Why this exists beside `vike-cli data hist fetch`
//!
//! A venue fetch is the right answer when the venue is reachable. It is not always: Binance geoblocks
//! whole countries at the REST layer, a corporate network may allow GitHub and nothing else, and an
//! air-gapped box allows neither. For every one of those the product looked identical to a broken
//! install — the tools ran and found no data.
//!
//! So the same bars are published as a release asset on the PUBLIC mirror
//! (`github.com/vike-io/vike-trader`), which is a plain HTTPS download with no credentials and no venue
//! involved. `scripts/publish_starter_data.sh` produces it, from this workspace's own fetch and
//! export, so the dataset is exactly what a user's own fetch would have written.
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
use std::time::Duration;

use vike_data::{SeriesId, StoreKindId};

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
/// `crates/vike-datahub/tests/starter_dataset_gate.rs`, because a name that disagrees between producer
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
pub struct StarterSeriesLoad {
    pub file: &'static str,
    /// Bytes downloaded.
    pub bytes: usize,
    /// Rows the store accepted. Zero means this exact file was already loaded — a success.
    pub rows: usize,
    /// The digest, so it appears in the terminal and in any log of the run.
    pub sha256: String,
}

/// How long opening the socket may take. A black-holed route otherwise waits out the OS SYN-retry
/// ladder (minutes) with the command printing nothing.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Ceiling on one whole request, redirects and body included. Unlike a collector's streamed
/// download, every body here is read into memory under a hard byte cap (`fetch_into`'s `CAP`, 8 MiB),
/// so a whole-request ceiling cannot clip a healthy download: 300 s still admits 8 MiB at ~28 KiB/s.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// The agent every download goes through: ureq's defaults (4xx/5xx are errors, redirects followed)
/// plus the two bounds above. `ureq::get` builds a default agent with NO timeout at all, which let a
/// stalled mirror hang `store fetch-starter` forever.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .new_agent()
}

/// Fetch `url` into memory, refusing anything larger than `cap`.
///
/// # Errors
///
/// Transport failures verbatim, plus an explicit refusal for a body over `cap` — an unbounded read
/// of a URL is how a redirect to something enormous becomes an out-of-memory kill rather than a
/// message.
fn get(url: &str, cap: usize) -> Result<Vec<u8>, String> {
    let mut response = agent().get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(cap as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("reading {url}: {e}"))?;
    if body.len() > cap {
        return Err(format!("{url} is larger than the {cap}-byte cap this command will accept"));
    }
    Ok(body)
}

/// The digest a `SHA256SUMS` body records for `file`, in the usual `<hex>  <name>` shape.
fn digest_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hex, name) = l.split_once("  ")?;
        (name.trim() == file).then(|| hex.trim().to_ascii_lowercase())
    })
}

/// Download every series published under `base` and append it to `store`.
///
/// `base` is [`STARTER_BASE`] in production and a loopback origin in this module's tests: the digest
/// and the store are what they prove, and neither needs the mirror.
///
/// `store` must be able to ingest a Parquet file, so this takes the concrete store rather than the
/// trait: `append_bars_from_parquet` is a `HistoryStoreDataFusion` method, and a bulk load through
/// `append_bars` would decode and re-encode every row for no reason.
///
/// # Errors
///
/// A target series that already holds another producer's rows, before anything is fetched. Any
/// transport, digest or store failure. A series that fails stops the run: a partially-loaded
/// starter dataset is the state hardest to reason about later, and re-running is free because each
/// series' commit key makes an already-loaded one a no-op.
pub fn fetch_into(
    base: &str,
    store: &vike_store::HistoryStoreDataFusion,
    scratch: &Path,
    mut progress: impl FnMut(&str),
) -> Result<Vec<StarterSeriesLoad>, String> {
    // 8 MB is comfortably above the published set and far below anything that could exhaust a
    // container's memory. It is a REFUSAL, not a truncation — see `get`.
    const CAP: usize = 8 * 1024 * 1024;

    // A first-run verb: the store's idempotency is per commit key and never per row, so a load beside
    // rows no starter file wrote would store the bars they share twice.
    for series in STARTER_SERIES {
        let interval = Some(series.interval.to_string());
        let id = SeriesId::per_symbol(StoreKindId::Bar, series.venue, series.symbol, interval);
        let (coverage, commits) =
            store.series_facts(&id).map_err(|e| format!("reading {}: {e}", series.file))?;
        let foreign = commits.iter().find(|key| !key.starts_with("starter:"));
        // A keyless append leaves rows and no key, and nothing then says a starter file wrote them.
        if coverage.rows > 0 && (foreign.is_some() || commits.is_empty()) {
            let by = foreign.map_or("no commit key", String::as_str);
            return Err(format!(
                "{} {} {} already holds {} rows from another producer ({by}), and the starter file \
                 would store the bars they share a second time. Nothing was downloaded or written. \
                 That series does not need a first-run dataset; to load it anyway, pass --store \
                 with an empty directory.",
                series.venue, series.symbol, series.interval, coverage.rows
            ));
        }
    }

    let sums_url = format!("{base}/SHA256SUMS");
    progress(&format!("fetching {sums_url}"));
    let sums = String::from_utf8(get(&sums_url, 64 * 1024)?)
        .map_err(|e| format!("{sums_url} is not text: {e}"))?;

    let mut out = Vec::with_capacity(STARTER_SERIES.len());
    for series in STARTER_SERIES {
        let want = digest_for(&sums, series.file).ok_or_else(|| {
            format!(
                "SHA256SUMS carries no line for {}. The published set and this build disagree \
                 about what exists — `crates/vike-datahub/tests/starter_dataset_gate.rs` holds the two \
                 rosters equal, so this means the RELEASE is behind, not the binary.",
                series.file
            )
        })?;
        let url = format!("{base}/{}", series.file);
        progress(&format!("fetching {url}"));
        let body = get(&url, CAP)?;
        let got = vike_hub_protocol::hash::sha256_hex(&body);
        if got != want {
            // True of THIS file only once an earlier series has landed: those rows stay, and the
            // sentence must not tell the operator the store is untouched.
            let landed: Vec<&str> =
                out.iter().filter(|l: &&StarterSeriesLoad| l.rows > 0).map(|l| l.file).collect();
            let written = if landed.is_empty() {
                "Nothing was written.".to_string()
            } else {
                format!(
                    "This file wrote nothing; {} landed before it and stays, and a re-run lands \
                     only what is missing.",
                    landed.join(", ")
                )
            };
            return Err(format!(
                "{} failed its checksum: expected {want}, got {got}. {written} A truncated \
                 download is the ordinary cause; re-run.",
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

        out.push(StarterSeriesLoad { file: series.file, bytes: body.len(), rows, sha256: got });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::PathBuf;

    use vike_data::{HistoryStore as _, TimestampRange};
    use vike_marketdata::Bar;
    use vike_store::HistoryStoreDataFusion;

    use crate::test_support::tree_bytes;

    use super::*;

    const HOURLY_ROWS: usize = 3;
    const MINUTE_ROWS: usize = 5;

    /// A scratch store, a scratch directory and the two files the roster names, as real Parquet.
    struct Rig {
        _dir: tempfile::TempDir,
        root: PathBuf,
        scratch: PathBuf,
        store: HistoryStoreDataFusion,
        hourly: Vec<u8>,
        minute: Vec<u8>,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("a scratch directory");
            let root = dir.path().join("hist");
            let scratch = dir.path().join("scratch");
            std::fs::create_dir_all(&scratch).expect("the scratch directory");
            let parquet = |rows: usize| {
                let bars: Vec<Bar> = (0..rows)
                    .map(|i| {
                        let t = i as f64;
                        Bar {
                            timestamp: 1_700_000_000_000 + i as i64 * 60_000,
                            open: 100.0 + t,
                            high: 101.0 + t,
                            low: 99.0 + t,
                            close: 100.5 + t,
                            volume: 7.0 + t,
                            funding: None,
                            bid: None,
                            ask: None,
                            symbol: Some("BTCUSDT".to_string()),
                        }
                    })
                    .collect();
                let path = dir.path().join(format!("published-{rows}.parquet"));
                vike_store::write_bars_parquet(&path, &bars).expect("encode the published file");
                std::fs::read(&path).expect("read the published file")
            };
            let hourly = parquet(HOURLY_ROWS);
            let minute = parquet(MINUTE_ROWS);
            let store = HistoryStoreDataFusion::open(&root).expect("the scratch store");
            Rig { _dir: dir, root, scratch, store, hourly, minute }
        }

        /// The true digests of both files, as `SHA256SUMS` publishes them.
        fn sums(&self) -> String {
            [(STARTER_SERIES[0].file, &self.hourly), (STARTER_SERIES[1].file, &self.minute)]
                .iter()
                .map(|(file, body)| {
                    format!("{}  {file}\n", vike_hub_protocol::hash::sha256_hex(body))
                })
                .collect()
        }

        /// A release serving the true `SHA256SUMS` beside the two bodies given, returning its base.
        fn release(&self, hourly: &[u8], minute: &[u8]) -> String {
            serve_release(vec![
                ("SHA256SUMS".to_string(), self.sums().into_bytes()),
                (STARTER_SERIES[0].file.to_string(), hourly.to_vec()),
                (STARTER_SERIES[1].file.to_string(), minute.to_vec()),
            ])
        }

        fn fetch(&self, base: &str) -> Result<Vec<StarterSeriesLoad>, String> {
            fetch_into(base, &self.store, &self.scratch, |_| {})
        }

        fn rows_of(&self, interval: &str) -> usize {
            self.store
                .load_bars("binance", "BTCUSDT", interval, TimestampRange::all())
                .expect("read the series back")
                .len()
        }
    }

    /// A published release on a loopback port: `GET /<name>` answers that asset, anything else a 404.
    fn serve_release(assets: Vec<(String, Vec<u8>)>) -> String {
        let addr = vike_log::loopback::spawn_on_loopback("starter-test-release", move |listener| {
            for mut conn in listener.incoming().flatten() {
                let mut head = [0u8; 4096];
                let n = conn.read(&mut head).unwrap_or(0);
                let request = String::from_utf8_lossy(&head[..n]);
                let asked = request.split_whitespace().nth(1).unwrap_or("").trim_start_matches('/');
                let body = assets.iter().find(|(name, _)| name == asked).map(|(_, b)| b.as_slice());
                let status = if body.is_some() { "200 OK" } else { "404 Not Found" };
                let body = body.unwrap_or_default();
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(body);
            }
        });
        format!("http://{addr}")
    }

    /// THE happy path, offline: every published series lands under its real venue, the digest is
    /// reported, the scratch is left empty, and a re-run writes NOTHING NEW, asserted on the tree's
    /// bytes rather than on the row count the command prints.
    #[test]
    fn a_published_release_loads_and_a_rerun_writes_nothing_new() {
        let rig = Rig::new();
        let base = rig.release(&rig.hourly, &rig.minute);

        let first = rig.fetch(&base).expect("the first load");
        let rows: Vec<usize> = first.iter().map(|l| l.rows).collect();
        assert_eq!(rows, [HOURLY_ROWS, MINUTE_ROWS], "{first:?}");
        assert_eq!(first[0].sha256, vike_hub_protocol::hash::sha256_hex(&rig.hourly));
        assert_eq!(first[0].bytes, rig.hourly.len());
        assert_eq!(rig.rows_of("1h"), HOURLY_ROWS);
        assert_eq!(rig.rows_of("1m"), MINUTE_ROWS);
        assert!(tree_bytes(&rig.scratch).is_empty(), "the scratch must be left empty");

        let after_first = tree_bytes(&rig.root);
        let second = rig.fetch(&base).expect("a re-run is a success");
        let rows: Vec<usize> = second.iter().map(|l| l.rows).collect();
        assert_eq!(rows, [0, 0], "each commit key is spent: {second:?}");
        assert_eq!(tree_bytes(&rig.root), after_first, "a re-run must write nothing new");
    }

    /// **A truncated download writes nothing.** The failure the checksum exists for: without it the
    /// file would land as a short slice nobody notices, or a store error about a torn footer.
    #[test]
    fn a_truncated_download_fails_its_checksum_and_writes_nothing() {
        let rig = Rig::new();
        let before = tree_bytes(&rig.root);
        let truncated = &rig.hourly[..rig.hourly.len() / 2];

        let err = rig.fetch(&rig.release(truncated, &rig.minute)).expect_err("a bad digest");
        assert!(err.contains("failed its checksum"), "{err}");
        assert!(err.contains(STARTER_SERIES[0].file), "names the file: {err}");
        assert!(err.contains("Nothing was written."), "true on the first series: {err}");
        assert_eq!(tree_bytes(&rig.root), before, "a failed checksum must write nothing");
        assert!(rig.store.list_series().expect("the listing").is_empty());
        assert!(tree_bytes(&rig.scratch).is_empty(), "nothing is left in the scratch either");
    }

    /// A series that fails STOPS the run and what landed before it stays, and the error SAYS so. A
    /// second run then lands only what is missing, which is the promise `fetch_into`'s doc makes for
    /// a partial load.
    #[test]
    fn a_failure_on_a_later_series_keeps_the_earlier_one_and_a_rerun_finishes() {
        let rig = Rig::new();
        let truncated = &rig.minute[..rig.minute.len() / 2];

        let err = rig.fetch(&rig.release(&rig.hourly, truncated)).expect_err("a bad digest");
        assert!(err.contains(STARTER_SERIES[1].file), "names the file: {err}");
        assert!(!err.contains("Nothing was written"), "the first series DID land: {err}");
        assert!(err.contains(STARTER_SERIES[0].file), "names what landed: {err}");
        assert!(err.contains("stays"), "{err}");
        assert_eq!(rig.rows_of("1h"), HOURLY_ROWS, "the series before it stays");
        assert_eq!(rig.rows_of("1m"), 0, "the failed series wrote nothing");

        let done = rig.fetch(&rig.release(&rig.hourly, &rig.minute)).expect("the re-run");
        let rows: Vec<usize> = done.iter().map(|l| l.rows).collect();
        assert_eq!(rows, [0, MINUTE_ROWS], "only what was missing lands: {done:?}");
    }

    /// A `SHA256SUMS` that does not name a file refuses before any download of it, and writes nothing:
    /// the release is behind this build, and the message says so.
    #[test]
    fn a_release_that_does_not_name_a_file_writes_nothing() {
        let rig = Rig::new();
        let before = tree_bytes(&rig.root);
        let base =
            serve_release(vec![("SHA256SUMS".to_string(), b"abc  other.parquet\n".to_vec())]);

        let err = rig.fetch(&base).expect_err("no line for the first file");
        assert!(err.contains("carries no line"), "{err}");
        assert_eq!(tree_bytes(&rig.root), before);
    }

    /// **A series another producer wrote is refused, naming it and the way on, and keeps every byte.**
    /// The store's idempotency is per commit key, so the load used to land beside those rows and store
    /// the bars they share twice, and exit 0. A keyless append (`None`) names no producer, and counts.
    #[test]
    fn a_series_another_producer_wrote_is_refused_and_nothing_is_written() {
        for key in [Some("klines:binance:BTCUSDT:1h:0-1"), None] {
            let rig = Rig::new();
            let theirs = rig.scratch.join("theirs.parquet");
            std::fs::write(&theirs, &rig.hourly).expect("write the other producer's file");
            rig.store
                .append_bars_from_parquet(&theirs, "binance", "BTCUSDT", "1h", key)
                .expect("the other producer's rows land");
            std::fs::remove_file(&theirs).expect("remove the file");
            let before = tree_bytes(&rig.root);

            let err = rig.fetch(&rig.release(&rig.hourly, &rig.minute)).expect_err("refused");
            assert!(err.contains("binance BTCUSDT 1h"), "names the series: {err}");
            assert!(err.contains(key.unwrap_or("no commit key")), "names who: {err}");
            assert!(err.contains("--store"), "names the way on: {err}");
            assert_eq!(tree_bytes(&rig.root), before, "not one byte moves");
            assert_eq!(rig.rows_of("1h"), HOURLY_ROWS, "the other producer's rows, once");
            assert_eq!(rig.rows_of("1m"), 0, "and the series next to it is not touched either");
        }
    }

    /// The download agent carries both bounds: a bare `ureq::get` has neither, so a stalled mirror
    /// would hang the command.
    #[test]
    fn the_download_agent_bounds_the_connect_and_the_whole_request() {
        let timeouts = agent().config().timeouts();
        assert_eq!(timeouts.connect, Some(CONNECT_TIMEOUT));
        assert_eq!(timeouts.global, Some(REQUEST_TIMEOUT));
    }

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
