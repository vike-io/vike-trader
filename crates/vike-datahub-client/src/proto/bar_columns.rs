//! The COLUMNS replies: a range read answered as one BINARY frame body instead of JSON, for a client that
//! set the request's `columns` and a server that advertises the read's string: a bar read
//! ([`Request::LoadBars`](super::Request::LoadBars), [`FEATURE_BAR_COLUMNS`](super::FEATURE_BAR_COLUMNS),
//! decision 0107's amendment of 2026-11-18) and the tick-level scans ([`Request::ScanQuotes`](super::Request::ScanQuotes),
//! `ScanTrades`, `ScanBookUpdates`, `ScanDepth`, `ScanPerpMetrics`, [`FEATURE_TICK_COLUMNS`](super::FEATURE_TICK_COLUMNS))
//! and every other row-shaped scan (`ScanCohort`, `ScanTexts`, `ScanChain`, `ScanFinancialFacts`,
//! `ScanInstitutionalHoldings`, `ScanTextScores`, `ScanEquity`, `ScanExecFills`, `ScanExecOrders`, `ScanForecasts`,
//! `StrategyForecasts`, [`FEATURE_SCAN_COLUMNS`](super::FEATURE_SCAN_COLUMNS)). `PropertiesAsOf` is one record per
//! key, not rows of one shape, and stays the connection's msgpack.
//!
//! **Why.** A backtest reads bars by the million and a tick replay reads ticks by the million, and every one
//! used to be written as JSON text twice on the datahub (the frame fit counts it, then the frame writes it)
//! and parsed back on the client: every float each way through decimal text. Here each field is its eight
//! bytes, copied.
//!
//! **ONE codec, one table.** Every kind is the same body. What differs is the row, which its [`ColumnRow`]
//! impl below states (the table, one impl per row type), and the reply's magic ([`ColumnsMagic`], one per
//! REPLY: `ScanBookUpdates` and `ScanDepth` share a row type and still cannot be read for each other, the
//! rule `Request::ScanDepth` argues for the JSON variants).
//!
//! **The body**, every number little-endian:
//!
//! | bytes | what |
//! |---|---|
//! | 4 | the reply's magic ([`ColumnsMagic::plain`]): a NUL first, which no JSON body has, so a reader tells a columns reply from a JSON one (`Response::Error`) by it |
//! | 4 | `rows`, a `u32` |
//! | `8 x rows`, once per word | the row's words ([`ColumnRow::WORDS`]), one column after another |
//! | `rows` | `flags`, one byte a row: only the bits [`ColumnRow::FLAGS`] allows |
//! | `8 x levels`, twice | a book row's levels: every price, then every quantity, each row's bids then its asks, row after row; no byte for a kind without levels |
//! | 4 | `runs`, a `u32`: the `symbol` column as runs of equal values, so a series of one symbol crosses one string |
//! | per run | `count` (`u32`), `len` (`u32`, `0` for none, else the UTF-8 length plus one), the bytes |
//! | the same, once per text column | a kind's text columns ([`ColumnRow::TEXTS`]), each as the symbol column is; nothing for a kind without them |
//!
//! | row | magic | words | flags | symbol | text columns |
//! |---|---|---|---|---|---|
//! | `Bar` | [`BAR_COLUMNS`] | `timestamp`, `open`, `high`, `low`, `close`, `volume`, `funding`, `bid`, `ask` (an absent optional is a zero word) | bit 0 `funding`, bit 1 `bid`, bit 2 `ask` present | optional | none |
//! | `TickQuote` | [`QUOTE_COLUMNS`] | `timestamp`, `local_timestamp`, `bid`, `ask`, `bid_size`, `ask_size` | none | always one | none |
//! | `TickTrade` | [`TRADE_COLUMNS`] | `timestamp`, `local_timestamp`, `price`, `size` | bit 0 `is_buyer_maker` | always one | none |
//! | `L2BookUpdate` | [`BOOK_UPDATE_COLUMNS`], [`DEPTH_COLUMNS`] | `timestamp`, `local_timestamp`, `sequence`, `tick_size`, the bid count, the ask count | the event's kind, its index in `BOOK_KINDS` | always one | none |
//! | `PerpMetricRow` | [`PERP_METRIC_COLUMNS`] | `ts`, `premium`, `open_interest` (absent: a zero word) | bit 0 `open_interest` present | none: one run of none | none |
//! | `EquitySample` | [`EQUITY_COLUMNS`] | `timestamp`, `equity`, `realized`, `unrealized`, `missing_prices` | none | `venue` | none |
//! | `ExecForecastRow` | [`FORECAST_COLUMNS`] | `ts`, `value`, `horizon_ms`, `confidence` (absent: `ABSENT`) | none | `mount_id` | none |
//! | `CohortRow` | [`COHORT_COLUMNS`] | `ts`, `long_usd`, `total_usd` | none | `asset` | `axis`, `cohort`, `grading`, `label_basis` |
//! | `TextRow` | [`TEXT_COLUMNS`] | `received_ms`, `published_ms` | none | `kind` | every other string, in field order |
//! | `ChainRow` | [`CHAIN_COLUMNS`] | `ts`, `expiry_ms`, `strike`, then the ten optional quote and greek fields (absent: `ABSENT`) | bit 0 a put | `underlying` | `instrument` |
//! | `FinancialFactRow` | [`FINANCIAL_FACT_COLUMNS`] | `filed_ms`, `value` | none | `cik` | every other string, in field order |
//! | `InstitutionalHoldingRow` | [`INSTITUTIONAL_HOLDING_COLUMNS`] | `filed_ms`, `value_usd`, `shares` | none | `filer_cik` | every other string, in field order |
//! | `TextScoreRow` | [`TEXT_SCORE_COLUMNS`] | `received_ms`, `chunk`, `scored_ms` | none | `model` | `text_id`, `values` |
//! | `ExecFillRow` | [`EXECUTION_FILL_COLUMNS`] | `ts`, `side`, `qty`, `px`, `commission`, `mark_price` (absent: `ABSENT`), then the four optional integers (absent: a zero word) | bits 0-3 `correction_seq`, `received_ms`, `journal_sequence`, `journal_generation` present | `symbol` | every other string in field order, an absent optional one a run of none |
//! | `ExecOrderRow` | [`EXECUTION_ORDER_COLUMNS`] | `ts`, `side`, `qty`, `price`, `trigger_price` (absent: `ABSENT`), `filled_qty`, `avg_fill_px`, then the two optional integers (absent: a zero word) | bits 0-1 `journal_sequence`, `journal_generation` present | `symbol` | every other string in field order, an absent optional one a run of none |
//!
//! The kinds after the tick ones mark an absent optional float IN its word (`ABSENT`, a NaN no present value is,
//! since a non-finite one is written absent), so a row of ten optional floats needs no ten flag bits.
//!
//! A read that asks [`BarColumnsBody::Lz4`] may get that body COMPRESSED instead: [`ColumnsMagic::lz4`],
//! the plain body's length (`u32`), one LZ4 block (`lz4_flex::block`, already in the tree through parquet).
//!
//! **ONE encoder.** [`encode_columns_into`] fills a `Vec` in ONE pass over the rows: the datahub's plain reply
//! straight into its frame behind four reserved bytes (`vike_hub_protocol::frame::write_frame_reserved`), the LZ4
//! reply, the backtesthub cache's kept days. A writer that STREAMED the body into the socket a column at a time
//! was dropped on 2026-11-19: it took about three times this encoder's CPU, and the one-pass reply measures at
//! the time and datahub peak per page bar columns had before the tick work; the tests keep it as the reference
//! the one-pass bytes are held to. Decoded straight into the caller's `Vec` ([`decode_columns_into`]), so a page
//! is never built apart and copied after. Bodies already encoded are joined by COPYING their columns
//! ([`splice_columns_into`], the backtest sandbox's bar and tick parts), byte for byte what this encoder writes
//! for their rows.
//!
//! ⚠ **The same rows as the JSON reply, which is the contract.** JSON writes a non-finite `f64` as `null`,
//! so an OPTIONAL float that is NaN or infinite reads back `None` there: the encoder writes it absent here
//! too. A non-finite REQUIRED field has no JSON answer at all (the client cannot decode `null` into an
//! `f64`), and here it keeps its bits.
//!
//! ⚠ **A malformed body is refused, never read short.** The decoder checks the length against `rows`, the
//! levels and the runs exactly, the runs' counts against `rows`, the flag bits, every string's UTF-8 and
//! every row's own fields (a book kind no event has, a symbol a kind must or must not carry), so a
//! truncated or garbled frame is an error and never fewer rows. A reader that takes a reply's body as it came
//! (`HistoryStoreDatahub`'s `read_columns`, [`plain_columns`]) makes every one of those checks where it arrives
//! ([`check_columns`], the decoder's own walk with no row built), before anything keeps or relays it.

