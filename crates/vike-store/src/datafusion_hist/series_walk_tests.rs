//! `SeriesWalkMemo` on a real store: the held walk answers while nothing it read has moved, every way a series
//! appears, disappears or is renamed moves its key (whichever handle wrote it), a write inside a leaf walks nothing,
//! and every answer is the answer a fresh walk gives.

use vike_data::store::hist::HistoryStore;
use vike_marketdata::test_support::{flat_bar_unit_volume, quote};

use super::*;
use crate::HistoryStoreDataFusion;

const T0: i64 = 1_700_000_000_000;
const DAY: i64 = 86_400_000;

/// Every folder under `root`, the root too, stamped an hour old: a store nobody wrote in the last [`SETTLED`].
fn age(root: &Path) {
    stamp(root, SystemTime::now() - Duration::from_secs(3_600));
}

/// Every folder under `root` stamped NOW: young at the next walk however long the box took to get here (a
/// loaded CI runner once spent more than [`SETTLED`] between a write and its second ask, so the folder had aged).
fn young(root: &Path) {
    stamp(root, SystemTime::now());
}

/// Every folder under `root`, the root too, stamped `at`.
fn stamp(root: &Path, at: SystemTime) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a listing") {
            let entry = entry.expect("an entry");
            if entry.file_type().expect("a type").is_dir() {
                stack.push(entry.path());
            }
        }
        std::fs::File::open(&dir).and_then(|f| f.set_modified(at)).expect("a folder's time");
    }
}

/// The walk the memo saves, taken fresh: what every answer must equal (the memo off).
fn fresh(store: &HistoryStoreDataFusion) -> Vec<SeriesId> {
    let mut dirs = Vec::new();
    find_manifest_series_dirs(store.root(), &mut dirs, &mut WalkKey::default()).expect("a walk");
    let mut ids: Vec<SeriesId> =
        dirs.iter().filter_map(|d| parse_series_id(store.root(), d)).collect();
    ids.sort();
    ids
}

fn walks(store: &HistoryStoreDataFusion) -> u64 {
    store.series_walk_memo.walks.load(Ordering::Relaxed)
}

/// After a write that adds, removes or renames a series: the next ask WALKS (the key moved) and answers `want`, the
/// fresh walk's answer; and once the tree is old, an ask walks and keeps it, and the one after walks nothing.
fn walked_then_held(store: &HistoryStoreDataFusion, want: &[SeriesId]) {
    let mut want = want.to_vec();
    want.sort();
    let before = walks(store);
    assert_eq!(store.list_series().expect("listed"), want, "the write is answered");
    assert_eq!(walks(store), before + 1, "the write moved the key: a walk");
    assert_eq!(fresh(store), want, "the answer a fresh walk gives");
    age(store.root());
    assert_eq!(store.list_series().expect("listed"), want);
    let kept = walks(store);
    assert_eq!(store.list_series().expect("listed"), want, "held");
    assert_eq!(walks(store), kept, "a held answer walks nothing");
}

/// Every writer kind, each through the store's own verbs: an append (an import, a backfill, a CSV lane, the
/// recorder all write through one), a `source=` lane, another handle on the same root (the store tool or a collector
/// in another process), a rename, a delete, a manifest lost by hand with its leaf kept, and a first manifest
/// published into a folder that had none (a creation that crashed after its lock, then finished; a repair).
#[test]
fn every_way_a_series_comes_or_goes_moves_the_key() {
    let dir = tempfile::tempdir().expect("a scratch store");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open");
    let bars = |ts: i64| [flat_bar_unit_volume(ts, 10.0)];
    let bar = |venue: &str, symbol: &str| {
        SeriesId::per_symbol(StoreKindId::Bar, venue, symbol, Some("1m".to_string()))
    };
    let quotes = SeriesId::per_symbol(StoreKindId::Quote, "binance", "BTCUSDT", None);
    let lane = SeriesId::per_symbol(StoreKindId::Quote, "binance", "ETHUSDT", None)
        .with_source("recorder");

    assert!(store.list_series().expect("listed").is_empty());
    store.append_bars("binance", "BTCUSDT", "1m", &bars(T0), Some("a")).expect("a new series");
    walked_then_held(&store, &[bar("binance", "BTCUSDT")]);

    // A part on a new day: a `date=` folder inside the leaf, which moves no key and changes no answer.
    let held = walks(&store);
    store.append_bars("binance", "BTCUSDT", "1m", &bars(T0 + DAY), Some("b")).expect("a new day");
    assert_eq!(store.list_series().expect("listed"), fresh(&store));
    assert_eq!(walks(&store), held, "a write inside a leaf walks nothing");

    store.append_quotes("binance", "BTCUSDT", &[quote(T0, 1.0, 2.0)], Some("q")).expect("quotes");
    walked_then_held(&store, &[bar("binance", "BTCUSDT"), quotes.clone()]);

    store
        .append_quotes_sourced("binance", "recorder", "ETHUSDT", &[quote(T0, 1.0, 2.0)], None)
        .expect("a lane");
    let mut want = vec![bar("binance", "BTCUSDT"), quotes.clone(), lane.clone()];
    walked_then_held(&store, &want);

    let other = HistoryStoreDataFusion::open(dir.path()).expect("a second handle");
    other.append_bars("okx", "BTC-USDT", "1m", &bars(T0), Some("o")).expect("another writer");
    want.push(bar("okx", "BTC-USDT"));
    walked_then_held(&store, &want);

    store.rename_series(&bar("binance", "BTCUSDT"), "XBTUSDT").expect("a rename");
    want.retain(|id| *id != bar("binance", "BTCUSDT"));
    want.push(bar("binance", "XBTUSDT"));
    walked_then_held(&store, &want);

    other.delete_series(&bar("okx", "BTC-USDT")).expect("a delete");
    want.retain(|id| *id != bar("okx", "BTC-USDT"));
    walked_then_held(&store, &want);

    std::fs::remove_file(store.series_dir_of(&quotes).join(super::super::manifest::MANIFEST))
        .expect("a manifest lost by hand");
    want.retain(|id| *id != quotes);
    walked_then_held(&store, &want);

    let pending = bar("bybit", "SOLUSDT");
    std::fs::create_dir_all(store.series_dir_of(&pending)).expect("a leaf with no manifest yet");
    walked_then_held(&store, &want);
    store.append_bars("bybit", "SOLUSDT", "1m", &bars(T0), Some("s")).expect("its first manifest");
    want.push(pending);
    walked_then_held(&store, &want);
}

/// A walk that read a folder younger than [`SETTLED`] answers and is NOT kept: two changes inside one tick of the
/// filesystem's clock would leave that folder's time as the walk read it.
#[test]
fn a_walk_over_a_young_folder_is_not_kept() {
    let dir = tempfile::tempdir().expect("a scratch store");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open");
    let bars = [flat_bar_unit_volume(T0, 10.0)];
    store.append_bars("binance", "BTCUSDT", "1m", &bars, Some("a")).expect("a series");
    let before = walks(&store);
    young(dir.path());
    assert_eq!(store.list_series().expect("listed").len(), 1);
    // ⚠ Stamped once only: a second stamp would move the walk's key and make the second walk vacuous.
    assert_eq!(store.list_series().expect("listed").len(), 1);
    assert_eq!(walks(&store), before + 2, "young folders: every ask walks");
    store.append_bars("binance", "ETHUSDT", "1m", &bars, Some("b")).expect("a second series");
    assert_eq!(store.list_series().expect("listed").len(), 2, "answered at once");
    assert_eq!(walks(&store), before + 3);
}
