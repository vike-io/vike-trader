//! Which `HistStore` a Polymarket backtest bin reads from — the ONE selector both drivers
//! (`crates/vike-backfill/src/bin/poly_ch_backtest.rs` and
//! `crates/vike-poly-research/src/main.rs`) share, so the two cannot answer "where does
//! this backtest's data come from" differently. Before this module they did: `poly_mm_batch` had
//! `--store`/`--archive` and `poly_ch_backtest` had no non-ClickHouse route at all.
//!
//! It exists because of the rule the owner set on 2026-09-19: **data is fetched by API or from the
//! venue directly, never by reaching ClickHouse.** Two collectors were retired under it that day
//! (`clickhouse_poly_backfill`, `clickhouse_spot_backfill`), this module demoted the read-side
//! ClickHouse store from the DEFAULT to an explicitly-named `--clickhouse`, and on 2026-09-20 the
//! owner ruled REMOVE on the rest. So there are TWO store flags now, not three, and **no code in
//! this workspace reaches a ClickHouse server by any route** — the last `clickhouse-client`
//! subprocess in the tree was that module's `run_export`, and it went with it.
//!
//! # ⚠ WHAT THE DELETION COST, recorded here because an absent file explains nothing
//!
//! `crates/vike-backfill/src/backtest_bridge.rs` (`ClickHousePolyHistStore`, 1,072 lines) and
//! `crates/vike-backfill/src/clickhouse_poly/` (the export→decode→append collector, 1,150) are
//! gone. Two capabilities went with them and have NO replacement anywhere in this tree. They were
//! named at `clickhouse_poly/mod.rs`'s module doc, which is why they are restated HERE: this is the
//! file an operator asking "where does a Polymarket backtest read from" lands in, and the answer
//! now has a hole in it that the two surviving flags cannot fill.
//!
//! 1. **`polymarket_snapshots` — the ONLY source of Polymarket L1 quotes before 2026-07-23.** The
//!    live L2 recorder's `l1_quotes` table starts on that date, and `data.vike.io/archive` serves
//!    `l1_quotes`; the pre-July tape was written by the retired Python poller into a table the
//!    archive does not carry and no API exposes. Roughly three months of quote history is therefore
//!    unreachable from this tree. ⚠ It is NOT deleted: the rows are still on the recorder's box.
//!    What is gone is every tool here that could read them, so recovering that window means a new
//!    API-side producer, not a revert.
//! 2. **The `<slug>#<outcome_index>` trade keying** `vike-backtest`'s `cheap_np_run` bin discovers
//!    its windows from (`crates/vike-backtest/src/bin/cheap_np_run.rs`). The archive is
//!    TOKEN-keyed — one `symbol=<token_id>` per row — and carries no slug column at all, so nothing
//!    in this tree writes a slug-keyed tape any more.
//!
//! **Neither of those was what held the two bins to ClickHouse**, which is what made the cut a code
//! question rather than a data one — measured 2026-09-19 against the deleted file and restated here
//! because the file it was measured on is gone: `backtest_bridge`'s `quotes_query` selected `FROM
//! {db}.l1_quotes` and named `polymarket_snapshots` nowhere; all three of its queries filtered
//! `WHERE token_id = '…'`, so the tape it read was token-keyed; and the ONLY item it imported from
//! `clickhouse_poly` was `run_export`, a generic `clickhouse-client` subprocess shell — it built its
//! own SQL and its own decoders. So the three lanes these bins actually read (`book_events`,
//! `trades`, `l1_quotes`, all keyed `venue=polymarket`/`symbol=token_id`) are exactly the three
//! `crates/vike-backfill/src/bin/vike_archive_backfill.rs` pulls from `data.vike.io/archive` over
//! HTTP, and exactly the three [`crate::archive_store::ArchiveParquetHistStore`] reads in place.
//!
//! ⚠ **One divergence the two surviving stores do NOT have between them, and which a comparison
//! against an OLD run still does.** `ArchiveParquetHistStore` and `vike_data::DataFusionHist` both
//! scope a `TsRange` on `ts` (the venue's own "as of" stamp); the deleted ClickHouse store scoped
//! on `local_ts` (when the recorder READ the frame off the socket). So the selector no longer has a
//! clock CHOICE to make — but a number in an operator's scrollback from a `--clickhouse` run is not
//! comparable with one produced today, and the recorder's two stamps still disagree on the tape
//! both surviving stores read. [`crate::window_clock::WindowClock`] carries the measurement and
//! [`window_clock_banner`] is what a run PRINTS, so a difference between two runs is not read as a
//! regression. (It sits in its own module rather than here because this one is feature-gated —
//! that module's doc argues it.)

