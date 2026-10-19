//! The backfill-on-demand collector seam (split-plane REQ-9): the venue → collector dispatch
//! table behind `Request::Backfill`.
//!
//! # Why a TABLE and not direct calls
//!
//! The real collectors (the rows `real_backfill_table` folds out of the gatewayhub client's two
//! rosters, directly below) do venue REST I/O — through the gatewayhub since decision 0164: this
//! daemon links no venue bridge for it — and write through the store, which only `backfill-serve`
//! compiles: a property the SERVER must not inherit unconditionally. The table is the smallest
//! injection point that solves both at once:
//!
//! - **Build shape**: the table TYPE is feature-free (`Box<dyn Fn>` over scalars — no
//!   `vike-backfill` name anywhere in its signature), so `server.rs` dispatches through it on
//!   every build; only `real_backfill_table` — the constructor that names the store and the remote
//!   sources — sits behind `backfill-serve`, which is what keeps the default server's dependency
//!   tree collector-free. ⚠ That property is what bounds how far 0059 Phase 3's collapse could reach
//!   in this file: the constructor FOLDS the rosters instead of listing six closures, but
//!   `vike_data::source::KlineSource` may NOT appear in [`BackfillTable`]'s own type, or the
//!   feature-free half would be gone.
//! - **Testability**: CI is deterministic and network-free, so the composed roundtrip test
//!   (`tests/backfill_roundtrip.rs`) installs FAKE entries that write known bars through the same
//!   `Arc<DataFusionHist>` the server serves — proving request → real store → `BackfillDone` →
//!   `LoadBars` without a venue on the wire. The REAL table is driven over a scripted gatewayhub
//!   (`vike_gatewayhub::testkit`, a dev-dependency): the sources it builds are the production
//!   ones, and what the daemon was asked is read back from the script. No test calls a venue; the
//!   `#[ignore]`d live probes are the daemon's own
//!   (`crates/vike-gatewayhub/tests/venue_interval_matrix.rs`).
//!
//! # The write-through contract
//!
//! Every entry writes INTO the store the server serves — the closure captures the same
//! `Arc<DataFusionHist>` handle `serve` holds (upcast to the trait for serving) — so a backfilled
//! range is visible to the very next `LoadBars` on any connection, and is never lost ("writers
//! live next to the data", Principle 3). The verb handler reads the range back through the served
//! handle before replying; [`BackfillDone`](vike_datahub_client::BackfillDone) carries what
//! it found.
//!
//! # Lanes
//!
//! An entry carries a [`HistoryLane`] — `vike_catalog`'s lane vocabulary, the one roster the
//! history-channels table and this table both speak — and the REQUEST picks the lane: `Backfill`
//! reaches the funding lane exactly when its interval is the reserved
//! `vike_data::source::FUNDING_INTERVAL` label and a bar lane otherwise ([`BackfillTable::get`]), while the chart seed reaches the kline
//! lane and nothing else ([`BackfillTable::get_for_seed`]) — so opening a chart never starts a tick
//! download or a funding fetch. The funding lane is negotiated apart from the verb, by
//! `vike_datahub_client::FEATURE_BACKFILL_FUNDING`, which a server advertises exactly when its
//! table's [`BackfillTable::has_funding`] answers `true`. The tick lane is dukascopy's alone and is
//! mounted the same way — one [`HistoryLane::TickBars`] row `real_backfill_table` writes by hand
//! (dukascopy has no `KlineSource` to fold a registry over), fetching through the gatewayhub's tick
//! fetch (`vike_gatewayhub_client::history::RemoteTicks`: the keyless `.bi5` archive read is the
//! daemon's) and storing+resampling through
//! `vike_backfill::venues::dukascopy::backfill_quotes_then_bars` — reachable only from a `Backfill`
//! request naming dukascopy, never from the chart seed.
//!
//! ⚠ **The CREDENTIALED lane is OANDA's alone, and it is the one lane whose collector spends a
//! secret** (docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md)
//! — and since decision 0164 that secret is NOT READ IN THIS PROCESS: the gatewayhub holds the
//! practice token's read and spends it. One hand-written [`HistoryLane::CredentialedKlines`] row
//! `real_backfill_table` builds around a session-scoped `vike_gatewayhub_client::history::
//! RemoteKlines::credentialed` — fetching a UTC day at a time through
//! `vike_backfill::kline_source::backfill_kline_source_chunked`. It answers `Backfill` (the
//! `Scope::Write` scope) at a bar interval and NOTHING else: the chart seed's lookup matches [`HistoryLane::Klines`]
//! alone, and no other verb consults this table. That exclusion is 0097's verdict 3 — a
//! `Scope::Read` client must never make this daemon authenticate as its operator (0062's decision 3).
//!
//! # The registry of RUNNING requests — what the operator's two verbs read
//!
//! The table also keeps every `Backfill` request it is running right now
//! (`docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §4), so the
//! registry lives where the collectors do and `crate::server`'s `serve_authed` takes no new
//! argument. `crate::server`'s `backfill_verb` registers each request just before its collector
//! runs (`BackfillTable::register`) and the returned `Registration`'s `Drop` removes it — the
//! `ConnSlot` and `SessionGuard` idiom, so a collector that PANICS still leaves no entry behind.
//! `ListBackfills` answers `BackfillTable::running`; `CancelBackfill` is `BackfillTable::cancel`,
//! which raises the flag of every request on one series whose lane can stop
//! (`stops_at_a_chunk_boundary`) and lists the rest as unstoppable. The cancel flag reaches the
//! collector through the request's stop probe, which `backfill_verb` composes from the flag and the
//! connection's peek.
//!
//! ⚠ **ONE lock, and it is never held across anything that can block or call out.** The registry is
//! shared by every connection thread. Its `Mutex` guards only the list itself: a push, a `retain`,
//! a snapshot, and raising atomic flags inside a cancel. No collector, no socket, no store and no
//! second lock is touched while it is held, so there is no lock ORDER to get wrong — the property is
//! "never nested", not "nested in the right order". A collector reads its own flag through its own
//! `Arc` without the lock, once per chunk. A poisoned lock is recovered
//! (`PoisonError::into_inner`, this crate's `md` idiom): every critical section is one whole
//! operation on the list, so a holder that panicked left nothing half-done — and a guard dropping
//! during an unwind must not panic a second time.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use vike_catalog::HistoryLane;
use vike_datahub_client::history::CredentialPresence;
use vike_datahub_client::proto::{BackfillCancelDone, RunningBackfill};
use vike_gatewayhub_client::client::GatewayLink;