use serde::{Deserialize, Serialize};
use vike_data::{
    ChainRow, CohortRow, ExecFillRow, ExecForecastRow, ExecOrderRow, FinancialFactRow,
    InstitutionalHoldingRow, PerpMetricRow, TextRow, TextScoreRow,
};
use vike_hub_protocol::frame::{BodyReader, MAX_FRAME_LEN};
use vike_marketdata::{
    Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, OptionKind, TickQuote, TickTrade,
};
use vike_model::EquitySample;

/// The first four bytes of a bar-columns body. A JSON body opens with `{`, `[`, `"` or whitespace,
/// never a NUL, so a reader that asked for columns tells the reply from a `Response::Error` by them.
pub const BAR_COLUMNS_MAGIC: [u8; 4] = *b"\0BAR";

/// The first four bytes of an LZ4-compressed bar-columns body ([`compress_columns`]): then the plain
/// body's length (`u32`) and one LZ4 block of it.
pub const BAR_COLUMNS_LZ4_MAGIC: [u8; 4] = *b"\0BLZ";

/// How a columns read wants its body: [`Request::LoadBars`](super::Request::LoadBars)' `columns`, and the
/// tick-level scans' (`Request::ScanQuotes` and its siblings).
///
/// `Lz4` is for a client on a THIN link (the desktop reaches its datahub over an ssh tunnel measured at
/// 13-40 KB/s): one LZ4 block of the plain body (`crates/vike-datahub/tests/datafusion_serve/bar_load_timing.rs`
/// prints the ratio on a synthetic store; real prices compress less). A same-box reader
/// (the backtesthub beside its datahub) asks `Plain`, which costs no CPU at either end. The server may
/// answer `Plain` to an `Lz4` read (a body LZ4 cannot shrink); a client reads either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BarColumnsBody {
    Plain,
    Lz4,
}

/// What one REPLY's columns body opens with, plain and compressed, and what its refusals call it. One per
/// reply, not per row type: [`BOOK_UPDATE_COLUMNS`] and [`DEPTH_COLUMNS`] carry the same rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnsMagic {
    /// The reply's word in a refusal: "a malformed bar-columns reply".
    pub name: &'static str,
    /// The first four bytes of the plain body: a NUL, then three letters.
    pub plain: [u8; 4],
    /// The first four bytes of the LZ4 body ([`compress_columns`]).
    pub lz4: [u8; 4],
}

/// `Request::LoadBars`' columns.
pub const BAR_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "bar", plain: BAR_COLUMNS_MAGIC, lz4: BAR_COLUMNS_LZ4_MAGIC };
/// `Request::ScanQuotes`' columns.
pub const QUOTE_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "quote", plain: *b"\0QUO", lz4: *b"\0QLZ" };
/// `Request::ScanTrades`' columns.
pub const TRADE_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "trade", plain: *b"\0TRD", lz4: *b"\0TLZ" };
/// `Request::ScanBookUpdates`' columns: the lossless book lane.
pub const BOOK_UPDATE_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "book-update", plain: *b"\0BOK", lz4: *b"\0KLZ" };
/// `Request::ScanDepth`' columns: the conflating depth lane, the same rows as [`BOOK_UPDATE_COLUMNS`] under
/// another magic.
pub const DEPTH_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "depth", plain: *b"\0DEP", lz4: *b"\0DLZ" };
/// `Request::ScanPerpMetrics`' columns.
pub const PERP_METRIC_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "perp-metric", plain: *b"\0PRM", lz4: *b"\0PLZ" };

/// The most words a [`ColumnRow`] has: the option chain row's thirteen.
pub const MAX_COLUMN_WORDS: usize = 13;

/// The most TEXT columns a [`ColumnRow`] has beside its symbol: the text row's sixteen.
pub const MAX_TEXT_COLUMNS: usize = 16;

/// One row type's place in the columns codec: the module doc's table, one impl per row type.
///
/// `words` and `flags` are what the encoders write and `from_columns` what the decoder reads back, so an
/// impl is the WHOLE of a kind: the layout, its lengths and its checks are the codec's, once.
pub trait ColumnRow: Sized + 'static {
    /// How many eight-byte word columns a row has, at most [`MAX_COLUMN_WORDS`].
    const WORDS: usize;
    /// The bits a row's flag byte may set; a body setting another is refused.
    const FLAGS: u8;
    /// The two words (by column) that count a row's bid levels and its ask levels: a book row's. `None` for
    /// a kind without levels, whose body holds no level column.
    const LEVEL_COUNTS: Option<[usize; 2]> = None;
    /// How many TEXT columns a row has beside its symbol, at most [`MAX_TEXT_COLUMNS`], each written as the symbol
    /// column is (runs of equal values, `None` a run of none). Zero for the bar and tick kinds, whose body is
    /// unchanged by the field.
    const TEXTS: usize = 0;

    /// The row's words in column order; past [`Self::WORDS`] they are zero and unwritten.
    fn words(&self) -> [u64; MAX_COLUMN_WORDS];
    /// The row's flag byte.
    fn flags(&self) -> u8;
    /// The row's symbol, run-length coded: `None` only for a kind whose symbol is optional or absent.
    fn symbol(&self) -> Option<&str>;
    /// The row's levels, bids then asks: a book row's ([`Self::LEVEL_COUNTS`] counts them). None for any
    /// other kind.
    fn levels(&self) -> [&[L2BookLevel]; 2] {
        [&[], &[]]
    }
    /// The row's text columns in column order ([`Self::TEXTS`]); past it they are `None` and unwritten.
    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        [None; MAX_TEXT_COLUMNS]
    }
    /// A row back from its words, its flag byte (inside [`Self::FLAGS`]), its levels, its symbol run and its
    /// text columns' runs; `Err` names what no row of this kind can be.
    fn from_columns(
        words: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        levels: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        texts: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str>;
    /// Whether [`Self::from_columns`] builds a row of these columns, decided without building one: `Err` is the words
    /// it refuses them with. What [`check_columns`] asks of every row. The default builds the row and drops it, right
    /// for every kind; a kind a reader checks by the million (the backtesthub's kept bars and ticks) states the same
    /// checks with no string made, held to `from_columns` by `bar_columns_tests.rs`'s
    /// `check_row_refuses_what_from_columns_refuses`.
    fn check_row(
        words: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        symbol: Option<&str>,
        texts: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<(), &'static str> {
        Self::from_columns(words, flags, [Vec::new(), Vec::new()], symbol, texts).map(drop)
    }
}

/// `words` padded with zero words to [`MAX_COLUMN_WORDS`]: what a row of fewer words returns.
fn pad<const N: usize>(words: [u64; N]) -> [u64; MAX_COLUMN_WORDS] {
    const { assert!(N <= MAX_COLUMN_WORDS) };
    std::array::from_fn(|i| words.get(i).copied().unwrap_or(0))
}

/// The word of an optional float in a kind that marks absence IN THE WORD (every kind after the tick ones, so a row
/// of many optional floats needs no flag bit for each): its bits when [`finite`], else [`ABSENT`]. A present value
/// is finite (a non-finite one is written absent, as JSON reads it), so no present value is a NaN.
fn absent_or(v: Option<f64>) -> u64 {
    finite(v).map_or(ABSENT, f64::to_bits)
}

