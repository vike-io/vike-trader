//! [`PipeHistStore`]: the sandboxed child's history, read from the parent over the protocol channel.
//!
//! A READ-ONLY `HistoryStore`: each read verb on the run path crosses as one [`StoreRead`] and comes back as
//! the parent store's own answer (rows, value or `HistoryStoreError`, variant for variant), so the run inside the
//! child sees what it would have seen in the hub. Every other verb, writes and deletes included, is
//! refused here and never crosses (`Unsupported`, naming the sandbox). The defaults the trait builds on
//! the forwarded verbs (`bar_edges`, `load_bars_head`, the `_capped` reads) stay defaults. The kept
//! instrument rows (`HistoryStore::kept_instruments`) never cross either: they are the ones the parent sent in
//! the start message, the rows its own run would read.
//!
//! ⚠ **The answers are CACHED by request for the child's life** (one backtest request): a sweep's points
//! re-read their series per point (`SearchEvaluatorStore` does), and each would cross the pipe again. The cache
//! keeps an answer's ENCODED frames, not its rows: the first read decodes once and keeps the bodies it
//! read (no copy), and a repeat decodes them again, so no read pays a clone of a million bars. A failed
//! read is not cached.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use vike_backtesthub_client::proto::Response;
use vike_data::{
    CohortRow, ExecFillRow, ExecOrderRow, HistoryStore, HistoryStoreError, PerpMetricRow,
    SeriesCoverage, SeriesId, TimestampRange,
};
use vike_hub_protocol::frame::read_frame_raw;
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, SymbolProperties};

use super::wire::{
    BarColumns, ChildFrame, ReadAnswerFrame, StoreRead, WireDataError, write_message,
};

/// The protocol channel's two halves, used by one reader at a time (the protocol is positional).
struct Channel {
    input: Box<dyn Read + Send>,
    output: Box<dyn Write + Send>,
}

/// See the module doc.
pub(crate) struct PipeHistStore {
    channel: Mutex<Channel>,
    // shortcut: unbounded for the one request a child serves (the encoded bodies of every distinct read).
    // Bound it if a measured run runs out of memory.
    cache: Mutex<HashMap<String, Arc<Vec<Vec<u8>>>>>,
    /// The kept instrument rows the parent sent in the start message (`super::wire::ChildRequest::kept`), what
    /// [`HistoryStore::kept_instruments`] answers: the child reads no folder and asks nothing more.
    kept: Result<Option<Vec<vike_model::Instrument>>, WireDataError>,
}

/// The words of a frame that is not the answer part it should be.
fn garbled(e: impl std::fmt::Display) -> HistoryStoreError {
    HistoryStoreError::Io(format!("the strategy sandbox read an answer it cannot decode: {e}"))
}

/// One answer frame's body, decoded (a [`BarColumns`] part borrows its columns from `body`).
fn frame_of<'a, P: Deserialize<'a>>(
    body: &'a [u8],
) -> Result<ReadAnswerFrame<P>, HistoryStoreError> {
    rmp_serde::from_slice(body).map_err(garbled)
}

/// The words of a lost channel: the parent is gone or cut the child off.
fn lost(e: std::io::Error) -> HistoryStoreError {
    HistoryStoreError::Io(format!("the strategy sandbox lost the channel to its parent: {e}"))
}

/// Every verb this store does not serve.
fn not_served(verb: &str) -> HistoryStoreError {
    HistoryStoreError::Unsupported(format!(
        "{verb} is not served to a strategy sandbox, which reads history only"
    ))
}

impl PipeHistStore {
    pub(crate) fn new(
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
        kept: Result<Option<Vec<vike_model::Instrument>>, WireDataError>,
    ) -> Self {
        Self {
            channel: Mutex::new(Channel { input, output }),
            cache: Mutex::new(HashMap::new()),
            kept,
        }
    }

    /// Write the finished answer: the last frame of the child.
    pub(crate) fn send_answer(&self, response: Response) -> std::io::Result<()> {
        let mut channel = self.channel.lock().unwrap_or_else(PoisonError::into_inner);
        write_message(&mut channel.output, &ChildFrame::Answer(Box::new(response)))
    }

