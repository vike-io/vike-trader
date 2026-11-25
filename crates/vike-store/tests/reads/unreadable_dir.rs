//! A directory the store cannot open is a failed read, never an empty series: `Path::exists` and
//! `Path::is_dir` answer `false` for a path they could not even look at, and every read here took
//! that `false` for "no such series".

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use vike_data::test_support::grouped_ticks::trade;
use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::Bar;
use vike_model::StoreKindId;

use crate::common::open;

const V: &str = "binance";
const S: &str = "BTCUSDT";
const I: &str = "1m";

/// Run `read` with `dir` at `mode`, restoring it before anything can fail so the temp dir can be
/// removed. `None` when the process reads through the mode (root, or a filesystem ignoring it): the
/// fixture proved nothing then, and the caller says so.
fn under_mode<T>(dir: &Path, mode: u32, read: impl FnOnce() -> T) -> Option<T> {
    assert!(dir.is_dir(), "the fixture's own path is wrong: {}", dir.display());
    let before = std::fs::metadata(dir).unwrap().permissions();
    std::fs::set_permissions(dir, Permissions::from_mode(mode)).unwrap();
    let denied = std::fs::metadata(dir.join("probe"))
        .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
    let out = read();
    std::fs::set_permissions(dir, before).unwrap();
    denied.then_some(out)
}

#[test]
fn a_bar_series_under_a_directory_that_cannot_be_opened_is_an_error() {
    let (tmp, store) = open();
    let bar = Bar::ohlcv(60_000, 1.0, 1.0, 1.0, 1.0, 1.0);
    store.append_bars(V, S, I, &[bar], Some("k")).unwrap();
    let all = TimestampRange::all();

    let venue = tmp.path().join("kind=bar").join(format!("venue={V}"));
    let answers = under_mode(&venue, 0o000, || {
        (
            store.load_bars(V, S, I, all).map(|rows| rows.len()),
            store.load_bars_head(V, S, I, all, 10).map(|rows| rows.len()),
            store.bar_edges(V, S, I, all).map(|edges| edges.rows),
        )
    });
    let Some((whole, head, edges)) = answers else {
        eprintln!("skipped: this process reads through a 0o000 directory");
        return;
    };
    assert!(whole.is_err(), "load_bars read a shut directory as an empty series: {whole:?}");
    assert!(head.is_err(), "load_bars_head read it as an empty series: {head:?}");
    assert!(edges.is_err(), "bar_edges read it as an empty series: {edges:?}");

    // The control: the same reads answer once the directory opens, and a series nobody ever wrote
    // is still the honest empty answer rather than an error.
    assert_eq!(store.load_bars(V, S, I, all).unwrap().len(), 1);
    assert_eq!(store.load_bars(V, "NOSUCH", I, all).unwrap().len(), 0);
    assert_eq!(store.load_bars_head(V, "NOSUCH", I, all, 10).unwrap().len(), 0);
    assert_eq!(store.bar_edges(V, "NOSUCH", I, all).unwrap().rows, 0);
}

/// `0o444` lets the venue directory be LISTED and nothing under it be reached, which is the one
/// shape where the group walk's `is_dir` was the thing answering `false`.
#[test]
fn a_tick_read_whose_directories_cannot_be_reached_is_an_error() {
    let (tmp, store) = open();
    store.append_trades(V, S, &[trade(S, 1_000, 1)], Some("own")).unwrap();
    store.append_trades_grouped(V, "fam", &[trade(S, 2_000, 2)], Some("group")).unwrap();
    let all = TimestampRange::all();
    let group = SeriesId::grouped(StoreKindId::Trade, V, "fam");

    let venue = tmp.path().join("kind=trade").join(format!("venue={V}"));
    let answers = under_mode(&venue, 0o444, || {
        (
            store.scan_trades(V, S, all).map(|rows| rows.len()),
            store.series_symbols(&group, all).map(|symbols| symbols.len()),
        )
    });
    let Some((rows, symbols)) = answers else {
        eprintln!("skipped: this process reads through a 0o444 directory");
        return;
    };
    assert!(rows.is_err(), "scan_trades read unreachable directories as nothing: {rows:?}");
    assert!(symbols.is_err(), "series_symbols read one as nothing: {symbols:?}");

    assert_eq!(store.scan_trades(V, S, all).unwrap().len(), 2, "both layouts, once it opens");
}
