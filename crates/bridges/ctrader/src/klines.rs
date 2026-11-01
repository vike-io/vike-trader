//! **cTrader's candle history as a store-free source** — [`CtraderKlines`], this bridge's
//! `vike_data::source::KlineSource`: the FETCH half of a backfill and nothing else. It returns rows; the
//! commit key, the settle rule and the append are `vike-backfill`'s, and which process dispatches to it is
//! the gatewayhub's (decisions 0094 and 0164: only the gatewayhub dials a venue).
//!
//! # One channel
//!
//! `ProtoOAGetTrendbarsReq` with `fromTimestamp`, `toTimestamp` (epoch ms, the bar's OPEN stamp), a period
//! and a symbol id. Whether `fromTimestamp` is inclusive is NOT MEASURED, and a chunk starts on a whole
//! minute, so the request asks ONE MILLISECOND BELOW the first wanted stamp ([`window_request`]): were the
//! bound exclusive, the first bar of every stored chunk would be missing and its key spent. No bar is
//! stamped in that millisecond, and the answer is cut back to the wanted window. The upper bound needs no
//! such care for the ingest's chunks (they end at a millisecond before a whole minute); a DIRECT caller
//! whose end is exactly a bar's stamp depends on the venue taking it inclusively, which the first demo
//! measurement reads (`docs/ops/ctrader-history.md`). No `count`: the venue's own ceiling (its staff say
//! about fourteen thousand bars a request) and its `hasMore` flag are the paging, see below. Twelve intervals
//! are served ([`SERVED_INTERVALS`]); the venue also has weekly and monthly periods, but the bar
//! pipeline's `vike_marketdata::interval::measures_bar_step` refuses `1w` and `1M` and this source
//! refuses them too, rather than depend on only ever being called through that guard.
//!
//! # What a stored row is: the BID side, volume = tick count
//!
//! cTrader serves no ask and no mid candle (a vendor staff answer of 2023-07-08; the vendor has not
//! confirmed the side in writing). The store keeps ONE price per bar, so every row of this source is the
//! **bid-side** bar, with `volume` = the venue's tick count and `Bar::bid` / `Bar::ask` left unset. A
//! series from this source is therefore NOT comparable with the mid-priced lanes (oanda, ig, fxcm,
//! dukascopy) nor with the live bars the tradehub builds from quote mids; the catalog row says so.
//!
//! # Decoding: low plus deltas, a fixed scale, and two readings of an absent field
//!
//! A trendbar is `low` and three unsigned deltas: `open = low + deltaOpen`, `close = low + deltaClose`,
//! `high = low + deltaHigh`. Prices are integers in 1/100000 for EVERY symbol, whatever its `digits`
//! (`crates/bridges/ctrader/src/event_mapper.rs`'s `RELATIVE_PRICE_SCALE`, live-verified: the seed and
//! the spot feed divide by it). The sum is taken in integers (`i128`, so no wire value can overflow) and
//! divided ONCE, so each price is the double nearest the decimal the venue meant.
//!
//! * **An absent delta reads as zero.** The wire fields are optional, and a bar whose open is its low (or
//!   whose range is a point) is exactly what a quiet minute looks like; a sender is free to leave a zero
//!   unwritten, and the vendor's own samples read an absent delta as zero. The live seed path
//!   (`crates/bridges/ctrader/src/event_mapper.rs`'s `trendbar_to_bar`) skips such a bar instead — left as
//!   it is, because it is a different contract (best-effort context for a chart). A history window must
//!   not drop quiet bars: a stored window whose bars went missing is a spent key nothing repairs.
//! * **An absent `low` or stamp fails the window.** Zero is not a price or a time, so there is no
//!   reading; the window is an error naming the fact, never a skip.
//! * **An open or close above the high fails the window.** Deltas are measured from the low, so a bar
//!   that breaks it was decoded under a wrong assumption; nothing is stored from it.
//!
//! The stamp is `utcTimestampInMinutes * 60_000`: the bar's open, seconds lost. For a period up to an hour
//! the open is a multiple of the period's width in UTC, and a stamp off that grid FAILS the window (the
//! venue's convention is then not the one this source reads, and a stored chunk could not be repaired).
//! Where a four-hour, twelve-hour or daily bar's boundary falls (the broker's session) is NOT MEASURED and
//! carries no such rule; rows are kept as served, and the ingest assigns a bar to the chunk its stamp falls
//! in.
//!
//! # Windows and paging: by time, with the venue's flag as the safety
//!
//! A request is cut into pieces of at most [`MAX_WINDOW_BARS`] bars, under the venue's ceiling, and of at
//! most a day below an hour or a week from the hour up (the vendor limits the distance between a request's
//! bounds per period and publishes no numbers; the cap is inside this source so a caller that bypasses the
//! ingest's chunking is not exposed). A fetch is at most [`MAX_REQUESTS_PER_FETCH`] requests, so a direct
//! window is at most about two hundred days of minute bars or about four years of hourly and coarser
//! ones. An answer the venue
//! flags `hasMore` was CUT SHORT: it keeps what it holds and the window's remainder BELOW the oldest bar
//! and ABOVE the newest bar is asked again. Which end the venue keeps is not documented, so both are
//! asked; the one the venue kept comes back empty. Every ask shrinks the uncovered time by at least one
//! bar, an answer that says "more" yet holds no bar inside the window is an error, and a fetch never
//! sends more than [`MAX_REQUESTS_PER_FETCH`] requests. Bars outside the asked window are dropped; the
//! result is ascending and de-duplicated by stamp.
//!
//! ⚠ A vendor staff answer (2020, unconfirmed since) says a window wider than the ceiling loses its old
//! part WITH NO FLAG. The windows stay under the stated ceiling so that cannot arise, but the ceiling is
//! a forum figure, not a measurement on our broker: the first demo measurement reads the real one
//! (`docs/ops/ctrader-history.md`), and until it has the lane keeps its chunks small.
//!
//! A window is clamped to `[0, now]` (and the venue's own latest `to`, 19 January 2038), so a window in
//! the future asks nothing. **The forming bar is dropped**: a bar is kept only when its open plus its
//! width is not after now (a trendbar has no "complete" flag). The ingest's settle rule already refuses to
//! store a chunk that holds the present, so this guards a direct caller.
//!
//! An EMPTY answer is a normal one (a weekend, a holiday; bars exist only where ticks arrived) and
//! returns no rows, which the ingest records as an empty chunk. ⚠ A venue that answers empty for a day
//! it has no history for (depth varies by broker and is NOT MEASURED on our demo) looks the same, and
//! that chunk's key is then spent: measure the depth first (`docs/ops/ctrader-history.md`).
//!
//! # The session: dedicated, read-only, one per source
//!
//! The source holds ONE authenticated session, opened on the first fetch through
//! `crates/bridges/ctrader/src/conn/history.rs`'s `LiveHistoryWire` and kept for the source's life (the
//! gatewayhub builds a source per connection, so that is one datahub request). It is NOT the trading
//! socket: the actor's `Command::GetTrendbars` is fire-and-forget and its answer goes to the live sink's
//! `seed_bars`, which would REPLACE a running strategy's series; a request/response over that socket
//! would need a reply path in the actor, and the order flow would share its reads with a multi-hundred
//! kilobyte answer. The dedicated socket is the shape `crates/bridges/ctrader/src/recon_client.rs`'s
//! `CtraderReconClient` and `crates/bridges/ctrader/src/conn/quote.rs` already use. The session only
//! FOLLOWS the process's grant (`ConnConfig::sync_grant`) and never spends a refresh token, so it adds no
//! second writer to the rotation (`crates/bridges/ctrader/CLAUDE.md`'s "The OAuth token loop is CLOSED, and
//! the credential store is the ONE home", rung 4).
//!
//! **One session at a time in the whole process.** Each session is an authenticated socket beside the
//! account's exec, data and recon sockets, and the venue's per-application connection cap is NOT MEASURED,
//! so `LiveHistoryWire` hands out ONE permit per process: a second source waits for it (a bounded wait) and
//! is refused in words when the wait is over, without dialling anything. The permit belongs to the WIRE and
//! not to one session: it is kept across every reopen (a drop, the idle replacement, the token retry) so a
//! waiting source cannot slip in between two sessions of a fetch under way, and it is returned when the
//! source is dropped, after its socket has closed. A session unused for longer than the exec lane's
//! heartbeat age, on a monotonic clock, is dropped and a new one opened before the next ask (cTrader
//! disconnects idle sockets and this session sends no heartbeat).
//!
//! A session that drops (EOF, a read error, silence past the reply wait, the venue's token-invalidated or
//! account-disconnect event) is opened again ONCE and the request repeated; a second drop fails the fetch.
//! An open refused for the TOKEN (a sibling's refresh has just replaced the pair this session presented)
//! waits a second and opens once more, presenting the grant as the process holds it by then. A rate-limit
//! refusal (`REQUEST_FREQUENCY_EXCEEDED`, `BLOCKED_PAYLOAD_TYPE` with its `retryAfter`) waits and repeats up
//! to three times. Any other venue error fails the fetch at once with the venue's words.
//!
//! # Pace
//!
//! The vendor allows five historical requests a second per connection. This source takes at most
//! [`MAX_REQUESTS_PER_SECOND`] (four), across EVERY fetch in the process (`LiveHistoryWire`'s gate), and
//! every attempt, retries included, takes a turn. The reply latency is NOT MEASURED (one forum complaint
//! reports twenty seconds a request), so the wall time of a long fetch is unknown until the first demo
//! measurement.
//!
//! # Seams
//!
//! [`HistoryWire`] and [`HistorySession`] are the injected transport: a test scripts answers, the clock,
//! the pace and the sleeps; production is `LiveHistoryWire`. [`HistoryConfigProvider`] is how the caller
//! hands the production wire a connection config WHEN a session opens (the gatewayhub reads the account's
//! grant; this crate reads no store for it).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_data::source::{KlineSource, SourceError};
use vike_marketdata::Bar;
use vike_marketdata::interval::interval_ms;
use vike_model::MS_PER_DAY;

