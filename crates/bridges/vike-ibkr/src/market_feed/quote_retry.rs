//! **A quote stream that IB ENDS on a market-data refusal is re-opened, and the operator is told why while it is
//! down** (decision 0220; the node's stock price lane was dead for two days from 2026-10-28 04:18 UTC).
//!
//! # What happened
//!
//! Two things, one after the other, and both silent:
//!
//! 1. IB ended every quote stream of the data connection with `[10197] No market data during competing live
//!    session` (two minutes after a 30 s loss of the gateway's link to IB). The pump logged one line and exited;
//!    that end is a REFUSED REQUEST, not a dead connection, so nothing supervised it and nothing re-subscribed it.
//! 2. The gateway restarted at 06:10. `ibapi` rebuilt the data connection ITSELF (the supervisor logged
//!    "the data connection is back and its quote streams are live again": true of the connection, false of the
//!    three streams that had ended hours before). A rebuilt connection is a new session whose market-data type is the
//!    real-time default, and the account holds no real-time stock subscription, so every later subscribe ended
//!    `[354] Requested market data is not subscribed` and every order on a stock the engine was not mounted on was
//!    refused "has no price on this node yet". A fresh client of the same gateway, which sets the delayed type at
//!    its connect, got quotes in 3 s.
//!
//! # What the pump does now
//!
//! * **The market-data type is sent again before EVERY subscribe** ([`QuoteWire::apply_data_type`]), so a session the
//!   connection lost it with is never asked for real-time stock data.
//! * **A stream IB ends on a market-data refusal is re-opened** ([`data_refusal`]: `354`, `10197`, `10168`) instead of
//!   ending: after a quick first retry ([`PRODUCTION`]'s ladder: 5 s, then 60 s, 2 min, 5 min, the gateway's one
//!   backoff table `crate::redial::backoff` after the quick one) the type is sent again and the stream opened again. Every
//!   refusal is a WARN line in words ([`refusal_words`]) that says what IB said, what it means here and when the next
//!   attempt is.
//! * **Disclosed from the SECOND refusal on**: the first one is a transient (the 10197 above came right after a link
//!   loss), so nothing is disclosed until a retry has failed too; then the symbol's quote lane says `GapStart`, which is
//!   what the control door's refusal reads to say "its price stream is DOWN ... the gatewayhub's log has the venue's
//!   words" instead of "retry in a moment" (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `await_price`). It
//!   says `Live` once the re-opened stream has stood [`Plan::live_after`] without a refusal (a closed market sends no
//!   tick, so a tick is not waited for), and when the pump is STOPPED with the gap open (a window nobody closes
//!   trips the link dead-man after its grace, `crate::quote_handle`'s module doc).
//!
//!   ⚠ A symbol IB refuses for good is therefore a lane DOWN for as long as it lasts, like any stream a venue
//!   closes, and the link dead-man treats it as such (decision 0180: it cancels and halts the venue past the grace
//!   while the venue holds something). That is the verdict, not a side effect: an admitted symbol that cannot be priced is a
//!   price blackout.
//! * **A lost CONNECTION is still the supervisor's** (`crate::quote_handle`): the pump ends with `lost` raised, says
//!   no `Live` (the supervisor's own outage window is open and its re-dial closes it), and the re-dial joins or dials
//!   the connection and subscribes again, through THIS loop.
//!
//! The loop is generic over a [`QuoteWire`] so a recording double proves it (`quote_retry_tests.rs`): the production
//! wire is the shared `ibapi` client ([`super::pump`]'s `ClientWire`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ibapi::market_data::realtime::TickTypes;
use vike_data::{LiveDataSink, StreamLabel};
use vike_model::StreamStatus;

use super::map::{QuoteAccumulator, forward_quote_tick};
use super::{VENUE, connection_gone, link_gone_code, stream_ended};

/// IB's own words for the codes this loop re-opens a stream on.
const NOT_SUBSCRIBED: i32 = 354;
const DELAYED_NOT_ENABLED: i32 = 10168;
const COMPETING_SESSION: i32 = 10197;

/// What one look at an open quote stream found.
pub(crate) enum Next {
    /// A tick (the label and the quote go to the sink).
    Tick(TickTypes),
    /// An advisory notice bound to the stream; only a link-gone code ends the pump.
    Notice(ibapi::Notice),
    /// The stream ended on an error.
    Ended(ibapi::Error),
    /// Nothing within the wait (or a clean end: the pump sleeps a poll, as it always did).
    Quiet,
}