/// The word of an absent optional float ([`absent_or`]): the canonical quiet NaN.
const ABSENT: u64 = 0x7ff8_0000_0000_0000;

/// An optional float back from its word ([`absent_or`]): absent when it is a NaN.
fn optional_float(word: u64) -> Option<f64> {
    let value = f64::from_bits(word);
    (!value.is_nan()).then_some(value)
}

/// An optional integer back from its word, present when its flag `bit` is set in `flags`.
fn optional_int(word: u64, flags: u8, bit: u8) -> Option<i64> {
    (flags & bit != 0).then_some(word as i64)
}

/// An optional integer's flag bit: `1` when present.
fn has_int(v: Option<i64>) -> u8 {
    u8::from(v.is_some())
}

/// A text column a kind always has.
fn text(t: Option<&str>) -> Result<String, &'static str> {
    t.map(str::to_string).ok_or("a text run carries none where this kind always has a text")
}

/// The text columns of a row, from its fields in column order, padded with `None`.
fn text_columns<const N: usize>(fields: [Option<&str>; N]) -> [Option<&str>; MAX_TEXT_COLUMNS] {
    const { assert!(N <= MAX_TEXT_COLUMNS) };
    std::array::from_fn(|i| fields.get(i).copied().flatten())
}

/// A signed word back as an `i32` (a fill's or an order's side), refused out of range.
fn side_word(word: u64) -> Result<i32, &'static str> {
    i32::try_from(word as i64).map_err(|_| "a side word outside an i32")
}

/// A word back as a `u32` (a count), refused out of range.
fn count_word(word: u64) -> Result<u32, &'static str> {
    u32::try_from(word).map_err(|_| "a count word outside a u32")
}

/// A finite value, as JSON would carry it: `NaN` and the infinities read `None` there (the module doc's ⚠).
fn finite(v: Option<f64>) -> Option<f64> {
    v.filter(|v| v.is_finite())
}

/// An optional float's word: its bits when [`finite`], else a zero word.
fn optional_word(v: Option<f64>) -> u64 {
    finite(v).map_or(0, f64::to_bits)
}

/// An optional float's flag bit: `1` when [`finite`].
fn present(v: Option<f64>) -> u8 {
    u8::from(finite(v).is_some())
}

/// An optional float back from its word, present when its flag `bit` is set in `flags`.
fn optional(word: u64, flags: u8, bit: u8) -> Option<f64> {
    (flags & bit != 0).then(|| f64::from_bits(word))
}

/// The symbol of a kind that always carries one, as the row's field holds it (a `String`, or an interned
/// `Symbol`: a run of one symbol interns it once, the thread's memo answering the rest).
fn named<T: for<'a> From<&'a str>>(symbol: Option<&str>) -> Result<T, &'static str> {
    symbol_of(symbol).map(T::from)
}

/// [`named`]'s check, with no string made and nothing interned.
fn symbol_of(symbol: Option<&str>) -> Result<&str, &'static str> {
    symbol.ok_or("a run carries no symbol, which this kind always has")
}

impl ColumnRow for Bar {
    const WORDS: usize = 9;
    const FLAGS: u8 = 0b111;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.timestamp as u64,
            self.open.to_bits(),
            self.high.to_bits(),
            self.low.to_bits(),
            self.close.to_bits(),
            self.volume.to_bits(),
            optional_word(self.funding),
            optional_word(self.bid),
            optional_word(self.ask),
        ])
    }

    fn flags(&self) -> u8 {
        present(self.funding) | (present(self.bid) << 1) | (present(self.ask) << 2)
    }

    fn symbol(&self) -> Option<&str> {
        self.symbol.as_deref()
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(Bar {
            timestamp: w[0] as i64,
            open: f64::from_bits(w[1]),
            high: f64::from_bits(w[2]),
            low: f64::from_bits(w[3]),
            close: f64::from_bits(w[4]),
            volume: f64::from_bits(w[5]),
            funding: optional(w[6], flags, 1),
            bid: optional(w[7], flags, 2),
            ask: optional(w[8], flags, 4),
            symbol: symbol.map(Into::into),
        })
    }

    /// Every row of these columns is a bar.
    fn check_row(
        _: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<(), &'static str> {
        Ok(())
    }
}

impl ColumnRow for TickQuote {
    const WORDS: usize = 6;
    const FLAGS: u8 = 0;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.timestamp as u64,
            self.local_timestamp as u64,
            self.bid.to_bits(),
            self.ask.to_bits(),
            self.bid_size.to_bits(),
            self.ask_size.to_bits(),
            0,
            0,
            0,
        ])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.symbol)
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(TickQuote {
            timestamp: w[0] as i64,
            local_timestamp: w[1] as i64,
            bid: f64::from_bits(w[2]),
            ask: f64::from_bits(w[3]),
            bid_size: f64::from_bits(w[4]),
            ask_size: f64::from_bits(w[5]),
            symbol: named(symbol)?,
        })
    }

    fn check_row(
        _: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<(), &'static str> {
        symbol_of(symbol).map(drop)
    }
}

impl ColumnRow for TickTrade {
    const WORDS: usize = 4;
    const FLAGS: u8 = 0b1;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.timestamp as u64,
            self.local_timestamp as u64,
            self.price.to_bits(),
            self.size.to_bits(),
            0,
            0,
            0,
            0,
            0,
        ])
    }

    fn flags(&self) -> u8 {
        u8::from(self.is_buyer_maker)
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.symbol)
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(TickTrade {
            timestamp: w[0] as i64,
            local_timestamp: w[1] as i64,
            price: f64::from_bits(w[2]),
            size: f64::from_bits(w[3]),
            is_buyer_maker: flags & 1 != 0,
            symbol: named(symbol)?,
        })
    }

    fn check_row(
        _: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<(), &'static str> {
        symbol_of(symbol).map(drop)
    }
}

/// A book event's kind as its flag byte: its index here, both ways, so the wire never leans on the enum's
/// declaration order.
const BOOK_KINDS: [L2BookUpdateKind; 5] = [
    L2BookUpdateKind::Delta,
    L2BookUpdateKind::Snapshot,
    L2BookUpdateKind::GapStart,
    L2BookUpdateKind::Stale,
    L2BookUpdateKind::LiveResume,
];

impl ColumnRow for L2BookUpdate {
    const WORDS: usize = 6;
    const FLAGS: u8 = 0b111;
    const LEVEL_COUNTS: Option<[usize; 2]> = Some([4, 5]);

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.timestamp as u64,
            self.local_timestamp as u64,
            self.sequence,
            self.tick_size.to_bits(),
            self.bids.len() as u64,
            self.asks.len() as u64,
            0,
            0,
            0,
        ])
    }

    fn flags(&self) -> u8 {
        BOOK_KINDS.iter().position(|k| *k == self.kind).expect("every kind is in BOOK_KINDS") as u8
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.symbol)
    }

    fn levels(&self) -> [&[L2BookLevel]; 2] {
        [&self.bids, &self.asks]
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        [bids, asks]: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(L2BookUpdate {
            timestamp: w[0] as i64,
            local_timestamp: w[1] as i64,
            sequence: w[2],
            kind: book_kind(flags)?,
            tick_size: f64::from_bits(w[3]),
            bids,
            asks,
            symbol: named(symbol)?,
        })
    }

    fn check_row(
        _: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<(), &'static str> {
        book_kind(flags)?;
        symbol_of(symbol).map(drop)
    }
}

/// A book event's kind from its flag byte, its index in [`BOOK_KINDS`].
fn book_kind(flags: u8) -> Result<L2BookUpdateKind, &'static str> {
    BOOK_KINDS.get(usize::from(flags)).copied().ok_or("a book kind no event has")
}

impl ColumnRow for PerpMetricRow {
    const WORDS: usize = 3;
    const FLAGS: u8 = 0b1;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.ts as u64,
            self.premium.to_bits(),
            optional_word(self.open_interest),
            0,
            0,
            0,
            0,
            0,
            0,
        ])
    }

    fn flags(&self) -> u8 {
        present(self.open_interest)
    }

    fn symbol(&self) -> Option<&str> {
        None
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        if symbol.is_some() {
            return Err("a run carries a symbol, which this kind never has");
        }
        Ok(PerpMetricRow {
            ts: w[0] as i64,
            premium: f64::from_bits(w[1]),
            open_interest: optional(w[2], flags, 1),
        })
    }
}

