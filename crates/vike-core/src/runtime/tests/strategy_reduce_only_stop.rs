//! A MOUNTED STRATEGY'S REDUCE-ONLY STOP (`LiveBroker::submit_stop_reduce`): the strategy's twin of the
//! operator's `--type stop --reduce-only`, through the same capability preflight and the same core-held
//! arm (`apply/conditional.rs`'s `arm_core_held_stop`), differing only in the route (`EngineRoute::Mount`).
//! White-box, driven the way the runtime drives a strategy (`drive_strategy_feed_status` -> the hook ->
//! `drain_broker` -> `apply_strategy_intent`), so the verb, the drain's carry of the trigger, the preflight
//! and the arm are all in the path.
//!
//! What each test pins:
//!
//! * a venue whose row REFUSES a reduce-only stop (polymarket: no `stop` kind; ig: `stop` an entry kind only)
//!   has the CORE hold it, as the mount's own (`ArmedBy::mount`); the fire is a reduce-only market capped to
//!   the position, and its coid is the mount's;
//! * a venue whose row takes it (binance) rests a NATIVE reduce-only stop, owned by the mount, and arms
//!   nothing in the core;
//! * a stop that fires after the position was closed another way drops and sends NOTHING (the defect the verb
//!   exists for), and a re-armed stop stays the mount's;
//! * a PAPER engine's stop reaches the simulator, as an operator's does (decision 0201), and says so once;
//! * a stop the arm cannot take (no usable trigger) is refused in the capability words, on the mount's order.

use super::*;
use vike_exec::PositionEntry;
use vike_exec::testing::RecordingClient;
use vike_model::events::PositionSide;
use vike_model::{AccountTier, OrderStatus, OrderType};

use crate::runtime::test_support::{core_of, engine_on};

type Core = CoreThread<RecordingClient>;

const SYM: &str = "X";

/// Rests ONE reduce-only SELL stop of `qty` at `trigger` on its first feed-status call, and nothing else.
struct ReduceStop {
    qty: f64,
    trigger: f64,
}

impl Strategy<LiveBroker> for ReduceStop {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_stop_reduce(-1, self.qty, self.trigger);
    }
}

/// A core whose one engine is `venue`'s, holding `position` of `X` (a ROSTER venue, so its capability row is
/// enforced), with one mount on it running [`ReduceStop`].
fn core(venue: &str, position: f64, tier: Option<AccountTier>, trigger: f64) -> Core {
    let mut engine = engine_on(venue, SYM, RecordingClient::default());
    if let Some(tier) = tier {
        engine.set_tier(tier);
    }
    let mount = StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: Some("rs".to_string()),
        underlying_symbol: None,
        venue: venue.into(),
        symbol: SYM.into(),
        interval: "1m".into(),
        strategy: Box::new(ReduceStop { qty: 2.0, trigger }),
    };
    let mut c =
        core_of(engine, Vec::new(), CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
    set_position(&mut c, venue, position);
    c
}

fn set_position(c: &mut Core, venue: &str, size: f64) {
    let key = (venue.into(), SYM.into(), PositionSide::Both);
    if size == 0.0 {
        c.engine.account.positions.shift_remove(&key);
    } else {
        c.engine
            .account
            .positions
            .insert(key, PositionEntry { size, avg_px: 100.0, ..Default::default() });
    }
}

/// The mount's strategy runs: the hook rests its stop through the real drain.
fn run_strategy(c: &mut Core, venue: &str) {
    c.drive_strategy_feed_status(venue, SYM, FeedStatus::Disconnected);
}

fn ring(c: &Core, needle: &str) -> usize {
    c.recent.iter().filter(|l| l.contains(needle)).count()
}

fn armed(c: &Core) -> usize {
    c.conditional_books.values().map(|b| b.len()).sum()
}

