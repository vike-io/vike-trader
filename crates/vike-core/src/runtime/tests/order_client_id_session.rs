//! The core's order client id SESSION at boot (decision 0270, row R3), white-box: each engine's minting plan comes
//! from `vike_venues::venue_order_client_id_budget::order_client_id_shape_for` and its account code (primary, extra and
//! appended engines, and a replay core's rebuild); the one-time switch to a fresh v2 session (a v1
//! session the boot restore resumed, or one past the roll point) writes a `Snapshot` before the first mint;
//! the late-retry memory is rebuilt at boot from a `Snapshot` and its tail and kept across restarts; an
//! offline replay that crosses the switch `Snapshot` mints under its session; and a refold of a v2 journal
//! mints a strategy's ids (`S` and its code) byte for byte as the live core did.

use super::*;
use crate::replay::{RestoreOutcome, replay_offline, restore_for_boot};
use crate::runtime::test_support::{core_of, core_with, sim_engine_with};
use crate::scratch::Scratch;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{AccountChange, OrderIntent};
use vike_journal::{Journal, JournalFileConfig, JournalRecord};
use vike_model::OrderClientIdOrigin;
use vike_model::events::{Event, OrderRejected};
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdReading, OrderClientIdShape, order_client_id_roll_point,
    parse_order_client_id,
};
use vike_venues::venue_order_client_id_budget::order_client_id_shape_for;

const HOUR: i64 = 3_600_000;

fn journal(dir: &Scratch) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    }
}

/// A journaled core on `sim` resuming the v1 session `cafef00d` the way a test hands one in.
fn v1_core(
    dir: &Scratch,
    recent_order_client_ids: Vec<(String, i64)>,
) -> CoreThread<RecordingClient> {
    core_with(CoreConfig {
        order_client_id_session: Some(("cafef00d".into(), 0)),
        journal: Some(journal(dir)),
        recent_order_client_ids,
        ..CoreConfig::default()
    })
}

/// The core a boot assembles over `restore`, on the same journal.
fn booted(dir: &Scratch, restore: crate::replay::RestoreReport) -> CoreThread<RecordingClient> {
    let mut config = CoreConfig { journal: Some(journal(dir)), ..CoreConfig::default() };
    restore.apply_to(&mut config);
    core_with(config)
}

/// Submit a limit through the journaled door (a `Command`, write-ahead) and answer the id the client got.
fn submit(c: &mut CoreThread<RecordingClient>, order_client_id: &str) -> String {
    c.dispatch(Ingest::Command(vike_execution::Command::Order(OrderIntent::Submit(Box::new(
        vike_model::OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            price: Some(100.0),
            order_type: vike_model::OrderType::Limit,
            ..Default::default()
        },
    )))));
    c.engine
        .client
        .submissions
        .last()
        .expect("the order reached the client")
        .order_client_id
        .clone()
}

fn snaps(dir: &Scratch) -> Vec<(String, u64)> {
    Journal::read_all(dir)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Snapshot {
                order_client_id_session, order_client_id_sequence, ..
            } => Some((order_client_id_session, order_client_id_sequence)),
            _ => None,
        })
        .collect()
}

fn ids(pairs: &[(String, i64)]) -> std::collections::BTreeSet<String> {
    pairs.iter().map(|(id, _)| id.clone()).collect()
}