use crate::conn::{ConnConfig, HEARTBEAT_INTERVAL, LiveHistoryWire, VENUE};
use crate::event_mapper::{RELATIVE_PRICE_SCALE, trendbar_period_for_interval, trendbar_period_ms};
use crate::proto::{
    ProtoOaGetTrendbarsReq, ProtoOaGetTrendbarsRes, ProtoOaTrendbar, ProtoOaTrendbarPeriod,
};
use crate::symbols::SymbolMap;
use crate::token_store::REAUTHORIZE_CMD;

/// The intervals this source serves, in the store's spelling: the twelve cTrader periods from one minute
/// to one day. `1w` and `1M` are the venue's too and are refused here (calendar-width).
pub const SERVED_INTERVALS: [&str; 12] =
    ["1m", "2m", "3m", "4m", "5m", "10m", "15m", "30m", "1h", "4h", "12h", "1d"];

/// The most historical requests a second this PROCESS sends, across every fetch: under the vendor's five a
/// second per connection, so a clock a little off or two sessions at once still stays inside it.
pub const MAX_REQUESTS_PER_SECOND: usize = 4;

/// The most bars a single request window is sized for. Under the vendor's ceiling (fourteen thousand a
/// request, a staff statement of 2020) with margin, so a healthy answer is never cut.
pub const MAX_WINDOW_BARS: i64 = 10_000;