/// One venue's collector: `(symbol, interval, start_ms, end_ms, should_stop)` → rows written (0 =
/// the window was already ingested — the collectors are idempotent by commit key). The venue and the
/// store are baked into the closure by the table's constructor; errors are stringified because the
/// wire carries them as `Response::Error` text either way.
///
/// `should_stop` is the REQUEST's stop probe — `true` means "stop now", and once it has said so it
/// keeps saying so. `crate::server`'s `backfill_verb` composes it from the requesting connection
/// (`docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §2). A CHUNKED row —
/// the tick lane and the credentialed lane — hands it to its ingest, which asks it between chunks and
/// never inside one, and stops at that boundary with every chunk before it stored. A ONE-BATCH row —
/// the kline fold and the funding lane — has no boundary to stop at and ignores it (the
/// design's §8). A caller that cannot be cancelled — the chart seed — passes one that never fires.
///
/// ⚠ It is a borrowed `&dyn Fn() -> bool`, a std type, and not a type of this crate's or
/// `vike_backfill`'s: the alias must stay feature-free (see "Build shape" in the module doc), and a
/// borrowed closure crosses that seam as it is.
pub type BackfillFn =
    Box<dyn Fn(&str, &str, i64, i64, &dyn Fn() -> bool) -> Result<usize, String> + Send + Sync>;

/// Whether a request on `lane` can be STOPPED — whether its collector is CHUNKED and asks the
/// request's stop probe between chunks. The ONE spelling `CancelBackfill` asks before it raises a
/// flag ([`BackfillTable::cancel`]) and `ListBackfills` reports as `RunningBackfill::stoppable`.
///
/// ⚠ **An exhaustive `match`, the `seed_may_reach` idiom, so a lane added later does not COMPILE
/// until somebody has decided whether a cancel can stop it.** `true` is a promise that the lane's
/// row hands the probe to a chunked ingest (`BackfillFn`'s doc names which rows do); a lane that
/// answers `true` and ignores the probe would make an operator's cancel report a stop that never
/// comes. The one-batch lanes answer `false` and the cancel refuses them by name with
/// `vike_datahub_client::proto::BACKFILL_ONE_BATCH` — the design's §8 and Q6.
fn stops_at_a_chunk_boundary(lane: HistoryLane) -> bool {
    match lane {
        HistoryLane::TickBars | HistoryLane::CredentialedKlines => true,
        HistoryLane::Klines | HistoryLane::Funding => false,
    }
}

/// Whether the chart seed — `Request::SeedSeries`, a `Scope::Read` verb
/// (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`; the scope was called Observe
/// when that record was written) — may reach `lane`. The ONE
/// spelling [`BackfillTable::seed_supported`] and [`BackfillTable::get_for_seed`] both ask.
///
/// ⚠ **An exhaustive `match`, not `lane == Klines`, so a lane added later does not COMPILE until
/// somebody has decided whether a `Scope::Read` client may start it.** [`HistoryLane::CredentialedKlines`]
/// is the case that decision exists for: a `true` there would let a connection holding only the
/// read-scope (observe) key make this daemon spend the operator's OANDA token, which is 0062's decision 3 and
/// 0097's verdict 3 broken at once.
fn seed_may_reach(lane: HistoryLane) -> bool {
    match lane {
        HistoryLane::Klines => true,
        HistoryLane::TickBars | HistoryLane::Funding | HistoryLane::CredentialedKlines => false,
    }
}

/// Whether a lane's collector reads a CREDENTIAL the operator stored — on the gatewayhub's box,
/// since decision 0164 — what the history-channels read asks before it reports a presence word for
/// a row on that lane (`crate::history`).
///
/// ⚠ **An exhaustive `match`, the [`seed_may_reach`] idiom, so a credentialed lane added later does
/// not COMPILE until somebody has decided it** — and so cannot be reported "no credential needed" by
/// omission, which would tell an operator a lane is ready that refuses every request.
pub(crate) fn needs_a_credential(lane: HistoryLane) -> bool {
    match lane {
        HistoryLane::CredentialedKlines => true,
        HistoryLane::Klines | HistoryLane::TickBars | HistoryLane::Funding => false,
    }
}

/// **Whether the credential one venue's credentialed lane spends is STORED** — a presence word,
/// never a value. `real_backfill_table` builds it with [`gateway_credential_probe`], which asks the
/// gatewayhub, and attaches it with [`BackfillTable::with_credential_probe`], keyed by the very
/// venue string the credentialed row was built under. Called once per history-channels request,
/// uncached, so a key stored or removed after start is what the next answer says.
///
/// ⚠ A std `Box<dyn Fn>` over a type of the client crate below, for [`BackfillFn`]'s reason: the
/// table's own type stays feature-free.
pub type CredentialProbe = Box<dyn Fn() -> CredentialPresence + Send + Sync>;

