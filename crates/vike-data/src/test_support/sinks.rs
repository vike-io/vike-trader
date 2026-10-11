//! The shared `LiveDataSink` doubles: `SinkCall`, `RecordingSink` and `NoopSink`.

use std::sync::Mutex;

use vike_model::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};

use crate::live::{LiveDataSink, StreamStatus};

// ===================================================================================================
// Shared `LiveDataSink` doubles (testing-arch Phase 4d).
// ===================================================================================================

/// One recorded [`LiveDataSink`] call, with its full payload — what [`RecordingSink`] captures.
/// Structured (not pre-formatted) so a test can assert on exactly the fields it cares about via
/// the typed accessors, while [`RecordingSink::calls`] renders the canonical one-line form for
/// exact-sequence assertions.
#[derive(Debug, Clone)]
pub enum SinkCall {
    SeedBars {
        venue: String,
        symbol: String,
        interval: String,
        bars: Vec<Bar>,
    },
    CloseBar {
        venue: String,
        symbol: String,
        interval: String,
        bar: Bar,
    },
    FormingBar {
        venue: String,
        symbol: String,
        interval: String,
        bar: Bar,
    },
    MarkTick {
        venue: String,
        symbol: String,
        px: f64,
        ts: i64,
    },
    BarCloseTick {
        venue: String,
        symbol: String,
        px: f64,
        ts: i64,
    },
    L2Snapshot {
        venue: String,
        symbol: String,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    },
    Quote {
        venue: String,
        symbol: String,
        quote: QuoteTick,
    },
    Trade {
        venue: String,
        symbol: String,
        trade: TradeTick,
    },
    Book {
        venue: String,
        symbol: String,
        book: L2Book,
    },
    BookUpdate {
        venue: String,
        symbol: String,
        update: BookUpdate,
    },
    StreamStatus {
        venue: String,
        symbol: String,
        stream: String,
        status: StreamStatus,
    },
}

impl std::fmt::Display for SinkCall {
    /// The canonical one-line form, the UNION of the formats the per-crate copies used (the
    /// bar-lane `verb(venue,symbol,…)` convention from `vike_data::live`'s original double, the
    /// tick-lane `verb:venue:symbol:…` convention at polymarket's richer field set) — so
    /// exact-sequence assertions read unchanged where they existed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SinkCall::SeedBars { venue, symbol, interval, bars } => {
                write!(f, "seed_bars({venue},{symbol},{interval},{})", bars.len())
            }
            SinkCall::CloseBar { venue, symbol, interval, bar } => {
                write!(f, "close_bar({venue},{symbol},{interval},{})", bar.close)
            }
            SinkCall::FormingBar { venue, symbol, interval, bar } => {
                write!(f, "forming_bar({venue},{symbol},{interval},{})", bar.close)
            }
            SinkCall::MarkTick { venue, symbol, px, ts } => {
                write!(f, "mark_tick({venue},{symbol},{px},{ts})")
            }
            SinkCall::BarCloseTick { venue, symbol, px, ts } => {
                write!(f, "bar_close_tick({venue},{symbol},{px},{ts})")
            }
            SinkCall::L2Snapshot { venue, symbol, tick_size, bids, asks, ts } => {
                write!(
                    f,
                    "l2_snapshot({venue},{symbol},{tick_size},{}b/{}a,{ts})",
                    bids.len(),
                    asks.len()
                )
            }
            SinkCall::Quote { venue, symbol, quote } => {
                write!(
                    f,
                    "quote:{venue}:{symbol}:{}/{}:{}x{}",
                    quote.bid, quote.ask, quote.bid_size, quote.ask_size
                )
            }
            SinkCall::Trade { venue, symbol, trade } => {
                write!(
                    f,
                    "trade:{venue}:{symbol}:{}/{}:maker={}",
                    trade.price, trade.size, trade.is_buyer_maker
                )
            }
            SinkCall::Book { venue, symbol, book } => {
                write!(f, "book:{venue}:{symbol}:{:?}", book.mid())
            }
            SinkCall::BookUpdate { venue, symbol, update } => {
                write!(
                    f,
                    "book_update:{venue}:{symbol}:{:?}:seq={}:bids={}:asks={}:local_ts_pos={}:tick={}",
                    update.kind,
                    update.seq,
                    update.bids.len(),
                    update.asks.len(),
                    update.local_ts > 0,
                    update.tick_size,
                )
            }
            SinkCall::StreamStatus { venue, symbol, stream, status } => {
                write!(f, "stream_status:{venue}:{symbol}:{stream}:{status:?}")
            }
        }
    }
}

/// The ONE shared capturing [`LiveDataSink`] (testing-arch Phase 4d): records EVERY sink call —
/// full payloads, in delivery order — into a `Mutex<Vec<SinkCall>>`. Replaces the ~13 per-crate
/// inline copies (vike-data's own, the crypto bridges' `market_feed` test modules and smoke
/// tests, polymarket's scripted-pump tests, ctrader's `tests/common`).
///
/// Two assertion styles, covering the union of what those copies asserted:
/// - [`RecordingSink::calls`] — the canonical formatted strings ([`SinkCall`]'s `Display`), for
///   exact-emission-sequence assertions;
/// - the typed accessors ([`RecordingSink::quotes`], [`RecordingSink::trades`], …) — full
///   payloads for field-level assertions (ts threading, per-fill deltas, counts on live smokes).
#[derive(Default)]
pub struct RecordingSink {
    calls: Mutex<Vec<SinkCall>>,
}

