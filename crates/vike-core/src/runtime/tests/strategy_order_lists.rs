//! A MOUNTED STRATEGY'S ORDER LISTS (`LiveBroker::submit_group`, `LiveBroker::submit_oco`,
//! `LiveBroker::submit_ladder`): each verb buffers ONE list on the mount's own symbol, and the drain lowers
//! it as ONE `OrderIntent::OrderList` with the venue and symbol stamped, or refuses it WHOLE as a `Denied`
//! in words before anything is minted or journaled. White-box, driven the way the runtime drives a strategy
//! (`drive_strategy_feed_status` -> the hook -> `drain_broker` -> `apply_strategy_intent`). What reached the
//! core is read off the journal's write-ahead `StrategySubmit`, which the drain writes before the lowering,
//! so these tests pin the drain and not what the core then does with a list. A mounted strategy's COMBO
//! (`LiveBroker::submit_combo`, wave 6) drains the same way, as ONE `OrderIntent::Combo`, and closes the file.

use std::sync::{Arc, Mutex};

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_journal::{Journal, JournalRecord};
use vike_model::{
    ContingencyType, LadderSpec, LadderTarget, OrderEvent, OrderEventKind, OrderListLeg, OrderType,
    TimeInForce,
};

use crate::runtime::test_support::core_of;

type Core = CoreThread<RecordingClient>;
type Heard = Arc<Mutex<Vec<OrderEvent>>>;
/// What the strategy buffers on its first feed-status call.
type Calls = fn(&mut LiveBroker);

const VENUE: &str = "deribit";
const SYM: &str = "X";

/// A reduce-only, `gtc`, unlinked leg selling 2.
fn leg_of(order_type: OrderType, price: Option<f64>, trigger_price: Option<f64>) -> OrderListLeg {
    OrderListLeg {
        side: -1,
        qty: 2.0,
        order_type,
        price,
        trigger_price,
        trail: None,
        reduce_only: true,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

/// A take-profit limit at 110 and a stop at 95, both unlinked.
fn pair() -> (OrderListLeg, OrderListLeg) {
    (leg_of(OrderType::Limit, Some(110.0), None), leg_of(OrderType::Stop, None, Some(95.0)))
}

fn targets() -> Vec<LadderTarget> {
    vec![LadderTarget { price: 105.0, qty: 1.0 }, LadderTarget { price: 110.0, qty: 1.0 }]
}

/// A long ladder of 2 on `venue`/`symbol`: entry at 100, stop at 95, `targets`.
fn ladder(venue: &str, symbol: &str, targets: Vec<LadderTarget>) -> LadderSpec {
    LadderSpec {
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        targets,
        account: None,
    }
}

struct Lists {
    calls: Calls,
    heard: Heard,
}

impl Strategy<LiveBroker> for Lists {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        (self.calls)(broker);
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.heard.lock().unwrap().push(event.clone());
    }
}

/// A journaling core on deribit with one mount on `X` whose strategy runs `calls` once; every lifecycle
/// event is delivered. Answers the core, what the strategy heard and every intent the drain journaled.
fn run(dir: &std::path::Path, calls: Calls) -> (Core, Vec<OrderEvent>, Vec<OrderIntent>) {
    run_declaring(dir, Vec::new(), calls)
}

/// [`run`] on a mount that declares `symbols` beside its own.
fn run_declaring(
    dir: &std::path::Path,
    symbols: Vec<MountLeg>,
    calls: Calls,
) -> (Core, Vec<OrderEvent>, Vec<OrderIntent>) {
    let heard: Heard = Arc::default();
    let mount = Mount {
        order_client_id_code: None,
        account: None,
        symbols,
        controller_id: Some("ol".to_string()),
        underlying_symbol: None,
        venue: VENUE.into(),
        symbol: SYM.into(),
        interval: "1m".into(),
        strategy: Box::new(Lists { calls, heard: Arc::clone(&heard) }),
    };
    let config = CoreConfig {
        strategy: Some(mount),
        journal: Some(crate::runtime::JournalConfig::at(dir.to_path_buf())),
        ..CoreConfig::default()
    };
    let mut c = core_of(engine_on(VENUE, SYM, RecordingClient::default()), Vec::new(), config);
    // What `spawn_core` turns on for a mounted primary, which `core_of` does not: the lifecycle buffer.
    c.engine.collect_applied_fills = true;
    c.drive_strategy_feed_status(VENUE, SYM, FeedStatus::Disconnected);
    c.dispatch_applied_fills();
    c.journal.as_mut().expect("journal").flush().expect("flush");
    let intents = Journal::read_all(dir)
        .expect("readable")
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::StrategySubmit { intent, .. } => Some(intent),
            _ => None,
        })
        .collect();
    let heard = heard.lock().unwrap().clone();
    (c, heard, intents)
}