/// `Request::ScanCohort`'s columns.
pub const COHORT_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "cohort", plain: *b"\0COH", lz4: *b"\0CLZ" };
/// `Request::ScanTexts`' columns.
pub const TEXT_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "text", plain: *b"\0TXT", lz4: *b"\0XLZ" };
/// `Request::ScanChain`'s columns.
pub const CHAIN_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "chain", plain: *b"\0CHN", lz4: *b"\0HLZ" };
/// `Request::ScanFinancialFacts`' columns.
pub const FINANCIAL_FACT_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "financial-fact", plain: *b"\0FIN", lz4: *b"\0FLZ" };
/// `Request::ScanInstitutionalHoldings`' columns.
pub const INSTITUTIONAL_HOLDING_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "institutional-holding", plain: *b"\0HLD", lz4: *b"\0ILZ" };
/// `Request::ScanTextScores`' columns.
pub const TEXT_SCORE_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "text-score", plain: *b"\0TSC", lz4: *b"\0SLZ" };
/// `Request::ScanEquity`'s columns.
pub const EQUITY_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "equity", plain: *b"\0EQY", lz4: *b"\0ELZ" };
/// `Request::ScanExecFills`' columns.
pub const EXECUTION_FILL_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "execution-fill", plain: *b"\0EXF", lz4: *b"\0GLZ" };
/// `Request::ScanExecOrders`' columns.
pub const EXECUTION_ORDER_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "execution-order", plain: *b"\0EXO", lz4: *b"\0OLZ" };
/// `Request::ScanForecasts`' and `Request::StrategyForecasts`' columns: one reply, `Response::Forecasts`.
pub const FORECAST_COLUMNS: ColumnsMagic =
    ColumnsMagic { name: "forecast", plain: *b"\0FCS", lz4: *b"\0RLZ" };

impl ColumnRow for EquitySample {
    const WORDS: usize = 5;
    const FLAGS: u8 = 0;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.timestamp as u64,
            self.equity.to_bits(),
            self.realized.to_bits(),
            self.unrealized.to_bits(),
            u64::from(self.missing_prices),
        ])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.venue)
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(EquitySample {
            timestamp: w[0] as i64,
            venue: named(symbol)?,
            equity: f64::from_bits(w[1]),
            realized: f64::from_bits(w[2]),
            unrealized: f64::from_bits(w[3]),
            missing_prices: count_word(w[4])?,
        })
    }
}

impl ColumnRow for ExecForecastRow {
    const WORDS: usize = 4;
    const FLAGS: u8 = 0;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.ts as u64,
            self.value.to_bits(),
            self.horizon_ms as u64,
            absent_or(self.confidence),
        ])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.mount_id)
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        _: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(ExecForecastRow {
            ts: w[0] as i64,
            mount_id: named(symbol)?,
            value: f64::from_bits(w[1]),
            horizon_ms: w[2] as i64,
            confidence: optional_float(w[3]),
        })
    }
}

impl ColumnRow for CohortRow {
    const WORDS: usize = 3;
    const FLAGS: u8 = 0;
    const TEXTS: usize = 4;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([self.ts as u64, self.long_usd.to_bits(), self.total_usd.to_bits()])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.asset)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([
            Some(self.axis.as_str()),
            Some(&self.cohort),
            Some(&self.grading),
            Some(&self.label_basis),
        ])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(CohortRow {
            ts: w[0] as i64,
            asset: named(symbol)?,
            axis: text(t[0])?,
            cohort: text(t[1])?,
            grading: text(t[2])?,
            label_basis: text(t[3])?,
            long_usd: f64::from_bits(w[1]),
            total_usd: f64::from_bits(w[2]),
        })
    }
}

impl ColumnRow for TextRow {
    const WORDS: usize = 2;
    const FLAGS: u8 = 0;
    const TEXTS: usize = 16;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([self.received_ms as u64, self.published_ms as u64])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.kind)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([
            Some(self.source.as_str()),
            Some(&self.source_id),
            Some(&self.subject),
            Some(&self.period),
            Some(&self.title),
            Some(&self.summary),
            Some(&self.body),
            Some(&self.url),
            Some(&self.market),
            Some(&self.tags),
            Some(&self.currency),
            Some(&self.impact),
            Some(&self.forecast),
            Some(&self.previous),
            Some(&self.actual),
            Some(&self.amends),
        ])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(TextRow {
            received_ms: w[0] as i64,
            published_ms: w[1] as i64,
            kind: named(symbol)?,
            source: text(t[0])?,
            source_id: text(t[1])?,
            subject: text(t[2])?,
            period: text(t[3])?,
            title: text(t[4])?,
            summary: text(t[5])?,
            body: text(t[6])?,
            url: text(t[7])?,
            market: text(t[8])?,
            tags: text(t[9])?,
            currency: text(t[10])?,
            impact: text(t[11])?,
            forecast: text(t[12])?,
            previous: text(t[13])?,
            actual: text(t[14])?,
            amends: text(t[15])?,
        })
    }
}

impl ColumnRow for ChainRow {
    const WORDS: usize = 13;
    const FLAGS: u8 = 0b1;
    const TEXTS: usize = 1;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        [
            self.ts as u64,
            self.expiry_ms as u64,
            self.strike.to_bits(),
            absent_or(self.bid),
            absent_or(self.ask),
            absent_or(self.mark),
            absent_or(self.iv),
            absent_or(self.open_interest),
            absent_or(self.volume),
            absent_or(self.delta),
            absent_or(self.gamma),
            absent_or(self.theta),
            absent_or(self.vega),
        ]
    }

    fn flags(&self) -> u8 {
        u8::from(self.kind == OptionKind::Put)
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.underlying)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([Some(self.instrument.as_str())])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(ChainRow {
            ts: w[0] as i64,
            underlying: named(symbol)?,
            instrument: text(t[0])?,
            expiry_ms: w[1] as i64,
            strike: f64::from_bits(w[2]),
            kind: if flags & 1 != 0 { OptionKind::Put } else { OptionKind::Call },
            bid: optional_float(w[3]),
            ask: optional_float(w[4]),
            mark: optional_float(w[5]),
            iv: optional_float(w[6]),
            open_interest: optional_float(w[7]),
            volume: optional_float(w[8]),
            delta: optional_float(w[9]),
            gamma: optional_float(w[10]),
            theta: optional_float(w[11]),
            vega: optional_float(w[12]),
        })
    }
}

impl ColumnRow for FinancialFactRow {
    const WORDS: usize = 2;
    const FLAGS: u8 = 0;
    const TEXTS: usize = 11;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([self.filed_ms as u64, self.value.to_bits()])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.cik)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([
            Some(self.entity.as_str()),
            Some(&self.taxonomy),
            Some(&self.concept),
            Some(&self.unit),
            Some(&self.period_start),
            Some(&self.period_end),
            Some(&self.accession),
            Some(&self.fiscal_year),
            Some(&self.fiscal_period),
            Some(&self.form),
            Some(&self.frame),
        ])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(FinancialFactRow {
            filed_ms: w[0] as i64,
            cik: named(symbol)?,
            entity: text(t[0])?,
            taxonomy: text(t[1])?,
            concept: text(t[2])?,
            unit: text(t[3])?,
            period_start: text(t[4])?,
            period_end: text(t[5])?,
            value: f64::from_bits(w[1]),
            accession: text(t[6])?,
            fiscal_year: text(t[7])?,
            fiscal_period: text(t[8])?,
            form: text(t[9])?,
            frame: text(t[10])?,
        })
    }
}

