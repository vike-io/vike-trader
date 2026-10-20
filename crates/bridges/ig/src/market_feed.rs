//! Live IG market data → the vike-data live seam. [`Feeds`] implements
//! [`vike_data::DataClient`] — IG's twin of the crypto venues' `market_feed`, over a transport no
//! other venue has: Lightstreamer TLCP 2.1.0 spoken RAW over the shared tungstenite WS stack
//! (`Sec-WebSocket-Protocol: TLCP-2.1.0.lightstreamer.com` — no Lightstreamer client crate, per
//! the one-transport-stack rule). The pure codec is [`crate::lightstreamer`]; the field
//! normalizers are [`crate::market_data`]; this module owns only transport + lifecycle.
//!
//! **Verbs served** (declared caps `{bars, quotes}` — see `vike_venues::venue_caps::IG`):
//! - `subscribe_quotes` → `PRICE:{accountId}:{epic}` on the `Pricing` data adapter (MERGE, snapshot)
//!   → [`LiveDataSink::quote`]. L1 only — the ladder's top tier; `book`/`depth` stay refusable.
//!   ⚠ `MARKET:{epic}` is gone (IG answers every ask `REQERR,1,21,Invalid group`); see
//!   [`crate::market_data`] for what the replacement item needs.
//! - `subscribe_bars` → `CHART:{epic}:{scale}` (MERGE, snapshot) → `forming_bar` +
//!   `close_bar`/`bar_close_tick`. Streaming scales only ([`crate::market_data::ig_scale`]:
//!   1s/1m/5m/1h); other intervals are a `Subscribe` refusal naming the set. ⚠ NO REST seed
//!   before streaming, deliberately: IG meters `/prices` on a weekly per-account allowance
//!   (~10k points), which an auto-seed on every chart open would silently burn — a chart fills
//!   from the first live close forward (the hyperliquid convention). `trades` is refused via
//!   [`vike_data::require_live_verb`]: IG is a dealer venue with no public trade tape.
//!
//! **Lifecycle** — one STOPPABLE thread per subscription ([`vike_data::FeedRegistry`]), each
//! riding the SHARED market-pump session loop ([`vike_bridge_core::market_pump::run_market_feed_on`]) with the
//! knobs of this venue's `MarketPumpSpec` row: stop-poll read timeout, subscribe-ack + silent-
//! stall watchdogs, stop-aware fixed backoff. The TLCP handshake cannot be a replayed subscribe
//! frame (`create_session` must answer CONOK before `control` may be sent), so it lives in the
//! CONNECT closure — the binance-trades shape (a venue-owned connect consuming the row's
//! `connect_timeout`) — and the row declares `subscribe_frame: false`.
//!
//! **A refused subscribe is not a dropped socket, and no failed session is redialed at the pump's fixed
//! 3 s for ever.** A `REQERR` answering the one `control` is IG's verdict on the item (group, epic, schema
//! or adapter), a `CONERR` refuses the session, a login IG answers with a credentials error is refused for
//! good, and the same bytes get the same answer; the shared pump's fixed 3 s redial (a shared home, left as
//! it is) would be ~26,600 Lightstreamer sessions, and for a `CONERR` or a refused login ~26,600 `POST
//! /session` too, a day per lane. [`Refusals`] counts EVERY ended session into one run (a verdict of IG's,
//! or any other end: an `END`, a `LOOP`, an ack or idle timeout, a dial failure) and stretches the interval
//! ([`refusal_wait`]: 6 s doubling, taken in the connect closure, stop-aware, owed once per failed session).
//! The CEILING depends on the kind of the latest failure ([`failure_cap`]): a REFUSAL (a `REQERR`, a `CONERR`
//! that is a verdict, a login refused for its credentials) climbs to 10 min, a plain TRANSPORT failure (a
//! dial failure, a drop, an `END`, a `LOOP`, a timeout, a `CONERR` that is not a refusal) to a minute, so a
//! live lane never goes dark for ten minutes after a network outage. The first `CONERR` or drop of a run is a
//! FREE try (the common cause is a token that expired or a socket that dropped), a refusal of the
//! subscription or a refused login is not; a refused login waits the longest wait at once; the cached login
//! is dropped by the first `CONERR` of a run only (a later one costs a WebSocket handshake, not a `POST
//! /session`); a lane that has been live is capped at a minute for its first few refusals (a blip after
//! quotes flowed is not a verdict); a session that delivers data ends the run. It
//! logs a `warn` for the first failure of a run and again at every dial made at the longest wait (the
//! journal is the operator's surface: `Feeds.status` is not reachable in production, the gatewayhub holds
//! the client as a `Box<dyn DataClient>`), writes what happened into the status line, and discloses a
//! `GapStart` for a lane that was never live when IG refused its subscription, session or login (the
//! dropped-lane channel; a recovery closes it with the usual `Live`).
//!
//! **An acknowledged item can still be dead.** [`Silence`] watches the quote lane for a SUBOK that no
//! update follows, which the shared pump cannot see, and discloses it the same way.
//!
//! **Sessions and tokens.** Each thread logs in with [`IgSession::login`] (the crate's ONE login
//! path) and caches the CST/XST pair across reconnects; the first `CONERR` of a run drops the cache so
//! the next attempt re-logs-in with fresh tokens. This lane therefore re-authenticates after token expiry
//! on its own session. (The exec lane does too, no longer fire-once: its shared [`IgSession`]
//! re-authenticates itself on a 401 — `crates/bridges/ig/src/rest.rs`'s `reauth_ladder`.) Cost:
//! one IG REST session per live subscription (IG tolerates concurrent sessions; the exec start
//! already opens several), renewed only on failure, never per rebind.

use std::cell::{Cell, RefCell};
use std::net::{TcpStream, ToSocketAddrs};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;

use vike_bridge_core::user_data::{StreamError, StreamMsg, UserStream, sleep_unless_stopped};
use vike_bridge_core::ws::{TungsteniteStream, WsSocket, configure_ws_stream};
use vike_bridge_core::{
    market_pump::{FrameOutcome, MarketStream, SessionStatus, run_market_feed_on},
    pump_spec::market_pump_spec,
};
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId,
    require_live_verb,
};
use vike_marketdata::QuoteTick;
use vike_model::{StreamStatus, now_ms};
use vike_venues::LiveVerb;
use vike_venues::venue_timings;

use crate::config::IgConfig;
use crate::lightstreamer::{
    LS_WS_SUBPROTOCOL, LsFrame, create_session_request, parse_frame, subscribe_request,
};
use crate::market_data::{
    CANDLE_ADAPTER, CANDLE_SCHEMA, CandleEvent, CandleFold, QUOTE_ADAPTER, QUOTE_SCHEMA, QuoteFold,
    chart_item, ig_scale, price_item,
};
use crate::market_facts::login_refused_for_good;
use crate::rest::IgSession;

const VENUE: &str = "ig";
/// One subscription per TLCP session, so the id is fixed.
const SUB_ID: u32 = 1;
/// Wall-clock budget for the create_session → CONOK handshake inside the connect closure: the
/// market lane's `handshake_timeout` in `vike_venues::venue_timings::IG`. It is the one
/// market-lane fact the venue's `pump_spec` row cannot carry (the pump's replayed-subscribe-frame
/// shape cannot express a deadline inside the connect closure); every other IG market knob is that
/// row's.
const HANDSHAKE_BUDGET: Duration = venue_timings::IG.market().handshake_timeout();

/// Live IG market data client (one per started venue; subscribe many).
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    /// Human-readable connection status (GUI Connections tool convention).
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: FeedRegistry,
    config: IgConfig,
    timing: Timing,
}

/// How long a lane waits before it calls itself silent, and before its first re-dial after a refusal.
#[derive(Clone, Copy)]
struct Timing {
    /// SUBOK to the first update, on the quote lane: see [`Silence`].
    silent_after: Duration,
    /// [`REFUSED_FIRST_WAIT`].
    refused_first_wait: Duration,
    /// The wait after a login IG answered with a credentials error: [`REFUSED_MAX_WAIT`] in production.
    credentials_wait: Duration,
}

/// Measured on the demo gateway (2026-10-19): a served `PRICE` item delivers its snapshot right behind the
/// SUBOK (every epic asked had an update within the second), and the one that never will (the wrong adapter)
/// delivers nothing but `PROBE`. Fifteen seconds is generous for the first and quick for the second.
const SILENT_AFTER: Duration = Duration::from_secs(15);

const PRODUCTION_TIMING: Timing = Timing {
    silent_after: SILENT_AFTER,
    refused_first_wait: REFUSED_FIRST_WAIT,
    credentials_wait: REFUSED_MAX_WAIT,
};

/// Which lane a feed thread drives.
#[derive(Clone)]
enum Lane {
    Quotes,
    Candles { interval: String, scale: &'static str },
}

impl Lane {
    /// The `(data adapter, LS_group)` this lane subscribes for `epic`. The quote item is named after
    /// the logged-in ACCOUNT, so it is only known once the session is.
    fn item(&self, account_id: &str, epic: &str) -> (&'static str, String) {
        match self {
            Lane::Quotes => (QUOTE_ADAPTER, price_item(account_id, epic)),
            Lane::Candles { scale, .. } => (CANDLE_ADAPTER, chart_item(epic, scale)),
        }
    }
}

/// What the status line says of a lane IG refused. ⚠ The live smoke (`tests/ig_market_feed_smoke.rs`) reads
/// this word to tell a refused lane from a working one: the line has neither "error" nor "failed" in it, so
/// a reword that does not go through this constant blinds the smoke.
#[doc(hidden)]
pub const REFUSED_STATUS: &str = "IG subscription refused";
/// What the status line says of a lane IG acknowledged and then left silent (see [`Silence`]).
#[doc(hidden)]
pub const SILENT_STATUS: &str = "IG subscription silent";

/// The status line of a refused lane: [`REFUSED_STATUS`], what IG said, and when the next try is.
fn refused_status(why: &str, next: Duration) -> String {
    format!("{REFUSED_STATUS} ({why}); next try in {}s or more", next.as_secs())
}

/// The status line of a failed session, by kind: a refused subscription ([`refused_status`]), a refused login
/// ("IG login failed ..."), or the dropped-stream line this lane has always written. `next` is the wait the
/// failure asks of the next dial (`0` for the free first try of a `CONERR` or a drop).
fn failure_status(kind: Kind, words: &str, next: Duration) -> String {
    let secs = next.as_secs();
    match kind {
        Kind::Subscribe => refused_status(words, next),
        Kind::Credentials => format!("IG login failed ({words}); next try in {secs}s or more"),
        Kind::Conerr | Kind::Dropped if next.is_zero() => {
            format!("IG stream error: {words}; reconnecting")
        }
        Kind::Conerr | Kind::Dropped => {
            format!("IG stream error: {words}; reconnecting (next try in {secs}s or more)")
        }
    }
}

/// The `warn` lines of a failure kind: the headline for the FIRST failure of a run, and the one repeated at
/// every dial made at the longest wait.
fn failure_headlines(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Subscribe => (
            "IG refused the market subscription; retrying with a growing wait",
            "IG still refuses the market subscription",
        ),
        Kind::Conerr => (
            "IG refused the Lightstreamer session; reconnecting, and waiting longer if it keeps refusing",
            "IG still refuses the Lightstreamer session",
        ),
        Kind::Credentials => (
            "IG refused the login; retrying only at the longest wait, a refused login is not cured by repeating it",
            "IG still refuses the login",
        ),
        Kind::Dropped => {
            ("IG market stream dropped; reconnecting", "IG market stream is still failing")
        }
    }
}

