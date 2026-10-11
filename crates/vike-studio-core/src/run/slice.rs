//! The data slice a Run executes over, and the store catalog folds the slice picker lists.

use vike_data::{DataError, HistStore, SeriesId, StoreKindId, TsRange};

#[cfg(doc)]
use super::{RunError, load_slice_bars};

/// Which recorded series a [`DataSlice`] replays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SliceKind {
    /// `kind=bar` series at `interval`, replayed through `StrategyEngine::run` (the original path).
    #[default]
    Bars,
    /// The recorded `kind=quote`/`trade`/`book` series for these symbols, replayed through
    /// `vike_backtest::hist_replay::replay_ticks` (`interval` is unused).
    Ticks,
}

/// The data window a Run executes over.
///
/// `symbols` is a LIST (was a single `symbol`): `StrategyEngine::run`/`run_ticks` have always
/// accepted several series, and a cross-instrument strategy needs them together in one run. Use
/// [`DataSlice::bars`] for the single-symbol bar case — it is exactly the old struct literal.
#[derive(Debug, Clone, PartialEq)]
pub struct DataSlice {
    pub venue: String,
    /// One or more symbols, in the order the engine should register them.
    pub symbols: Vec<String>,
    /// Bar step (`"1m"`). Unused — and conventionally empty — for [`SliceKind::Ticks`].
    pub interval: String,
    pub range: TsRange,
    pub kind: SliceKind,
}

impl DataSlice {
    /// A single-symbol BAR slice — the original `DataSlice { venue, symbol, interval, range }`.
    pub fn bars(
        venue: impl Into<String>,
        symbol: impl Into<String>,
        interval: impl Into<String>,
        range: TsRange,
    ) -> Self {
        DataSlice {
            venue: venue.into(),
            symbols: vec![symbol.into()],
            interval: interval.into(),
            range,
            kind: SliceKind::Bars,
        }
    }

    /// A multi-symbol BAR slice. NB `StrategyEngine::new` asserts every symbol's bar series shares
    /// one length; [`load_slice_bars`] checks that itself and returns a [`RunError::Data`] rather
    /// than letting the assert panic a worker thread.
    pub fn multi_bars(
        venue: impl Into<String>,
        symbols: Vec<String>,
        interval: impl Into<String>,
        range: TsRange,
    ) -> Self {
        DataSlice {
            venue: venue.into(),
            symbols,
            interval: interval.into(),
            range,
            kind: SliceKind::Bars,
        }
    }

    /// A TICK slice over the store's recorded quote/trade/book series.
    pub fn ticks(venue: impl Into<String>, symbols: Vec<String>, range: TsRange) -> Self {
        DataSlice {
            venue: venue.into(),
            symbols,
            interval: String::new(),
            range,
            kind: SliceKind::Ticks,
        }
    }

    /// The first symbol (`""` for the degenerate empty slice) — for the many single-symbol call
    /// sites that used to read `slice.symbol`.
    pub fn symbol(&self) -> &str {
        self.symbols.first().map(String::as_str).unwrap_or("")
    }

    /// A compact human label, e.g. `binance · BTCUSDT · 1m` or `polymarket · TKN+1 · ticks`.
    pub fn label(&self) -> String {
        let sym = match self.symbols.len() {
            0 => "(no symbol)".to_string(),
            1 => self.symbols[0].clone(),
            n => format!("{}+{}", self.symbols[0], n - 1),
        };
        let tail = match self.kind {
            SliceKind::Bars => self.interval.clone(),
            SliceKind::Ticks => "ticks".to_string(),
        };
        format!("{} · {sym} · {tail}", self.venue)
    }
}

/// The `(venue, symbol, interval)` triples of every `kind=bar` series in the store, in
/// `list_series`'s stable sorted order (tick series — `interval == None` — are dropped). The ONE
/// place the "bars only, interval present" filter over [`HistStore::list_series`] lives —
/// the Studio's picker reaches it through [`series_lists`] rather than re-deriving the filter
/// (the retired `vike-mcp` `list_series` tool was the second caller; its successor,
/// `vike-cli mcp`, lists series over the datahub wire verb instead of a local store). Takes the
/// TRAIT (split-plane B12), so the Studio's picker enumerates a remote `RemoteHistStore` exactly
/// like a local `DataFusionHist`.
pub fn bar_series(store: &dyn HistStore) -> Result<Vec<(String, String, String)>, DataError> {
    Ok(bar_rows(&store.list_series()?))
}

