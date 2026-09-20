//! Opt-in REAL-DATA smoke for the pmxt Polymarket-L2 archive backfill (Task 3). Downloads one
//! real hourly Parquet part from the pmxt archive (CC BY 4.0, `r2v2.pmxt.dev`) and validates the
//! seq/anchor design — the spec's one flagged subtlety — against ACTUAL archive data, not the
//! synthetic fixtures Task 1/2 exercise. `#[ignore]`d AND gated behind an explicit `PMXT_SMOKE=1`
//! (the venue-smoke convention, see e.g. `crates/bridges/polymarket/tests/gamma_smoke.rs`) so
//! `cargo test -- --ignored` alone never triggers it and CI never runs it (real network, ~100-400
//! MB download).
//!
//! Run explicitly:
//! ```sh
//! PMXT_SMOKE=1 cargo test -p vike-backfill --test pmxt_smoke -- --ignored --nocapture
//! ```
//!
//! Bounded-processing strategy: [`download_hour`] always streams the WHOLE hour's Parquet part to
//! a temp file (the archive has no partial-download API), but the row-group reader IS bounded —
//! this smoke reads only the file's first row group to discover a handful of busy asset (token)
//! ids, then drives the real [`ingest_file`] filtered to just those tokens. `ingest_file`'s
//! `tokens` filter still walks every row of the file (its row-group streaming is bounded in
//! MEMORY, not wall-clock), so the smoke's disk/network/decode cost is a real full-hour pass, but
//! its assertion surface — rows actually appended into the store — stays small. Measured against
//! the real `2026-04-13T19` hour (133 MB downloaded, 3 sampled tokens filtered): full run takes
//! ~90-95s wall-clock, well within "keep it from taking many minutes" — no need for the row-group-
//! restricted-ingest fallback in practice.
//!
//! REAL-DATA RESULT, first pass (see `.superpowers/sdd/task-3-report.md` for the full writeup):
//! the anchor invariant (every sampled token's FIRST book update is a `Snapshot`) HOLDS. A
//! second, previously-unflagged invariant did NOT hold: `seq` was not strictly increasing once
//! `scan_book_updates` re-sorted by `(ts, seq)`, because pmxt's archived rows are not perfectly
//! chronologically ordered per asset (small `timestamp` regressions, up to ~500ms, within one
//! asset's own row stream) while the mapper's `seq` tracks file (true event) order.
//!
//! FIX: `crates/vike-backfill/src/pmxt/map.rs` now clamps the emitted `ts` to
//! `max(source_ts, prev_ts_for_this_asset)` per asset (file order is already true order — this
//! makes `ts` monotonic without reordering anything), so `(ts, seq)` now reproduces file order
//! exactly. This smoke's seq-monotonicity check was promoted from a diagnostic to a hard assert
//! to re-validate that against real archive data.
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;

use datafusion::arrow::array::{Array, StringArray};
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use vike_backfill::pmxt::{download_hour, ingest_file};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::BookUpdateKind;

/// The first hour the pmxt archive publishes (per the design spec's real-file verification) — a
/// stable, always-available choice for a real-data smoke.
const HOUR: &str = "2026-04-13T19";

/// How many busy (highest `book`-event-count) asset ids to sample from the file's first row
/// group. A handful is enough to exercise the seq/anchor invariant across multiple independent
/// per-asset streams without ingesting the whole file's token universe.
const WANT_TOKENS: usize = 3;