/// Extra wait before the next dial after the FIRST refused subscribe (see [`refusal_wait`]).
const REFUSED_FIRST_WAIT: Duration = Duration::from_secs(6);
/// The longest extra wait between dials of a lane IG keeps refusing: a refused lane costs a full TLS +
/// WS + `create_session` per try, so this is ~140 sessions a day at most, against the ~26,600 a day the
/// shared pump's fixed 3 s made of `MARKET`'s `REQERR 21`. Not infinite: a refusal an operator cures at
/// IG (a permission, a streaming entitlement) heals without a restart.
const REFUSED_MAX_WAIT: Duration = Duration::from_secs(600);
/// The longest extra wait for a lane that HAS been live, during its first [`LIVE_TRANSIENT_STREAK`] refusals
/// in a row: a refusal after quotes were flowing is more likely a blip than a verdict, and a lane that may
/// hold a position must not go dark for ten minutes over one.
const LIVE_REFUSED_MAX_WAIT: Duration = Duration::from_secs(60);
/// How many refusals in a row a lane that has been live is treated as transient (waits capped at
/// [`LIVE_REFUSED_MAX_WAIT`]) before it backs off like a lane that never was: about four minutes of quick
/// retries, then the standing-refusal cadence (and its log lines) of a never-live lane.
const LIVE_TRANSIENT_STREAK: u32 = 6;

/// The longest extra wait after a plain TRANSPORT failure (a dial failure, a drop, an `END`, a `LOOP`, an ack
/// or idle timeout, a `CONERR` that is not a refusal), whatever the lane was: the same ladder as a refusal
/// (`first`, doubling) but never past a minute, because a lane that was live must not go dark for ten minutes
/// after a long network outage. That is still ~1,400 dials a day against the ~26,600 of the pump's fixed 3 s.
const TRANSPORT_MAX_WAIT: Duration = Duration::from_secs(60);

/// The extra wait before a dial that follows `streak` consecutive failed sessions of kind `kind` (the latest of
/// the run; `0` = the last session delivered, no extra wait). It rides on top of the shared pump's own fixed
/// backoff (the `ig` row of `vike_bridge_core::pump_spec`, a shared home that stays as it is): `first`, twice
/// that, four times ... up to [`failure_cap`]. `first` is [`REFUSED_FIRST_WAIT`] in production.
fn refusal_wait(kind: Kind, streak: u32, ever_live: bool, first: Duration) -> Duration {
    if streak == 0 {
        return Duration::ZERO;
    }
    let doublings = (streak - 1).min(16);
    first.saturating_mul(1 << doublings).min(failure_cap(kind, streak, ever_live))
}

/// The longest [`refusal_wait`] after `streak` failures in a row, by the KIND of the latest one. ⚠ The ceiling
/// follows the LAST failure; the doubling position follows the run: a drop after nine refusals waits a minute
/// (not ten), the refusal after it waits ten minutes again.
/// - a refusal (a `REQERR`, a refusing `CONERR`): a minute for a lane that has been live during its first
///   [`LIVE_TRANSIENT_STREAK`] failures (a blip after quotes flowed is not a verdict), ten minutes otherwise;
/// - a login refused for its credentials: [`REFUSED_MAX_WAIT`] (the wait itself is [`Refusals::wait`]'s);
/// - a plain transport failure: [`TRANSPORT_MAX_WAIT`], always.
fn failure_cap(kind: Kind, streak: u32, ever_live: bool) -> Duration {
    match kind {
        Kind::Dropped => TRANSPORT_MAX_WAIT,
        Kind::Credentials => REFUSED_MAX_WAIT,
        Kind::Subscribe | Kind::Conerr => {
            if ever_live && streak <= LIVE_TRANSIENT_STREAK {
                LIVE_REFUSED_MAX_WAIT
            } else {
                REFUSED_MAX_WAIT
            }
        }
    }
}

/// Is a `CONERR` of this code a REFUSAL (the server's verdict on this client or these credentials: Lightstreamer
/// 1 user/password check failed, 2 adapter set not available, 60/70/71 a client it does not accept, and a code at
/// or below zero, IG's own adapter) or a transient server-side decline (7-11: a session or load limit, new
/// sessions temporarily blocked, streaming unavailable) or a code nobody has seen? Only the first is a
/// refusal; a code that is not named is not a verdict this lane can name, and is never stretched past a minute
/// on a guess.
fn conerr_is_refusal(code: &str) -> bool {
    code.parse::<i32>().is_ok_and(|n| matches!(n, 1 | 2 | 60 | 70 | 71) || n <= 0)
}

/// What a `REQERR` code means, in words (Lightstreamer's client documentation lists 17 and 21-30; a
/// code at or below zero is the venue's own Metadata Adapter speaking, IG's `-1` being "Incorrect
/// instrument setup"). ⚠ IG's `21` does not tell a withdrawn group from an epic it does not know (a
/// bad epic under `MARKET` was `-1` while the group lived, and `21` once it was gone; under `PRICE` a
/// bad epic is `21`), so the words name both.
fn refusal_meaning(code: &str) -> &'static str {
    match code.parse::<i32>() {
        Ok(17) => "the data adapter is not served",
        Ok(21 | 22) => "the item (group or epic) is not served for this session",
        Ok(23) => "a field name of the schema is not served",
        Ok(n) if n <= 0 => {
            "the venue's own adapter refused it (a permission or an instrument setup)"
        }
        _ => "the server refused the subscription",
    }
}

/// What a `CONERR` code means, in words (Lightstreamer's client documentation: 1 the user/password check
/// failed, 2 the adapter set is not available, 7-11 the server's session, load or availability limits,
/// 60/70/71 a client the server does not accept; a code at or below zero is IG's own adapter refusing the
/// session). The number is the evidence: the words are a reading of the documented table.
fn conerr_meaning(code: &str) -> &'static str {
    match code.parse::<i32>() {
        Ok(1) => "the user/password check failed (the session tokens may have expired)",
        Ok(2) => "the adapter set is not available",
        Ok(7..=11) => {
            "the server declined the session (a session or load limit, or streaming unavailable)"
        }
        Ok(60 | 70 | 71) => {
            "this kind of client is not accepted (a client defect, not the account)"
        }
        Ok(n) if n <= 0 => "IG's own adapter refused the session",
        _ => "the server refused the session",
    }
}

/// The quote lane's memory of a RUN of failed sessions (a `REQERR` answering its one subscribe, a `CONERR`
/// refusing the session, a login IG answered with a credentials error, or any other end the shared pump
/// redials at its fixed 3 s): shared by the lane's closures, all of which run on its one thread. The same
/// bytes get the same answer, so [`Refusals::wait`] stretches the interval (the first `CONERR` or drop of
/// a run is a free try: the common cause is benign; the ceiling follows the KIND of the latest failure,
/// [`failure_cap`]), only the FIRST failure of a run is logged and
/// disclosed ([`Refusals::first`]) plus each dial at the longest wait ([`Refusals::at_cap`]), and the
/// cached login is dropped by the first `CONERR` of a run only ([`Refusals::relogin_for_conerr`]). A
/// session that delivers data ends the run ([`Refusals::accepted`]).
struct Refusals {
    /// [`REFUSED_FIRST_WAIT`] in production (a test shortens it through [`Feeds::with_test_timing`]).
    first_wait: Duration,
    /// [`REFUSED_MAX_WAIT`] in production: the wait after a credentials error (a test shortens it).
    credentials_wait: Duration,
    /// Consecutive failed sessions; `0` once one delivered.
    streak: Cell<u32>,
    /// How many failures at the start of a run are a free try: 0 for a verdict on the subscription or the
    /// credentials, 1 for a `CONERR` or a drop (set by the first failure of the run).
    grace: Cell<u32>,
    /// The kind of the latest failure.
    kind: Cell<Kind>,
    /// Whether this lane has ever delivered a quote (a failure after that is treated as transient first).
    ever_live: Cell<bool>,
    /// A failure ended the last session, so the next dial owes the wait, once ([`Refusals::due`]).
    owed: Cell<bool>,
    /// A `CONERR` has already dropped the cached login in this run.
    relogin_spent: Cell<bool>,
    /// Failed sessions since the last `warn` of this run (the dials a throttled line counts).
    failures_since_warn: Cell<u32>,
    /// When the run's last `warn` was written (epoch ms), `None` before the first.
    last_warn_ms: Cell<Option<i64>>,
    /// When the run's first failure was reported (epoch ms), for the recovery line.
    run_started_ms: Cell<Option<i64>>,
    /// What `on_text`, the handshake or the login saw of the session that is ending, and
    /// `on_session_status` has not yet counted.
    unread: RefCell<Option<Counted>>,
}

