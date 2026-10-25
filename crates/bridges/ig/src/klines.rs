//! **IG's price history as a store-free source** — [`IgKlines`], this bridge's
//! `vike_data::source::KlineSource`: the FETCH half of a backfill and nothing else. It returns rows; the
//! commit key, the settle rule and the append are `vike-backfill`'s, the credential read and the
//! allowance ledger are the gatewayhub's, and which process dispatches to it, and with what, is decision
//! 0194's.
//!
//! # The door, and what the 2026-10-23 probe measured about it
//!
//! One request: `GET /prices/{epic}` version 3 with `resolution`, `from`, `to` and `pageSize=0`. Every fact
//! below is `crates/bridges/ig/tests/ig_history_probe.rs`'s reading of the demo gateway (150 points spent):
//!
//! * **A row costs one point of a weekly allowance.** `GET /operations/application` says
//!   `allowanceAccountHistoricalData` 10000, every `/prices` answer carries `metadata.allowance`
//!   (`remainingAllowance`, `totalAllowance`, `allowanceExpiry` in seconds), and `remainingAllowance` is read
//!   AFTER the call's own cost. The window is a fixed week: the expiry counted down with the wall clock and
//!   nothing refilled after a minute.
//! * **`pageSize=0` returns every row in one answer** (`totalPages` 1). This source refuses an answer that
//!   says otherwise, because a chunk stored from a first page would be short for good.
//! * **Prices are in PRICE UNITS** for the three classes read (EURUSD, the DAX, WTI crude): the mid of a
//!   row's bid and ask is the level the market snapshot gives. Bid and ask OHLC arrive side by side
//!   (`lastTraded` is null for FX); the store holds ONE price per bar, so the bar is the MID
//!   `(bid + ask) / 2` of each of the four, the side OANDA's and Dukascopy's lines store.
//! * **`lastTradedVolume` is present** and is a tick count, not a traded quantity. It is stored as the
//!   bar's volume, as OANDA's tick count is.
//! * **`snapshotTimeUTC` is the bar's OPEN stamp** and `snapshotTime` is the same instant on the ACCOUNT's
//!   local wall clock (+480 minutes on the demo account in British Summer Time, +420 outside it). The
//!   newest row is the bar still FORMING, which this source drops: closed bars only.
//! * **`from` and `to` are read on that local clock, not in UTC.** Asked for 06:00 to 11:00 the gateway
//!   answered the bars stamped 22:00Z to 03:00Z. The clock is NOT fixed: it is London time plus seven hours,
//!   a rule of the instant (`crate::account_clock`), measured by the 2026-10-24 acceptance run to move with
//!   the UK clock change. So every window is asked in the offset valid at it, a window that holds a change
//!   is cut at it and asked in two pieces, and every answered row (the first read's too) is checked against
//!   the rule: a row that disagrees with it, an offset that is not a whole number of minutes or no time
//!   zone fails the request loudly instead of storing a window that is shifted.
//! * **A daily bar is stamped 16:00Z of the previous UTC day, in summer time and in winter alike**
//!   (measured on the demo account on 2026-10-24: every daily stamp on both sides of the 2025-10-26 clock
//!   change is 16:00:00Z, which is local midnight in summer and 23:00 on the account's own clock in
//!   winter). It is stored under IG's own stamp, exactly as OANDA's and FXCM's daily bars are stored at
//!   17:00 New York and not at midnight: a reader of a daily series must not assume a UTC-midnight stamp,
//!   and a chunk holds a bar by its stamp, so the day grid copes without a shift.
//!
//! # The gate around every request
//!
//! [`AllowanceGate`] is the ledger's door (the ledger itself is the gatewayhub's, one per process): it
//! paces the requests, refuses a chunk that would leave less than [`RESERVE_PERCENT`] percent of the
//! allowance, learns the allowance from every answer and turns a `403` allowance refusal into the plain
//! sentence the operator reads. This source also caps one REQUEST (one source is built per request) at
//! [`REQUEST_SHARE_PERCENT`] percent of the week, so a mistyped window cannot spend it.
//!
//! # What it will not do
//!
//! * **It reads only.** IG offers no read-only key, so a demo session CAN trade; this file is held to
//!   reading by [`PricesApi`] (which has one method, `get`) and by
//!   `crates/vike-ops/tests/venues/collector_dispatch_gate.rs`'s `the_ig_history_lane_only_reads`. The
//!   one other call is the lane's own `DELETE /session` at idle or shutdown, which ends the session the
//!   lane opened itself and lives in `IgSession::logout`, outside this file.
//! * **It never repeats a request.** A timeout or a reset may have been charged; the failure names the
//!   chunk and the next run of the same command resumes there, because the ingest skips what is stored.
//! * **It never corrects a scale.** The first read of an epic is compared with the market snapshot
//!   (`level x scalingFactor x pip`, within [`RATE_TOLERANCE`]); a disagreement is a refusal. The check
//!   fails CLOSED: only for the three classes the probe read in price units (`CURRENCIES`, `INDICES`,
//!   `COMMODITIES`) may a shut market or an unreadable snapshot skip it, with a warning; any other
//!   class is stored only when the snapshot confirms the scale.
//! * **It never stores a shifted or an unproven chunk.** A bar more than one step outside the window
//!   asked for fails the chunk, and so does an empty answer for a window wider than a day that holds
//!   weekdays (an empty chunk is final once a later one holds data).
//! * **It shares one session.** The process's [`SessionCache`] holds the single logged-in session every
//!   request uses, ends it after twenty idle minutes and at a clean shutdown.
//! * **It does not bound the number of chunks.** The ingest asks one chunk at a time.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;
use vike_data::source::{KlineSource, SourceError};
use vike_marketdata::Bar;
use vike_marketdata::interval::interval_ms;

use crate::account_clock::{self, Part};
use crate::config::IgConfig;
use crate::data::{parse_ig_time_utc, resolution};
use crate::quote_rate::RATE_TOLERANCE;
use crate::rest::{IgApiError, IgSession};

/// The share of the weekly allowance that is never spent by a history fetch: a chunk that would leave
/// less than this many percent of `totalAllowance` is refused before it is asked. It protects the other
/// readers of the same pool (the reconcile lane's `GET /history/activity` is documented as subject to the
/// same error), so a drained pool cannot blind a running account's fills read.
pub const RESERVE_PERCENT: u32 = 20;

