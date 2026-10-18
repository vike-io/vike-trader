//! An account STOPPED in a running core (decision 0169, slice S2), driven on the test thread: the
//! pieces that need the private state of a `CoreThread` — the tombstone list, the route-key lookup, the
//! venue counts, the capital base and the link dead-man's windows. The same change through a spawned core
//! is `tests/wiring/hot_engine_stop.rs`.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_model::TradingState;
use vike_model::{FeedStatus, OrderRequest};

use crate::runtime::test_support::{core_of, engine_on};

const VENUE: &str = "sim";
const ALT_KEY: &str = "sim#ALT";

fn alt_label() -> vike_model::accounts::account_keys::AccountLabel {
    vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label")
}

/// The second account of the venue, trading `symbol`.
fn alt_engine(symbol: &str) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_on(VENUE, symbol, RecordingClient::default());
    e.route_key = ALT_KEY.to_string();
    e
}

fn stop_of_alt() -> vike_exec::NotRunningAccount {
    vike_exec::NotRunningAccount {
        venue: VENUE.to_string(),
        account: Some(alt_label()),
        route_key: ALT_KEY.to_string(),
        tier: None,
        reason: "account inactive".to_string(),
    }
}

/// ⚠ The clock is injected (always 0): `dispatch` re-stamps every message from `config.clock`, and under
/// the wall clock a disclosed disconnect is stamped with an epoch no injected sweep time is past, so the
/// trip tests would read green for the wrong reason (`link_deadman.rs`'s `StepClock` argues it).
fn dead_man(action: DeadManAction) -> CoreConfig {
    CoreConfig {
        clock: Box::new(|| 0),
        link_deadman: Some(LinkDeadManConfig {
            grace: std::time::Duration::from_millis(1000),
            action,
            venues: [VENUE.to_string()].into_iter().collect(),
        }),
        ..Default::default()
    }
}

/// A resting order in the book of engine `idx`, placed through the engine route so a two-account venue
/// does not refuse it as ambiguous.
fn rest(
    c: &mut CoreThread<RecordingClient>,
    idx: usize,
    coid: &str,
    symbol: &str,
    cancel_intent: CancelIntent,
) {
    c.apply_intent_routed(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: coid.into(),
            venue: VENUE.into(),
            symbol: symbol.into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        })),
        0,
        cancel_intent,
        EngineRoute::Engine(idx),
    );
}

fn trade_link<C: ExecutionClient>(c: &mut CoreThread<C>, symbol: &str, status: FeedStatus) {
    c.dispatch(Ingest::StreamStatus(Box::new(StreamStatusUpdate {
        venue: VENUE.to_string(),
        symbol: symbol.to_string(),
        stream: vike_venues::link_deadman::TRADE_LINK_STREAM.to_string(),
        status,
    })));
}

/// Two accounts of one venue, each on its own symbol, each holding one resting order.
fn two_accounts(config: CoreConfig) -> CoreThread<RecordingClient> {
    let mut c = core_of(
        engine_on(VENUE, "BTCUSDT", RecordingClient::default()),
        vec![(500.0, alt_engine("ETHUSDT"))],
        config,
    );
    rest(&mut c, 0, "d1", "BTCUSDT", CancelIntent::Unspecified);
    rest(&mut c, 1, "a1", "ETHUSDT", CancelIntent::Unspecified);
    c
}

/// The stop cancels the stopped account's own book and nobody else's, tombstones its index, keeps the
/// engine, and lists the account NOT RUNNING with the reason it was given.
#[test]
fn a_stop_cancels_its_own_book_tombstones_the_index_and_lists_the_account() {
    let mut c = two_accounts(CoreConfig::default());
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);

    assert_eq!(c.extra_engines[0].1.client.cancels, vec!["a1".to_string()]);
    assert!(c.engine.client.cancels.is_empty(), "the other account's order is not touched");
    assert_eq!(c.retired_engines, vec![1]);
    assert_eq!(c.extra_engines.len(), 1, "the engine stays in the list");
    assert!(
        c.config
            .not_running
            .iter()
            .any(|nr| nr.route_key == ALT_KEY && nr.reason == "account inactive")
    );
    assert!(c.is_retired(1) && !c.is_retired(0));
}