/// **Plans follow the venue table**: the primary (a Binance engine with a 12-character broker code under
/// a 1-character tag: the narrowest tagged compact shape), an extra (Bybit: full) and an engine appended
/// at run time each get `order_client_id_shape_for(venue, broker code length, tag cost)` and their own account
/// code; a replay core rebuilt from the engine's snapshot builds the same plan.
#[test]
fn plans_follow_order_client_id_shape_for_on_the_primary_extra_and_appended_engines() {
    let mut binance = engine_on("binance", "BTCUSDT", RecordingClient::default());
    binance.broker_code_len = 12;
    binance.account_code = OrderClientIdAccountCode::of_id(7);
    let mut bybit = engine_on("bybit", "ETHUSDT", RecordingClient::default());
    bybit.account_code = OrderClientIdAccountCode::of_id(4);
    let mut c = core_of(
        binance,
        vec![(1.0, bybit)],
        CoreConfig {
            instance_origin: OrderClientIdOrigin::parse("w").ok(),
            ..CoreConfig::default()
        },
    );
    assert!(c.order_client_id_gen.is_v2(), "a fresh session is v2");
    assert_eq!(c.order_client_id_gen.origin(), Some("w"), "stamped with the configured origin");
    let tag_cost = 2;
    assert_eq!(
        c.order_client_id_plans[0].shape(),
        order_client_id_shape_for("binance", 12, tag_cost)
    );
    assert_eq!(
        c.order_client_id_plans[0].shape().u_width,
        7,
        "non-vacuous: the tagged compact fit"
    );
    assert_eq!(c.order_client_id_plans[0].account(), OrderClientIdAccountCode::of_id(7));
    assert_eq!(c.order_client_id_plans[1].shape(), OrderClientIdShape::FULL);
    assert_eq!(c.order_client_id_plans[1].account(), OrderClientIdAccountCode::of_id(4));

    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    let mut alt = engine_on("binance", "SOLUSDT", RecordingClient::default());
    alt.route_key = "binance#ALT".into();
    alt.broker_code_len = 8;
    alt.account_code = OrderClientIdAccountCode::of_id(9);
    c.apply_account_change(AccountChange::add_engine(500.0, alt), 20);
    assert_eq!(c.order_client_id_plans.len(), 3, "the appended engine's plan sits at its index");
    assert_eq!(
        c.order_client_id_plans[2].shape(),
        order_client_id_shape_for("binance", 8, tag_cost)
    );
    assert_eq!(c.order_client_id_plans[2].account(), OrderClientIdAccountCode::of_id(9));

    let rebuilt = Engine::from_snapshot(&c.engine.snapshot_state(), RecordingClient::default());
    let plan = super::order_client_id_plan_of(&rebuilt, &c.order_client_id_gen);
    assert_eq!(
        (plan.shape(), plan.account()),
        (c.order_client_id_plans[0].shape(), c.order_client_id_plans[0].account())
    );
}

/// **The switch**: a v1 session the BOOT RESTORE resumed (`order_client_id_switch` set) becomes a fresh v2 one, and
/// ONE `Snapshot` naming it is the journal's first record, before any mint; the first id is v2. A v1 session a
/// caller hands in itself is resumed verbatim and writes nothing.
#[test]
fn a_v1_session_the_boot_restore_resumed_switches_and_snaps_before_the_first_mint() {
    let dir = Scratch::reserved("vco-order-client-id-switch");
    let mut c = core_with(CoreConfig {
        order_client_id_session: Some(("cafef00d".into(), 7)),
        order_client_id_switch: true,
        journal: Some(journal(&dir)),
        ..CoreConfig::default()
    });
    let (session, seq) = c.order_client_id_gen.state();
    assert!(c.order_client_id_gen.is_v2() && session != "cafef00d" && seq == 0, "{session} {seq}");
    let first = submit(&mut c, "");
    assert!(matches!(parse_order_client_id(&first), Some(OrderClientIdReading::V2(_))), "{first}");
    drop(c);
    let records = Journal::read_all(&dir).unwrap();
    assert!(
        matches!(&records[0], JournalRecord::Snapshot { order_client_id_session, order_client_id_sequence: 0, .. } if *order_client_id_session == session),
        "the Snapshot comes first: {:?}",
        records.first()
    );

    let kept = Scratch::reserved("vco-order-client-id-no-switch");
    let c = core_with(CoreConfig {
        order_client_id_session: Some(("cafef00d".into(), 7)),
        journal: Some(journal(&kept)),
        ..CoreConfig::default()
    });
    assert_eq!(
        c.order_client_id_gen.state(),
        ("cafef00d".to_string(), 7),
        "a handed-in session is kept"
    );
    drop(c);
    assert!(snaps(&kept).is_empty(), "and nothing is written");
}

/// **The roll point**: a restored v2 session keeps going below its engines' roll point and is replaced at it.
#[test]
fn a_restored_v2_session_rolls_at_its_roll_point() {
    let v2 = vike_model::orders::client_id::OrderClientIdGenerator::new_v2(None).state().0;
    let roll = order_client_id_roll_point([OrderClientIdShape::FULL]);
    let at = |seq: u64| {
        core_with(CoreConfig {
            order_client_id_session: Some((v2.clone(), seq)),
            order_client_id_switch: true,
            ..CoreConfig::default()
        })
        .order_client_id_gen
        .state()
    };
    assert_eq!(at(roll - 1), (v2.clone(), roll - 1), "below the roll point the session is kept");
    let (rolled, seq) = at(roll);
    assert!(rolled != v2 && seq == 0, "at the roll point a fresh one: {rolled} {seq}");
}