/// The most of the weekly allowance ONE request (one `data hist fetch` command, or one calendar-year
/// piece of one) may spend, in percent of `totalAllowance`. It is a guard against a wrong window, not a
/// budget: the reserve is what protects the account, and a second command has its own count.
pub const REQUEST_SHARE_PERCENT: u32 = 50;

/// The least time between two requests the gateway sees from this daemon on one account: six seconds,
/// ten a minute. IG's non-trading ceiling on an account is thirty a minute
/// (`allowanceAccountOverall`, read by the probe) and the same account is also used by the exec,
/// reconcile and market-feed sessions, so a third of it is the history lane's share. A lower pace is a
/// one-constant change; the application-wide ceiling (sixty a minute) is not approached.
pub const PACE: Duration = Duration::from_secs(6);

/// What the first read of an epic costs: one daily bar, one point.
const FIRST_READ_POINTS: u32 = 1;

/// The widest real UTC offset in minutes (UTC+14:00); anything beyond is a misread, not a zone.
const MAX_OFFSET_MINUTES: i64 = 14 * 60;

/// One reading of IG's historical-data allowance, as a `/prices` answer states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    /// Points left once the answer's own cost is taken (`remainingAllowance`).
    pub remaining: u32,
    /// The week's size (`totalAllowance`): 10000 on the demo key.
    pub total: u32,
    /// Seconds until the window resets (`allowanceExpiry`).
    pub resets_in_s: u64,
}

/// `metadata.allowance` out of a `/prices` body, or `None` when the body carries none (an error body
/// usually does not).
pub fn parse_allowance(body: &Value) -> Option<Allowance> {
    let allowance = body.pointer("/metadata/allowance")?;
    let number = |key: &str| allowance.get(key).and_then(Value::as_u64);
    Some(Allowance {
        remaining: u32::try_from(number("remainingAllowance")?).ok()?,
        total: u32::try_from(number("totalAllowance")?).ok()?,
        resets_in_s: number("allowanceExpiry")?,
    })
}

/// The points kept in reserve out of a week of `total`: [`RESERVE_PERCENT`] percent, rounded UP.
pub fn reserve_points(total: u32) -> u32 {
    (u64::from(total) * u64::from(RESERVE_PERCENT)).div_ceil(100) as u32
}

/// The most points one request may spend out of a week of `total`: [`REQUEST_SHARE_PERCENT`] percent,
/// rounded down.
pub fn request_cap_points(total: u32) -> u32 {
    (u64::from(total) * u64::from(REQUEST_SHARE_PERCENT) / 100) as u32
}

/// The IG resolution and the bar width of `interval`, or `None` for one this source does not serve: the
/// twelve minute, hour and day widths. `1w` and `1mo` have a resolution but no fixed step
/// (`vike_marketdata::interval::measures_bar_step` refuses them ahead of any source, and so does this
/// one); `1s` has neither a label in [`resolution`] nor a place in the history (the live feed's).
pub fn served_interval(interval: &str) -> Option<(&'static str, i64)> {
    let step = interval_ms(interval).filter(|step| *step > 0)?;
    Some((resolution(interval)?, step))
}

/// **The most rows a window can return, which is its cost in points**: the calendar bound, one bar per
/// step from the window's start to its end. It is deliberately above what an FX week really holds (the
/// market is shut from Friday evening to Sunday evening), so it only ever makes a refusal slightly
/// early; and if closed hours are in fact filled with flat bars, it is exact. `None` for an interval not
/// [`served_interval`] or an inverted window. Saturates at `u32::MAX`.
pub fn estimate_points(interval: &str, start_ms: i64, end_ms: i64) -> Option<u32> {
    let (_, step) = served_interval(interval)?;
    if end_ms < start_ms {
        return None;
    }
    let rows = (i128::from(end_ms) - i128::from(start_ms)) / i128::from(step) + 1;
    Some(u32::try_from(rows).unwrap_or(u32::MAX))
}

// ── the words ──────────────────────────────────────────────────────────────────────────────────

fn plural(n: u64, unit: &str) -> String {
    if n == 1 { format!("1 {unit}") } else { format!("{n} {unit}s") }
}

/// A span in the words an operator reads: `6 days 23 hours`, `5 hours 12 minutes`, `42 minutes`.
pub fn span_words(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / 86_400, seconds % 86_400 / 3_600, seconds % 3_600 / 60);
    if days > 0 {
        format!("{} {}", plural(days, "day"), plural(hours, "hour"))
    } else if hours > 0 {
        format!("{} {}", plural(hours, "hour"), plural(minutes, "minute"))
    } else if minutes > 0 {
        plural(minutes, "minute")
    } else {
        "under a minute".to_string()
    }
}

/// `ms` as UTC to the minute, `2026-10-30T17:22Z`.
fn utc_minute(ms: i64) -> String {
    let full = vike_model::time::utc_rfc3339(ms.div_euclid(1000));
    match full.get(..16) {
        Some(head) if full.len() == 20 => format!("{head}Z"),
        _ => full,
    }
}

/// `ms` as a UTC date, `2026-10-30`.
fn utc_date(ms: i64) -> String {
    let full = vike_model::time::utc_rfc3339(ms.div_euclid(1000));
    match full.get(..10) {
        Some(head) if full.len() == 20 => head.to_string(),
        _ => full,
    }
}

/// When the allowance resets, as a sentence: from the last reading (`seen_ms` is when it was taken,
/// `resets_in_s` what it said), or the plain admission that IG has not said.
pub fn reset_sentence(last: Option<(i64, u64)>) -> String {
    match last {
        Some((seen_ms, resets_in_s)) => {
            let ms = i64::try_from(resets_in_s.saturating_mul(1000)).unwrap_or(i64::MAX);
            format!(
                "The allowance resets in {} (about {}).",
                span_words(resets_in_s),
                utc_minute(seen_ms.saturating_add(ms))
            )
        }
        None => {
            "IG has not said when the allowance resets (its window is a week long).".to_string()
        }
    }
}

/// The chunk a refusal is about, in words: `1h bars from 2026-09-01 to 2026-09-21`.
pub fn chunk_words(interval: &str, start_ms: i64, end_ms: i64) -> String {
    format!("{interval} bars from {} to {}", utc_date(start_ms), utc_date(end_ms))
}

/// **The sentence for a chunk the reserve rule stops** — nothing was asked of IG for it.
pub fn reserve_stop_words(
    chunk: &str,
    est_points: u32,
    allowance: &Allowance,
    last: Option<(i64, u64)>,
) -> String {
    format!(
        "ig: {chunk} were NOT requested. They could use up to {est_points} points of IG's historical-data \
         allowance (one point a row); {} of this week's {} points are left, and {} ({RESERVE_PERCENT} percent) \
         are kept in reserve for the account's other reads, so the fetch stops here. {} Run the same \
         command again after the reset to carry on.",
        allowance.remaining,
        allowance.total,
        reserve_points(allowance.total),
        reset_sentence(last),
    )
}

