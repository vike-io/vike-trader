//! Pure CP Gateway market-data JSON → vike-model decoders: the `smd` stream rows, the
//! `/iserver/marketdata/snapshot` rows and the `/iserver/marketdata/history` body. No I/O, no
//! threads, no clock except the receive stamp of a row that carries none. Fixture-tested offline in
//! `crates/bridges/vike-ibkr/tests/ibkr_cpapi_mktdata_decode.rs` and held to the crate's totality
//! harness (`crates/bridges/vike-ibkr/tests/decoder_never_panics.rs`).
//!
//! ## ⚠ UNVERIFIED wire shapes: IBKR's public Client Portal API documentation, not a capture
//!
//! Nothing in this file has been read off a live gateway yet. The field codes, the row shapes,
//! the availability letters and the history body are transcribed from IBKR's public CP Web API
//! docs; `crates/bridges/vike-ibkr/tests/ibkr_cpapi_mktdata_smoke.rs` is the read-only vehicle
//! that retires them (it prints the raw rows). Until it has run, read every constant below as a
//! documented claim:
//!
//! | field | meaning (per IBKR's docs) | read as |
//! |---|---|---|
//! | `31` | last price; a leading `C` = the prior CLOSE, `H` = HALTED | a trade print only when unprefixed |
//! | `84` / `86` | bid / ask price | quote sides |
//! | `88` / `85` | bid / ask SIZE (note the crossed order: `85` is the ASK size) | quote sizes |
//! | `7059` | last size | the print's size |
//! | `6509` | market-data availability: first letter `R` real-time, `D` delayed, `Z` frozen, `Y` delayed-frozen, `N` not subscribed | [`availability_delay`] |
//! | `_updated` | the row's epoch-ms stamp | the tick's `ts` |
//!
//! A stream row carries only the fields that CHANGED since the last row for that conId, so the
//! accumulator keeps every field sticky — an absent `7059` means "the same size", not "zero".
//! Numbers may arrive as JSON numbers or as strings with thousands separators (`"1,100"`) and,
//! for sizes, a `K`/`M`/`B` magnitude suffix.
//!
//! ## The delayed marker
//!
//! Decision 0140 makes every IBKR stream say whether it is delayed, read off each row as it
//! arrives. Here that is `6509`'s first letter, decoded straight to the workspace word
//! `vike_model::DataDelay` by [`availability_delay`] and carried on every
//! [`CpapiQuote`]/[`CpapiTrade`] (`None` until a row carries a marker this decoder reads). Frozen
//! and delayed-frozen both read as `Frozen`; a delayed row states no lag. The ONE site that sends
//! it to the sink, at first knowledge and on change only, is the cpapi pump's `note_delay`
//! (`crates/bridges/vike-ibkr/src/cpapi_feed/pump.rs`).

use serde_json::Value;
use vike_marketdata::{Bar, QuoteTick, TradeTick};
use vike_model::DataDelay;

/// The `smd` field codes this client asks for, in the order they are sent. See the module doc's
/// table for what each one is claimed to mean.
pub const SMD_FIELDS: &[&str] = &["31", "84", "85", "86", "88", "7059", "6509"];

const F_LAST: &str = "31";
const F_BID: &str = "84";
const F_ASK_SIZE: &str = "85";
const F_ASK: &str = "86";
const F_BID_SIZE: &str = "88";
const F_LAST_SIZE: &str = "7059";
const F_AVAILABILITY: &str = "6509";
const F_UPDATED: &str = "_updated";

/// The quote/price fields a snapshot row must carry at least one of to be data rather than the
/// gateway's first-request "preflight" answer (a row naming only the conId).
const DATA_FIELDS: &[&str] = &[F_LAST, F_BID, F_ASK_SIZE, F_ASK, F_BID_SIZE, F_LAST_SIZE];

/// The workspace delay label an availability string states (`6509`'s FIRST letter, or a history
/// body's `mdAvailability`): `R` → [`DataDelay::RealTime`], `D` → [`DataDelay::DELAYED`] (a row
/// states no lag), `Z` (frozen) and `Y` (delayed-frozen) → [`DataDelay::Frozen`]. `N` (no
/// market-data subscription for the conId) and any other letter are `None`: no label, never a
/// guess. The later letters (snapshot/consolidated, book) are not read.
#[must_use]
pub fn availability_delay(code: &str) -> Option<DataDelay> {
    match code.trim().chars().next() {
        Some('R') => Some(DataDelay::RealTime),
        Some('D') => Some(DataDelay::DELAYED),
        Some('Z' | 'Y') => Some(DataDelay::Frozen),
        _ => None,
    }
}

