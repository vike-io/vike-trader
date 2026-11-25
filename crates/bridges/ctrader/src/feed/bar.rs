//! **A live trendbar's CLOSE, read off the stream** (decision 0271's live half: an input on a cTrader series
//! is kept fresh by the venue's own closes). cTrader marks no snapshot closed: a `SPOT_EVENT` carries the
//! trendbar of the period STILL forming (`crates/bridges/ctrader/src/transport/connection/inbound.rs`'s `on_inbound`). So
//! [`TrendbarCloses`], the sink every frame of a connection's actor passes through, holds the last snapshot
//! of each `(symbol, interval)` and, when a snapshot of a LATER period arrives, hands the held one to
//! `close_bar` before the new `forming_bar`. The last snapshot of a period is the venue's own bar of it,
//! stamped with the period's open as a stored trendbar is (`crates/bridges/ctrader/src/history.rs`'s `bar_of`).
//!
//! - **A seed is closed bars** (the core files a seed into its closed series): the `GetTrendbars` answer's
//!   still-forming period is dropped from it, as `history.rs` drops the forming bar, and held as the key's
//!   first snapshot unless the stream already holds one of that period or a later one, so a period with no
//!   tick after the subscribe still closes at the next period's first snapshot. A seed left empty is not sent.
//! - **Filled from the venue's stored bars across a reconnect.** A period that straddled an outage is never
//!   closed from the snapshot before it, which lacks the ticks the outage hid: [`TrendbarCloses::forget_all`]
//!   (called by the actor's reconnect) drops every held snapshot and every fill still asked, and names where each
//!   key's hole starts: the period after the last bar it handed on closed (a close, or a seed's last), else the
//!   one the dropped snapshot was of. The new socket asks the venue's trendbars of each key still subscribed, one
//!   key at a time and paced under the vendor's limit (`crates/bridges/ctrader/src/transport/connection/trendbar_fill.rs`'s
//!   `TrendbarFills`, beside the actor's read), and [`TrendbarCloses::asked_fill`] notes each ask.
//! - **A key's live CLOSES wait for its fill, nothing else does.** From `forget_all` until the key's fill is
//!   answered, refused ([`TrendbarCloses::refused_fill`]) or given up ([`TrendbarCloses::gave_up_fill`]: its wait
//!   ran out, or it was never asked), a close the stream makes of that key is held back and handed on right after
//!   the fill's stored bars (the stored bar of a period wins over the stream's close of it), so the fill's older
//!   periods reach the sink first. The key's snapshots, every other key and every other frame go on at once.
//! - **WHAT WAS ASKED decides fill or seed, never the envelope id**: the first answer for a key with a fill asked
//!   is that fill's, every other answer is a seed. A fill hands on as `close_bar`, oldest first, the venue's
//!   STORED bar of every period from the hole on that is newer than the newest bar the key already handed on
//!   closed (the core drops an older close), so an answer read late is still exact, or closes nothing (a WARN)
//!   once the stream has closed a later period after its wait was given up. Its period still forming is held as a seed's is. A fill asks the
//!   seed's own window (`crate::feed::SEED_BAR_COUNT` periods back, or back to the hole if that is older), so of a
//!   seed and a fill both asked for one key, whichever answer is taken for which, each is exact: as a seed it is
//!   the venue's stored series up to now, as a fill it closes only what the outage hid. A key subscribed or
//!   released again drops its fill, and so does the venue's refusal of it ([`TrendbarCloses::refused_fill`], tied
//!   to the key because the fills go one at a time): that key's next answer is then a seed. A key that knew nothing
//!   before the outage, a fill refused or answered after a later close, and a socket that dies again first leave
//!   that period a hole.
//! - **Forgotten when a key's stream is subscribed or released** ([`TrendbarCloses::forget`]): its next seed
//!   starts it again.
//! - Every snapshot still goes out as `forming_bar`, and every other verb is handed on unchanged.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_data::DataClientSink;
use vike_marketdata::{Bar, FundingRate, L2Book, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamStatus, now_ms};

use crate::feed::decode::{trendbar_period_for_interval, trendbar_period_ms};
use crate::transport::proto::ProtoOaTrendbarPeriod;

/// The longest calendar month: a monthly bar is called closed only once this has passed since its open.
const LONGEST_MONTH_MS: i64 = 31 * vike_model::MS_PER_DAY;

/// The sink in front of a connection's own (module doc): closes a live trendbar when the next period begins.
pub struct TrendbarCloses {
    inner: Arc<dyn DataClientSink>,
    /// Every key a snapshot, a seed or a fill arrived for: a handful, so a scan.
    keys: Mutex<Vec<Key>>,
}