/// What kind of failure ended a session: it decides the free first try, the ceiling of the wait
/// ([`failure_cap`]: a REFUSAL climbs to ten minutes, a plain transport failure to a minute), the wording and
/// the login budget.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A `REQERR` answering the subscribe. A refusal.
    Subscribe,
    /// A `CONERR` that is a REFUSAL (see [`conerr_is_refusal`]): the server's verdict on this client or these
    /// credentials. A refusal.
    Conerr,
    /// The REST login was answered with a credentials error. A refusal, and the longest wait at once.
    Credentials,
    /// Anything else the shared pump redials at its fixed 3 s, TRANSPORT-class: an `END`, a `LOOP`, an ack or
    /// idle timeout, a dial or read failure, a login that never reached IG, a `CONERR` that is not a refusal
    /// (a session or load limit, a code nobody has seen).
    Dropped,
}

/// What one ended session counted into the run.
struct Counted {
    kind: Kind,
    /// What to say of it (IG's own words for a verdict, the pump's for a drop).
    words: String,
}

impl Refusals {
    fn new(first_wait: Duration, credentials_wait: Duration) -> Self {
        Refusals {
            first_wait,
            credentials_wait,
            streak: Cell::new(0),
            grace: Cell::new(0),
            kind: Cell::new(Kind::Dropped),
            ever_live: Cell::new(false),
            owed: Cell::new(false),
            relogin_spent: Cell::new(false),
            failures_since_warn: Cell::new(0),
            last_warn_ms: Cell::new(None),
            run_started_ms: Cell::new(None),
            unread: RefCell::new(None),
        }
    }

    /// The session that is ending failed in `kind`, in `words`.
    fn saw_as(&self, kind: Kind, words: String) {
        *self.unread.borrow_mut() = Some(Counted { kind, words });
    }

    /// `on_text` saw a `REQERR` answering the subscribe.
    fn saw(&self, code: &str, message: &str) {
        self.saw_as(Kind::Subscribe, format!("REQERR {code} {message}: {}", refusal_meaning(code)));
    }

    /// `on_text` (or the handshake) saw a `CONERR`.
    fn saw_conerr(&self, code: &str, message: &str) {
        let kind = if conerr_is_refusal(code) { Kind::Conerr } else { Kind::Dropped };
        self.saw_as(kind, format!("CONERR {code} {message}: {}", conerr_meaning(code)));
    }

    /// The REST login was answered with a credentials error (`words` is IG's status and `errorCode`, never
    /// a credential).
    fn saw_login(&self, words: &str) {
        self.saw_as(Kind::Credentials, words.to_string());
    }

    /// A session ended with `error`: counted into the run, by the kind of failure it was (what was seen of
    /// it, or a drop: an `END`, a `LOOP`, a timeout, a dial or read failure, a login that never reached IG).
    fn ended(&self, error: &str) -> Counted {
        let counted = self
            .unread
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Counted { kind: Kind::Dropped, words: error.to_string() });
        let streak = self.streak.get().saturating_add(1);
        if streak == 1 {
            self.grace.set(match counted.kind {
                Kind::Subscribe | Kind::Credentials => 0,
                Kind::Conerr | Kind::Dropped => 1,
            });
        }
        self.streak.set(streak);
        self.kind.set(counted.kind);
        self.owed.set(true);
        self.failures_since_warn.set(self.failures_since_warn.get().saturating_add(1));
        counted
    }

    /// What the failure just counted ([`Refusals::ended`]) says at `warn` level, at `now_ms`. The FIRST
    /// failure of a run speaks at once. After it, a failure speaks only at its kind's longest wait
    /// ([`Refusals::at_cap`]): a REFUSAL at its ten minutes, every dial (about 144 lines a day); a plain
    /// TRANSPORT failure at its minute, once per [`TRANSPORT_WARN_EVERY`] counting the dials in between
    /// (a standing outage would otherwise write a line every 63 s).
    fn say(&self, now_ms: i64) -> Say {
        let dials = self.failures_since_warn.get();
        let spoke = |say| {
            self.last_warn_ms.set(Some(now_ms));
            self.failures_since_warn.set(0);
            say
        };
        if self.first() {
            self.run_started_ms.set(Some(now_ms));
            return spoke(Say::First);
        }
        if !self.at_cap() {
            return Say::Quiet;
        }
        if self.kind.get() == Kind::Dropped {
            let every = TRANSPORT_WARN_EVERY.as_millis() as i64;
            if self.last_warn_ms.get().is_some_and(|at| now_ms - at < every) {
                return Say::Quiet;
            }
        }
        spoke(Say::Cap { dials })
    }

    /// A session delivered while a run of failures was open: the run's summary, once (the next call, with no
    /// run open, is `None`). `now_ms` is read only when a run was open.
    fn recovered(&self, now_ms: impl FnOnce() -> i64) -> Option<Recovery> {
        let (failures, started) = (self.streak.get(), self.run_started_ms.get());
        if !self.accepted() {
            return None;
        }
        Some(Recovery { failures, millis: started.map_or(0, |at| now_ms() - at) })
    }

    /// The failures of this run that count against the wait: the free first try of a `CONERR` or a drop is
    /// not one.
    fn effective(&self) -> u32 {
        self.streak.get().saturating_sub(self.grace.get())
    }

    /// May a `CONERR` drop the cached login? Once per run: the first is most likely a token that expired;
    /// a second after a fresh login is not expiry, and costs a WebSocket handshake, not a `POST /session`.
    fn relogin_for_conerr(&self) -> bool {
        !self.relogin_spent.replace(true)
    }

    /// Is the current failure the first of its run (the only one that is logged and disclosed)?
    fn first(&self) -> bool {
        self.streak.get() == 1
    }

    /// Has the wait reached the longest it will be for this lane (each such dial is logged, so the standing
    /// cadence stays audible in the journal)?
    fn at_cap(&self) -> bool {
        if self.streak.get() == 0 {
            return false;
        }
        if self.kind.get() == Kind::Credentials {
            return true;
        }
        let effective = self.effective();
        effective > 0
            && self.wait() == failure_cap(self.kind.get(), effective, self.ever_live.get())
    }

    /// The extra wait before the next dial: the longest at once after a credentials error (the same
    /// credentials get the same answer, and a documented lockout waits at the end of repeated tries),
    /// otherwise [`refusal_wait`] over the failures that count.
    fn wait(&self) -> Duration {
        if self.streak.get() > 0 && self.kind.get() == Kind::Credentials {
            return self.credentials_wait;
        }
        refusal_wait(self.kind.get(), self.effective(), self.ever_live.get(), self.first_wait)
    }

    /// What `connect` sleeps before this dial: the wait if a failure ended the last session, else nothing.
    fn due(&self) -> Duration {
        if self.owed.replace(false) { self.wait() } else { Duration::ZERO }
    }

    /// The lane delivered a quote.
    fn live(&self) {
        self.ever_live.set(true);
    }

    /// A session delivered data: the run is over (and the login budget is back).
    fn accepted(&self) -> bool {
        self.owed.set(false);
        self.relogin_spent.set(false);
        self.grace.set(0);
        self.kind.set(Kind::Dropped);
        self.failures_since_warn.set(0);
        self.last_warn_ms.set(None);
        self.run_started_ms.set(None);
        self.streak.replace(0) > 0
    }
}

/// What one failed session says at `warn` level ([`Refusals::say`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Say {
    /// Nothing: not the first of the run, and not at its kind's longest wait (or inside the throttle).
    Quiet,
    /// The first failure of the run.
    First,
    /// A dial made at the kind's longest wait: `dials` failed sessions since the last line.
    Cap { dials: u32 },
}

/// How long a run of failures lasted when a session delivered again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Recovery {
    /// Failed sessions in the run.
    failures: u32,
    /// From the first reported failure to the delivering session, in ms.
    millis: i64,
}

/// A plain transport failure at its longest wait writes one `warn` per this long, counting the dials between.
const TRANSPORT_WARN_EVERY: Duration = Duration::from_secs(600);

/// The quote lane's watch on a subscription IG ACKNOWLEDGED and then said nothing on. A SUBOK proves
/// nothing about data (measured: `PRICE` on the wrong adapter is ACKed and stays silent, and the shared pump
/// cannot tell: its ack watchdog is satisfied by the SUBOK, its idle watchdog by the server's `PROBE`s), but
/// a served item sends its snapshot (`LS_snapshot=true`) behind the SUBOK, so "no update at all
/// [`SILENT_AFTER`] after the SUBOK" is a precise dead-lane test. It is judged in the pump's `on_tick` hook,
/// which cannot end the session, so it discloses (a `warn`, the status line, a `GapStart`) and does not
/// reconnect, which would not cure a mis-addressed item anyway; the first update closes the gap as usual.
#[derive(Default)]
struct Silence {
    /// When this session's SUBOK arrived, until its first update does.
    waiting_since: Cell<Option<Instant>>,
    /// Whether this session has been called silent already (once per session, not per tick).
    told: Cell<bool>,
}

impl Silence {
    /// The SUBOK arrived.
    fn acked(&self) {
        self.waiting_since.set(Some(Instant::now()));
        self.told.set(false);
    }

    /// An update arrived: the lane is not silent.
    fn heard(&self) {
        self.waiting_since.set(None);
    }

    /// Is the lane silent past `after`? True once per session.
    fn overdue(&self, after: Duration) -> bool {
        match self.waiting_since.get() {
            Some(since) if !self.told.get() && since.elapsed() >= after => {
                self.told.set(true);
                true
            }
            _ => false,
        }
    }
}

