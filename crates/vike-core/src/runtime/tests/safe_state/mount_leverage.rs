//! The mount LEVERAGE cap at the drain and at a held order's release (`leverage_refusal_on`, through
//! `vike_execution::risk::judge_mount_leverage`, rule v3): each verb judged ALONE on the MOUNT's own book,
//! netted on the order's row, an order list judged whole (its held child with its parent), a combo as one
//! unit, a reduce-only order judged unless it covers the mount's own row, an amend on its growth over what
//! is left to fill (a named price on all it rests, a held child with its parent), a held release when it
//! releases, a held row nothing prices valued at its entry, an order valued at the worse of its price and
//! the mark, an order of unknown value refused, and the cap judged LAST, after the drain's other refusals.
//!
//! Every rig is "sim"/BTCUSDT at 100 with a sizing equity of 10 000: mount `first` is capped at 2 (it may hold
//! 20 000), its sibling `second` states no cap.

use std::sync::{Arc, Mutex};

use super::mount_budget::{budget_mount, order_client_id_fill};
use super::*;
use vike_execution::risk::MOUNT_LEVERAGE_REFUSAL;
use vike_model::{OrderBroker, OrderListLeg, OrderPlacement, OrderRelease, TimeInForce};

const SYM: &str = "BTCUSDT";

type Action = Box<dyn FnOnce(&mut LiveBroker) + Send>;

/// What a test hands a mount and reads back from it.
#[derive(Default, Clone)]
struct Handles {
    actions: Arc<Mutex<Vec<Action>>>,
    denied: Arc<Mutex<Vec<String>>>,
}

impl Handles {
    fn queue(&self, action: impl FnOnce(&mut LiveBroker) + Send + 'static) {
        self.actions.lock().unwrap().push(Box::new(action));
    }

    fn denied(&self) -> Vec<String> {
        self.denied.lock().unwrap().clone()
    }

    /// Whether a refusal heard starts with `word`.
    fn refused_by(&self, word: &str) -> bool {
        self.denied().iter().any(|d| d.starts_with(word))
    }

    /// How many refusals heard are the leverage cap's.
    fn cap_refusals(&self) -> usize {
        self.denied().iter().filter(|d| d.starts_with(MOUNT_LEVERAGE_REFUSAL)).count()
    }
}

/// Runs every queued action on a quote tick, and keeps the reason of every order the core refused it.
struct Script(Handles);

impl Strategy<LiveBroker> for Script {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &TickQuote) {
        let actions: Vec<Action> = self.0.actions.lock().unwrap().drain(..).collect();
        for action in actions {
            action(broker);
        }
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &event.kind {
            self.0.denied.lock().unwrap().push(reason.clone());
        }
    }
}

fn capped(cap: f64) -> MountBudget {
    MountBudget {
        max_loss: None,
        max_notional: None,
        loss_action: vike_model::MountLossAction::CancelOnly,
        max_leverage: Some(cap),
    }
}

/// The rig: `first` (capped at 2, declaring `legs`) and `second` on "sim"/BTCUSDT, marked at 100.
fn rig_with_legs(legs: Vec<MountLeg>) -> (CoreThread<RecordingClient>, [Handles; 2]) {
    let handles = [Handles::default(), Handles::default()];
    let mount = |id: &str, h: &Handles| Mount {
        controller_id: Some(id.to_string()),
        strategy: Box::new(Script(h.clone())),
        ..budget_mount(SYM)
    };
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(Mount { symbols: legs, ..mount("first", &handles[0]) }),
        extra_mounts: vec![mount("second", &handles[1])],
        mount_budgets: [("first".to_string(), capped(2.0))].into_iter().collect(),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.engine.collect_applied_fills = true;
    core.engine.price_board.set_mark("sim", SYM, 100.0, 0);
    (core, handles)
}

fn rig() -> (CoreThread<RecordingClient>, [Handles; 2]) {
    rig_with_legs(Vec::new())
}

