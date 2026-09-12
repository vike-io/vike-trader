//! `ReconManager` — the reconciliation runtime driver that ties the pure reconcile engine
//! (`vike_exec::recon`) into the live single-writer core. Phase-3 startup-cadence wiring.
//!
//! **Design (Option A — reports-in, compute-on-fold).** `recon::diff`/`recon::resolve` need the
//! CURRENT local engine state (`LocalView`), which lives inside the `ExecutionEngine` on the core
//! FOLD thread. This manager runs on its OWN thread and does ONLY the blocking REST report fetch
//! (`ReconClient::fetch_*`) — it must never touch the p99<10µs fold. It then enqueues the raw
//! reports as [`vike_exec::Command::ReconcileReports`]; the fold thread reads its own
//! `local_view()`, runs the pure diff/resolve, and folds the synthesized events through the same
//! `on_event` path real venue events use (`runtime::CoreThread::reconcile_reports`). This keeps
//! diff/resolve on the same thread as the state they read — no cross-thread local-state snapshot,
//! no staleness window — and reuses the existing command choke point rather than adding a
//! query/reply lane.
//!
//! **Single-writer invariant.** This thread NEVER mutates engine/`Account` state: it fetches
//! (blocking I/O) and enqueues a `Command`. All state mutation stays on the fold thread. It holds
//! a WEAK ingest sender (like [`crate::runtime::CoreHandle::spawn_periodic_reconcile`]) so it never
//! keeps the core alive — it self-exits when the core is gone or on [`ReconDriver::shutdown`].
//!
//! **Cadence.** A startup pass (one fetch+enqueue per venue after `startup_delay`), PLUS
//! (Task 13) an on-demand pass whenever a venue bridge pokes the driver's reconcile-trigger
//! channel — typically wired to a reconnect (a bridge's `run_resync_supervisor` fires it after
//! its own event-replay settles, complementing rather than replacing that replay: see
//! `vike_bridge_core::user_data::run_resync_supervisor`'s doc for the blind spots neither half
//! covers alone). Both cadences share [`ReconManager::run_startup_pass`] — the trigger loop calls
//! the exact same fetch→diff/resolve pass, it just runs it more than once.
//!
//! **Task 16 (continuous audits).** Two MORE timer arms on the same select loop, both optional
//! and both `None` by default (byte-identical to pre-Task-16 behavior):
//!   - [`ReconConfig::interval`]: re-runs [`ReconManager::run_startup_pass`] on a fixed cadence,
//!     in ADDITION to startup + trigger — same pass, same [`ReconManager::should_reconcile`]
//!     health gate, just fired more often. This is what makes a reconcile pass IDEMPOTENT under
//!     continuous cadence load-bearing rather than merely convenient: a venue trade_id already
//!     folded diffs to zero divergences (`vike_exec::recon::diff` dedups fills by trade_id
//!     against the local engine's `seen_trade_ids`), so a tick that finds nothing new folds
//!     nothing new (see `crates/vike-core/tests/recon/recon_continuous_audit.rs`).
//!   - [`ReconConfig::audit_interval`]: a lighter DELEGATED tick — see
//!     [`ReconManager::run_audit_tick`] — that does NOT run a reconcile pass at all. It pokes the
//!     core's own pre-existing stuck-order watchdog waker (`Ingest::Watchdog`, already spawned by
//!     `runtime::spawn_core` whenever `CoreConfig::submit_ack_timeout` is set) rather than
//!     reimplementing any sweep logic here.
//!
//! The internal-book-vs-venue-book audit (comparing local positions against venue-reported
//! positions) is explicitly OUT of scope for a separate mechanism: [`ReconManager::run_startup_pass`]
//! already diffs `PositionStatusReport`s against local state on every pass (`Divergence::
//! PositionDrift`/`ExternalOnlyPosition` in `vike_exec::recon::diff`) — the interval cadence above
//! is what turns that existing diff into a continuous audit, no new plumbing needed.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::WeakSender;

use vike_exec::recon::{BalanceTol, MassStatus, ReconClient, ReconPolicy};
use vike_exec::{Command, Ingest, ReconcileReports};

use crate::runtime::CoreHandle;

/// Neutral outage classification the reconcile driver gates on. Deliberately NOT
/// `vike_bridge_core::stream_health::StreamHealth`/`ConnectivityProbe` — `vike-core` depends on
/// `vike-exec` + `vike-model` only (down-only layering) and must not pull in `vike-bridge-core`.
/// This enum is the same generic-boundary trick [`ReconDriver::reconcile_trigger`] (Task 13) used
/// for its trigger channel: the app root, which DOES see both crates, is the one place that maps
/// the real bridge health signal onto this closure (see [`ReconConfig::health`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconHealth {
    /// Local network + venue feed both look connected — reconcile passes run normally.
    Healthy,
    /// Local network down OR a venue feed is mid-gap (resyncing) — reconcile passes are
    /// suppressed for this poll/poke so a blip doesn't fire a reconcile storm against reports
    /// the venue itself hasn't caught up on yet.
    Degraded,
}

/// A per-venue reconcile health probe (`venue -> ReconHealth`), wired by the app root to each
/// venue's real feed health. Factored into an alias so the `ReconConfig`/`ReconManager` `health`
/// fields don't trip `clippy::type_complexity`.
pub type HealthProbe = Arc<dyn Fn(&str) -> ReconHealth + Send + Sync>;

/// Reconcile driver configuration. `policy`, `lookback_ms`, `startup_delay`, `health`, `interval`,
/// `audit_interval`, and `generate_missing_orders` are all live.
pub struct ReconConfig {
    /// Fold-vs-quarantine policy per divergence kind (default = Synthesize everything).
    pub policy: ReconPolicy,
    /// How far back (ms) to request order/fill reports at pass time (`since = now - lookback_ms`).
    pub lookback_ms: i64,
    /// Delay before the startup pass runs (0 = immediately).
    pub startup_delay: Duration,
    /// Task 15: optional PER-VENUE health probe consulted before each venue's leg of a pass. The
    /// `&str` is the venue being reconciled — so a bybit-disconnected moment suppresses only bybit's
    /// leg while binance's still runs (the earlier binance-only gate let an unhealthy venue reconcile
    /// as long as binance was up). `None` = always [`ReconHealth::Healthy`] — byte-identical to
    /// pre-Task-15 behavior. Wired by the app root to each venue's real feed health (`StreamHealth`/
    /// `ConnectivityProbe`), which `vike-core` cannot name directly (see [`ReconHealth`]'s doc); an
    /// unknown venue should map to `Healthy` so an un-gated venue is never blocked.
    pub health: Option<HealthProbe>,
    /// Task 16: continuous re-reconcile cadence. `Some(d)` re-runs the SAME
    /// [`ReconManager::run_startup_pass`] every `d`, on top of startup + trigger passes. `None`
    /// (default) = no interval timer arm at all — byte-identical to pre-Task-16 behavior.
    pub interval: Option<Duration>,
    /// Synthesize adoption events for a venue order with no local match
    /// (`vike_exec::recon::DivergenceKind::UnknownOrder`), instead of `resolve`'s default empty
    /// catch-all for that kind. Threaded verbatim into each pass's [`vike_exec::ReconcileReports`]
    /// (the fold thread reads `ReconConfig` via that payload, not this struct directly — see
    /// `ReconManager::run_startup_pass`). `false` (default) is byte-identical to before this flag
    /// existed: under `hybrid`, `UnknownOrder` still quarantines with EMPTY proposed events, so an
    /// operator confirm has nothing to adopt.
    ///
    /// `true` enables the NARROW adoption semantics of `vike_exec::recon::resolve`'s
    /// `AdoptContext` (its module doc is the authority): only a TERMINAL unknown order with
    /// executed qty and no fill report in the same pass synthesizes anything that folds (a
    /// decorative accept + one cumulative `Fill`, deterministic `EXT-ORD-*` trade_id); a live or
    /// fill-lane-covered unknown order folds NOTHING (its executions arrive as `MissingFill`
    /// divergences with real venue trade-ids), surfacing only a dedup-keyed held alert under
    /// `hybrid`/`quarantine` for operator visibility/adoption. Once adopted, recurring passes are
    /// a true no-op (no events, no counter drift, no new alert rows).
    pub generate_missing_orders: bool,
    /// Task 16: periodic in-flight-timeout audit cadence. `Some(d)` pokes the core's existing
    /// stuck-order watchdog waker every `d` (see [`ReconManager::run_audit_tick`] — DELEGATED, not
    /// a reimplemented sweep). `None` (default) = no audit timer arm.
    pub audit_interval: Option<Duration>,
    /// Feature 2 (`VIKE_RECONCILE_BALANCE`): promote venue balance from a silent authoritative
    /// overwrite to a first-class DIFFED dimension. Threaded verbatim into each pass's
    /// [`vike_exec::ReconcileReports`] (the fold thread reads `ReconConfig` only through that
    /// payload — see [`ReconManager::run_startup_pass`]). `false` (default) is byte-identical to
    /// before this flag: `reconcile_reports` takes the legacy silent seed. `true` ⇒ venue cash is
    /// diffed against the realized-PnL-corrected local balance and any drift routed through
    /// `policy` (quarantined by default — a surprise cash move is never auto-folded).
    pub reconcile_balance: bool,
    /// Feature 2 money tolerance for the first-class cash reconcile diff — the abs/rel bands
    /// `vike_exec::recon::diff_balance` compares against (only consulted when `reconcile_balance` is
    /// `true`). Env-tuned per deployment via `VIKE_RECONCILE_BALANCE_TOL_{ABS,REL}` (see
    /// `vike-app`'s `reconcile_config::build_recon_config`); [`BalanceTol::default`] (the
    /// conservative constant) when unset. Threaded verbatim into each pass's
    /// [`vike_exec::ReconcileReports`] alongside `reconcile_balance` (the fold thread reads it only
    /// through that payload — see [`ReconManager::run_startup_pass`]).
    pub balance_tol: BalanceTol,
}