/// **The presence probe for `venue`'s credentialed lane: a question to the gatewayhub, never a read
/// of a store.** The credential is the gatewayhub's (decision 0164: the OANDA practice token is read
/// there, per connection, and nowhere in this process), so the word comes from
/// `vike_gatewayhub_client::credential::credential_presence`, asked afresh on every call.
///
/// ⚠ **An unreachable gatewayhub answers `NotChecked`, never `Absent`**: nothing was read, so
/// "not stored" would send an operator to store a key they may already have stored. The client
/// function is total (a dead link, a refused scope, a daemon that does not advertise the verb and a
/// reply that is not a presence word are all `NotChecked`), and the mapping below is an exhaustive
/// `match`, so a sixth word on either side stops compiling here rather than being folded into one
/// of the five by a wildcard.
pub fn gateway_credential_probe(link: GatewayLink, venue: &str) -> CredentialProbe {
    use vike_gatewayhub_client::credential::CredentialPresence as Asked;
    let venue = venue.to_string();
    Box::new(move || match vike_gatewayhub_client::credential::credential_presence(&link, &venue) {
        Asked::NotNeeded => CredentialPresence::NotNeeded,
        Asked::Present => CredentialPresence::Present,
        Asked::Absent => CredentialPresence::Absent,
        Asked::Unreadable => CredentialPresence::Unreadable,
        Asked::NotChecked => CredentialPresence::NotChecked,
    })
}

/// The venue → collector dispatch table `serve_with_backfill` mounts, one entry per
/// `(venue, lane)`. Order is the declared order — it is what the unknown-venue errors print — and
/// for the real table it is decided by the rosters it folds: the bar lane's `KLINE_VENUES` and the
/// funding lane's `FUNDING_VENUES` (the gatewayhub client's), whose own declaration orders this
/// constructor preserves.
pub struct BackfillTable {
    entries: Vec<(String, HistoryLane, BackfillFn)>,
    /// The `Backfill` requests running right now — the module doc's last section.
    running: Running,
    /// One presence probe per venue whose credentialed lane is mounted — what the history-channels
    /// read reports for that lane's row. A venue without one reads `NotChecked`.
    credential_probes: Vec<(String, CredentialProbe)>,
}

impl BackfillTable {
    /// A table of KLINE entries — the test seam, unchanged for every existing caller. Production
    /// code goes through `real_backfill_table` instead, so a fake can never be mounted by
    /// accident: this constructor takes closures, and the only closures naming real collectors
    /// live there.
    pub fn new(entries: Vec<(String, BackfillFn)>) -> Self {
        Self {
            entries: entries.into_iter().map(|(v, f)| (v, HistoryLane::Klines, f)).collect(),
            running: Running::default(),
            credential_probes: Vec::new(),
        }
    }

    /// Add one entry on `lane`.
    pub fn with(mut self, venue: &str, lane: HistoryLane, collect: BackfillFn) -> Self {
        self.entries.push((venue.to_string(), lane, collect));
        self
    }

    /// Attach the presence probe for `venue`'s credentialed lane — see [`CredentialProbe`]. A later
    /// probe for the same venue replaces the earlier one.
    pub fn with_credential_probe(mut self, venue: &str, probe: CredentialProbe) -> Self {
        self.credential_probes.retain(|(v, _)| v != venue);
        self.credential_probes.push((venue.to_string(), probe));
        self
    }

    /// **Whether this table carries an entry for `venue` on `lane`** — the history-channels read's
    /// `mounted` answer for a built row. A lookup over the entries; no collector is called.
    pub fn mounts(&self, venue: &str, lane: HistoryLane) -> bool {
        self.entries.iter().any(|(v, l, _)| v == venue && *l == lane)
    }

    /// **The presence word for the credential `venue`'s row on `lane` reads.** A lane that reads none
    /// is `NotNeeded` ([`needs_a_credential`]); one that does asks its venue's probe, per call, and
    /// is `NotChecked` when it has none — never `Absent`, because nothing was read. No collector is
    /// called and no token is read: the probe answers a word.
    pub fn credential_presence(&self, venue: &str, lane: HistoryLane) -> CredentialPresence {
        if !needs_a_credential(lane) {
            return CredentialPresence::NotNeeded;
        }
        self.credential_probes
            .iter()
            .find(|(v, _)| v == venue)
            .map_or(CredentialPresence::NotChecked, |(_, probe)| probe())
    }

    /// The venues that answer a BAR interval (klines, tick-resampled bars and credentialed klines),
    /// in declared order — the unknown-venue error's set.
    pub fn supported(&self) -> Vec<&str> {
        self.venues_where(|lane| lane != HistoryLane::Funding)
    }

    /// The venues with a funding-rate source, in declared order.
    pub fn funding_supported(&self) -> Vec<&str> {
        self.venues_where(|lane| lane == HistoryLane::Funding)
    }

    /// The venues the chart seed may reach — keyless klines only (`seed_may_reach`).
    pub fn seed_supported(&self) -> Vec<&str> {
        self.venues_where(seed_may_reach)
    }

    /// Whether any funding lane is mounted — what `backfill_funding` advertises.
    pub fn has_funding(&self) -> bool {
        !self.funding_supported().is_empty()
    }

    /// The entry for a `Backfill` request: the funding lane when `interval` is
    /// `vike_data::source::FUNDING_INTERVAL`, otherwise the venue's bar lane.
    pub fn get(&self, venue: &str, interval: &str) -> Option<&BackfillFn> {
        self.get_with_lane(venue, interval).map(|(_, f)| f)
    }