/// **The memory is the union, pruned at the restore's clock**: the `Snapshot`'s carried ids (one ages out
/// between the `Snapshot` and the restore), its registry (an ENDED order too), the tail's minted id and the
/// tail's explicit id that no order answered; and the core takes it as its memory.
#[test]
fn the_boot_memory_is_the_snaps_memory_and_registry_and_the_tails_ids_pruned_at_24h() {
    let dir = Scratch::reserved("vco-order-client-id-memory");
    let tmp = Scratch::created("vco-order-client-id-memory-tmp");
    let now = vike_model::now_ms();
    let carried = vec![("aging".to_string(), now - 23 * HOUR), ("carried".to_string(), now - HOUR)];
    let mut c = v1_core(&dir, carried);
    let ended = submit(&mut c, "");
    c.dispatch(Ingest::Event(Event::OrderRejected(OrderRejected {
        order_client_id: ended.clone(),
        reason: "no".into(),
        timestamp: 1,
        cause: None,
        refusal_id: None,
    })));
    assert!(c.engine.registry.get(&ended).is_some_and(|o| o.status.is_terminal()), "premise");
    c.write_snap(now);
    let minted = submit(&mut c, "");
    submit(&mut c, "retryme1");
    drop(c); // a crash: no exit Snapshot

    let restore =
        restore_for_boot(Some(&journal(&dir)), &tmp, now + 2 * HOUR, &[]).expect("readable");
    let want: std::collections::BTreeSet<String> =
        ["carried".to_string(), ended, minted, "retryme1".to_string()].into();
    assert_eq!(
        ids(restore.recent_order_client_ids()),
        want,
        "{:?}",
        restore.recent_order_client_ids()
    );
    let c = booted(&dir, restore);
    assert!(
        c.recent_order_client_ids.contains("retryme1")
            && !c.recent_order_client_ids.contains("aging")
    );
}

/// **Two restarts, one journal**: process 1 (v1) mints A; boot 2 switches, writes its `Snapshot` first,
/// remembers A and mints B under v2, then dies; boot 3 refolds from that `Snapshot` to the state hash process
/// 2 held, keeps its session (no second switch) and remembers A and B.
#[test]
fn two_restarts_keep_the_first_process_ids_and_the_switch_refolds_to_the_same_state_hash() {
    let dir = Scratch::reserved("vco-order-client-id-two-restarts");
    let tmp = Scratch::created("vco-order-client-id-two-restarts-tmp");
    let mut c1 = v1_core(&dir, Vec::new());
    let a = submit(&mut c1, "");
    assert!(a.starts_with("cafef00d"), "process 1 mints v1: {a}");
    c1.write_snap(vike_model::now_ms());
    drop(c1);
    let process_one = Journal::read_all(&dir).unwrap().len();

    let restore = restore_for_boot(Some(&journal(&dir)), &tmp, vike_model::now_ms(), &[]).unwrap();
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let mut c2 = booted(&dir, restore);
    let s2 = c2.order_client_id_gen.state().0;
    assert!(
        c2.order_client_id_gen.is_v2() && c2.recent_order_client_ids.contains(&a),
        "switched, and remembers A"
    );
    let b = submit(&mut c2, "");
    let live = vike_execution::state_hash(&[c2.engine.snapshot_state()]);
    drop(c2); // a crash: the switch Snapshot is the latest checkpoint
    let records = Journal::read_all(&dir).unwrap();
    assert!(
        matches!(&records[process_one], JournalRecord::Snapshot { order_client_id_session, order_client_id_sequence: 0, .. } if *order_client_id_session == s2),
        "boot 2's first record is the switch Snapshot: {:?}",
        records.get(process_one)
    );

    let restore = restore_for_boot(Some(&journal(&dir)), &tmp, vike_model::now_ms(), &[]).unwrap();
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    assert_eq!(
        vike_execution::state_hash(restore.restored_engines()),
        live,
        "the refold reproduces it"
    );
    assert_eq!(restore.order_client_id_session().map(|s| s.0.as_str()), Some(s2.as_str()));
    let c3 = booted(&dir, restore);
    assert_eq!(c3.order_client_id_gen.state().0, s2, "a v2 session below its roll point is kept");
    assert!(
        c3.recent_order_client_ids.contains(&a) && c3.recent_order_client_ids.contains(&b),
        "A and B"
    );
}