impl ColumnRow for InstitutionalHoldingRow {
    const WORDS: usize = 3;
    const FLAGS: u8 = 0;
    const TEXTS: usize = 10;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([self.filed_ms as u64, self.value_usd as u64, self.shares as u64])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.filer_cik)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([
            Some(self.accession.as_str()),
            Some(&self.filer_name),
            Some(&self.period),
            Some(&self.submission_type),
            Some(&self.amendment_type),
            Some(&self.issuer),
            Some(&self.class_title),
            Some(&self.cusip),
            Some(&self.shares_type),
            Some(&self.put_call),
        ])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(InstitutionalHoldingRow {
            filed_ms: w[0] as i64,
            accession: text(t[0])?,
            filer_cik: named(symbol)?,
            filer_name: text(t[1])?,
            period: text(t[2])?,
            submission_type: text(t[3])?,
            amendment_type: text(t[4])?,
            issuer: text(t[5])?,
            class_title: text(t[6])?,
            cusip: text(t[7])?,
            value_usd: w[1] as i64,
            shares: w[2] as i64,
            shares_type: text(t[8])?,
            put_call: text(t[9])?,
        })
    }
}

impl ColumnRow for TextScoreRow {
    const WORDS: usize = 3;
    const FLAGS: u8 = 0;
    const TEXTS: usize = 2;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([self.received_ms as u64, u64::from(self.chunk), self.scored_ms as u64])
    }

    fn flags(&self) -> u8 {
        0
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.model)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([Some(self.text_id.as_str()), Some(&self.values)])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        _: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(TextScoreRow {
            received_ms: w[0] as i64,
            text_id: text(t[0])?,
            chunk: count_word(w[1])?,
            scored_ms: w[2] as i64,
            model: named(symbol)?,
            values: text(t[1])?,
        })
    }
}

impl ColumnRow for ExecFillRow {
    const WORDS: usize = 10;
    const FLAGS: u8 = 0b1111;
    const TEXTS: usize = 11;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.ts as u64,
            i64::from(self.side) as u64,
            self.qty.to_bits(),
            self.px.to_bits(),
            self.commission.to_bits(),
            absent_or(self.mark_price),
            self.correction_seq.unwrap_or(0) as u64,
            self.received_ms.unwrap_or(0) as u64,
            self.journal_sequence.unwrap_or(0) as u64,
            self.journal_generation.unwrap_or(0) as u64,
        ])
    }

    fn flags(&self) -> u8 {
        has_int(self.correction_seq)
            | (has_int(self.received_ms) << 1)
            | (has_int(self.journal_sequence) << 2)
            | (has_int(self.journal_generation) << 3)
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.symbol)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([
            Some(self.trade_id.as_str()),
            Some(&self.order_client_id),
            Some(&self.venue),
            Some(&self.liquidity_side),
            Some(&self.commission_asset),
            self.venue_order_id.as_deref(),
            self.account.as_deref(),
            self.position_side.as_deref(),
            self.corrects_trade_id.as_deref(),
            self.mount_id.as_deref(),
            self.ownership.as_deref(),
        ])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(ExecFillRow {
            ts: w[0] as i64,
            trade_id: text(t[0])?,
            order_client_id: text(t[1])?,
            venue: text(t[2])?,
            symbol: named(symbol)?,
            side: side_word(w[1])?,
            qty: f64::from_bits(w[2]),
            px: f64::from_bits(w[3]),
            commission: f64::from_bits(w[4]),
            mark_price: optional_float(w[5]),
            liquidity_side: text(t[3])?,
            commission_asset: text(t[4])?,
            venue_order_id: t[5].map(str::to_string),
            account: t[6].map(str::to_string),
            position_side: t[7].map(str::to_string),
            corrects_trade_id: t[8].map(str::to_string),
            correction_seq: optional_int(w[6], flags, 1),
            mount_id: t[9].map(str::to_string),
            received_ms: optional_int(w[7], flags, 2),
            ownership: t[10].map(str::to_string),
            journal_sequence: optional_int(w[8], flags, 4),
            journal_generation: optional_int(w[9], flags, 8),
        })
    }
}

impl ColumnRow for ExecOrderRow {
    const WORDS: usize = 9;
    const FLAGS: u8 = 0b11;
    const TEXTS: usize = 9;

    fn words(&self) -> [u64; MAX_COLUMN_WORDS] {
        pad([
            self.ts as u64,
            i64::from(self.side) as u64,
            self.qty.to_bits(),
            absent_or(self.price),
            absent_or(self.trigger_price),
            self.filled_qty.to_bits(),
            self.avg_fill_px.to_bits(),
            self.journal_sequence.unwrap_or(0) as u64,
            self.journal_generation.unwrap_or(0) as u64,
        ])
    }

    fn flags(&self) -> u8 {
        has_int(self.journal_sequence) | (has_int(self.journal_generation) << 1)
    }

    fn symbol(&self) -> Option<&str> {
        Some(&self.symbol)
    }

    fn texts(&self) -> [Option<&str>; MAX_TEXT_COLUMNS] {
        text_columns([
            Some(self.order_client_id.as_str()),
            Some(&self.venue),
            Some(&self.order_type),
            Some(&self.status),
            self.venue_order_id.as_deref(),
            self.account.as_deref(),
            self.mount_id.as_deref(),
            self.ownership.as_deref(),
            self.reason.as_deref(),
        ])
    }

    fn from_columns(
        w: &[u64; MAX_COLUMN_WORDS],
        flags: u8,
        _: [Vec<L2BookLevel>; 2],
        symbol: Option<&str>,
        t: [Option<&str>; MAX_TEXT_COLUMNS],
    ) -> Result<Self, &'static str> {
        Ok(ExecOrderRow {
            ts: w[0] as i64,
            order_client_id: text(t[0])?,
            venue: text(t[1])?,
            symbol: named(symbol)?,
            side: side_word(w[1])?,
            qty: f64::from_bits(w[2]),
            order_type: text(t[2])?,
            status: text(t[3])?,
            price: optional_float(w[3]),
            trigger_price: optional_float(w[4]),
            venue_order_id: t[4].map(str::to_string),
            filled_qty: f64::from_bits(w[5]),
            avg_fill_px: f64::from_bits(w[6]),
            account: t[5].map(str::to_string),
            mount_id: t[6].map(str::to_string),
            ownership: t[7].map(str::to_string),
            journal_sequence: optional_int(w[7], flags, 1),
            journal_generation: optional_int(w[8], flags, 2),
            reason: t[8].map(str::to_string),
        })
    }
}

/// The magic and the row count.
const HEAD: usize = 8;
/// One level's price and quantity.
const LEVEL: usize = 16;
/// The run count after the columns.
const RUNS: usize = 4;
/// A run's `count` and `len`, before its bytes.
const RUN_HEAD: usize = 8;

/// The words and the flag byte of one row of `R`.
const fn row_bytes<R: ColumnRow>() -> usize {
    8 * R::WORDS + 1
}

/// The bytes of a body of a kind with no text column that holds no row: what [`columns_row_len`] counts on top of.
pub const COLUMNS_EMPTY_LEN: usize = HEAD + RUNS;

/// [`COLUMNS_EMPTY_LEN`] of a kind `R`: one more run count per text column.
pub const fn columns_empty_len<R: ColumnRow>() -> usize {
    HEAD + RUNS * (1 + R::TEXTS)
}

/// How many levels `row` carries, bids and asks.
fn level_count<R: ColumnRow>(row: &R) -> usize {
    row.levels().iter().map(|side| side.len()).sum()
}

/// What `row` adds to a body whose previous row is `previous`: its columns and levels, plus a run's head
/// and its symbol's bytes when it starts a run. Summed over a page on top of [`COLUMNS_EMPTY_LEN`], it is
/// exactly the body's length ([`columns_len`]), so the datahub can fit a page to the frame before encoding.
pub fn columns_row_len<R: ColumnRow>(previous: Option<&R>, row: &R) -> usize {
    let starts_run = previous.is_none_or(|p| p.symbol() != row.symbol());
    let texts = if R::TEXTS == 0 {
        0
    } else {
        let (now, before) = (row.texts(), previous.map(R::texts));
        (0..R::TEXTS)
            .filter(|c| before.is_none_or(|b| b[*c] != now[*c]))
            .map(|c| RUN_HEAD + now[c].map_or(0, str::len))
            .sum()
    };
    row_bytes::<R>()
        + LEVEL * level_count(row)
        + if starts_run { RUN_HEAD + row.symbol().map_or(0, str::len) } else { 0 }
        + texts
}