/// **The sentence for a request that has used its share of the week** — nothing was asked of IG for the
/// chunk it stops at.
pub fn request_share_words(
    chunk: &str,
    est_points: u32,
    spent: u32,
    total: u32,
    remaining: Option<u32>,
) -> String {
    let left = remaining.map_or_else(String::new, |r| format!(" {r} points are left this week."));
    format!(
        "ig: {chunk} were NOT requested. This request has already used {spent} points and they could \
         use up to {est_points} more; one fetch is capped at {REQUEST_SHARE_PERCENT} percent of the \
         week's allowance ({} of {total} points) so that a wrong window cannot spend the week.{left} \
         Run the same command again to carry on from here: what is stored is skipped, and the new \
         request starts its own count.",
        request_cap_points(total),
    )
}

/// **The sentence for IG's own refusal of an allowance** (`403`, `error.public-api.exceeded-…-allowance`):
/// which allowance, that nothing is retried, and when it resets.
pub fn exhausted_words(code: &str, last: Option<(i64, u64)>) -> String {
    let (which, refills) = if code.contains("historical-data") {
        ("the week's historical-data allowance is spent", false)
    } else if code.contains("api-key") {
        ("the API key's request allowance is spent (a per-minute ceiling)", true)
    } else if code.contains("account") {
        ("the account's request allowance is spent (a per-minute ceiling)", true)
    } else {
        ("an IG allowance is spent", false)
    };
    let when = if refills {
        format!(
            "That ceiling refills within a minute; repeat the command after a minute. {}",
            reset_sentence(last)
        )
    } else {
        reset_sentence(last)
    };
    format!(
        "ig: IG refused the request with {code}: {which}. Nothing is retried. {when} The chunks before \
         this one are stored, and the same command resumes here."
    )
}

// ── the gate ───────────────────────────────────────────────────────────────────────────────────

/// **The ledger's door** — what an [`IgKlines`] asks around every request to IG. The implementation is
/// the gatewayhub's (`AllowanceLedger`, one per process, shared by every IG history fetch), so the pace
/// and the remaining allowance are a property of the key and not of one request.
pub trait AllowanceGate: Send + Sync {
    /// The account this source logged in as (IG's public account id, never a credential): the ledger
    /// forgets what it knew of any other account.
    fn bind(&self, account_id: &str);

    /// **Before a request that could cost `points`** (`what` says in words what it is for, `1h bars
    /// from 2026-09-01 to 2026-09-21`, and goes into a refusal): wait for the pace (asking `stop` while
    /// it waits), and refuse — `SourceError::Fetch`, with the sentence an operator reads — a request
    /// that would leave less than the reserve, or one the ledger already knows IG will refuse. `Ok`
    /// means send it.
    fn admit(&self, points: u32, what: &str, stop: &dyn Fn() -> bool) -> Result<(), SourceError>;

    /// An answer's allowance, to replace what the ledger believed.
    fn learn(&self, allowance: Allowance);

    /// IG refused with an allowance error code: record it so no request follows into the same wall, and
    /// return the sentence for the operator ([`exhausted_words`]).
    fn exhausted(&self, code: &str) -> String;

    /// **Before the login**: refuse at once, without waiting for the pace, what the ledger already
    /// knows — a wall IG answered with, a login it refused a moment ago, a chunk of `points` that would
    /// leave less than the reserve — so that a known refusal costs no login and takes no turn.
    fn precheck(&self, points: u32, what: &str) -> Result<(), SourceError>;

    /// A login IG refused (`why` is the sentence the operator reads): remember it, so a fresh login is
    /// not tried again at once (IG locks an account after repeated failed logins, and the trading
    /// side's own re-login shares it).
    fn login_failed(&self, why: &str);
}

/// A logged-in session the history lane may also END: the one thing beyond [`PricesApi`]'s `get`, and
/// only for a session this lane itself opened.
pub trait HistorySession: PricesApi {
    /// End the session (best effort: every failure is ignored).
    fn log_out(&self);
}

/// How a [`SessionCache`] logs in: a fresh session, ending-capable.
pub type SessionLoginFn =
    Arc<dyn Fn() -> Result<Arc<dyn HistorySession>, SourceError> + Send + Sync>;

/// How long a session may sit unused before it is ended and replaced.
pub const SESSION_IDLE: Duration = Duration::from_secs(20 * 60);

/// The status a lease answers with once the cache has ended its session: not 0 (`never sent`), not a
/// 401 (a refused login) and not a 403 (an allowance), so no caller acts on it but to report it.
const SESSION_ENDED_STATUS: u16 = 410;

/// The session a [`SessionCache`] holds, and when it was last used (epoch-ms, shared with the leases
/// handed out so that a long request keeps it fresh), and whether the cache has ended it (shared with
/// the leases, so a source that slept through the end does not use a logged-out session).
struct Held {
    session: Arc<dyn HistorySession>,
    last_used_ms: Arc<AtomicI64>,
    retired: Arc<AtomicBool>,
}

/// **The one logged-in session of the process.** Every `data hist fetch ig` request used to log in
/// again and never log out (a fifteen-year daily fetch is fifteen calendar-year requests), and IG's FAQ
/// warns that many concurrent connections can get a key suspended. The cache holds ONE session for all
/// of them:
///
/// * a request asks for it (through [`SessionCache::through`]) and gets the held session if it was
///   used within `idle` — every `get` through it counts as a use, so a request that is still asking is
///   never "idle" to the next one — else a fresh login;
/// * a session idle for MORE than `idle` is logged out (the cache's own session, `DELETE /session` of
///   its own tokens, best effort) and replaced;
/// * a `401` that reaches a lease is one the session's own re-login did not cure (a rotated password):
///   the session is stale AT ONCE, and the next request replaces it;
/// * a lease on a session the cache has ended answers a plain `410` and never calls it again;
/// * a clean shutdown (the cache dropped by its last owner) logs the held session out;
/// * a login that fails caches nothing (the gatewayhub's ledger holds a lockout off).
///
/// The session re-logs in on a `401` by itself (`IgSession`'s token ladder), so token expiry needs no
/// rule here. The credential is read when a LOGIN happens: a field stored or changed while a session is
/// held takes effect after `idle`.
pub struct SessionCache {
    idle: Duration,
    now_ms: fn() -> i64,
    held: Mutex<Option<Held>>,
}