/// **A replay crossing the switch `Snapshot` mints under its session**: the offline fence over a journal whose
/// first `Snapshot` is process 1's (v1) and whose tail crosses boot 2's switch reproduces the final hash.
#[test]
fn an_offline_replay_that_crosses_the_switch_snap_adopts_its_session() {
    let dir = Scratch::reserved("vco-order-client-id-crossing");
    let tmp = Scratch::created("vco-order-client-id-crossing-tmp");
    let mut c1 = v1_core(&dir, Vec::new());
    c1.write_snap(vike_model::now_ms());
    drop(c1);
    let restore = restore_for_boot(Some(&journal(&dir)), &tmp, vike_model::now_ms(), &[]).unwrap();
    let mut c2 = booted(&dir, restore);
    let s2 = c2.order_client_id_gen.state().0;
    let b = submit(&mut c2, "");
    c2.write_snap(vike_model::now_ms());
    drop(c2);
    assert_eq!(snaps(&dir).len(), 3, "process 1's, the switch's and the exit's");

    let out = replay_offline(&dir).expect("the fence holds across the switch");
    assert_eq!(out.order_client_id_session, s2);
    assert!(out.engines[0].registry.iter().any(|(id, _)| *id == b), "{b} was re-minted");
}

/// **A replay across trade-id windows cuts as each process did** (`vike_execution::Engine::size_fill_window`):
/// process 1 reads back an hour (a four-day window), boot 2 switches sessions and reads back thirty days, boot 3
/// keeps the session (no `Snapshot` at its boot) and reads back ten. Fills over four weeks make each window keep
/// another set of ids, and the offline fence over the whole journal holds: each replay core cuts by the window the
/// process it replays was told.
#[test]
fn an_offline_replay_across_window_changes_cuts_as_each_process_did() {
    use std::sync::atomic::{AtomicI64, Ordering};
    const DAY: i64 = 24 * HOUR;
    const T0: i64 = 20_000 * DAY;
    let dir = Scratch::reserved("vco-order-client-id-windows");
    let tmp = Scratch::created("vco-order-client-id-windows-tmp");
    let t = std::sync::Arc::new(AtomicI64::new(T0));
    let clock = || -> Box<dyn vike_model::Clock + Send> {
        let t = std::sync::Arc::clone(&t);
        Box::new(move || t.load(Ordering::SeqCst))
    };
    let sized = |lookback_ms: i64| {
        // as `spawn_core_multi` sizes it from `CoreConfig::reconcile_lookback_ms`, before the assembly's `Snapshot`
        let mut e = sim_engine_with(RecordingClient::default());
        e.size_fill_window(lookback_ms);
        e
    };
    // One bare fill a day over `days`, each folded at its own day.
    let fold_days = |c: &mut CoreThread<RecordingClient>, days: std::ops::Range<i64>| {
        for d in days {
            let ts = T0 + d * DAY + HOUR;
            t.store(ts, Ordering::SeqCst);
            let fill = vike_model::events::FillEvent {
                trade_id: vike_model::events::TradeId::prefixed("w-", d),
                order_client_id: String::new(),
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: if d % 2 == 0 { 1 } else { -1 },
                last_qty: 1.0,
                last_price: 100.0 + d as f64,
                commission: 0.0,
                commission_asset: String::new().into(),
                liquidity_side: "taker".into(),
                timestamp: ts,
                mark_price: None,
                position_side: vike_model::events::PositionSide::Both,
            };
            c.dispatch(Ingest::Event(Event::Fill(fill)));
        }
    };
    let exit = |c: &mut CoreThread<RecordingClient>| {
        let now = c.config.clock.now_ms();
        c.write_snap(now);
    };
    let boot = |lookback_ms: i64| {
        let now = t.load(Ordering::SeqCst);
        let restore = restore_for_boot(Some(&journal(&dir)), &tmp, now, &[]).unwrap();
        assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
        let mut config =
            CoreConfig { journal: Some(journal(&dir)), clock: clock(), ..CoreConfig::default() };
        restore.apply_to(&mut config);
        core_of(sized(lookback_ms), Vec::new(), config)
    };

    let mut c1 = core_of(
        sized(HOUR),
        Vec::new(),
        CoreConfig {
            order_client_id_session: Some(("cafef00d".into(), 0)),
            journal: Some(journal(&dir)),
            clock: clock(),
            ..CoreConfig::default()
        },
    );
    exit(&mut c1); // the replay's base
    fold_days(&mut c1, 0..10);
    exit(&mut c1);
    drop(c1);
    let mut c2 = boot(30 * DAY);
    assert!(c2.order_client_id_gen.is_v2(), "boot 2 switched, and wrote its Snapshot");
    fold_days(&mut c2, 12..15);
    exit(&mut c2);
    drop(c2);
    let mut c3 = boot(10 * DAY);
    fold_days(&mut c3, 25..28);
    exit(&mut c3);
    drop(c3);

    let snapshots: Vec<(Option<u32>, usize)> = Journal::read_all(&dir)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Snapshot { engines, .. } => {
                Some((engines[0].fill_window_days, engines[0].seen_trade_ids.len()))
            }
            _ => None,
        })
        .collect();
    let windows: Vec<Option<u32>> = snapshots.iter().map(|(w, _)| *w).collect();
    assert_eq!(
        windows,
        [Some(4), Some(4), Some(33), Some(33), Some(13)],
        "process 1's two, the switch's and boot 2's exit, boot 3's exit"
    );
    // Boot 3 holds days 5..=9 (process 1's window), 12..=14 and 25..=27; its window keeps day 14 and on.
    assert_eq!(snapshots.last().map(|(_, ids)| *ids), Some(4), "{snapshots:?}");

    replay_offline(&dir).expect("the fence holds across the window changes");
}