/// A second stop of the same account, and a stop of one that never ran, are notes and change nothing.
#[test]
fn a_repeated_or_unknown_stop_is_a_note() {
    let mut c = two_accounts(CoreConfig::default());
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 11);
    assert_eq!(c.retired_engines, vec![1], "tombstoned once");
    assert_eq!(c.config.not_running.len(), 1, "listed once");

    let mut unknown = stop_of_alt();
    unknown.route_key = "sim#NOPE".to_string();
    c.apply_account_change(AccountChange::StopEngine { not_running: unknown }, 12);
    assert_eq!(c.retired_engines, vec![1]);
}

/// ⚠ The dead-man half. The stopped account's trade link was DOWN when it stopped; a link closed on
/// purpose never says `Live`, so without the stop forgetting that window the grace would expire into a
/// `CancelAllAndHalt` for the whole venue, the other account included.
#[test]
fn a_trade_link_outage_disclosed_before_the_stop_does_not_trip_the_venue_afterwards() {
    let mut c = two_accounts(dead_man(DeadManAction::CancelAllAndHalt));
    trade_link(&mut c, "ETHUSDT", FeedStatus::Disconnected);
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);

    c.sweep_link_deadman(100_000);
    assert_eq!(c.engine.trading_state, TradingState::Active, "no halt for a link we closed");
    assert!(c.engine.client.cancels.is_empty(), "and no venue-wide cancel of the other account");
}