/// One L1 quote decoded off the stream (or a snapshot), with the delay label in force when it was
/// built (`None` until a row has carried a marker this decoder reads).
#[derive(Clone, Debug, PartialEq)]
pub struct CpapiQuote {
    pub quote: QuoteTick,
    pub delay: Option<DataDelay>,
}

/// One last-price print decoded off the stream. ⚠ NOT a tape: `smd` is a conflating snapshot
/// stream, so two prints between rows arrive as one, and the first row after a subscribe carries
/// the most recent print, not a new one.
#[derive(Clone, Debug, PartialEq)]
pub struct CpapiTrade {
    pub trade: TradeTick,
    pub delay: Option<DataDelay>,
}

/// What one row changed: at most one quote and at most one print, plus the label in force.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SmdUpdate {
    pub quote: Option<CpapiQuote>,
    pub trade: Option<CpapiTrade>,
    pub delay: Option<DataDelay>,
}

/// Folds `smd` rows (and snapshot rows, which share the field codes) for ONE conId into a running
/// quote, keeping every field sticky because a row carries only what changed. A quote is emitted
/// when a quote field arrives and both a bid and an ask price are known; a print when an
/// UNPREFIXED last price arrives (a `C` close or `H` halted last is a level, not a trade).
#[derive(Clone, Debug, Default)]
pub struct SmdAccumulator {
    bid: Option<f64>,
    ask: Option<f64>,
    bid_size: f64,
    ask_size: f64,
    last_size: f64,
    delay: Option<DataDelay>,
}

impl SmdAccumulator {
    /// Apply one row. `local_ts` is the receive stamp; a row with no usable `_updated` is stamped
    /// with it too.
    pub fn apply(&mut self, row: &Value, local_ts: i64) -> SmdUpdate {
        if let Some(code) = row.get(F_AVAILABILITY).and_then(Value::as_str) {
            self.delay = availability_delay(code);
        }
        let ts = row.get(F_UPDATED).and_then(Value::as_i64).filter(|t| *t > 0).unwrap_or(local_ts);

        let mut quote_changed = false;
        if let Some((px, _)) = row.get(F_BID).and_then(parse_md_price) {
            self.bid = Some(px);
            quote_changed = true;
        }
        if let Some((px, _)) = row.get(F_ASK).and_then(parse_md_price) {
            self.ask = Some(px);
            quote_changed = true;
        }
        if let Some(sz) = row.get(F_BID_SIZE).and_then(parse_md_size) {
            self.bid_size = sz;
            quote_changed = true;
        }
        if let Some(sz) = row.get(F_ASK_SIZE).and_then(parse_md_size) {
            self.ask_size = sz;
            quote_changed = true;
        }
        if let Some(sz) = row.get(F_LAST_SIZE).and_then(parse_md_size) {
            self.last_size = sz;
        }

        let quote = if quote_changed { self.quote_at(ts, local_ts) } else { None };
        let trade = match row.get(F_LAST).and_then(parse_md_price) {
            Some((price, PriceMark::Traded)) => Some(CpapiTrade {
                trade: TradeTick {
                    ts,
                    local_ts,
                    price,
                    size: self.last_size,
                    // `smd` names no aggressor side.
                    is_buyer_maker: false,
                    symbol: String::new(),
                },
                delay: self.delay,
            }),
            _ => None,
        };
        SmdUpdate { quote, trade, delay: self.delay }
    }

    /// The quote the accumulator holds now, if both sides are known — what a second stream on an
    /// already-subscribed conId is handed first.
    #[must_use]
    pub fn current_quote(&self, local_ts: i64) -> Option<CpapiQuote> {
        self.quote_at(local_ts, local_ts)
    }

    /// The label in force (`None` until a row carried a marker this decoder reads).
    #[must_use]
    pub fn delay(&self) -> Option<DataDelay> {
        self.delay
    }

    fn quote_at(&self, ts: i64, local_ts: i64) -> Option<CpapiQuote> {
        let (bid, ask) = (self.bid?, self.ask?);
        Some(CpapiQuote {
            quote: QuoteTick {
                ts,
                local_ts,
                bid,
                ask,
                bid_size: self.bid_size,
                ask_size: self.ask_size,
                symbol: String::new(),
            },
            delay: self.delay,
        })
    }
}