/// Each verb buffers ONE list in the order-list lane and nothing in another: `submit_oco` links its two legs
/// OCO to each other (their own links replaced), `submit_ladder` buffers `vike_model::build_ladder`'s legs or
/// its refusal in words, `submit_group` its legs verbatim.
#[test]
fn each_verb_buffers_one_list_linked_as_it_says() {
    let mut b = LiveBroker {
        data_delay: None,
        inputs: None,
        closed_trades: None,
        position: 0.0,
        average_price: 0.0,
        price: 100.0,
        equity: 10_000.0,
        bars: Arc::new(Vec::new()),
        index: 0,
        now: 0,
        contract_value: vike_model::ContractValue::default(),
        quote_rate: 1.0,
        lot_size: 0.0,
        tick_size: 0.0,
        min_qty: 0.0,
        min_notional: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        leg_values: None,
        brackets: Vec::new(),
        order_lists: Vec::new(),
        combos: Vec::new(),
        order_emulation_arms: Vec::new(),
        mass_cancel: false,
    };
    let (take_profit, stop_loss) = pair();
    let stale = OrderListLeg {
        parent: Some(7),
        contingency: Some(ContingencyType::Ouo),
        linked: vec![3],
        ..stop_loss.clone()
    };
    b.submit_oco(take_profit.clone(), stale);
    b.submit_ladder(1, 2.0, Some(100.0), 95.0, targets());
    b.submit_ladder(1, 2.0, Some(100.0), 95.0, vec![LadderTarget { price: 105.0, qty: 3.0 }]);
    b.submit_group(vec![take_profit.clone(), stop_loss.clone()]);

    assert!(b.submissions.is_empty() && b.brackets.is_empty() && b.order_emulation_arms.is_empty());
    let [oco, good, past, group] = b.order_lists.as_slice() else {
        panic!("one list per verb call, got {}", b.order_lists.len())
    };
    let oco_to = |leg: &OrderListLeg, other| OrderListLeg {
        contingency: Some(ContingencyType::Oco),
        linked: vec![other],
        ..leg.clone()
    };
    assert_eq!(
        oco.legs,
        Ok(vec![oco_to(&take_profit, 1), oco_to(&stop_loss, 0)]),
        "the verb's links, not the leg's"
    );
    let want = vike_model::build_ladder(&ladder("", "", targets())).expect("a good ladder").legs;
    assert_eq!(good.legs, Ok(want));
    let words =
        vike_model::build_ladder(&ladder("", "", vec![LadderTarget { price: 105.0, qty: 3.0 }]))
            .expect_err("targets past the entry's size");
    assert_eq!(past.legs, Err(words));
    assert_eq!(group.legs, Ok(vec![take_profit, stop_loss]), "verbatim");
}

/// **A buffered list reaches the core as ONE `OrderList`** (never one intent per leg), with the drain's venue
/// and symbol stamped and no account (the mount's route names it), and nothing is denied.
#[test]
fn a_buffered_list_reaches_the_core_as_one_order_list() {
    let dir = crate::scratch::Scratch::reserved("strategy-order-list-reaches-the-core");
    let (_, heard, intents) = run(&dir, |b| {
        let (take_profit, stop_loss) = pair();
        b.submit_oco(take_profit, stop_loss);
        b.submit_ladder(1, 2.0, Some(100.0), 95.0, targets());
    });

    let lists: Vec<&vike_model::OrderList> = intents
        .iter()
        .map(|i| match i {
            OrderIntent::OrderList(spec) => spec.as_ref(),
            other => panic!("only order lists were buffered: {other:?}"),
        })
        .collect();
    assert_eq!(lists.len(), 2, "one intent per list: {intents:?}");
    let (take_profit, stop_loss) = pair();
    let oco = vike_model::OrderList {
        venue: VENUE.into(),
        symbol: SYM.into(),
        legs: vec![
            OrderListLeg {
                contingency: Some(ContingencyType::Oco),
                linked: vec![1],
                ..take_profit
            },
            OrderListLeg { contingency: Some(ContingencyType::Oco), linked: vec![0], ..stop_loss },
        ],
        account: None,
    };
    assert_eq!(*lists[0], oco);
    let want = vike_model::build_ladder(&ladder(VENUE, SYM, targets())).expect("a good ladder");
    assert_eq!(*lists[1], want, "the ladder, its venue and symbol stamped");
    assert!(
        heard.iter().all(|e| !matches!(e.kind, OrderEventKind::Denied { .. })),
        "nothing denied: {heard:?}"
    );
}