/// The control for the test above: the same outage WITHOUT the stop does trip, so the test above
/// passes because of the stop and not because the window never opened.
#[test]
fn the_same_outage_without_a_stop_does_trip_the_venue() {
    let mut c = two_accounts(dead_man(DeadManAction::CancelAllAndHalt));
    trade_link(&mut c, "ETHUSDT", FeedStatus::Disconnected);
    c.sweep_link_deadman(100_000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
}

/// The account's own trade-link transition, as its client files it beside the `StreamStatus`.
fn exec_link<C: ExecutionClient>(c: &mut CoreThread<C>, route_key: &str, up: bool) {
    c.dispatch(Ingest::ExecLink(Box::new(vike_exec::ExecLinkUpdate {
        route_key: route_key.into(),
        venue: VENUE.into(),
        up,
        at_ts_ms: 0,
    })));
}

/// Two accounts on ONE `(venue, symbol)` share one trade-link window. A window a RUNNING account of that key
/// still owns (its own link is disclosed down) is not forgotten by the other's stop: its `Live` closes it,
/// and a real outage must still trip.
#[test]
fn a_window_a_running_account_still_owns_is_kept() {
    let mut c = core_of(
        engine_on(VENUE, "BTCUSDT", RecordingClient::default()),
        vec![(500.0, alt_engine("BTCUSDT"))],
        dead_man(DeadManAction::CancelAllAndHalt),
    );
    trade_link(&mut c, "BTCUSDT", FeedStatus::Disconnected);
    exec_link(&mut c, VENUE, false);
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    c.sweep_link_deadman(100_000);
    assert_eq!(c.engine.trading_state, TradingState::Halted);
}

/// The other half of the shared key (decision 0169, slice S4): when the account that stays is UP, the window
/// can only be the stopped account's, and a link closed on purpose never says `Live`, so it is forgotten
/// instead of expiring into a `CancelAllAndHalt` for the survivor's venue.
#[test]
fn a_shared_window_the_surviving_account_does_not_own_is_forgotten() {
    let mut c = core_of(
        engine_on(VENUE, "BTCUSDT", RecordingClient::default()),
        vec![(500.0, alt_engine("BTCUSDT"))],
        dead_man(DeadManAction::CancelAllAndHalt),
    );
    trade_link(&mut c, "BTCUSDT", FeedStatus::Disconnected);
    exec_link(&mut c, ALT_KEY, false);
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    c.sweep_link_deadman(100_000);
    assert_eq!(c.engine.trading_state, TradingState::Active, "no halt for a link we closed");
    assert!(c.engine.client.cancels.is_empty());
}

/// After a stop the venue has ONE running account: an order that names only the exchange is not
/// "ambiguous" any more, and the counts do not include the stopped engine.
#[test]
fn a_stopped_account_is_not_an_account_of_the_exchange() {
    let mut c = two_accounts(CoreConfig::default());
    assert_eq!(c.accounts_of_venue(VENUE), 2);
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    assert_eq!(c.accounts_of_venue(VENUE), 1);
    assert_eq!(c.engines_of_venue(VENUE), vec![0]);
    assert!(
        c.ambiguous_accounts(EngineRoute::Payload, VENUE).is_none(),
        "one running account is not an ambiguity"
    );
}

/// The account starts again: a NEW engine under the same route key. The lookup answers the new one, the
/// stopped one stays at its index, the duplicate guard accepts it, and its seed leaves the base while
/// the new seed enters it (the latch peak moves with both, so the restart is neither a new high nor a
/// drawdown).
#[test]
fn a_stopped_account_starts_again_as_a_new_engine_the_lookup_prefers() {
    let mut c = two_accounts(CoreConfig::default());
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    c.pnl_curve_peak = Some(10_500.0);
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    assert_eq!(
        c.engine_idx_for_route_key(RouteKey::declared(ALT_KEY)),
        Some(1),
        "a stopped engine stays reachable while nothing runs under its key"
    );

    c.apply_account_change(AccountChange::add_engine(500.0, alt_engine("ETHUSDT")), 20);
    assert_eq!(c.extra_engines.len(), 2, "the list grew");
    assert_eq!(c.engine_idx_for_route_key(RouteKey::declared(ALT_KEY)), Some(2));
    assert_eq!(c.retired_engines, vec![1], "the old index stays a tombstone");
    assert_eq!(c.extra_engines[0].0, 0.0, "the stopped engine's seed left the capital base");
    assert_eq!(c.extra_engines[1].0, 500.0);
    assert_eq!(c.pnl_curve_peak, Some(10_500.0), "-500 for the old seed, +500 for the new one");
    assert_eq!(c.accounts_of_venue(VENUE), 2);
    assert!(
        c.recent.iter().any(|l| l.contains("ACCOUNT STARTED")),
        "the restart is said, and not refused as a duplicate: {:?}",
        c.recent
    );
}

/// A client that only records whether its detach began.
struct DetachFlag(Arc<std::sync::atomic::AtomicBool>);

impl ExecutionClient for DetachFlag {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
    fn begin_detach(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Phase two begins the detach of the STOPPED engine's client and no other, forgets the dead-man window a
/// link that dropped between the phases opened, and is a note for an account that was not stopped.
#[test]
fn a_detach_begins_only_the_stopped_clients_detach_and_closes_a_window_opened_in_between() {
    let flag = || Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (default_flag, alt_flag) = (flag(), flag());
    let mut alt = engine_on(VENUE, "ETHUSDT", DetachFlag(Arc::clone(&alt_flag)));
    alt.route_key = ALT_KEY.to_string();
    let mut c = core_of(
        engine_on(VENUE, "BTCUSDT", DetachFlag(Arc::clone(&default_flag))),
        vec![(500.0, alt)],
        dead_man(DeadManAction::CancelAllAndHalt),
    );
    let began =
        |f: &Arc<std::sync::atomic::AtomicBool>| f.load(std::sync::atomic::Ordering::Relaxed);

    // A running account is not "stopped": a detach for it is a note and detaches nothing.
    c.apply_account_change(AccountChange::DetachEngine { route_key: ALT_KEY.to_string() }, 5);
    assert!(!began(&alt_flag) && !began(&default_flag));

    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    assert!(!began(&alt_flag), "phase one leaves the client attached");

    // The link drops between the two phases: a window opens that no `Live` will ever close.
    trade_link(&mut c, "ETHUSDT", FeedStatus::Disconnected);
    c.apply_account_change(AccountChange::DetachEngine { route_key: ALT_KEY.to_string() }, 20);
    assert!(began(&alt_flag), "phase two begins the stopped client's detach");
    assert!(!began(&default_flag), "and only that one");
    c.sweep_link_deadman(100_000);
    assert_eq!(c.engine.trading_state, TradingState::Active, "the window was closed again");
}

/// How many orders engine `idx`'s client was sent since `before`.
fn sent_since(c: &CoreThread<RecordingClient>, idx: usize, before: usize) -> usize {
    c.eng(idx).client.submissions.len() - before
}

/// A BRACKET and a COMBO addressed to a stopped account are refused with a terminal lifecycle, like a
/// single order: the client is still attached until the detach, so anything it is sent rests at the venue
/// after the mass cancel. A mounted strategy reaches them through `EngineRoute::Mount`, not the edge.
#[test]
fn a_bracket_and_a_combo_for_a_stopped_account_never_reach_its_attached_client() {
    let mut c = two_accounts(CoreConfig::default());
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    let before = c.eng(1).client.submissions.len();

    let bracket = vike_model::BracketSpec {
        venue: VENUE.into(),
        symbol: "ETHUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    };
    let coids = c.apply_intent_routed(
        OrderIntent::Bracket(Box::new(bracket)),
        11,
        CancelIntent::Unspecified,
        EngineRoute::Engine(1),
    );
    assert_eq!(coids.len(), 3, "the refusal names the three legs it refused");
    assert_eq!(sent_since(&c, 1, before), 0, "the bracket entry was sent to the stopped client");
    assert!(
        coids.iter().all(|k| c.eng(1).registry.get(k).is_some_and(|o| o.status.is_terminal())),
        "every leg is terminal, none vanished"
    );

    let combo = vike_model::ComboSpec {
        venue: VENUE.into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::ComboLeg { symbol: "ETH-A".into(), ratio: 1 },
            vike_model::ComboLeg { symbol: "ETH-B".into(), ratio: -1 },
        ],
        net_limit: Some(-0.01),
        time_in_force: vike_model::TimeInForce::Gtc,
    };
    let coids = c.lower_combo(combo, 12, true, EngineRoute::Engine(1));
    assert_eq!(coids.len(), 1);
    assert_eq!(sent_since(&c, 1, before), 0, "the combo was sent to the stopped client");
    assert!(c.eng(1).registry.get(&coids[0]).is_some_and(|o| o.status.is_terminal()));
}

/// An order for a venue no engine claims goes to engine 0 (`unwrap_or(0)`); when engine 0 is a STOPPED
/// primary it is refused all the same.
#[test]
fn an_unrouted_order_that_falls_to_a_stopped_primary_is_refused() {
    let mut c = two_accounts(CoreConfig::default());
    c.apply_account_change(
        AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: VENUE.to_string(),
                account: None,
                route_key: VENUE.to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    assert!(c.is_retired(0));
    let before = c.eng(0).client.submissions.len();
    let coids = c.apply_intent_routed(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: "orphan".into(),
            venue: "nowhere".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        })),
        11,
        CancelIntent::Unspecified,
        EngineRoute::Payload,
    );
    assert_eq!(coids, vec!["orphan".to_string()]);
    assert_eq!(sent_since(&c, 0, before), 0, "the stopped primary's client was sent the order");
}

/// A strategy MOUNTED on the account follows it across a stop and a start: it is bound to the new engine,
/// not left on the tombstone (which refuses everything it sends and never sees the new engine's events).
#[test]
fn a_mount_bound_to_a_stopped_account_is_repointed_when_the_account_starts_again() {
    let mut c = two_accounts(CoreConfig::default());
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    c.mount_engine = vec![1, 0];
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);
    assert_eq!(c.mount_engine, vec![1, 0], "while it is stopped the mount keeps its index");
    c.apply_account_change(AccountChange::add_engine(500.0, alt_engine("ETHUSDT")), 20);
    assert_eq!(c.mount_engine, vec![2, 0], "the mount on the account follows it to the new engine");
}