/// What a price field's prefix letter says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriceMark {
    /// No prefix: a traded/quoted value.
    Traded,
    /// `C`: the prior session's close, shown while there is no print yet.
    Close,
    /// `H`: the instrument is halted.
    Halted,
}

/// A price field: a JSON number, or a string optionally prefixed `C`/`H` and possibly carrying
/// thousands separators. `None` for anything that is not a finite number.
#[must_use]
pub fn parse_md_price(v: &Value) -> Option<(f64, PriceMark)> {
    if let Some(n) = v.as_f64() {
        return n.is_finite().then_some((n, PriceMark::Traded));
    }
    let s = v.as_str()?.trim();
    let (mark, rest) = match s.chars().next()? {
        'C' => (PriceMark::Close, &s[1..]),
        'H' => (PriceMark::Halted, &s[1..]),
        _ => (PriceMark::Traded, s),
    };
    let n: f64 = rest.trim().replace(',', "").parse().ok()?;
    n.is_finite().then_some((n, mark))
}

/// A size field: a JSON number, or a string with thousands separators and an optional `K`/`M`/`B`
/// magnitude suffix. `None` for anything that is not a finite, non-negative number.
#[must_use]
pub fn parse_md_size(v: &Value) -> Option<f64> {
    let n = if let Some(n) = v.as_f64() {
        n
    } else {
        let s = v.as_str()?.trim().replace(',', "");
        let (digits, scale) = match s.chars().last()? {
            'K' | 'k' => (&s[..s.len() - 1], 1e3),
            'M' | 'm' => (&s[..s.len() - 1], 1e6),
            'B' | 'b' => (&s[..s.len() - 1], 1e9),
            _ => (s.as_str(), 1.0),
        };
        digits.trim().parse::<f64>().ok()? * scale
    };
    (n.is_finite() && n >= 0.0).then_some(n)
}

/// The text frame that subscribes `conid` to the `smd` topic with [`SMD_FIELDS`]:
/// `smd+<conid>+{"fields":[...]}` (IBKR's documented spelling, unverified here).
#[must_use]
pub fn smd_subscribe(conid: i64) -> String {
    let fields: Vec<String> = SMD_FIELDS.iter().map(|f| format!("\"{f}\"")).collect();
    format!("smd+{conid}+{{\"fields\":[{}]}}", fields.join(","))
}

/// The text frame that ends `conid`'s `smd` subscription: `umd+<conid>+{}`.
#[must_use]
pub fn smd_unsubscribe(conid: i64) -> String {
    format!("umd+{conid}+{{}}")
}

/// The conId an `smd` stream row is about: the topic's suffix (`"smd+265598"`), else the row's
/// `conid` field. `None` for a frame on any other topic.
#[must_use]
pub fn smd_conid(frame: &Value) -> Option<i64> {
    let topic = frame.get("topic").and_then(Value::as_str)?;
    let rest = topic.strip_prefix("smd")?;
    rest.strip_prefix('+')
        .and_then(|id| id.split('+').next())
        .and_then(|id| id.trim().parse::<i64>().ok())
        .or_else(|| row_conid(frame))
}

/// A row's `conid`, as a JSON number or a numeric string.
fn row_conid(row: &Value) -> Option<i64> {
    let c = row.get("conid")?;
    c.as_i64().or_else(|| c.as_str()?.trim().parse().ok())
}

/// The `/iserver/marketdata/snapshot` row for `conid` when it carries data, or `None` (no row for
/// it, or only the first request's preflight row naming the conId and nothing else — IBKR
/// documents that the first snapshot request for a conId primes the stream and returns no fields).
#[must_use]
pub fn snapshot_row(body: &Value, conid: i64) -> Option<&Value> {
    body.as_array()?.iter().find(|row| {
        row_conid(row) == Some(conid) && DATA_FIELDS.iter().any(|f| row.get(f).is_some())
    })
}

/// The `fields=` query value for a snapshot request: [`SMD_FIELDS`] comma-joined.
#[must_use]
pub fn snapshot_fields() -> String {
    SMD_FIELDS.join(",")
}

