//! The account watcher's decisions, over a scripted [`World`] (no store, no gatewayhub, no core): what
//! it starts, what it leaves alone, what it publishes, and that a refusal is one account's.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use vike_model::AccountTier::{Demo, Live, Paper};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::{AdapterPolicy, NotRunning};

use crate::server::symbol_admission::SymbolFacts;

use super::*;

/// What the scripted world was asked and what it answers next.
#[derive(Default)]
struct Script {
    changes: VecDeque<Result<bool, String>>,
    accounts: AccountDirectory,
    outcomes: VecDeque<HotStart>,
    started: Vec<String>,
    published: Vec<Vec<NotRunningAccount>>,
    /// Every stop asked for, in order, and the answer the next one gets (default: `Stopped`).
    stopped: Vec<NotRunningAccount>,
    stop_outcomes: VecDeque<HotStop>,
    /// The size of every `settle` batch (the one bounded wait for the cancels, then the detaches).
    settled: Vec<usize>,
    /// `publish` / `stop:<key>` in the order they happened: the stop's first rule is the order.
    log: Vec<String>,
    /// The positions the scripted venue says an account holds, by route key.
    positions: Vec<(String, Vec<(String, f64)>)>,
    /// The scripted clock the backoff is read on: a real instant plus what the test has advanced.
    clock: Clock,
    /// What the scripted gatewayhub answers to each "which running accounts hold a changed key" (an empty
    /// queue, or `None`, is "could not say"), and every list the watcher published as the warning.
    key_answers: VecDeque<Option<BTreeSet<String>>>,
    key_published: Vec<Vec<String>>,
}

/// A clock that only moves when the test says so.
struct Clock {
    base: Instant,
    advanced: Duration,
}

impl Default for Clock {
    fn default() -> Self {
        Clock { base: Instant::now(), advanced: Duration::ZERO }
    }
}

#[derive(Clone, Default)]
struct Scripted(Arc<Mutex<Script>>);

impl Scripted {
    /// Let `by` pass on the scripted clock.
    fn advance(&self, by: Duration) {
        self.0.lock().unwrap().clock.advanced += by;
    }
    fn rows(&self, policy: &AdapterPolicy) {
        self.0.lock().unwrap().accounts = policy.accounts.clone();
    }
    fn change(&self) {
        self.0.lock().unwrap().changes.push_back(Ok(true));
    }
    fn next_start(&self, outcome: HotStart) {
        self.0.lock().unwrap().outcomes.push_back(outcome);
    }
    fn started(&self) -> Vec<String> {
        self.0.lock().unwrap().started.clone()
    }
    fn last_published(&self) -> Option<Vec<NotRunningAccount>> {
        self.0.lock().unwrap().published.last().cloned()
    }
    fn publications(&self) -> usize {
        self.0.lock().unwrap().published.len()
    }
    fn publication(&self, n: usize) -> Vec<NotRunningAccount> {
        self.0.lock().unwrap().published[n].clone()
    }
    fn stops(&self) -> Vec<String> {
        self.0.lock().unwrap().stopped.iter().map(|nr| nr.route_key.clone()).collect()
    }
    fn log(&self) -> Vec<String> {
        self.0.lock().unwrap().log.clone()
    }
    /// The gatewayhub's next answer: these accounts run on a key the database no longer holds.
    fn key_says(&self, route_keys: &[&str]) {
        let set = route_keys.iter().map(|k| (*k).to_string()).collect();
        self.0.lock().unwrap().key_answers.push_back(Some(set));
    }
    /// The gatewayhub's next answer: none (it could not say).
    fn key_unanswered(&self) {
        self.0.lock().unwrap().key_answers.push_back(None);
    }
    /// Every warning list the watcher published, in order.
    fn key_warnings(&self) -> Vec<Vec<String>> {
        self.0.lock().unwrap().key_published.clone()
    }
    fn holds(&self, route_key: &str, symbol: &str, size: f64) {
        self.0
            .lock()
            .unwrap()
            .positions
            .push((route_key.to_string(), vec![(symbol.to_string(), size)]));
    }
}

/// `policy` plus one DEACTIVATED row of `venue`/`label` at `tier`: a row nobody uses any more, which
/// is what a stop is decided on.
fn with_inactive_row(
    policy: AdapterPolicy,
    venue: &str,
    label: &AccountLabel,
    tier: vike_model::AccountTier,
) -> AdapterPolicy {
    policy.with_account_row(vike_secrets::Account {
        id: 100,
        venue: venue.to_string(),
        tier,
        label: label.text().map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: false,
        last_verified_at: None,
        max_exposure: None,
    })
}

