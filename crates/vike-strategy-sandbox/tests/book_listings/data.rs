//! The book's FIXED data, read from `content/book/data/` and served to a listing from a store of its own:
//! the synthetic test tape, plus ONLY the files the listing's recipe DECLARES, `book_data = ["<file>", …]`
//! (paths under `content/book/data/`; absent = none). An undeclared file in the folder is never opened,
//! so a leftover on a lane's checkout cannot change a page (on 2026-11-17 one did, when every file was
//! read), and a declared file that is missing is refused, naming it. A declared `<name>.jsonl` at the top
//! of the folder is BARS (the rows `vike-cli data history export --format jsonl` writes, each under its own
//! venue, symbol and interval; a funding series is interval `funding`), and a declared
//! `order_book/<name>.jsonl` is recorded BOOK events (the rows of `--kind book` or `--kind depth`: venue,
//! symbol, timestamp, sequence, `event`, tick size, `bids` and `asks` as `[price, quantity]`). A book
//! file's name says its lane, `_book_` or `_depth_`, because a row does not:
//! `StudyContext::book_updates` reads the first, `StudyContext::depth` the second.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use vike_data::{
    ExecFillRow, ExecOrderRow, HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, SymbolProperties};

/// `(venue, symbol)`.
type Market = (String, String);

/// `(venue, symbol, interval)` and its bars, in file order.
type BarSeries = ((String, String, String), Vec<Bar>);

/// One JSON object of a `.jsonl` file.
type Row = serde_json::Map<String, serde_json::Value>;

/// What one listing declared of `content/book/data/`: bar series by `(venue, symbol, interval)`, and book
/// events by market for each lane, every series in declaration and file order.
#[derive(Default)]
pub(crate) struct BookData {
    bars: Vec<BarSeries>,
    book: BTreeMap<Market, Vec<L2BookUpdate>>,
    depth: BTreeMap<Market, Vec<L2BookUpdate>>,
}

impl BookData {
    /// The files of `dir` that `recipe`'s `book_data` names, and no other. A declaration that is not a
    /// list of file names, a file named twice or in neither form, and a declared file that is missing or
    /// does not read are each refused in words naming it.
    pub(crate) fn read(dir: &Path, recipe: &toml::Value) -> Result<Self, String> {
        let declared: Vec<&str> = match recipe.get("book_data") {
            None => Vec::new(),
            Some(list) => list
                .as_array()
                .and_then(|files| files.iter().map(toml::Value::as_str).collect())
                .ok_or("`book_data` is not a list of file names under content/book/data/")?,
        };
        let mut data = BookData::default();
        for (index, file) in declared.iter().enumerate() {
            let path = dir.join(file);
            let refused = |why: String| format!("{}: {why}", path.display());
            if declared[..index].contains(file) {
                return Err(refused("declared twice in `book_data`".to_string()));
            }
            // `None` is a bar file at the top, `Some(name)` a book file in `order_book/`.
            let book_file = match file.split_once('/') {
                None if file.ends_with(".jsonl") => None,
                Some(("order_book", name)) if name.ends_with(".jsonl") && !name.contains('/') => {
                    Some(name)
                }
                _ => {
                    return Err(refused(
                        "a declared file is `<name>.jsonl` (bars) or `order_book/<name>.jsonl` \
                         (book events)"
                            .to_string(),
                    ));
                }
            };
            let text = std::fs::read_to_string(&path).map_err(|e| {
                refused(format!("declared in `book_data`, and cannot be read: {e}"))
            })?;
            let Some(name) = book_file else {
                data.bars.extend(bar_series(&text).map_err(refused)?);
                continue;
            };
            let lane = match (name.contains("_book_"), name.contains("_depth_")) {
                (true, false) => &mut data.book,
                (false, true) => &mut data.depth,
                _ => {
                    return Err(refused(
                        "the name must say its lane, `_book_` or `_depth_`, and only one"
                            .to_string(),
                    ));
                }
            };
            for (market, events) in book_events(&text).map_err(refused)? {
                lane.entry(market).or_default().extend(events);
            }
        }
        Ok(data)
    }

    /// One line per series, for the run's log.
    pub(crate) fn describe(&self) -> Vec<String> {
        let bars =
            self.bars.iter().map(|((v, s, i), b)| format!("{v}:{s}:{i}, {} bar(s)", b.len()));
        let lane = |word: &'static str, map: &BTreeMap<Market, Vec<L2BookUpdate>>| {
            map.iter()
                .map(move |((v, s), e)| format!("{v}:{s} {word}, {} event(s)", e.len()))
                .collect::<Vec<_>>()
        };
        bars.chain(lane("book", &self.book)).chain(lane("depth", &self.depth)).collect()
    }

    /// A store of its own holding the synthetic test tape and all of this data, for one listing.
    pub(crate) fn store(&self) -> Arc<dyn HistoryStore + Send + Sync> {
        let bars = HistoryStoreMemory::new();
        vike_data::demo::seed(&bars).expect("the in-memory store takes the test tape");
        for ((venue, symbol, interval), series) in &self.bars {
            let kept =
                bars.append_bars(venue, symbol, interval, series, None).unwrap_or_else(|e| {
                    panic!("the sample's {venue}:{symbol}:{interval} is refused by the store: {e}")
                });
            assert_eq!(kept, series.len(), "the sample's {venue}:{symbol}:{interval} lost rows");
        }
        Arc::new(BookStore { bars, book: self.book.clone(), depth: self.depth.clone() })
    }
}

