//! The production wire of the candle-history source ([`crate::klines::CtraderKlines`]): one DEDICATED,
//! authenticated, read-only session per source, opened through the same handshake the quote and listing
//! asks use ([`open_and_read_lists_by`]), and the process's one request pace.
//!
//! A history session is NOT the trading socket and sends nothing but `GET_TRENDBARS_REQ` after the
//! handshake (the argument is `crate::klines`'s module doc: the actor's trendbar command has no reply
//! path and its answer replaces a running strategy's series). It FOLLOWS the process's grant
//! ([`ConnConfig::sync_grant`]) and never spends a refresh token: the sockets the account's exec and data
//! feeds hold refresh it, and a history session beside them adds no second writer to the rotation.
//!
//! # The reply wait is its own, and long
//!
//! `read_until_by` waits at most the exec lane's `response_timeout` (a handshake step's bound), which is
//! not a bound for a history answer: one forum report has a trendbar answer take over twenty seconds, and
//! an answer can run to hundreds of kilobytes. The wait here is [`REPLY_WAIT`], and a session that waits
//! it out is DROPPED, never reused: the answer that arrives late would be read as the next request's.
//!
//! # One pace for the process
//!
//! [`history_gate`] is a `RateGate`, the blocking sliding-window limiter, shared by every wire in the
//! process: [`MAX_REQUESTS_PER_SECOND`] (four) in any second, under the vendor's five per connection.
//!
//! # One session at a time in the process
//!
//! Each history session is an authenticated socket beside the account's exec, data and recon sockets, and
//! the venue's per-application connection cap (`CONNECTIONS_LIMIT_EXCEEDED`) is NOT MEASURED. A loop of
//! `data hist fetch` runs over many symbols must not open that many sessions, and a trading-side reconnect
//! must not be the one refused for it. [`permits`] hands out [`MAX_OPEN_SESSIONS`] (one) counted permit,
//! taken by a wire's first `open` and KEPT BY THE WIRE across every reopen (a drop, an idle replacement,
//! the token retry), so a fetch under way never loses its turn to one that is waiting; it is returned when
//! the wire is dropped, which `klines.rs`'s `State` arranges to happen after its session's socket has
//! closed. A second wire's `open` waits for it at most the wire's permit wait and is then refused in
//! words, having dialled nothing; the wait is bounded and not cancellable, as a fetch has no cancel probe
//! (the gatewayhub checks the cancel before the call).

use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::ratelimit::RateGate;
use vike_model::now_ms;

use super::{ConnError, Reader, decode_payload, open_and_read_lists_by, send};
use crate::klines::{
    HistoryConfigProvider, HistorySession, HistoryWire, MAX_REQUESTS_PER_SECOND, Window, WireError,
    window_request,
};
use crate::proto::{ProtoMessage, ProtoOaErrorRes, ProtoOaGetTrendbarsRes, pt};
use crate::symbols::SymbolMap;

#[cfg(doc)]
use super::ConnConfig;

/// How long a history session's handshake may take as a whole (connect and every step). A bound on a sick
/// host, not a budget for a slow one: a healthy handshake is a few hundred milliseconds. UNMEASURED for a
/// broker with a very long symbol list.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);

/// How long one trendbar answer is waited for. UNMEASURED (the reply latency of the real venue is the
/// first thing the demo measurement reads); the daemon's fetch link stays alive meanwhile because the
/// gatewayhub reports progress while a fetch runs.
const REPLY_WAIT: Duration = Duration::from_secs(60);

/// The process's history request pace, built on first use.
fn history_gate() -> &'static RateGate {
    static GATE: OnceLock<RateGate> = OnceLock::new();
    GATE.get_or_init(|| RateGate::new(MAX_REQUESTS_PER_SECOND, Duration::from_secs(1)))
}

/// How many history sessions this PROCESS holds open at once (module doc).
pub const MAX_OPEN_SESSIONS: usize = 1;

/// How long an `open` waits for the session permit before it is refused. A held permit is one fetch (one
/// datahub request, whose chunks are paced a few a second), so the usual wait is the end of a fetch
/// that began a moment earlier; past this the second request is told, rather than parked for good.
const PERMIT_WAIT: Duration = Duration::from_secs(20);

/// The count of open history sessions and the signal that one was returned.
struct Permits {
    open: Mutex<usize>,
    returned: Condvar,
}

fn permits() -> &'static Permits {
    static PERMITS: OnceLock<Permits> = OnceLock::new();
    PERMITS.get_or_init(|| Permits { open: Mutex::new(0), returned: Condvar::new() })
}

/// One session's right to exist: counted while held, returned on drop.
struct Permit;