/// One `(symbol, interval)` of a [`TrendbarCloses`].
struct Key {
    symbol: String,
    interval: String,
    /// The last snapshot of the period still forming; a reconnect drops it.
    forming: Option<Bar>,
    /// The open stamp of the newest bar handed on closed: a close, or a seed's or a fill's last.
    closed: Option<i64>,
    /// The hole a fill was asked from on this socket, until an answer for this key takes it.
    fill_from: Option<i64>,
    /// Whether this key's reconnect fill is still to come (queued or asked, [`TrendbarCloses::forget_all`] to its
    /// answer, refusal or end): its live closes wait in `deferred` meanwhile, so the fill's older periods reach the
    /// sink before them.
    fill_pending: bool,
    /// The live closes read while `fill_pending`, oldest first.
    deferred: Vec<Bar>,
}

/// Where a key's hole starts after an outage ([`TrendbarCloses::forget_all`]): `(symbol, interval, open stamp)`, the
/// first stamp a bar the outage may have hidden can open at.
pub type Hole = (String, String, i64);

impl TrendbarCloses {
    pub fn new(inner: Arc<dyn DataClientSink>) -> Self {
        TrendbarCloses { inner, keys: Mutex::new(Vec::new()) }
    }

    /// The socket died: nothing held is closed from before it, and no fill asked on it will be answered. A live close
    /// still waiting for a fill goes on first (it closed on the dead socket). Returns each key's [`Hole`]: just past the
    /// last bar it handed on closed, else the dropped snapshot's period; each such key's live closes then wait for its
    /// fill (module doc). A key that knew neither has nothing to fill from.
    pub fn forget_all(&self) -> Vec<Hole> {
        let mut flushed = Vec::new();
        let holes = self
            .keys()
            .iter_mut()
            .filter_map(|k| {
                flush(k, &mut flushed);
                k.fill_from = None;
                let dropped = k.forming.take();
                let from = k.closed.map(|ts| ts + 1).or(dropped.map(|b| b.timestamp))?;
                k.fill_pending = true;
                Some((k.symbol.clone(), k.interval.clone(), from))
            })
            .collect();
        self.close_all(flushed);
        holes
    }

    /// The stream of `(symbol, interval)` was subscribed or released: what it held is forgotten, its fill too (a live
    /// close waiting for it goes on first).
    pub fn forget(&self, symbol: &str, interval: &str) {
        let mut flushed = Vec::new();
        {
            let mut keys = self.keys();
            for k in keys.iter_mut().filter(|k| k.symbol == symbol && k.interval == interval) {
                flush(k, &mut flushed);
            }
            keys.retain(|k| k.symbol != symbol || k.interval != interval);
        }
        self.close_all(flushed);
    }

    /// A reconnect asked the venue's trendbars of `(symbol, interval)` to fill its [`Hole`] at `from`: the next answer
    /// for that key is the fill's (module doc).
    pub fn asked_fill(&self, symbol: &str, interval: &str, from: i64) {
        key_of(&mut self.keys(), symbol, interval).fill_from = Some(from);
    }

    /// The venue refused the fill asked for `(symbol, interval)`: it is dropped, the key's live closes go on, and its
    /// next answer is a seed.
    pub fn refused_fill(&self, symbol: &str, interval: &str) {
        self.end_fill(symbol, interval, true);
    }

    /// The reconnect stopped waiting for the fill of `(symbol, interval)` (its wait ran out, or it was never asked):
    /// the key's live closes go on, and a fill still asked stays asked, so a late answer is still the fill's.
    pub fn gave_up_fill(&self, symbol: &str, interval: &str) {
        self.end_fill(symbol, interval, false);
    }

    fn end_fill(&self, symbol: &str, interval: &str, drop_ask: bool) {
        let mut flushed = Vec::new();
        for k in self.keys().iter_mut().filter(|k| k.symbol == symbol && k.interval == interval) {
            flush(k, &mut flushed);
            if drop_ask {
                k.fill_from = None;
            }
        }
        self.close_all(flushed);
    }

    /// Hand on `closes`, each `(symbol, interval, bar)`, in order.
    fn close_all(&self, closes: Vec<(String, String, Bar)>) {
        for (symbol, interval, bar) in closes {
            self.inner.close_bar(crate::transport::connection::VENUE, &symbol, &interval, bar);
        }
    }

    /// Whether a fill asked for `(symbol, interval)` is still unanswered.
    pub fn awaits_fill(&self, symbol: &str, interval: &str) -> bool {
        self.keys()
            .iter()
            .any(|k| k.symbol == symbol && k.interval == interval && k.fill_from.is_some())
    }

    fn keys(&self) -> MutexGuard<'_, Vec<Key>> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The entry of `(symbol, interval)`, made empty if it is new.
fn key_of<'a>(keys: &'a mut Vec<Key>, symbol: &str, interval: &str) -> &'a mut Key {
    let at = keys.iter().position(|k| k.symbol == symbol && k.interval == interval);
    let at = at.unwrap_or_else(|| {
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        keys.push(Key {
            symbol,
            interval,
            forming: None,
            closed: None,
            fill_from: None,
            fill_pending: false,
            deferred: Vec::new(),
        });
        keys.len() - 1
    });
    &mut keys[at]
}