/// A fill of `side` · `qty` at 100 of an order mount `m` owns (`id`): into the account and the mount's ledger.
fn book(core: &mut CoreThread<RecordingClient>, m: usize, id: &str, side: i32, qty: f64) {
    core.order_client_id_mount.insert(id.to_string(), m);
    let fill = order_client_id_fill(id, SYM, side, qty, 100.0);
    core.bus.publish(Event::Fill(fill), &mut core.engine);
    core.dispatch_applied_fills();
}

/// One quote tick: every mount runs what it was queued.
fn tick(core: &mut CoreThread<RecordingClient>, now: i64) {
    core.drive_strategy_tick("sim", SYM, 100.0, 100.0, now, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    core.dispatch_applied_fills();
}

/// Whether an order of `side` · `qty` reached the venue.
fn sent(core: &CoreThread<RecordingClient>, side: i32, qty: f64) -> bool {
    core.engine.client.submissions.iter().any(|r| r.side == side && r.qty == qty)
}

fn market(side: i32, qty: f64) -> impl FnOnce(&mut LiveBroker) + Send + 'static {
    move |b: &mut LiveBroker| b.place("", "", side, qty, OrderPlacement::Market, false)
}

/// A REVERSAL is netted on the mount's row: long 100, a sell of 200 holds 100 short after it (10 000), so a
/// cap-2 mount reverses; one of 400 (300 short, 30 000) is refused. Before the netting the sell of 200 was
/// judged 10 000 + 20 000 and refused, so an always-in-market strategy could never reverse.
#[test]
fn a_reversal_is_netted_on_the_mounts_own_row() {
    let (mut core, [first, _]) = rig();
    book(&mut core, 0, "f1", 1, 100.0);
    first.queue(market(-1, 200.0));
    first.queue(market(-1, 400.0));
    tick(&mut core, 1);
    assert!(sent(&core, -1, 200.0), "the reversal went out: {:?}", first.denied());
    assert!(!sent(&core, -1, 400.0), "300 short is past the cap");
    let denied = first.denied();
    assert!(denied.len() == 1 && denied[0].starts_with(MOUNT_LEVERAGE_REFUSAL), "{denied:?}");
}

/// The MOUNT's own book decides what opens, never the account's: `first` long 150 closes it while its sibling's
/// short 300 leaves the account short 150 (before, the close read as an add to that short and was refused).
#[test]
fn a_mount_closes_its_own_position_whatever_its_sibling_holds() {
    let (mut core, [first, _]) = rig();
    book(&mut core, 0, "f1", 1, 150.0);
    book(&mut core, 1, "s1", -1, 300.0);
    first.queue(market(-1, 150.0));
    tick(&mut core, 1);
    assert!(sent(&core, -1, 150.0), "its own close went out: {:?}", first.denied());
    assert!(first.denied().is_empty(), "{:?}", first.denied());
}

/// ...and a flat mount cannot "reduce" a sibling's long past its own cap: its sell of 250 opens 25 000 on its
/// book (before, the account's long 300 covered it and it was never judged).
#[test]
fn a_mount_cannot_reduce_its_siblings_position_past_its_cap() {
    let (mut core, [first, second]) = rig();
    book(&mut core, 1, "s1", 1, 300.0);
    first.queue(market(-1, 250.0));
    tick(&mut core, 1);
    assert!(!sent(&core, -1, 250.0), "the sell stayed home");
    assert!(first.refused_by(MOUNT_LEVERAGE_REFUSAL), "{:?}", first.denied());
    // the uncapped sibling sells its own
    second.queue(market(-1, 250.0));
    tick(&mut core, 2);
    assert!(sent(&core, -1, 250.0), "{:?}", second.denied());
}

