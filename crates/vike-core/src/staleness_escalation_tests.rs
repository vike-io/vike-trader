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
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
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
    // `sole_account_of`: every row in this harness is a venue's only account, which is what
    // these tests are about (the escalation clocks, the health gate, the reason-keyed edges).
    // The two-account producer has its own proofs in `tests/recon/recon_per_account.rs`.
    let clients: Vec<ReconLeg> = clients
        .into_iter()
        .map(|(v, c)| ReconLeg::sole_account_of(v, Box::new(c) as Box<dyn ReconClient>))
        .collect();
    let health: HealthProbe = Arc::new(move |venue: &str| {
        if degraded.contains(&venue) { ReconHealth::Degraded } else { ReconHealth::Healthy }
    });
    let liveness = clients
        .iter()
        .map(|leg| VenueLiveness {
            venue: leg.venue.clone(),
            leg_key: leg.leg_key().to_string(),
            last_pass_ms: Some(t0),
            // Armed at the baseline like `last_pass_ms`, so a balance-staleness test measures
            // the clock rather than the arming rule (which has its own test).
            last_balance_ms: Some(t0),
            degraded: None,
            last_escalation_ms: 0,
        })
        .collect();
    let mgr =
        ReconManager { clients, config, ingest: tx.downgrade(), health: Some(health), liveness };
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

/// **The byte-identity invariant, as a property of the TYPE rather than of a caller's care.**
/// A default account's route key IS its venue id (`vike_mount::account_route_key` renders the
/// bare id for `AccountLabel::Default`), so a leg built for one must carry `route_key: None` —
/// the literal value the pre-fix producer hardcoded — no matter which constructor built it.
///
/// Without the normalization in [`ReconLeg::account`], a fan-out that handed every account
/// through the labelled constructor would put a `Some("binance")` on the wire and into the
/// JOURNAL, where a command payload is a persisted schema, for every existing deployment.
/// Routing would be unaffected (`RouteKey::declared(k) == RouteKey::sole_account_of(v)` when
/// `k == v`), which is exactly why this needs a test rather than a reading.
#[test]
fn a_default_accounts_leg_carries_no_route_key() {
    let stub = || Box::new(StubClient::healthy()) as Box<dyn ReconClient>;
    assert_eq!(ReconLeg::sole_account_of("binance", stub()).route_key, None);
    assert_eq!(
        ReconLeg::account("binance", "binance", stub()).route_key,
        None,
        "a route key equal to its own venue normalizes away"
    );
    assert_eq!(
        ReconLeg::account("binance", "binance#ALT", stub()).route_key.as_deref(),
        Some("binance#ALT"),
        "…and a genuinely labelled account keeps its key"
    );
}

/// **A labelled leg's `venue` stays CANONICAL.** The other half of the same invariant, and the
/// one the health gate reads: `ReconLeg::account` must never fold the route key into `venue`,
/// however decorated that key is.
#[test]
fn a_labelled_legs_venue_is_the_canonical_exchange_id() {
    let leg = ReconLeg::account("binance", "binance#ALT", Box::new(StubClient::healthy()));
    assert_eq!(leg.venue, "binance", "the health-probe key is the exchange, not the account");
    assert_eq!(leg.leg_key(), "binance#ALT", "…while the leg key names the account");
    // A sole account's leg key is its venue, so the escalation lines are unchanged there.
    let sole = ReconLeg::sole_account_of("binance", Box::new(StubClient::healthy()));
    assert_eq!(sole.leg_key(), "binance");
}