impl Feeds {
    /// `sink` receives every quote/forming/close call from every subscription; `wake` is the GUI
    /// repaint nudge (pass `|| {}` headless); `config` is the SAME login shape the exec side uses
    /// ([`crate::load_ig_config_from`]) — this client opens its own sessions from it and never
    /// shares the exec side's. The registry's spawn hook is the venue's HFT affinity pin.
    pub fn new(
        sink: Arc<dyn LiveDataSink>,
        wake: impl Fn() + Send + Sync + 'static,
        config: IgConfig,
    ) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to IG…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    VENUE,
                );
            }),
            config,
            timing: PRODUCTION_TIMING,
        }
    }

    /// ⚠ Test-only: shorten the three waits the offline suites would otherwise sit through for real
    /// (`crates/bridges/ig/tests/offline/quote_feed.rs`). The shared pump's own 3 s redial is a row of
    /// `vike_bridge_core::pump_spec` and cannot be shortened. Not public API.
    #[doc(hidden)]
    #[must_use]
    pub fn with_test_timing(
        mut self,
        silent_after: Duration,
        refused_first_wait: Duration,
        credentials_wait: Duration,
    ) -> Self {
        self.timing = Timing { silent_after, refused_first_wait, credentials_wait };
        self
    }

    fn spawn(&mut self, epic: &str, lane: Lane) -> Result<SubscriptionId, LiveDataError> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (config, epic_owned, timing) = (self.config.clone(), epic.to_string(), self.timing);
        let label = match &lane {
            Lane::Quotes => StreamLabel::Quotes.as_str().to_string(),
            Lane::Candles { interval, .. } => interval.clone(),
        };
        self.registry
            .spawn(format!("feed-ig-{epic}@{label}"), move |stop| {
                feed_main(epic_owned, lane, FeedCtx { sink, status, wake, stop, config, timing })
            })
            .map_err(|e| LiveDataError::Subscribe(format!("ig {epic}@{label}: {e}")))
    }
}

impl DataClient for Feeds {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let Some(scale) = ig_scale(interval) else {
            return Err(LiveDataError::Subscribe(format!(
                "ig streams no {interval} candles (Lightstreamer scales: 1s/1m/5m/1h; other \
                 resolutions are REST-history only)"
            )));
        };
        self.spawn(symbol, Lane::Candles { interval: interval.to_string(), scale })
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        require_live_verb(VENUE, LiveVerb::Quotes)?;
        self.spawn(symbol, Lane::Quotes)
    }

    fn subscribe_trades(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (ig live_data.trades = false): a dealer venue has no trade tape.
        require_live_verb(VENUE, LiveVerb::Trades)?;
        unreachable!("ig declares no live trades")
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (ig live_data.book = false): IG streams L1 BID/OFFER only.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("ig declares no live book")
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

/// Everything a feed thread owns.
struct FeedCtx {
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
    config: IgConfig,
    timing: Timing,
}

/// Cached Lightstreamer credentials (from one [`IgSession::login`]), reused across reconnects
/// until a `CONERR` proves them dead.
struct LsAuth {
    ws_url: String,
    user: String,
    password: String,
}

/// The whole thread body for one subscription: login/handshake in the connect closure, decode +
/// emit in `on_text`, gap disclosure in `on_session_status`'s `Error` arm (and the healthy status
/// line in its `Live` arm), lifecycle in the shared driver.
fn feed_main(epic: String, lane: Lane, ctx: FeedCtx) {
    let knobs = market_pump_spec(VENUE).knobs();
    let opts = knobs.opts(None, None);

    let (schema, label) = match &lane {
        Lane::Quotes => (QUOTE_SCHEMA.join(" "), StreamLabel::Quotes.as_str().to_string()),
        Lane::Candles { interval, .. } => (CANDLE_SCHEMA.join(" "), interval.clone()),
    };

    // Shared across the driver's closures (all run on THIS thread; Rc is deliberate).
    let auth: Rc<RefCell<Option<LsAuth>>> = Rc::new(RefCell::new(None));
    let live = Rc::new(Cell::new(false));
    let gap_started: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));
    let refusals =
        Rc::new(Refusals::new(ctx.timing.refused_first_wait, ctx.timing.credentials_wait));
    let silence = Rc::new(Silence::default());

    let connect = {
        let (auth, refusals) = (Rc::clone(&auth), Rc::clone(&refusals));
        let (ctx_config, stop) = (ctx.config.clone(), Arc::clone(&ctx.stop));
        let (lane, epic, schema) = (lane.clone(), epic.clone(), schema.clone());
        move || -> Result<LsStream, String> {
            // A lane IG keeps refusing waits longer each time, on top of the pump's fixed backoff.
            let wait = refusals.due();
            if !wait.is_zero() {
                sleep_unless_stopped(&stop, wait);
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err("stop requested while waiting out a refusal".into());
                }
            }
            if auth.borrow().is_none() {
                let session = match IgSession::login(&ctx_config) {
                    Ok(session) => session,
                    Err(e) => {
                        // IG ANSWERED with a credentials error (400, 401, a 403 that is not an
                        // allowance): the same credentials get the same answer, and a documented
                        // lockout (`error.security.too-many-failed-attempts`) waits at the end of
                        // repeated tries, so the next dial waits the longest wait. A login that never
                        // reached IG, a 429 or a 5xx is an ordinary drop. `e` carries IG's status and
                        // `errorCode`, never a credential.
                        if login_refused_for_good(e.status, &e.message) {
                            refusals.saw_login(&e.to_string());
                        }
                        return Err(format!("login failed: {e}"));
                    }
                };
                if session.lightstreamer_endpoint.is_empty() {
                    return Err("login returned no lightstreamerEndpoint".into());
                }
                *auth.borrow_mut() = Some(LsAuth {
                    ws_url: ls_ws_url(&session.lightstreamer_endpoint),
                    user: session.account_id.clone(),
                    password: session.ls_password(),
                });
            }
            let res = {
                let borrowed = auth.borrow();
                let a = borrowed.as_ref().expect("auth cached above");
                let (adapter, group) = lane.item(&a.user, &epic);
                open_ls_session(
                    a,
                    adapter,
                    &group,
                    &schema,
                    knobs.read_timeout,
                    opts.connect_timeout,
                    &stop,
                )
            };
            match res {
                Ok(stream) => Ok(stream),
                Err(HandshakeErr::Refused { code, message }) => {
                    // CONERR: the cached CST/XST pair may be dead (a token that expired) — the FIRST
                    // of a run drops it so the next attempt re-logs-in fresh; a second after a fresh
                    // login is not expiry, and keeps it (a WebSocket handshake, not a `POST /session`).
                    if refusals.relogin_for_conerr() {
                        *auth.borrow_mut() = None;
                    }
                    refusals.saw_conerr(&code, &message);
                    Err(format!("CONERR,{code},{message}"))
                }
                Err(HandshakeErr::Transport(msg)) => Err(msg),
            }
        }
    };

    let on_text = {
        let (auth, live, gap_started, refusals, silence) = (
            Rc::clone(&auth),
            Rc::clone(&live),
            Rc::clone(&gap_started),
            Rc::clone(&refusals),
            Rc::clone(&silence),
        );
        let (sink, wake, status) =
            (Arc::clone(&ctx.sink), Arc::clone(&ctx.wake), Arc::clone(&ctx.status));
        let (epic, label) = (epic.clone(), label.clone());
        let mut quote_fold = QuoteFold::new();
        let mut candle_fold = CandleFold::new();
        let mut lane_kind = match &lane {
            Lane::Quotes => LaneKind::Quotes,
            Lane::Candles { interval, .. } => LaneKind::Candles { interval: interval.clone() },
        };
        move |text: &str| -> FrameOutcome {
            let mut outcome = FrameOutcome::Ignore;
            // One WS message may bundle several CR-LF-terminated TLCP lines.
            for line in text.lines().map(str::trim_end) {
                let frame = match parse_frame(line) {
                    Some(f) => f,
                    None => continue,
                };
                match frame {
                    LsFrame::Update(u) if u.sub_id == SUB_ID => {
                        silence.heard();
                        // A session that DELIVERS ends the run of failures (a SUBOK alone does not), and
                        // says so ONCE: how many failures, how long.
                        if let Some(rec) = refusals.recovered(now_ms) {
                            let (failures, minutes) = (rec.failures, rec.millis as f64 / 60_000.0);
                            tracing::info!(target: "vike_ig::market_feed", %epic, %label, failures, minutes, "IG {epic}@{label} recovered after {failures} failures / {minutes:.1} min");
                        }
                        let emitted = match &mut lane_kind {
                            LaneKind::Quotes => emit_quote(&mut quote_fold, &u, &sink, &epic),
                            LaneKind::Candles { interval } => {
                                emit_candles(&mut candle_fold, &u, &sink, &epic, interval)
                            }
                        };
                        if emitted {
                            if !live.get() {
                                live.set(true);
                                refusals.live();
                                *status.lock().unwrap() = format!("IG live ({epic}@{label})");
                                sink.stream_status(
                                    VENUE,
                                    &epic,
                                    &label,
                                    StreamStatus::Live { gap_started_ts_ms: gap_started.take() },
                                );
                            }
                            (wake)();
                        }
                        outcome = FrameOutcome::Confirm;
                    }
                    LsFrame::Update(_) | LsFrame::Reqok | LsFrame::Probe | LsFrame::Other(_) => {}
                    LsFrame::Subok { .. } => {
                        silence.acked();
                        outcome = FrameOutcome::Confirm;
                    }
                    LsFrame::Conok { .. } => {} // handshake already consumed one; late/dup is inert
                    LsFrame::Loop => {
                        return FrameOutcome::Fatal("server requested rebind (LOOP)".into());
                    }
                    LsFrame::End { code, message } => {
                        return FrameOutcome::Fatal(format!(
                            "session ended (END,{code},{message})"
                        ));
                    }
                    LsFrame::Conerr { code, message } => {
                        if refusals.relogin_for_conerr() {
                            *auth.borrow_mut() = None;
                        }
                        refusals.saw_conerr(&code, &message);
                        return FrameOutcome::Fatal(format!("CONERR,{code},{message}"));
                    }
                    LsFrame::Reqerr { req_id, code, message } => {
                        refusals.saw(&code, &message);
                        return FrameOutcome::Fatal(format!(
                            "subscribe refused (REQERR,{req_id},{code},{message})"
                        ));
                    }
                }
            }
            outcome
        }
    };

    // ⚠ TWO corrections ride along here, and the second one is deliberately NOT made.
    //
    // (a) This venue already wrote a success string — inside its own connect closure, as
    //     `"IG stream up ({group})"`. It was not a latch, and it was still wrong: that text
    //     contains NONE of `vike_model::parse_feed_status`'s Connected tokens (`connected` /
    //     `live` / `streaming` / `subscribed` — "stream up" is not "streaming"), so it parsed
    //     `Unknown` and painted the MUTED dot. It is respelled `"LIVE · IG"` and moved into the
    //     `Live` arm, so one rule holds tree-wide rather than this venue keeping a private half-fix.
    //
    // (b) The `on_text` half — `live.set(true)` plus `sink.stream_status(StreamStatus::Live {..})`
    //     — is LEFT EXACTLY WHERE IT IS. That is the TYPED channel, with a different consumer
    //     (`vike_venues::link_deadman`), and the two halves living apart is precisely what produced
    //     ig's own half-fix. Routing this hook through `StreamStatus` would make
    //     `CEX_BOOK_LANES`'s claim that the kline/mark/trade lanes disclose no link state false,
    //     and put decision 0038's connection dead-man one sibling PR away from cancelling resting
    //     orders on a kline blip. This stays a status-LINE disclosure.
    let on_session_status = {
        let (live, gap_started) = (Rc::clone(&live), Rc::clone(&gap_started));
        let (refusals, stop) = (Rc::clone(&refusals), Arc::clone(&ctx.stop));
        let (sink, wake, status) =
            (Arc::clone(&ctx.sink), Arc::clone(&ctx.wake), Arc::clone(&ctx.status));
        let (epic, label) = (epic.clone(), label.clone());
        move |s: SessionStatus<'_>| match s {
            SessionStatus::Live => {
                *status.lock().unwrap() = "LIVE · IG".to_string();
                (wake)();
            }
            SessionStatus::Error(e) => {
                // A requested stop (the connect closure's wait, the handshake) ends a session with an
                // `Err` too: that is not a fault, and counts for nothing.
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                // Every end is counted into the run (`Refusals`): a verdict of IG's (a REQERR, a CONERR,
                // a refused login) in IG's words, any other end (an END, a LOOP, a timeout, a dial
                // failure) in the pump's. ONE warn for the first failure of a run, one more at every
                // dial made at the longest wait, the status line says what happened, and the dial
                // interval grows (`refusal_wait`).
                let counted = refusals.ended(e);
                let next = refusals.wait();
                let (headline, again) = failure_headlines(counted.kind);
                match refusals.say(now_ms()) {
                    Say::First => {
                        tracing::warn!(target: "vike_ig::market_feed", %epic, %label, error = %counted.words, "{headline}");
                    }
                    Say::Cap { dials } => {
                        // The standing cadence stays audible: `Feeds.status` is not reachable in
                        // production (the gatewayhub holds the client as `Box<dyn DataClient>`) and the
                        // journal is the operator's surface. A refusal at its ten minutes logs every
                        // dial (~144 lines a day); a transport failure at its minute logs one line per
                        // ten minutes that counts the dials in between.
                        let line = if counted.kind == Kind::Dropped {
                            format!("{again}: {dials} dials in the last 10 min")
                        } else {
                            again.to_string()
                        };
                        tracing::warn!(target: "vike_ig::market_feed", %epic, %label, error = %counted.words, failures = refusals.streak.get(), next_try_s = next.as_secs(), "{line}");
                    }
                    Say::Quiet => {}
                }
                *status.lock().unwrap() = failure_status(counted.kind, &counted.words, next);
                // A lane refused from its first subscribe, its session or its login was never live,
                // but it is as down as one that dropped: say so ONCE (the first failure of the run),
                // on the channel a dropped lane uses, so the node and the operator can tell it from a
                // market with no quotes. A plain drop of a lane that never was live stays quiet.
                let opens_gap = counted.kind != Kind::Dropped
                    && refusals.first()
                    && gap_started.get().is_none();
                if live.replace(false) || opens_gap {
                    let at = now_ms();
                    gap_started.set(Some(at));
                    sink.stream_status(
                        VENUE,
                        &epic,
                        &label,
                        StreamStatus::GapStart { at_ts_ms: at },
                    );
                }
                (wake)();
            }
        }
    };

    // The quote lane's dead-lane watch (`Silence`), on the pump's dataless-tick hook: it cannot end the
    // session, so it discloses (a `warn`, the status line, a `GapStart`) and the first update closes it.
    let on_tick = {
        let (silence, gap_started) = (Rc::clone(&silence), Rc::clone(&gap_started));
        let (sink, wake, status) =
            (Arc::clone(&ctx.sink), Arc::clone(&ctx.wake), Arc::clone(&ctx.status));
        let (epic, label) = (epic.clone(), label.clone());
        let silent_after = matches!(lane, Lane::Quotes).then_some(ctx.timing.silent_after);
        move || {
            let Some(after) = silent_after else { return };
            if !silence.overdue(after) {
                return;
            }
            tracing::warn!(target: "vike_ig::market_feed", %epic, %label, after = ?after, "IG acknowledged the market subscription and has sent no update since: the item may be mis-addressed (data adapter) or not streamed to this account");
            *status.lock().unwrap() =
                format!("{SILENT_STATUS} (acknowledged, no update within {}s)", after.as_secs());
            if gap_started.get().is_none() {
                let at = now_ms();
                gap_started.set(Some(at));
                sink.stream_status(VENUE, &epic, &label, StreamStatus::GapStart { at_ts_ms: at });
            }
            (wake)();
        }
    };

    run_market_feed_on(connect, &opts, &ctx.stop, &now_ms, on_text, on_tick, on_session_status);
}