/// A decoded `/iserver/marketdata/history` body.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CpapiHistory {
    /// The bars, ascending by `ts`, one per timestamp (a duplicate stamp keeps the later row).
    pub bars: Vec<Bar>,
    /// The body's delay label: [`DataDelay::DELAYED`] when `mktDataDelay` is positive, else
    /// `mdAvailability`'s first letter through [`availability_delay`]. The lag stays `None`:
    /// `mktDataDelay`'s unit is unknown (below), and an unknown unit is not a stated lag.
    pub delay: Option<DataDelay>,
    /// The raw `mktDataDelay` number when positive. ⚠ Its UNIT is not documented anywhere this
    /// crate has read, so it is carried as the gateway sent it and never becomes
    /// `DataDelay::Delayed`'s `lag_ms` until a capture settles it.
    pub mkt_data_delay: Option<i64>,
}

/// Decode a history body: `{"data":[{"t":ms,"o":..,"h":..,"l":..,"c":..,"v":..}], "mdAvailability":
/// "..", "mktDataDelay": n, ...}` per IBKR's docs. A row without a positive `t` or with a
/// non-finite price is skipped; a missing or unparseable volume is `0`.
#[must_use]
pub fn decode_history(body: &Value) -> CpapiHistory {
    let mkt_data_delay = body.get("mktDataDelay").and_then(Value::as_i64).filter(|d| *d > 0);
    let delay = if mkt_data_delay.is_some() {
        Some(DataDelay::DELAYED)
    } else {
        body.get("mdAvailability").and_then(Value::as_str).and_then(availability_delay)
    };
    let mut bars: Vec<Bar> = body
        .get("data")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(history_bar).collect())
        .unwrap_or_default();
    bars.sort_by_key(|b| b.ts);
    // Keep the LAST row of a duplicated stamp: reverse, dedup (keeps the first of each run), undo.
    bars.reverse();
    bars.dedup_by_key(|b| b.ts);
    bars.reverse();
    CpapiHistory { bars, delay, mkt_data_delay }
}

fn history_bar(row: &Value) -> Option<Bar> {
    let ts = row.get("t").and_then(Value::as_i64).filter(|t| *t > 0)?;
    let px = |k: &str| row.get(k).and_then(parse_md_price).map(|(p, _)| p);
    Some(Bar {
        ts,
        open: px("o")?,
        high: px("h")?,
        low: px("l")?,
        close: px("c")?,
        volume: row.get("v").and_then(parse_md_size).unwrap_or(0.0),
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    })
}

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

/// The history endpoint's `bar=` word that is EXACTLY `interval_ms` long, or `None` when the
/// endpoint documents no bar of that length (exact, never "closest" — the same rule as the socket
/// backend's `hist_bar_size`: seeded bars are filed under the caller's interval).
#[must_use]
pub fn history_bar_param(interval_ms: i64) -> Option<&'static str> {
    Some(match interval_ms {
        x if x == MIN => "1min",
        x if x == 2 * MIN => "2min",
        x if x == 3 * MIN => "3min",
        x if x == 5 * MIN => "5min",
        x if x == 10 * MIN => "10min",
        x if x == 15 * MIN => "15min",
        x if x == 30 * MIN => "30min",
        x if x == HOUR => "1h",
        x if x == 2 * HOUR => "2h",
        x if x == 3 * HOUR => "3h",
        x if x == 4 * HOUR => "4h",
        x if x == 8 * HOUR => "8h",
        x if x == DAY => "1d",
        x if x == WEEK => "1w",
        _ => return None,
    })
}

/// IBKR documents a 1,000-point ceiling per history request.
const MAX_HISTORY_POINTS: i64 = 1_000;

/// The `period=` word for a seed of `interval_ms` bars: for an intraday bar, the longest of one
/// day, `8h`, `4h`, `2h`, `1h`, `30min` that stays within [`MAX_HISTORY_POINTS`] (the socket
/// backend seeds one day); for a daily or longer bar, a hundred bars, capped at the 1,000-day
/// ceiling the endpoint documents for `d`.
#[must_use]
pub fn history_period(interval_ms: i64) -> String {
    let interval_ms = interval_ms.max(1);
    if interval_ms < DAY {
        const LADDER: [(i64, &str); 6] = [
            (DAY, "1d"),
            (8 * HOUR, "8h"),
            (4 * HOUR, "4h"),
            (2 * HOUR, "2h"),
            (HOUR, "1h"),
            (30 * MIN, "30min"),
        ];
        return LADDER
            .iter()
            .find(|(span, _)| span / interval_ms <= MAX_HISTORY_POINTS)
            .map_or("30min", |&(_, word)| word)
            .to_string();
    }
    let days = (100 * (interval_ms / DAY)).min(MAX_HISTORY_POINTS);
    format!("{days}d")
}