/// The stopped PRIMARY's seed is `CoreConfig::seed_cash`, not an entry of the list. When the account starts
/// again as a new engine its seed leaves the base with the old one, or the capital would be counted twice
/// for the rest of the process (decision 0169, slice S4): the latch's base, the published capital base and
/// the peak all follow.
#[test]
fn a_stopped_primary_that_starts_again_does_not_count_its_seed_twice() {
    let mut c = two_accounts(CoreConfig::default());
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    c.config.seed_cash = 10_000.0;
    c.pnl_curve_peak = Some(10_500.0);
    let stop_primary = vike_exec::NotRunningAccount {
        venue: VENUE.to_string(),
        account: None,
        route_key: VENUE.to_string(),
        tier: None,
        reason: "account inactive".to_string(),
    };
    c.apply_account_change(AccountChange::StopEngine { not_running: stop_primary }, 10);
    assert_eq!(c.config.seed_cash, 10_000.0, "a stop alone is a zero step");

    c.apply_account_change(
        AccountChange::add_engine(
            10_000.0,
            engine_on(VENUE, "BTCUSDT", RecordingClient::default()),
        ),
        20,
    );
    assert_eq!(c.config.seed_cash, 0.0, "the stopped primary's seed left the base");
    assert_eq!(c.extra_engines.last().map(|(seed, _)| *seed), Some(10_000.0));
    assert_eq!(c.pnl_curve_peak, Some(10_500.0), "-10000 for the old seed, +10000 for the new one");
    let base = vike_model::py_sum(
        std::iter::once(c.config.seed_cash).chain(c.extra_engines.iter().map(|(s, _)| *s)),
    );
    assert_eq!(base, 10_500.0, "the account's capital is in the base once");
}