    /// [`Self::get`], with the LANE the entry runs on — what `crate::server`'s `backfill_verb`
    /// registers the request under, so the registry knows whether a cancel can stop it.
    pub(crate) fn get_with_lane(
        &self,
        venue: &str,
        interval: &str,
    ) -> Option<(HistoryLane, &BackfillFn)> {
        let funding = interval == vike_data::source::FUNDING_INTERVAL;
        self.entries
            .iter()
            .find(|(v, lane, _)| v == venue && (*lane == HistoryLane::Funding) == funding)
            .map(|(_, lane, f)| (*lane, f))
    }

    /// **Register one `Backfill` request as RUNNING**, for as long as the returned [`Registration`]
    /// lives — `crate::server`'s `backfill_verb` holds it across the collector call, and its `Drop`
    /// removes the entry on every way out of that call, a panic included.
    ///
    /// The request gets the next id this process has not used. `window` is the request's own
    /// inclusive `(start, end)`, `peer` its connection's peer as the server sees it, and `lane` the
    /// lane [`Self::get_with_lane`] found it on.
    pub(crate) fn register(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        (start, end): (i64, i64),
        peer: Option<SocketAddr>,
        lane: HistoryLane,
    ) -> Registration<'_> {
        let mut state = self.running.lock();
        // The id is drawn UNDER the lock that orders the list, so the list stays in id order — the
        // order `ListBackfills` promises — however two connections race to register.
        state.next_id += 1;
        let request = Arc::new(RunningRequest {
            id: state.next_id,
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start,
            end,
            peer,
            lane,
            started_ms: vike_model::now_ms(),
            started: Instant::now(),
            cancelled: AtomicBool::new(false),
            boundaries: AtomicU64::new(0),
        });
        state.requests.push(Arc::clone(&request));
        Registration { running: &self.running, request }
    }

    /// Every request running right now, in the order they began — `ListBackfills`' answer.
    pub(crate) fn running(&self) -> Vec<RunningBackfill> {
        let now = Instant::now();
        self.running.lock().requests.iter().map(|r| r.listed(now)).collect()
    }

    /// **Raise the cancel flag of every running request on `venue`/`symbol`/`interval` whose lane
    /// can stop**, and say which it flagged and which it could not — `CancelBackfill`'s answer. It
    /// does NOT wait: each flagged request stops at its next chunk boundary, on its own thread.
    ///
    /// The series is matched EXACTLY, on the three strings the request carried. A request on a
    /// one-batch lane (`stops_at_a_chunk_boundary` answers `false`) is reported under `unstoppable`
    /// and its flag is NOT raised: it never asks the probe, so a raised flag would read as a stop
    /// that is never coming. A flag already raised is raised again and reported again — the verb is
    /// idempotent.
    pub(crate) fn cancel(&self, venue: &str, symbol: &str, interval: &str) -> BackfillCancelDone {
        let now = Instant::now();
        let state = self.running.lock();
        let mut done = BackfillCancelDone::default();
        for request in state
            .requests
            .iter()
            .filter(|r| r.venue == venue && r.symbol == symbol && r.interval == interval)
        {
            if stops_at_a_chunk_boundary(request.lane) {
                request.cancelled.store(true, Ordering::Release);
                done.flagged.push(request.listed(now));
            } else {
                done.unstoppable.push(request.listed(now));
            }
        }
        done
    }

    /// The entry the chart seed may use: keyless klines only (`seed_may_reach`), so opening a
    /// chart never starts a tick download, a funding fetch or a credentialed fetch.
    pub fn get_for_seed(&self, venue: &str) -> Option<&BackfillFn> {
        self.entries
            .iter()
            .find(|(v, lane, _)| v == venue && seed_may_reach(*lane))
            .map(|(_, _, f)| f)
    }

    fn venues_where(&self, keep: impl Fn(HistoryLane) -> bool) -> Vec<&str> {
        self.entries.iter().filter(|(_, lane, _)| keep(*lane)).map(|(v, _, _)| v.as_str()).collect()
    }
}

impl std::fmt::Debug for BackfillTable {
    /// Venue names only — the closures have nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackfillTable")
            .field("supported", &self.supported())
            .field("funding", &self.funding_supported())
            .finish()
    }
}

/// The registry of running requests a [`BackfillTable`] keeps — the module doc's last section
/// carries the lock argument.
#[derive(Default)]
struct Running {
    state: Mutex<RunningState>,
}

/// What the registry's one lock guards.
#[derive(Default)]
struct RunningState {
    /// The last id handed out; ids start at 1 and are never reused in a process.
    next_id: u64,
    /// Every request running now, in id order.
    requests: Vec<Arc<RunningRequest>>,
}

impl Running {
    /// The registry's lock, recovered if a holder panicked — see the module doc for why that is
    /// sound here, and why it must not panic: a [`Registration`] takes it inside `Drop`, which may
    /// run during an unwind.
    fn lock(&self) -> MutexGuard<'_, RunningState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One `Backfill` request the registry holds while its collector runs.
///
/// Its fields are what the request carried plus counters the server keeps — and nothing more,
/// because a read-scope (observe) key reads them (`docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`,
/// verdict 2, whose reopener is a field beyond that).
pub(crate) struct RunningRequest {
    id: u64,
    venue: String,
    symbol: String,
    interval: String,
    start: i64,
    end: i64,
    peer: Option<SocketAddr>,
    lane: HistoryLane,
    /// Wall clock at registration, for the listing.
    started_ms: i64,
    /// Monotonic clock at registration, for the listing's elapsed time.
    started: Instant,
    /// Raised by an operator's cancel and never lowered — the half of the stop probe that is not
    /// the connection's.
    cancelled: AtomicBool,
    /// How many times the collector has asked its stop probe — one per chunk begun.
    boundaries: AtomicU64,
}

impl RunningRequest {
    /// Whether an operator's cancel has been raised on this request. Read WITHOUT the registry's
    /// lock, through the request's own handle — once per chunk boundary, by the collector's thread.
    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Count one chunk boundary: the collector is asking its stop probe.
    pub(crate) fn reached_a_boundary(&self) {
        self.boundaries.fetch_add(1, Ordering::Relaxed);
    }

