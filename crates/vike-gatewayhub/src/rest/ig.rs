//! **The IG history lane: the one demo-account login, the allowance ledger, the pace, the presence
//! probe and the startup line** — decision 0194, which generalises decision 0097's OANDA lane to a
//! venue whose history needs a LOGIN. The source it dispatches is `vike_ig::IgKlines` (the fetch half,
//! store-free); this file is what only this daemon may do for it.
//!
//! # The credential, and what the lane may do with it
//!
//! IG's history needs THREE fields of one account — `API_KEY`, `IDENTIFIER`, `PASSWORD`
//! (`vike_ig::FIELDS`) — and a logged-in session. The fields are those of the ONE ACTIVE unlabelled ig
//! account at tier `demo` ([`VENUE`], `DEMO_TIER`), read through
//! `vike_secrets::CredentialStore::account_field`, the narrowest read the store offers, once per
//! CONNECTION (a connection is a request: `crate::session`'s module doc) and only when a LOGIN is
//! needed, kept in a [`Redacted`] memo and dropped with the connection. The logged-in session itself is
//! the PROCESS's, one for every request (`vike_ig::SessionCache`: reused while it is used, logged out
//! after twenty idle minutes and at a clean shutdown), so a field stored or changed while a session is
//! held takes effect after that idle time. No credential NAME is composed, a `live` account is never
//! read, and the gateway is the demo gateway (`vike_ig::ig_rest_base`).
//!
//! ⚠ **IG offers no read-only key, so a demo session CAN place demo orders.** This lane is read-only
//! by construction and by a gate, not by IG: it serves `FetchWhat::Klines` only, `vike_ig::PricesApi`
//! has one method (`get`), and `crates/vike-ops/tests/venues/collector_dispatch_gate.rs`'s
//! `the_ig_history_lane_only_reads` holds this file and `crates/bridges/ig/src/klines.rs` to GET requests
//! and no order builder. A change that adds any write call here reopens decision 0194.
//!
//! ⚠ **Unreadable is not absent.** A store that exists and will not open is
//! [`IgCredentialError::StoreUnreadable`] and `CredentialPresence::Unreadable`, logged, and never
//! folded into the not-configured answer (0097's verdict 6). The error types carry field NAMES by
//! position and no value, so a credential cannot reach an error text by construction.
//!
//! # The allowance ledger
//!
//! IG meters history in points, one per row, against a weekly allowance that every `/prices` answer
//! states (`vike_ig::parse_allowance`). [`AllowanceLedger`] is what the daemon believes about it, ONE per
//! process and shared by every IG history fetch, because the pool is the key's and not a request's:
//!
//! * **Every answer teaches it** (`learn`): the server's figure replaces the belief, so spending by
//!   anything else (a manual run, the reconcile reads) is repaired by the next answer.
//! * **It refuses to START a chunk that would leave less than the reserve** (`vike_ig::RESERVE_PERCENT`
//!   of the week, rounded up): the refusal is a sentence with the numbers and the reset
//!   (`vike_ig::reserve_stop_words`), and nothing is asked of IG for that chunk.
//! * **It paces**: one request per `vike_ig::PACE` (six seconds, ten a minute, a third of IG's
//!   documented thirty non-trading requests a minute per account), in turn, whichever fetch asks; the
//!   wait asks the fetch's cancel flag every 200 milliseconds.
//! * **It remembers a wall**: a `403` allowance refusal blocks the next requests locally until the
//!   window resets (the weekly allowance) or for a minute (the per-minute ceilings), so no later chunk
//!   or fetch walks into it again.
//! * **It is asked BEFORE the login** (`precheck`: the wall, the reserve, without taking a turn), so a
//!   known refusal costs no login; and **it holds a refused login off for five minutes**
//!   (`login_failed`), because IG documents a lockout on repeated failed logins and the trading side's
//!   own re-login shares the account. A login that worked (`bind`) clears the hold.
//! * **It is keyed by IG's account id** (public, never a credential): `bind` forgets everything when
//!   the logged-in account is another, which is how a rotated credential starts a clean ledger.
//!
//! What the operator reads is in the refusal sentences and, once a request ends, one `info` line with
//! the points left and when the week resets ([`AllowanceLedger::summary`]). A field on the wire's
//! `BackfillDone` that carried it would be a shared wire change and is not built.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vike_data::source::SourceError;
use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::proto::FetchFailure;
use vike_ig::{Allowance, AllowanceGate, ConfigProvider, FIELDS, IgConfig, IgKlines, SessionCache};
use vike_marketdata::Bar;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::{FetchCtx, cancelled};
use crate::session::{ConnectionMemo, ConnectionScope, Redacted};