/// **THE ARM, for a venue whose row refuses a reduce-only stop.** The strategy's stop is HELD by the core,
/// with nothing minted, registered or sent, as the MOUNT's own (so its fire is that strategy's order), on the
/// tick lane though the profile's knob is off, and the ring names the mount. Run for both rows the capability
/// map sends down this path.
#[test]
fn a_strategys_reduce_only_stop_the_row_refuses_is_held_by_the_core_as_the_mounts_own() {
    for venue in ["polymarket", "ig"] {
        let mut c = core(venue, 2.0, None, 95.0);
        let seq_before = c.coid_gen.state().1;
        assert!(!c.tick_conditionals, "{venue}: precondition: the knob is off and nothing rests");

        run_strategy(&mut c, venue);

        assert!(c.engine.client.submissions.is_empty(), "{venue}: nothing reaches the venue");
        assert!(
            c.engine.registry.is_empty(),
            "{venue}: nothing was registered, so nothing was rejected"
        );
        assert_eq!(c.coid_gen.state().1, seq_before, "{venue}: validate-then-mint: no coid burned");
        assert_eq!(armed(&c), 1, "{venue}");
        let a = c.cond_engine.values().next().expect("the arm's side record");
        assert!(
            a.reduce_only && a.covered && a.engine == 0 && a.mount == Some(0),
            "{venue}: held as the mount's own, over a position it reduces: {a:?}"
        );
        assert!(c.tick_conditionals, "{venue}: a stop checked per closed bar is no protection");
        let mount_id = c.mount_ids[0].clone();
        assert_eq!(
            ring(&c, &format!("Armed by the strategy mount `{mount_id}`")),
            1,
            "{venue}: the note names the mount: {:?}",
            c.recent
        );

        // The crossing sends a market for the 2 held, reduce-only, and it is the MOUNT's order.
        c.fire_conditionals_at_price(venue, SYM, 94.0, 5);
        let sent = &c.engine.client.submissions;
        assert_eq!(sent.len(), 1, "{venue}: fired once");
        assert_eq!(
            (sent[0].order_type.clone(), sent[0].side, sent[0].qty, sent[0].reduce_only),
            (OrderType::Market, -1, 2.0, true),
            "{venue}: a reduce-only market for the position"
        );
        assert_eq!(
            c.coid_mount.get(&sent[0].client_order_id),
            Some(&0),
            "{venue}: the strategy's own order: its fills and events reach it"
        );
    }
}

/// **A venue whose row TAKES a reduce-only stop gets a NATIVE one**: the order reaches the client as a stop
/// with its trigger and the flag, owned by the mount, and the core arms nothing. (`vike-venues`' row is the
/// authority; binance lists `stop` among its `reduce_only_kinds`.)
#[test]
fn on_a_venue_whose_row_takes_it_the_strategys_reduce_only_stop_is_a_native_stop() {
    let mut c = core("binance", 2.0, None, 95.0);

    run_strategy(&mut c, "binance");

    assert_eq!(armed(&c), 0, "the core holds nothing: the venue does");
    assert!(c.cond_engine.is_empty());
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "one native order: {sent:?}");
    assert_eq!(
        (
            sent[0].order_type.clone(),
            sent[0].side,
            sent[0].qty,
            sent[0].trigger_price,
            sent[0].reduce_only
        ),
        (OrderType::Stop, -1, 2.0, Some(95.0), true),
        "a reduce-only stop with its trigger"
    );
    assert_eq!(c.coid_mount.get(&sent[0].client_order_id), Some(&0), "the mount's own order");
}

/// **THE DEFECT THE VERB EXISTS FOR.** The position is closed another way while the stop rests; the stop that
/// fires afterwards sends NOTHING (a plain stop would OPEN a position on the far side), says so by name, and is
/// gone.
#[test]
fn a_stop_that_fires_after_the_position_was_closed_another_way_sends_nothing() {
    let mut c = core("polymarket", 2.0, None, 95.0);
    run_strategy(&mut c, "polymarket");
    assert_eq!(armed(&c), 1);

    set_position(&mut c, "polymarket", 0.0); // closed another way: a take-profit, the strategy's own exit, a flatten
    c.fire_conditionals_at_price("polymarket", SYM, 94.0, 5);

    assert!(
        c.engine.client.submissions.is_empty(),
        "nothing was sent: it would have opened a position"
    );
    assert_eq!(ring(&c, "REDUCE-ONLY STOP DROPPED"), 1, "said by name: {:?}", c.recent);
    assert_eq!(armed(&c), 0, "and the arm is spent");
}