/// The lease a request holds: the shared session, whose every `get` refreshes its last-use stamp.
struct Lease {
    session: Arc<dyn HistorySession>,
    last_used_ms: Arc<AtomicI64>,
    retired: Arc<AtomicBool>,
    now_ms: fn() -> i64,
}

impl PricesApi for Lease {
    fn account_id(&self) -> String {
        self.session.account_id()
    }
    fn get(&self, path: &str, version: &str, query: &str) -> Result<Value, IgApiError> {
        // A session the cache has ended is never used again: its own ladder would log in anew, outside
        // the cache, and leave a session nobody ends.
        if self.retired.load(Ordering::SeqCst) {
            return Err(IgApiError {
                status: SESSION_ENDED_STATUS,
                message: "the shared IG session was ended after it sat idle or was refused, and \
                          replaced; run the command again"
                    .to_string(),
            });
        }
        self.last_used_ms.store((self.now_ms)(), Ordering::SeqCst);
        let answer = self.session.get(path, version, query);
        // A 401 that reaches here is one the session's own ladder (one re-login) could not cure, a
        // rotated password for one: the session must not look fresh for the idle bound while every
        // retry sends another failed login. The next request replaces it.
        if matches!(&answer, Err(e) if e.status == 401) {
            self.last_used_ms.store(i64::MIN, Ordering::SeqCst);
        }
        answer
    }
}

impl SessionCache {
    /// The production cache: [`SESSION_IDLE`] on the wall clock.
    pub fn new() -> Arc<Self> {
        Self::with(SESSION_IDLE, vike_model::now_ms)
    }

    /// A cache with `idle` reading "now" from `now_ms` (a suite's clock).
    pub fn with(idle: Duration, now_ms: fn() -> i64) -> Arc<Self> {
        Arc::new(Self { idle, now_ms, held: Mutex::new(None) })
    }

    /// `login`, held: the [`LoginFn`] an [`IgKlines`] logs in through.
    pub fn through(self: &Arc<Self>, login: SessionLoginFn) -> LoginFn {
        let cache = Arc::clone(self);
        Arc::new(move || cache.session(&login))
    }

    fn lease(&self, held: &Held) -> Arc<dyn PricesApi> {
        Arc::new(Lease {
            session: Arc::clone(&held.session),
            last_used_ms: Arc::clone(&held.last_used_ms),
            retired: Arc::clone(&held.retired),
            now_ms: self.now_ms,
        })
    }

    /// The held session if it is fresh; else the stale one logged out and a new login. The lock is held
    /// across the login on purpose: concurrent requests make ONE login between them.
    fn session(&self, login: &SessionLoginFn) -> Result<Arc<dyn PricesApi>, SourceError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let now = (self.now_ms)();
        let idle_ms = i64::try_from(self.idle.as_millis()).unwrap_or(i64::MAX);
        if let Some(h) = held.as_ref() {
            if now.saturating_sub(h.last_used_ms.load(Ordering::SeqCst)) <= idle_ms {
                h.last_used_ms.store(now, Ordering::SeqCst);
                tracing::debug!(target: "vike_ig::klines", "ig history session: reused");
                return Ok(self.lease(h));
            }
            if let Some(stale) = held.take() {
                let refused = stale.last_used_ms.load(Ordering::SeqCst) == i64::MIN;
                tracing::info!(
                    target: "vike_ig::klines",
                    account = %stale.session.account_id(),
                    "ig history session: logged out ({})",
                    if refused {
                        "refused by IG".to_string()
                    } else {
                        format!("idle for more than {} minutes", self.idle.as_secs() / 60)
                    }
                );
                // Leases still held by a source that slept through this must not use the session.
                stale.retired.store(true, Ordering::SeqCst);
                stale.session.log_out();
            }
        }
        let session = login()?;
        tracing::info!(
            target: "vike_ig::klines",
            account = %session.account_id(),
            "ig history session: logged in"
        );
        let fresh = Held {
            session,
            last_used_ms: Arc::new(AtomicI64::new(now)),
            retired: Arc::new(AtomicBool::new(false)),
        };
        let lease = self.lease(&fresh);
        *held = Some(fresh);
        Ok(lease)
    }
}

impl Drop for SessionCache {
    /// A clean shutdown ends the session this cache opened. A kill leaves it to IG's own expiry.
    fn drop(&mut self) {
        let held = self.held.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(h) = held.take() {
            tracing::info!(
                target: "vike_ig::klines",
                account = %h.session.account_id(),
                "ig history session: logged out (shutdown)"
            );
            h.session.log_out();
        }
    }
}

/// What an [`IgKlines`] needs of a logged-in session: one method, `get`. A write verb is not in the
/// trait, so a history source cannot call one.
pub trait PricesApi: Send + Sync {
    /// The session's account id (public).
    fn account_id(&self) -> String;
    /// An authenticated `GET` (`path`, the IG `Version` header, a pre-formatted query).
    fn get(&self, path: &str, version: &str, query: &str) -> Result<Value, IgApiError>;
}

impl PricesApi for IgSession {
    fn account_id(&self) -> String {
        self.account_id.clone()
    }
    fn get(&self, path: &str, version: &str, query: &str) -> Result<Value, IgApiError> {
        IgSession::get(self, path, version, query)
    }
}

impl HistorySession for IgSession {
    fn log_out(&self) {
        self.logout();
    }
}

/// How an [`IgKlines`] gets its session: log in once (reading the credential however its owner does)
/// and keep it for the source's life.
pub type LoginFn = Arc<dyn Fn() -> Result<Arc<dyn PricesApi>, SourceError> + Send + Sync>;

/// How an [`IgKlines`] gets the demo account's three fields, as an [`IgConfig`] (which prints none of
/// them). `Refused` for a field that is absent or a store that cannot be read: IG was not asked.
pub type ConfigProvider = Arc<dyn Fn() -> Result<IgConfig, SourceError> + Send + Sync>;

// ── the decoding ───────────────────────────────────────────────────────────────────────────────

/// A `/prices` body, decoded and checked, forming bar still in it.
#[derive(Debug)]
struct Rows {
    bars: Vec<Bar>,
    /// The answer's `instrumentType` (`CURRENCIES`, `INDICES`, …), when it states one.
    instrument_type: Option<String>,
}