/// A stopped account takes no more reconcile passes, so its held divergences could never clear and a confirm
/// would fold the venue's side into a shut book: they go with the stop, announcement state included, and
/// another account's stay.
#[test]
fn a_stopped_accounts_held_divergences_are_dropped_and_the_others_kept() {
    use crate::runtime::core_thread::HeldReconAlert;
    use crate::runtime::recon_held::HeldId;
    use vike_exec::recon::DivergenceKind;

    let mut c = two_accounts(CoreConfig::default());
    for (id, key) in [(1_u64, VENUE), (2, ALT_KEY)] {
        let identity =
            HeldId::new(VENUE, key, DivergenceKind::OrphanLocalOrder, Some("o1"), "", &[]);
        c.recon_announce.observe(key, 0, [identity.clone()].into_iter().collect());
        c.recon_alerts.insert(
            id,
            HeldReconAlert {
                venue: VENUE.to_string(),
                route_key: key.to_string(),
                kind: DivergenceKind::OrphanLocalOrder,
                detail: String::new(),
                proposed_events: Vec::new(),
                recover_orders: Vec::new(),
                orphan_position: None,
                confirm_refused: false,
                identity,
                raised_seq: 0,
            },
        );
    }
    assert_eq!((c.recon_announce.held(VENUE), c.recon_announce.held(ALT_KEY)), (1, 1));

    c.apply_account_change(AccountChange::StopEngine { not_running: stop_of_alt() }, 10);

    assert_eq!(
        c.recon_alerts.keys().copied().collect::<Vec<_>>(),
        vec![1],
        "only the other's stays"
    );
    assert_eq!((c.recon_announce.held(VENUE), c.recon_announce.held(ALT_KEY)), (1, 0));
    assert!(c.recent.iter().any(|l| l.contains("held reconcile divergence")), "{:?}", c.recent);
}

/// A hot start never reopens risk the core has closed (decision 0169 item 8): an engine added while the
/// drawdown latch has put the core in `Reducing` starts `Reducing`, and one added while it is `Halted` starts
/// `Halted`.
#[test]
fn an_engine_added_under_a_closed_risk_state_starts_in_it() {
    for state in [TradingState::Reducing, TradingState::Halted] {
        let mut c = two_accounts(CoreConfig::default());
        c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
        c.engine.trading_state = state;
        let mut third = engine_on(VENUE, "SOLUSDT", RecordingClient::default());
        third.route_key = "sim#THIRD".to_string();
        c.apply_account_change(AccountChange::add_engine(100.0, third), 20);
        let added = &c.extra_engines.last().expect("the engine joined").1;
        assert_eq!(added.trading_state, state, "a new account never trades where the core may not");
    }
}

/// The link dead-man halts a VENUE, not the node (decision 0180): an account added while its venue is link-halted
/// joins the halt and is released with it on the venue's `Live`; an account of ANOTHER venue added while the
/// primary is link-halted does not inherit that halt as if it were node-wide.
#[test]
fn an_engine_added_under_a_venue_link_halt_follows_its_own_venue() {
    let mut c = two_accounts(dead_man(DeadManAction::CancelAllAndHalt));
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    trade_link(&mut c, "BTCUSDT", FeedStatus::Disconnected);
    c.sweep_link_deadman(100_000);
    assert_eq!(c.engine.trading_state, TradingState::Halted, "the venue is link-halted");

    let mut same = engine_on(VENUE, "SOLUSDT", RecordingClient::default());
    same.route_key = "sim#THIRD".to_string();
    c.apply_account_change(AccountChange::add_engine(100.0, same), 20);
    let added = &c.extra_engines.last().expect("the engine joined").1;
    assert_eq!(
        added.trading_state,
        TradingState::Halted,
        "an account of the halted venue joins it"
    );

    let other = engine_on("other", "SOLUSDT", RecordingClient::default());
    c.apply_account_change(AccountChange::add_engine(100.0, other), 21);
    let added = &c.extra_engines.last().expect("the engine joined").1;
    assert_eq!(
        added.trading_state,
        TradingState::Active,
        "another venue's link is not its problem"
    );

    trade_link(&mut c, "BTCUSDT", FeedStatus::Live);
    c.sweep_link_deadman(100_100);
    for i in 0..=3 {
        assert_eq!(
            c.eng(i).trading_state,
            TradingState::Active,
            "engine {i} is released with the venue"
        );
    }
}