/// The most requests one fetch may send (retries not counted): a venue that keeps saying "more" without
/// ever finishing cannot hold the process.
pub const MAX_REQUESTS_PER_FETCH: usize = 200;

/// The widest piece of one request below an hour, in whole days; from the hour up it is
/// [`PIECE_DAYS_FROM_THE_HOUR`]. The vendor limits the distance between a request's two bounds per period
/// and publishes no numbers, so a piece stays at what the ingest's own chunks are (a day of minute bars, a
/// week of coarser ones) whatever [`MAX_WINDOW_BARS`] would allow; the first demo measurement reads the
/// real limits (`docs/ops/ctrader-history.md`).
const PIECE_DAYS_BELOW_THE_HOUR: i64 = 1;

/// The widest piece of one request from the hour up, in whole days.
const PIECE_DAYS_FROM_THE_HOUR: i64 = 7;

const HOUR_MS: i64 = 3_600_000;

/// A session unused for longer than this is dropped and a new one opened BEFORE the next ask: cTrader
/// disconnects an idle socket (the exec lane heartbeats this often for that reason, and the recon client
/// revives its own connection at the same age) and this session sends no heartbeat, so the alternative is
/// a dead socket found by a failed request.
const IDLE_REOPEN_AFTER: Duration = HEARTBEAT_INTERVAL;