/// Rests ONE reduce-only SELL stop of 2 at 95 when its feed goes down (polymarket's row rests none, so the
/// core holds it for the mount).
struct ReduceStop;

impl Strategy<LiveBroker> for ReduceStop {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_stop_reduce(-1, 2.0, 95.0);
    }
}

/// A strategy that does nothing on its own: the test lowers its intents.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

/// A journaled core over `primary` minting the v2 session `Wk3x9q2m7d1p4` (handed in, so no switch), with
/// one mount on `venue`/`symbol` running `strategy` under the strategy code `00A3`, and `budgets`.
fn v2_mounted_core(
    dir: &Scratch,
    primary: Engine<RecordingClient>,
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
    budgets: std::collections::HashMap<String, MountBudget>,
) -> CoreThread<RecordingClient> {
    let mut mount =
        crate::testkit::mount_of(&primary.venue.clone(), &primary.symbol.clone(), "1m", strategy);
    mount.order_client_id_code =
        vike_model::orders::client_id::OrderClientIdStrategyCode::parse("00A3");
    let mut c = core_of(
        primary,
        Vec::new(),
        CoreConfig {
            seed_cash: 10_000.0,
            order_client_id_session: Some(("Wk3x9q2m7d1p4".into(), 0)),
            strategy: Some(mount),
            mount_budgets: budgets,
            journal: Some(journal(dir)),
            ..CoreConfig::default()
        },
    );
    // As `spawn_core_multi` derives it for an engine a mount trades.
    c.engine.collect_applied_fills = true;
    c
}

/// The live engine's order ids and state hash, then a crash (no exit `Snapshot`): the boot restore refolds the
/// journal's tail through a replay core to the same ids and the same `state_hash`. Answers the ids.
fn refolds_alike(dir: &Scratch, c: CoreThread<RecordingClient>) -> Vec<String> {
    let tmp = Scratch::created("vco-order-client-id-refold-tmp");
    let live = c.engine.snapshot_state();
    let ids: Vec<String> = live.registry.iter().map(|(id, _)| id.clone()).collect();
    let hash = vike_execution::state_hash(std::slice::from_ref(&live));
    drop(c);
    let restore = restore_for_boot(Some(&journal(dir)), &tmp, vike_model::now_ms(), &[]).unwrap();
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let replayed: Vec<String> =
        restore.restored_engines()[0].registry.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(replayed, ids, "the replay core minted the live core's ids");
    assert_eq!(
        vike_execution::state_hash(restore.restored_engines()),
        hash,
        "and folded the same book"
    );
    ids
}