/// One open quote stream.
pub(crate) trait QuoteDrain {
    /// The next look, waiting at most `wait`.
    fn next(&mut self, wait: Duration) -> Next;
}

/// The wire half of one symbol's quote stream.
pub(crate) trait QuoteWire {
    /// Send the connection's market-data type (again): called before EVERY open.
    fn apply_data_type(&self);
    /// `reqMktData` of the symbol.
    ///
    /// # Errors
    /// What `ibapi` refused the request with.
    fn open(&self) -> Result<Box<dyn QuoteDrain>, ibapi::Error>;
}

/// The schedule of one pump.
pub(crate) struct Plan {
    /// How long one look waits, and how long a quiet stream sleeps.
    pub(crate) poll: Duration,
    /// The wait after the 1st, 2nd, ... consecutive refusal; the last repeats.
    pub(crate) ladder: &'static [Duration],
    /// How long a re-opened stream must stand without a refusal before its gap is closed.
    pub(crate) live_after: Duration,
    /// The consecutive refusal that DISCLOSES the gap (the ones before it are told in the log only).
    pub(crate) disclose_at: u32,
}

/// The production schedule: a quick retry (the 10197 after a link loss is a transient), then the gateway's table.
pub(crate) const PRODUCTION: Plan = Plan {
    poll: Duration::from_millis(200),
    ladder: &[
        Duration::from_secs(5),
        Duration::from_secs(60),
        Duration::from_secs(120),
        Duration::from_secs(300),
    ],
    live_after: Duration::from_secs(10),
    disclose_at: 2,
};

impl Plan {
    fn wait_after(&self, refusals: u32) -> Duration {
        let at = (refusals.max(1) as usize - 1).min(self.ladder.len().saturating_sub(1));
        self.ladder.get(at).copied().unwrap_or(Duration::from_secs(300))
    }
}

/// A refusal of the stream's request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: i32,
    pub(crate) message: String,
}

/// Whether `e` is IB refusing the stream for want of market data (and not a bad contract or a dead link): the
/// request is worth sending again.
pub(crate) fn data_refusal(e: &ibapi::Error) -> Option<Refusal> {
    match e {
        ibapi::Error::Notice(n)
            if matches!(n.code, NOT_SUBSCRIBED | DELAYED_NOT_ENABLED | COMPETING_SESSION) =>
        {
            Some(Refusal { code: n.code, message: n.message.clone() })
        }
        _ => None,
    }
}

/// What a refusal means HERE, in the operator's words.
pub(crate) fn refusal_words(r: &Refusal) -> String {
    let meaning = match r.code {
        COMPETING_SESSION => {
            "another login on this IBKR username (TWS, the mobile app, a second gateway) holds the market-data \
             entitlement, so this session gets no market data while that session lasts"
        }
        NOT_SUBSCRIBED => {
            "the connection asked for a kind of market data the account is not subscribed to; this account is \
             served stocks as DELAYED data only, and the delayed market-data type is a setting of the gateway \
             SESSION that a reconnect resets to real-time"
        }
        DELAYED_NOT_ENABLED => "delayed market data is not enabled for this account",
        _ => "IB refused the market-data request",
    };
    format!("[{}] {} ({meaning})", r.code, r.message.trim())
}

fn sleep_unless_stopped(stop: &AtomicBool, wait: Duration, poll: Duration) {
    let until = Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(poll));
    }
}

/// The state of one symbol's lane across its re-opens.
struct Lane<'a> {
    symbol: &'a str,
    sink: &'a dyn LiveDataSink,
    plan: &'a Plan,
    /// Consecutive refusals (reset by a tick).
    refusals: u32,
    /// When the `GapStart` this pump told was said (epoch ms).
    gap: Option<i64>,
}