/// The venue whose demo account holds the credential.
pub const VENUE: &str = "ig";

/// The account's tier: IG's demo gateway is the account at tier `demo`. A `live` account is never
/// read, and the lane's host is the demo host.
const DEMO_TIER: AccountTier = AccountTier::Demo;

/// How long a per-minute ceiling (`error.public-api.exceeded-api-key-allowance` and its account
/// sibling) blocks the next requests locally: a minute for the ceiling to refill and five seconds'
/// margin.
const PER_MINUTE_CEILING_HOLD_MS: i64 = 65_000;

/// How long the weekly allowance blocks when IG refused it and no reset instant is known: an hour.
const UNKNOWN_RESET_HOLD_MS: i64 = 3_600_000;

/// How long a login IG refused holds the next login off locally: five minutes. IG documents a lockout on
/// repeated failed logins, and the trading side's own re-login shares the account.
const LOGIN_REFUSED_HOLD_MS: i64 = 300_000;

/// How often a wait for the pace asks whether the fetch was cancelled.
const CANCEL_SLICE: Duration = Duration::from_millis(200);

// ── the credential ─────────────────────────────────────────────────────────────────────────────

/// Why the three fields could not be had. Names by position only: no value is in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgCredentialError {
    /// Which of the three fields (`vike_ig::FIELDS`' order) are absent or blank on the demo account,
    /// or all of them when no account or no store holds them.
    NotConfigured([bool; 3]),
    /// The store exists and will not open.
    StoreUnreadable,
}

/// The account in words, for a log line or a refusal: which account, never a value.
fn demo_account_words() -> String {
    format!("the ACTIVE unlabelled {VENUE} account at tier `{DEMO_TIER}`")
}

/// The names of the fields flagged absent, `IDENTIFIER, PASSWORD`.
fn missing_words(absent: [bool; 3]) -> String {
    let names: Vec<&str> =
        FIELDS.iter().zip(absent).filter(|(_, gone)| *gone).map(|(field, _)| *field).collect();
    names.join(", ")
}

/// The refusal an absent or unreadable credential earns, naming the fix. Words and field names only.
fn refusal_words(e: IgCredentialError) -> String {
    let account = demo_account_words();
    let all = FIELDS.join(", ");
    match e {
        IgCredentialError::NotConfigured(absent) => format!(
            "ig history needs {all} of {account} in this gatewayhub's credential store, and the \
             store lacks: {}. Nothing was asked of IG. On THIS box, `vike-cli secrets set --account \
             <ID> <FIELD>` stores one field (`vike-cli secrets accounts` lists the rows; `vike-cli \
             secrets account add --venue {VENUE} --tier {DEMO_TIER} --no-label` creates the \
             account). The fields are read per connection, so there is nothing to restart",
            missing_words(absent)
        ),
        IgCredentialError::StoreUnreadable => format!(
            "ig history could not read this gatewayhub's credential store, so whether {account} \
             holds {all} cannot be told. This is not the same as the fields being absent; the \
             store's error is in the gatewayhub's log. Nothing was asked of IG"
        ),
    }
}