/// A leg of `side` · `qty`, a limit at `price` or a stop at `trigger`, held for leg `parent`'s fill when given.
fn leg(side: i32, qty: f64, order: OrderPlacement, parent: Option<usize>) -> OrderListLeg {
    let (order_type, price, trigger_price, trail) = order.request_terms();
    OrderListLeg {
        side,
        qty,
        order_type,
        price,
        trigger_price,
        trail,
        reduce_only: false,
        time_in_force: TimeInForce::Gtc,
        parent,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

/// An ORDER LIST's held OTO child is judged at the drain WITH its parent, as the simulator's `judge_group`
/// judges it, since the core releases it later with no judge: a market buy of 1 arming a breakout buy of 1 000
/// at 101 is 1 001 on a flat book, refused whole, each leg hearing it under its tag (before, the child was
/// skipped and the mount reached 10x). A ladder whose entry fits goes out (its exits reach no further).
#[test]
fn an_order_lists_child_is_judged_with_its_parent_at_the_drain() {
    let (mut core, [first, _]) = rig();
    let entry = leg(1, 1.0, OrderPlacement::Market, None);
    let child = leg(1, 1_000.0, OrderPlacement::Stop { trigger: 101.0 }, Some(0));
    first.queue(move |b| b.place_group("", vec![("e".into(), entry), ("c".into(), child)]));
    tick(&mut core, 1);
    assert_eq!(first.cap_refusals(), 2, "each leg hears it: {:?}", first.denied());
    assert!(core.engine.client.submissions.is_empty(), "nothing went out");
    let ladder = vec![
        ("in".into(), leg(1, 150.0, OrderPlacement::Limit { price: 100.0 }, None)),
        ("sl".into(), leg(-1, 150.0, OrderPlacement::Stop { trigger: 90.0 }, Some(0))),
        ("tp".into(), leg(-1, 150.0, OrderPlacement::Limit { price: 110.0 }, Some(0))),
    ];
    first.queue(move |b| b.place_group("", ladder));
    tick(&mut core, 2);
    assert_eq!(first.cap_refusals(), 2, "the ladder passes: {:?}", first.denied());
}

/// EACH ORDER ALONE (review #3/#6): two opening orders of one callback, in two verbs, each fit and both go out
/// whichever comes first, though their sum (25 000) would not; the mount cap does not sum a callback's
/// orders (the engine's `RiskGate` bounds the account).
#[test]
fn a_callbacks_orders_are_each_judged_alone() {
    for market_first in [true, false] {
        let (mut core, [first, _]) = rig();
        let limit = |b: &mut LiveBroker| {
            b.place("", "l", 1, 100.0, OrderPlacement::Limit { price: 99.0 }, false);
        };
        if market_first {
            first.queue(market(1, 150.0));
            first.queue(limit);
        } else {
            first.queue(limit);
            first.queue(market(1, 150.0));
        }
        tick(&mut core, 1);
        assert!(sent(&core, 1, 150.0) && sent(&core, 1, 100.0), "{:?}", first.denied());
        assert_eq!(first.cap_refusals(), 0, "{market_first}");
    }
}

/// An AMEND is judged on its growth over what is still to FILL (review #1): a buy of 100 at 100, 60 of it
/// filled, amended to 190 rests 190 more where 40 rested, so it adds 150 to the mount's 60 (21 000) and is
/// refused with its terms kept (judging the growth over the stated 100 added 90 and let it through); to 180
/// it reaches 20 000, at the cap, and goes. A decrease passes.
#[test]
fn an_amend_is_judged_on_its_growth_over_what_is_left_to_fill() {
    let (mut core, [first, _]) = rig();
    first.queue(|b| b.place("", "q", 1, 100.0, OrderPlacement::Limit { price: 100.0 }, false));
    tick(&mut core, 1);
    let id =
        core.engine.client.submissions.last().expect("the buy went out").order_client_id.clone();
    // the venue accepts it and 60 of it fills, as a bridge emits a partial fill (`[Fill, wrap]`): the ledger
    // and its `filled_qty`
    let fill = order_client_id_fill(&id, SYM, 1, 60.0, 100.0);
    let events = [
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: id.clone(),
            timestamp: 1,
        }),
        Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: id.clone(),
            venue_order_id: Some(format!("v-{id}").into()),
            timestamp: 1,
        }),
        Event::Fill(fill.clone()),
        Event::OrderPartiallyFilled(vike_model::events::OrderPartiallyFilled {
            order_client_id: id.clone(),
            fill,
            timestamp: 1,
        }),
    ];
    for event in events {
        core.dispatch(Ingest::Event(event));
    }
    core.dispatch_applied_fills();
    assert_eq!(core.engine.registry.get(&id).map(|o| o.filled_qty), Some(60.0));
    assert_eq!(core.mount_attribution[0].size, 60.0);
    first.queue(|b| OrderBroker::amend(b, "q", Some(190.0), None, None));
    tick(&mut core, 2);
    assert_eq!(amends_refused(&core), 1, "{:?}", core.recent);
    let grew = |c: &CoreThread<RecordingClient>, q| {
        c.engine.client.modifies.iter().any(|m| m.1 == Some(q)) || sent(c, 1, q)
    };
    assert!(!grew(&core, 190.0), "nothing went out");
    first.queue(|b| OrderBroker::amend(b, "q", Some(180.0), None, None));
    tick(&mut core, 3);
    first.queue(|b| OrderBroker::amend(b, "q", Some(20.0), None, None));
    tick(&mut core, 4);
    assert_eq!(amends_refused(&core), 1, "at the cap it goes, a decrease too: {:?}", core.recent);
}