impl World for Scripted {
    fn rows_changed(&mut self) -> Result<bool, String> {
        self.0.lock().unwrap().changes.pop_front().unwrap_or(Ok(false))
    }
    fn accounts(&mut self) -> AccountDirectory {
        self.0.lock().unwrap().accounts.clone()
    }
    fn start(&mut self, _: &AccountDirectory, venue: &str, label: &AccountLabel) -> HotStart {
        let mut me = self.0.lock().unwrap();
        me.started.push(route_key_of(venue, label));
        me.outcomes.pop_front().expect("a scripted outcome for every start")
    }
    fn publish_not_running(&mut self, list: &[NotRunningAccount]) {
        let mut me = self.0.lock().unwrap();
        me.log.push("publish".to_string());
        me.published.push(list.to_vec());
    }
    fn stopping(
        &mut self,
        venue: &str,
        label: &AccountLabel,
        route_key: &str,
        tier: Option<vike_model::AccountTier>,
        restart_at: Option<vike_model::AccountTier>,
    ) -> NotRunningAccount {
        let me = self.0.lock().unwrap();
        let open = me
            .positions
            .iter()
            .find(|(key, _)| key == route_key)
            .map(|(_, held)| held.clone())
            .unwrap_or_default();
        NotRunningAccount {
            venue: venue.to_string(),
            account: (!label.is_default()).then(|| label.clone()),
            route_key: route_key.to_string(),
            tier,
            reason: restart_at.map_or_else(|| stop_reason(&open), |to| restart_reason(to, &open)),
        }
    }
    fn stop(&mut self, entry: &NotRunningAccount) -> HotStop {
        let mut me = self.0.lock().unwrap();
        me.log.push(format!("stop:{}", entry.route_key));
        me.stopped.push(entry.clone());
        me.stop_outcomes.pop_front().unwrap_or(HotStop::Stopped)
    }
    fn settle(&mut self, stopped: &[NotRunningAccount]) {
        let mut me = self.0.lock().unwrap();
        me.log.push(format!(
            "settle:{}",
            stopped.iter().map(|nr| nr.route_key.as_str()).collect::<Vec<_>>().join(",")
        ));
        me.settled.push(stopped.len());
    }
    fn now(&mut self) -> Instant {
        let me = self.0.lock().unwrap();
        me.clock.base + me.clock.advanced
    }
    fn key_changed(&mut self, _: &AccountDirectory) -> Option<BTreeSet<String>> {
        self.0.lock().unwrap().key_answers.pop_front().flatten()
    }
    fn publish_key_changed(&mut self, list: &[String]) {
        self.0.lock().unwrap().key_published.push(list.to_vec());
    }
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A node that runs binance's default demo account and has been told binance has a second, `ALT`.
fn node_with_a_second_demo_account() -> (Scripted, Watcher<Scripted>) {
    let world = Scripted::default();
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
    );
    let watcher = Watcher::new(world.clone(), vec!["binance".to_string()], Vec::new());
    (world, watcher)
}

fn started(route_key: &str, tier: vike_model::AccountTier) -> HotStart {
    HotStart::Started { route_key: route_key.to_string(), tier }
}

fn not_running(block: ArmingBlock) -> HotStart {
    HotStart::NotRunning(NotRunning {
        venue: "binance".to_string(),
        account: alt(),
        tier: Some(Demo),
        block,
    })
}
#[test]
fn wanted_accounts_resolve_through_the_one_tier_authority() {
    let policy = AdapterPolicy::default()
        .with_account("binance", &AccountLabel::Default, Demo)
        .with_account("bybit", &AccountLabel::Default, Live)
        .with_account("okx", &AccountLabel::Default, Paper);
    let wanted = wanted_accounts(&policy.accounts);
    let tier_of = |key: &str| wanted.iter().find(|w| w.route_key == key).map(|w| w.tier);
    assert_eq!(tier_of("binance"), Some(Ok(Demo)));
    assert_eq!(tier_of("bybit"), Some(Ok(Live)));
    assert_eq!(tier_of("okx"), Some(Ok(Paper)));
    assert_eq!(
        tier_of("deribit"),
        Some(Err(ArmingBlock::NoAccountRow)),
        "every venue lists its default account, and no row means it does not run"
    );
}

#[test]
fn a_store_that_did_not_change_starts_nothing() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    watcher.tick();
    watcher.tick();
    assert!(world.started().is_empty());
    assert_eq!(world.publications(), 0);
}

#[test]
fn an_active_account_without_an_engine_is_started_and_the_rest_are_left_alone() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    let before = vec![NotRunningAccount {
        venue: "binance".to_string(),
        account: Some(alt()),
        route_key: "binance#ALT".to_string(),
        tier: Some(Demo),
        reason: "account inactive".to_string(),
    }];
    watcher.not_running = before;
    world.change();
    world.next_start(started("binance#ALT", Demo));
    watcher.tick();
    // Only the account with no engine is asked; the default account that runs is not touched.
    assert_eq!(world.started(), vec!["binance#ALT".to_string()]);
    assert_eq!(world.last_published(), Some(Vec::new()), "the started account leaves the list");
    // A later change that moves nothing about it starts nothing again.
    world.change();
    watcher.tick();
    assert_eq!(world.started().len(), 1, "an account that runs is never started twice");
}

#[test]
fn a_refusal_is_that_one_account_not_running_and_is_tried_again_at_the_next_change() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    world.change();
    world.next_start(not_running(ArmingBlock::GatewayUnavailable));
    watcher.tick();
    let listed = world.last_published().expect("the refusal is published");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].route_key, "binance#ALT");
    assert_eq!(listed[0].reason, ArmingBlock::GatewayUnavailable.as_str());
    assert!(!watcher.running.contains_key("binance#ALT"));
    assert!(watcher.running.contains_key("binance"), "the account that runs keeps running");

    // The same refusal again does not republish an identical list ...
    world.change();
    world.next_start(not_running(ArmingBlock::GatewayUnavailable));
    watcher.tick();
    assert_eq!(world.started().len(), 2, "tried again at the next change of the rows");
    assert_eq!(world.publications(), 1, "an unchanged list is not published twice");

    // ... and when the gatewayhub is back the account starts and leaves the list.
    world.change();
    world.next_start(started("binance#ALT", Demo));
    watcher.tick();
    assert_eq!(world.last_published(), Some(Vec::new()));
    assert!(watcher.running.contains_key("binance#ALT"));
}