/// The wait before the one more open that follows a token-class refusal at an open: a sibling socket's
/// refresh invalidates this session and writes the new pair a moment after the venue dropped us, so an
/// immediate reopen can present the old pair.
const TOKEN_RETRY_WAIT: Duration = Duration::from_secs(1);

/// How many times a rate-limit refusal is waited out and repeated before the fetch fails.
const MAX_THROTTLE_RETRIES: u32 = 3;

/// The wait after a rate-limit refusal that states no `retryAfter` (or a shorter one).
const THROTTLE_BACKOFF: Duration = Duration::from_secs(2);

/// The longest a stated `retryAfter` is honoured for: a confused answer must not park a fetch for an hour.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);

/// The venue's latest `toTimestamp` (19 January 2038, in the request's own words).
const LATEST_TO_MS: i64 = 2_147_483_646_000;

/// One window of one symbol and period, in the request's own units. The connection's account id is the
/// session's to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub symbol_id: i64,
    pub period: ProtoOaTrendbarPeriod,
    /// First bar-open stamp wanted, epoch ms.
    pub from_ms: i64,
    /// Last bar-open stamp wanted, epoch ms.
    pub to_ms: i64,
}

/// The wire request for `window` on account `ctid`: both bounds set and NO `count` (the venue's flag
/// pages, see the module doc). The lower bound is asked ONE MILLISECOND BELOW the first wanted stamp (never
/// below the epoch): whether the venue's `fromTimestamp` is inclusive is NOT MEASURED, a chunk starts on a
/// whole minute, and an exclusive bound would lose the first bar of every stored chunk with its key spent.
/// No bar is stamped in the extra millisecond (stamps are whole minutes) and the window filter of the
/// answer's decoder keeps what was wanted.
pub fn window_request(ctid: i64, window: &Window) -> ProtoOaGetTrendbarsReq {
    ProtoOaGetTrendbarsReq {
        ctid_trader_account_id: ctid,
        from_timestamp: Some((window.from_ms - 1).max(0)),
        to_timestamp: Some(window.to_ms),
        period: window.period as i32,
        symbol_id: window.symbol_id,
        count: None,
        ..Default::default()
    }
}

/// Why a [`HistoryConfigProvider`] produced no connection config. No variant carries a payload, so the
/// text that reaches an operator cannot hold a credential by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryConfigError {
    /// No usable grant: the venue's application pair or the demo account's token pair is absent.
    NotConfigured,
    /// The credential store could not be read at all (the cause is in the caller's log).
    StoreUnreadable,
}

impl fmt::Display for HistoryConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HistoryConfigError::NotConfigured => write!(
                f,
                "ctrader history needs the DEMO account's OAuth grant, and this data server holds \
                 none: give the venue its CLIENT_ID and CLIENT_SECRET and an ACTIVE ctrader account \
                 at tier `demo` its ACCESS_TOKEN and REFRESH_TOKEN (`vike-cli secrets accounts` names \
                 the accounts; `{REAUTHORIZE_CMD}` writes the grant). Only a demo account is read; a \
                 live account's grant never is"
            ),
            HistoryConfigError::StoreUnreadable => f.write_str(
                "the credential store on this data server could not be read, so the cTrader history \
                 grant is unavailable (the cause is in the data server's log)",
            ),
        }
    }
}

impl std::error::Error for HistoryConfigError {}