/// **A list trades the symbol `place_group` names** (one broker plan, step 12): on a mount that declares
/// `Y` beside its own `X`, a list naming `Y` reaches the core on `Y` (it used to trade the dispatching
/// series whatever it named), one naming nothing trades the mount's own, and one naming a symbol the mount
/// did not declare is refused before anything is minted, each tagged leg hearing its own tag.
#[test]
fn a_list_trades_the_declared_symbol_it_names() {
    let dir = crate::scratch::Scratch::reserved("strategy-order-list-names-its-symbol");
    let (_, heard, intents) = run_declaring(&dir, vec![MountLeg::same_venue("Y")], |b| {
        let (take_profit, stop_loss) = pair();
        let oco = |tag_a: &str, tag_b: &str| {
            vec![
                (
                    tag_a.to_string(),
                    OrderListLeg {
                        contingency: Some(ContingencyType::Oco),
                        linked: vec![1],
                        ..take_profit.clone()
                    },
                ),
                (
                    tag_b.to_string(),
                    OrderListLeg {
                        contingency: Some(ContingencyType::Oco),
                        linked: vec![0],
                        ..stop_loss.clone()
                    },
                ),
            ]
        };
        vike_model::OrderBroker::place_group(b, "Y", oco("y-tp", "y-sl"));
        vike_model::OrderBroker::place_group(b, "", oco("x-tp", "x-sl"));
        vike_model::OrderBroker::place_group(b, "Z", oco("z-tp", "z-sl"));
    });

    let symbols: Vec<&str> = intents
        .iter()
        .map(|i| match i {
            OrderIntent::OrderList(spec) => spec.symbol.as_str(),
            other => panic!("only order lists were buffered: {other:?}"),
        })
        .collect();
    assert_eq!(symbols, ["Y", SYM], "the named leg, then the mount's own; Z is not journaled");
    let refused: Vec<Option<&str>> = heard
        .iter()
        .filter(|e| matches!(e.kind, OrderEventKind::Denied { .. }))
        .map(|e| e.tag.as_deref())
        .collect();
    assert_eq!(refused, [Some("z-tp"), Some("z-sl")], "each leg of the refused list hears its tag");
}

/// **A list the rules refuse is `Denied` WHOLE, in words**: a ladder `build_ladder` refuses, a leg whose
/// terms (`order_terms_refusal`) or lifetime (`lifetime_refusal`: a leg carries no expiry) the submit lane's
/// rules refuse, named by its index, and links `build_order_list` cannot resolve. Nothing is journaled,
/// minted, sent or registered, and the strategy hears one `Denied` in the rule's words under a refusal id
/// routed back to the mount.
#[test]
fn a_list_the_rules_refuse_is_denied_whole_in_words_before_anything_is_minted() {
    let past = vec![LadderTarget { price: 105.0, qty: 3.0 }];
    let unlinked = || {
        let (take_profit, stop_loss) = pair();
        vike_model::OrderList {
            venue: VENUE.into(),
            symbol: SYM.into(),
            legs: vec![take_profit, stop_loss],
            account: None,
        }
    };
    let cases: Vec<(Calls, String)> = vec![
        (
            (|b: &mut LiveBroker| {
                let past = vec![LadderTarget { price: 105.0, qty: 3.0 }];
                b.submit_ladder(1, 2.0, Some(100.0), 95.0, past);
            }) as Calls,
            vike_model::build_ladder(&ladder(VENUE, SYM, past)).expect_err("past the entry's size"),
        ),
        (
            (|b: &mut LiveBroker| {
                let (take_profit, stop_loss) = pair();
                b.submit_oco(take_profit, OrderListLeg { trigger_price: Some(0.0), ..stop_loss });
            }) as Calls,
            format!(
                "leg 1: {}",
                vike_model::order_terms_refusal(
                    &OrderType::Stop,
                    None,
                    Some(0.0),
                    None,
                    None,
                    None,
                    None
                )
                .expect("a stop at zero")
            ),
        ),
        (
            (|b: &mut LiveBroker| {
                let (take_profit, stop_loss) = pair();
                b.submit_oco(
                    OrderListLeg { time_in_force: TimeInForce::Gtd, ..take_profit },
                    stop_loss,
                );
            }) as Calls,
            format!(
                "leg 0: {}",
                vike_model::lifetime_refusal(&OrderType::Limit, TimeInForce::Gtd, None, None, 0)
                    .expect("a gtd with no expiry")
            ),
        ),
        (
            (|b: &mut LiveBroker| {
                let (take_profit, stop_loss) = pair();
                b.submit_group(vec![take_profit, stop_loss]);
            }) as Calls,
            vike_model::build_order_list(&unlinked(), &["a".into(), "b".into()])
                .expect_err("two legs linked to nothing"),
        ),
    ];
    for (n, (calls, words)) in cases.into_iter().enumerate() {
        let dir = crate::scratch::Scratch::reserved(&format!("strategy-order-list-refused-{n}"));
        let (c, heard, intents) = run(&dir, calls);

        assert!(intents.is_empty(), "{words}: nothing journaled: {intents:?}");
        assert!(c.engine.client.submissions.is_empty(), "{words}: nothing reaches the venue");
        assert!(c.engine.registry.is_empty(), "{words}: nothing was registered");
        assert_eq!(
            c.order_client_id_gen.state().1,
            0,
            "{words}: validate-then-mint: no order client id burned"
        );
        let [event] = heard.as_slice() else { panic!("{words}: one event: {heard:?}") };
        assert_eq!(event.kind, OrderEventKind::Denied { reason: words.clone() });
        assert_eq!(
            c.order_client_id_mount.get(&event.order_client_id),
            Some(&0),
            "{words}: the mount's"
        );
    }
}

