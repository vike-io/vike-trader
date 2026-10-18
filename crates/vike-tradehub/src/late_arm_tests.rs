//! The late-arm machine on a virtual clock: the 90-second guard, the backoff and the once-only
//! latch are exact, and the thread is the machine with a real clock and a real probe.

use super::*;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// **Never in the first 90 seconds**: a gatewayhub that answers from the first instant is not asked
/// before the process is [`MIN_UPTIME`] old, however often the loop turns.
#[test]
fn the_probe_is_not_made_in_the_first_ninety_seconds() {
    let mut machine = LateArm::new();
    let mut probes = 0;
    for now in [0, 1, 30, 89] {
        let step = machine.step(secs(now), &mut || {
            probes += 1;
            true
        });
        assert_eq!(step, Step::Idle, "at {now} s");
    }
    assert_eq!(probes, 0, "the gatewayhub is not even asked");
    assert!(!machine.fired());
}

/// The first probe is made AT 90 s, and a gatewayhub that is still down is asked again after 5, 10,
/// 20, 30 and 30 seconds — the backoff the plan names, capped.
#[test]
fn a_gatewayhub_still_down_is_asked_on_a_capped_backoff() {
    let mut machine = LateArm::new();
    let mut asked_at = Vec::new();
    for now in 0..=300u64 {
        let step = machine.step(secs(now), &mut || {
            asked_at.push(now);
            false
        });
        assert_ne!(step, Step::Fire);
    }
    assert_eq!(
        asked_at[..6],
        [90, 95, 105, 125, 155, 185],
        "first poll at 90 s, then +5 +10 +20 +30"
    );
    assert!(
        asked_at.windows(2).skip(4).all(|w| w[1] - w[0] == 30),
        "and every 30 s after that: {asked_at:?}"
    );
}

/// **It fires at most once per process start**: the step that finds the gatewayhub up fires, and no
/// later step probes or fires again, whatever the probe says.
#[test]
fn it_fires_once_and_never_again() {
    let mut machine = LateArm::new();
    assert_eq!(machine.step(secs(90), &mut || true), Step::Fire);
    assert!(machine.fired());
    let mut probes = 0;
    for now in [91, 200, 10_000] {
        let step = machine.step(secs(now), &mut || {
            probes += 1;
            true
        });
        assert_eq!(step, Step::Idle);
    }
    assert_eq!(probes, 0, "a fired machine probes nothing");
}

/// A gatewayhub that flaps — down, up — fires on the first UP answer after the guard, not before it.
#[test]
fn a_flapping_gatewayhub_fires_on_the_first_answer_after_the_guard() {
    let mut machine = LateArm::new();
    let answers = [false, true];
    let mut i = 0;
    let mut fired_at = None;
    for now in 0..=200u64 {
        if machine.step(secs(now), &mut || {
            let up = answers[i.min(1)];
            i += 1;
            up
        }) == Step::Fire
        {
            fired_at = Some(now);
        }
    }
    assert_eq!(fired_at, Some(95), "down at 90 s, up at 95 s");
}

/// A plan that waits for no one starts no thread.
#[test]
fn an_empty_plan_spawns_nothing() {
    let plan = LateArmPlan {
        link: GatewayLink::new(
            "127.0.0.1:1",
            vike_hub_proto::auth::HubKeys::new(Vec::new(), Vec::new()),
        ),
        waiting: Vec::new(),
        gateway: false,
        datahub: None,
        feeds_why: None,
        restarts: RestartLedger::none(),
    };
    assert!(spawn(plan, Instant::now(), Arc::new(AtomicBool::new(false))).is_none());
}

/// **The operator's stop wins a race with the probe.** A SIGTERM that latched while the probe was in
/// flight leaves the stop flag up; a probe that then succeeds must not claim the stop, must not record
/// `fired`, and so the process exits with the teardown's status, never 75.
#[test]
fn a_stop_already_up_is_the_operators_and_the_late_arm_does_not_claim_it() {
    let stop = AtomicBool::new(true);
    let fired = AtomicBool::new(false);
    assert!(!claim_stop(&stop, &fired), "the late arm does not own a stop that was already up");
    assert!(!fired.load(Ordering::SeqCst), "and records nothing: no exit 75 on a systemctl stop");
    assert!(stop.load(Ordering::SeqCst), "the stop stands");
}

/// The ordinary fire: the stop was down, the late arm raises it and records that IT did.
#[test]
fn a_stop_that_was_down_is_raised_by_the_late_arm_and_recorded() {
    let stop = AtomicBool::new(false);
    let fired = AtomicBool::new(false);
    assert!(claim_stop(&stop, &fired));
    assert!(stop.load(Ordering::SeqCst) && fired.load(Ordering::SeqCst));
}