/// The exact length of `rows`' columns body: what both encoders write, and what the datahub fits a page to
/// its frame by ([`columns_row_len`] row by row).
pub fn columns_len<R: ColumnRow>(rows: &[R]) -> usize {
    columns_empty_len::<R>()
        + rows
            .iter()
            .enumerate()
            .map(|(i, r)| columns_row_len(i.checked_sub(1).map(|p| &rows[p]), r))
            .sum::<usize>()
}

/// The head: the magic and the row count.
///
/// # Panics
/// On more than `u32::MAX` rows, which no frame can carry: the datahub reads at most a ceiling's rows for one
/// reply (`LOAD_BARS_CEILING` and its siblings).
fn head(magic: &ColumnsMagic, rows: usize) -> [u8; HEAD] {
    let n = u32::try_from(rows).expect("a reply carries fewer than u32::MAX rows");
    let mut head = [0u8; HEAD];
    head[..4].copy_from_slice(&magic.plain);
    head[4..].copy_from_slice(&n.to_le_bytes());
    head
}

/// The symbol column's runs of equal values, as the encoders gather them in their pass.
type SymbolRuns<'a> = Vec<(u32, Option<&'a str>)>;

/// Count `count` rows of `symbol` into the runs: more of the last run, or a new run.
fn add_to_runs<'a>(runs: &mut SymbolRuns<'a>, count: u32, symbol: Option<&'a str>) {
    match runs.last_mut() {
        Some((counted, last)) if *last == symbol => *counted += count,
        _ => runs.push((count, symbol)),
    }
}

/// The bytes after the columns: the runs' count, then each run's count, its length plus one or `0` for
/// none, and its bytes.
fn append_runs(out: &mut Vec<u8>, runs: &SymbolRuns<'_>) {
    out.extend_from_slice(&(runs.len() as u32).to_le_bytes());
    for (count, symbol) in runs {
        out.extend_from_slice(&count.to_le_bytes());
        let len = symbol.map_or(0, |s| s.len() as u32 + 1);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(symbol.map_or(&[][..], str::as_bytes));
    }
}

/// `rows` as the `magic` reply's columns body APPENDED to `out` in ONE pass over the rows: the columns' room is
/// made once, each row's words stored in their columns' eight-byte slots, its flag byte, its levels and its
/// symbol run on the way, and the runs appended after.
///
/// # Panics
/// On more than `u32::MAX` rows ([`head`]).
pub fn encode_columns_into<R: ColumnRow>(out: &mut Vec<u8>, magic: &ColumnsMagic, rows: &[R]) {
    encode_rows_into(out, magic, rows.len(), rows.iter());
}

/// The bar sandbox relay's spelling of [`encode_columns_into`] (decision 0281 names it): `rows` as a
/// bar-columns body appended to `out`.
pub fn encode_bar_columns_into(out: &mut Vec<u8>, rows: &[Bar]) {
    encode_columns_into(out, &BAR_COLUMNS, rows);
}

/// The rows `cut` of each of `runs`, each a plain `magic` body of `R` as the encoders write it, read as ONE: their
/// body APPENDED to `out`, byte for byte the one [`encode_columns_into`] writes for those rows, made by copying each
/// run's stretch of every column, its flags, its levels and its runs of equal text (a run of one value that goes on
/// across two bodies is one run), no row decoded or encoded. The backtesthub's bar and tick cache keeps its days as
/// bodies and relays a read's parts so. A body that is not exactly one of `magic`, or a cut past its rows, is
/// refused and appends nothing.
///
/// ⚠ A book cut's levels are found by summing the level counts of the rows before it, so a cut deep into a long
/// book body costs that body's rows: a keeper of long days keeps them in bodies of a bounded length.
///
/// # Panics
/// On more than `u32::MAX` rows ([`head`]).
pub fn splice_columns_into<R: ColumnRow>(
    out: &mut Vec<u8>,
    magic: &ColumnsMagic,
    runs: &[(&[u8], std::ops::Range<usize>)],
) -> Result<(), String> {
    splice_columns_within::<R>(out, magic, runs, usize::MAX).map(drop)
}

/// [`splice_columns_into`] that first counts the body's exact length (its rows' [`columns_len`], from the cut
/// layouts) and appends NOTHING, answering `Ok(false)`, when it would pass `most` bytes: the backtest sandbox's relay
/// refuses a part over its frame cap before a byte of it is built. `Ok(true)`: the body is appended.
///
/// # Panics
/// On more than `u32::MAX` rows ([`head`]).
pub fn splice_columns_within<R: ColumnRow>(
    out: &mut Vec<u8>,
    magic: &ColumnsMagic,
    runs: &[(&[u8], std::ops::Range<usize>)],
    most: usize,
) -> Result<bool, String> {
    let mut cuts = Vec::with_capacity(runs.len());
    for (body, cut) in runs {
        let layout = layout::<R>(body, magic)?;
        if cut.start > cut.end || cut.end > layout.n {
            let n = layout.n;
            return Err(format!("a cut {cut:?} of a {}-columns body of {n} rows", magic.name));
        }
        let before = layout.levels_in::<R>(0..cut.start);
        let levels = before..before + layout.levels_in::<R>(cut.clone());
        cuts.push((layout, cut.clone(), levels));
    }
    // The symbol column's runs, then each text column's, joined across the bodies: a run of one value that goes on
    // across two bodies is one run.
    let joined: Vec<SymbolRuns> = (0..=R::TEXTS)
        .map(|column| {
            let mut joined = SymbolRuns::new();
            for (layout, cut, _) in &cuts {
                let mut start = 0;
                for &(count, value) in &layout.runs[column] {
                    let (from, to) = (start.max(cut.start), (start + count).min(cut.end));
                    if from < to {
                        add_to_runs(&mut joined, (to - from) as u32, value);
                    }
                    start += count;
                }
            }
            joined
        })
        .collect();
    let rows: usize = cuts.iter().map(|(_, cut, _)| cut.len()).sum();
    let levels: usize = cuts.iter().map(|(_, _, levels)| levels.len()).sum();
    let texts: usize = joined
        .iter()
        .map(|runs| {
            RUNS + runs.iter().map(|(_, v)| RUN_HEAD + v.map_or(0, str::len)).sum::<usize>()
        })
        .sum();
    let len = HEAD + row_bytes::<R>() * rows + LEVEL * levels + texts;
    if len > most {
        return Ok(false);
    }
    out.reserve(len);
    out.extend_from_slice(&head(magic, rows));
    for c in 0..R::WORDS {
        for (layout, cut, _) in &cuts {
            let column = &layout.words[c * layout.n..(c + 1) * layout.n];
            out.extend_from_slice(column[cut.clone()].as_flattened());
        }
    }
    for (layout, cut, _) in &cuts {
        out.extend_from_slice(&layout.flags[cut.clone()]);
    }
    for (layout, _, levels) in &cuts {
        out.extend_from_slice(layout.prices[levels.clone()].as_flattened());
    }
    for (layout, _, levels) in &cuts {
        out.extend_from_slice(layout.quantities[levels.clone()].as_flattened());
    }
    for runs in &joined {
        append_runs(out, runs);
    }
    Ok(true)
}