/// How many amends the cap refused: the ring line each leaves, the order's terms kept.
fn amends_refused(core: &CoreThread<RecordingClient>) -> usize {
    let word = "REFUSED, its terms kept: over-mount-leverage";
    core.recent.iter().filter(|l| l.contains(word)).count()
}

/// A TIMED order is judged at its RELEASE, against the mount's book then, as the simulator's `release_held`
/// judges it: a buy of 150 held for 5 000 goes out on a flat book, and is refused (ended by a journaled cancel,
/// the ring naming the cap) once the mount bought 100 meanwhile, 25 000. Before, it was judged at accept
/// (15 000) and released whatever the mount held by then.
#[test]
fn an_order_held_for_its_release_is_judged_at_the_release() {
    for bought in [false, true] {
        let (mut core, [first, _]) = rig();
        let release = OrderRelease { at: 5_000, price: None };
        first.queue(move |b| b.submit_market_at("", 1, 150.0, release));
        tick(&mut core, 1);
        assert_eq!(core.held_releases.len(), 1, "{bought}: held, never judged at accept");
        if bought {
            book(&mut core, 0, "f1", 1, 100.0);
        }
        core.drive_due_timers(5_000);
        core.dispatch_applied_fills();
        assert!(core.held_releases.is_empty(), "{bought}: the hold ends either way");
        assert_eq!(sent(&core, 1, 150.0), !bought, "{bought}: {:?}", core.recent);
        let refused = core.recent.iter().any(|l| l.contains("NOT RELEASED: over-mount-leverage"));
        assert_eq!(refused, bought, "{bought}: {:?}", core.recent);
    }
}

/// A REDUCE-ONLY order passes unjudged only as a covered reduce of the mount's OWN row, judged on its STATED
/// size (review #8): a flat `first`'s reduce-only sell of 250 against its sibling's long 150 would fill up to
/// the ACCOUNT's position (the venue caps it there) and book short on `first`; judged as stated (25 000), it is
/// refused, as the simulator now judges it before its reduce-only clamp (the clamped 150 was 15 000 there).
#[test]
fn a_flat_mounts_reduce_only_sell_against_its_siblings_long_is_judged() {
    let (mut core, [first, _]) = rig();
    book(&mut core, 1, "s1", 1, 150.0);
    first.queue(|b| b.place("", "", -1, 250.0, OrderPlacement::Market, true));
    tick(&mut core, 1);
    assert!(!sent(&core, -1, 250.0), "it stayed home");
    assert!(first.refused_by(MOUNT_LEVERAGE_REFUSAL), "{:?}", first.denied());
}