/// The `error!` an unreadable store earns on the lane's reads.
fn log_store_unreadable(e: &vike_secrets::SecretsError) {
    tracing::error!(
        error = %e,
        "the credential store could not be read for the IG history lane's demo account; a \
         credentialed fetch for ig is refused, and it is NOT the same as the fields being absent"
    );
}

/// **The demo account's three fields as an [`IgConfig`]**, read through
/// `vike_secrets::CredentialStore::account_field` — one field of one account row each, and nothing
/// else. `pub(crate)`: the result holds the password, and a `pub` here is a function any dependent crate
/// could call to bypass the [`Redacted`] memo. A store with no location holds nothing; a field that is
/// absent or blank is flagged; a store that exists and will not open is
/// [`IgCredentialError::StoreUnreadable`], logged.
pub(crate) fn read_ig_history_config(
    store: &vike_secrets::CredentialStore,
) -> Result<IgConfig, IgCredentialError> {
    if store.location().is_none() {
        return Err(IgCredentialError::NotConfigured([true; 3]));
    }
    let mut values: [Option<String>; 3] = [None, None, None];
    for (slot, field) in values.iter_mut().zip(FIELDS) {
        match store.account_field(VENUE, &AccountLabel::Default, DEMO_TIER, field) {
            Ok(value) => *slot = value.map(vike_model::secret::Secret::into_inner),
            Err(e) => {
                log_store_unreadable(&e);
                return Err(IgCredentialError::StoreUnreadable);
            }
        }
    }
    let [api_key, identifier, password] = values;
    match (api_key, identifier, password) {
        (Some(api_key), Some(identifier), Some(password)) => Ok(IgConfig {
            api_key,
            identifier,
            password,
            rest_base: vike_ig::ig_rest_base(DEMO_TIER).to_string(),
            // The history lane reads prices, not an account's money: the rule picks the account.
            account_pin: None,
        }),
        (api_key, identifier, password) => Err(IgCredentialError::NotConfigured([
            api_key.is_none(),
            identifier.is_none(),
            password.is_none(),
        ])),
    }
}

/// **Whether the IG history lane's credential is STORED — a presence word, never a value.** Present
/// only when ALL THREE fields are non-blank on the demo account (one boolean per field, computed by
/// SQLite; no value becomes a Rust value in this process), so this word cannot say the lane is armed
/// when the lane then refuses. No settings directory is `Absent`; a store that exists and will not
/// read is `Unreadable`, logged, and never folded into `Absent`.
pub fn ig_history_presence(store: &vike_secrets::CredentialStore) -> CredentialPresence {
    if store.location().is_none() {
        return CredentialPresence::Absent;
    }
    let mut all_present = true;
    for field in FIELDS {
        match store.account_field_present(VENUE, &AccountLabel::Default, DEMO_TIER, field) {
            Ok(present) => all_present &= present,
            Err(e) => {
                log_store_unreadable(&e);
                return CredentialPresence::Unreadable;
            }
        }
    }
    if all_present { CredentialPresence::Present } else { CredentialPresence::Absent }
}