/// **A strategy's bracket and its fired core-held stop refold to the same ids**: on a v2 session the
/// strategy's bracket (refused whole on polymarket, its three legs registered) and the close its core-held
/// stop fires on its arm's engine carry `S` and the mount's code live, and the refold mints them alike.
#[test]
fn a_v2_strategys_bracket_and_fired_core_held_stop_refold_to_the_same_ids_and_state_hash() {
    let dir = Scratch::reserved("vco-order-client-id-refold-fire");
    let mut c = v2_mounted_core(
        &dir,
        engine_on("polymarket", "X", RecordingClient::default()),
        Box::new(ReduceStop),
        std::collections::HashMap::new(),
    );
    let key = ("polymarket".into(), "X".into(), vike_model::events::PositionSide::Both);
    let long =
        vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() };
    c.engine.account.positions.insert(key, long);
    c.write_snap(1); // the restore's base: the position is in it
    let bracket = vike_model::OrderBracket {
        venue: "polymarket".into(),
        symbol: "X".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    c.apply_strategy_intent(OrderIntent::Bracket(Box::new(bracket)), 2, 0);
    c.drive_strategy_feed_status("polymarket", "X", FeedStatus::Disconnected);
    c.fire_conditionals_at_price("polymarket", "X", 94.0, 5);
    let close =
        c.engine.client.submissions.last().expect("the fire's close").order_client_id.clone();

    let ids = refolds_alike(&dir, c);
    assert_eq!(ids.len(), 4, "three legs and the close: {ids:?}");
    assert!(ids.iter().all(|id| id.starts_with("S00A300k3x9q2m7d1p")), "{ids:?}");
    assert!(close.ends_with("CL") && ids.contains(&close), "{close}");
}

/// **A strategy's submit and its budget latch's flatten refold to the same ids**: both lower on the mount's
/// route live (`S`, the mount's code) and the refold mints them alike, the fill between them folded too.
#[test]
fn a_v2_strategys_submit_and_latch_flatten_refold_to_the_same_ids_and_state_hash() {
    let dir = Scratch::reserved("vco-order-client-id-refold-latch");
    let mut budgets = std::collections::HashMap::new();
    budgets.insert(
        crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m"),
        MountBudget {
            max_loss: Some(50.0),
            max_notional: None,
            loss_action: vike_model::MountLossAction::Flatten,
            max_leverage: None,
        },
    );
    let mut c =
        v2_mounted_core(&dir, sim_engine_with(RecordingClient::default()), Box::new(Idle), budgets);
    c.write_snap(1);
    let buy = vike_model::OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    let open = c.apply_strategy_intent(OrderIntent::Submit(Box::new(buy)), 2, 0)[0].clone();
    assert_eq!(open, "S00A300k3x9q2m7d1p4OL");
    c.dispatch(Ingest::Event(Event::Fill(vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::prefixed("t-", &open),
        order_client_id: open.clone(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 3,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })));
    c.dispatch_applied_fills();
    c.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 4);
    c.sweep_mount_budgets(5);
    assert!(c.mount_latched[0], "premise: a loss of 60 passes the budget of 50");

    let ids = refolds_alike(&dir, c);
    assert_eq!(ids, [open, "S00A300k3x9q2m7d1p5CL".to_string()]);
}

/// **Only the replay core lowers an order as a mount's**: a LIVE core handed a `Command::OrderOn` whose route
/// key is one of its mount ids refuses it with the note it always gave (no engine for that key), and nothing
/// is minted or sent.
#[test]
fn a_live_core_refuses_an_order_on_a_mount_id_with_the_no_engine_note() {
    let mut mount = crate::testkit::mount_of("sim", "BTCUSDT", "1m", Box::new(Idle));
    mount.order_client_id_code =
        vike_model::orders::client_id::OrderClientIdStrategyCode::parse("00A3");
    let mut c = core_with(CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
    let mount_id = c.mount_ids[0].clone();
    let buy = vike_model::OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    c.dispatch(Ingest::Command(vike_execution::Command::OrderOn(Box::new(
        vike_execution::RoutedOrder {
            route_key: mount_id.clone(),
            intent: OrderIntent::Submit(Box::new(buy)),
        },
    ))));
    assert!(c.engine.client.submissions.is_empty(), "nothing was sent");
    assert_eq!(c.order_client_id_gen.state().1, 0, "nothing was minted");
    let note = format!("OrderOn REFUSED: no engine for route key {mount_id}");
    assert!(c.recent.iter().any(|l| &**l == note.as_str()), "{:?}", c.recent);
}