/// The instrument classes whose history the 2026-10-23 probe read in PRICE UNITS (EURUSD, the DAX, WTI
/// crude): for these a shut or unreadable market snapshot only warns. For any other class the snapshot
/// must witness the scale, or the history is refused.
const MEASURED_CLASSES: [&str; 3] = ["CURRENCIES", "INDICES", "COMMODITIES"];

/// IG's own allowance refusal: a `403` whose code says an allowance was exceeded.
fn is_allowance_refusal(e: &IgApiError) -> bool {
    e.status == 403 && e.message.contains("exceeded") && e.message.contains("allowance")
}

/// How many UTC weekdays (Monday to Friday) the window `[start_ms, end_ms]` touches.
fn weekdays_in(start_ms: i64, end_ms: i64) -> u64 {
    let day = vike_model::MS_PER_DAY;
    let (first, last) = (start_ms.div_euclid(day), end_ms.div_euclid(day));
    if last < first {
        return 0;
    }
    let days = u64::try_from(last - first + 1).unwrap_or(u64::MAX);
    // 1970-01-01 was a Thursday: day number + 3, modulo 7, counts Monday as 0.
    let weekday = |n: i64| (n + 3).rem_euclid(7) < 5;
    let tail = (0..i64::try_from(days % 7).unwrap_or(0)).filter(|k| weekday(first + k)).count();
    days / 7 * 5 + tail as u64
}

