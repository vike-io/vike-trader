//! The `source=` path segment, identity only.

use std::path::{Path, PathBuf};

use vike_data::{DataFusionHist, HistStore, SeriesId};

use crate::common::{bar, bars_id, qt};

// ============================ the `source=` dimension — IDENTITY ONLY ============================
//
// Stages 0-3 of `docs/superpowers/specs/2026-09-07-store-source-dimension-design.md`, accepted by
// the owner 2026-09-22. They add the fifth identity dimension and NOTHING ELSE: no verb takes a
// lane, no producer is scoped, `for_source` does not exist, and the one-leaf-or-REFUSE read rule is
// not built. So **nothing in this tree can write a `source=` leaf**, and these tests reach one the
// only way available — by MOVING a written leaf into place, which is precisely the rename
// `split_series_by_source` will perform when stage 6 is authorised.
//
// What they prove is the identity half end to end: a leaf carrying the segment is WALKED, PARSED
// back into an id that carries the lane, and addressed again through every `SeriesId`-taking verb.
// That is what makes `series_coverage`, `series_gaps`, `delete_series`, `rebuild_series_manifest`,
// compaction and retention correct for a sourced leaf with no signature change anywhere.

/// Rehouse a written per-symbol leaf under a `source=` lane, returning its new directory.
///
/// `kind=k/venue=v/symbol=s[/interval=i]` becomes
/// `kind=k/venue=v/source=<lane>/symbol=s[/interval=i]`. Spelled with path joins rather than
/// through the store, because the store deliberately offers no way to do it (see the section
/// header).
fn rehouse_under_source(root: &Path, id: &SeriesId, lane: &str) -> PathBuf {
    let venue = root.join(format!("kind={}", id.kind)).join(format!("venue={}", id.venue));
    let mut from = venue.join(format!("symbol={}", id.symbol));
    let mut to = venue.join(format!("source={lane}")).join(format!("symbol={}", id.symbol));
    if let Some(iv) = &id.interval {
        from = from.join(format!("interval={iv}"));
        to = to.join(format!("interval={iv}"));
    }
    std::fs::create_dir_all(to.parent().expect("a source= parent")).unwrap();
    std::fs::rename(&from, &to).unwrap();
    to
}

/// **The POSITION pin.** `source=` sits directly under `venue=` and ABOVE `symbol=`, and the
/// placement is forced rather than chosen: `find_manifest_series_dirs` stops descending at the
/// first `_manifest.json`, so a `source=` segment BELOW the leaf would be hidden by the legacy
/// sourceless series above it — never listed, never compacted, never pruned, never shown.
///
/// ⚠ **Read the two halves apart, because the first one cannot fail on its own.** The segment-order
/// assertion reads a path THIS FILE built, so it states the contract and tests nothing — MEASURED:
/// with `series_dir` mutated to append `source=` BELOW the leaf, that assertion still passed. What
/// puts the BUILDER on the hook is the `series_coverage` read at the end: it resolves through
/// `series_dir_of`, so a builder that places the segment anywhere else addresses a directory that
/// does not exist, `read_manifest` answers EMPTY for a missing dir rather than failing, and the
/// coverage comes back as zero rows. The parser cannot be the witness here either — it is
/// ORDER-AGNOSTIC by construction and parses either layout happily.
#[test]
fn the_source_segment_sits_directly_under_venue() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(0, 1.0, None)], Some("k1")).unwrap();
    let id = bars_id();
    let leaf = rehouse_under_source(dir.path(), &id, "venue");

    let rel = leaf.strip_prefix(dir.path()).unwrap();
    let segs: Vec<String> =
        rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    assert_eq!(
        segs,
        vec!["kind=bar", "venue=binance", "source=venue", "symbol=BTCUSDT", "interval=1m"],
        "the order is the CONTRACT — this half states it; the coverage read below tests it"
    );

    // The store finds that leaf and reads its lane back off the path.
    let listed = store.list_series().unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0], id.clone().with_source("venue"));
    // The ANTI-VACUITY control: the same id WITHOUT the lane is a different series, and it is gone.
    assert_ne!(listed[0], id);

    // ...and THIS is the half that bites: `series_coverage` takes a `SeriesId` and resolves it
    // through `series_dir_of`, so it reads the row above only if the builder puts `source=` exactly
    // where this test's own path does.
    assert_eq!(
        store.series_coverage(&listed[0]).unwrap().rows,
        1,
        "the builder addressed a different directory than the one holding the rows"
    );
}

