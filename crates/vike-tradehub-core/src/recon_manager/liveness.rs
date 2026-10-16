//! The staleness escalation's per-leg state, its thresholds, and the edge-triggered reason text.

use std::time::Duration;

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
pub(crate) struct VenueLiveness {
    pub(crate) venue: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this row is about** — the leg's route key, or the venue
    /// itself for a sole account. Purely diagnostic: every DECISION in this file keys on `venue`
    /// (the health probe) or on the row's index in `clients` (the liveness bookkeeping).
    ///
    /// ⚠ It exists because the escalation lines below are the operator-facing output of this file
    /// and `venue` alone stopped identifying a row once a venue could have two: two `binance`
    /// legs would emit two ERRORs reading `venue=binance` and nothing would say which book had
    /// gone unchecked. Equal to `venue` on every single-account box, so those lines are unchanged.
    pub(crate) leg_key: String,
    /// Wall-clock ms of the last pass that actually ENQUEUED reports for this venue.
    ///
    /// ⚠ `None` until the driver's FIRST pass, which seeds it — the baseline is deliberately "when
    /// this driver first ran", not "when it was constructed". Two reasons, and the first is the
    /// real one: a deployment with a long `startup_delay` must not have that delay counted against
    /// every venue as staleness. The second is that it keeps the reconcile driver's ambient-clock
    /// read count where `crates/vike-ops/tests/architecture/clock_pin/scanner_tests.rs`'s
    /// `the_clock_ratchet_has_a_non_empty_input` pinned it (six `Instant::now` reads in
    /// `recon_manager/driver.rs`, one `now_ms` in `recon_manager/manager.rs`) — a constructor-time
    /// `now_ms()` would be an eighth read, and that ratchet's failure message reads an extra one as
    /// evidence its comment stripping broke.
    ///
    /// ⚠ It is the ENQUEUE, not the FOLD — the honest bound of what this thread can observe. The
    /// manager holds a weak ingest sender and never sees the fold thread's outcome, so a core that
    /// accepted the command and then failed to fold it would not be caught here. That gap is what
    /// `CoreSnapshot.recon.last_pass_ts` answers from the other side; making it PER VENUE is the
    /// declared follow-up on [`ReconManager::escalate_stale_venues`].
    pub(crate) last_pass_ms: Option<i64>,
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
    pub(crate) last_balance_ms: Option<i64>,
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
    pub(crate) degraded: Option<String>,
    /// Wall-clock ms of the last ERROR-level staleness escalation, so the reminder decays instead
    /// of repeating per pass. 0 = never escalated.
    pub(crate) last_escalation_ms: i64,
}

/// The suppression reason — a constant, so the edge-triggered WARN fires ONCE per outage however
/// long it lasts. (The 2,516 identical the CI box lines were this one.)
pub(crate) const SUPPRESSED_REASON: &str = "venue health probe reports Degraded";

/// Lower bound of the staleness threshold — see [`staleness_threshold`].
pub(crate) const STALENESS_FLOOR: Duration = Duration::from_secs(5 * 60);
/// Upper bound of the staleness threshold — see [`staleness_threshold`].
pub(crate) const STALENESS_CEILING: Duration = Duration::from_secs(30 * 60);
/// How many missed passes the threshold allows before it fires — see [`staleness_threshold`].
const STALENESS_INTERVALS: u32 = 10;
/// How often an already-escalated venue repeats its ERROR line. Decaying rather than per-pass:
/// the incident's lesson is that a line repeated every 60 s for 42 hours is not a signal.
pub(crate) const ESCALATION_REMINDER: Duration = Duration::from_secs(60 * 60);

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
pub(crate) fn staleness_threshold(interval: Option<Duration>) -> Duration {
    // `saturating_mul`, not `*`: `VIKE_RECONCILE_INTERVAL_MS` is operator-supplied and a plain
    // multiply PANICS on overflow. A panic here would take down the reconcile driver thread of a
    // live daemon over a mistyped setting, and the clamp below makes the saturated value
    // indistinguishable from any other large one.
    let base = interval.unwrap_or(Duration::from_secs(60)).saturating_mul(STALENESS_INTERVALS);
    base.clamp(STALENESS_FLOOR, STALENESS_CEILING)
}