    /// This request as the wire lists it, its elapsed time measured to `now`.
    fn listed(&self, now: Instant) -> RunningBackfill {
        RunningBackfill {
            id: self.id,
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            interval: self.interval.clone(),
            start: self.start,
            end: self.end,
            peer: self.peer.map(|p| p.to_string()),
            started_ms: self.started_ms,
            elapsed_ms: u64::try_from(now.saturating_duration_since(self.started).as_millis())
                .unwrap_or(u64::MAX),
            lane: self.lane.name().to_string(),
            stoppable: stops_at_a_chunk_boundary(self.lane),
            cancelled: self.cancelled(),
            boundaries: self.boundaries.load(Ordering::Relaxed),
        }
    }
}

/// **A request's place in the registry, held for as long as its collector runs** — dropping it
/// removes the entry.
///
/// ⚠ **The removal is in `Drop`, and that placement is the whole point of the type** —
/// `crate::server`'s `ConnSlot` idiom, and `crate::md::SessionGuard`'s. A removal written as the last
/// statement after the collector call is correct on every ordinary return and LEAKS the entry when
/// the collector panics: the connection thread unwinds and dies, and the registry would list a
/// request nobody is running, forever, with a cancel that flags nothing that will ever stop. `Drop`
/// runs on the unwind too, and the lock it takes recovers from poison rather than panicking again.
pub(crate) struct Registration<'t> {
    running: &'t Running,
    request: Arc<RunningRequest>,
}

impl Registration<'_> {
    /// The registered request — the handle the stop probe reads the cancel flag through.
    pub(crate) fn request(&self) -> &RunningRequest {
        &self.request
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let id = self.request.id;
        self.running.lock().requests.retain(|r| r.id != id);
    }
}