/// The lane discriminant `on_text` matches on (owned clone of [`Lane`]'s data).
enum LaneKind {
    Quotes,
    Candles { interval: String },
}

/// Fold one update into the quote lane; returns whether anything was emitted.
fn emit_quote(
    fold: &mut QuoteFold,
    u: &crate::lightstreamer::LsUpdate,
    sink: &Arc<dyn LiveDataSink>,
    epic: &str,
) -> bool {
    let Some(q) = fold.on_update(u) else {
        return false;
    };
    let ts = now_ms();
    sink.quote(
        VENUE,
        epic,
        QuoteTick {
            ts,
            local_ts: ts,
            bid: q.bid,
            ask: q.offer,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: epic.to_string(),
        },
    );
    true
}

/// Fold one update into the candle lane; returns whether anything was emitted.
fn emit_candles(
    fold: &mut CandleFold,
    u: &crate::lightstreamer::LsUpdate,
    sink: &Arc<dyn LiveDataSink>,
    epic: &str,
    interval: &str,
) -> bool {
    let events = fold.on_update(u);
    let emitted = !events.is_empty();
    for ev in events {
        match ev {
            CandleEvent::Forming(bar) => sink.forming_bar(VENUE, epic, interval, bar),
            CandleEvent::Closed(bar) => {
                sink.bar_close_tick(VENUE, epic, bar.close, bar.ts);
                sink.close_bar(VENUE, epic, interval, bar);
            }
        }
    }
    emitted
}

/// Why an attempted TLCP session did not come up.
enum HandshakeErr {
    /// The server answered `CONERR` — the credentials (or session request) were refused; the first
    /// of a run drops the cached tokens ([`Refusals::relogin_for_conerr`]).
    Refused { code: String, message: String },
    /// Anything transport-shaped (dial/read/timeout) — retry with the same tokens.
    Transport(String),
}

/// Dial the Lightstreamer WS, run the TLCP handshake (`create_session` → `CONOK`) and send the
/// one `control LS_op=add` subscribe. Returns the stream ready for the driver's read loop (the
/// `SUBOK`/first-`U` ack is observed there).
fn open_ls_session(
    auth: &LsAuth,
    adapter: &str,
    group: &str,
    schema: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
    stop: &AtomicBool,
) -> Result<LsStream, HandshakeErr> {
    let sock = dial_ls(&auth.ws_url, connect_timeout).map_err(HandshakeErr::Transport)?;
    configure_ws_stream(&sock, read_timeout);
    let mut stream = LsStream { inner: TungsteniteStream(sock), last_rx: Instant::now() };

    stream
        .send_text(&create_session_request(&auth.user, &auth.password))
        .map_err(|e| HandshakeErr::Transport(format!("create_session send: {e:?}")))?;

    // Wait for CONOK/CONERR (read ticks bounded by HANDSHAKE_BUDGET, stop-aware).
    let deadline = Instant::now() + HANDSHAKE_BUDGET;
    'conok: loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(HandshakeErr::Transport("stop requested mid-handshake".into()));
        }
        if Instant::now() >= deadline {
            return Err(HandshakeErr::Transport("no CONOK within handshake budget".into()));
        }
        let text = match stream.read_frame() {
            Ok(t) => t,
            Err(StreamError::Timeout) => continue,
            Err(StreamError::Closed(m)) => {
                return Err(HandshakeErr::Transport(format!("closed during handshake: {m}")));
            }
        };
        for line in text.lines().map(str::trim_end) {
            match parse_frame(line) {
                Some(LsFrame::Conok { .. }) => break 'conok,
                Some(LsFrame::Conerr { code, message }) => {
                    return Err(HandshakeErr::Refused { code, message });
                }
                Some(LsFrame::End { code, message }) => {
                    return Err(HandshakeErr::Transport(format!("END,{code},{message}")));
                }
                _ => {} // SERVNAME/CLIENTIP/CONS/... — inert preamble
            }
        }
    }

    stream
        .send_text(&subscribe_request(1, SUB_ID, adapter, "MERGE", group, schema, true))
        .map_err(|e| HandshakeErr::Transport(format!("subscribe send: {e:?}")))?;
    Ok(stream)
}

/// `https://apd.marketdatasystems.com` (the login's `lightstreamerEndpoint`) → the TLCP WS URL.
///
/// ⚠ `pub` for ONE reason: `crates/bridges/ig/tests/ig_market_group_probe.rs` dials the venue to
/// re-ask which Lightstreamer item groups IG still serves, and a probe that rebuilt this URL from
/// its own reading of the endpoint would be measuring a URL production does not send. Same shape
/// as the crate's other `#[doc(hidden)]` test-only exports. Not public API.
#[doc(hidden)]
pub fn ls_ws_url(endpoint: &str) -> String {
    let e = endpoint.trim_end_matches('/');
    let (scheme, rest) = match e.split_once("://") {
        Some(("http", r)) => ("ws", r),
        Some((_, r)) => ("wss", r),
        None => ("wss", e),
    };
    format!("{scheme}://{rest}/lightstreamer")
}