/// The JSON objects of `text`, one per non-blank line, each with its line number.
fn rows(text: &str) -> impl Iterator<Item = Result<(usize, Row), String>> {
    text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()).map(|(index, line)| {
        serde_json::from_str(line)
            .map(|row| (index + 1, row))
            .map_err(|e| format!("line {}: not a JSON object: {e}", index + 1))
    })
}

/// Take the text value of `key` out of `row`.
fn take(row: &mut Row, key: &str, line: usize) -> Result<String, String> {
    match row.remove(key) {
        Some(serde_json::Value::String(text)) => Ok(text),
        _ => Err(format!("line {line}: no text `{key}`")),
    }
}

/// Bar rows grouped by `(venue, symbol, interval)` in the order each first appears; the rest of a row is
/// the stored [`Bar`] (`timestamp_utc` is ignored), so a funding row keeps its `funding` rate.
fn bar_series(text: &str) -> Result<Vec<BarSeries>, String> {
    let mut out: Vec<BarSeries> = Vec::new();
    for row in rows(text) {
        let (line, mut row) = row?;
        let key = (
            take(&mut row, "venue", line)?,
            take(&mut row, "symbol", line)?,
            take(&mut row, "interval", line)?,
        );
        let bar: Bar = serde_json::from_value(serde_json::Value::Object(row))
            .map_err(|e| format!("line {line}: not a stored bar: {e}"))?;
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, bars)) => bars.push(bar),
            None => out.push((key, vec![bar])),
        }
    }
    Ok(out)
}

/// Book event rows grouped by `(venue, symbol)`; the rest of a row is the recorded [`L2BookUpdate`], its
/// kind under the export's word `event`.
fn book_events(text: &str) -> Result<BTreeMap<Market, Vec<L2BookUpdate>>, String> {
    let mut out: BTreeMap<Market, Vec<L2BookUpdate>> = BTreeMap::new();
    for row in rows(text) {
        let (line, mut row) = row?;
        let market = (take(&mut row, "venue", line)?, take(&mut row, "symbol", line)?);
        let kind = row.remove("event").ok_or_else(|| format!("line {line}: no `event`"))?;
        row.insert("kind".to_string(), kind);
        let event: L2BookUpdate = serde_json::from_value(serde_json::Value::Object(row))
            .map_err(|e| format!("line {line}: not a recorded book event: {e}"))?;
        out.entry(market).or_default().push(event);
    }
    Ok(out)
}

/// The listings' store: bars and the rest from the in-memory double, the book and depth lanes from the
/// sample (the double holds no book). A study only reads, so its writes are inert.
struct BookStore {
    bars: HistoryStoreMemory,
    book: BTreeMap<Market, Vec<L2BookUpdate>>,
    depth: BTreeMap<Market, Vec<L2BookUpdate>>,
}

fn events_in(
    map: &BTreeMap<Market, Vec<L2BookUpdate>>,
    venue: &str,
    symbol: &str,
    range: TimestampRange,
) -> Vec<L2BookUpdate> {
    let inside = |t: i64| range.start.is_none_or(|s| t >= s) && range.end.is_none_or(|e| t <= e);
    map.get(&(venue.to_string(), symbol.to_string()))
        .map(|events| events.iter().filter(|e| inside(e.timestamp)).cloned().collect())
        .unwrap_or_default()
}

impl HistoryStore for BookStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.bars.load_bars(venue, symbol, interval, range)
    }
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.bars.scan_quotes(venue, symbol, range)
    }
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.bars.scan_trades(venue, symbol, range)
    }
    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        Ok(events_in(&self.book, venue, symbol, range))
    }
    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        Ok(events_in(&self.depth, venue, symbol, range))
    }
    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        self.bars.scan_symbol_properties(venue, symbol, range)
    }
    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        self.bars.scan_equity(venue, symbol, range)
    }
    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        self.bars.scan_exec_fills(venue, symbol)
    }
    fn scan_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        self.bars.scan_exec_orders(venue, symbol)
    }
    vike_data::hist_store_stubs!(inert: writes);
}