/// The startup line that says whether the IG history lane is ARMED, from the presence word alone (the
/// lane's own read never runs at startup, so no value is ever in scope). The prefix `ig history lane:`
/// is what an operator greps.
pub fn ig_history_lane_line(settings_dir: Option<&Path>, presence: CredentialPresence) -> String {
    let account = demo_account_words();
    let all = FIELDS.join(", ");
    match (settings_dir, presence) {
        (None, _) => format!(
            "ig history lane: not armed — this daemon resolved no settings directory, so there is \
             no credential store to hold {all} of {account}, and a credentialed fetch for ig is \
             refused"
        ),
        (Some(_), CredentialPresence::Present) => format!(
            "ig history lane: ARMED — {all} of {account} are present in this daemon's credential \
             store. They are read when a login is needed (a connection's first FetchCredentialed \
             for ig, or the first after the session has been idle {} minutes; that request needs the \
             Control key). ONE session is logged in on IG's DEMO gateway and shared by every fetch, \
             and only prices are read through it (decision 0194). IG's weekly historical-data \
             allowance is counted here: {} percent of it is never spent, one request is capped at \
             {} percent of it, a window is refused above {} percent of the week, only 1h and 1d \
             bars are served, and requests are paced at one every {} seconds",
            vike_ig::SESSION_IDLE.as_secs() / 60,
            vike_ig::RESERVE_PERCENT,
            vike_ig::REQUEST_SHARE_PERCENT,
            vike_catalog::IG_WINDOW_SHARE_PERCENT,
            vike_ig::PACE.as_secs()
        ),
        (Some(_), CredentialPresence::Unreadable) => format!(
            "ig history lane: UNKNOWN — the credential store could not be read (the error above \
             names it), so whether {all} of {account} are stored cannot be told, and a credentialed \
             fetch for ig is refused until it can be. This is not the same as the fields being \
             absent"
        ),
        (Some(_), _) => format!(
            "ig history lane: not armed — {all} of {account} are not all present in this daemon's \
             credential store, so a credentialed fetch for ig is refused naming the missing \
             field. On THIS box, `vike-cli secrets set --account <ID> <FIELD>` stores one \
             (`vike-cli secrets accounts` lists the rows; `vike-cli secrets account add --venue \
             {VENUE} --tier {DEMO_TIER} --no-label` creates the account). The fields are read per \
             connection, so there is nothing to restart"
        ),
    }
}

// ── the ledger ─────────────────────────────────────────────────────────────────────────────────

/// What the ledger last heard, and when (epoch-ms on this daemon's clock).
#[derive(Debug, Clone, Copy)]
struct Seen {
    allowance: Allowance,
    at_ms: i64,
}

/// A wall IG answered with: until when requests are refused locally, and the sentence to refuse with.
#[derive(Debug)]
struct Blocked {
    until_ms: i64,
    words: String,
}

/// The ledger's one cell. IG's account id is public (the trade side logs it); the credentials are not
/// here.
#[derive(Debug, Default)]
struct Cell {
    account: Option<String>,
    seen: Option<Seen>,
    blocked: Option<Blocked>,
    /// A login IG refused: until when no new login is tried. Cleared by a login that worked (`bind`).
    login_hold: Option<Blocked>,
}

/// **What this daemon believes about IG's historical-data allowance**: see the module doc. One per
/// process, owned by the [`IgLane`] and shared by every connection's source through
/// [`AllowanceGate`].
#[derive(Debug)]
pub struct AllowanceLedger {
    pace: Duration,
    now_ms: fn() -> i64,
    cell: Mutex<Cell>,
    /// The instant the next request may be sent: each admission takes the later of now and this, then
    /// moves it one pace on, so requests leave in turn whichever fetch asks.
    next_slot: Mutex<Instant>,
}

impl Default for AllowanceLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl AllowanceLedger {
    /// The production ledger: IG's pace ([`vike_ig::PACE`]) on the wall clock.
    pub fn new() -> Self {
        Self::with(vike_ig::PACE, vike_model::now_ms)
    }

    /// A ledger at `pace` reading "now" from `now_ms` — a suite's way to stand on a reset instant or to
    /// not wait six seconds.
    pub fn with(pace: Duration, now_ms: fn() -> i64) -> Self {
        // The first request waits for nothing: the slot starts one pace in the past.
        let first = Instant::now().checked_sub(pace).unwrap_or_else(Instant::now);
        Self { pace, now_ms, cell: Mutex::new(Cell::default()), next_slot: Mutex::new(first) }
    }

    fn cell(&self) -> MutexGuard<'_, Cell> {
        self.cell.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The last allowance an answer stated and the epoch-ms it was heard at, if any.
    pub fn last_seen(&self) -> Option<(Allowance, i64)> {
        self.cell().seen.map(|seen| (seen.allowance, seen.at_ms))
    }