/// Word `word` of row `row` of a plain columns body, read where it lies (the columns are `rows` words each, one
/// after another, behind the head), checking nothing else of the body: `None` when either index passes what the
/// body's head and length hold.
pub fn columns_word(body: &[u8], word: usize, row: usize) -> Option<u64> {
    let rows = columns_rows(body)?;
    if row >= rows {
        return None;
    }
    let at = word.checked_mul(rows)?.checked_add(row)?.checked_mul(8)?.checked_add(HEAD)?;
    Some(u64::from_le_bytes(body.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// The rows a plain columns body's head states, checking nothing else of it: `None` when it holds no head.
pub fn columns_rows(body: &[u8]) -> Option<usize> {
    Some(u32::from_le_bytes(body.get(4..HEAD)?.try_into().ok()?) as usize)
}

/// The one pass [`encode_columns_into`] makes: `rows` yields exactly `n` rows (asked twice for a kind with levels,
/// whose count sizes the body first).
fn encode_rows_into<'a, R: ColumnRow>(
    out: &mut Vec<u8>,
    magic: &ColumnsMagic,
    n: usize,
    rows: impl Iterator<Item = &'a R> + Clone,
) {
    const { assert!(R::WORDS <= MAX_COLUMN_WORDS && R::TEXTS <= MAX_TEXT_COLUMNS) };
    let levels: usize =
        if R::LEVEL_COUNTS.is_some() { rows.clone().map(level_count).sum() } else { 0 };
    let columns_len = row_bytes::<R>() * n + LEVEL * levels;
    out.reserve(HEAD + columns_len + RUNS + RUN_HEAD);
    out.extend_from_slice(&head(magic, n));
    let start = out.len();
    out.resize(start + columns_len, 0);
    let mut runs = SymbolRuns::new();
    let mut text_runs: [SymbolRuns; MAX_TEXT_COLUMNS] = std::array::from_fn(|_| SymbolRuns::new());
    if n > 0 {
        let (words, rest) = out[start..].split_at_mut(8 * R::WORDS * n);
        let (flags, rest) = rest.split_at_mut(n);
        let (prices, quantities) = rest.split_at_mut(8 * levels);
        let (slots, _) = words.as_chunks_mut::<8>();
        let (prices, _) = prices.as_chunks_mut::<8>();
        let (quantities, _) = quantities.as_chunks_mut::<8>();
        let mut columns = slots.chunks_exact_mut(n);
        let mut columns: [&mut [[u8; 8]]; MAX_COLUMN_WORDS] =
            std::array::from_fn(|_| columns.next().unwrap_or_default());
        let mut level = 0;
        for (i, (row, flag)) in rows.zip(flags.iter_mut()).enumerate() {
            for (column, word) in columns.iter_mut().zip(row.words()).take(R::WORDS) {
                column[i] = word.to_le_bytes();
            }
            *flag = row.flags();
            for l in row.levels().into_iter().flatten() {
                prices[level] = l.price.to_bits().to_le_bytes();
                quantities[level] = l.qty.to_bits().to_le_bytes();
                level += 1;
            }
            add_to_runs(&mut runs, 1, row.symbol());
            if R::TEXTS > 0 {
                for (runs, text) in text_runs.iter_mut().zip(row.texts()).take(R::TEXTS) {
                    add_to_runs(runs, 1, text);
                }
            }
        }
    }
    append_runs(out, &runs);
    for runs in &text_runs[..R::TEXTS] {
        append_runs(out, runs);
    }
}

/// A columns `body` LZ4-compressed for a client that asked ([`BarColumnsBody::Lz4`]): the reply's
/// [`ColumnsMagic::lz4`], the body's length as a `u32`, then one LZ4 block of it.
pub fn compress_columns(magic: &ColumnsMagic, body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(body.len()).expect("a body is under MAX_FRAME_LEN");
    let block = lz4_flex::block::compress(body);
    let mut out = Vec::with_capacity(8 + block.len());
    out.extend_from_slice(&magic.lz4);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&block);
    out
}

/// The bar sandbox relay's spelling of [`decode_columns_into`] (decision 0281 names it): a bar-columns body,
/// plain or compressed, appended to `out`.
pub fn decode_bar_columns_into(body: &[u8], out: &mut Vec<Bar>) -> Result<usize, String> {
    decode_columns_into(body, &BAR_COLUMNS, out)
}

/// A reply to a `magic` columns read back as its rows APPENDED to `out` (a pager's whole read, a relay's
/// answer), so no page is built apart and copied after: a plain body ([`ColumnsMagic::plain`]) or a
/// compressed one ([`ColumnsMagic::lz4`]), whichever the server sent (it may answer plain to a client that
/// asked for LZ4); the count appended. Anything else, or a body that does not decode, is refused (the module
/// doc's ⚠) and appends NOTHING.
///
/// ⚠ A compressed body's stated length is checked against [`MAX_FRAME_LEN`] BEFORE the buffer is made, so a
/// garbled or hostile length cannot make the client allocate what no frame could carry.
pub fn decode_columns_into<R: ColumnRow>(
    body: &[u8],
    magic: &ColumnsMagic,
    out: &mut Vec<R>,
) -> Result<usize, String> {
    match decompressed(body, magic)? {
        Some(plain) => decode_plain(&plain, magic, out),
        None => decode_plain(body, magic, out),
    }
}

/// A reply to a `magic` columns read as its PLAIN body, not its rows: as it came, or its LZ4 body decompressed under
/// [`decode_columns_into`]'s bound. What a reader that KEEPS the columns asks (`HistoryStoreDatahub`'s `read_columns`,
/// for the backtesthub's cache). Only its magic is checked here, so a body under another reply's magic is refused;
/// the reader checks the rest before anything reads it ([`check_columns`]: `HistoryStoreDatahub` does, where each page
/// arrives).
pub fn plain_columns(body: Vec<u8>, magic: &ColumnsMagic) -> Result<Vec<u8>, String> {
    let plain = decompressed(&body, magic)?.unwrap_or(body);
    if plain.get(..4) != Some(&magic.plain[..]) {
        return Err(format!(
            "a malformed {name}-columns reply ({} bytes): it does not open with the {name}-columns magic",
            plain.len(),
            name = magic.name
        ));
    }
    Ok(plain)
}

/// An LZ4 `magic` body decompressed, its stated length checked against [`MAX_FRAME_LEN`] before the buffer is made;
/// `None` for any other body, which is read as it is.
fn decompressed(body: &[u8], magic: &ColumnsMagic) -> Result<Option<Vec<u8>>, String> {
    let Some(packed) = body.strip_prefix(&magic.lz4[..]) else {
        return Ok(None);
    };
    let malformed = |why: &str| {
        format!("a malformed {}-columns reply ({} bytes): {why}", magic.name, body.len())
    };
    let (len, block) =
        packed.split_at_checked(4).ok_or_else(|| malformed("it ends inside its head"))?;
    let len = u32::from_le_bytes(len.try_into().expect("four bytes"));
    if len > MAX_FRAME_LEN {
        return Err(malformed("it states a body over MAX_FRAME_LEN"));
    }
    let mut plain = vec![0u8; len as usize];
    match lz4_flex::block::decompress_into(block, &mut plain) {
        Ok(n) if n == plain.len() => Ok(Some(plain)),
        Ok(_) => Err(malformed("its LZ4 block is shorter than the length it states")),
        Err(e) => Err(malformed(&format!("its LZ4 block does not decompress ({e})"))),
    }
}

/// Every check [`decode_columns_into`] makes of a PLAIN `magic` `body` of `R`, with no row built: its lengths, flag
/// bits and runs ([`layout`]), the text its runs expand to ([`check_expansion`]) and each row's own fields
/// ([`ColumnRow::check_row`]), over the same walk the decoder makes ([`each_row`]); the rows it holds. What a reader
/// that KEEPS a body it did not encode asks first (the backtesthub's cache of its datahub's pages), so a body it keeps
/// decodes.
pub fn check_columns<R: ColumnRow>(body: &[u8], magic: &ColumnsMagic) -> Result<usize, String> {
    let malformed = |why: &str| {
        format!("a malformed {}-columns reply ({} bytes): {why}", magic.name, body.len())
    };
    let layout = layout::<R>(body, magic)?;
    check_expansion(&layout).map_err(malformed)?;
    each_row::<R>(&layout, |words, flags, symbol, texts| R::check_row(words, flags, symbol, texts))
        .map_err(malformed)?;
    Ok(layout.n)
}

/// A plain body's parts, every length checked ([`layout`]).
struct Layout<'a> {
    /// The rows.
    n: usize,
    /// The words, one column after another, `n` slots each.
    words: &'a [[u8; 8]],
    flags: &'a [u8],
    /// Every level's price, then every level's quantity.
    prices: &'a [[u8; 8]],
    quantities: &'a [[u8; 8]],
    /// The symbol column's runs of equal values, then each text column's: a count and the value.
    runs: Vec<Vec<(usize, Option<&'a str>)>>,
}

impl Layout<'_> {
    fn word(&self, c: usize, i: usize) -> u64 {
        u64::from_le_bytes(self.words[c * self.n + i])
    }

    /// The levels the rows `rows` carry, bids and asks: 0 for a kind without levels.
    fn levels_in<R: ColumnRow>(&self, rows: std::ops::Range<usize>) -> usize {
        R::LEVEL_COUNTS.map_or(0, |[bids, asks]| {
            rows.map(|i| (self.word(bids, i) + self.word(asks, i)) as usize).sum()
        })
    }
}