/// Scan just the first row group of a downloaded pmxt Parquet file and return the `WANT_TOKENS`
/// asset ids with the most `event_type == "book"` rows in that slice. Reading only row group 0
/// keeps discovery itself cheap; the busy tokens it names are then ingested across the WHOLE file
/// by [`ingest_file`] (which does its own full row-group walk), so the per-token series this
/// smoke asserts against still covers the entire hour for those assets.
fn discover_busy_tokens(path: &std::path::Path) -> Vec<String> {
    let file = File::open(path).expect("open downloaded pmxt parquet");
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(file).expect("open parquet arrow reader builder");
    let num_row_groups = builder.metadata().num_row_groups();
    assert!(num_row_groups > 0, "downloaded pmxt parquet has no row groups");
    let builder = builder.with_row_groups(vec![0]);
    let reader = builder.build().expect("build row-group-0-only reader");

    let mut book_counts: HashMap<String, usize> = HashMap::new();
    for batch in reader {
        let batch = batch.expect("read row group 0 batch");
        let asset_id = batch
            .column_by_name("asset_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .expect("asset_id column (string)");
        let event_type = batch
            .column_by_name("event_type")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .expect("event_type column (string)");
        for i in 0..batch.num_rows() {
            if !event_type.is_null(i) && event_type.value(i) == "book" && !asset_id.is_null(i) {
                *book_counts.entry(asset_id.value(i).to_string()).or_insert(0) += 1;
            }
        }
    }

    let mut ranked: Vec<(String, usize)> = book_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(WANT_TOKENS).map(|(asset, _)| asset).collect()
}

#[test]
#[ignore = "LIVE network: downloads a real pmxt archive hour (~100-400 MB) — opt-in PMXT_SMOKE=1"]
fn pmxt_real_hour_seq_anchor_holds() {
    if std::env::var("PMXT_SMOKE").ok().as_deref() != Some("1") {
        println!("skipped: set PMXT_SMOKE=1");
        return;
    }

    let dl_dir = tempfile::tempdir().expect("tempdir for download");
    let store_dir = tempfile::tempdir().expect("tempdir for hist store");

    println!("pmxt smoke: downloading real hour {HOUR} ...");
    let path = download_hour(HOUR, dl_dir.path())
        .expect("download_hour transport/HTTP failure")
        .unwrap_or_else(|| {
            panic!(
                "pmxt archive returned 404 for {HOUR} — expected the first-available hour to exist; \
                 pick a different HOUR if the archive's coverage start has moved"
            )
        });
    let bytes = std::fs::metadata(&path).expect("stat downloaded file").len();
    println!("pmxt smoke: downloaded {} ({bytes} bytes)", path.display());

    let busy_tokens = discover_busy_tokens(&path);
    assert!(
        !busy_tokens.is_empty(),
        "expected to discover at least one busy asset id in {HOUR}'s first row group"
    );
    println!("pmxt smoke: sampled {} busy token(s): {busy_tokens:?}", busy_tokens.len());

    let tokens: HashSet<String> = busy_tokens.iter().cloned().collect();
    let store = DataFusionHist::open(store_dir.path()).expect("open DataFusionHist store");

    let (books, trades) =
        ingest_file(&store, &path, HOUR, Some(&tokens)).expect("ingest_file real hour");
    println!("pmxt smoke: ingested {books} book event(s), {trades} trade(s) for {HOUR}");
    assert!(books > 0, "expected at least one book event across the sampled busy tokens");

    let mut first_kind_by_token: Vec<(String, BookUpdateKind)> = Vec::new();
    let mut unanchored_tokens: Vec<String> = Vec::new();

    for token in &busy_tokens {
        let updates = store
            .scan_book_updates("polymarket", token, TsRange::all())
            .unwrap_or_else(|e| panic!("scan_book_updates({token}): {e}"));
        assert!(!updates.is_empty(), "{token}: expected non-empty book-update series");

        // ts-ascending: this is a structural property of the store's own (ts, seq) sort in
        // `scan_book_updates` (`datafusion_hist.rs`), so a violation here would mean a bug in the
        // store, not a real-data surprise — kept as a hard assert.
        for w in updates.windows(2) {
            assert!(
                w[0].ts <= w[1].ts,
                "{token}: scan_book_updates must return ts-ascending events (got {} then {})",
                w[0].ts,
                w[1].ts
            );
        }

        // seq strictly increasing in scan order: with the mapper's ts-clamp fix (map.rs), `ts`
        // is monotonic per asset by construction (source-ts regressions are clamped up to the
        // prior max rather than sorted away from file order), so scan_book_updates's (ts, seq)
        // sort can no longer reorder events away from the true event order the mapper assigned
        // `seq` in. ts-ascending and seq-ascending must now agree. Hard assert — this is the
        // real-data re-validation of the fix, not a synthetic-fixture check.
        for w in updates.windows(2) {
            assert!(
                w[1].seq > w[0].seq,
                "{token}: scan_book_updates must return strictly increasing seq after the \
                 ts-clamp fix (got seq {} then {} at ts {} then {})",
                w[0].seq,
                w[1].seq,
                w[0].ts,
                w[1].ts
            );
        }

        let has_snapshot = updates.iter().any(|u| u.kind == BookUpdateKind::Snapshot);
        assert!(has_snapshot, "{token}: expected at least one Snapshot in its book-update series");

        let first = updates[0].kind;
        first_kind_by_token.push((token.clone(), first));
        if first != BookUpdateKind::Snapshot {
            unanchored_tokens.push(token.clone());
        }

        println!(
            "pmxt smoke: {token}: {} update(s), first-kind={:?}, seq range [{}, {}], \
             ts range [{}, {}]",
            updates.len(),
            first,
            updates.first().unwrap().seq,
            updates.last().unwrap().seq,
            updates.first().unwrap().ts,
            updates.last().unwrap().ts,
        );
    }

    println!("pmxt smoke summary: tokens={busy_tokens:?} books={books} trades={trades}");
    println!("pmxt smoke summary: first-update-kind per token: {first_kind_by_token:?}");

    let mut problems: Vec<String> = Vec::new();
    if !unanchored_tokens.is_empty() {
        problems.push(format!(
            "UNANCHORED GAP — {} of {} sampled tokens have a Delta (not Snapshot) as their first \
             book update in {HOUR}: {unanchored_tokens:?}. This is the design doc's flagged \
             subtlety materializing on real data: a busy asset's earliest event visible in this \
             hour's file is a price_change with no preceding book snapshot in the ingested window.",
            unanchored_tokens.len(),
            busy_tokens.len()
        ));
    }

    if problems.is_empty() {
        println!(
            "pmxt smoke RESULT: seq/anchor design HOLDS on real data — every sampled token's \
             first book update in {HOUR} is a Snapshot, and seq is strictly increasing per asset."
        );
    } else {
        // Do NOT silently paper over this: report every issue found, clearly, so the controller
        // can decide on a fix rather than the smoke quietly passing over real-data surprises.
        panic!(
            "pmxt smoke RESULT: real-data validation surfaced {} issue(s) against {HOUR}:\n - {}",
            problems.len(),
            problems.join("\n - ")
        );
    }
}