/// Dial the Lightstreamer WS with the mandatory TLCP subprotocol header, ALWAYS under a connect
/// bound.
///
/// ⚠ **This cannot call `vike_bridge_core::ws_proxy::connect_ws`, and the reason is a header.**
/// TLCP 2.1.0 requires the WS handshake to carry
/// `Sec-WebSocket-Protocol: TLCP-2.1.0.lightstreamer.com`; `connect_ws` takes a URL and builds its
/// own request, so it has nowhere to put one. Extending it would mean editing a shared home every
/// venue dials through for one venue's protocol quirk, which the shared-home rule forbids in
/// passing. So the BOUND is reproduced here rather than the dial being re-spelled loosely:
/// `connect_timeout` is spent across name resolution AND every address the name resolves to, in
/// order — the same accounting `connect_ws`'s bounded arm does, and for the same reason (a
/// per-address bound would make the true ceiling `n x window` for an `n` DNS picks).
///
/// There is deliberately **no unbounded arm**. A `None` row falls back to
/// [`vike_bridge_core::pump_spec::CONNECT_10S`] rather than to a bare `tungstenite::connect`,
/// because that call applies no connect bound at all: a feed thread whose SYNs are black-holed
/// ignores its stop flag for the OS's own SYN ladder (~127 s on Linux defaults) with the flag
/// already raised. `crates/vike-ops/tests/venues/feed_stop_windows_gate.rs`'s
/// `no_feed_path_file_opens_an_unbounded_window` is the gate, and it caught exactly that arm here.
///
/// Residual, shared with `connect_ws`: the bound covers the TCP phase only — the TLS + WS
/// handshake that follows is not inside it.
fn dial_ls(ws_url: &str, connect_timeout: Option<Duration>) -> Result<WsSocket, String> {
    let bound = connect_timeout.unwrap_or(vike_bridge_core::pump_spec::CONNECT_10S);
    let mut req = ws_url.into_client_request().map_err(|e| format!("bad LS url {ws_url}: {e}"))?;
    req.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_static(LS_WS_SUBPROTOCOL));

    let (host, port) = ws_host_port(ws_url)?;
    let started = Instant::now();
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}:{port}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("resolve {host}:{port}: no addresses"));
    }
    let mut last_err = String::new();
    for addr in addrs {
        let Some(remaining) = bound.checked_sub(started.elapsed()) else { break };
        match TcpStream::connect_timeout(&addr, remaining) {
            Ok(tcp) => return Ok(tungstenite::client_tls(req, tcp).map_err(|e| e.to_string())?.0),
            Err(e) => last_err = format!("{addr}: {e}"),
        }
    }
    Err(format!("dial {host}:{port} exhausted its {bound:?} connect bound ({last_err})"))
}

/// Host + port of a `ws(s)://host[:port]/…` URL (default 443, or 80 for plain `ws://`).
fn ws_host_port(url: &str) -> Result<(String, u16), String> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("no scheme in {url}"))?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    match authority.split_once(':') {
        Some((h, p)) => Ok((h.to_string(), p.parse().map_err(|e| format!("bad port: {e}"))?)),
        None => Ok((authority.to_string(), if scheme == "ws" { 80 } else { 443 })),
    }
}

/// tungstenite [`MarketStream`] for the driver (the same wrap as the shared driver's private
/// stream): a Ping is auto-ponged and looped past; a Close surfaces as `Err(Closed)`; every
/// inbound frame stamps `last_rx` for the idle watchdog.
struct LsStream {
    inner: TungsteniteStream,
    last_rx: Instant,
}

impl LsStream {
    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.inner.send_text(s)
    }
}