use std::path::PathBuf;
use std::sync::Arc;

use crate::{DataFusionHist, HistStore};

use crate::archive_store::ArchiveParquetHistStore;
use crate::window_clock::WindowClock;

/// Which backing store a Polymarket backtest reads.
///
/// Measured on a 1.20 GiB / 74.4M-row / 562-token day
/// (`.superpowers/sdd/2026-07-28-poly-mm-latency-batch/archive-store-report.md`):
///
/// | path | one-market slice |
/// |---|---|
/// | [`Self::Archive`] (read in place) | **1.2 s** — 6/73 row groups, 8.3% of compressed bytes, 92 MB RSS |
/// | [`Self::Local`] (download + import first) | **128.6 s** — 23.6 s download + ~105 s import |
///
/// The crossover is ~195 token-scans: below that, reading in place wins outright; above it the
/// one-time import amortises.  A 20-token run is 13.2 s on `Archive`, still ~10x ahead.
///
/// ⚠ A THIRD variant stood here — `ClickHouse`, the live recorder's `polymarket.*` tables through
/// `ClickHousePolyHistStore` — and is DELETED (2026-09-20). This module's doc carries what went
/// with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BacktestStore {
    /// `--archive PATH` — downloaded `data.vike.io` archive Parquet, read IN PLACE. A directory
    /// takes every `*.parquet` under it (one file per UTC day is the archive's shape); a single
    /// file path takes just that file.
    Archive(PathBuf),
    /// `--store DIR` — a `vike_data::DataFusionHist` store an operator already filled, e.g. with
    /// `vike_archive_backfill --kind all --store DIR`.
    Local(PathBuf),
}

impl BacktestStore {
    /// Which clock this selection's `TsRange` will be scoped on. Kept HERE rather than on each
    /// store so the selector — the one place that knows every store exists — is what answers.
    ///
    /// ⚠ Every surviving selection answers [`WindowClock::Ts`], and the match is written out
    /// variant by variant rather than collapsed to one `Ts` so that a store added later cannot
    /// inherit that answer without being classified. That is the whole failure this type exists to
    /// prevent, and it was a live disagreement until the `local_ts`-scoped store was deleted.
    pub fn window_clock(&self) -> WindowClock {
        match self {
            BacktestStore::Archive(_) | BacktestStore::Local(_) => WindowClock::Ts,
        }
    }
}

/// The line a RUN prints, so the clock its window was scoped on is in the operator's own scrollback
/// beside the numbers rather than only in a doc comment. Both bins print it on stderr right after
/// the store is selected — `crates/vike-backfill/src/bin/poly_ch_backtest.rs`'s `main` and
/// `crates/vike-poly-research/src/main.rs`'s `main`.
///
/// ⚠ It named BOTH clocks and warned that two runs through different stores were not comparable.
/// With one clock left in the tree that warning would be false, and the honest line is narrower: it
/// states the column this run used and that the recorder's OTHER stamp is a different number, which
/// is what makes a `--clickhouse` run recorded before 2026-09-20 incomparable with one produced
/// today.
///
/// ⚠ It names the COLUMN and the tape, never a hostname, the same constraint
/// [`NO_STORE_SELECTED`] carries and for the same reason: this is a shipped string literal and
/// `crates/vike-ops/tests/shipped_box_name_gate.rs` greps every one of them.
pub fn window_clock_banner(sel: &BacktestStore) -> String {
    let clock = sel.window_clock();
    format!(
        "window clock: {} ({}) — every store a backtest can be pointed at now scopes its window on \
         this column. The recorder stamps each row twice and {} is NOT the same number (measured: \
         at a 5-minute window the two disagree about whether a book snapshot anchor is present for \
         ~1 market-window in 8), so a run recorded against the retired {}-scoped ClickHouse store \
         is not row-for-row comparable with this one; see vike_backfill::window_clock::WindowClock",
        clock.column(),
        clock.gloss(),
        clock.other().column(),
        clock.other().column(),
    )
}

/// The refusal an operator sees when no store flag was given at all. A message rather than a
/// default because the one available default is a guess: a path the bin would have to invent is
/// exactly the guess this crate refuses to make elsewhere.
///
/// ⚠ It used to carry a third line, `--clickhouse`, and the box that line named ("the recorder's
/// own box") was the reason it had to name a ROLE rather than a hostname. That constraint has not
/// gone away for the two lines that remain: this is a shipped string literal,
/// `crates/vike-ops/tests/shipped_box_name_gate.rs` greps every one of them, and the release build
/// greps the published binary's raw bytes for the same tokens and publishes NOTHING on a hit.
pub const NO_STORE_SELECTED: &str = "\
no store selected — name one of:
  --archive PATH   downloaded data.vike.io archive Parquet, read in place (no import step)
  --store DIR      a DataFusion hist store, e.g. filled by
                   `vike_archive_backfill --kind all --store DIR`";