/// What a caller hands [`CtraderKlines::new`] instead of a credential: a closure that produces the
/// connection config WHEN a session opens. It is called once per opened session (so a session reopened
/// after a drop sees the grant a sibling socket rotated meanwhile) and the value it returns is dropped
/// with the session.
pub type HistoryConfigProvider =
    Arc<dyn Fn() -> Result<ConnConfig, HistoryConfigError> + Send + Sync>;

/// Why a session could not open or answer.
#[derive(Debug)]
pub enum WireError {
    /// The provider had no config: nothing was asked of the venue.
    NotConfigured(HistoryConfigError),
    /// The socket is gone or went quiet (EOF, a read error, no answer in time): the session cannot be
    /// trusted any more, because a late answer would be read as the next request's.
    Dropped(String),
    /// The venue answered with an `ERROR_RES`.
    Venue { code: String, description: String, retry_after_s: Option<u64> },
    /// Anything else (a refused handshake, an answer that does not decode).
    Failed(String),
}

/// One authenticated, read-only session able to answer trendbar windows.
pub trait HistorySession: Send {
    /// The account's symbols (name to id), read by the handshake.
    fn symbols(&self) -> &SymbolMap;
    /// Ask one window and return the venue's answer as decoded.
    fn ask(&mut self, window: &Window) -> Result<ProtoOaGetTrendbarsRes, WireError>;
}

/// Everything a fetch needs from outside the process: the handshake, the pace, the clock, the sleep.
pub trait HistoryWire: Send {
    /// Open a session (the handshake). Called on the first fetch and again after a drop.
    fn open(&mut self) -> Result<Box<dyn HistorySession>, WireError>;
    /// Block until one more historical request may be sent. Every attempt takes a turn.
    fn pace(&mut self);
    /// Wait `wait` (a rate-limit back-off).
    fn sleep(&mut self, wait: Duration);
    /// Now, epoch ms: the forming-bar rule's clock.
    fn now_ms(&self) -> i64;
    /// A monotonic clock, ms from an arbitrary start: the idle age is a DURATION and is read off this,
    /// never off [`HistoryWire::now_ms`], which an operator or a time sync can step either way.
    fn monotonic_ms(&self) -> i64;
}

/// **cTrader's history source** — the `KlineSource` the gatewayhub dispatches a cTrader credentialed
/// `Backfill` to. One authenticated session, opened lazily (module doc).
pub struct CtraderKlines {
    state: Mutex<State>,
}

struct State {
    // ⚠ `session` is declared BEFORE `wire`: fields drop in order, so the socket closes before the wire
    // returns the process's session permit, and the next holder never overlaps this one's socket.
    session: Option<Box<dyn HistorySession>>,
    wire: Box<dyn HistoryWire>,
    /// When the held session was last handed out or answered, on the wire's monotonic clock (ms).
    last_used_ms: i64,
}

impl CtraderKlines {
    /// The production source: sessions are opened through `config`, on the process's one request pace.
    pub fn new(config: HistoryConfigProvider) -> Self {
        Self::with_wire(Box::new(LiveHistoryWire::shared(config)))
    }

    /// A source over any wire (a test's script).
    pub fn with_wire(wire: Box<dyn HistoryWire>) -> Self {
        Self { state: Mutex::new(State { session: None, wire, last_used_ms: 0 }) }
    }
}

impl fmt::Debug for CtraderKlines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The wire holds the provider of a credential: nothing about it is printed.
        f.debug_struct("CtraderKlines").finish_non_exhaustive()
    }
}

impl KlineSource for CtraderKlines {
    fn venue(&self) -> &str {
        VENUE
    }

    /// Fetch `[start_ms, end_ms]` of `(symbol, interval)` bid-side bars, oldest first.
    ///
    /// `Refused` — before any session opens — for an interval the venue does not serve or this source
    /// cannot page; `Refused` too for a symbol the account does not list (after the handshake, before any
    /// trendbar request) and for an absent grant. Every other failure is `Fetch`.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        let (period, step_ms) = period_for(interval)?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.fetch(symbol, period, step_ms, start_ms, end_ms)
    }
}