/// [`bar_series`]'s filter, over an ALREADY-listed catalog — the pure half, so [`series_lists`]
/// can fold one `list_series` answer three ways instead of asking three times.
fn bar_rows(listed: &[SeriesId]) -> Vec<(String, String, String)> {
    listed
        .iter()
        .filter(|s| s.kind == StoreKindId::Bar)
        .filter_map(|s| s.interval.clone().map(|iv| (s.venue.clone(), s.symbol.clone(), iv)))
        .collect()
}

/// The store `kind=` values a tick slice can actually REPLAY — the lanes
/// `vike_backtest::hist_replay::replay_ticks` reads (`scan_quotes` / `scan_trades` /
/// `scan_book_updates`), one entry per lane, and the filter [`tick_series`] applies.
///
/// A named constant rather than an inline `matches!` because [`depth_only_series`] is the
/// complement of exactly this set, and the two must be derived from one list or the disclosure
/// silently stops covering a lane the replay later gains.
pub const REPLAYABLE_TICK_KINDS: [StoreKindId; 3] =
    [StoreKindId::Quote, StoreKindId::Trade, StoreKindId::Book];

/// The store `kind=` this module lists as RECORDED-but-not-replayable — see [`depth_only_series`]
/// for the argument, which is the whole reason the value has a name here at all.
pub const UNREPLAYABLE_TICK_KIND: StoreKindId = StoreKindId::Depth;

/// The DISTINCT `(venue, symbol)` pairs that have any REPLAYABLE tick series
/// ([`REPLAYABLE_TICK_KINDS`]), in `list_series`'s stable sorted order — the tick twin of
/// [`bar_series`], and what the Studio's slice picker lists under "ticks".
///
/// Deduplicated across kinds on purpose: `replay_ticks` scans all three kinds for a symbol and
/// merges them, so one picker row per `(venue, symbol)` is exactly one runnable slice. A symbol
/// recorded as quotes AND trades must not appear twice.
///
/// ⚠ **`kind=depth` is deliberately NOT admitted here, and its exclusion is REPORTED rather than
/// silent** — [`depth_only_series`] names the instruments it costs. The exclusion needs writing
/// down precisely because adding the arm would compile and run: `crates/vike-data/src/
/// store/store_kind.rs`'s `STORE_KINDS` marks `depth` a tick lane carrying the same `BookUpdate` rows
/// and the same codec as `book`, so the rows would drop straight into `merge_ticks`. Two
/// independent facts say no anyway:
///
/// - **The replay has no depth lane at all.** `vike_backtest::hist_replay::replay_ticks` reads
///   `scan_quotes`/`scan_trades`/`scan_book_updates` and never `scan_depth`, and its per-series
///   lane filter `vike_backtest::hist_replay::SeriesKind` has no depth variant to select one. A
///   depth-only row listed here would pick a slice that loads nothing and die as
///   [`RunError::Data`] — a broken arm, not a feature.
/// - **Folding depth into the book lane is the thing the store layout exists to prevent.** That
///   `STORE_KINDS` row states its own identity: depth is the CONFLATING L2 lane, a periodic full
///   snapshot with every intermediate book state discarded, kept a DIFFERENT series from `book`
///   on purpose because the path IS the disclosure. `crates/vike-data/src/rec/live_rec.rs`'s
///   `l2_snapshot` carries the consequence of ignoring that — a market-making backtest run over
///   teleporting depth reports fills it could never have got. Routing depth rows into the `books`
///   slot would hand a strategy conflated snapshots under the name of a lossless book, silently,
///   which is a worse defect than the one this doc exists to close.
pub fn tick_series(store: &dyn HistStore) -> Result<Vec<(String, String)>, DataError> {
    Ok(tick_rows(&store.list_series()?))
}