impl fmt::Debug for ReconConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReconConfig")
            .field("policy", &self.policy)
            .field("lookback_ms", &self.lookback_ms)
            .field("startup_delay", &self.startup_delay)
            .field("health", &self.health.as_ref().map(|_| "<fn>"))
            .field("interval", &self.interval)
            .field("generate_missing_orders", &self.generate_missing_orders)
            .field("audit_interval", &self.audit_interval)
            .field("reconcile_balance", &self.reconcile_balance)
            .field("balance_tol", &self.balance_tol)
            .finish()
    }
}

impl Default for ReconConfig {
    fn default() -> Self {
        ReconConfig {
            policy: ReconPolicy::default(),
            lookback_ms: 24 * 60 * 60 * 1_000, // one day
            startup_delay: Duration::from_secs(0),
            health: None,
            interval: None,
            generate_missing_orders: false,
            audit_interval: None,
            reconcile_balance: false,
            balance_tol: BalanceTol::default(),
        }
    }
}

/// The reconcile manager: owns the per-venue [`ReconClient`]s + config + a WEAK ingest sender.
/// Lives on the driver thread; its only side effect is enqueuing [`Command::ReconcileReports`].
pub struct ReconManager {
    clients: Vec<(String, Box<dyn ReconClient>)>,
    config: ReconConfig,
    ingest: WeakSender<Ingest>,
    /// Task 15: mirrors `config.health` (hoisted onto the struct so [`ReconManager::should_reconcile`]
    /// doesn't need to reach through `config` — same shape as `ingest`/`clients`).
    health: Option<HealthProbe>,
    /// **The staleness escalation's whole state** (2026-09-11). Per venue, in `clients` order.
    liveness: Vec<VenueLiveness>,
}

/// What this manager remembers about ONE venue's reconcile leg between passes — the escalation
/// half of the 2026-09-10 incident.
///
/// ⚠ **The cure for the health-gate latch was a WRITE-PATH fix in the bridges; this is the answer
/// to the OTHER half of the incident, which is that nothing escalated.** The per-minute
/// `tracing::warn!` below was the entire signal for 42 hours — 2,516 identical lines, and the
/// failure-notify unit fires only on unit failure, which never happened. One line repeated 2,516
/// times is indistinguishable from noise; nothing reads it.
///
/// ⚠ **It keys on ELAPSED TIME since this venue's last successful pass, NOT on a consecutive-
/// suppression count, and the difference is coverage.** A suppression counter gives the WITHHELD
/// venues zero coverage — ig/oanda/deribit/alpaca/ctrader are absent from
/// `vike_tradehub::feeds`'s `LiveFeeds::recon_feed_statuses`, read `Healthy` in
/// `build_recon_config`'s closure, and are therefore NEVER suppressed, so a suppression threshold
/// could never fire for them. It also misses the second silent death entirely:
/// [`ReconManager::run_startup_pass`] treats a `fetch_mass_status` error exactly as it treats
/// suppression — `warn!` and `continue`, no fault, no counter. Elapsed-since-last-success covers
/// suppression, fetch failure, and a venue silently dropped from `clients`, with ONE primitive.
///
/// Time rather than count for a second reason: a count's wall-clock meaning moves with
/// `VIKE_RECONCILE_INTERVAL_MS`, and the operator-facing quantity is "how stale is the wallet
/// figure I am reading", not "how many ticks were missed".
#[derive(Debug, Clone)]
struct VenueLiveness {
    venue: String,
    /// Wall-clock ms of the last pass that actually ENQUEUED reports for this venue.
    ///
    /// ⚠ `None` until the driver's FIRST pass, which seeds it — the baseline is deliberately "when
    /// this driver first ran", not "when it was constructed". Two reasons, and the first is the
    /// real one: a deployment with a long `startup_delay` must not have that delay counted against
    /// every venue as staleness. The second is that it keeps this file's ambient-clock read count
    /// where `crates/vike-ops/tests/clock_pin.rs`'s `the_clock_ratchet_has_a_non_empty_input`
    /// pinned it — a constructor-time `now_ms()` would be an eighth read, and that ratchet's
    /// failure message reads an eighth as evidence its comment stripping broke.
    ///
    /// ⚠ It is the ENQUEUE, not the FOLD — the honest bound of what this thread can observe. The
    /// manager holds a weak ingest sender and never sees the fold thread's outcome, so a core that
    /// accepted the command and then failed to fold it would not be caught here. That gap is what
    /// `CoreSnapshot.recon.last_pass_ts` answers from the other side; making it PER VENUE is the
    /// declared follow-up on [`ReconManager::escalate_stale_venues`].
    last_pass_ms: Option<i64>,
    /// Wall-clock ms of the last pass that read this venue's authoritative BALANCE — `Ok(Some(_))`
    /// out of [`ReconClient::fetch_balance`], and only while [`ReconConfig::reconcile_balance`] is
    /// on.
    ///
    /// ⚠ **A SECOND clock, because the first one cannot see the incident's headline loss.**
    /// [`ReconManager::run_pass_at`] stamps `last_pass_ms` as soon as `fetch_mass_status` succeeds
    /// and the command is enqueued; the balance is read one line earlier and its `Err` is
    /// swallowed. So a venue whose orders/fills/positions reconcile perfectly while its balance
    /// endpoint 401s keeps a permanently fresh liveness clock and escalates NOTHING — and "the
    /// wallet figure has been frozen at an identical value across all 2,209 summaries" is exactly
    /// what the CI box reported. `reconcile_balance` is explicitly enabled on that box.
    ///
    /// ⚠ `None` means NO BALANCE CLAIM IS MADE for this venue, and that is load-bearing rather
    /// than a not-yet-seeded sentinel: `ReconClient::fetch_balance` defaults to `Ok(None)` for a
    /// venue that does not surface a balance at all, so a venue that has never returned a figure
    /// must never go "balance-stale" — there is nothing it was supposed to report. The clock is
    /// therefore armed by the FIRST `Ok(Some(_))` and judged only after that. The accepted
    /// residual: a venue that reported a balance and then starts answering `Ok(None)` is
    /// indistinguishable from one that stopped being wired, and this escalates it. That direction
    /// is the right one — the wallet genuinely stopped being re-read.
    last_balance_ms: Option<i64>,
    /// Why this venue's leg did not run on the previous pass, or `None` if it ran.
    ///
    /// ⚠ **A REASON, not a bool, and the bool was a defect.** The first round of this escalation
    /// edge-triggered on `degraded: bool`, which two different causes shared — health suppression
    /// and a `fetch_mass_status` failure. So: pass 1 the feed reads Degraded, the flag sets and one
    /// WARN lands; pass 2 the feed recovers but the venue's REST answers 401, the fetch arm finds
    /// the flag already true and logs NOTHING. The operator never sees the 401, and the only
    /// per-occurrence diagnostic either arm carries — the error's own message — is the thing that
    /// went missing. Before that round, a failing fetch was logged every pass, so it was a
    /// REGRESSION in the arm that was meant to be widened.
    ///
    /// Deduping on the reason TEXT instead is the idiom the bridges adopted in the same fix
    /// (`crates/bridges/bybit/src/market_feed.rs`'s `FeedCtx::set_status` emits on a text change,
    /// never on a flag), and it bounds the chattiness honestly: a cause whose message varies every
    /// pass logs every pass, which is exactly what this code did BEFORE the edge trigger and is
    /// strictly better than a flag that hides the cause. The repetition problem the incident
    /// actually had was the SUPPRESSION line, whose text is a constant.
    degraded: Option<String>,
    /// Wall-clock ms of the last ERROR-level staleness escalation, so the reminder decays instead
    /// of repeating per pass. 0 = never escalated.
    last_escalation_ms: i64,
}