#[test]
fn a_tier_the_build_does_not_follow_is_listed_as_needing_a_restart() {
    let world = Scripted::default();
    world.rows(&AdapterPolicy::default().with_account("binance", &alt(), Paper));
    let mut watcher = Watcher::new(world.clone(), Vec::new(), Vec::new());
    world.change();
    world.next_start(HotStart::RestartRequired(
        "this venue's price feed was not wired at boot".into(),
    ));
    watcher.tick();
    let listed = world.last_published().expect("published");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].route_key, "binance#ALT");
    assert_eq!(listed[0].reason, "restart required to start");
    assert!(watcher.running.is_empty(), "nothing was started");
    // A restart is nothing a backoff mends: the clock asks nothing again.
    world.advance(MAX_POLL * 4);
    watcher.tick();
    assert_eq!(world.started().len(), 1);
}

#[test]
fn a_store_that_cannot_be_read_changes_nothing_that_runs() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    world.0.lock().unwrap().changes.push_back(Err("database is locked".to_string()));
    watcher.tick();
    assert!(world.started().is_empty(), "an unreadable store is not an answer");
    assert_eq!(world.publications(), 0);
    assert!(watcher.running.contains_key("binance"));
    // ... and the next poll that can read acts on the rows as usual.
    world.change();
    world.next_start(started("binance#ALT", Demo));
    watcher.tick();
    assert_eq!(world.started(), vec!["binance#ALT".to_string()]);
}

#[test]
fn a_running_account_whose_row_stopped_saying_so_keeps_running() {
    // A table with NO row for it is not a deactivation (slice S2 stops on `account inactive` only): the
    // engine stays, nothing is started or published, and the difference is reported.
    let world = Scripted::default();
    world.rows(&AdapterPolicy::default().with_account("binance", &AccountLabel::Default, Demo));
    let mut watcher = Watcher::new(world.clone(), vec!["binance".to_string()], Vec::new());
    world.rows(&AdapterPolicy::default());
    world.change();
    watcher.tick();
    assert!(world.started().is_empty());
    assert_eq!(world.publications(), 0);
    assert_eq!(
        watcher.running.get("binance"),
        Some(&Some(Demo)),
        "it still runs, at the tier it started at"
    );
}

#[test]
fn the_core_going_away_ends_the_watcher() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    world.change();
    world.next_start(HotStart::CoreGone);
    watcher.tick();
    assert!(watcher.done());
}

// ---- slice S4: a key changed under a running account --------------------------------------------

/// **A changed key is a warning on the RUNNING account and nothing else** (owner ruling 2026-10-17): it is
/// published, not repeated, not acted on (no stop, no start), kept when the gatewayhub cannot answer, ignored
/// for an account that does not run, and cleared when the account is restarted.
#[test]
fn a_key_changed_under_a_running_account_is_a_warning_and_a_restart_clears_it() {
    let (world, mut watcher) = node_running_two_accounts();

    // A commit that changes no key: the gatewayhub names nobody, and nothing is published.
    world.key_says(&[]);
    world.change();
    watcher.tick();
    assert!(world.key_warnings().is_empty());

    // The key of ALT is rotated in the database; an account that does not run is named too and is ignored.
    world.key_says(&["binance#ALT", "bybit"]);
    world.change();
    watcher.tick();
    assert_eq!(world.key_warnings(), vec![vec!["binance#ALT".to_string()]]);
    assert!(world.stops().is_empty(), "a changed key restarts nothing");
    assert!(world.started().is_empty());
    assert!(watcher.running.contains_key("binance#ALT"), "the account keeps running");

    // Later commits: the same answer is not published again, and an unanswered question changes nothing.
    world.key_says(&["binance#ALT"]);
    world.change();
    watcher.tick();
    world.key_unanswered();
    world.change();
    watcher.tick();
    assert_eq!(world.key_warnings().len(), 1, "said once: {:?}", world.key_warnings());

    // The operator restarts the account (deactivate, activate): the warning is gone as soon as it stops,
    // even though the gatewayhub still names it (it does not run), and the new session stays clear.
    world.rows(&alt_deactivated());
    world.key_says(&["binance#ALT"]);
    world.change();
    watcher.tick();
    assert_eq!(world.key_warnings().last(), Some(&Vec::new()));
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
    );
    world.next_start(started("binance#ALT", Demo));
    world.key_says(&[]);
    world.change();
    watcher.tick();
    assert!(watcher.running.contains_key("binance#ALT"));
    assert_eq!(world.key_warnings().last(), Some(&Vec::new()));
    assert_eq!(world.key_warnings().len(), 2, "raised, then cleared; nothing in between");
}

/// The warning also clears when the key goes back to the one the session started with (the gatewayhub stops
/// naming the account), and a second account can be raised beside the first.
#[test]
fn a_key_put_back_clears_the_warning_and_two_accounts_are_listed_in_order() {
    let (world, mut watcher) = node_running_two_accounts();
    world.key_says(&["binance#ALT"]);
    world.change();
    watcher.tick();
    world.key_says(&["binance#ALT", "binance"]);
    world.change();
    watcher.tick();
    assert_eq!(
        world.key_warnings().last(),
        Some(&vec!["binance".to_string(), "binance#ALT".to_string()])
    );
    world.key_says(&[]);
    world.change();
    watcher.tick();
    assert_eq!(world.key_warnings().last(), Some(&Vec::new()));
}