/// ...so a protective exit sent SEPARATELY is judged alone, on the flat row it is sent over: a reduce-only take
/// profit of 150 at 140 sent beside its entry of 150 opens 21 000 short on that row and is refused, while the
/// same exits sent as a BRACKET are covered by its entry and go out (near the cap, send a bracket).
#[test]
fn a_separate_exit_is_judged_alone_and_a_brackets_is_covered_by_its_entry() {
    let (mut core, [first, _]) = rig();
    first.queue(market(1, 150.0));
    first.queue(|b| b.place("", "tp", -1, 150.0, OrderPlacement::Limit { price: 140.0 }, true));
    tick(&mut core, 1);
    assert!(sent(&core, 1, 150.0) && !sent(&core, -1, 150.0), "{:?}", first.denied());
    assert_eq!(first.cap_refusals(), 1, "{:?}", first.denied());
    let (mut core, [first, _]) = rig();
    first.queue(|b| b.submit_bracket(1, 150.0, None, 90.0, 140.0));
    tick(&mut core, 1);
    assert_eq!(first.cap_refusals(), 0, "{:?}", first.denied());
}

/// An order whose value cannot be known is REFUSED, fail closed (review #5), and a declared leg never traded is
/// judged on a row at its own resolver mark (review #7): ETHUSDT, declared, never quoted, never traded, takes
/// no market buy (nothing values it) and takes a limit buy, valued at its own price.
#[test]
fn a_market_order_on_a_never_quoted_leg_is_refused() {
    let (mut core, [first, _]) = rig_with_legs(vec![MountLeg::same_venue("ETHUSDT")]);
    first.queue(|b| b.place("ETHUSDT", "", 1, 1.0, OrderPlacement::Market, false));
    first.queue(|b| b.place("ETHUSDT", "", 1, 2.0, OrderPlacement::Limit { price: 50.0 }, false));
    tick(&mut core, 1);
    assert!(!sent(&core, 1, 1.0), "the market buy stayed home");
    assert_eq!(first.cap_refusals(), 1, "the limit passes the cap: {:?}", first.denied());
    assert!(first.denied().iter().any(|d| d.contains("cannot be known")), "{:?}", first.denied());
}

/// The cap is judged LAST, after the drain's other refusals, as the simulator judges it (review #9): a buy
/// of 1 000 (100 000, far past the cap) at a limit no finite number names hears the terms rule, never the cap.
#[test]
fn the_cap_is_judged_after_the_drains_other_refusals() {
    let (mut core, [first, _]) = rig();
    let infinite = OrderPlacement::Limit { price: f64::INFINITY };
    first.queue(move |b| b.place("", "", 1, 1_000.0, infinite, false));
    tick(&mut core, 1);
    let denied = first.denied();
    assert!(denied.len() == 1 && denied[0].contains("not a finite number"), "{denied:?}");
}

/// A held row its resolver cannot price (ETHUSDT, no mark: a leg a settlement left in the ledger) is valued at
/// its own average entry price, never refused for it (before, every opening order was refused for ever): 5 at
/// 50 counts 250 and a buy goes out; 150 at 100 counts 15 000 and a buy of 60 more (6 000) is refused.
#[test]
fn an_unpriced_held_row_is_valued_at_its_entry() {
    for (held, average, qty, goes) in [(5.0, 50.0, 1.0, true), (150.0, 100.0, 60.0, false)] {
        let (mut core, [first, _]) = rig_with_legs(vec![MountLeg::same_venue("ETHUSDT")]);
        let eth = core.mount_attribution[0].leg_mut("sim", None, "ETHUSDT");
        (eth.size, eth.average_price) = (held, average);
        first.queue(market(1, qty));
        tick(&mut core, 1);
        assert_eq!(sent(&core, 1, qty), goes, "{held}: {:?}", first.denied());
        assert_eq!(first.refused_by(MOUNT_LEVERAGE_REFUSAL), !goes, "{held}");
    }
}