/// **The both-layouts walk.** A legacy sourceless leaf and a sourced sibling under ONE `venue=` are
/// BOTH returned. This is the regression test for the invisibility the placement avoids.
#[test]
fn a_sourced_leaf_and_its_legacy_sibling_are_both_walked() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(0, 1.0, None)], Some("k1")).unwrap();
    rehouse_under_source(dir.path(), &bars_id(), "vikearchive");
    // A second, sourceless copy of the SAME series, written the ordinary way after the move.
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(60_000, 2.0, None)], Some("k2")).unwrap();

    let listed = store.list_series().unwrap();
    assert_eq!(
        listed,
        vec![bars_id(), bars_id().with_source("vikearchive")],
        "both leaves are walked, and they are two DIFFERENT series"
    );
    // Each addresses its OWN rows through the id — the whole point of the dimension, and the thing
    // a `source` COLUMN could not have given: two producers would still share one leaf, one
    // manifest, one commit log and one lock.
    assert_eq!(store.series_coverage(&listed[0]).unwrap().rows, 1);
    assert_eq!(store.series_coverage(&listed[1]).unwrap().rows, 1);
    assert_ne!(
        store.series_coverage(&listed[0]).unwrap().first_ts,
        store.series_coverage(&listed[1]).unwrap().first_ts,
        "the two coverages must come from different leaves, or this test proves nothing"
    );
}

/// **Every `SeriesId`-taking verb addresses the sourced leaf**, because they all resolve through
/// `series_dir_of`. `delete_series` is the sharp end: it is a `remove_dir_all`, so a wrong path
/// here either deletes the wrong series or silently deletes nothing and returns `Ok`.
#[test]
fn deleting_one_lane_leaves_the_other_intact() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(0, 1.0, None)], Some("k1")).unwrap();
    rehouse_under_source(dir.path(), &bars_id(), "vikearchive");
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(60_000, 2.0, None)], Some("k2")).unwrap();

    let sourced = bars_id().with_source("vikearchive");
    store.delete_series(&sourced).unwrap();
    assert_eq!(store.list_series().unwrap(), vec![bars_id()], "only the lane named was removed");
    assert_eq!(store.series_coverage(&bars_id()).unwrap().rows, 1, "the survivor still reads");

    // ...and the inverse, so this is not "delete_series removes whatever it finds": the lane that
    // no longer exists is a no-op rather than a hit on the sourceless leaf beside it.
    store.delete_series(&sourced).unwrap();
    assert_eq!(store.list_series().unwrap(), vec![bars_id()]);
}

/// **BYTE-IDENTICAL, stated as a test.** A store written entirely through the verbs contains NO
/// `source=` directory and every id it lists is sourceless — the claim these stages rest on, which
/// is otherwise only an argument about call sites.
#[test]
fn nothing_this_tree_writes_creates_a_source_segment() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(0, 1.0, None)], Some("k1")).unwrap();
    store.append_quotes("binance", "BTCUSDT", &[qt(1, 0.5)], Some("q1")).unwrap();
    let mut grouped = qt(2, 0.6);
    grouped.symbol = "AAA".into(); // a grouped part tells rows apart by this column and refuses an empty one
    store.append_quotes_grouped("polymarket", "btc-5m", &[grouped], Some("g1")).unwrap();

    let listed = store.list_series().unwrap();
    // Anti-vacuity: an empty store would satisfy every assertion below.
    assert_eq!(listed.len(), 3, "{listed:?}");
    for id in &listed {
        assert_eq!(id.source, None, "{id:?} was written by a verb that names no lane");
    }
    let mut dirs = Vec::new();
    walk_dirs(dir.path(), &mut dirs);
    assert!(dirs.len() >= 6, "the directory walk collapsed to {dirs:?}");
    let sourced: Vec<&PathBuf> = dirs
        .iter()
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("source="))
        })
        .collect();
    assert!(sourced.is_empty(), "a write verb created a `source=` directory: {sourced:?}");
}

/// Every directory under `root`, recursively — the test-side twin of the store's own walk, spelled
/// here so the assertion above reads the REAL tree rather than the store's view of it.
fn walk_dirs(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            out.push(e.path());
            walk_dirs(&e.path(), out);
        }
    }
}