// ---- slice S2: the stop -------------------------------------------------------------------------

/// A node that runs binance's default account and its `ALT` account, both at `Demo`.
fn node_running_two_accounts() -> (Scripted, Watcher<Scripted>) {
    let world = Scripted::default();
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
    );
    let watcher = Watcher::new(
        world.clone(),
        vec!["binance".to_string(), "binance#ALT".to_string()],
        Vec::new(),
    );
    (world, watcher)
}

/// The rows after the operator deactivated `ALT`: the default account's row stays active.
fn alt_deactivated() -> AdapterPolicy {
    with_inactive_row(
        AdapterPolicy::default().with_account("binance", &AccountLabel::Default, Demo),
        "binance",
        &alt(),
        Demo,
    )
}

/// The headline: `active = 0` on a running account stops THAT account and nothing else, and the account
/// is on the published NOT RUNNING list BEFORE the core is told (so the control edge refuses its orders
/// from the first frame), with the reason the arming table gives for an inactive row.
#[test]
fn a_running_account_whose_row_went_inactive_is_listed_first_and_then_stopped() {
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();

    assert_eq!(world.stops(), vec!["binance#ALT".to_string()], "only the deactivated account");
    assert_eq!(
        world.log(),
        vec![
            "publish".to_string(),
            "stop:binance#ALT".to_string(),
            "settle:binance#ALT".to_string()
        ],
        "refuse first (publish the list), then stop, then settle"
    );
    let listed = world.last_published().expect("published");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].route_key, "binance#ALT");
    assert_eq!(listed[0].reason, ArmingBlock::AccountInactive.as_str());
    assert!(watcher.running.contains_key("binance"), "the other account keeps running");
    assert!(!watcher.running.contains_key("binance#ALT"));
    assert!(world.started().is_empty(), "a stop starts nothing");
}

/// The account stays stopped through later commits that change nothing about it, and starts again,
/// as a start, when its row says active.
#[test]
fn a_stopped_account_stays_stopped_and_starts_again_when_its_row_says_active() {
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();
    assert_eq!(world.stops().len(), 1);

    // Any later commit (another row, a credential) re-reads the rows: the stopped account is not
    // stopped twice and not started.
    world.change();
    watcher.tick();
    assert_eq!(world.stops().len(), 1, "stopped once");
    assert!(world.started().is_empty());
    assert_eq!(world.publications(), 1, "and the list was not republished");

    // Active again: a normal start, which takes it off the list and lifts the stop.
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
    );
    world.change();
    world.next_start(started("binance#ALT", Demo));
    watcher.tick();
    assert_eq!(world.started(), vec!["binance#ALT".to_string()]);
    assert_eq!(world.last_published(), Some(Vec::new()));
    assert!(watcher.running.contains_key("binance#ALT"));
    assert!(watcher.stopped.is_empty());

    // And it can be stopped again: the bookkeeping did not wedge on the first stop.
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();
    assert_eq!(world.stops(), vec!["binance#ALT".to_string(); 2]);
}

/// Deactivating the `demo` row of an account that keeps a `paper` row ACTIVE answers `paper`: the account's
/// tier is now `paper` (slice S4). It is stopped and STARTED at the paper tier in the same pass, listed with
/// the tier it comes back at in between, and a later commit that changes nothing about it does nothing.
#[test]
fn deactivating_the_last_non_paper_row_beside_a_paper_row_restarts_the_account_as_paper() {
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(&with_inactive_row(
        AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Paper),
        "binance",
        &alt(),
        Demo,
    ));
    world.change();
    world.next_start(started("binance#ALT", Paper));
    watcher.tick();
    assert_eq!(world.stops(), vec!["binance#ALT".to_string()]);
    assert_eq!(
        world.started(),
        vec!["binance#ALT".to_string()],
        "and started again at the new tier"
    );
    let between = world.publication(0);
    assert_eq!(between[0].reason, "tier changed, restarting at `paper`");
    assert_eq!(between[0].tier, Some(Paper), "the tier its remaining row states");
    assert_eq!(
        world.log(),
        vec![
            "publish".to_string(),
            "stop:binance#ALT".to_string(),
            "settle:binance#ALT".to_string(),
            "publish".to_string()
        ],
        "listed, stopped, settled (the old session is gone), then started and delisted"
    );
    assert_eq!(world.last_published(), Some(Vec::new()));
    assert_eq!(watcher.running.get("binance#ALT"), Some(&Some(Paper)));

    world.change();
    watcher.tick();
    assert_eq!(world.stops().len(), 1, "an account that runs at its row's tier is left alone");
    assert_eq!(world.started().len(), 1);
}

/// A tier changed IN PLACE on a running account (`demo` to `live`) is the same: stop, then start at the tier the
/// row states, with the start's own gates. The account never keeps the old tier.
#[test]
fn a_tier_changed_in_place_is_a_stop_and_a_start_at_the_new_tier() {
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Live),
    );
    world.change();
    world.next_start(started("binance#ALT", Live));
    watcher.tick();
    assert_eq!(world.stops(), vec!["binance#ALT".to_string()]);
    assert_eq!(world.started(), vec!["binance#ALT".to_string()]);
    assert_eq!(world.publication(0)[0].reason, "tier changed, restarting at `live`");
    assert_eq!(watcher.running.get("binance#ALT"), Some(&Some(Live)));
    assert_eq!(watcher.running.get("binance"), Some(&Some(Demo)), "the other account is untouched");
    assert_eq!(world.stops().len(), 1);
}