/// The reader's own check: bar series kept apart and a funding rate kept; book events by market and kind;
/// each lane served on its own verb, inside the range; bad rows refused.
pub(crate) fn self_check() {
    let bars = "{\"venue\":\"v\",\"symbol\":\"S\",\"interval\":\"1h\",\"timestamp\":0,\
                \"timestamp_utc\":\"1970-01-01T00:00:00Z\",\"open\":1,\"high\":2,\"low\":0.5,\
                \"close\":1.5,\"volume\":3}\n\n\
                {\"venue\":\"v\",\"symbol\":\"S\",\"interval\":\"funding\",\"timestamp\":0,\
                \"open\":0,\"high\":0,\"low\":0,\"close\":0,\"volume\":0,\"funding\":0.0001}\n\
                {\"venue\":\"v\",\"symbol\":\"S\",\"interval\":\"1h\",\"timestamp\":3600000,\
                \"open\":1.5,\"high\":2,\"low\":1,\"close\":2,\"volume\":1}\n";
    let series = bar_series(bars).expect("a well-formed bar sample reads");
    assert_eq!(series.len(), 2, "{series:?}");
    assert_eq!((series[0].0.2.as_str(), series[0].1.len()), ("1h", 2));
    assert_eq!(series[1].1[0].funding, Some(0.0001));
    assert!(bar_series("{\"venue\":\"v\",\"symbol\":\"S\",\"timestamp\":0}").is_err());
    assert!(
        bar_series("{\"venue\":\"v\",\"symbol\":\"S\",\"interval\":\"1h\",\"open\":null}").is_err()
    );

    let book = "{\"venue\":\"v\",\"symbol\":\"S\",\"timestamp\":10,\"timestamp_utc\":\"x\",\"local_timestamp\":0,\
                \"sequence\":1,\"event\":\"Snapshot\",\"tick_size\":0.5,\"bids\":[[99.5,2.0]],\"asks\":[[100.0,1.0],[100.5,3.0]]}\n\
                {\"venue\":\"v\",\"symbol\":\"S\",\"timestamp\":20,\"local_timestamp\":0,\
                \"sequence\":2,\"event\":\"Delta\",\"tick_size\":0.5,\"bids\":[],\"asks\":[[100.0,0.0]]}\n";
    let events = book_events(book).expect("a well-formed book sample reads");
    let chain = &events[&("v".to_string(), "S".to_string())];
    assert_eq!(chain.len(), 2);
    assert_eq!((chain[0].sequence, chain[0].asks.len(), chain[1].asks[0].qty), (1, 2, 0.0));
    assert!(book_events("{\"venue\":\"v\",\"symbol\":\"S\",\"timestamp\":1,\"sequence\":1,\"tick_size\":1,\"bids\":[],\"asks\":[]}").is_err());
    assert!(book_events("{\"venue\":\"v\",\"symbol\":\"S\",\"timestamp\":1,\"sequence\":1,\"event\":\"Nope\",\"tick_size\":1,\"bids\":[],\"asks\":[]}").is_err());

    // Only the declared files: a leftover planted beside them, in either folder and for the same markets,
    // is never opened, so the store (a page's only data) is the same with it as without it.
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("book-data-self-check");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("order_book")).expect("a scratch folder");
    let write =
        |file: &str, text: &str| std::fs::write(dir.join(file), text).expect("a scratch file");
    let recipe = |text: &str| -> toml::Value { toml::from_str(text).expect("a recipe") };
    let read = |declaration: &str| BookData::read(&dir, &recipe(declaration));
    write("v_S_1h.jsonl", bars);
    write("order_book/v_S_depth_a.jsonl", book);
    let declared = "book_data = [\"v_S_1h.jsonl\", \"order_book/v_S_depth_a.jsonl\"]";
    let clean = read(declared).expect("the declared files read").describe();
    assert_eq!(clean.len(), 3, "two bar series and one depth market: {clean:?}");
    write("v_S_1h_leftover.jsonl", bars);
    write("order_book/v_S_depth_leftover.jsonl", book);
    let planted = read(declared).expect("the declared files still read").describe();
    assert_eq!(planted, clean, "an undeclared file changed what a listing reads");
    assert!(read("").expect("no declaration").describe().is_empty(), "absent = no file");
    let refusal = |declaration: &str| read(declaration).err().unwrap_or_default();
    assert!(refusal("book_data = [\"gone.jsonl\"]").contains("gone.jsonl: declared"));
    assert!(refusal("book_data = [\"v_S_1h.jsonl\", \"v_S_1h.jsonl\"]").contains("twice"));
    assert!(refusal("book_data = [\"order_book/README.md\"]").contains("`<name>.jsonl`"));
    assert!(refusal("book_data = [\"../v_S_1h.jsonl\"]").contains("`<name>.jsonl`"));
    assert!(refusal("book_data = \"v_S_1h.jsonl\"").contains("not a list"));
    let _ = std::fs::remove_dir_all(&dir);

    let data = BookData { bars: Vec::new(), book: BTreeMap::new(), depth: events };
    let store = data.store();
    let all = TimestampRange::all();
    let count =
        |events: Result<Vec<L2BookUpdate>, HistoryStoreError>| events.expect("a read").len();
    assert_eq!(count(store.scan_depth("v", "S", all)), 2, "the depth lane serves its events");
    assert_eq!(
        count(store.scan_depth("v", "S", TimestampRange::of(15, 30))),
        1,
        "inside the range"
    );
    assert_eq!(count(store.scan_book_updates("v", "S", all)), 0, "the book lane holds none");
    let tape = store.load_bars("demo", "BTCUSDT", "1h", all).expect("the test tape").len();
    assert!(tape > 0, "the bars are the test tape's");
}