/// The PRODUCTION table: **every venue-direct collector this daemon can dispatch**, each writing
/// through `store`, the SAME `Arc<DataFusionHist>` the caller serves, and each fetching through
/// `remote` — the link to the gatewayhub, the one daemon that makes every venue call
/// (docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md). This process
/// links no venue bridge for it and reads no venue credential.
///
/// ⚠ **It names NO venue, and that is 0059 Phase 3's whole point.** This was six hand-written
/// closures naming six `backfill_<venue>_klines` functions — the second of two independent copies
/// of one roster, the other being the supervisor's own `COLLECTORS` table (a since-deleted
/// `supervisor/registry.rs`), in a crate that cannot see this one. Both collapsed into one kline
/// registry — since docs/decisions/0094 a static of THIS file, and since decision 0164 the
/// gatewayhub's (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`, with
/// `FUNDING_SOURCES` beside it) — and this constructor FOLDS the client's copy of their ORDER
/// (`KLINE_VENUES`, `FUNDING_VENUES`): adding a venue costs one impl
/// in the venue's own bridge crate, one row in that static and one name in the client's roster,
/// nothing else here. What the fold could not do, while the supervisor lived (docs/decisions/0094
/// deleted it), was silently disagree with it about which fetcher a venue gets — the class of bug
/// the two copies made possible.
///
/// ⚠ **ORDER IS OBSERVABLE.** It is what the datahub's unknown-venue refusal prints
/// (`BackfillTable::supported` preserves it) and it is the order of
/// `vike_gatewayhub_client::history::KLINE_VENUES`, which the daemon's own registry must equal in
/// order (a test in the gatewayhub pins that). `crates/vike-datahub/tests/backfill_roundtrip.rs`'s
/// `the_real_table_names_every_kline_venue` pins the rendered order here. This is the collectors'
/// own order — the three that shipped first, then the three 0059 Phase 2 dispatched — and a reorder
/// of either roster is a behaviour change, not an edit.
///
/// ⚠ **It named three of six until 0059 Phase 2**, while six collectors were written. Aster and
/// deribit already matched the dispatch shape and were reachable from nothing at all; hyperliquid
/// needed a one-symbol adapter. Nothing in the tree could see that, because every test near this
/// table walked from a row OUTWARD; `crates/vike-ops/tests/venues/collector_dispatch_gate.rs` is the walk
/// in the other direction and now compares the written collector modules to the ONE registry.
///
/// ⚠ **The per-venue spellings this comment used to argue about are GONE with the closures** — the
/// crate-root re-exports for binance/bybit/okx versus module paths for the other three, and
/// hyperliquid naming its `_by_symbol` ADAPTER rather than the collector proper. The adapter's
/// reason survives and has simply moved inside the seam: a one-symbol dispatch can only express
/// `coin == symbol`, which is right for every HL perp and WRONG for HL spot, so
/// the hyperliquid bridge's `HyperliquidKlines::fetch` (`crates/bridges/hyperliquid/src/feeds/history.rs`) REFUSES those spellings rather than
/// guessing a coin, fetching the wrong book and spending the commit key that would make a
/// corrective re-fetch a silent zero-row success. (The refusal now crosses the hop as
/// `SourceError::Refused` with its text unchanged.)
///
/// ⚠ **`BackfillTable`'s own TYPE stays feature-free** — the fold happens here, inside
/// `backfill-serve`, and produces the same feature-free entries it always did (a venue string and
/// a `Box<dyn Fn…>`, now tagged with a [`HistoryLane`]). No
/// `vike_backfill` name reaches the struct's signature, so `server.rs` still dispatches through it
/// on every build and a default server's dependency tree is still collector-free. Folding a
/// `Box<dyn KlineSource>` into the table's type instead would have undone that, which is the one
/// way an otherwise-correct version of this refactor goes wrong.
///
/// ORDER is `KLINE_VENUES`' declared order, then the two hand-written rows (dukascopy's, then
/// oanda's), which is what the unknown-venue error prints — see the ORDER note above, and
/// `tests/backfill_roundtrip.rs`'s `the_real_table_names_every_kline_venue`, which pins the rendered
/// list. (It said the `Welcome` frame advertises this order too; `crate::server`'s
/// `served_features` carries no venue list at all.)
///
/// ⚠ **A source is built PER REQUEST, and holds one connection for it.** Every row builds its
/// `vike_gatewayhub_client::history` source inside the collector, so a request is one lazily dialled
/// connection to the gatewayhub (a request whose every day is stored dials nothing) and the daemon's
/// per-connection state — the OANDA token's read-once memo — lives exactly as long as the request.
/// A source shared across requests would keep that state alive between them.
///
/// ⚠ **The stop probe is NOT handed to a remote fetch.** The in-process rows never gave one to the
/// venue call: a one-batch row ignores the probe and a chunked row hands it to the ingest, which
/// asks it between chunks and never inside one. The remote sources could stop a fetch that is in
/// flight (`RemoteTicks::fetcher`, `RemoteKlines::with_stop`), but a cancel that lands inside a
/// fetch would then surface as that chunk's fetch ERROR where the ingest says `stopped:` — a change
/// of wording and of behaviour in a path an operator reads (`CancelBackfill`), which this rewiring
/// does not make.
///
/// Behind `backfill-serve`: this feature is what turns on the store-touching ingest
/// (`vike-backfill`); a default or plain `datafusion-serve` build carries none of it. Every row takes
/// the plain pager, whose internal paging/pacing is already venue-safe (the daemon's); the
/// `_paced` twins and their per-process pace file belonged to the one-shot CLI programs, which
/// docs/decisions/0094 deleted.
///
/// ⚠ **The FUNDING lane is folded here too**, from `FUNDING_VENUES`, and it is what replaced the
/// `funding_rate_backfill` program (docs/decisions/0094): each row becomes a
/// [`HistoryLane::Funding`] entry, ingested by `vike_backfill::funding_rate::backfill_funding_rate`
/// through `store` under the reserved `interval=funding` label. Those entries answer only a request
/// whose interval IS that label, so they appear in [`BackfillTable::funding_supported`] and in
/// neither [`BackfillTable::supported`] nor the chart seed's set.
///
/// ⚠ **The TICK lane is ONE HAND-WRITTEN ROW, not a fold — the row before the last.** Dukascopy
/// carries no `KlineSource` impl to fold a registry over: its fetch is already `(symbol, start_ms,
/// end_ms) -> Vec<QuoteTick>`, with no one-symbol seam left to adapt
/// (`crates/vike-ops/tests/venues/collector_dispatch_gate.rs`'s `written_kline_collectors` doc says why).
/// So its [`HistoryLane::TickBars`] entry composes the gatewayhub's tick fetch
/// (`vike_gatewayhub_client::history::RemoteTicks`, which the daemon answers with the keyless `.bi5`
/// fetch) with `vike_backfill::venues::dukascopy::backfill_quotes_then_bars` (store the ticks as
/// quotes, then resample to `interval` bars) directly — the way every kline venue's row did before
/// the registry existed to fold. It answers a `Backfill` request naming dukascopy at any BAR interval (never the reserved
/// `funding` label); [`BackfillTable::get_for_seed`] still restricts the chart seed to
/// [`HistoryLane::Klines`], so a chart open still cannot reach it.
///
/// ⚠ **…and the CREDENTIALED lane is the row after it, hand-written for the same kind of reason and
/// one more.** OANDA's source DOES implement `KlineSource` (the daemon's, behind its
/// `FetchCredentialed` verb), but it spends a credential, so it cannot be a keyless roster entry —
/// and it must not be: a roster row is a [`HistoryLane::Klines`] entry, the lane the chart seed
/// reaches, and a chart open must never make the daemon spend the operator's token
/// (docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md,
/// verdict 3). So it is ONE [`HistoryLane::CredentialedKlines`] row built by
/// [`credentialed_klines_row`] over `vike_gatewayhub_client::history::RemoteKlines::credentialed`:
/// it answers a `Backfill` for oanda at a bar interval, it is in [`BackfillTable::supported`]
/// (appended after dukascopy — the order the unknown-venue refusal prints), and it is in neither
/// [`BackfillTable::seed_supported`] nor the reach of [`BackfillTable::get_for_seed`].
/// `crates/vike-ops/tests/venues/collector_dispatch_gate.rs`'s `CREDENTIALED_ROWS` is where the one
/// venue allowed a hand-written kline row is declared.
///
/// ⚠ **The oanda `account` row ADDRESSES the token and arms nothing here.** A credential belongs to
/// its account (decision 0156 item 6), so the daemon finds the token by the row — the ONE active
/// unlabelled oanda account at tier `demo` — and reads its `tier` and `active` for that and nothing
/// else (`crates/vike-gatewayhub/src/rest/oanda.rs`'s `oanda_history_credentials`). What the row
/// arms is ORDERS, through `vike_tradehub_core::make_engine`, and this lane places none. The lane's gates
/// are the key's presence on that account and the `Scope::Write` scope of the verb that reaches it.
#[cfg(feature = "backfill-serve")]
pub fn real_backfill_table(
    store: std::sync::Arc<vike_store::DataFusionHist>,
    remote: GatewayLink,
) -> BackfillTable {
    use vike_gatewayhub_client::history::{
        FUNDING_VENUES, KLINE_VENUES, RemoteFunding, RemoteKlines, RemoteTicks,
    };

    let mut table = BackfillTable::new(
        KLINE_VENUES
            .iter()
            .copied()
            .map(|venue| {
                let store = std::sync::Arc::clone(&store);
                let remote = remote.clone();
                // One fetch and one commit per request: no boundary to stop at, so the stop probe
                // is not asked (`BackfillFn`'s doc).
                let collect: BackfillFn = Box::new(move |symbol, interval, start, end, _| {
                    let source = RemoteKlines::new(remote.clone(), venue);
                    vike_backfill::kline_source::backfill_kline_source(
                        &store, &source, symbol, interval, start, end,
                    )
                    .map_err(|e| e.to_string())
                });
                (venue.to_string(), collect)
            })
            .collect(),
    );
    for venue in FUNDING_VENUES {
        let store = std::sync::Arc::clone(&store);
        let remote = remote.clone();
        table = table.with(
            venue,
            HistoryLane::Funding,
            // One batch per request, like the kline fold above: the stop probe is not asked.
            Box::new(
                move |symbol: &str, _interval: &str, start: i64, end: i64, _: &dyn Fn() -> bool| {
                    let source = RemoteFunding::new(remote.clone(), venue);
                    vike_backfill::funding_rate::backfill_funding_rate(
                        &store, &source, symbol, start, end,
                    )
                    .map(|written| {
                        // `BackfillDone` carries one count, the RATE rows; the premium half lands
                        // under its own commit key, so a re-run can write premium rows while
                        // reporting 0 rate rows — the log is where an operator sees both.
                        tracing::info!(
                            "funding {venue}:{symbol} [{start}, {end}]: {} rate rows, {} premium rows",
                            written.rate_rows,
                            written.premium_rows
                        );
                        written.rate_rows
                    })
                    .map_err(|e| e.to_string())
                },
            ),
        );
    }
    let tick_store = std::sync::Arc::clone(&store);
    let tick_remote = remote.clone();
    let table = table.with(
        vike_backfill::venues::dukascopy::VENUE,
        HistoryLane::TickBars,
        // CHUNKED: the request's stop probe goes to the ingest, which asks it between chunks.
        Box::new(
            move |symbol: &str,
                  interval: &str,
                  start: i64,
                  end: i64,
                  should_stop: &dyn Fn() -> bool| {
                let ticks = RemoteTicks::new(tick_remote.clone());
                vike_backfill::venues::dukascopy::backfill_quotes_then_bars(
                    &tick_store,
                    symbol,
                    interval,
                    start,
                    end,
                    should_stop,
                    |sym, from_ms, to_ms| ticks.fetch_quotes_range(sym, from_ms, to_ms, None),
                )
                .map_err(|e| e.to_string())
            },
        ),
    );
    let presence_link = remote.clone();
    let (venue, collect) = credentialed_klines_row(
        store,
        move || RemoteKlines::credentialed(remote.clone(), "oanda"),
        oanda_history_refusal,
    );
    // ...and the history-channels read's PRESENCE probe for that row: a word per request, never a
    // value (decision 0102), asked of the gatewayhub that holds the credential. Keyed by the venue
    // the row was just built under, so the two cannot name different strings.
    table
        .with(&venue, HistoryLane::CredentialedKlines, collect)
        .with_credential_probe(&venue, gateway_credential_probe(presence_link, &venue))
}