/// Pure store-flag reader — no I/O, and no validation of the paths themselves (a bad or missing
/// path surfaces from [`open_backtest_store`] as an open error, which is where the filesystem
/// is actually consulted).
///
/// Precedence when both are named: `--archive` beats `--store`. It is ordered by COST, cheapest
/// first, because naming a flag is an explicit request for that path and silently preferring the
/// slower one would be the surprising reading. ⚠ That precedence is pre-existing behaviour carried
/// verbatim from `poly_mm_batch`'s own former `parse_store_sel`.
pub fn parse_backtest_store(
    store_arg: Option<&str>,
    archive_arg: Option<&str>,
) -> Result<BacktestStore, String> {
    match (archive_arg, store_arg) {
        (Some(p), _) => Ok(BacktestStore::Archive(PathBuf::from(p))),
        (None, Some(dir)) => Ok(BacktestStore::Local(PathBuf::from(dir))),
        (None, None) => Err(NO_STORE_SELECTED.to_string()),
    }
}

/// Build the `Arc<dyn HistStore + Send + Sync>` the harness takes, per `sel`.
///
/// ⚠ This took three more parameters — `ch_bin`/`db`/`scratch` — consulted only by the deleted
/// ClickHouse arm. `scratch` was `<project>/tmp` (`crate::cli::scratch_root`), resolved by the
/// calling BINARY and passed down because a library must not reach for global state its caller can
/// neither see nor override (`crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is the
/// rule). Both surviving stores read a path the operator named outright, so nothing is staged
/// anywhere and there is no root to thread.
pub fn open_backtest_store(sel: BacktestStore) -> Result<Arc<dyn HistStore + Send + Sync>, String> {
    match sel {
        BacktestStore::Local(dir) => DataFusionHist::open(&dir)
            .map(|h| Arc::new(h) as Arc<dyn HistStore + Send + Sync>)
            .map_err(|e| format!("open local store at {}: {e}", dir.display())),
        BacktestStore::Archive(path) => {
            let store = if path.is_dir() {
                ArchiveParquetHistStore::from_dir(&path)
                    .map_err(|e| format!("open archive dir {}: {e}", path.display()))?
            } else {
                ArchiveParquetHistStore::from_files([path.clone()])
            };
            // An empty selection is a CLEAR startup error, not a store that answers every scan
            // "no rows" — `HistStore`'s `Ok(vec![])` means "none in this range", a claim of fact,
            // and `crates/vike-data/src/archive_store.rs`'s module doc carries what believing
            // that wrongly once cost (a scalper scoring -2.74% where the truth was -27.64%).
            if store.files().is_empty() {
                return Err(format!("no .parquet files under {}", path.display()));
            }
            Ok(Arc::new(store) as Arc<dyn HistStore + Send + Sync>)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TsRange;

    /// Naming NOTHING is a refusal, not a silent default. This was the 2026-09-19 behaviour change:
    /// the pre-selector `poly_mm_batch` mapped "neither flag" to the ClickHouse store, so an
    /// operator who forgot a flag reached the recorder's database by omission.
    ///
    /// ⚠ The refusal may no longer OFFER that path, and this test says so: with the store deleted,
    /// a message still printing `--clickhouse` would send an operator to a flag the bin rejects.
    #[test]
    fn no_flag_refuses_rather_than_defaulting() {
        let err = parse_backtest_store(None, None).unwrap_err();
        assert!(err.contains("--archive"), "{err}");
        assert!(err.contains("--store"), "{err}");
        assert!(!err.contains("--clickhouse"), "{err}");
    }

    /// Cheapest-first precedence, with every combination that has a winner spelled out.
    #[test]
    fn archive_beats_store() {
        assert_eq!(
            parse_backtest_store(Some("/tmp/hist"), Some("/tmp/dl")).unwrap(),
            BacktestStore::Archive(PathBuf::from("/tmp/dl")),
        );
        assert_eq!(
            parse_backtest_store(Some("/tmp/hist"), None).unwrap(),
            BacktestStore::Local(PathBuf::from("/tmp/hist")),
        );
        assert_eq!(
            parse_backtest_store(None, Some("/tmp/dl")).unwrap(),
            BacktestStore::Archive(PathBuf::from("/tmp/dl")),
        );
    }

    /// `--archive FILE` (a single day file, not a directory) is carried through as that one file —
    /// the directory-vs-file split happens in `open_backtest_store`, not in the parse.
    #[test]
    fn archive_takes_a_single_file_path_too() {
        assert_eq!(
            parse_backtest_store(None, Some("/tmp/dl/btc5m_2026-07-28.parquet")).unwrap(),
            BacktestStore::Archive(PathBuf::from("/tmp/dl/btc5m_2026-07-28.parquet")),
        );
    }

    /// An `--archive` path with no `.parquet` under it is a startup error, never an empty store.
    #[test]
    fn empty_archive_dir_is_an_error_not_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        // `Arc<dyn HistStore>` is not `Debug`, so `unwrap_err`/`expect_err` cannot be used on this
        // return type — every negative assertion below matches instead.
        let Err(err) = open_backtest_store(BacktestStore::Archive(dir.path().to_path_buf())) else {
            panic!("an archive dir with no parquet must not open");
        };
        assert!(err.contains("no .parquet files"), "{err}");
    }

    /// `Local` really opens a working `DataFusionHist`: a fresh empty store answers scans cleanly
    /// (empty, not an error), proving `--store DIR` produces a usable store rather than a handle
    /// that only fails later.
    #[test]
    fn local_opens_a_real_datafusion_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_backtest_store(BacktestStore::Local(dir.path().to_path_buf()))
            .expect("open empty DataFusionHist");
        assert!(store.scan_trades("polymarket", "tok", TsRange::all()).unwrap().is_empty());
    }

    /// `--archive FILE` (a single day file, not a directory) opens that one file. Opening is LAZY —
    /// the file is only decoded on a scan — so this asserts the FILE LIST, and a corrupt file
    /// surfaces at scan time exactly as it does for every other store.
    #[test]
    fn archive_opens_a_single_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("btc5m_2026-07-28.parquet");
        std::fs::write(&f, b"not really parquet").unwrap();
        open_backtest_store(BacktestStore::Archive(f)).expect("a single archive file path opens");
    }

    /// A bogus local directory (a FILE where `DataFusionHist` expects a directory store) is a clean
    /// `Err` naming the path, not a panic — each bin turns this into `ExitCode::FAILURE`.
    #[test]
    fn local_bad_dir_is_a_clean_error_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let bad_path = dir.path().join("not_a_dir_but_a_file");
        std::fs::write(&bad_path, b"nope").unwrap();
        let Err(err) = open_backtest_store(BacktestStore::Local(bad_path.clone())) else {
            panic!("a file in place of a store directory must not open");
        };
        assert!(err.contains(&bad_path.display().to_string()), "{err}");
    }

    // ---- the window-clock divergence (see `WindowClock`) -----------------------------------

    /// EVERY selection now scopes on the venue's own stamp. Written as a match over every variant
    /// rather than two `assert_eq!`s so a THIRD store cannot be added without classifying it —
    /// which is the whole failure this type exists to prevent, and which was a live disagreement
    /// until the `local_ts`-scoped ClickHouse store was deleted on 2026-09-20.
    #[test]
    fn every_selection_scopes_on_the_venue_clock() {
        for sel in [
            BacktestStore::Archive(PathBuf::from("/tmp/dl")),
            BacktestStore::Local(PathBuf::from("/tmp/hist")),
        ] {
            let expected = match sel {
                BacktestStore::Archive(_) | BacktestStore::Local(_) => WindowClock::Ts,
            };
            assert_eq!(sel.window_clock(), expected, "{sel:?}");
        }
    }

    /// The banner names BOTH columns — the one this run used and the one the recorder's other stamp
    /// carries. A banner naming only its own clock would read as a fact about the data rather than
    /// as a warning that an older run is not comparable, which is the whole point of printing it.
    #[test]
    fn the_banner_names_both_clocks_and_never_a_hostname() {
        for sel in [
            BacktestStore::Archive(PathBuf::from("/tmp/dl")),
            BacktestStore::Local(PathBuf::from("/tmp/hist")),
        ] {
            let banner = window_clock_banner(&sel);
            assert!(banner.contains("window clock: ts"), "{banner}");
            assert!(banner.contains("local_ts"), "{banner}");
            // Same constraint as `NO_STORE_SELECTED`: a shipped string may name the ROLE, never a
            // box.
            assert!(!banner.contains("prod"), "{banner}");
        }
    }
}