    /// The closing line of a request, in words: how many points are left and when the week resets.
    /// `None` until an answer has been heard.
    pub fn summary(&self) -> Option<String> {
        let (allowance, at_ms) = self.last_seen()?;
        Some(format!(
            "ig history request ended. IG allowance left: {} of {} points. {}",
            allowance.remaining,
            allowance.total,
            vike_ig::reset_sentence(Some((at_ms, allowance.resets_in_s)))
        ))
    }

    /// The refusal rules, judged now and without waiting: a wall IG answered with, or a chunk that
    /// would leave less than the reserve. With nothing heard yet (the first read has not happened)
    /// there is nothing to refuse on.
    fn check(&self, points: u32, what: &str) -> Result<(), SourceError> {
        let now = (self.now_ms)();
        let cell = self.cell();
        if let Some(blocked) = &cell.blocked
            && now < blocked.until_ms
        {
            return Err(SourceError::Fetch(blocked.words.clone()));
        }
        let Some(seen) = cell.seen else { return Ok(()) };
        let allowance = seen.allowance;
        let resets_at = seen.at_ms.saturating_add(
            i64::try_from(allowance.resets_in_s.saturating_mul(1000)).unwrap_or(i64::MAX),
        );
        // A window that has reset is a full one; the next answer says so for certain.
        let remaining = if now >= resets_at { allowance.total } else { allowance.remaining };
        if remaining.saturating_sub(points) < vike_ig::reserve_points(allowance.total) {
            return Err(SourceError::Fetch(vike_ig::reserve_stop_words(
                what,
                points,
                &Allowance { remaining, ..allowance },
                Some((seen.at_ms, allowance.resets_in_s)),
            )));
        }
        Ok(())
    }

    /// Take the next slot and wait for it, asking `stop` every [`CANCEL_SLICE`].
    fn wait_for_pace(&self, stop: &dyn Fn() -> bool) -> Result<(), SourceError> {
        let slot = {
            let mut next = self.next_slot.lock().unwrap_or_else(PoisonError::into_inner);
            let slot = (*next).max(Instant::now());
            *next = slot + self.pace;
            slot
        };
        loop {
            if stop() {
                return Err(SourceError::Fetch(
                    "ig: the history fetch was cancelled while it waited for its turn".to_string(),
                ));
            }
            let now = Instant::now();
            if now >= slot {
                return Ok(());
            }
            std::thread::sleep((slot - now).min(CANCEL_SLICE));
        }
    }
}

impl AllowanceGate for AllowanceLedger {
    fn bind(&self, account_id: &str) {
        let mut cell = self.cell();
        if cell.account.as_deref() != Some(account_id) {
            *cell = Cell { account: Some(account_id.to_string()), ..Cell::default() };
        }
        // A login worked: whatever hold a refused one set is gone.
        cell.login_hold = None;
    }

    /// Before the login: a login IG refused a moment ago, a wall it answered with, a chunk that would
    /// leave less than the reserve — refused without a turn of the pace.
    fn precheck(&self, points: u32, what: &str) -> Result<(), SourceError> {
        let now = (self.now_ms)();
        {
            let cell = self.cell();
            if let Some(hold) = &cell.login_hold
                && now < hold.until_ms
            {
                return Err(SourceError::Fetch(hold.words.clone()));
            }
        }
        self.check(points, what)
    }

    fn login_failed(&self, why: &str) {
        let now = (self.now_ms)();
        let words = format!(
            "ig: IG refused a login a moment ago ({why}); the history lane will not try another for \
             {} minutes, because IG locks an account after repeated failed logins and that would also \
             block the trading side's own re-login. Nothing was asked of IG. Check the stored fields, \
             then run the command again after the wait.",
            LOGIN_REFUSED_HOLD_MS / 60_000
        );
        let until_ms = now.saturating_add(LOGIN_REFUSED_HOLD_MS);
        self.cell().login_hold = Some(Blocked { until_ms, words });
    }