// ---- the COMBO (wave 6) -----------------------------------------------------------------------------------------

/// A two-leg call spread, priced by its legs when `limits` names them.
fn spread_legs(limits: Option<(f64, f64)>) -> Vec<vike_model::OrderComboLeg> {
    let leg = |symbol: &str, ratio, limit| vike_model::OrderComboLeg {
        symbol: symbol.into(),
        ratio,
        limit,
    };
    vec![leg("C1", 1, limits.map(|l| l.0)), leg("C2", -1, limits.map(|l| l.1))]
}

/// **A buffered combo reaches the core as ONE `Combo`**, its venue stamped by the drain and no account on the spec
/// (the mount's route names the book, the bracket's way), its legs, ratios and limits verbatim. What the core then says
/// of it (its gate, its venue row) is the core's verdict, not the drain's.
#[test]
fn a_buffered_combo_reaches_the_core_as_one_combo() {
    let dir = crate::scratch::Scratch::reserved("strategy-combo-reaches-the-core");
    let (_, _, intents) = run(&dir, |b| {
        b.submit_combo(1, 2.0, spread_legs(None), Some(-0.03));
        b.submit_combo(-1, 1.0, spread_legs(Some((0.05, 0.02))), None);
    });
    let combos: Vec<&vike_model::OrderCombo> = intents
        .iter()
        .map(|i| match i {
            OrderIntent::Combo(spec) => spec.as_ref(),
            other => panic!("only combos were buffered: {other:?}"),
        })
        .collect();
    let want = |side, qty, limits, net_limit| vike_model::OrderCombo {
        venue: VENUE.into(),
        side,
        qty,
        legs: spread_legs(limits),
        net_limit,
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    assert_eq!(
        combos,
        [&want(1, 2.0, None, Some(-0.03)), &want(-1, 1.0, Some((0.05, 0.02)), None)]
    );
}

/// **A combo the model refuses is `Denied` WHOLE, in its words**, before anything is journaled, minted or sent: a
/// net beside leg limits, and one leg.
#[test]
fn a_combo_the_model_refuses_is_denied_whole_before_anything_is_minted() {
    let cases: Vec<(Calls, String)> = vec![
        (
            (|b: &mut LiveBroker| {
                b.submit_combo(1, 1.0, spread_legs(Some((0.05, 0.02))), Some(0.03))
            }) as Calls,
            vike_model::ComboError::NetAndLegLimits.to_string(),
        ),
        (
            (|b: &mut LiveBroker| b.submit_combo(1, 1.0, spread_legs(None)[..1].to_vec(), None))
                as Calls,
            vike_model::ComboError::TooFewLegs(1).to_string(),
        ),
    ];
    for (n, (calls, words)) in cases.into_iter().enumerate() {
        let dir = crate::scratch::Scratch::reserved(&format!("strategy-combo-refused-{n}"));
        let (c, heard, intents) = run(&dir, calls);
        assert!(intents.is_empty(), "{words}: nothing journaled: {intents:?}");
        assert!(c.engine.client.submissions.is_empty(), "{words}: nothing reaches the venue");
        assert_eq!(c.order_client_id_gen.state().1, 0, "{words}: no order client id burned");
        let [event] = heard.as_slice() else { panic!("{words}: one event: {heard:?}") };
        assert_eq!(event.kind, OrderEventKind::Denied { reason: words.clone() });
    }
}