impl RecordingSink {
    fn push(&self, call: SinkCall) {
        self.calls.lock().unwrap().push(call);
    }

    /// Every recorded call, in delivery order.
    pub fn recorded(&self) -> Vec<SinkCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Every recorded call in the canonical one-line form (see [`SinkCall`]'s `Display`).
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|c| c.to_string()).collect()
    }

    /// Recorded `quote` calls: `(venue, symbol, quote)`, in order.
    pub fn quotes(&self) -> Vec<(String, String, QuoteTick)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::Quote { venue, symbol, quote } => {
                    Some((venue.clone(), symbol.clone(), quote.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `trade` calls: `(venue, symbol, trade)`, in order.
    pub fn trades(&self) -> Vec<(String, String, TradeTick)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::Trade { venue, symbol, trade } => {
                    Some((venue.clone(), symbol.clone(), trade.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `book` calls: `(venue, symbol, book)`, in order.
    pub fn books(&self) -> Vec<(String, String, L2Book)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::Book { venue, symbol, book } => {
                    Some((venue.clone(), symbol.clone(), book.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `book_update` calls: `(venue, symbol, update)`, in order.
    pub fn book_updates(&self) -> Vec<(String, String, BookUpdate)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::BookUpdate { venue, symbol, update } => {
                    Some((venue.clone(), symbol.clone(), update.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `seed_bars` calls: `(venue, symbol, interval, bars)`, in order.
    pub fn seeded_bars(&self) -> Vec<(String, String, String, Vec<Bar>)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::SeedBars { venue, symbol, interval, bars } => {
                    Some((venue.clone(), symbol.clone(), interval.clone(), bars.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `forming_bar` calls: `(venue, symbol, interval, bar)`, in order.
    pub fn forming_bars(&self) -> Vec<(String, String, String, Bar)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::FormingBar { venue, symbol, interval, bar } => {
                    Some((venue.clone(), symbol.clone(), interval.clone(), bar.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `close_bar` calls: `(venue, symbol, interval, bar)`, in order.
    pub fn closed_bars(&self) -> Vec<(String, String, String, Bar)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::CloseBar { venue, symbol, interval, bar } => {
                    Some((venue.clone(), symbol.clone(), interval.clone(), bar.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `l2_snapshot` calls: `(venue, symbol, tick_size, bids, asks, ts)`, in order.
    #[expect(clippy::type_complexity)] // the verb's own signature, tuple-captured
    pub fn l2_snapshots(&self) -> Vec<(String, String, f64, Vec<BookLevel>, Vec<BookLevel>, i64)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::L2Snapshot { venue, symbol, tick_size, bids, asks, ts } => Some((
                    venue.clone(),
                    symbol.clone(),
                    *tick_size,
                    bids.clone(),
                    asks.clone(),
                    *ts,
                )),
                _ => None,
            })
            .collect()
    }
}

impl LiveDataSink for RecordingSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.push(SinkCall::SeedBars {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            bars,
        });
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(SinkCall::CloseBar {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            bar,
        });
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(SinkCall::FormingBar {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            bar,
        });
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push(SinkCall::MarkTick { venue: venue.into(), symbol: symbol.into(), px, ts });
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push(SinkCall::BarCloseTick { venue: venue.into(), symbol: symbol.into(), px, ts });
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.push(SinkCall::L2Snapshot {
            venue: venue.into(),
            symbol: symbol.into(),
            tick_size,
            bids,
            asks,
            ts,
        });
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.push(SinkCall::Quote { venue: venue.into(), symbol: symbol.into(), quote });
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.push(SinkCall::Trade { venue: venue.into(), symbol: symbol.into(), trade });
    }
    fn book(&self, venue: &str, symbol: &str, book: std::sync::Arc<L2Book>) {
        // `SinkCall::Book` keeps an OWNED book so every existing reader (`books()`, the `Display`
        // rendering) is untouched; this test-double clone is off any hot path.
        self.push(SinkCall::Book {
            venue: venue.into(),
            symbol: symbol.into(),
            book: (*book).clone(),
        });
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.push(SinkCall::BookUpdate { venue: venue.into(), symbol: symbol.into(), update });
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.push(SinkCall::StreamStatus {
            venue: venue.into(),
            symbol: symbol.into(),
            stream: stream.into(),
            status,
        });
    }
}

/// A [`LiveDataSink`] that does nothing — for tests that need a warm body (a `Feeds`/client
/// constructor argument) but never assert on delivered data.
pub struct NoopSink;

impl LiveDataSink for NoopSink {
    fn seed_bars(&self, _venue: &str, _symbol: &str, _interval: &str, _bars: Vec<Bar>) {}
    fn close_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn forming_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn mark_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}
    fn quote(&self, _venue: &str, _symbol: &str, _quote: QuoteTick) {}
    fn trade(&self, _venue: &str, _symbol: &str, _trade: TradeTick) {}
    fn book(&self, _venue: &str, _symbol: &str, _book: std::sync::Arc<L2Book>) {}
}