/// A restart that the new tier's gates refuse leaves THAT account NOT RUNNING with the reason - not at the old
/// tier and not at a stand-in. A refusal that is only a closing lease is asked again.
#[test]
fn a_restart_the_new_tier_refuses_is_not_running_with_the_reason() {
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Live),
    );
    world.change();
    world.next_start(HotStart::NotRunning(NotRunning {
        venue: "binance".to_string(),
        account: alt(),
        tier: Some(Live),
        block: ArmingBlock::NoCredentials,
    }));
    watcher.tick();
    assert!(!watcher.running.contains_key("binance#ALT"));
    let listed = world.last_published().expect("published");
    assert_eq!(listed[0].reason, ArmingBlock::NoCredentials.as_str());
    // Credentials are a fact about the row, not about time: nothing is asked again by the clock.
    world.advance(MAX_POLL * 4);
    watcher.tick();
    assert_eq!(world.started().len(), 1);
}

/// A writer's kick makes the next slice tick even though the poll is not due, and is taken once.
#[test]
fn a_kick_wakes_the_watcher_before_the_poll_is_due() {
    assert!(!tick_due(POLL, false), "no kick, poll not due: no tick");
    assert!(tick_due(Duration::ZERO, false), "the poll is due");
    kick();
    assert!(take_kick(), "a kick is waiting");
    assert!(tick_due(POLL, true), "a kicked slice ticks at once");
}

/// A `live` account is stopped by the same rule: the stop is the safe direction, and it does not look
/// at the tier the account ran at.
#[test]
fn a_live_account_is_stopped_the_same_way() {
    let world = Scripted::default();
    world.rows(&with_inactive_row(AdapterPolicy::default(), "bybit", &AccountLabel::Default, Live));
    let mut watcher = Watcher::new(world.clone(), vec!["bybit".to_string()], Vec::new());
    world.change();
    watcher.tick();
    assert_eq!(world.stops(), vec!["bybit".to_string()]);
}

/// The absence of an answer is not an instruction to stop: a table with no row for a running account
/// is reported and the account keeps running. A stop is decided on a positive statement of the rows.
#[test]
fn no_row_is_reported_and_never_stops_an_account() {
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(&AdapterPolicy::default());
    world.change();
    watcher.tick();
    assert!(world.stops().is_empty());
    assert!(watcher.running.contains_key("binance") && watcher.running.contains_key("binance#ALT"));
}

/// A position the account holds is named in the reason, and nothing is closed: the stop still goes.
#[test]
fn a_position_open_at_the_venue_is_named_in_the_reason_and_the_stop_still_goes() {
    let (world, mut watcher) = node_running_two_accounts();
    world.holds("binance#ALT", "BTCUSDT", 0.5);
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();
    assert_eq!(world.stops(), vec!["binance#ALT".to_string()]);
    let reason = world.last_published().expect("published")[0].reason.clone();
    assert_eq!(reason, "account inactive; position open at venue: BTCUSDT 0.5");
}

/// Several deactivations in one commit are one batch: every account is listed and told to stop first, then
/// the bounded wait for the cancels is made ONCE and the accounts are detached together (a slow venue costs
/// one wait, not one per account), and a core that is gone settles nothing.
#[test]
fn several_deactivations_in_one_commit_are_settled_in_one_batch() {
    let world = Scripted::default();
    let bob = AccountLabel::parse("BOB").expect("a legal label");
    let inactive = |id: i64, label: &AccountLabel| vike_secrets::Account {
        id,
        venue: "binance".to_string(),
        tier: Demo,
        label: label.text().map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: false,
        last_verified_at: None,
        max_exposure: None,
    };
    let running = AdapterPolicy::default()
        .with_account("binance", &AccountLabel::Default, Demo)
        .with_account("binance", &alt(), Demo)
        .with_account("binance", &bob, Demo);
    world.rows(&running);
    let mut watcher = Watcher::new(
        world.clone(),
        vec!["binance".to_string(), "binance#ALT".to_string(), "binance#BOB".to_string()],
        Vec::new(),
    );
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account_row(inactive(100, &alt()))
            .with_account_row(inactive(101, &bob)),
    );
    world.change();
    watcher.tick();
    assert_eq!(world.stops(), vec!["binance#ALT".to_string(), "binance#BOB".to_string()]);
    assert_eq!(world.0.lock().unwrap().settled, vec![2], "one settle for the whole commit");
    let log = world.log();
    let settle_at = log.iter().position(|l| l.starts_with("settle:")).expect("settled");
    assert!(
        log.iter().rposition(|l| l.starts_with("stop:")) < Some(settle_at),
        "every stop is sent before the one wait: {log:?}"
    );

    let (world, mut watcher) = node_running_two_accounts();
    world.0.lock().unwrap().stop_outcomes.push_back(HotStop::CoreGone);
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();
    assert!(
        world.0.lock().unwrap().settled.is_empty(),
        "nothing is settled on a core that is gone"
    );
}