/// **One [`HistoryLane::CredentialedKlines`] row: `(venue, collector)` for a `KlineSource` the
/// gatewayhub answers with a credential it holds**, fetched a UTC day at a time through
/// `vike_backfill::kline_source::backfill_kline_source_chunked` once `refuse` has passed the
/// request. [`real_backfill_table`] builds OANDA's from a session-scoped
/// `vike_gatewayhub_client::history::RemoteKlines::credentialed` and `oanda_history_refusal`; a
/// test builds one over a fake source through this SAME body, which is why it takes the source's
/// constructor rather than naming it.
///
/// - **The venue is the SOURCE's own** (`KlineSource::venue`), read once here from an instance that
///   is built and dropped without being fetched from — a remote source dials lazily, so nothing
///   reaches the gatewayhub — the way the roster fold takes each row's. The table key and the store
///   partition the chunked ingest writes under are then the same string by construction.
/// - **`refuse` is asked FIRST**, before a manifest is read, the gatewayhub is dialled or the venue
///   is asked, and whatever it names is a refusal of the whole request. It is for what would be
///   STORED wrongly rather than fetched wrongly — a second spelling of one instrument, a sparse
///   series under a dense label — which is this daemon's to decide: the source returns what the
///   venue serves.
/// - **The chunked ingest, never the per-window one.** A credentialed history is years of
///   sub-minute bars, and `vike_backfill::kline_source::backfill_kline_source_chunked`'s own doc
///   carries why one series must take ONE of the two ingests. The request's stop probe goes with
///   it, so a request whose client has gone stops at the next day boundary rather than running its
///   years out — the incident `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md`
///   was written from was this lane.
/// - ⚠ **A source PER REQUEST, and the request is the SESSION** — docs/decisions/0097's verdict 2 in
///   so many words: the credential is read when a request arrives and dropped when it ends. The
///   read is the gatewayhub's now, and it is made once per CONNECTION: a remote source holds one
///   lazily dialled connection for its life, and the ingest fetches once per day chunk, so a
///   source shared across requests (or a connection per chunk) would read the store once per DAY or
///   keep the token between requests. Each call builds its source from `source_for` instead: the
///   daemon reads the store when the request's first chunk needs a fetch (a request whose days
///   are all stored or unsettled dials nothing and reads nothing), and the source — with its
///   connection, and so the daemon's one copy of the token — is dropped when the call returns. A key
///   stored, rotated or removed between requests is what the next one sees.
///
/// Errors are the ingest's own text, and `refuse`'s under the same `refused: ` prefix a
/// `CollectError::Refused` prints. The two token refusals are the daemon's, carried over the hop as
/// `SourceError::Refused` with their text unchanged, and every string the source composes from the
/// wire is masked of the token before it can become one (`crates/bridges/oanda/src/klines.rs`'s
/// module doc).
#[cfg(feature = "backfill-serve")]
pub fn credentialed_klines_row<S>(
    store: std::sync::Arc<vike_store::DataFusionHist>,
    source_for: impl Fn() -> S + Send + Sync + 'static,
    refuse: fn(&str, &str, i64, i64) -> Option<String>,
) -> (String, BackfillFn)
where
    S: vike_data::source::KlineSource + 'static,
{
    let venue = source_for().venue().to_string();
    let collect: BackfillFn = Box::new(move |symbol, interval, start, end, should_stop| {
        if let Some(why) = refuse(symbol, interval, start, end) {
            return Err(format!("refused: {why}"));
        }
        let source = source_for();
        vike_backfill::kline_source::backfill_kline_source_chunked(
            &store,
            &source,
            symbol,
            interval,
            start,
            end,
            should_stop,
        )
        .map_err(|e| e.to_string())
    });
    (venue, collect)
}