/// The suppression reason — a constant, so the edge-triggered WARN fires ONCE per outage however
/// long it lasts. (The 2,516 identical the CI box lines were this one.)
const SUPPRESSED_REASON: &str = "venue health probe reports Degraded";

/// Lower bound of the staleness threshold — see [`staleness_threshold`].
const STALENESS_FLOOR: Duration = Duration::from_secs(5 * 60);
/// Upper bound of the staleness threshold — see [`staleness_threshold`].
const STALENESS_CEILING: Duration = Duration::from_secs(30 * 60);
/// How many missed passes the threshold allows before it fires — see [`staleness_threshold`].
const STALENESS_INTERVALS: u32 = 10;
/// How often an already-escalated venue repeats its ERROR line. Decaying rather than per-pass:
/// the incident's lesson is that a line repeated every 60 s for 42 hours is not a signal.
const ESCALATION_REMINDER: Duration = Duration::from_secs(60 * 60);

/// How stale a venue's reconcile leg may get before it is an ERROR rather than a WARN:
/// `clamp(10 x interval, 5 min, 30 min)`, i.e. **10 minutes on the shipped 60 s cadence**.
///
/// Chosen against the incident rather than picked round: a balance figure an operator would
/// tolerate being ten minutes stale and would NOT tolerate being 42 hours stale. The clamp exists
/// because the multiplier alone would make a 1 s debug cadence fire in ten seconds and a one-hour
/// cadence take ten hours.
///
/// ⚠ **This threshold keys on RECONCILE-PASS age and must never be transplanted onto FEED
/// freshness.** Polymarket's measured floors are 300 s (book) and 1800 s (trades), tuned from a
/// 2026-07-11 sample where quiet books went 8-25 minutes between updates; ig's `pump_spec` row
/// carries a 60 s idle threshold sized around TLCP probe frames on a CLOSED market, because FX
/// closes every weekend. A later version keyed on "time since last healthy feed signal" would fire
/// every Saturday on ig and across every quiet Polymarket book. A reconcile pass is something THIS
/// PROCESS does on a cadence it controls, which is why age is meaningful here and not there.
fn staleness_threshold(interval: Option<Duration>) -> Duration {
    // `saturating_mul`, not `*`: `VIKE_RECONCILE_INTERVAL_MS` is operator-supplied and a plain
    // multiply PANICS on overflow. A panic here would take down the reconcile driver thread of a
    // live daemon over a mistyped setting, and the clamp below makes the saturated value
    // indistinguishable from any other large one.
    let base = interval.unwrap_or(Duration::from_secs(60)).saturating_mul(STALENESS_INTERVALS);
    base.clamp(STALENESS_FLOOR, STALENESS_CEILING)
}

impl ReconManager {
    /// Task 15 gate, consulted before EACH VENUE's leg of a pass (startup, trigger-driven, AND Task
    /// 16's interval-driven — see [`ReconManager::run_startup_pass`]). `true` (reconcile this venue)
    /// when no probe is wired (`health: None` — back-compat, byte-identical to pre-Task-15 behavior)
    /// or the probe reports [`ReconHealth::Healthy`] for `venue`; `false` (suppress this venue's leg)
    /// when it reports [`ReconHealth::Degraded`] (that venue's feed mid-gap, or local network down).
    /// Suppression DEFERS this venue's leg only — it sets no pending flag and never blocks a DIFFERENT
    /// healthy venue in the same pass, so a Degraded venue simply isn't reconciled until the next
    /// poke lands while it reads Healthy.
    ///
    /// ⚠ An ASSOCIATED function over the probe rather than a `&self` method: the pass now mutates
    /// per-venue liveness state in the same loop, so a `&self` call would borrow the whole struct
    /// and conflict with it. Taking the one field it actually reads is the narrower borrow and
    /// says so at the signature.
    fn should_reconcile(health: &Option<HealthProbe>, venue: &str) -> bool {
        match health {
            None => true,
            Some(probe) => probe(venue) == ReconHealth::Healthy,
        }
    }

    /// One reconcile pass: for each venue client, blocking-fetch the atomic mass-status bundle
    /// (orders + positions + fills via [`ReconClient::fetch_mass_status`], whose default composes
    /// the same three per-report fetches) and enqueue it for the fold thread to diff/resolve.
    /// Blocking REST runs HERE (never the fold). A fetch
    /// error skips that venue with a pass-boundary warning; a dropped core (weak upgrade / send
    /// fails) ends the pass. Called once at startup, again (Task 13) on every reconcile-trigger
    /// poke, and again (Task 16) on every [`ReconConfig::interval`] tick — always the SAME pass
    /// over ALL configured venues, not just the one that reconnected (simplest correct behavior; a
    /// per-venue-targeted pass is out of scope if ever needed). Idempotent by construction: a
    /// report the fold thread has already seen (fill trade_id already in `seen_trade_ids`, order
    /// already terminal, position already matching) diffs to zero divergences, so a redundant pass
    /// folds nothing new (see `tests/recon_continuous_audit.rs`). Task 15: gated PER VENUE by
    /// [`ReconManager::should_reconcile`] — a Degraded probe suppresses only THAT venue's leg (no
    /// fetch, no enqueue for it), so a healthy venue in the same pass still reconciles.
    fn run_startup_pass(&mut self, stop: &AtomicBool) {
        self.run_pass_at(stop, vike_model::clock::now_ms());
    }