/// The core going away during a stop ends the watcher, like it does during a start.
#[test]
fn the_core_going_away_during_a_stop_ends_the_watcher() {
    let (world, mut watcher) = node_running_two_accounts();
    world.0.lock().unwrap().stop_outcomes.push_back(HotStop::CoreGone);
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();
    assert!(watcher.done());
}

#[test]
fn the_stop_reason_and_the_open_positions_are_read_off_the_newest_block_of_the_key() {
    assert_eq!(stop_reason(&[]), "account inactive");
    assert_eq!(
        stop_reason(&[("BTCUSDT".to_string(), 0.5), ("ETHUSDT".to_string(), -2.0)]),
        "account inactive; position open at venue: BTCUSDT 0.5, ETHUSDT -2"
    );
    let position = |symbol: &str, size: f64| vike_exec::PositionView {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
        size,
        avg_px: 100.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    };
    let block = |key: &str, positions: Vec<vike_exec::PositionView>| vike_exec::VenueBlock {
        route_key: key.to_string(),
        positions,
        ..Default::default()
    };
    // The account stopped and started again: two blocks under one key, the old one is history.
    let blocks = vec![
        block("binance#ALT", vec![position("BTCUSDT", 1.0)]),
        block("binance", vec![position("BTCUSDT", 9.0)]),
        block("binance#ALT", vec![position("ETHUSDT", 2.0), position("SOLUSDT", 0.0)]),
    ];
    assert_eq!(open_positions(&blocks, "binance#ALT"), vec![("ETHUSDT".to_string(), 2.0)]);
    assert!(open_positions(&blocks, "okx").is_empty());
}

/// What the stop waits on: the live orders of ONE account in the published snapshot. A terminal order and
/// another account's order (the same exchange, another label) do not count.
#[test]
fn the_stop_waits_on_the_live_orders_of_its_own_account_only() {
    let order =
        |coid: &str, account: Option<&str>, status: vike_model::OrderStatus| vike_exec::OrderView {
            data_delay: None,
            client_order_id: coid.to_string(),
            venue: "binance".to_string(),
            account: account.map(|a| AccountLabel::parse(a).expect("a legal label")),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(1.0),
            trigger_price: None,
            status,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
            reason: None,
        };
    use vike_model::OrderStatus::{Accepted, Canceled, Submitted};
    let orders = vec![
        order("a1", Some("ALT"), Accepted),
        order("a2", Some("ALT"), Submitted),
        order("a3", Some("ALT"), Canceled),
        order("d1", None, Accepted),
    ];
    assert_eq!(live_orders_of(&orders, "binance#ALT"), 2);
    assert_eq!(live_orders_of(&orders, "binance"), 1, "the default account's own");
    assert_eq!(live_orders_of(&orders, "okx"), 0);
}

// ---- slice S3: the backoff retry, and the `live` gate ----------------------------------------------

/// The classification of a refusal: what is only a matter of time, and what only a change of the rows can
/// alter. An `adapter failed` is transient only right after this watcher's own stop of the account.
#[test]
fn only_a_matter_of_time_is_retried() {
    let soon = Some(Duration::from_secs(3));
    let late = Some(LEASE_CLOSE_GRACE + Duration::from_secs(1));
    assert!(is_transient(ArmingBlock::GatewayUnavailable, None));
    assert!(is_transient(ArmingBlock::FeedsUnavailable, None));
    assert!(is_transient(ArmingBlock::AdapterFailed, soon), "its own lease was still closing");
    assert!(!is_transient(ArmingBlock::AdapterFailed, None), "the venue's own refusal stands");
    assert!(!is_transient(ArmingBlock::AdapterFailed, late), "past the grace it is the venue's");
    for permanent in [
        ArmingBlock::NoCredentials,
        ArmingBlock::LiveCredentialsAbsent,
        ArmingBlock::LiveSwitchOff,
        ArmingBlock::PreflightFailed,
        ArmingBlock::TierConflict,
        ArmingBlock::FeatureAbsent,
        ArmingBlock::AccountInactive,
    ] {
        assert!(!is_transient(permanent, soon), "{permanent:?} is a fact, not a delay");
    }
}

/// **A gatewayhub that does not answer is asked again on a backoff, with no change of the rows**: the
/// first pause is the late arm's `FIRST_POLL`, each one doubles, and none passes `MAX_POLL`. The account
/// starts the moment the gatewayhub answers, and the asking stops.
#[test]
fn a_refusal_for_want_of_the_gatewayhub_is_asked_again_on_a_backoff() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    world.change();
    world.next_start(not_running(ArmingBlock::GatewayUnavailable));
    watcher.tick();
    assert_eq!(world.started().len(), 1);

    // Nothing is due yet, and no row moved: nothing is asked.
    world.advance(FIRST_POLL - Duration::from_secs(1));
    watcher.tick();
    assert_eq!(world.started().len(), 1);

    // The first pause is up: asked again, refused again, and the next pause is twice as long.
    world.advance(Duration::from_secs(1));
    world.next_start(not_running(ArmingBlock::GatewayUnavailable));
    watcher.tick();
    assert_eq!(world.started().len(), 2);
    world.advance(FIRST_POLL);
    watcher.tick();
    assert_eq!(world.started().len(), 2, "5 s after a 10 s pause was ordered");
    world.advance(FIRST_POLL);
    world.next_start(not_running(ArmingBlock::GatewayUnavailable));
    watcher.tick();
    assert_eq!(world.started().len(), 3);

    // The pauses double 5, 10, 20 and stop at 30.
    for pause in [Duration::from_secs(20), MAX_POLL, MAX_POLL] {
        world.advance(pause - Duration::from_secs(1));
        watcher.tick();
        let before = world.started().len();
        world.advance(Duration::from_secs(1));
        world.next_start(not_running(ArmingBlock::GatewayUnavailable));
        watcher.tick();
        assert_eq!(world.started().len(), before + 1, "asked again after {pause:?}");
    }

    // The gatewayhub is back: the account starts, leaves the list, and nothing is asked afterwards.
    world.advance(MAX_POLL);
    world.next_start(started("binance#ALT", Demo));
    watcher.tick();
    assert!(watcher.running.contains_key("binance#ALT"));
    assert_eq!(world.last_published(), Some(Vec::new()));
    let asked = world.started().len();
    world.advance(MAX_POLL * 3);
    watcher.tick();
    assert_eq!(world.started().len(), asked, "an account that runs is not asked again");
}