    /// `read`'s answer, each encoded `Rows` part body handed to `each` in order: from the cache, or read
    /// from the parent until `Done` and cached (a `Failed` is the store's error, not cached). A body read
    /// from the parent is decoded AS IT ARRIVES, so the child decodes one part while the parent encodes
    /// the next. A part `each` refuses is the answer's error, returned once the rest is read through
    /// `Done` (the channel stays in step) and not cached.
    fn parts(
        &self,
        read: StoreRead,
        mut each: impl FnMut(&[u8]) -> Result<(), HistoryStoreError>,
    ) -> Result<(), HistoryStoreError> {
        let key = serde_json::to_string(&read).map_err(|e| HistoryStoreError::Io(e.to_string()))?;
        let cached = self.cache.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned();
        if let Some(bodies) = cached {
            return bodies.iter().try_for_each(|body| each(body));
        }
        let (mut bodies, mut refused) = (Vec::new(), None);
        {
            let mut channel = self.channel.lock().unwrap_or_else(PoisonError::into_inner);
            write_message(&mut channel.output, &ChildFrame::Read(read)).map_err(lost)?;
            loop {
                let body = read_frame_raw(&mut channel.input).map_err(lost)?;
                match frame_of::<serde::de::IgnoredAny>(&body)? {
                    ReadAnswerFrame::Rows(_) => {
                        if refused.is_none() {
                            refused = each(&body).err();
                        }
                        bodies.push(body);
                    }
                    ReadAnswerFrame::Done => break,
                    ReadAnswerFrame::Failed(e) => return Err(e.into()),
                }
            }
        }
        if let Some(e) = refused {
            return Err(e);
        }
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).insert(key, Arc::new(bodies));
        Ok(())
    }

    /// A row answer: its parts, appended.
    fn rows<T: DeserializeOwned>(&self, read: StoreRead) -> Result<Vec<T>, HistoryStoreError> {
        let mut rows = Vec::new();
        self.parts(read, |body| {
            rows.append(&mut part_of::<Vec<T>>(body)?);
            Ok(())
        })?;
        Ok(rows)
    }

    /// A BAR answer: its [`BarColumns`] parts, each decoded straight into the answer from the body it
    /// borrows.
    fn bars(&self, read: StoreRead) -> Result<Vec<Bar>, HistoryStoreError> {
        let mut rows = Vec::new();
        self.parts(read, |body| part_of::<BarColumns<'_>>(body)?.push_into(&mut rows))?;
        Ok(rows)
    }

    /// A single-value answer: its one part.
    fn value<V: DeserializeOwned>(&self, read: StoreRead) -> Result<V, HistoryStoreError> {
        let mut value = None;
        self.parts(read, |body| {
            if value.is_none() {
                value = Some(part_of::<V>(body)?);
            }
            Ok(())
        })?;
        value.ok_or_else(|| {
            HistoryStoreError::Io(
                "the strategy sandbox's parent answered a value with no value".into(),
            )
        })
    }
}

/// A `Rows` part's payload, decoded.
fn part_of<'a, P: Deserialize<'a>>(body: &'a [u8]) -> Result<P, HistoryStoreError> {
    match frame_of::<P>(body)? {
        ReadAnswerFrame::Rows(part) => Ok(part),
        _ => Err(garbled("an answer part is not a part")),
    }
}

impl HistoryStore for PipeHistStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.bars(StoreRead::LoadBars {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range: range.into(),
        })
    }

    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.bars(StoreRead::LoadBarsSourcedCapped {
            venue: venue.into(),
            source: source.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range: range.into(),
            budget,
        })
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.rows(StoreRead::ScanQuotes {
            venue: venue.into(),
            symbol: symbol.into(),
            range: range.into(),
        })
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.rows(StoreRead::ScanTrades {
            venue: venue.into(),
            symbol: symbol.into(),
            range: range.into(),
        })
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.rows(StoreRead::ScanBookUpdates {
            venue: venue.into(),
            symbol: symbol.into(),
            range: range.into(),
        })
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.rows(StoreRead::ScanDepth {
            venue: venue.into(),
            symbol: symbol.into(),
            range: range.into(),
        })
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        self.rows(StoreRead::ScanCohort {
            venue: venue.into(),
            asset: asset.into(),
            range: range.into(),
        })
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        self.rows(StoreRead::ScanPerpMetrics {
            venue: venue.into(),
            symbol: symbol.into(),
            range: range.into(),
        })
    }

    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, HistoryStoreError> {
        self.value(StoreRead::PropertiesAsOf {
            venue: venue.into(),
            symbol: symbol.into(),
            timestamp: ts,
        })
    }

    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        self.rows(StoreRead::ListSeries)
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        self.rows(StoreRead::Inventory)
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        self.rows(StoreRead::SeriesGaps(id.clone()))
    }

    fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        self.value(StoreRead::SeriesFacts(id.clone()))
    }

    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        self.kept.clone().map_err(HistoryStoreError::from)
    }

    fn scan_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        Err(not_served("scan_symbol_properties"))
    }

    fn scan_equity(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        Err(not_served("scan_equity"))
    }

    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        Err(not_served("scan_exec_fills"))
    }

    fn scan_exec_orders(&self, _: &str, _: &str) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        Err(not_served("scan_exec_orders"))
    }

    fn append_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[Bar],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_bars"))
    }

    fn append_quotes(
        &self,
        _: &str,
        _: &str,
        _: &[TickQuote],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_quotes"))
    }

    fn append_trades(
        &self,
        _: &str,
        _: &str,
        _: &[TickTrade],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_trades"))
    }

    fn append_book_updates(
        &self,
        _: &str,
        _: &str,
        _: &[L2BookUpdate],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_book_updates"))
    }

    fn append_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: &[(i64, SymbolProperties)],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_symbol_properties"))
    }

    fn append_equity(
        &self,
        _: &str,
        _: &str,
        _: &[EquitySample],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_equity"))
    }

    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_exec_fills"))
    }

    fn append_exec_orders(
        &self,
        _: &str,
        _: &str,
        _: &[ExecOrderRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("append_exec_orders"))
    }

    fn resample_quotes_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("resample_quotes_to_bars"))
    }

    fn resample_trades_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(not_served("resample_trades_to_bars"))
    }
}