impl Lane<'_> {
    /// IB refused the stream: tell the operator, disclose from the second one on, and say how long to wait.
    fn refused(&mut self, r: &Refusal) -> Duration {
        self.refusals += 1;
        let wait = self.plan.wait_after(self.refusals);
        let disclosed = self.refusals >= self.plan.disclose_at;
        if disclosed && self.gap.is_none() {
            let at = vike_model::now_ms();
            self.gap = Some(at);
            self.sink.stream_status(
                VENUE,
                self.symbol,
                StreamLabel::Quotes.as_str(),
                StreamStatus::GapStart { at_ts_ms: at },
            );
        }
        tracing::warn!(
            symbol = %self.symbol,
            code = r.code,
            refusals = self.refusals,
            retry_in_s = wait.as_secs_f64(),
            "ibkr quotes: IB ended the quote stream of {} with {}; the market-data type is sent again and the stream \
             is re-opened in {} s (refusal {}){}",
            self.symbol,
            refusal_words(r),
            wait.as_secs(),
            self.refusals,
            if disclosed {
                "; the symbol has NO PRICE meanwhile: an order on it is refused naming the stream as down"
            } else {
                ""
            }
        );
        wait
    }

    /// Say `Live` for a gap this pump told.
    fn close_gap(&mut self) {
        if let Some(at) = self.gap.take() {
            self.sink.stream_status(
                VENUE,
                self.symbol,
                StreamLabel::Quotes.as_str(),
                StreamStatus::Live { gap_started_ts_ms: Some(at) },
            );
        }
    }
}

/// **Run one symbol's quote stream until it is stopped or its connection is gone**, re-opening it when IB ends it on
/// a market-data refusal. See the module doc.
pub(crate) fn run_quotes(
    wire: &dyn QuoteWire,
    symbol: &str,
    sink: &dyn LiveDataSink,
    stop: &AtomicBool,
    lost: &AtomicBool,
    plan: &Plan,
) {
    let mut lane = Lane { symbol, sink, plan, refusals: 0, gap: None };
    'open: while !stop.load(Ordering::Relaxed) {
        wire.apply_data_type();
        let mut stream = match wire.open() {
            Ok(stream) => stream,
            Err(e) => {
                if let Some(r) = data_refusal(&e) {
                    let wait = lane.refused(&r);
                    sleep_unless_stopped(stop, wait, plan.poll);
                    continue 'open;
                }
                tracing::warn!(%symbol, error = %e, "ibkr market_data subscribe failed");
                // A subscribe that fails because the CONNECTION is gone (a re-open that met a rebuild in flight) is the
                // supervisor's: a pump that exits with `lost` unraised would be a dead stream nothing re-dials.
                if connection_gone(&e) {
                    lost.store(true, Ordering::Relaxed);
                    return;
                }
                break 'open;
            }
        };
        let mut acc = QuoteAccumulator::default();
        let opened = Instant::now();
        let mut stood = false;
        while !stop.load(Ordering::Relaxed) {
            match stream.next(plan.poll) {
                Next::Tick(tick) => {
                    // A tick is the recovery in full: the ladder starts over and the gap closes.
                    lane.refusals = 0;
                    lane.close_gap();
                    stood = true;
                    // The label rides beside the quote, read off this tick (decision 0140).
                    forward_quote_tick(&mut acc, &tick, VENUE, symbol, sink);
                }
                // Advisory, the stream stays open: unless it says the connection is gone, which ends the pump as a
                // dead connection (the price stream's supervisor re-dials on it).
                Next::Notice(notice) => {
                    if link_gone_code(notice.code) {
                        stream_ended(lost, "quotes", symbol, &ibapi::Error::Notice(notice));
                        return;
                    }
                }
                Next::Ended(e) => {
                    if let Some(r) = data_refusal(&e) {
                        let wait = lane.refused(&r);
                        sleep_unless_stopped(stop, wait, plan.poll);
                        continue 'open;
                    }
                    stream_ended(lost, "quotes", symbol, &e);
                    if lost.load(Ordering::Relaxed) {
                        // The supervisor's outage window is open and its re-dial closes it: no `Live` from here.
                        return;
                    }
                    break 'open;
                }
                Next::Quiet => {
                    if !stood && opened.elapsed() >= plan.live_after {
                        // Stood without a refusal: the lane is up whether or not a market is sending.
                        stood = true;
                        if lane.gap.is_some() {
                            tracing::info!(
                                %symbol,
                                "ibkr quotes: the quote stream of {symbol} is open again and has stood {} s without a refusal",
                                plan.live_after.as_secs()
                            );
                        }
                        lane.close_gap();
                    }
                    // Bounded sleep: on a clean stream-end, next returns Quiet instantly, so a bare
                    // `Quiet => {}` would busy-spin a core. Cap it at one poll.
                    std::thread::sleep(plan.poll);
                }
            }
        }
    }
    // Stopped (or the subscribe failed): a gap this pump told is closed with it, so no window outlives its stream.
    lane.close_gap();
}

#[cfg(test)]
#[path = "quote_retry_tests.rs"]
mod quote_retry_tests;