/// [`tick_series`]'s filter, over an ALREADY-listed catalog — the pure half (see [`bar_rows`]).
fn tick_rows(listed: &[SeriesId]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for s in listed {
        if !REPLAYABLE_TICK_KINDS.contains(&s.kind) {
            continue;
        }
        let pair = (s.venue.clone(), s.symbol.clone());
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

/// The DISTINCT `(venue, symbol)` pairs whose ONLY recorded tick data is
/// [`UNREPLAYABLE_TICK_KIND`] — present in the store, listed by [`HistStore::list_series`], and
/// not replayable by any slice this module can build.
///
/// This is the disclosure half of [`tick_series`]'s filter, and it exists because that filter's
/// omission used to be invisible. `subscribe_depth` is an independently subscribable lane on
/// several bridges, so an instrument recorded through it alone is a real store state — and it
/// appeared in NO Studio list, which a human reads as the store's complete runnable inventory.
/// "There is no such data" and "the data is there and cannot be replayed" then looked the same,
/// and only the second one has an action attached to it (record `book`/`trade`/`quote` too).
///
/// A pair that ALSO has a replayable lane is excluded: it is already a pickable tick row, and its
/// depth series adds nothing the picker can offer separately. So the returned set is exactly the
/// instruments [`tick_series`] costs, never a second opinion about ones it already lists.
///
/// Ordering and dedup follow [`tick_series`]: `list_series`'s stable sorted order, one entry per
/// `(venue, symbol)`.
pub fn depth_only_series(store: &dyn HistStore) -> Result<Vec<(String, String)>, DataError> {
    Ok(depth_only_rows(&store.list_series()?))
}

/// [`depth_only_series`]'s filter, over an ALREADY-listed catalog — the pure half (see
/// [`bar_rows`]).
fn depth_only_rows(listed: &[SeriesId]) -> Vec<(String, String)> {
    let mut depth: Vec<(String, String)> = Vec::new();
    let mut replayable: Vec<(String, String)> = Vec::new();
    for s in listed {
        let bucket = if s.kind == UNREPLAYABLE_TICK_KIND {
            &mut depth
        } else if REPLAYABLE_TICK_KINDS.contains(&s.kind) {
            &mut replayable
        } else {
            continue;
        };
        let pair = (s.venue.clone(), s.symbol.clone());
        if !bucket.contains(&pair) {
            bucket.push(pair);
        }
    }
    depth.retain(|pair| !replayable.contains(pair));
    depth
}

/// Every list the Studio's slice picker draws, folded from ONE [`HistStore::list_series`] call.
///
/// The three fields are exactly what [`bar_series`], [`tick_series`] and [`depth_only_series`]
/// return — same filters, same order, same dedup, because [`series_lists`] and those three
/// functions call the SAME pure folds ([`bar_rows`]/[`tick_rows`]/[`depth_only_rows`]). There is
/// no second reading of the catalog rules for the two shapes to drift apart in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesLists {
    /// `(venue, symbol, interval)` per `kind=bar` series — [`bar_series`]'s answer.
    pub bars: Vec<(String, String, String)>,
    /// `(venue, symbol)` per instrument with a replayable tick lane — [`tick_series`]'s answer.
    pub ticks: Vec<(String, String)>,
    /// `(venue, symbol)` per instrument recorded ONLY as `kind=depth` — [`depth_only_series`]'s
    /// answer, the disclosure half.
    pub depth_only: Vec<(String, String)>,
}

/// Fold the store's catalog into all three picker lists with a SINGLE [`HistStore::list_series`]
/// call.
///
/// The Studio's picker used to call [`bar_series`], then [`tick_series`], then
/// [`depth_only_series`] — three enumerations of one catalog, and on a `RemoteHistStore` (which is
/// connect-per-read since split-plane B12) three fresh TCP connects for one Refresh click. The
/// three filters are pure functions of the SAME list, so asking three times bought nothing but
/// latency.
///
/// It also makes an existing claim literally true rather than nearly true:
/// `crates/vike-studio/src/panes/picker.rs`'s picker — the sentence now lives on `SlicePicker::apply`, the
/// fold `SlicePicker::refresh` shares with the off-thread path — has always documented that "every filter folds the same
/// `list_series` call, so a mixed outcome — bars listed, ticks refused — is unreachable". With
/// three calls that held only because every impl in the tree answers the verb the same way twice
/// in a row; with one call it holds by construction.
///
/// It deliberately does NOT cache: this is a one-shot maintenance walk (open / Refresh), and a
/// cache would be a second answer that can disagree with the store.
pub fn series_lists(store: &dyn HistStore) -> Result<SeriesLists, DataError> {
    let listed = store.list_series()?;
    Ok(SeriesLists {
        bars: bar_rows(&listed),
        ticks: tick_rows(&listed),
        depth_only: depth_only_rows(&listed),
    })
}