/// An account the node booted without for want of the gatewayhub is asked for after `FIRST_POLL`, with no
/// change of the rows at all: the late-arm restart is not ordered for it any more.
#[test]
fn an_account_booted_without_the_gatewayhub_is_started_when_it_answers() {
    let world = Scripted::default();
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
    );
    let waiting = vec![NotRunningAccount {
        venue: "binance".to_string(),
        account: Some(alt()),
        route_key: "binance#ALT".to_string(),
        tier: Some(Demo),
        reason: ArmingBlock::GatewayUnavailable.as_str().to_string(),
    }];
    let mut watcher = Watcher::new(world.clone(), vec!["binance".to_string()], waiting);

    watcher.tick();
    assert!(world.started().is_empty(), "nothing is due at boot");
    world.advance(FIRST_POLL);
    world.next_start(started("binance#ALT", Demo));
    watcher.tick();
    assert_eq!(world.started(), vec!["binance#ALT".to_string()]);
    assert!(watcher.running.contains_key("binance#ALT"));
    assert_eq!(world.last_published(), Some(Vec::new()), "it left the NOT RUNNING list");
}

/// A boot refusal that is NOT the gatewayhub's (no credentials, a price feed held for the restart path) is
/// not asked again by the clock.
#[test]
fn an_account_booted_without_credentials_is_not_asked_by_the_clock() {
    let world = Scripted::default();
    world.rows(&AdapterPolicy::default().with_account("binance", &alt(), Demo));
    let waiting = vec![NotRunningAccount {
        venue: "binance".to_string(),
        account: Some(alt()),
        route_key: "binance#ALT".to_string(),
        tier: Some(Demo),
        reason: ArmingBlock::NoCredentials.as_str().to_string(),
    }];
    let mut watcher = Watcher::new(world.clone(), Vec::new(), waiting);
    world.advance(MAX_POLL * 10);
    watcher.tick();
    assert!(world.started().is_empty());
}

/// **Switching an account off and on within seconds works.** The stop begins the old lease's close; a start
/// 3 s later can meet that lease still held and is refused `adapter failed`. That refusal is retried (the
/// account was stopped by this watcher moments ago) and the account runs once the lease is gone, with no
/// further change of the rows.
#[test]
fn an_account_switched_off_and_on_within_seconds_starts_when_its_old_lease_is_gone() {
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();
    assert_eq!(world.stops(), vec!["binance#ALT".to_string()]);

    world.advance(Duration::from_secs(3));
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
    );
    world.change();
    world.next_start(not_running(ArmingBlock::AdapterFailed));
    watcher.tick();
    assert_eq!(world.started().len(), 1, "the first start met the closing lease");
    assert!(!watcher.running.contains_key("binance#ALT"));

    world.advance(FIRST_POLL);
    world.next_start(started("binance#ALT", Demo));
    watcher.tick();
    assert_eq!(world.started().len(), 2, "asked again on the backoff, with no row change");
    assert!(watcher.running.contains_key("binance#ALT"));
    assert_eq!(world.last_published(), Some(Vec::new()));
}

/// The same refusal with no stop of ours behind it (a lease another client holds), or long after the
/// stop, is the venue's own and stands until the rows change: nothing loops on it.
#[test]
fn an_adapter_refusal_that_is_not_a_closing_lease_is_not_retried() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    world.change();
    world.next_start(not_running(ArmingBlock::AdapterFailed));
    watcher.tick();
    world.advance(MAX_POLL * 10);
    watcher.tick();
    assert_eq!(world.started().len(), 1, "never stopped here: a lease held elsewhere");

    // Stopped here, but the grace has run out by the time it is started again.
    let (world, mut watcher) = node_running_two_accounts();
    world.rows(&alt_deactivated());
    world.change();
    watcher.tick();
    world.advance(LEASE_CLOSE_GRACE + Duration::from_secs(1));
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
    );
    world.change();
    world.next_start(not_running(ArmingBlock::AdapterFailed));
    watcher.tick();
    world.advance(MAX_POLL * 10);
    watcher.tick();
    assert_eq!(world.started().len(), 1);
}

/// A permanent refusal is not retried, whatever the clock says.
#[test]
fn a_permanent_refusal_is_not_retried_by_the_clock() {
    let (world, mut watcher) = node_with_a_second_demo_account();
    world.change();
    world.next_start(not_running(ArmingBlock::LiveCredentialsAbsent));
    watcher.tick();
    world.advance(MAX_POLL * 10);
    watcher.tick();
    assert_eq!(world.started().len(), 1);
}