/// End `k`'s wait for its fill: the live closes it deferred join `out`, each newer than the newest bar it handed on
/// closed (a fill's stored bar of a period wins over the stream's), and they count as handed on.
fn flush(k: &mut Key, out: &mut Vec<(String, String, Bar)>) {
    k.fill_pending = false;
    for bar in std::mem::take(&mut k.deferred) {
        if k.closed.is_none_or(|c| bar.timestamp > c) {
            k.closed = Some(bar.timestamp);
            out.push((k.symbol.clone(), k.interval.clone(), bar));
        }
    }
}

/// What an answer brought `k`: its period still forming is held unless the stream already holds that period or a
/// later one, and `closed` is the newest bar it handed on closed.
fn note(k: &mut Key, forming: Option<Bar>, closed: Option<i64>) {
    if let Some(forming) = forming
        && k.forming.as_ref().is_none_or(|held| held.timestamp < forming.timestamp)
    {
        k.forming = Some(forming);
    }
    k.closed = k.closed.max(closed);
}

/// Is the bar that opened at `ts` still forming at `now`? A month is taken at its longest, so a forming month
/// is never called closed (a closed one called forming is closed by the next month's first snapshot).
fn still_forming(ts: i64, interval: &str, now: i64) -> bool {
    let width = match trendbar_period_for_interval(interval) {
        Some(ProtoOaTrendbarPeriod::Mn1) => LONGEST_MONTH_MS,
        Some(period) => trendbar_period_ms(period),
        None => return false,
    };
    ts + width > now
}

impl DataClientSink for TrendbarCloses {
    /// Every `GET_TRENDBARS_RES` arrives here, a reconnect's fill as a subscribe's seed: which one it is, is what the
    /// key has asked (module doc).
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, mut bars: Vec<Bar>) {
        let forming = bars.pop_if(|b| still_forming(b.timestamp, interval, now_ms()));
        let mut flushed = Vec::new();
        let fill = {
            let mut keys = self.keys();
            let k = key_of(&mut keys, symbol, interval);
            let fill = k.fill_from.take();
            if let Some(from) = fill {
                let after = k.closed;
                if after.is_some_and(|c| c >= from) {
                    tracing::warn!(
                        target: "ctrader",
                        symbol,
                        interval,
                        from_ms = from,
                        "a reconnect's trendbar fill answered after the stream closed a later period: what it \
                         held before that period stays a hole"
                    );
                }
                bars.retain(|b| b.timestamp >= from && after.is_none_or(|c| b.timestamp > c));
            }
            note(k, forming, bars.last().map(|b| b.timestamp));
            // The fill's stored bars first, then the live closes that waited for them.
            if fill.is_some() {
                flush(k, &mut flushed);
            }
            fill
        };
        if fill.is_some() {
            for bar in bars {
                self.inner.close_bar(venue, symbol, interval, bar);
            }
            self.close_all(flushed);
        } else if !bars.is_empty() {
            // An empty seed would REPLACE the core's series with nothing.
            self.inner.seed_bars(venue, symbol, interval, bars);
        }
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        let closed = {
            let mut keys = self.keys();
            let k = key_of(&mut keys, symbol, interval);
            if k.forming.as_ref().is_some_and(|held| bar.timestamp < held.timestamp) {
                None // an older period's snapshot: what is held stays
            } else {
                let last =
                    k.forming.replace(bar.clone()).filter(|last| last.timestamp < bar.timestamp);
                match last {
                    // Its fill is still to come: the close waits, so the fill's older periods go on first.
                    Some(last) if k.fill_pending => {
                        k.deferred.push(last);
                        None
                    }
                    last => {
                        k.closed = k.closed.max(last.as_ref().map(|b| b.timestamp));
                        last
                    }
                }
            }
        };
        if let Some(closed) = closed {
            self.inner.close_bar(venue, symbol, interval, closed);
        }
        self.inner.forming_bar(venue, symbol, interval, bar);
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.mark_tick(venue, symbol, px, ts);
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
    }
    vike_data::forward_l2_snapshot!(inner);
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        self.inner.quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        self.inner.trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.inner.book(venue, symbol, book);
    }
    fn book_update(&self, venue: &str, symbol: &str, update: L2BookUpdate) {
        self.inner.book_update(venue, symbol, update);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.inner.stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        self.inner.data_delay(venue, symbol, stream, delay);
    }
    fn funding(&self, venue: &str, symbol: &str, funding: FundingRate) {
        self.inner.funding(venue, symbol, funding);
    }
}