/// The wire period and the width in ms of `interval`, or the refusal.
fn period_for(interval: &str) -> Result<(ProtoOaTrendbarPeriod, i64), SourceError> {
    let Some(period) = trendbar_period_for_interval(interval) else {
        return Err(SourceError::Refused(format!(
            "ctrader history: {interval:?} is not an interval cTrader serves candles for (served: {})",
            SERVED_INTERVALS.join(" ")
        )));
    };
    // The width is the interval's own, from the one parser every forming-bar guard shares; `1w` and
    // `1M` have none.
    match interval_ms(interval) {
        Some(ms) => Ok((period, ms)),
        None => Err(SourceError::Refused(format!(
            "ctrader history: {interval:?} candles are calendar-width, and this source pages by a \
             fixed step. Fetch 1d and resample"
        ))),
    }
}

impl State {
    fn fetch(
        &mut self,
        symbol: &str,
        period: ProtoOaTrendbarPeriod,
        step_ms: i64,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        let now = self.wire.now_ms();
        let lo = start_ms.max(0);
        let hi = end_ms.min(now).min(LATEST_TO_MS);
        if hi < lo {
            return Ok(Vec::new());
        }
        let symbol_id = self.symbol_id(symbol)?;
        let mut todo = pieces(lo, hi, piece_span_ms(step_ms));
        let mut found: BTreeMap<i64, Bar> = BTreeMap::new();
        let mut asked = 0usize;
        while let Some((from_ms, to_ms)) = todo.pop() {
            asked += 1;
            if asked > MAX_REQUESTS_PER_FETCH {
                return Err(SourceError::Fetch(format!(
                    "ctrader history: the window needs more than {MAX_REQUESTS_PER_FETCH} requests; \
                     ask for a narrower one"
                )));
            }
            let window = Window { symbol_id, period, from_ms, to_ms };
            let res = self.ask(&window)?;
            let page = read_page(&res, &window)?;
            if page.cut {
                // Which end the venue kept is not documented: ask what lies below the oldest bar and
                // above the newest. Each remainder is smaller than the window by at least one bar.
                let oldest = page.bars.iter().map(|b| b.ts).min();
                let newest = page.bars.iter().map(|b| b.ts).max();
                let (Some(oldest), Some(newest)) = (oldest, newest) else {
                    return Err(SourceError::Fetch(
                        "ctrader history: the venue said more bars follow but sent none inside the \
                         window"
                            .to_string(),
                    ));
                };
                if oldest > from_ms {
                    todo.push((from_ms, oldest - 1));
                }
                if newest < to_ms {
                    todo.push((newest + 1, to_ms));
                }
            }
            for bar in page.bars {
                // The forming bar is not a bar yet.
                if bar.ts + step_ms <= now {
                    found.insert(bar.ts, bar);
                }
            }
        }
        Ok(found.into_values().collect())
    }

    /// The id of `symbol` in the account's list (opening the session if there is none), or the refusal.
    fn symbol_id(&mut self, symbol: &str) -> Result<i64, SourceError> {
        let session = self.take_session()?;
        let id = session
            .symbols()
            .id_of(symbol)
            .ok_or_else(|| SourceError::Refused(unknown_symbol(symbol, session.symbols())));
        self.session = Some(session);
        id
    }

    /// The held session, or a new one when there is none or it has been idle longer than
    /// [`IDLE_REOPEN_AFTER`]. The idle session is dropped FIRST: its permit is the one the new session
    /// needs.
    fn take_session(&mut self) -> Result<Box<dyn HistorySession>, SourceError> {
        let now = self.wire.monotonic_ms();
        if let Some(session) = self.session.take() {
            let idle_ms = i64::try_from(IDLE_REOPEN_AFTER.as_millis()).unwrap_or(i64::MAX);
            if now.saturating_sub(self.last_used_ms) <= idle_ms {
                self.last_used_ms = now;
                return Ok(session);
            }
            tracing::debug!(target: "ctrader", "history session idle; opening a new one");
            drop(session);
        }
        let session = self.open_session()?;
        self.last_used_ms = now;
        Ok(session)
    }