    /// [`Self::run_startup_pass`] with the wall clock injected, so the staleness escalation is
    /// testable without sleeping through a ten-minute threshold.
    fn run_pass_at(&mut self, stop: &AtomicBool, now_ms: i64) {
        // Wall clock: reconcile `since` bounds are venue REST timestamps, so a wall clock is correct
        // here (unlike the core's injectable fold clock).
        let since = (now_ms - self.config.lookback_ms).max(0);
        // Split the borrow by FIELD: the loop reads `clients`/`config`/`ingest`/`health` while
        // mutating `liveness`, and a `&self` method call would borrow the whole struct.
        let ReconManager { clients, config, ingest, health, liveness } = self;
        for (idx, (venue, client)) in clients.iter().enumerate() {
            if stop.load(Ordering::Relaxed) || ingest.upgrade().is_none() {
                return;
            }
            // Per-venue health gate: skip only THIS venue's leg when its feed is Degraded; other
            // venues in the same pass are unaffected.
            if !Self::should_reconcile(health, venue) {
                // ⚠ EDGE-TRIGGERED, and that is the incident's second lesson. This line used to
                // fire on EVERY pass: on the CI box it produced 2,516 identical copies over 42 hours,
                // which buried the fact rather than reporting it. Entering suppression speaks once;
                // leaving it speaks once (below); the staleness escalation is what says the
                // condition PERSISTS. Same rule `crate::runtime::recon_held`'s `warn_newly_held`
                // already learned here for a different repetition.
                //
                // ⚠ The edge is on the REASON, not on a bool — see `VenueLiveness::degraded`. A
                // bool shared with the fetch arm below swallowed the 401 of a venue that recovered
                // its feed and lost its REST in the same window.
                if liveness[idx].degraded.as_deref() != Some(SUPPRESSED_REASON) {
                    liveness[idx].degraded = Some(SUPPRESSED_REASON.to_string());
                    tracing::warn!(
                        target: "vike_core::reconcile",
                        venue = %venue,
                        "reconcile leg suppressed: {SUPPRESSED_REASON}"
                    );
                }
                continue;
            }
            // Atomic mass-status snapshot (the Nautilus `ExecutionMassStatus` graft): one bundled
            // fetch of orders + positions + fills. The default `fetch_mass_status` composes the same
            // three per-report fetches in the same order (orders → fills → positions), so this is
            // byte-identical to the pre-seam per-report fetch for every current venue; a venue with a
            // real single-call snapshot endpoint may override it for cross-report consistency. A
            // fetch error still skips only THIS venue's leg with a pass-boundary warning, exactly as
            // the per-report fetches did.
            let MassStatus { orders, positions, fills } = match client.fetch_mass_status(since) {
                Ok(v) => v,
                Err(e) => {
                    // ⚠ Edge-triggered for the SAME reason as the suppression line above, and this
                    // is the arm that makes an elapsed-time escalation strictly wider than a
                    // suppression counter: a venue whose fetch fails every pass is exactly as
                    // silently dead as a suppressed one, and carried no counter at all before now.
                    //
                    // ⚠ On the REASON TEXT, which here VARIES with the error. That is deliberate:
                    // `{e}` is the only per-occurrence diagnostic this arm carries, so a cause that
                    // changes (401 → timeout → 500) must be reportable, and a suppression that
                    // preceded it must not swallow the first one. `VenueLiveness::degraded` argues
                    // the bound.
                    let reason = format!("mass-status report fetch failed: {e}");
                    if liveness[idx].degraded.as_deref() != Some(reason.as_str()) {
                        tracing::warn!(target: "vike_core::reconcile", venue = %venue, "{reason}");
                        liveness[idx].degraded = Some(reason);
                    }
                    continue;
                }
            };
            // Task 3: the venue's authoritative cash. A failure is swallowed here rather than
            // skipping the whole pass — a venue that doesn't report balance still gets its
            // order/fill/position divergences diffed and folded, it just leaves `Account` balance
            // untouched.
            //
            // ⚠ **It used to be swallowed SILENTLY, and this was the one fetch in the pass with no
            // log at all.** `Err` was collapsed into `None` by `unwrap_or(None)`, which is also
            // what a venue that doesn't surface a balance returns, so the two were
            // indistinguishable downstream and neither reached an operator. The incident's
            // headline loss was a wallet figure frozen across 2,209 summaries, so a balance fetch
            // that fails silently is precisely the shape that must not stay silent. Same
            // reason-keyed edge as the two arms above, so a permanently-401ing balance endpoint
            // says so once rather than once a minute.
            //
            // The RESIDUAL is carried to the end of the leg rather than written here: this half
            // failing does not stop the pass (orders/fills/positions still fold), so the
            // recovery bookkeeping below owns the transition and this arm only reports.
            let (balance, balance_reason) = match client.fetch_balance() {
                Ok(b) => {
                    // `Ok(Some(_))` ARMS and advances the balance clock; `Ok(None)` is "this venue
                    // does not report one" and must never arm it (see `last_balance_ms`).
                    if b.is_some() && config.reconcile_balance {
                        liveness[idx].last_balance_ms = Some(now_ms);
                    }
                    (b, None)
                }
                Err(e) => {
                    let reason = format!("balance fetch failed: {e}");
                    if liveness[idx].degraded.as_deref() != Some(reason.as_str()) {
                        tracing::warn!(target: "vike_core::reconcile", venue = %venue, "{reason}");
                    }
                    (None, Some(reason))
                }
            };
            // Upgrade at the last moment: a core that exited mid-fetch ends the pass losslessly.
            let Some(tx) = ingest.upgrade() else { return };
            let reports = ReconcileReports {
                venue: venue.clone(),
                since,
                orders,
                fills,
                positions,
                policy: config.policy.clone(),
                balance,
                generate_missing_orders: config.generate_missing_orders,
                reconcile_balance: config.reconcile_balance,
                balance_tol: config.balance_tol,
                // `None` = "this venue's sole account", which is the truth for every client this
                // workspace can build: `self.clients` is keyed by CANONICAL venue (the same key
                // `should_reconcile` probes for feed health above, and the only key a per-venue
                // health map has), and `vike_run::build_node` mounts at most one `ReconClient` per
                // venue. A second account of one exchange would need its own client HERE, carrying
                // its own route key — that is the one edit that turns this into `Some`, and the
                // fold thread already knows what to do with it (`ReconcileReports::route`).
                route_key: None,
            };
            // Plain std thread (no tokio runtime) → blocking_send is legal and lossless.
            if tx
                .blocking_send(Ingest::Command(Command::ReconcileReports(Box::new(reports))))
                .is_err()
            {
                return; // core exited
            }
            // This venue's leg RAN. Clear the degraded edge (announcing the recovery, since a
            // divergence going away is a fact too) and stamp the liveness clock the escalation
            // reads.
            //
            // ⚠ A pass whose BALANCE half failed is not a full recovery: the reports folded, so
            // `last_pass_ms` advances and no `resumed` is claimed, and `balance_reason` becomes the
            // carried state so the next pass's balance arm dedups against it rather than warning
            // again. `last_balance_ms` deliberately does NOT advance — that is what makes the
            // escalation see a wallet that stopped being re-read behind a healthy order leg.
            let prior = liveness[idx].degraded.take();
            if prior.is_some() && balance_reason.is_none() {
                tracing::info!(
                    target: "vike_core::reconcile",
                    venue = %venue,
                    stale_ms = liveness[idx].last_pass_ms.map(|t| now_ms - t).unwrap_or(0),
                    prior = %prior.as_deref().unwrap_or(""),
                    "reconcile leg resumed"
                );
            }
            liveness[idx].degraded = balance_reason;
            liveness[idx].last_pass_ms = Some(now_ms);
            // ⚠ The escalation reminder is re-armed by [`Self::escalate_stale_venues`] when NOTHING
            // is late, not here. Clearing it on any successful pass would make a venue whose
            // BALANCE is stale behind a healthy order leg re-fire its ERROR every single pass —
            // the 2,516-line flood one severity up, which is the failure this whole escalation
            // exists to replace.
        }
        self.escalate_stale_venues(now_ms);
    }

    /// **THE ESCALATION.** A venue whose reconcile leg has not run for longer than
    /// [`staleness_threshold`] gets an `tracing::error!`, once, then a decaying reminder every
    /// [`ESCALATION_REMINDER`].
    ///
    /// ⚠ **TWO CLOCKS, not one, and the second is the one that sees the incident's headline loss.**
    /// The leg clock ([`VenueLiveness::last_pass_ms`]) answers "did this venue reconcile at all";
    /// the balance clock ([`VenueLiveness::last_balance_ms`]) answers "was its authoritative cash
    /// re-read". They come apart because `fetch_balance`'s failure does not stop a pass: the
    /// orders/fills/positions still fold and the leg clock still advances. the CI box's loss was
    /// reported as a wallet figure frozen across 2,209 summaries, so a one-clock escalation would
    /// have been silent about exactly that shape (it caught the actual 2026-09-10 incident only
    /// because the WHOLE leg was suppressed). Each ERROR names which claim went stale and carries
    /// the last reported `cause` — the reason string [`VenueLiveness::degraded`] holds — so the
    /// escalation is not a bare age with no diagnosis.
    ///
    /// ⚠ **Why an ERROR line and not `CoreSnapshot.fault`, which is the obvious-looking carrier.**
    /// That field's doc reads "set once a handler panicked — the core is HALTED in safe-state", and
    /// both of its setters prove it: `crate::runtime::watchdog`'s `enter_safe_state` sets `fault`,
    /// sets `TradingState::Halted`, halts every extra engine and cancels every working order with
    /// `CancelIntent::RiskOff`; `crate::runtime::publish`'s `publish_guarded` does the same on a
    /// publish panic. Writing a stale-reconcile escalation there would either HALT THE DAEMON AND
    /// PULL ITS BOOK over a health-gate false positive — the exact inverse of
    /// `vike_ops::reconcile_config`'s fail-soft asymmetry, and this incident's false positive is
    /// precisely what would have triggered it — or, set without `enter_safe_state`, publish
    /// `fault: "…"` beside `trading_state: "Active"` and break the invariant every reader of that
    /// field relies on.
    ///
    /// ⚠ **What is DEFERRED, and it is the consumer half.** This escalation is a JOURNAL line —
    /// visible to `journalctl -p err` and to any unit-level notifier, which is more than the 2,516
    /// WARNs offered and is deliberately the cheapest thing that is not nothing. What it is NOT is
    /// a channel-delivered alert, and TWO pieces of that are blocked rather than merely unbuilt:
    ///
    /// 1. `crate::snapshot::ReconBlock` should gain `last_pass_by_venue: Vec<(String, i64)>`,
    ///    written where `recon_last_pass_ts` is written today. That scalar is a single GLOBAL
    ///    written by whichever venue folded last, so on a multi-venue mount one healthy venue keeps
    ///    it fresh while another goes dark forever — a pre-existing defect this field would also
    ///    fix. It cannot land here: `ReconBlock` is constructed as an EXHAUSTIVE struct literal by
    ///    `crates/vike-alerting/src/eval.rs`'s
    ///    `recon_alert_rule_fires_on_presence_edge_with_optional_kind_filter`, so adding a field
    ///    reddens a crate PR #1752 currently owns. (`crate::snapshot::CoreSnapshot` is no escape —
    ///    `crates/vike-app-core/src/observe_bridge.rs`'s `wire_to_core` literals that one
    ///    exhaustively, and that crate is a sibling branch's too.)
    /// 2. `crates/vike-tradehub/src/tradehub_cli.rs`'s `summary_line` should then gain
    ///    `recon_stale_venues` and `recon_oldest_pass_age_ms`, computed from that map and placed
    ///    BESIDE `fault`, never inside it. It has nothing to read until (1) lands.
    ///
    /// And the alerting RULE itself is a third: `crates/vike-alerting/src/rule.rs` has exactly one
    /// recon rule and it keys on `snap.recon.alerts` PRESENCE, so pass-staleness is a new rule
    /// shape in that same crate. **Sequencing: (1) must land before the rule, or the rule has
    /// nothing to read.**
    ///
    /// ⚠ **Not covered, stated so it is not claimed.** Nothing here escalates a venue that was
    /// never in `clients` at all — an unarmed venue has no leg to be stale. That is the arming
    /// banner's job (`vike_mount::venue_arming_migration`), not this counter's. And this runs at
    /// the END of an attempted PASS, so a deployment with `VIKE_RECONCILE_INTERVAL_MS=0` (the
    /// interval arm disabled entirely) evaluates staleness only when a trigger poke lands. That is
    /// the honest scope rather than a gap: with no cadence there is no expected pass for a venue to
    /// be late for, and adding a second timer to judge a driver that was asked not to run would be
    /// a different feature.
    fn escalate_stale_venues(&mut self, now_ms: i64) {
        let threshold = staleness_threshold(self.config.interval).as_millis() as i64;
        let balance_claimed = self.config.reconcile_balance;
        for row in &mut self.liveness {
            // The FIRST pass establishes this venue's baseline and never escalates on it: before
            // it there is no expected pass for the venue to be late for, and a `0` sentinel would
            // make every venue read 55 years stale on the driver's first tick.
            let Some(last) = row.last_pass_ms else {
                row.last_pass_ms = Some(now_ms);
                continue;
            };
            let pass_age = now_ms - last;
            // ⚠ **TWO CLOCKS, because the leg clock cannot see the incident's headline loss.** The
            // BALANCE clock is judged only when this deployment reconciles balances at all AND this
            // venue has proven it reports one (`VenueLiveness::last_balance_ms` argues both gates);
            // otherwise `None` and no balance claim is made. Without it a venue whose orders, fills
            // and positions reconcile perfectly while its balance endpoint 401s keeps a permanently
            // fresh `last_pass_ms` and escalates nothing — which is exactly the state the CI box
            // reported as "the wallet figure frozen at an identical value across all 2,209
            // summaries".
            let balance_age =
                if balance_claimed { row.last_balance_ms.map(|t| now_ms - t) } else { None };
            let stale_pass = pass_age >= threshold;
            let stale_balance = balance_age.is_some_and(|a| a >= threshold);
            if !stale_pass && !stale_balance {
                // Nothing is late. Re-arm the reminder HERE rather than on any successful pass, so
                // a venue that is half-healthy (leg running, balance stale) keeps its hourly decay
                // instead of re-escalating every minute.
                row.last_escalation_ms = 0;
                continue;
            }
            let due = row.last_escalation_ms == 0
                || now_ms - row.last_escalation_ms >= ESCALATION_REMINDER.as_millis() as i64;
            if !due {
                continue;
            }
            row.last_escalation_ms = now_ms;
            if stale_pass {
                tracing::error!(
                    target: "vike_core::reconcile",
                    venue = %row.venue,
                    stale_ms = pass_age,
                    threshold_ms = threshold,
                    cause = %row.degraded.as_deref().unwrap_or("unknown"),
                    "reconcile leg has not run for longer than its staleness threshold — this \
                     venue's orders, fills, positions and authoritative balance are UNCHECKED \
                     against the exchange"
                );
            } else {
                tracing::error!(
                    target: "vike_core::reconcile",
                    venue = %row.venue,
                    stale_ms = balance_age.unwrap_or(0),
                    threshold_ms = threshold,
                    cause = %row.degraded.as_deref().unwrap_or("unknown"),
                    "reconcile legs are RUNNING but this venue's authoritative BALANCE has not \
                     been re-read for longer than the staleness threshold — the equity the \
                     pre-trade margin lane judges against is the last figure the venue gave"
                );
            }
        }
    }