/// A stop whose close was refused is armed again, and the re-armed stop is STILL the mount's: its next fire
/// is that strategy's order, not an unowned one.
#[test]
fn a_re_armed_stop_stays_the_mounts_own() {
    let mut c = core("polymarket", 2.0, None, 95.0);
    run_strategy(&mut c, "polymarket");
    c.fire_conditionals_at_price("polymarket", SYM, 94.0, 1);
    let close = c.engine.client.submissions[0].client_order_id.clone();
    let mo = c.eng_mut(0).registry.get_mut(&close).expect("registered");
    mo.status = OrderStatus::Rejected; // the venue refused the close; the position is still open

    c.sweep_core_held_stops(30_000);

    assert_eq!(armed(&c), 1, "armed again: {:?}", c.recent);
    let a = c.cond_engine.values().next().expect("the new arm");
    assert_eq!((a.mount, a.reduce_only, a.rearms), (Some(0), true, 1), "{a:?}");
    c.fire_conditionals_at_price("polymarket", SYM, 94.0, 31);
    let again = c
        .engine
        .client
        .submissions
        .last()
        .expect("the re-armed stop fired")
        .client_order_id
        .clone();
    assert_ne!(again, close);
    assert_eq!(c.coid_mount.get(&again), Some(&0), "the second close is the mount's too");
}

/// A PAPER engine is not judged by the row's reduce-only axis (decision 0201): the simulator gets the stop,
/// the core holds nothing, and the ring says once that the live adapter would refuse it.
#[test]
fn a_paper_engines_strategy_stop_reaches_the_simulator_and_says_so() {
    let mut c = core("ig", 2.0, Some(AccountTier::Paper), 95.0);

    run_strategy(&mut c, "ig");

    assert_eq!(armed(&c), 0, "the simulator holds it, not the core");
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!((sent[0].order_type.clone(), sent[0].reduce_only), (OrderType::Stop, true));
    assert_eq!(ring(&c, "the live ig adapter would refuse this (stop)"), 1, "{:?}", c.recent);

    // A demo engine of the same venue is judged by the whole row: the core holds it.
    let mut demo = core("ig", 2.0, Some(AccountTier::Demo), 95.0);
    run_strategy(&mut demo, "ig");
    assert_eq!((armed(&demo), demo.engine.client.submissions.len()), (1, 0));
}

/// A stop the arm cannot take (a trigger that is no price) is not armed and is not silent: it falls through to
/// the unchanged mint and capability refusal, so the order exists, is the mount's, ends `Rejected`, and the
/// refusal says what the row says.
#[test]
fn a_stop_with_no_usable_trigger_is_refused_in_words_on_the_mounts_order() {
    let mut c = core("polymarket", 2.0, None, 0.0);

    run_strategy(&mut c, "polymarket");

    assert_eq!(armed(&c), 0, "nothing was armed");
    assert!(c.engine.client.submissions.is_empty(), "nothing was sent");
    let (coid, mo) = c.engine.registry.iter().next().expect("the refused order is registered");
    assert_eq!(mo.status, OrderStatus::Rejected, "terminal: {mo:?}");
    assert_eq!(
        c.coid_mount.get(coid.as_str()),
        Some(&0),
        "and it is the mount's, so the strategy hears it"
    );
    assert_eq!(ring(&c, "TRIGGER_UNSUPPORTED: kind=stop venue=polymarket"), 1, "{:?}", c.recent);
}