    /// Open a session. A token-class refusal waits [`TOKEN_RETRY_WAIT`] and opens once more (every open
    /// presents the grant as the process holds it then); any other refusal fails at once.
    fn open_session(&mut self) -> Result<Box<dyn HistorySession>, SourceError> {
        match self.wire.open() {
            Err(WireError::Venue { code, .. }) if is_grant_refusal(&code) => {
                tracing::warn!(
                    target: "ctrader",
                    "history session refused the token; opening it once more in a second"
                );
                self.wire.sleep(TOKEN_RETRY_WAIT);
                self.wire.open().map_err(source_error)
            }
            other => other.map_err(source_error),
        }
    }

    /// One window over the held session: paced, reopened once after a drop, waited out after a
    /// rate-limit refusal.
    fn ask(&mut self, window: &Window) -> Result<ProtoOaGetTrendbarsRes, SourceError> {
        let mut reopened = false;
        let mut throttled = 0u32;
        loop {
            let mut session = self.take_session()?;
            self.wire.pace();
            match session.ask(window) {
                Ok(res) => {
                    self.last_used_ms = self.wire.monotonic_ms();
                    self.session = Some(session);
                    return Ok(res);
                }
                Err(WireError::Dropped(why)) => {
                    if std::mem::replace(&mut reopened, true) {
                        return Err(SourceError::Fetch(format!(
                            "ctrader history: the session dropped twice in a row ({why})"
                        )));
                    }
                    tracing::warn!(target: "ctrader", "history session dropped; opening it again once");
                    // `session` is dropped here: the socket closes and the next turn opens a new one.
                }
                Err(WireError::Venue { code, retry_after_s, .. })
                    if is_throttle(&code) && throttled < MAX_THROTTLE_RETRIES =>
                {
                    throttled += 1;
                    let stated =
                        Duration::from_secs(retry_after_s.unwrap_or(0)).min(RETRY_AFTER_CAP);
                    let wait = THROTTLE_BACKOFF.max(stated);
                    tracing::warn!(
                        target: "ctrader",
                        attempt = throttled,
                        wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                        "history request rate-limited; waiting"
                    );
                    self.session = Some(session);
                    self.wire.sleep(wait);
                }
                Err(e) => return Err(source_error(e)),
            }
        }
    }
}

/// The widest piece of one request for a period `step_ms` wide: the bar-count ceiling
/// ([`MAX_WINDOW_BARS`]) and the distance cap of [`PIECE_DAYS_BELOW_THE_HOUR`] and
/// [`PIECE_DAYS_FROM_THE_HOUR`], whichever is narrower.
fn piece_span_ms(step_ms: i64) -> i64 {
    let days = if step_ms < HOUR_MS { PIECE_DAYS_BELOW_THE_HOUR } else { PIECE_DAYS_FROM_THE_HOUR };
    (step_ms * MAX_WINDOW_BARS).min(days * MS_PER_DAY)
}

/// The windows covering `[lo, hi]`, ascending, each `span` ms wide at most.
fn pieces(lo: i64, hi: i64, span: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut from = lo;
    while from <= hi {
        let to = hi.min(from + span - 1);
        out.push((from, to));
        from = to + 1;
    }
    out
}

/// Whether a venue error code is a rate-limit refusal worth waiting out.
fn is_throttle(code: &str) -> bool {
    matches!(code, "REQUEST_FREQUENCY_EXCEEDED" | "BLOCKED_PAYLOAD_TYPE")
}

/// Whether a venue error code is about the grant (an access token or an authorisation).
fn is_grant_refusal(code: &str) -> bool {
    let upper = code.to_ascii_uppercase();
    upper.contains("AUTH") || upper.contains("TOKEN")
}

/// A wire failure as the source's error: a missing grant is `Refused` (the venue was not asked), the rest
/// are `Fetch` in the venue's own words, with the remedy named for a refused grant.
fn source_error(e: WireError) -> SourceError {
    match e {
        WireError::NotConfigured(why) => SourceError::Refused(why.to_string()),
        WireError::Dropped(why) | WireError::Failed(why) => {
            SourceError::Fetch(format!("ctrader history: {why}"))
        }
        WireError::Venue { code, description, .. } => {
            let remedy = if is_grant_refusal(&code) {
                format!(
                    ". The account's grant was refused: a running ctrader socket of this account \
                     refreshes it (a history session only follows it), otherwise re-issue it with \
                     `{REAUTHORIZE_CMD}`"
                )
            } else {
                String::new()
            };
            SourceError::Fetch(format!(
                "ctrader history: cTrader error {code}: {description}{remedy}"
            ))
        }
    }
}