/// A backoff running out follows nothing but the accounts that are due: it stops no account, reports no
/// difference, and an account deactivated meanwhile is simply not asked.
#[test]
fn a_backoff_asks_only_the_due_accounts_and_not_one_deactivated_meanwhile() {
    let (world, mut watcher) = node_running_two_accounts();
    // `ALT` is running, `ALT2` is refused for want of the gatewayhub.
    let alt2 = AccountLabel::parse("ALT2").expect("a legal label");
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo)
            .with_account("binance", &alt2, Demo),
    );
    world.change();
    world.next_start(HotStart::NotRunning(NotRunning {
        venue: "binance".to_string(),
        account: alt2.clone(),
        tier: Some(Demo),
        block: ArmingBlock::GatewayUnavailable,
    }));
    watcher.tick();
    assert_eq!(world.started(), vec!["binance#ALT2".to_string()]);

    // The operator deactivates `ALT2`, but no commit is seen (no change signal): the clock alone
    // follows nothing but the due account, and that one is no longer wanted.
    world.rows(&with_inactive_row(
        AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Demo),
        "binance",
        &alt2,
        Demo,
    ));
    world.advance(FIRST_POLL);
    watcher.tick();
    assert!(world.stops().is_empty(), "a backoff stops nothing");
    assert_eq!(world.started().len(), 1, "an account whose row went inactive is not asked");
    assert!(watcher.retry.is_empty(), "and its retry is dropped");
}

/// `flags.tradehub_live` off: a `live` account is NOT RUNNING `flags.tradehub_live off` and the world is
/// never asked to start it (the switch stops, it never demotes), nor is it retried; a `demo` account is
/// not affected by the switch.
#[test]
fn a_live_account_is_not_started_while_the_live_switch_is_off() {
    let world = Scripted::default();
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Live),
    );
    let mut watcher = Watcher::new(world.clone(), vec!["binance".to_string()], Vec::new())
        .with_live_switch(false);
    world.change();
    watcher.tick();
    assert!(world.started().is_empty(), "the world was never asked");
    let listed = world.last_published().expect("published");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].route_key, "binance#ALT");
    assert_eq!(listed[0].tier, Some(Live));
    assert_eq!(listed[0].reason, ArmingBlock::LiveSwitchOff.as_str());
    world.advance(MAX_POLL * 4);
    watcher.tick();
    assert!(world.started().is_empty(), "and the clock does not ask either");
}

/// The same account with the switch on is started, at its own tier.
#[test]
fn a_live_account_is_started_while_the_live_switch_is_on() {
    let world = Scripted::default();
    world.rows(
        &AdapterPolicy::default()
            .with_account("binance", &AccountLabel::Default, Demo)
            .with_account("binance", &alt(), Live),
    );
    let mut watcher = Watcher::new(world.clone(), vec!["binance".to_string()], Vec::new());
    world.change();
    world.next_start(started("binance#ALT", Live));
    watcher.tick();
    assert_eq!(world.started(), vec!["binance#ALT".to_string()]);
    assert_eq!(watcher.running.get("binance#ALT"), Some(&Some(Live)));
}

/// **A hot start tells the door the ceiling of the engine it built, from the rows it built it from**
/// (the door used to answer from the boot's rows both ways: an account that gained a cap was refused,
/// one that lost it stayed admitted). Only a start that built an engine says anything.
#[test]
fn a_started_engine_tells_the_door_its_own_ceiling() {
    let capped = AdapterPolicy::default()
        .with_account_row(vike_secrets::Account {
            id: 1,
            venue: "binance".to_string(),
            tier: Live,
            label: Some("ALT".to_string()),
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: vike_secrets::MaxExposure::new(5000.0),
        })
        .accounts;
    let uncapped = AdapterPolicy::default().with_account("binance", &alt(), Live).accounts;
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| true);
    // What the door reads WHILE the start runs (the engine is not visible yet): the new engine's own answer.
    let during = std::cell::Cell::new(None);
    let note = |policy_cap, rows: &AccountDirectory, outcome: HotStart| {
        start_with_cap(Some(&facts), policy_cap, rows, "binance", &alt(), || {
            during.set(Some(facts.exposure_cap_armed("binance#ALT")));
            outcome
        });
    };

    note(false, &uncapped, not_running(ArmingBlock::AdapterFailed));
    assert_eq!(during.get(), Some(false), "announced before the start, from the rows it uses");
    assert!(
        facts.exposure_cap_armed("binance#ALT"),
        "no engine was built: the previous answer is back"
    );
    note(true, &uncapped, HotStart::AlreadyRunning);
    assert_eq!(during.get(), Some(true));
    assert!(facts.exposure_cap_armed("binance#ALT"), "and again after a start that built nothing");
    let before = facts.generation("binance#ALT");

    note(false, &uncapped, started("binance#ALT", Live));
    assert!(
        !facts.exposure_cap_armed("binance#ALT"),
        "the engine has no ceiling, whatever the boot saw"
    );
    note(true, &uncapped, started("binance#ALT", Live));
    assert!(facts.exposure_cap_armed("binance#ALT"), "the box-wide row arms every engine");
    note(false, &capped, started("binance#ALT", Live));
    assert!(facts.exposure_cap_armed("binance#ALT"), "the account's own row arms it");
    assert!(facts.generation("binance#ALT") > before, "every start moves the account's generation");
}