fn json_number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// Decode a `/prices` body. Every defect is an `Err` naming the row, because a row skipped silently
/// would be a hole in a chunk whose commit key then cannot be repaired. A row whose `snapshotTime` is not
/// the account's clock at its `snapshotTimeUTC` (`crate::account_clock`) is a defect too: the window was
/// asked in the wrong clock.
fn read_rows(body: &Value) -> Result<Rows, String> {
    let prices = body
        .get("prices")
        .and_then(Value::as_array)
        .ok_or_else(|| "the answer carries no `prices` array".to_string())?;
    if let Some(pages) = body.pointer("/metadata/pageData/totalPages").and_then(Value::as_u64)
        && pages > 1
    {
        return Err(format!(
            "the answer is paged ({pages} pages) although pageSize 0 was asked: only the first \
             page is here, and a chunk stored from it would be short for good"
        ));
    }
    if let Some(size) = body.pointer("/metadata/size").and_then(Value::as_u64)
        && size != prices.len() as u64
    {
        return Err(format!(
            "the answer says {size} rows and carries {}: a truncated body",
            prices.len()
        ));
    }
    let mut bars: Vec<Bar> = Vec::with_capacity(prices.len());
    for (n, row) in prices.iter().enumerate() {
        let at = n + 1;
        let stamp = |key: &str| -> Result<(i64, String), String> {
            let text = row
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("row {at}: no `{key}`"))?;
            match parse_ig_time_utc(text) {
                0 => Err(format!("row {at}: `{key}` {text:?} is not a stamp")),
                ms => Ok((ms, text.to_string())),
            }
        };
        let (utc_ms, utc_text) = stamp("snapshotTimeUTC")?;
        let (local_ms, _) = stamp("snapshotTime")?;
        let difference = local_ms - utc_ms;
        if difference % 60_000 != 0 {
            return Err(format!(
                "row {at} ({utc_text}): `snapshotTime` is {difference} ms from `snapshotTimeUTC`, \
                 not a whole number of minutes, so the account's clock offset is not understood"
            ));
        }
        let minutes = difference / 60_000;
        if minutes.abs() > MAX_OFFSET_MINUTES {
            return Err(format!(
                "row {at} ({utc_text}): `snapshotTime` is {minutes} minutes from `snapshotTimeUTC`, \
                 which no time zone is"
            ));
        }
        let minutes = minutes as i32;
        match account_clock::offset_minutes_at(utc_ms) {
            Some(rule) if rule == minutes => {}
            Some(rule) => {
                return Err(format!(
                    "row {at} ({utc_text}): the account's clock is {minutes} minutes from UTC here, \
                     but London time plus 7 hours is {rule} (480 in British Summer Time, 420 outside \
                     it) and this lane asks every window by that rule, so the window was asked in the \
                     wrong clock and is shifted; the lane cannot fetch until the rule is corrected"
                ));
            }
            None => {
                return Err(format!(
                    "row {at} ({utc_text}): this stamp is outside the years (1996 to 9999) the \
                     account's clock rule is known for"
                ));
            }
        }
        if bars.last().is_some_and(|last| utc_ms <= last.ts) {
            return Err(format!(
                "row {at} ({utc_text}): the stamps are not strictly increasing, so the answer is \
                 not a series this source can store"
            ));
        }
        let mid = |field: &str| -> Result<f64, String> {
            let side = |name: &str| {
                row.get(field)
                    .and_then(|o| o.get(name))
                    .and_then(json_number)
                    .filter(|x| x.is_finite())
            };
            match (side("bid"), side("ask")) {
                (Some(bid), Some(ask)) => Ok((bid + ask) / 2.0),
                _ => Err(format!("row {at} ({utc_text}): `{field}` has no two-sided price")),
            }
        };
        bars.push(Bar {
            ts: utc_ms,
            open: mid("openPrice")?,
            high: mid("highPrice")?,
            low: mid("lowPrice")?,
            close: mid("closePrice")?,
            // A tick count, as OANDA's volume is; absent reads as no ticks.
            volume: row.get("lastTradedVolume").and_then(json_number).unwrap_or(0.0),
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    let instrument_type = body.get("instrumentType").and_then(Value::as_str).map(str::to_string);
    Ok(Rows { bars, instrument_type })
}

/// Drop the bars still FORMING at `now_ms` — the one whose open plus its step is in the future. IG serves
/// the current bar as the last row; at most one is forming, but the loop does not rely on it.
fn closed_bars(mut bars: Vec<Bar>, step_ms: i64, now_ms: i64) -> Vec<Bar> {
    while bars.last().is_some_and(|bar| bar.ts.saturating_add(step_ms) > now_ms) {
        bars.pop();
    }
    bars
}

/// `ms` on the account's local wall clock, `yyyy-MM-dd'T'HH:mm:ss` — the form `from` and `to` are read
/// in. `None` when the instant has no four-digit year.
fn local_wall_clock(ms: i64, offset_minutes: i32) -> Option<String> {
    let local = ms.checked_add(i64::from(offset_minutes) * 60_000)?;
    let full = vike_model::time::utc_rfc3339(local.div_euclid(1000));
    (full.len() == 20 && full.ends_with('Z')).then(|| full[..19].to_string())
}

/// What the market snapshot says the epic costs now, as a price: `level x scalingFactor x pip`.
/// `Err` is why it cannot be had — the market is shut, or the snapshot lacks a figure.
fn snapshot_price(market: &Value) -> Result<f64, String> {
    let status = market.pointer("/snapshot/marketStatus").and_then(Value::as_str).unwrap_or("");
    if status != "TRADEABLE" {
        return Err(format!("its market status is `{status}`, not TRADEABLE"));
    }
    let number = |pointer: &str| {
        market.pointer(pointer).and_then(json_number).filter(|x| x.is_finite() && *x > 0.0)
    };
    let (Some(bid), Some(offer)) = (number("/snapshot/bid"), number("/snapshot/offer")) else {
        return Err("it carries no two-sided price".to_string());
    };
    let Some(factor) = number("/snapshot/scalingFactor") else {
        return Err("it states no scalingFactor".to_string());
    };
    let pip = market
        .pointer("/instrument/onePipMeans")
        .and_then(Value::as_str)
        .and_then(|text| text.split_whitespace().next())
        .and_then(|word| word.parse::<f64>().ok())
        .filter(|x| x.is_finite() && *x > 0.0);
    let Some(pip) = pip else {
        return Err("its onePipMeans is unreadable".to_string());
    };
    Ok((bid + offer) / 2.0 * factor * pip)
}

/// How the newest history close compares with the market snapshot.
#[derive(Debug, PartialEq)]
enum Scale {
    /// Within [`RATE_TOLERANCE`] of the snapshot's price: the levels are price units.
    Agrees,
    /// Off by more than the tolerance: the history is not in the snapshot's units.
    Disagrees(String),
    /// The snapshot cannot witness a price right now (the market is shut, a figure is missing).
    Unchecked(String),
}

fn compare_scale(epic: &str, newest_close: f64, market: &Value) -> Scale {
    match snapshot_price(market) {
        Err(why) => Scale::Unchecked(why),
        Ok(price) if (newest_close / price - 1.0).abs() <= RATE_TOLERANCE => Scale::Agrees,
        Ok(price) => Scale::Disagrees(format!(
            "ig: {epic}'s newest history close is {newest_close} but its market snapshot prices it at \
             {price} (level x scalingFactor x pip), more than {} percent apart, so the history is not \
             in price units. It is REFUSED, never rescaled on a guess: a wrong scale stored once is \
             permanent.",
            RATE_TOLERANCE * 100.0
        )),
    }
}

// ── the source ─────────────────────────────────────────────────────────────────────────────────

/// What one source remembers across its chunks. Never held across a network call.
#[derive(Default)]
struct State {
    api: Option<Arc<dyn PricesApi>>,
    /// Epics whose first read and scale check have been done by this source.
    warmed: HashSet<String>,
    /// Points this source has spent, by the rows IG sent.
    spent: u32,
    /// The week's size, once an answer has stated it.
    total: Option<u32>,
    /// The last allowance an answer stated.
    remaining: Option<u32>,
}

/// **IG's history as a `KlineSource`**: one per REQUEST (the datahub builds a source per `Backfill`, and
/// the gatewayhub keeps it for the connection), holding one logged-in session and a count of the points
/// it has spent. See the module doc for what the probe measured and what it refuses.
pub struct IgKlines {
    login: LoginFn,
    gate: Arc<dyn AllowanceGate>,
    clock: fn() -> i64,
    state: Mutex<State>,
}

impl IgKlines {
    /// The production source: `config_for` yields the demo account's three fields (asked only when a
    /// LOGIN happens), the session is `IgSession::login` on the demo gateway held in `cache` — the
    /// process's one session, shared by every request — and `gate` is the process's ledger.
    pub fn new(
        config_for: ConfigProvider,
        gate: Arc<dyn AllowanceGate>,
        cache: &Arc<SessionCache>,
    ) -> Self {
        let login: SessionLoginFn = Arc::new(move || {
            let config = config_for()?;
            let session = IgSession::login(&config).map_err(|e| {
                SourceError::Fetch(format!(
                    "ig: the history lane could not log in ({e}); nothing was asked of the price endpoint"
                ))
            })?;
            Ok(Arc::new(session) as Arc<dyn HistorySession>)
        });
        Self::with_login(cache.through(login), gate)
    }

    /// A source whose session comes from `login`: the seam a suite scripts the gateway through.
    pub fn with_login(login: LoginFn, gate: Arc<dyn AllowanceGate>) -> Self {
        Self { login, gate, clock: vike_model::now_ms, state: Mutex::new(State::default()) }
    }

    /// The same source reading "now" from `clock` (a suite's fixed instant).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The session, logged in on first use. A login is made once per source, and the gate is told whose
    /// account it is.
    fn session(&self) -> Result<Arc<dyn PricesApi>, SourceError> {
        let held = self.lock().api.clone();
        if let Some(api) = held {
            return Ok(api);
        }
        let api = match (self.login)() {
            Ok(api) => api,
            Err(e) => {
                // A login IG refused (a `Fetch`) is told to the ledger, which holds the next one off; a
                // credential that is simply not stored (`Refused`) asked nothing of IG.
                if let SourceError::Fetch(why) = &e {
                    self.gate.login_failed(why);
                }
                return Err(e);
            }
        };
        self.gate.bind(&api.account_id());
        self.lock().api = Some(Arc::clone(&api));
        Ok(api)
    }

    /// Everything that can be refused without asking IG, as `Refused`: the symbol's spelling, the
    /// interval, the window. Returns the IG resolution and the bar width.
    fn refuse_before_asking(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<(&'static str, i64), SourceError> {
        if !vike_catalog::is_ig_epic(symbol) {
            return Err(SourceError::Refused(format!(
                "ig: {symbol:?} is not spelled as an IG epic (five upper-case dotted segments, like \
                 CS.D.EURUSD.MINI.IP). A series is stored under the symbol the request carried, so a \
                 second spelling of one instrument would be a second series; this lane maps no pair \
                 name to an epic. Nothing was asked of IG."
            )));
        }
        let Some(served) = served_interval(interval) else {
            return Err(SourceError::Refused(format!(
                "ig: the history serves minute, hour and day bars (1m 2m 3m 5m 10m 15m 30m 1h 2h 3h \
                 4h 1d); {interval:?} is not one of them. Nothing was asked of IG."
            )));
        };
        if end_ms < start_ms {
            return Err(SourceError::Refused(format!(
                "ig: the window [{start_ms}, {end_ms}] is inverted. Nothing was asked of IG."
            )));
        }
        Ok(served)
    }

    /// `GET /prices/{epic}` through the gate's learning and the 403 politeness: the allowance of any
    /// answer replaces the ledger's belief, and an allowance refusal becomes the plain sentence.
    fn get_prices(
        &self,
        api: &dyn PricesApi,
        epic: &str,
        query: &str,
    ) -> Result<Value, SourceError> {
        match api.get(&format!("/prices/{epic}"), "3", query) {
            Ok(body) => {
                if let Some(allowance) = parse_allowance(&body) {
                    self.gate.learn(allowance);
                    let mut state = self.lock();
                    state.total = Some(allowance.total);
                    state.remaining = Some(allowance.remaining);
                }
                Ok(body)
            }
            Err(e) if is_allowance_refusal(&e) => {
                Err(SourceError::Fetch(self.gate.exhausted(&e.message)))
            }
            Err(e) => {
                self.note_unauthorised(&e);
                let may_be_counted = if e.status == 0 || e.outcome_unknown() {
                    " The request may have reached IG and been counted against the allowance; it is \
                     not repeated."
                } else {
                    ""
                };
                Err(SourceError::Fetch(format!(
                    "ig: the price request failed: {e}.{may_be_counted}"
                )))
            }
        }
    }

    /// A `401` that reaches this far is one the session's own ladder (one re-login) did not cure — a
    /// rotated password, say. Told to the ledger as a refused login, so it holds the next login off
    /// instead of letting every retry send another failed one (the session itself is marked stale by
    /// its lease, so the next login after the hold replaces it).
    fn note_unauthorised(&self, e: &IgApiError) {
        if e.status == 401 {
            self.gate.login_failed(&format!(
                "IG answered {e} to a request even after the session's own re-login"
            ));
        }
    }

    fn add_spent(&self, points: u32) {
        let mut state = self.lock();
        state.spent = state.spent.saturating_add(points);
    }

    /// **The first read of an epic, once per source**: one daily bar (one point), which teaches the
    /// ledger the allowance, proves the account's clock is still the one the lane asks by (its row is
    /// checked against `crate::account_clock` like every other) and gives the newest close the scale is
    /// checked with.
    fn warm(
        &self,
        api: &dyn PricesApi,
        epic: &str,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), SourceError> {
        if self.lock().warmed.contains(epic) {
            return Ok(());
        }
        self.gate.admit(FIRST_READ_POINTS, &format!("the first daily bars of {epic}"), stop)?;
        let body = self.get_prices(api, epic, "resolution=DAY&max=1&pageSize=0")?;
        let rows = read_rows(&body).map_err(|why| {
            self.add_spent(FIRST_READ_POINTS);
            SourceError::Fetch(format!("ig: the first read of {epic} is unusable: {why}"))
        })?;
        self.add_spent(u32::try_from(rows.bars.len()).unwrap_or(u32::MAX));
        let Some(newest) = rows.bars.last() else {
            return Err(SourceError::Fetch(format!(
                "ig: the first read of {epic} returned no daily bar, so the account's clock (which \
                 `from` and `to` are read in) was not seen to agree with the lane's rule and no \
                 window can be asked"
            )));
        };
        self.check_scale(api, epic, newest.close, rows.instrument_type.as_deref(), stop)?;
        self.lock().warmed.insert(epic.to_string());
        Ok(())
    }

    /// **The scale check, which fails CLOSED**: the newest close against the market snapshot. A
    /// disagreement is a refusal for every class. A snapshot that cannot witness a price (the market is
    /// shut, a figure is missing, the read failed) only warns for the three [`MEASURED_CLASSES`], whose
    /// history the probe read in price units; for any other class, or none, it is a refusal. A `403`
    /// allowance refusal on the snapshot goes through the gate like any other.
    fn check_scale(
        &self,
        api: &dyn PricesApi,
        epic: &str,
        newest_close: f64,
        class: Option<&str>,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), SourceError> {
        self.gate.admit(0, &format!("the market prices of {epic}"), stop)?;
        let measured = class.is_some_and(|c| MEASURED_CLASSES.contains(&c));
        let unwitnessed = |why: String| -> Result<(), SourceError> {
            if measured {
                tracing::warn!(
                    target: "vike_ig::klines",
                    %epic,
                    "the history's scale is NOT checked: {why}"
                );
                return Ok(());
            }
            let kind = class.map_or_else(
                || "a market with no stated instrument type".to_string(),
                |c| format!("a {c} market"),
            );
            Err(SourceError::Refused(format!(
                "ig: {epic} is {kind}, and the scale of its history could not be checked: {why}. \
                 Only currencies, indices and commodities were read in price units by the 2026-10-23 \
                 probe, so for any other class the history is stored only when the market snapshot \
                 confirms the scale. It is REFUSED, never guessed: a wrong scale stored once is \
                 permanent. Nothing was stored; try again while the market is open."
            )))
        };
        let market = match api.get(&format!("/markets/{epic}"), "3", "") {
            Ok(market) => market,
            Err(e) if is_allowance_refusal(&e) => {
                return Err(SourceError::Fetch(self.gate.exhausted(&e.message)));
            }
            Err(e) => {
                self.note_unauthorised(&e);
                return unwitnessed(format!("the market snapshot could not be read ({e})"));
            }
        };
        match compare_scale(epic, newest_close, &market) {
            Scale::Agrees => Ok(()),
            Scale::Disagrees(why) => Err(SourceError::Refused(why)),
            Scale::Unchecked(why) => {
                unwitnessed(format!("the market snapshot cannot witness a price ({why})"))
            }
        }
    }

    /// One request may spend only [`REQUEST_SHARE_PERCENT`] percent of the week.
    fn check_request_share(&self, chunk: &str, est: u32) -> Result<(), SourceError> {
        let (spent, total, remaining) = {
            let state = self.lock();
            (state.spent, state.total, state.remaining)
        };
        let Some(total) = total else { return Ok(()) };
        if u64::from(spent) + u64::from(est) > u64::from(request_cap_points(total)) {
            return Err(SourceError::Fetch(request_share_words(
                chunk, est, spent, total, remaining,
            )));
        }
        Ok(())
    }

    /// [`KlineSource::fetch`] with the request's stop probe: asked while the pace is waited out and
    /// before every call, never inside one.
    ///
    /// The bars are the CLOSED mid bars of `[start_ms, end_ms]`, stamped at their open, ascending; the
    /// forming bar is dropped. A window that holds a clock change of the account is asked in two (or
    /// more) pieces, each in the offset valid at it, and returned whole: the chunk and its key are the
    /// caller's and do not change. Every failure is `Fetch` (IG was asked, or the ledger stopped the
    /// chunk in the sentence the operator reads) except the things that need no IG at all (the symbol,
    /// the interval, the window), which are `Refused`.
    pub fn fetch_until(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        let (resolution, step_ms) =
            self.refuse_before_asking(symbol, interval, start_ms, end_ms)?;
        let est = estimate_points(interval, start_ms, end_ms).unwrap_or(u32::MAX);
        let chunk = chunk_words(interval, start_ms, end_ms);
        let cancelled = || SourceError::Fetch("ig: the history fetch was cancelled".to_string());
        if stop() {
            return Err(cancelled());
        }
        // The chunk, cut at every clock change inside it: no piece is asked in an offset it does not
        // have, so no request is knowingly sent into a failure that is charged.
        let Some(pieces) = account_clock::parts(start_ms, end_ms) else {
            return Err(SourceError::Refused(format!(
                "ig: the window [{start_ms}, {end_ms}] lies outside the years (1996 to 9999) the \
                 account's clock rule is known for. Nothing was asked of IG."
            )));
        };
        // What the ledger already knows is refused BEFORE the login: a known wall, a drained week or a
        // refused login costs neither a session nor a turn.
        self.gate.precheck(est, &chunk)?;
        let api = self.session()?;
        self.warm(&*api, symbol, stop)?;
        let mut bars: Vec<Bar> = Vec::new();
        for (n, piece) in pieces.into_iter().enumerate() {
            // Each piece is asked EXACTLY its own window, whatever side of a change "now" is on: IG reads
            // `from` and `to` at the offset of the instant they name (`crate::account_clock`).
            let ask = Ask { symbol, interval, resolution, step_ms, piece };
            match self.ask_piece(&*api, &ask, stop) {
                Ok(piece_bars) => bars.extend(piece_bars),
                Err(SourceError::Fetch(why)) if n > 0 => {
                    return Err(SourceError::Fetch(format!(
                        "{why} The earlier piece of {chunk} WAS asked and charged and is not stored: a \
                         chunk is stored whole or not at all."
                    )));
                }
                Err(e) => return Err(e),
            }
        }
        // An empty chunk is final once a later chunk holds data, so a wide one that has weekdays in it
        // and no bars (IG failed, or the instrument was not listed yet) is not stored.
        if bars.is_empty() && end_ms.saturating_sub(start_ms) >= vike_model::MS_PER_DAY {
            let weekdays = weekdays_in(start_ms, end_ms);
            if weekdays > 0 {
                return Err(SourceError::Fetch(format!(
                    "ig: IG returned no bars for {chunk}, a window that holds {weekdays} weekdays. An \
                     empty chunk is final once a later chunk holds data (its key is spent for good), \
                     so this one is not stored. If the instrument was not listed yet, start the window \
                     after its first trading day; otherwise run the command again later."
                )));
            }
        }
        Ok(closed_bars(bars, step_ms, (self.clock)()))
    }

    /// **One request to IG**: a piece of a chunk, asked in the offset valid at it, through the gate, then
    /// checked against the window and cut to it. The piece of a chunk with no clock change is the chunk.
    fn ask_piece(
        &self,
        api: &dyn PricesApi,
        ask: &Ask<'_>,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        let Part { start_ms, end_ms, offset_minutes } = ask.piece;
        let est = estimate_points(ask.interval, start_ms, end_ms).unwrap_or(u32::MAX);
        let chunk = chunk_words(ask.interval, start_ms, end_ms);
        self.check_request_share(&chunk, est)?;
        self.gate.admit(est, &chunk, stop)?;
        let (Some(from), Some(to)) =
            (local_wall_clock(start_ms, offset_minutes), local_wall_clock(end_ms, offset_minutes))
        else {
            return Err(SourceError::Refused(format!(
                "ig: the window [{start_ms}, {end_ms}] has no four-digit year on the account's \
                 clock. Nothing was asked of IG."
            )));
        };
        // Both ends are inclusive (the probe read them so); `to` is the last whole second of the chunk.
        let query = format!("resolution={}&from={from}&to={to}&pageSize=0", ask.resolution);
        let body = self.get_prices(api, ask.symbol, &query)?;
        let rows = match read_rows(&body) {
            Ok(rows) => rows,
            Err(why) => {
                self.add_spent(est);
                return Err(SourceError::Fetch(format!(
                    "ig: the answer for {chunk} is unusable and is not stored: {why}. It may have \
                     been charged up to {est} points, and the same window asked again is charged again."
                )));
            }
        };
        let charged = u32::try_from(rows.bars.len()).unwrap_or(u32::MAX);
        self.add_spent(charged);
        tracing::info!(
            target: "vike_ig::klines",
            "ig history chunk: {chunk} rows {charged} of at most {est} points"
        );
        // A shifted window must never be stored: the ingest silently drops the bars outside a chunk and
        // spends its key. A bar one step outside is the edge of an inclusive range; more is a shift.
        let stray = rows.bars.iter().find(|bar| {
            bar.ts < start_ms.saturating_sub(ask.step_ms)
                || bar.ts > end_ms.saturating_add(ask.step_ms)
        });
        if let Some(stray) = stray {
            return Err(SourceError::Fetch(format!(
                "ig: the answer for {chunk} holds a bar at {} outside the window it was asked for \
                 ({} to {}), so the window was shifted or the answer is not what was asked: the chunk \
                 is not stored. The answer WAS charged ({charged} points).",
                utc_minute(stray.ts),
                utc_minute(start_ms),
                utc_minute(end_ms)
            )));
        }
        Ok(rows.bars.into_iter().filter(|bar| (start_ms..=end_ms).contains(&bar.ts)).collect())
    }
}

/// One request of [`IgKlines::fetch_until`]: a piece of a chunk, asked exactly as the piece is.
struct Ask<'a> {
    symbol: &'a str,
    interval: &'a str,
    resolution: &'static str,
    step_ms: i64,
    piece: Part,
}

impl KlineSource for IgKlines {
    fn venue(&self) -> &str {
        crate::recon_client::VENUE
    }

    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        self.fetch_until(symbol, interval, start_ms, end_ms, &|| false)
    }
}

impl std::fmt::Debug for IgKlines {
    /// Prints nothing it holds: the session and the credential stay out of every formatter.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IgKlines").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "klines_tests.rs"]
mod klines_tests;