impl MarketStream for LsStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        loop {
            match self.inner.recv() {
                Ok(StreamMsg::Text(t)) => {
                    self.last_rx = Instant::now();
                    return Ok(t);
                }
                Ok(StreamMsg::Ping(p)) => {
                    self.last_rx = Instant::now();
                    self.inner.pong(p)?;
                }
                Ok(StreamMsg::Other) => self.last_rx = Instant::now(),
                Err(e) => return Err(e),
            }
        }
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.inner.send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        self.last_rx.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_ws_url_rewrites_scheme_and_appends_path() {
        assert_eq!(
            ls_ws_url("https://demo-apd.marketdatasystems.com"),
            "wss://demo-apd.marketdatasystems.com/lightstreamer"
        );
        assert_eq!(
            ls_ws_url("https://apd.marketdatasystems.com:443/"),
            "wss://apd.marketdatasystems.com:443/lightstreamer"
        );
        assert_eq!(ls_ws_url("http://local:8080"), "ws://local:8080/lightstreamer");
        assert_eq!(ls_ws_url("push.ig.com"), "wss://push.ig.com/lightstreamer");
    }

    /// The create_session → CONOK deadline `open_ls_session` arms is the market row's 15 s.
    #[test]
    fn the_handshake_budget_is_the_market_rows_fifteen_seconds() {
        assert_eq!(HANDSHAKE_BUDGET, Duration::from_secs(15));
    }

    /// Which adapter and item each lane subscribes: the quote item is named after the account, from
    /// `Pricing`; the bar item stays on `DEFAULT` (measured on the demo gateway, 2026-10-19).
    #[test]
    fn each_lane_names_its_adapter_and_item() {
        let epic = "CS.D.EURGBP.MINI.IP";
        assert_eq!(Lane::Quotes.item("ACC1", epic), ("Pricing", format!("PRICE:ACC1:{epic}")));
        let bars = Lane::Candles { interval: "1m".into(), scale: "1MINUTE" };
        assert_eq!(bars.item("ACC1", epic), ("DEFAULT", format!("CHART:{epic}:1MINUTE")));
    }

    /// A refused item is dialed less and less often, never more than `REFUSED_MAX_WAIT` apart: a day of
    /// refusals is a few hundred sessions, not the ~26,600 of the pump's fixed 3 s.
    #[test]
    fn refusal_wait_doubles_to_its_cap_and_bounds_a_days_dials() {
        let wait = |streak| refusal_wait(Kind::Subscribe, streak, false, REFUSED_FIRST_WAIT);
        assert_eq!(wait(0), Duration::ZERO);
        assert_eq!(wait(1), REFUSED_FIRST_WAIT);
        assert_eq!(wait(2), REFUSED_FIRST_WAIT * 2);
        assert_eq!(wait(3), REFUSED_FIRST_WAIT * 4);
        for n in 1..200 {
            assert!(wait(n + 1) >= wait(n), "never shortens");
            assert!(wait(n) <= REFUSED_MAX_WAIT, "never past the cap");
        }
        assert_eq!(wait(u32::MAX), REFUSED_MAX_WAIT);
        // A day of it: every dial costs the pump's fixed 3 s plus the wait.
        let (mut t, mut dials, mut streak) = (Duration::ZERO, 0_u32, 0_u32);
        while t < Duration::from_secs(86_400) {
            (dials, streak) = (dials + 1, streak + 1);
            t += Duration::from_secs(3) + wait(streak);
        }
        assert!(dials < 200, "{dials} dials in a day");
    }

    /// A lane that HAS been live is not stretched past a minute by a blip: a refusal after quotes were
    /// flowing waits at most `LIVE_REFUSED_MAX_WAIT` for its first `LIVE_TRANSIENT_STREAK` refusals in
    /// a row, then backs off like a lane that never was (a standing refusal is not a blip).
    #[test]
    fn a_lane_that_was_live_is_not_stretched_past_a_minute_by_a_blip() {
        let first = REFUSED_FIRST_WAIT;
        let wait = |streak, ever_live| refusal_wait(Kind::Subscribe, streak, ever_live, first);
        for streak in 1..=LIVE_TRANSIENT_STREAK {
            let (live, never) = (wait(streak, true), wait(streak, false));
            assert!(live <= LIVE_REFUSED_MAX_WAIT, "streak {streak}: {live:?}");
            assert!(live <= never, "a live lane never waits longer than a never-live one");
        }
        assert_eq!(wait(LIVE_TRANSIENT_STREAK, true), LIVE_REFUSED_MAX_WAIT);
        // the never-live lane is untouched at the same streak
        assert!(wait(LIVE_TRANSIENT_STREAK, false) > LIVE_REFUSED_MAX_WAIT);
        // a refusal that will not go away stops being transient
        assert!(wait(LIVE_TRANSIENT_STREAK + 1, true) > LIVE_REFUSED_MAX_WAIT);
        assert_eq!(wait(200, true), REFUSED_MAX_WAIT);
    }

    /// A plain TRANSPORT failure (a dial failure, a drop, an `END`, a `LOOP`, an ack or idle timeout) climbs
    /// the same ladder from `first` but never waits more than a minute between dials, however long the
    /// outage and whatever the lane was: a LIVE lane must not go dark for ten minutes after a long network
    /// outage. (The pump's own 3 s rides on top, so a dial is at most 63 s after the last.)
    #[test]
    fn a_long_transport_outage_never_waits_more_than_a_minute_between_dials() {
        for ever_live in [false, true] {
            let r = runs();
            if ever_live {
                r.live();
            }
            let mut waits = Vec::new();
            for _ in 0..40 {
                r.ended("closed: connection reset");
                waits.push(r.wait());
            }
            let first = REFUSED_FIRST_WAIT;
            // free first try, then 6, 12, 24, 48, and a minute from there on
            assert_eq!(
                waits[..7],
                [
                    Duration::ZERO,
                    first,
                    first * 2,
                    first * 4,
                    first * 8,
                    TRANSPORT_MAX_WAIT,
                    TRANSPORT_MAX_WAIT
                ],
                "ever_live {ever_live}"
            );
            assert!(waits.iter().all(|w| *w <= TRANSPORT_MAX_WAIT), "{waits:?}");
            // A day of it: 3 s of the pump plus the wait per dial; a minute's cadence, not ten.
            let (mut t, mut dials, mut streak) = (Duration::ZERO, 0_u32, 0_u32);
            let day = Duration::from_secs(86_400);
            while t < day {
                (dials, streak) = (dials + 1, streak + 1);
                t += Duration::from_secs(3)
                    + refusal_wait(Kind::Dropped, streak.saturating_sub(1), ever_live, first);
            }
            assert!((1_300..1_500).contains(&dials), "{dials} dials in a day");
        }
    }

    /// A REFUSAL still reaches the ten-minute cap: a refused subscription, and a `CONERR` that is a
    /// verdict (Lightstreamer 1, 2, 60, 70, 71 and anything at or below zero), after the free first try.
    #[test]
    fn a_refusal_still_reaches_the_ten_minute_cap() {
        let r = runs();
        for _ in 0..12 {
            r.saw("21", "Invalid group");
            r.ended("subscribe refused");
        }
        assert_eq!(r.wait(), REFUSED_MAX_WAIT);
        for code in ["1", "2", "60", "70", "71", "0", "-1"] {
            let r = runs();
            for _ in 0..12 {
                r.saw_conerr(code, "refused");
                assert_eq!(r.ended("CONERR").kind, Kind::Conerr, "code {code}");
            }
            assert_eq!(r.wait(), REFUSED_MAX_WAIT, "code {code}");
        }
    }

    /// A `CONERR` that is not a verdict on this client (a server session or load limit, new sessions
    /// temporarily blocked, streaming unavailable: 7-11) or whose code nobody has seen is TRANSPORT-class: it
    /// climbs to a minute and no further. The cached login is still dropped by the first CONERR of a run only.
    #[test]
    fn a_conerr_that_is_not_a_refusal_takes_the_transport_ladder() {
        for code in ["7", "8", "9", "10", "11", "99", "not a number"] {
            let r = runs();
            assert!(
                r.relogin_for_conerr(),
                "the first CONERR of a run may drop the login ({code})"
            );
            for _ in 0..12 {
                r.saw_conerr(code, "declined");
                assert_eq!(r.ended("CONERR").kind, Kind::Dropped, "code {code}");
                assert!(!r.relogin_for_conerr(), "later ones keep it ({code})");
            }
            assert_eq!(r.wait(), TRANSPORT_MAX_WAIT, "code {code}");
        }
        assert!(!conerr_is_refusal("7") && conerr_is_refusal("1") && conerr_is_refusal("-1"));
    }

    /// THE RULE, pinned: the ceiling follows the LATEST failure, the doubling position follows the RUN. So
    /// a transport failure after nine refusals waits a minute (it takes the transport ladder because the
    /// last failure is transport), the refusal after it waits ten minutes again (the run's position is
    /// kept, not restarted), and at-cap reporting follows the same kind's cap.
    #[test]
    fn the_ceiling_follows_the_latest_failure_and_the_doubling_follows_the_run() {
        let r = runs();
        for _ in 0..9 {
            r.saw("21", "Invalid group");
            r.ended("subscribe refused");
        }
        assert_eq!((r.wait(), r.at_cap()), (REFUSED_MAX_WAIT, true));
        r.ended("closed: connection reset");
        assert_eq!(
            r.wait(),
            TRANSPORT_MAX_WAIT,
            "the last failure is transport: the transport ceiling"
        );
        assert!(r.at_cap(), "and a dial at THAT cap is reported");
        r.saw("21", "Invalid group");
        r.ended("subscribe refused");
        assert_eq!(
            r.wait(),
            REFUSED_MAX_WAIT,
            "the last failure is a refusal again: the refusal ceiling"
        );
        assert!(r.at_cap());
        // and a transport failure in the middle of the climb does not reset it
        let fresh = runs();
        fresh.saw("21", "Invalid group");
        fresh.ended("subscribe refused"); // 6 s
        fresh.ended("closed: reset"); // run position 2: 12 s, under the transport cap
        assert_eq!(fresh.wait(), REFUSED_FIRST_WAIT * 2);
    }

    /// Reporting at the cap follows the kind's own cap: a transport failure is "at cap" from the fifth
    /// counted failure (a minute), a refusal only at the eighth (ten minutes).
    #[test]
    fn a_transport_failure_is_reported_at_its_own_cap() {
        let r = runs();
        let at_cap: Vec<bool> = (0..8)
            .map(|_| {
                r.ended("closed: connection reset");
                r.at_cap()
            })
            .collect();
        // the first is the free try; 6, 12, 24, 48 are below a minute; a minute from the sixth failure
        assert_eq!(at_cap, [false, false, false, false, false, true, true, true]);
    }

    /// A day of plain transport outage, simulated: every dial is the pump's 3 s plus the owed wait, fails,
    /// and is asked what it says. The first failure speaks at once; after it a lane at its minute writes ONE
    /// `warn` per ten minutes, counting the dials between, instead of one per dial (~1,370 a day).
    #[test]
    fn a_simulated_day_of_transport_outage_writes_about_a_hundred_and_fifty_warns() {
        let r = runs();
        r.live(); // a lane that was live: the case the minute cap exists for
        let (mut t, day) = (0_i64, 86_400_000_i64);
        let mut said: Vec<(i64, Say)> = Vec::new();
        let mut dials = 0_u32;
        while t < day {
            r.ended("closed: connection reset");
            dials += 1;
            let say = r.say(t);
            if say != Say::Quiet {
                said.push((t, say));
            }
            t += 3_000 + r.wait().as_millis() as i64;
        }
        assert!(dials > 1_300, "{dials} dials in a day: the cadence is a minute, not ten");
        assert!((100..=150).contains(&said.len()), "{} warn lines in a day", said.len());
        assert_eq!(said[0], (0, Say::First), "the first failure speaks at once");
        for pair in said.windows(2) {
            assert!(
                pair[1].0 - pair[0].0 >= 600_000,
                "two lines closer than ten minutes: {pair:?}"
            );
        }
        // every later line counts the dials since the one before it, so no dial goes uncounted
        let counted: u32 = said
            .iter()
            .filter_map(|(_, s)| if let Say::Cap { dials } = s { Some(*dials) } else { None })
            .sum();
        assert!(counted + 1 + 15 >= dials, "{counted} counted of {dials}");
        assert!(said.iter().skip(1).all(|(_, s)| matches!(s, Say::Cap { dials } if *dials >= 8)));
    }

    /// A REFUSAL at its ten-minute cap keeps one `warn` per dial (about 144 lines a day already).
    #[test]
    fn refusals_at_their_ten_minute_cap_keep_one_warn_per_dial() {
        let r = runs();
        let (mut t, mut caps, mut first) = (0_i64, Vec::new(), 0);
        for _ in 1..=40 {
            r.saw("21", "Invalid group");
            r.ended("subscribe refused");
            match r.say(t) {
                Say::First => first += 1,
                Say::Cap { dials } => caps.push(dials),
                Say::Quiet => {}
            }
            t += 3_000 + r.wait().as_millis() as i64;
        }
        assert_eq!(first, 1);
        // the cap is reached at the eighth refusal; every dial from there to the fortieth speaks
        assert_eq!(caps.len(), 33, "{caps:?}");
        assert!(caps[1..].iter().all(|d| *d == 1), "one dial per line: {caps:?}");
    }

    /// The first failure of a run speaks at once whatever its kind, and the failures before the cap stay quiet.
    #[test]
    fn the_first_failure_of_a_run_speaks_at_once_whatever_its_kind() {
        for kind in [Kind::Subscribe, Kind::Conerr, Kind::Credentials, Kind::Dropped] {
            let r = runs();
            match kind {
                Kind::Subscribe => r.saw("21", "x"),
                Kind::Conerr => r.saw_conerr("1", "x"),
                Kind::Credentials => r.saw_login("IG error 401: x"),
                Kind::Dropped => {}
            }
            assert_eq!(r.ended("closed: x").kind, kind);
            assert_eq!(r.say(1_000), Say::First, "{kind:?}");
        }
        let r = runs();
        r.ended("closed: x");
        assert_eq!(r.say(0), Say::First);
        r.ended("closed: x");
        assert_eq!(r.say(1), Say::Quiet, "below the cap, after the first, nothing");
    }

    /// A run of failures that ends in a delivering session says so ONCE: how many failures, how long. No run
    /// open, no line, and the clock is not even read; the next run starts afresh.
    #[test]
    fn a_recovery_writes_its_summary_once() {
        let r = runs();
        assert_eq!(r.recovered(|| unreachable!("no run is open")), None);
        r.ended("closed: x");
        assert_eq!(r.say(1_000), Say::First);
        for n in 1..5 {
            r.ended("closed: x");
            r.say(1_000 + n * 60_000);
        }
        let rec = r.recovered(|| 301_000).expect("a run was open");
        assert_eq!((rec.failures, rec.millis), (5, 300_000));
        assert_eq!(r.recovered(|| 400_000), None, "once");
        r.ended("closed: y");
        assert_eq!(r.say(500_000), Say::First, "a new run speaks at once again");
        let again = r.recovered(|| 560_000).expect("the second run");
        assert_eq!((again.failures, again.millis), (1, 60_000));
    }

    /// One report per run of refusals; an accepted subscription ends the run (and the next refusal is
    /// reported again).
    #[test]
    fn only_the_first_refusal_of_a_run_is_reported() {
        let r = Refusals::new(REFUSED_FIRST_WAIT, REFUSED_MAX_WAIT);
        assert_eq!(r.wait(), Duration::ZERO, "nothing failed, nothing to wait");
        r.saw("21", "Invalid group");
        assert!(r.ended("subscribe refused").words.contains("21"));
        assert!(r.first());
        for _ in 0..50 {
            r.saw("21", "Invalid group");
            r.ended("subscribe refused");
            assert!(!r.first(), "silent after the first");
        }
        assert_eq!(r.wait(), REFUSED_MAX_WAIT);
        assert!(r.accepted());
        assert_eq!(r.wait(), Duration::ZERO);
        assert!(!r.accepted(), "nothing to end twice");
        r.saw("21", "Invalid group");
        r.ended("subscribe refused");
        assert!(r.first(), "a new run is reported again");
    }

    /// The refuse -> accept recovery, on the struct the wiring uses: a live lane's blip is short, its
    /// recovery clears the run, and the next refusal starts a fresh one.
    #[test]
    fn a_refusal_then_an_accept_resets_the_run_and_the_wait() {
        let r = Refusals::new(REFUSED_FIRST_WAIT, REFUSED_MAX_WAIT);
        r.live();
        for n in 1..=LIVE_TRANSIENT_STREAK {
            r.saw("21", "Invalid group");
            r.ended("subscribe refused");
            assert!(r.wait() <= LIVE_REFUSED_MAX_WAIT, "refusal {n}: {:?}", r.wait());
        }
        assert!(r.accepted(), "a refused run was ended");
        assert_eq!((r.wait(), r.due()), (Duration::ZERO, Duration::ZERO));
        r.saw("21", "Invalid group");
        r.ended("subscribe refused");
        assert!(
            r.first() && r.wait() == REFUSED_FIRST_WAIT,
            "a fresh run starts from the first wait"
        );
    }

    /// A failed session owes the next dial its wait ONCE: the dial sleeps it (`due` consumes it) and, if it
    /// fails in turn, that failure is counted and owes the NEXT wait; nothing is slept twice for one failure.
    #[test]
    fn a_failure_owes_the_next_dial_its_wait_once() {
        let r = Refusals::new(REFUSED_FIRST_WAIT, REFUSED_MAX_WAIT);
        assert_eq!(r.due(), Duration::ZERO, "nothing refused, nothing owed");
        r.saw("21", "Invalid group");
        r.ended("subscribe refused");
        assert_eq!(r.due(), REFUSED_FIRST_WAIT, "the refusal's wait is owed once");
        assert_eq!(
            r.due(),
            Duration::ZERO,
            "and a dial that failed before a session ran does not owe it again"
        );
        r.saw("21", "Invalid group");
        r.ended("subscribe refused");
        assert_eq!(r.due(), REFUSED_FIRST_WAIT * 2, "the next refusal owes the next wait");
    }

    /// Each dial at the longest wait is reported (the 10-minute cadence is audible in the journal), and the
    /// ones before it are not.
    #[test]
    fn a_lane_at_its_longest_wait_says_so_at_every_dial() {
        let r = Refusals::new(REFUSED_FIRST_WAIT, REFUSED_MAX_WAIT);
        let mut at_cap = Vec::new();
        for _ in 0..12 {
            r.saw("21", "Invalid group");
            r.ended("subscribe refused");
            at_cap.push(r.at_cap());
        }
        // 6, 12, 24, 48, 96, 192, 384 are below the cap; 600 from the eighth refusal on.
        assert_eq!(
            at_cap,
            [false, false, false, false, false, false, false, true, true, true, true, true]
        );
        assert!(r.accepted());
        assert!(!r.at_cap(), "an accepted lane is at no cap");
    }

    fn runs() -> Refusals {
        Refusals::new(REFUSED_FIRST_WAIT, REFUSED_MAX_WAIT)
    }

    /// Every session-level failure the shared pump redials at its fixed 3 s (a `CONERR`, an `END`, a `LOOP`,
    /// an ack or idle timeout, a dial or read failure, a login that never reached IG) is counted into the run:
    /// the FIRST costs nothing extra (the common cause is benign: a token that expired, a socket that
    /// dropped), and each one after it asks the next dial for the stretched wait, from `first` doubling.
    #[test]
    fn a_session_failure_gets_a_free_first_try_and_then_the_stretched_wait() {
        for (name, is_conerr) in [("CONERR", true), ("drop", false)] {
            let r = runs();
            let waits: Vec<Duration> = (0..4)
                .map(|_| {
                    if is_conerr {
                        r.saw_conerr("1", "Wrong credentials");
                        r.ended("CONERR,1,Wrong credentials");
                    } else {
                        r.ended("closed: connection reset");
                    }
                    r.wait()
                })
                .collect();
            let first = REFUSED_FIRST_WAIT;
            assert_eq!(waits, [Duration::ZERO, first, first * 2, first * 4], "{name}");
        }
    }

    /// The kinds are told apart, counted into ONE streak, and worded: a verdict in IG's words, a drop in the
    /// pump's.
    #[test]
    fn every_kind_of_session_failure_is_counted_and_worded() {
        let r = runs();
        r.saw("21", "Invalid group");
        assert_eq!(r.ended("subscribe refused").kind, Kind::Subscribe);
        r.saw_conerr("1", "Wrong credentials");
        let conerr = r.ended("CONERR,1,Wrong credentials");
        assert_eq!(conerr.kind, Kind::Conerr);
        assert!(conerr.words.contains("CONERR") && conerr.words.contains("Wrong credentials"));
        r.saw_login("IG error 401: error.security.invalid-details");
        let login = r.ended("login failed: IG error 401: error.security.invalid-details");
        assert_eq!(login.kind, Kind::Credentials);
        assert!(login.words.contains("error.security.invalid-details"));
        let dropped = r.ended("closed during handshake: reset");
        assert_eq!(
            (dropped.kind, dropped.words.as_str()),
            (Kind::Dropped, "closed during handshake: reset")
        );
        assert_eq!(r.streak.get(), 4, "one streak, not one per kind");
    }

    /// The cached login is dropped by the FIRST `CONERR` of a run only: a second after a fresh login is not
    /// expiry, and re-logging-in on every attempt is a `POST /session` every few seconds. A recovery gives
    /// the budget back.
    #[test]
    fn a_cached_login_is_dropped_by_the_first_conerr_of_a_run_only() {
        let r = runs();
        assert!(r.relogin_for_conerr(), "the first CONERR of a run may drop the login");
        assert!(!r.relogin_for_conerr() && !r.relogin_for_conerr(), "later ones keep it");
        assert!(!r.accepted(), "no failure was counted, so nothing to end");
        r.saw_conerr("1", "x");
        r.ended("CONERR,1,x");
        assert!(r.accepted());
        assert!(r.relogin_for_conerr(), "a recovered lane has its one login back");
    }

    /// IG answered the LOGIN with a credentials error: the same credentials get the same answer, and a
    /// documented lockout (`too-many-failed-attempts`) waits at the end of repeated tries. So the wait is the
    /// longest at once, whatever the lane was before, and every dial at it is reported.
    #[test]
    fn a_credentials_error_waits_the_longest_wait_at_once() {
        let r = runs();
        r.live(); // a lane that was live: still no 60 s blip cap for a verdict
        r.saw_login("IG error 401: error.security.invalid-details");
        assert_eq!(r.ended("login failed").kind, Kind::Credentials);
        assert!(r.first());
        assert_eq!(r.wait(), REFUSED_MAX_WAIT);
        assert!(r.at_cap(), "every dial at the longest wait is reported");
        assert_eq!(r.due(), REFUSED_MAX_WAIT, "and it is owed by the next dial");
        assert_eq!(r.due(), Duration::ZERO);
        // a test shortens it, and only it
        let short = Refusals::new(REFUSED_FIRST_WAIT, Duration::from_secs(4));
        short.saw_login("IG error 401: x");
        short.ended("login failed");
        assert_eq!(short.wait(), Duration::from_secs(4));
    }

    /// CONERR, then success: the run ends, the wait is gone, the next failure is a free first try again, and
    /// the login budget is back.
    #[test]
    fn a_conerr_then_an_accept_resets_the_run() {
        let r = runs();
        for _ in 0..3 {
            r.saw_conerr("1", "x");
            r.ended("CONERR,1,x");
        }
        assert!(r.wait() > Duration::ZERO && r.at_cap() == (r.wait() == REFUSED_MAX_WAIT));
        assert!(r.accepted(), "a session that delivered ends the run");
        assert_eq!((r.wait(), r.due()), (Duration::ZERO, Duration::ZERO));
        r.ended("closed: reset");
        assert!(
            r.first() && r.wait() == Duration::ZERO,
            "the next failure is a free first try again"
        );
    }

    /// The status line of a refused lane carries the word the live smoke looks for, and the silent lane's
    /// carries its own: neither has "error" or "failed" in it, which is what the smoke used to read.
    #[test]
    fn the_statuses_carry_the_words_the_smoke_reads() {
        let refused = refused_status("REQERR 21 Invalid group: x", Duration::from_secs(6));
        assert!(refused.contains(REFUSED_STATUS), "{refused}");
        assert!(!refused.contains("error") && !refused.contains("failed"), "{refused}");
        assert_ne!(REFUSED_STATUS, SILENT_STATUS);
    }

    /// A SUBOK with no update behind it is silent once `after` has passed, told once per session; an update
    /// clears it; the next session's SUBOK starts the watch afresh.
    #[test]
    fn a_subscription_with_no_update_behind_its_ack_is_silent_once() {
        let s = Silence::default();
        assert!(!s.overdue(Duration::ZERO), "no SUBOK, nothing to wait for");
        s.acked();
        assert!(!s.overdue(Duration::from_secs(3600)), "not yet");
        assert!(s.overdue(Duration::ZERO), "past the bound");
        assert!(!s.overdue(Duration::ZERO), "told once, not per tick");
        s.acked();
        s.heard();
        assert!(!s.overdue(Duration::ZERO), "an update ends the watch");
        s.acked();
        assert!(s.overdue(Duration::ZERO), "a new session is watched afresh");
    }

    #[test]
    fn refusal_codes_are_worded() {
        assert!(refusal_meaning("21").contains("item"));
        assert!(refusal_meaning("23").contains("field"));
        assert!(refusal_meaning("17").contains("adapter"));
        assert!(refusal_meaning("-1").contains("venue's own"));
        assert!(refusal_meaning("0").contains("venue's own"));
        assert!(refusal_meaning("not a number").contains("refused"));
    }

    #[test]
    fn ws_host_port_parses_defaults_and_explicit_ports() {
        assert_eq!(
            ws_host_port("wss://demo-apd.marketdatasystems.com/lightstreamer").unwrap(),
            ("demo-apd.marketdatasystems.com".to_string(), 443)
        );
        assert_eq!(ws_host_port("wss://h:8443/x").unwrap(), ("h".to_string(), 8443));
        assert_eq!(ws_host_port("ws://h/x").unwrap(), ("h".to_string(), 80));
        assert!(ws_host_port("no-scheme").is_err());
    }
}