    /// Refuse at once what is already known to be refused, wait for the pace, and judge again on what
    /// the wait may have taught: another fetch's answer can arrive while this one waits its turn.
    fn admit(&self, points: u32, what: &str, stop: &dyn Fn() -> bool) -> Result<(), SourceError> {
        self.check(points, what)?;
        self.wait_for_pace(stop)?;
        self.check(points, what)
    }

    fn learn(&self, allowance: Allowance) {
        let at_ms = (self.now_ms)();
        let mut cell = self.cell();
        cell.seen = Some(Seen { allowance, at_ms });
        // An answer arrived: whatever wall was remembered is gone.
        cell.blocked = None;
    }

    fn exhausted(&self, code: &str) -> String {
        let now = (self.now_ms)();
        let mut cell = self.cell();
        let last = cell.seen.map(|seen| (seen.at_ms, seen.allowance.resets_in_s));
        let words = vike_ig::exhausted_words(code, last);
        let until_ms = if code.contains("historical-data") {
            last.map_or(now.saturating_add(UNKNOWN_RESET_HOLD_MS), |(at_ms, resets_in_s)| {
                at_ms.saturating_add(
                    i64::try_from(resets_in_s.saturating_mul(1000)).unwrap_or(i64::MAX),
                )
            })
        } else {
            now.saturating_add(PER_MINUTE_CEILING_HOLD_MS)
        };
        cell.blocked = Some(Blocked { until_ms, words: words.clone() });
        words
    }
}

// ── the lane ───────────────────────────────────────────────────────────────────────────────────

/// What the lane needs of a connection's source: a fetch that asks the request's cancel flag.
/// `vike_ig::IgKlines` in production, a scripted source in a suite.
pub trait IgHistory: Send + Sync {
    /// The CLOSED mid bars of `[start_ms, end_ms]`, asking `stop` while it waits and before each call.
    fn fetch_until(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError>;
}

impl IgHistory for IgKlines {
    fn fetch_until(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        IgKlines::fetch_until(self, symbol, interval, start_ms, end_ms, stop)
    }
}

/// How the lane builds a connection's source around its credential provider and the process's ledger:
/// `IgKlines::new` in production, a scripted source in a suite.
pub type SourceFor =
    Arc<dyn Fn(ConfigProvider, Arc<dyn AllowanceGate>) -> Box<dyn IgHistory> + Send + Sync>;

/// The memo a connection keeps the three fields in: read once, the answer kept (an error included),
/// dropped with the connection. Its error half carries field names and no value.
type ConfigMemo = ConnectionMemo<Redacted<IgConfig>, IgCredentialError>;

/// What one connection holds for the IG lane: the credential memo, the source built around it (which
/// holds the logged-in session once the first fetch has logged in) and the ledger it reports to.
///
/// The memo is an `Arc` of its own so the provider can hold it WITHOUT holding the slot — the
/// OANDA lane's cycle rule, for the same reason: the memo holds neither the source nor the slot, so
/// dropping the scope drops the source (and its session), then the provider, then the fields.
#[derive(Default)]
struct IgSlot {
    config: Arc<ConfigMemo>,
    source: OnceLock<Box<dyn IgHistory>>,
    ledger: OnceLock<Arc<AllowanceLedger>>,
}

impl Drop for IgSlot {
    /// A request ends when its connection closes: say what the week has left, once.
    fn drop(&mut self) {
        if self.source.get().is_none() {
            return;
        }
        if let Some(line) = self.ledger.get().and_then(|ledger| ledger.summary()) {
            tracing::info!("{line}");
        }
    }
}

/// **The credentialed kline lane for IG**: the store handle (a location and a scope, no value), the
/// process's one ledger and the way a connection's source is built.
pub struct IgLane {
    store: vike_secrets::CredentialStore,
    ledger: Arc<AllowanceLedger>,
    source_for: SourceFor,
}

impl IgLane {
    /// The production lane: `vike_ig::IgKlines` around each connection's provider, reading `store`.
    pub fn new(store: vike_secrets::CredentialStore) -> Self {
        // The process's ONE logged-in session: every connection's source logs in through this cache, so
        // a fetch of many calendar-year pieces shares a session instead of opening one per piece. It
        // lives as long as the lane (the source builder holds it) and logs the session out on a clean
        // shutdown.
        let sessions = SessionCache::new();
        Self::with_source(
            store,
            Arc::new(
                move |config: ConfigProvider, gate: Arc<dyn AllowanceGate>| -> Box<dyn IgHistory> {
                    Box::new(IgKlines::new(config, gate, &sessions))
                },
            ),
        )
    }