/// **A teardown that did not complete keeps its own status.** 75 is ordered only over a graceful one:
/// a hard-capped or aborted teardown (resting orders may not all have been cancelled) is not restarted
/// by the late arm's say-so.
#[test]
fn only_a_graceful_teardown_is_turned_into_the_restart_status() {
    assert_eq!(exit_status(true, true), Some(EXIT_LATE_ARM));
    assert_eq!(exit_status(true, false), None, "a capped teardown keeps its status");
    assert_eq!(exit_status(false, true), None, "an operator's stop is not a late arm");
    assert_eq!(exit_status(false, false), None);
}

/// The root asks [`exit_status`] and nothing else for the late arm's exit code: a root that went back to
/// returning [`EXIT_LATE_ARM`] whenever `fired` is set would restart over a capped teardown again (the
/// root cannot be driven here, so this pins its text).
#[test]
fn the_root_takes_the_late_arm_status_from_exit_status_alone() {
    let root = include_str!("tradehub_cli.rs");
    assert!(root.contains("crate::late_arm::exit_status(fired, graceful)"));
    assert!(!root.contains("ExitCode::from(crate::late_arm::EXIT_LATE_ARM)"));
}

// ---- the datahub half of the wait (W12b) ------------------------------------------------------------

fn dead_link() -> GatewayLink {
    GatewayLink::new("127.0.0.1:1", vike_hub_proto::auth::HubKeys::new(Vec::new(), Vec::new()))
}

/// **What the plan waits for is what its probe asks.** A plan built for the price feeds alone asks the
/// datahub and NOT the gatewayhub (a box of `paper` accounts may have no gatewayhub at all and must not wait
/// for one); a plan with accounts waiting asks the gatewayhub; both asks must pass. Without the datahub half a
/// datahub that stays away while the gatewayhub is up would fire the restart at the first poll, and the
/// restarted process would wait again: a restart every ~95 s.
#[test]
fn the_plan_asks_the_datahub_for_the_feeds_and_the_gatewayhub_only_for_accounts() {
    use std::sync::atomic::AtomicBool;
    let up = Arc::new(AtomicBool::new(false));
    let probe = {
        let up = Arc::clone(&up);
        crate::feeds::DatahubProbe::new(move || {
            (!up.load(Ordering::SeqCst)).then(|| "datahub away".to_string())
        })
    };

    // The feeds alone (every account is paper): the dead link is never asked.
    let feeds_only =
        LateArmPlan::of(dead_link(), Vec::new(), Some(probe.clone())).expect("the feeds wait");
    assert!(!feeds_only.gateway && feeds_only.waiting == ["price feeds (datahub)"]);
    assert!(!feeds_only.answers(), "the datahub is away");
    up.store(true, Ordering::SeqCst);
    assert!(feeds_only.answers(), "the datahub answers, and nothing else is waited for");

    // An account waiting for the gatewayhub as well: the dead gatewayhub holds the plan back whatever the
    // datahub says.
    let both = LateArmPlan::of(dead_link(), vec!["binance#ALT".to_string()], Some(probe.clone()))
        .expect("both wait");
    assert!(both.gateway && both.datahub.is_some());
    assert_eq!(both.waiting, ["binance#ALT", "price feeds (datahub)"]);
    assert!(!both.answers(), "the datahub is up but the gatewayhub is not");

    // The gatewayhub alone is the plan it always was.
    let gateway_only = LateArmPlan::of(dead_link(), vec!["binance#ALT".to_string()], None)
        .expect("the gatewayhub waits");
    assert!(gateway_only.gateway && gateway_only.datahub.is_none());

    // Nothing waits, no plan.
    assert!(LateArmPlan::of(dead_link(), Vec::new(), None).is_none());
}

// ---- the restart budget of the feed-driven wait (W12d) ---------------------------------------------------------

const HOUR_MS: i64 = 3_600_000;
const NOW_MS: i64 = 1_700_000_000_000;

fn feeds_plan(ledger: RestartLedger) -> LateArmPlan {
    let mut plan =
        LateArmPlan::of(dead_link(), Vec::new(), Some(crate::feeds::DatahubProbe::new(|| None)))
            .expect("the feeds wait");
    plan.feeds_why = Some("gatewayhub 127.0.0.1:1: the link is down".to_string());
    plan.restarts = ledger;
    plan
}

fn ledger_file(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(RESTART_LEDGER_FILE)
}