/// The refusal for a symbol the account does not list: names are the broker's and case-significant.
fn unknown_symbol(symbol: &str, map: &SymbolMap) -> String {
    let shown: String = symbol.chars().take(40).collect();
    let near = map
        .ids()
        .into_iter()
        .filter_map(|id| map.name_of(id))
        .find(|n| n.eq_ignore_ascii_case(symbol));
    let hint = near.map_or_else(String::new, |n| format!(" Did you mean {n:?}?"));
    format!(
        "ctrader history: {shown:?} is not a symbol this account lists. Names are the broker's own and \
         case-significant (like EURUSD), so one instrument can never be stored under two spellings.{hint}"
    )
}

/// One answer, reduced to the bars inside the window it was asked for.
struct Page {
    bars: Vec<Bar>,
    /// The venue flagged `hasMore`: the answer was cut short.
    cut: bool,
}

/// Check that `res` answers `window` and decode it.
fn read_page(res: &ProtoOaGetTrendbarsRes, window: &Window) -> Result<Page, SourceError> {
    let crossed = res.period != window.period as i32
        || res.symbol_id.is_some_and(|id| id != window.symbol_id);
    if crossed {
        return Err(SourceError::Fetch(
            "ctrader history: the venue answered for another symbol or period than the one asked"
                .to_string(),
        ));
    }
    let step_ms = trendbar_period_ms(window.period);
    let mut bars = Vec::with_capacity(res.trendbar.len());
    for tb in &res.trendbar {
        let bar = bar_of(tb)
            .map_err(|why| SourceError::Fetch(format!("ctrader history: a trendbar {why}")))?;
        if (window.from_ms..=window.to_ms).contains(&bar.ts) {
            // A period up to an hour opens on a multiple of its width in UTC. A stamp off that grid means
            // the venue's convention is not the one this source assumes; the chunk fails rather than be
            // stored under a key nothing repairs. Longer periods open where the broker's session says
            // (NOT MEASURED), so they carry no rule.
            if step_ms <= HOUR_MS && bar.ts % step_ms != 0 {
                return Err(SourceError::Fetch(format!(
                    "ctrader history: a trendbar stamped minute {} is off the {}-minute grid of its \
                     period, so the venue's stamp convention is not the one this source reads. \
                     Nothing was stored for the window",
                    bar.ts / 60_000,
                    step_ms / 60_000
                )));
            }
            bars.push(bar);
        }
    }
    Ok(Page { bars, cut: res.has_more == Some(true) })
}

/// One trendbar as a bid-side bar (module doc: the readings of an absent field).
fn bar_of(tb: &ProtoOaTrendbar) -> Result<Bar, String> {
    let (Some(low), Some(minutes)) = (tb.low, tb.utc_timestamp_in_minutes) else {
        return Err(format!(
            "(stamp minute {:?}) has no low price or no stamp, so it has no reading",
            tb.utc_timestamp_in_minutes
        ));
    };
    let delta_open = tb.delta_open.unwrap_or(0);
    let delta_close = tb.delta_close.unwrap_or(0);
    let delta_high = tb.delta_high.unwrap_or(0);
    if delta_open > delta_high || delta_close > delta_high {
        return Err(format!(
            "(stamp minute {minutes}) opens or closes above its high (deltas {delta_open} and \
             {delta_close} against {delta_high} from the low)"
        ));
    }
    let price = |delta: u64| (i128::from(low) + i128::from(delta)) as f64 / RELATIVE_PRICE_SCALE;
    Ok(Bar {
        ts: i64::from(minutes) * 60_000,
        open: price(delta_open),
        high: price(delta_high),
        low: price(0),
        close: price(delta_close),
        volume: tb.volume as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    })
}