    /// A lane whose source is built by `source_for` — the seam a suite scripts a source through, so
    /// the REAL memo, the REAL store read, the REAL ledger and the REAL server are under test with no
    /// IG host.
    pub fn with_source(store: vike_secrets::CredentialStore, source_for: SourceFor) -> Self {
        Self { store, ledger: Arc::new(AllowanceLedger::new()), source_for }
    }

    /// The same lane over `ledger` — a suite's ledger (no pace, a fixed clock).
    #[must_use]
    pub fn with_ledger(mut self, ledger: Arc<AllowanceLedger>) -> Self {
        self.ledger = ledger;
        self
    }

    /// The store handle the lane reads.
    pub fn store(&self) -> &vike_secrets::CredentialStore {
        &self.store
    }

    /// The process's ledger.
    pub fn ledger(&self) -> &Arc<AllowanceLedger> {
        &self.ledger
    }

    /// The presence word of the lane's credential, read afresh.
    pub fn presence(&self) -> CredentialPresence {
        ig_history_presence(&self.store)
    }

    /// `FetchCredentialed { Klines }`: the connection's source, fetched once. The source asks its
    /// provider on its first fetch, and the provider answers from the connection's memo, so the store
    /// is read on the first fetch that gets as far as needing a login (a refusal before it, for a
    /// symbol or an interval IG's history does not serve, reads nothing) and never again for this
    /// connection.
    ///
    /// A cancel is seen before the call, while the pace is waited out, and before each request of the
    /// chunk; never inside a request.
    pub fn fetch(
        &self,
        scope: &ConnectionScope,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        ctx: &FetchCtx,
    ) -> Result<Vec<Bar>, FetchFailure> {
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        // Only the measured intervals, whoever asks: the datahub refuses the same ones first, but a
        // client that dials this daemon directly would otherwise reach the source with a minute series.
        if !vike_catalog::ig_interval_admitted(interval) {
            return Err(FetchFailure::Refused(vike_catalog::ig_unmeasured_words(interval)));
        }
        let slot = scope.slot::<IgSlot>();
        let _ = slot.ledger.get_or_init(|| Arc::clone(&self.ledger));
        let source = slot.source.get_or_init(|| {
            (self.source_for)(
                self.config_provider(&slot.config),
                Arc::clone(&self.ledger) as Arc<dyn AllowanceGate>,
            )
        });
        source
            .fetch_until(symbol, interval, start_ms, end_ms, &|| ctx.is_cancelled())
            .map_err(FetchFailure::from_source_error)
    }

    /// The provider a connection's source asks: the memo's answer, read from the store on the first
    /// ask. It holds the store HANDLE and the memo, never the slot.
    fn config_provider(&self, memo: &Arc<ConfigMemo>) -> ConfigProvider {
        let store = self.store.clone();
        let memo = Arc::clone(memo);
        Arc::new(move || {
            match memo.get_or_read(|| read_ig_history_config(&store).map(Redacted::new)) {
                Ok(config) => Ok(config.expose().clone()),
                Err(e) => Err(SourceError::Refused(refusal_words(*e))),
            }
        })
    }
}

impl std::fmt::Debug for IgLane {
    /// The store's location is a path, never a value; the constructor has nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IgLane").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "ig_tests.rs"]
mod ig_tests;