/// An OPENING order is valued at the WORSE of its own price and the mark (review round 4 #1): a sell limit of
/// 300 at 1 fills near the mark (30 000, refused; it counted 300 before); a resting buy of 150 at 50 (15 000
/// at the mark) re-priced to 140, through the market, is judged on all it rests at the new price (21 000,
/// refused; a price-only amend passed before).
#[test]
fn an_order_priced_through_the_market_counts_at_the_market() {
    let (mut core, [first, _]) = rig();
    first.queue(|b| b.place("", "", -1, 300.0, OrderPlacement::Limit { price: 1.0 }, false));
    first.queue(|b| b.place("", "q", 1, 150.0, OrderPlacement::Limit { price: 50.0 }, false));
    tick(&mut core, 1);
    assert!(!sent(&core, -1, 300.0) && sent(&core, 1, 150.0), "{:?}", first.denied());
    assert_eq!(first.cap_refusals(), 1, "{:?}", first.denied());
    first.queue(|b| OrderBroker::amend(b, "q", None, Some(140.0), None));
    tick(&mut core, 2);
    assert_eq!(amends_refused(&core), 1, "{:?}", core.recent);
}

/// An amend that grows a HELD OTO child is judged with the parent that arms it, as its list was (review round
/// 4 #2): a buy of 1 at 50 arming a buy of 100 at 101 passes (101 at 101, 10 201); growing the held child to 300
/// adds 200 to the parent's 1 (20 301), refused with the child's terms kept. Before, the drain found no live
/// order and the held child's stored request took the new size unjudged.
#[test]
fn an_amend_of_a_held_child_is_judged_with_its_parent() {
    let (mut core, [first, _]) = rig();
    let parent = leg(1, 1.0, OrderPlacement::Limit { price: 50.0 }, None);
    let child = leg(1, 100.0, OrderPlacement::Limit { price: 101.0 }, Some(0));
    first.queue(move |b| b.place_group("", vec![("p".into(), parent), ("c".into(), child)]));
    tick(&mut core, 1);
    assert_eq!(first.cap_refusals(), 0, "{:?}", first.denied());
    assert_eq!(core.held_orders.len(), 1, "the child waits for its parent");
    first.queue(|b| OrderBroker::amend(b, "c", Some(300.0), None, None));
    tick(&mut core, 2);
    assert_eq!(amends_refused(&core), 1, "{:?}", core.recent);
    assert!(core.held_orders.values().all(|r| r.qty == 100.0), "its terms kept");
}

/// A size-only grow of a HELD child on the OPPOSITE side of its parent is judged on its WHOLE new size after
/// the parent, exactly as its list was (review round 5 #1): a buy of 100 at 99 arming a sell of 100 at 110,
/// grown to 350, reaches 250 short at 110 (27 500), refused, where its growth alone (16 500) passed; a
/// size-only decrease passes.
#[test]
fn a_grow_of_a_held_child_on_the_far_side_is_judged_on_its_whole_size() {
    let (mut core, [first, _]) = rig();
    let parent = leg(1, 100.0, OrderPlacement::Limit { price: 99.0 }, None);
    let child = leg(-1, 100.0, OrderPlacement::Limit { price: 110.0 }, Some(0));
    first.queue(move |b| b.place_group("", vec![("p".into(), parent), ("c".into(), child)]));
    tick(&mut core, 1);
    assert_eq!((first.cap_refusals(), core.held_orders.len()), (0, 1), "{:?}", first.denied());
    first.queue(|b| OrderBroker::amend(b, "c", Some(350.0), None, None));
    tick(&mut core, 2);
    first.queue(|b| OrderBroker::amend(b, "c", Some(50.0), None, None));
    tick(&mut core, 3);
    assert_eq!(amends_refused(&core), 1, "the grow alone: {:?}", core.recent);
}