/// **The ledger counts one window and forgets the rest**: stamps older than the hour do not count, a garbled
/// token is skipped, a stamp written now counts, and a write keeps only what is still inside the window.
#[test]
fn the_ledger_counts_one_window_and_forgets_the_rest() {
    let dir = tempfile::tempdir().expect("a dir");
    let ledger = RestartLedger::in_dir(dir.path());
    assert_eq!(ledger.recent(NOW_MS), Ok(0), "no file is no restarts");
    std::fs::write(
        ledger_file(dir.path()),
        format!(
            "{},{},not-a-stamp,{}",
            NOW_MS - 2 * HOUR_MS,
            NOW_MS - HOUR_MS / 2,
            NOW_MS - 60_000
        ),
    )
    .expect("seed");
    assert_eq!(ledger.recent(NOW_MS), Ok(2), "the two-hour-old stamp is outside the window");
    ledger.record(NOW_MS).expect("writable");
    assert_eq!(ledger.recent(NOW_MS), Ok(3));
    let kept = std::fs::read_to_string(ledger_file(dir.path())).expect("read");
    assert!(
        !kept.contains(&(NOW_MS - 2 * HOUR_MS).to_string()),
        "the write dropped the old stamp: {kept}"
    );
    // A later reading ages them out.
    assert_eq!(ledger.recent(NOW_MS + HOUR_MS), Ok(0));
    // A ledger with no path counts nothing and never refuses.
    let none = RestartLedger::none();
    none.record(NOW_MS).expect("no-op");
    assert_eq!(none.recent(NOW_MS), Ok(0));
}

/// **A spent budget stops the feed-driven wait, and only that**: below the budget the plan is untouched; at it
/// the plan stops waiting for the feeds (no probe, nothing to restart for, the banner's cause leads with the mark
/// and keeps the original reason); an account still waiting for the gatewayhub keeps its wait; a plan with no feed
/// wait is not counted at all; and a ledger that cannot be read is read as spent.
#[test]
fn a_spent_budget_stops_the_feed_driven_wait_and_nothing_else() {
    let dir = tempfile::tempdir().expect("a dir");
    let ledger = RestartLedger::in_dir(dir.path());
    for _ in 0..FEED_RESTARTS_PER_WINDOW - 1 {
        ledger.record(NOW_MS).expect("writable");
    }
    let plan = feeds_plan(ledger.clone()).within_budget(NOW_MS);
    assert!(plan.datahub.is_some() && plan.waiting == [FEEDS_WAIT_ENTRY], "one restart left");

    ledger.record(NOW_MS).expect("writable");
    let spent = feeds_plan(ledger.clone()).within_budget(NOW_MS);
    assert!(spent.datahub.is_none() && spent.waiting.is_empty(), "{spent:?}");
    let why = spent.feeds_why.as_deref().expect("the banner keeps a cause");
    assert!(why.starts_with(RESTARTS_SPENT_MARK) && why.ends_with("the link is down"), "{why}");

    // An hour later the budget is back.
    assert!(feeds_plan(ledger.clone()).within_budget(NOW_MS + HOUR_MS).datahub.is_some());

    // An account waiting for the gatewayhub keeps its wait; only the feeds' entry goes.
    let mut mixed = LateArmPlan::of(
        dead_link(),
        vec!["binance#ALT".to_string()],
        Some(crate::feeds::DatahubProbe::new(|| None)),
    )
    .expect("both wait");
    mixed.restarts = ledger.clone();
    let mixed = mixed.within_budget(NOW_MS);
    assert_eq!(mixed.waiting, ["binance#ALT"]);
    assert!(mixed.gateway && mixed.datahub.is_none());

    // A gatewayhub-only plan is not counted, however full the ledger.
    let mut gateway_only =
        LateArmPlan::of(dead_link(), vec!["binance#ALT".to_string()], None).expect("waits");
    gateway_only.restarts = ledger;
    assert_eq!(gateway_only.within_budget(NOW_MS).waiting, ["binance#ALT"]);

    // A ledger that exists and cannot be read (here: a directory in its place) is not "no restarts yet".
    let broken = tempfile::tempdir().expect("a dir");
    std::fs::create_dir(ledger_file(broken.path())).expect("a directory where the file goes");
    let unreadable = feeds_plan(RestartLedger::in_dir(broken.path())).within_budget(NOW_MS);
    assert!(unreadable.datahub.is_none(), "an untrustworthy counter licenses no restart");
}

/// What the Fire line says the wait was for follows the plan: the gatewayhub is named only when it was asked,
/// the datahub only when it was.
#[test]
fn the_fire_line_names_only_what_was_waited_for() {
    let feeds = feeds_plan(RestartLedger::none());
    assert_eq!(feeds.answers_words(), "the datahub's strategy lane answers");
    let gateway_only =
        LateArmPlan::of(dead_link(), vec!["binance#ALT".to_string()], None).expect("waits");
    assert_eq!(gateway_only.answers_words(), "the gatewayhub at 127.0.0.1:1 answers");
    let both = LateArmPlan::of(
        dead_link(),
        vec!["binance#ALT".to_string()],
        Some(crate::feeds::DatahubProbe::new(|| None)),
    )
    .expect("both wait");
    assert_eq!(
        both.answers_words(),
        "the gatewayhub at 127.0.0.1:1 and the datahub's strategy lane answer"
    );
}