/// The first instant of OANDA's DENSE sub-minute candle series, 2005-01-03T00:00:00Z.
///
/// ⚠ **MEASURED on four instruments, applied to all.** The S5 probe (branch `probe/oanda-s5`,
/// 2026-09-30) found, on EUR_USD, USD_JPY, GBP_USD and AUD_USD, ONE end-of-day candle per trading
/// day under the S5 label from 2002 until the Sunday open of 2005-01-02, and the real 5-second
/// series from that week on. Nothing was measured on another instrument, and none is known to be
/// dense earlier, so the floor is every instrument's. The grid's first whole day of the dense
/// series is the Monday, which is the day a chunked request may start on.
#[cfg(feature = "backfill-serve")]
const OANDA_DENSE_SUBMINUTE_FROM_MS: i64 = 1_104_710_400_000;

/// **OANDA history's two refusals** — `refuse` for [`credentialed_klines_row`], asked before the
/// ingest reads anything. Both are about what would be STORED:
///
/// 1. **The symbol must be OANDA's OWN instrument name** — `EUR_USD`, the spelling the venue's
///    catalog lists. `vike_catalog::to_oanda_instrument` maps `eurusd` and `EURUSD` onto it too, and
///    the store keys a series on the symbol the REQUEST carried, so accepting a second spelling
///    would store a SECOND series of one instrument; the store dedups by commit key and never by row,
///    so nothing downstream would notice. Refused naming the canonical spelling.
/// 2. **A sub-minute step may not start before [`OANDA_DENSE_SUBMINUTE_FROM_MS`].** Before that day
///    the venue answers the S-class granularities with about one candle a DAY, which would be stored
///    as a 5-second series and spend each day's commit key on it — permanently, since no verb retires
///    a key. Refused, not clamped: a clamp would store less than was asked while reporting success.
///    Steps of a minute and more are unmeasured before 2005 and are not refused.
#[cfg(feature = "backfill-serve")]
fn oanda_history_refusal(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    _end_ms: i64,
) -> Option<String> {
    // Echoed back bounded: the symbol is the client's text, and a refusal is not a place to
    // reproduce an arbitrarily long argument — so is the spelling suggested from it.
    let bounded = |s: &str| -> String { s.chars().take(40).collect() };
    let canonical = vike_catalog::to_oanda_instrument(symbol);
    let is_instrument = |s: &str| {
        s.contains('_')
            && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    };
    if symbol != canonical || !is_instrument(symbol) {
        let shown = bounded(symbol);
        let ask = if is_instrument(&canonical) {
            format!("ask for `{}`", bounded(&canonical))
        } else {
            "ask for the instrument by OANDA's own name, like `EUR_USD`".to_string()
        };
        return Some(format!(
            "oanda history stores a series under OANDA's OWN instrument name — the spelling its \
             catalog lists — and {shown:?} is not one: {ask}. A second spelling of one instrument \
             would be stored as a SECOND series of it. Nothing was fetched or written"
        ));
    }
    let sub_minute =
        vike_marketdata::interval::interval_ms(interval).is_some_and(|step| step < 60_000);
    if sub_minute && start_ms < OANDA_DENSE_SUBMINUTE_FROM_MS {
        let from = vike_model::time::utc_rfc3339(OANDA_DENSE_SUBMINUTE_FROM_MS.div_euclid(1000));
        return Some(format!(
            "oanda serves {interval} candles as a DENSE series only from {from}: before that day it \
             answers the sub-minute granularities (5s, 10s, 15s, 30s) with about one candle a DAY, \
             which would be stored as a {interval} series. Refused rather than clamped — start the \
             window at {from} or later. Nothing was fetched or written"
        ));
    }
    None
}

#[path = "backfill_tests.rs"]
#[cfg(test)]
mod backfill_tests;