/// A PRICE-ONLY amend of a held exit its own list covers passes (review round 5 #2): a market buy of 190 with
/// its reduce-only stop and target held under it, the mount long 20 since, re-prices its stop; the entry puts
/// 210 on and the stop takes 190 off, so it adds nothing (judged as if the entry filled it read 21 000).
#[test]
fn a_repriced_held_exit_its_list_covers_passes() {
    let (mut core, [first, _]) = rig();
    let exit = |order: OrderPlacement, other: usize| OrderListLeg {
        reduce_only: true,
        contingency: Some(vike_model::ContingencyType::Oco),
        linked: vec![other],
        ..leg(-1, 190.0, order, Some(0))
    };
    let legs = vec![
        ("in".into(), leg(1, 190.0, OrderPlacement::Market, None)),
        ("sl".into(), exit(OrderPlacement::Stop { trigger: 90.0 }, 2)),
        ("tp".into(), exit(OrderPlacement::Limit { price: 110.0 }, 1)),
    ];
    first.queue(move |b| b.place_group("", legs));
    tick(&mut core, 1);
    assert_eq!((first.cap_refusals(), core.held_orders.len()), (0, 2), "{:?}", first.denied());
    book(&mut core, 0, "f1", 1, 20.0);
    first.queue(|b| OrderBroker::amend(b, "sl", None, Some(85.0), None));
    tick(&mut core, 2);
    assert_eq!(amends_refused(&core), 0, "{:?}", core.recent);
}

/// A COMBO is judged as ONE unit, all its legs or none (review round 4 #3): a roll of `first`'s long 150 of
/// BTCUSDT into 150 of ETHUSDT holds 15 000 either way and goes (judged leg by leg, the ETH leg counted 30 000
/// beside the long and was refused), while a combo buying 110 of each on a flat book reaches 22 000, refused
/// (each leg alone was 11 000).
#[test]
fn a_combo_is_judged_as_one_unit() {
    for (held, ratios, qty, refused) in [(150.0, [-1, 1], 150.0, 0), (0.0, [1, 1], 110.0, 1)] {
        let (mut core, [first, _]) = rig_with_legs(vec![MountLeg::same_venue("ETHUSDT")]);
        core.engine.price_board.set_mark("sim", "ETHUSDT", 100.0, 0);
        if held > 0.0 {
            book(&mut core, 0, "f1", 1, held);
        }
        first.queue(move |b| {
            let leg = |symbol: &str, ratio| vike_model::OrderComboLeg {
                symbol: symbol.into(),
                ratio,
                limit: None,
            };
            b.submit_combo(1, qty, vec![leg(SYM, ratios[0]), leg("ETHUSDT", ratios[1])], None);
        });
        tick(&mut core, 1);
        assert_eq!(first.cap_refusals(), refused, "{held}: {:?}", first.denied());
    }
}

/// A bracket's exits net its row in QUANTITY (review round 4 #4): flat, a market buy of 190 with a stop at 90
/// and a take-profit at 300 is its entry (19 000) and goes; netting in notional counted the far target as
/// 57 000 of profit against the entry and refused it.
#[test]
fn a_brackets_far_take_profit_is_no_exposure() {
    let (mut core, [first, _]) = rig();
    first.queue(|b| b.submit_bracket(1, 190.0, None, 90.0, 300.0));
    tick(&mut core, 1);
    assert_eq!(first.cap_refusals(), 0, "{:?}", first.denied());
}

/// A DISCRETIONARY limit is judged at the worst price it may fill at, as the simulator judges it (review round
/// 4 #5): a buy of 190 at 100 with a discretion of 10 may fill at 110 (20 900), refused; judged at its limit it
/// was 19 000.
#[test]
fn a_discretionary_limit_is_judged_at_its_worst_price() {
    let (mut core, [first, _]) = rig();
    let attributes =
        vike_model::OrderAttributes { discretionary_amount: Some(10.0), ..Default::default() };
    first.queue(move |b| b.submit_limit_attributed(1, 190.0, 100.0, attributes));
    tick(&mut core, 1);
    assert_eq!(first.cap_refusals(), 1, "{:?}", first.denied());
}