/// A PLAIN `magic` `body` of `R` read into its parts, refusing any body that is not exactly one (the module doc's ⚠):
/// what [`decode_plain`] and [`splice_columns_into`] read.
fn layout<'a, R: ColumnRow>(body: &'a [u8], magic: &ColumnsMagic) -> Result<Layout<'a>, String> {
    const { assert!(R::WORDS <= MAX_COLUMN_WORDS && R::TEXTS <= MAX_TEXT_COLUMNS) };
    let malformed = |why: &str| {
        format!("a malformed {}-columns reply ({} bytes): {why}", magic.name, body.len())
    };
    let mut at = BodyReader::new(body);
    if at.take(4) != Some(&magic.plain[..]) {
        return Err(malformed(&format!("it does not open with the {}-columns magic", magic.name)));
    }
    let n = at.u32().ok_or_else(|| malformed("it ends inside its head"))? as usize;
    let words = n
        .checked_mul(8 * R::WORDS)
        .and_then(|len| at.take(len))
        .ok_or_else(|| malformed("it ends inside its columns"))?;
    let flags = at.take(n).ok_or_else(|| malformed("it ends inside its columns"))?;
    if flags.iter().any(|f| f & !R::FLAGS != 0) {
        return Err(malformed("a flag byte sets a bit its kind has no use for"));
    }
    let (words, _) = words.as_chunks::<8>();
    let word = |c: usize, i: usize| u64::from_le_bytes(words[c * n + i]);
    let levels = match R::LEVEL_COUNTS {
        None => Some(0),
        Some([bids, asks]) => (0..n).try_fold(0usize, |sum, i| {
            let bids = usize::try_from(word(bids, i)).ok()?;
            let asks = usize::try_from(word(asks, i)).ok()?;
            sum.checked_add(bids.checked_add(asks)?)
        }),
    };
    let level_bytes = levels
        .and_then(|l| l.checked_mul(8))
        .ok_or_else(|| malformed("its level counts pass what a body can hold"))?;
    let prices = at.take(level_bytes).ok_or_else(|| malformed("it ends inside its levels"))?;
    let quantities = at.take(level_bytes).ok_or_else(|| malformed("it ends inside its levels"))?;
    // The symbol column, then each text column: runs of equal values that cover the rows exactly.
    let mut runs_of_columns: Vec<Vec<(usize, Option<&str>)>> = Vec::with_capacity(1 + R::TEXTS);
    for _ in 0..=R::TEXTS {
        let runs = at.u32().ok_or_else(|| malformed("it ends before its symbol runs"))?;
        let mut column: Vec<(usize, Option<&str>)> = Vec::new();
        let mut covered = 0usize;
        for _ in 0..runs {
            let (Some(count), Some(len)) = (at.u32(), at.u32()) else {
                return Err(malformed("it ends inside a symbol run"));
            };
            let value = match len {
                0 => None,
                len => {
                    let bytes = at
                        .take(len as usize - 1)
                        .ok_or_else(|| malformed("it ends inside a symbol"))?;
                    Some(
                        std::str::from_utf8(bytes)
                            .map_err(|_| malformed("a symbol is not UTF-8"))?,
                    )
                }
            };
            covered = covered.saturating_add(count as usize);
            column.push((count as usize, value));
        }
        if covered != n {
            return Err(malformed("its symbol runs do not cover its rows"));
        }
        runs_of_columns.push(column);
    }
    if !at.is_done() {
        return Err(malformed("bytes follow its last symbol run"));
    }
    let (prices, _) = prices.as_chunks::<8>();
    let (quantities, _) = quantities.as_chunks::<8>();
    Ok(Layout { n, words, flags, prices, quantities, runs: runs_of_columns })
}

/// A PLAIN columns `body`'s rows appended to `out`: the inverse of the encoders, refusing any body that is
/// not exactly one; a row that does not decode takes back every row this call appended.
fn decode_plain<R: ColumnRow>(
    body: &[u8],
    magic: &ColumnsMagic,
    out: &mut Vec<R>,
) -> Result<usize, String> {
    let malformed = |why: &str| {
        format!("a malformed {}-columns reply ({} bytes): {why}", magic.name, body.len())
    };
    let layout = layout::<R>(body, magic)?;
    let (n, prices, quantities) = (layout.n, layout.prices, layout.quantities);
    check_expansion(&layout).map_err(malformed)?;
    let held = out.len();
    out.reserve(n);
    let mut level = 0usize;
    let walked = each_row::<R>(&layout, |words, flags, symbol, texts| {
        let sides = R::LEVEL_COUNTS.map_or_else(Default::default, |counts| {
            counts.map(|c| {
                let k = words[c] as usize;
                let side: Vec<L2BookLevel> = (level..level + k)
                    .map(|l| {
                        let bits = |slot: [u8; 8]| f64::from_bits(u64::from_le_bytes(slot));
                        L2BookLevel::new(bits(prices[l]), bits(quantities[l]))
                    })
                    .collect();
                level += k;
                side
            })
        });
        out.push(R::from_columns(words, flags, sides, symbol, texts)?);
        Ok(())
    });
    if let Err(why) = walked {
        out.truncate(held);
        return Err(malformed(why));
    }
    Ok(n)
}

/// ⚠ What the rows' strings will take once `layout`'s runs are expanded: a run's value is COPIED into every row it
/// covers, and both its count and its length come off the wire, so a 63 MB body (a few hundred KB as LZ4) could ask
/// for terabytes. No reply carries more text than one frame (a JSON reply spells every row's strings), so a body whose
/// runs expand past `MAX_FRAME_LEN` is refused before anything is copied.
fn check_expansion(layout: &Layout<'_>) -> Result<(), &'static str> {
    let expanded = layout.runs.iter().flatten().try_fold(0usize, |total, (count, value)| {
        count.checked_mul(value.map_or(0, str::len))?.checked_add(total)
    });
    if expanded.is_none_or(|total| total > MAX_FRAME_LEN as usize) {
        return Err("its runs expand to more text than one reply can carry");
    }
    Ok(())
}

/// Each row of a checked `layout` in order, handed to `row` as its words, its flag byte, its symbol and its text
/// columns, the runs expanded as the walk goes: the ONE walk the decoder and [`check_columns`] make, so the check reads
/// every row as the decode does. The first `Err` ends it. Every length is checked by [`layout`], so each word is an
/// eight-byte slot of its column.
fn each_row<'a, R: ColumnRow>(
    layout: &Layout<'a>,
    mut row: impl FnMut(
        &[u64; MAX_COLUMN_WORDS],
        u8,
        Option<&'a str>,
        [Option<&'a str>; MAX_TEXT_COLUMNS],
    ) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let mut values: Vec<_> = layout
        .runs
        .iter()
        .map(|runs| runs.iter().flat_map(|(count, s)| std::iter::repeat_n(*s, *count)))
        .collect();
    for (i, flags) in layout.flags.iter().enumerate() {
        let mut words = [0u64; MAX_COLUMN_WORDS];
        for (c, w) in words.iter_mut().enumerate().take(R::WORDS) {
            *w = layout.word(c, i);
        }
        let symbol = values[0].next().flatten();
        let mut texts = [None; MAX_TEXT_COLUMNS];
        for (text, column) in texts.iter_mut().zip(&mut values[1..]) {
            *text = column.next().flatten();
        }
        row(&words, *flags, symbol, texts)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "bar_columns_tests.rs"]
mod tests;