    /// Task 16 [`ReconConfig::audit_interval`] tick — DELEGATED, not a reimplemented sweep. This
    /// does NOT run a reconcile pass and does NOT itself decide anything is stuck: it enqueues
    /// [`Ingest::Watchdog`], the same message `runtime::spawn_core`'s own `vt-core-watchdog`
    /// thread already injects on a cadence whenever `CoreConfig::submit_ack_timeout` is
    /// configured. `Ingest::Watchdog` is a pure WAKER on the fold thread (`Ingest::Watchdog =>
    /// {}` in the dispatch match) — its only effect is making the core reach its drain-loop
    /// boundary sooner, where [`crate::runtime`]'s `DeadlineTimerWheel` runs the existing
    /// `sweep_stuck_orders` if-and-when it is actually due. So: with `submit_ack_timeout` unset
    /// (the default), this poke is inert — this manager has no visibility into `CoreConfig` (and
    /// must not; down-only layering) and does not need any to stay correct. Best-effort: a failed
    /// upgrade/send is silently dropped, same as any other poke on the weak ingest sender.
    fn run_audit_tick(&self) {
        if let Some(tx) = self.ingest.upgrade() {
            let _ = tx.blocking_send(Ingest::Watchdog);
        }
    }
}

/// Spawn the reconcile runtime driver. After `config.startup_delay`, runs ONE startup pass
/// (fetch + enqueue per venue) on a dedicated `vt-core-recon` thread; the thread then stays alive,
/// selecting between the stop flag, the Task-13 reconcile-trigger channel (`ReconDriver::trigger`/
/// [`ReconDriver::reconcile_trigger`]), and (Task 16) the `interval`/`audit_interval` timer arms
/// below, so an external poke (typically a venue reconnect) OR the interval cadence re-runs the
/// SAME startup pass. It self-exits either on [`ReconDriver::shutdown`] or once the core is gone
/// (the weak ingest sender fails to upgrade) — checked on every poll tick, not only inside a pass,
/// so an un-shutdown driver never leaks a thread past the core's lifetime. Holds a WEAK ingest
/// sender throughout, so it never keeps the core alive.
///
/// **Reconciliation-activation Task 7: adopting an external trigger channel.** `trigger` lets a
/// caller pre-create the `on_reconcile` `mpsc` pair BEFORE this driver exists and hand `Sender`
/// clones to venue bridges at spawn time. This matters because the app root builds every live
/// venue's exec client (and its `run_resync_supervisor` call, deep inside) well before
/// `spawn_core_multi`/`spawn_recon` run — see `vike-app/src/main.rs`'s `App::new`, where
/// `make_engine` executes long before this function is called. `Some((tx, rx))` ADOPTS the given
/// pair verbatim: `rx` drives this driver's trigger-poll loop below and `tx` seeds
/// `ReconDriver.trigger`, so [`ReconDriver::reconcile_trigger`] keeps handing out clones of the
/// SAME channel the caller already threaded into venues, rather than a second, disconnected one.
/// `None` (every pre-Task-7 call site) reproduces the original behavior byte-for-byte: this
/// function mints its own private pair, exactly as before.
pub fn spawn_recon(
    handle: &CoreHandle,
    clients: Vec<(String, Box<dyn ReconClient>)>,
    config: ReconConfig,
    trigger: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> ReconDriver {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let (trigger_tx, trigger_rx) = trigger.unwrap_or_else(mpsc::channel);
    // WEAK like spawn_periodic_reconcile: must NOT keep the ingest channel open, or the core's
    // "every strong sender dropped -> clean break" exit would never fire.
    let health = config.health.clone();
    // `last_pass_ms: None` — the staleness baseline is seeded by the FIRST pass, not here; see
    // `VenueLiveness::last_pass_ms`. `last_balance_ms: None` is stronger than a baseline: it means
    // NO balance claim is made for this venue until it has actually returned one, so a venue that
    // reports no balance at all can never go balance-stale.
    let liveness = clients
        .iter()
        .map(|(v, _)| VenueLiveness {
            venue: v.clone(),
            last_pass_ms: None,
            last_balance_ms: None,
            degraded: None,
            last_escalation_ms: 0,
        })
        .collect();
    // `mut` because the pass now carries per-venue liveness state. Verified safe without interior
    // mutability: this value is constructed ONCE and moved into the driver thread below, so it has
    // exactly one owner for its whole life.
    let mut mgr =
        ReconManager { clients, config, ingest: handle.ingest.downgrade(), health, liveness };
    let thread = std::thread::Builder::new()
        .name("vt-core-recon".into())
        .spawn(move || {
            if sleep_watching(&stop_t, mgr.config.startup_delay) {
                return; // stopped (or core gone) during the startup delay
            }
            mgr.run_startup_pass(&stop_t);
            // Task 13: react to on-demand pokes (a venue reconnect, typically) with the SAME pass —
            // no separate logic. Task 16: TWO more timer arms on this same select loop, both `None`
            // by default (each `next_*` stays `None`, so `wait` below is always exactly
            // `select_poll` and every branch below is a no-op for that arm — byte-identical to
            // pre-Task-16 behavior when neither `interval` nor `audit_interval` is configured).
            // Deadlines reschedule relative to `Instant::now()` at each fire (not the previous
            // deadline), so a late tick (thread briefly descheduled) resumes the cadence from
            // "now" instead of bursting catch-up passes.
            let select_poll = Duration::from_millis(50);
            let mut next_interval = mgr.config.interval.map(|d| Instant::now() + d);
            let mut next_audit = mgr.config.audit_interval.map(|d| Instant::now() + d);
            while !stop_t.load(Ordering::Relaxed) {
                let now = Instant::now();
                let mut wait = select_poll;
                if let Some(deadline) = next_interval {
                    wait = wait.min(deadline.saturating_duration_since(now));
                }
                if let Some(deadline) = next_audit {
                    wait = wait.min(deadline.saturating_duration_since(now));
                }
                match trigger_rx.recv_timeout(wait) {
                    Ok(()) => {
                        // Coalesce a burst of pokes (e.g. several venues reconnecting together)
                        // into one pass rather than one per poke.
                        while trigger_rx.try_recv().is_ok() {}
                        mgr.run_startup_pass(&stop_t);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if mgr.ingest.upgrade().is_none() {
                            return; // core gone — self-exit like the startup-pass path
                        }
                        let now = Instant::now();
                        if let (Some(deadline), Some(d)) = (next_interval, mgr.config.interval)
                            && now >= deadline
                        {
                            mgr.run_startup_pass(&stop_t);
                            next_interval = Some(now + d);
                        }
                        if let (Some(deadline), Some(d)) = (next_audit, mgr.config.audit_interval)
                            && now >= deadline
                        {
                            mgr.run_audit_tick();
                            next_audit = Some(now + d);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return, // every trigger handle dropped
                }
            }
        })
        .expect("spawn vt-core recon driver");
    ReconDriver { stop, trigger: trigger_tx, handle: thread }
}

/// Sleep for `dur`, polling `stop` in short slices so shutdown is observed promptly. Returns true
/// if `stop` was raised (caller should exit). A zero duration returns immediately.
fn sleep_watching(stop: &AtomicBool, dur: Duration) -> bool {
    if dur.is_zero() {
        return stop.load(Ordering::Relaxed);
    }
    let poll = dur.min(Duration::from_millis(50)).max(Duration::from_millis(1));
    let deadline = Instant::now() + dur;
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Join half of the reconcile driver. Mirrors [`crate::runtime::ReconcileDriver`]: raise stop, join
/// the thread. The driver self-exits once the core is gone (checked on every trigger-poll tick,
/// not only inside a pass), so an explicit shutdown is optional in that case — dropping the driver
/// is safe. Absent that, the driver thread stays alive (Task 13: it is listening for reconcile
/// pokes), so a live core's driver should be shut down explicitly when the venue bridges it serves
/// are torn down.
pub struct ReconDriver {
    stop: Arc<AtomicBool>,
    /// Task 13: the reconcile-trigger sender. Cloned out via [`ReconDriver::reconcile_trigger`] and
    /// handed to a venue bridge's `run_resync_supervisor(..., on_reconcile)` so a reconnect can poke
    /// this driver into an on-demand pass. Kept here (not just inside the thread) so the channel
    /// stays open — and thus the trigger loop stays alive — for as long as the `ReconDriver` itself
    /// does; dropping the driver without cloning out a sender lets `Disconnected` end the thread.
    /// Task 7 (reconciliation-activation): this is `trigger_tx` from [`spawn_recon`]'s `trigger`
    /// param when the caller adopted an external pair (main.rs's usual case, so venues already
    /// hold clones of THIS exact sender before the driver ever existed), or a freshly minted one
    /// when `trigger: None` (every pre-Task-7 caller, e.g. this crate's own tests).
    trigger: mpsc::Sender<()>,
    handle: std::thread::JoinHandle<()>,
}

impl ReconDriver {
    /// A clonable handle a venue bridge can pass as `run_resync_supervisor`'s `on_reconcile`
    /// argument: `driver.reconcile_trigger()` per bridge (all venues share one manager thread and
    /// one pass — see the module doc). Sending never blocks (unbounded channel) and a poke after
    /// the driver has shut down is silently dropped.
    pub fn reconcile_trigger(&self) -> mpsc::Sender<()> {
        self.trigger.clone()
    }

    /// Supervisor probe: true once the driver thread has exited (core gone, or after `shutdown`).
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Stop the driver thread and join it. Idempotent with the driver's own self-exit.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

#[cfg(test)]
mod staleness_escalation_tests {
    //! **The escalation half of the 2026-09-10 incident** — see [`VenueLiveness`].
    //!
    //! the CI box's daemon emitted the SAME `tracing::warn!` 2,516 times over 42 hours and escalated
    //! nothing: the failure-notify unit fires only on unit failure, which never happened, and one
    //! line repeated every minute is indistinguishable from noise. These tests drive
    //! [`ReconManager::run_pass_at`] over an injected clock — no threads, no sleeps, no ten-minute
    //! waits.
    //!
    //! ⚠ **Declared residual: these assert the STATE the journal lines are gated on, not the lines
    //! themselves.** `tracing` caches an `Interest` verdict per callsite, process-globally, so a
    //! capturing subscriber inside a shared test binary would race every other test in it (see
    //! `crates/bridges/binance/tests/feed_status_journal.rs`'s module doc for what that costs). The
    //! `degraded` flag IS the `if` the WARN sits inside and `last_escalation_ms` IS the gate on the
    //! ERROR, so asserting them is asserting the emission gate — one indirection short of the
    //! rendered line.

    use super::*;
    use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

    /// A `ReconClient` that answers every fetch with empty reports, or fails them on a SCHEDULE.
    ///
    /// ⚠ The schedule (rather than a bare `fails: bool`) is what lets a test drive one cause after
    /// another through ONE venue — suppression, then a 401, then a timeout. That interaction is the
    /// case the first round of this escalation got wrong: two causes shared one `bool`, so the
    /// second went unlogged.
    struct StubClient {
        /// `report_fails[n]` — does the nth mass-status fetch fail, and with what text? Shorter
        /// than the pass count means "never again"; `always_fails` is the flat form.
        report_fails: Vec<Option<String>>,
        always_fails: bool,
        /// Same shape for [`ReconClient::fetch_balance`]. An entry of `None` is a SUCCESS whose
        /// value is `balance`.
        balance_fails: Vec<Option<String>>,
        balance: Option<f64>,
        /// Which pass this is. `fetch_order_status_reports` is the FIRST call the default
        /// `fetch_mass_status` makes, so it is the pass counter.
        report_calls: std::sync::atomic::AtomicUsize,
        balance_calls: std::sync::atomic::AtomicUsize,
    }

    impl StubClient {
        fn healthy() -> Self {
            StubClient {
                report_fails: Vec::new(),
                always_fails: false,
                balance_fails: Vec::new(),
                balance: None,
                report_calls: std::sync::atomic::AtomicUsize::new(0),
                balance_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn always_failing() -> Self {
            StubClient { always_fails: true, ..Self::healthy() }
        }
    }

    impl ReconClient for StubClient {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<OrderStatusReport>, String> {
            let n = self.report_calls.fetch_add(1, Ordering::Relaxed);
            if self.always_fails {
                return Err("stub fetch failure".into());
            }
            match self.report_fails.get(n).cloned().flatten() {
                Some(e) => Err(e),
                None => Ok(Vec::new()),
            }
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_balance(&self) -> Result<Option<f64>, String> {
            let n = self.balance_calls.fetch_add(1, Ordering::Relaxed);
            match self.balance_fails.get(n).cloned().flatten() {
                Some(e) => Err(e),
                None => Ok(self.balance),
            }
        }
    }

    /// A manager over the given venues, with a probe that reports `Degraded` for anything in
    /// `degraded`. The ingest channel's receiver is returned so the sender stays upgradable — a
    /// dropped receiver would make every pass end early at the `blocking_send`, and the test would
    /// pass for the wrong reason.
    fn manager(
        venues: &[(&str, bool)],
        degraded: &'static [&'static str],
        t0: i64,
    ) -> (ReconManager, tokio::sync::mpsc::Receiver<Ingest>) {
        manager_with(
            venues.iter().map(|(v, fails)| {
                (
                    (*v).to_string(),
                    if *fails { StubClient::always_failing() } else { StubClient::healthy() },
                )
            }),
            degraded,
            t0,
            ReconConfig { interval: Some(Duration::from_secs(60)), ..ReconConfig::default() },
        )
    }

    /// [`manager`] with the clients and config supplied outright — for the tests that need a
    /// SCHEDULE of failures or `reconcile_balance` on.
    fn manager_with(
        clients: impl IntoIterator<Item = (String, StubClient)>,
        degraded: &'static [&'static str],
        t0: i64,
        config: ReconConfig,
    ) -> (ReconManager, tokio::sync::mpsc::Receiver<Ingest>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Ingest>(256);
        let clients: Vec<(String, Box<dyn ReconClient>)> =
            clients.into_iter().map(|(v, c)| (v, Box::new(c) as Box<dyn ReconClient>)).collect();
        let health: HealthProbe = Arc::new(move |venue: &str| {
            if degraded.contains(&venue) { ReconHealth::Degraded } else { ReconHealth::Healthy }
        });
        let liveness = clients
            .iter()
            .map(|(v, _)| VenueLiveness {
                venue: v.clone(),
                last_pass_ms: Some(t0),
                // Armed at the baseline like `last_pass_ms`, so a balance-staleness test measures
                // the clock rather than the arming rule (which has its own test).
                last_balance_ms: Some(t0),
                degraded: None,
                last_escalation_ms: 0,
            })
            .collect();
        let mgr = ReconManager {
            clients,
            config,
            ingest: tx.downgrade(),
            health: Some(health),
            liveness,
        };
        // Leak the strong sender for the test's lifetime so the weak one upgrades.
        std::mem::forget(tx);
        (mgr, rx)
    }

    fn row<'a>(mgr: &'a ReconManager, venue: &str) -> &'a VenueLiveness {
        mgr.liveness.iter().find(|r| r.venue == venue).expect("venue is in the manager")
    }

    /// **(i) THE MULTI-VENUE DEFECT, reproduced and fixed.** A suppressed venue's liveness clock
    /// must NOT advance while a healthy venue's does.
    ///
    /// This is what `CoreSnapshot.recon.last_pass_ts` cannot express: it is a single GLOBAL scalar
    /// written by whichever venue folded last, so on a multi-venue mount one healthy venue keeps it
    /// fresh forever while another goes dark. On the CI box's single-venue mount it WOULD have gone
    /// stale for 42 hours — the signal existed and had no consumer.
    #[test]
    fn a_suppressed_venue_goes_stale_while_a_healthy_one_stays_fresh() {
        let t0 = 1_000_000;
        let (mut mgr, _rx) = manager(&[("bybit", false), ("binance", false)], &["bybit"], t0);
        let stop = AtomicBool::new(false);

        for k in 1..=5 {
            mgr.run_pass_at(&stop, t0 + k * 60_000);
        }

        assert_eq!(
            row(&mgr, "bybit").last_pass_ms,
            Some(t0),
            "the suppressed venue's leg never ran, so its clock must not have moved"
        );
        assert_eq!(
            row(&mgr, "binance").last_pass_ms,
            Some(t0 + 5 * 60_000),
            "a healthy venue in the SAME pass reconciles normally — suppression is per-venue"
        );
    }

    /// **(ii) THE WARN IS EDGE-TRIGGERED.** Entering suppression sets the gate once; every later
    /// suppressed pass finds it already set and says nothing. On the CI box the ungated form produced
    /// 2,516 identical lines.
    #[test]
    fn the_suppression_warning_is_gated_on_the_edge_not_the_pass() {
        let t0 = 1_000_000;
        let (mut mgr, _rx) = manager(&[("bybit", false)], &["bybit"], t0);
        let stop = AtomicBool::new(false);

        assert!(row(&mgr, "bybit").degraded.is_none(), "nothing is degraded before the first pass");
        mgr.run_pass_at(&stop, t0 + 60_000);
        assert!(
            row(&mgr, "bybit").degraded.is_some(),
            "the first suppressed pass is the edge, and speaks"
        );
        for k in 2..=50 {
            mgr.run_pass_at(&stop, t0 + k * 60_000);
        }
        assert!(
            row(&mgr, "bybit").degraded.is_some(),
            "…and stays set, so the WARN's `if` is false on all 49 later passes"
        );
    }

    /// A FETCH FAILURE is the same silent death as suppression, and carried no counter at all
    /// before this change — which is most of why an elapsed-time escalation is strictly wider than
    /// a suppression count. The venue reads `Healthy`, is never suppressed, and still never
    /// reconciles.
    #[test]
    fn a_venue_whose_fetch_always_fails_goes_stale_too() {
        let t0 = 1_000_000;
        let (mut mgr, _rx) = manager(&[("okx", true)], &[], t0);
        let stop = AtomicBool::new(false);

        for k in 1..=5 {
            mgr.run_pass_at(&stop, t0 + k * 60_000);
        }
        assert_eq!(row(&mgr, "okx").last_pass_ms, Some(t0), "a failing fetch enqueues nothing");
        assert!(row(&mgr, "okx").degraded.is_some(), "…and it is on the same edge-triggered gate");
    }

    /// **SUPPRESSION THEN A FETCH FAILURE — the interaction a shared `bool` swallowed.**
    ///
    /// The first round of this escalation edge-triggered both arms on one `degraded: bool`, so this
    /// sequence lost the fetch error's message entirely: pass 1 suppressed (flag set, one WARN),
    /// pass 2 the feed recovers but the venue's REST answers 401 — the fetch arm found the flag
    /// already true and logged NOTHING, while `last_pass_ms` still did not advance. The operator
    /// saw a suppression that had ended and never saw the 401 that replaced it. Before that round
    /// the 401 was logged EVERY pass, so it was a regression in the arm being widened.
    ///
    /// Asserting the REASON (the state the WARN's `if` reads) rather than the rendered line, per
    /// this module's declared residual.
    #[test]
    fn a_fetch_failure_after_a_suppression_is_still_reportable() {
        let t0 = 1_000_000;
        // ⚠ The schedule is indexed by CALL, not by pass, and pass 1 never calls the fetch at all
        // (the probe suppresses it before the client is touched). So these three entries are
        // passes 2, 3 and 4 — and getting that wrong would make pass 2 SUCCEED and quietly test
        // nothing, which is why it is spelled out rather than counted in a reviewer's head.
        let client = StubClient {
            report_fails: vec![
                Some("HTTP 401 Unauthorized".into()),
                Some("HTTP 401 Unauthorized".into()),
                Some("connect timed out".into()),
            ],
            ..StubClient::healthy()
        };
        let (mut mgr, _rx) = manager_with(
            [("bybit".to_string(), client)],
            &["bybit"],
            t0,
            ReconConfig { interval: Some(Duration::from_secs(60)), ..ReconConfig::default() },
        );
        let stop = AtomicBool::new(false);

        mgr.run_pass_at(&stop, t0 + 60_000);
        assert_eq!(
            row(&mgr, "bybit").degraded.as_deref(),
            Some(SUPPRESSED_REASON),
            "pass 1: suppressed, and the reason says so"
        );

        // The feed comes back; the venue's REST does not.
        mgr.health = Some(Arc::new(|_: &str| ReconHealth::Healthy));
        mgr.run_pass_at(&stop, t0 + 120_000);
        assert_eq!(
            row(&mgr, "bybit").degraded.as_deref(),
            Some("mass-status report fetch failed: HTTP 401 Unauthorized"),
            "pass 2: a DIFFERENT cause replaced the suppression, so the WARN's `if` is true and \
             the 401 reaches the operator — the exact line a shared bool swallowed"
        );

        // The SAME cause again is silent (this is what the edge trigger is for)…
        mgr.run_pass_at(&stop, t0 + 180_000);
        assert_eq!(
            row(&mgr, "bybit").degraded.as_deref(),
            Some("mass-status report fetch failed: HTTP 401 Unauthorized"),
            "pass 3: an unchanged cause does not re-announce itself"
        );

        // …and a cause that CHANGES is reported again.
        mgr.run_pass_at(&stop, t0 + 240_000);
        assert_eq!(
            row(&mgr, "bybit").degraded.as_deref(),
            Some("mass-status report fetch failed: connect timed out"),
            "pass 4: 401 → timeout is a new fact, and a bool could never have said so"
        );
        assert_eq!(
            row(&mgr, "bybit").last_pass_ms,
            Some(t0),
            "…and through all of it the leg never ran, so the staleness clock never moved"
        );
    }

    /// **THE FROZEN WALLET — a venue whose BALANCE fetch fails while everything else reconciles.**
    ///
    /// `fetch_balance`'s `Err` used to be collapsed into `None` by `unwrap_or(None)`: no log, no
    /// edge, and no effect on the liveness clock. So the leg clock stayed permanently fresh and the
    /// staleness escalation could never fire for the shape the CI box actually reported — "the wallet
    /// figure frozen at an identical value across all 2,209 summaries". Two claims, two clocks.
    #[test]
    fn a_venue_whose_balance_fetch_fails_goes_balance_stale_while_its_leg_stays_fresh() {
        let t0 = 1_000_000;
        let client = StubClient {
            // Every balance fetch fails; the reports all succeed.
            balance_fails: (0..64).map(|_| Some("HTTP 401 Unauthorized".to_string())).collect(),
            balance: Some(1_234.0),
            ..StubClient::healthy()
        };
        let (mut mgr, _rx) = manager_with(
            [("bybit".to_string(), client)],
            &[],
            t0,
            ReconConfig {
                interval: Some(Duration::from_secs(60)),
                reconcile_balance: true,
                ..ReconConfig::default()
            },
        );
        let stop = AtomicBool::new(false);

        mgr.run_pass_at(&stop, t0 + 60_000);
        assert_eq!(
            row(&mgr, "bybit").degraded.as_deref(),
            Some("balance fetch failed: HTTP 401 Unauthorized"),
            "the one fetch in the pass that used to have no log at all now has one"
        );
        assert_eq!(
            row(&mgr, "bybit").last_pass_ms,
            Some(t0 + 60_000),
            "the LEG ran — orders, fills and positions folded, so its clock is honestly fresh"
        );
        assert_eq!(
            row(&mgr, "bybit").last_balance_ms,
            Some(t0),
            "…and the BALANCE clock did not move, which is the whole point of the second one"
        );

        // Ten minutes on, the leg clock is fresh and the balance clock is not: the escalation must
        // fire on the second claim, or the incident's headline loss is invisible to it.
        let at_threshold = t0 + 10 * 60_000;
        for k in 2..=10 {
            mgr.run_pass_at(&stop, t0 + k * 60_000);
        }
        assert_eq!(
            row(&mgr, "bybit").last_escalation_ms,
            at_threshold,
            "a frozen wallet behind a perfectly healthy order leg IS an escalation"
        );

        // …and it decays hourly rather than re-firing on every one of the passes that keep
        // succeeding. Clearing the reminder on any successful pass would make this the 2,516-line
        // flood one severity up.
        for k in 11..=60 {
            mgr.run_pass_at(&stop, t0 + k * 60_000);
        }
        assert_eq!(
            row(&mgr, "bybit").last_escalation_ms,
            at_threshold,
            "the reminder decays even though every pass in between SUCCEEDED"
        );
    }

    /// A venue that reports no balance at all must never go balance-stale: `fetch_balance`'s
    /// default is `Ok(None)` for a venue that does not surface one, so an un-armed clock is
    /// "no claim made", not "not seeded yet". Without this, every exec-only venue would escalate
    /// ten minutes after `reconcile_balance` was switched on.
    #[test]
    fn a_venue_that_reports_no_balance_never_goes_balance_stale() {
        let t0 = 1_000_000;
        let (mut mgr, _rx) = manager_with(
            [("okx".to_string(), StubClient::healthy())], // `balance: None` — the trait default
            &[],
            t0,
            ReconConfig {
                interval: Some(Duration::from_secs(60)),
                reconcile_balance: true,
                ..ReconConfig::default()
            },
        );
        mgr.liveness[0].last_balance_ms = None; // the production seed: no claim yet
        let stop = AtomicBool::new(false);

        for k in 1..=30 {
            mgr.run_pass_at(&stop, t0 + k * 60_000);
        }
        assert_eq!(row(&mgr, "okx").last_balance_ms, None, "nothing ever armed the balance claim");
        assert_eq!(
            row(&mgr, "okx").last_escalation_ms,
            0,
            "…so half an hour of passes escalates nothing — a venue with no balance to report \
             cannot have a stale one"
        );
    }

    /// **(iii) THE ESCALATION FIRES EXACTLY WHEN THE THRESHOLD ELAPSES, AND NOT BEFORE** — then
    /// repeats on a DECAYING cadence, never per pass.
    ///
    /// With the shipped 60 s interval the threshold is ten minutes ([`staleness_threshold`]), so a
    /// pass at T+9 min must not escalate and one at T+10 must.
    #[test]
    fn the_escalation_fires_at_the_threshold_and_then_decays() {
        let t0 = 1_000_000;
        let (mut mgr, _rx) = manager(&[("bybit", false)], &["bybit"], t0);
        let stop = AtomicBool::new(false);

        mgr.run_pass_at(&stop, t0 + 9 * 60_000);
        assert_eq!(
            row(&mgr, "bybit").last_escalation_ms,
            0,
            "nine minutes of staleness is under the ten-minute threshold — a WARN, not an ERROR"
        );

        let at_threshold = t0 + 10 * 60_000;
        mgr.run_pass_at(&stop, at_threshold);
        assert_eq!(
            row(&mgr, "bybit").last_escalation_ms,
            at_threshold,
            "ten minutes stale IS the escalation"
        );

        // …and the next fifty passes (fifty minutes) do NOT re-escalate: the reminder is hourly, so
        // this cannot become the 2,516-line flood one severity up.
        for k in 11..=60 {
            mgr.run_pass_at(&stop, t0 + k * 60_000);
        }
        assert_eq!(
            row(&mgr, "bybit").last_escalation_ms,
            at_threshold,
            "the reminder decays — an escalation repeated every pass is the defect it replaces"
        );

        let past_reminder = t0 + 71 * 60_000;
        mgr.run_pass_at(&stop, past_reminder);
        assert_eq!(
            row(&mgr, "bybit").last_escalation_ms,
            past_reminder,
            "…but it does come back, so a condition nobody fixed keeps saying so"
        );
    }

    /// A recovered venue clears BOTH gates, so the next outage escalates afresh rather than being
    /// swallowed by the previous one's reminder clock.
    #[test]
    fn a_recovered_venue_resets_the_escalation_clock() {
        let t0 = 1_000_000;
        let (mut mgr, _rx) = manager(&[("bybit", false)], &["bybit"], t0);
        let stop = AtomicBool::new(false);
        mgr.run_pass_at(&stop, t0 + 10 * 60_000);
        assert_ne!(row(&mgr, "bybit").last_escalation_ms, 0);

        // Swap in an all-healthy probe — the venue's feed came back.
        mgr.health = Some(Arc::new(|_: &str| ReconHealth::Healthy));
        let recovered_at = t0 + 11 * 60_000;
        mgr.run_pass_at(&stop, recovered_at);

        let r = row(&mgr, "bybit");
        assert_eq!(r.last_pass_ms, Some(recovered_at), "the leg ran");
        assert!(r.degraded.is_none(), "…so the WARN edge is re-armed for the next outage");
        assert_eq!(r.last_escalation_ms, 0, "…and so is the escalation");
    }

    /// The FIRST pass establishes the baseline and never escalates on it — the production shape,
    /// where `spawn_recon` seeds `last_pass_ms: None`.
    ///
    /// Without this, a `0` sentinel would make every venue read ~55 years stale on the driver's
    /// very first tick and escalate the moment the process starts; and seeding at CONSTRUCTION
    /// time instead would count a long `startup_delay` against every venue as staleness (and add an
    /// eighth ambient clock read to this file, which `crates/vike-ops/tests/clock_pin.rs`'s
    /// `the_clock_ratchet_has_a_non_empty_input` pins at seven).
    #[test]
    fn the_first_pass_seeds_the_baseline_and_does_not_escalate() {
        let (mut mgr, _rx) = manager(&[("bybit", false)], &["bybit"], 0);
        mgr.liveness[0].last_pass_ms = None; // the production seed
        let stop = AtomicBool::new(false);

        let t0 = 1_700_000_000_000; // a real wall clock, decades past any 0 sentinel
        mgr.run_pass_at(&stop, t0);
        assert_eq!(
            row(&mgr, "bybit").last_escalation_ms,
            0,
            "a venue with no baseline yet cannot be late — escalating here would fire on every \
             process start"
        );
        assert_eq!(
            row(&mgr, "bybit").last_pass_ms,
            Some(t0),
            "…and that first pass IS the baseline every later staleness judgement is against"
        );

        // …and from that baseline the ordinary threshold applies.
        mgr.run_pass_at(&stop, t0 + 10 * 60_000);
        assert_eq!(row(&mgr, "bybit").last_escalation_ms, t0 + 10 * 60_000);
    }

    /// The threshold's own arithmetic, including both clamp arms — a debug cadence of one second
    /// must not escalate every ten seconds, and an hourly cadence must not take ten hours.
    #[test]
    fn the_staleness_threshold_is_clamped_at_both_ends() {
        assert_eq!(
            staleness_threshold(Some(Duration::from_secs(60))),
            Duration::from_secs(600),
            "the SHIPPED cadence: ten minutes, the number chosen against the incident"
        );
        assert_eq!(staleness_threshold(Some(Duration::from_secs(1))), STALENESS_FLOOR);
        assert_eq!(staleness_threshold(Some(Duration::from_secs(3600))), STALENESS_CEILING);
        // An operator-supplied interval that would OVERFLOW a plain multiply saturates into the
        // ceiling rather than panicking the live daemon's reconcile thread.
        assert_eq!(staleness_threshold(Some(Duration::MAX)), STALENESS_CEILING);
        assert_eq!(
            staleness_threshold(None),
            Duration::from_secs(600),
            "an interval-less driver is still judged on the documented default cadence"
        );
    }
}