impl Permits {
    /// Take a permit, waiting at most `wait` for one to be returned.
    fn take(&self, wait: Duration) -> Option<Permit> {
        let deadline = Instant::now() + wait;
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        while *open >= MAX_OPEN_SESSIONS {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            open = self.returned.wait_timeout(open, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        *open += 1;
        Some(Permit)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let permits = permits();
        *permits.open.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        permits.returned.notify_one();
    }
}

/// The production [`HistoryWire`]: sessions opened through a config provider, paced by the process gate,
/// one at a time in the process.
pub struct LiveHistoryWire {
    config: HistoryConfigProvider,
    permit_wait: Duration,
    /// The process's session permit, taken at the first open and KEPT until this wire is dropped: a fetch
    /// that reopens (a drop, an idle replacement, the token retry) keeps its turn, so a waiting fetch
    /// cannot slip in between two sessions of one that is under way.
    permit: Option<Permit>,
}

impl LiveHistoryWire {
    /// A wire whose sessions are opened with the config `config` yields. Every wire shares the one
    /// process pace and the one session permit.
    pub fn shared(config: HistoryConfigProvider) -> Self {
        Self { config, permit_wait: PERMIT_WAIT, permit: None }
    }

    /// The most an `open` waits for the process's session permit (a suite shortens it).
    pub fn with_permit_wait(mut self, wait: Duration) -> Self {
        self.permit_wait = wait;
        self
    }
}

impl HistoryWire for LiveHistoryWire {
    fn open(&mut self) -> Result<Box<dyn HistorySession>, WireError> {
        let mut cfg = (self.config)().map_err(WireError::NotConfigured)?;
        if self.permit.is_none() {
            let wait = self.permit_wait;
            self.permit = Some(permits().take(wait).ok_or_else(|| {
                WireError::Failed(format!(
                    "another cTrader history session is open in this process, and it was still open \
                     after {} ms of waiting (one at a time: the venue's per-application connection \
                     cap is not measured). Run the same command again when that fetch has finished",
                    wait.as_millis()
                ))
            })?);
        }
        // The pair the process holds NOW, after any wait for the permit: a session opened after a
        // sibling's rotation presents the rotated one, and this session never refreshes (module doc).
        cfg.sync_grant();
        let ceiling = Instant::now() + OPEN_DEADLINE;
        let (reader, ctid, lists, _money_digits) =
            open_and_read_lists_by(&cfg, None, Some(ceiling)).map_err(handshake_error)?;
        Ok(Box::new(LiveSession { reader, ctid, symbols: SymbolMap::from_lists(&lists) }))
    }

    fn pace(&mut self) {
        history_gate().proceed();
    }

    fn sleep(&mut self, wait: Duration) {
        std::thread::sleep(wait);
    }

    fn now_ms(&self) -> i64 {
        now_ms()
    }

    fn monotonic_ms(&self) -> i64 {
        // shortcut: `Instant` does not count a suspended host's sleep on Linux, so a session idle through
        // a suspend reads younger than it is; the first ask then fails as a drop and is reopened once.
        static START: OnceLock<Instant> = OnceLock::new();
        i64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// A [`ConnError`] of the handshake as a [`WireError`]. A connection error says nothing about a
/// credential (`ConnError` never carries one).
fn handshake_error(e: ConnError) -> WireError {
    match e {
        ConnError::Venue { error_code, description } => {
            WireError::Venue { code: error_code, description, retry_after_s: None }
        }
        other => WireError::Failed(other.to_string()),
    }
}

/// An open session: the socket, the account it is authorised for and its symbols. It holds no permit: the
/// wire that opened it does.
struct LiveSession {
    reader: Reader,
    ctid: i64,
    symbols: SymbolMap,
}

impl HistorySession for LiveSession {
    fn symbols(&self) -> &SymbolMap {
        &self.symbols
    }

    fn ask(&mut self, window: &Window) -> Result<ProtoOaGetTrendbarsRes, WireError> {
        let req = window_request(self.ctid, window);
        send(&mut self.reader, pt::GET_TRENDBARS_REQ, &req, "hist")
            .map_err(|e| WireError::Dropped(format!("write: {e}")))?;
        read_trendbars_res(&mut self.reader, Instant::now() + REPLY_WAIT)
    }
}

/// Read frames until the trendbar answer, the venue's error, a closed socket or `deadline`. Other frames
/// (heartbeats, account events) are skipped; a token-invalidation event ends the session.
fn read_trendbars_res(
    reader: &mut Reader,
    deadline: Instant,
) -> Result<ProtoOaGetTrendbarsRes, WireError> {
    loop {
        if Instant::now() >= deadline {
            return Err(WireError::Dropped(format!("no answer within {} s", REPLY_WAIT.as_secs())));
        }
        match reader.next_frame() {
            Err(e) => return Err(WireError::Dropped(format!("read: {e}"))),
            Ok(Some(msg)) => match msg.payload_type {
                pt::GET_TRENDBARS_RES => {
                    return decode_payload(&msg).map_err(|e| WireError::Failed(e.to_string()));
                }
                pt::ERROR_RES => return Err(venue_error(&msg)),
                pt::ACCOUNTS_TOKEN_INVALIDATED_EVENT => {
                    return Err(WireError::Dropped("the account's token was invalidated".into()));
                }
                pt::ACCOUNT_DISCONNECT_EVENT => {
                    return Err(WireError::Dropped("the venue ended the account's session".into()));
                }
                _ => {}
            },
            Ok(None) if reader.get_mut().eof.load(Ordering::SeqCst) => {
                return Err(WireError::Dropped("connection closed".into()));
            }
            Ok(None) => {} // a read timeout, or half a frame: keep reading to the deadline
        }
    }
}

/// The venue's `ERROR_RES` as a [`WireError::Venue`], with its `retryAfter` (seconds) when it states one.
fn venue_error(msg: &ProtoMessage) -> WireError {
    match decode_payload::<ProtoOaErrorRes>(msg) {
        Ok(e) => WireError::Venue {
            code: e.error_code,
            description: e.description.unwrap_or_default(),
            retry_after_s: e.retry_after,
        },
        Err(e) => WireError::Failed(e.to_string()),
    }
}
