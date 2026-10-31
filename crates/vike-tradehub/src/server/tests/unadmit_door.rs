//! The give-back of an admitted symbol at the control door (`WireCommand::UnadmitSymbol`, decision 0205).
//!
//! Two layers, as the admission's own suite has. The PLAN ([`plan_unadmit`]) is a pure function of the
//! published block and orders, so each rule has a row: only a symbol the engine ADMITTED is a slot, it must be
//! flat with no order that has not ended (an armed conditional is one), the frame carries the block's spelling, and a
//! node with no block to judge by lets the frame go to the core. The END-TO-END test drives `accept_command` into
//! a REAL core: the statement reaches the engine, which lets the symbol go, and the facts source is told the
//! request (it forgets what it kept when the published block shows the engine let go).

use super::*;
use crate::server::unadmit::{UnadmitPlan, plan_unadmit};
use vike_exec::{Command, OrderView, PositionView, VenueBlock};
use vike_model::OrderStatus;
use vike_model::accounts::account_keys::AccountLabel;

fn unadmit(venue: &str, account: &str, symbol: &str) -> WireCommand {
    WireCommand::UnadmitSymbol {
        venue: venue.into(),
        account: account.into(),
        symbol: symbol.into(),
    }
}

fn block(
    route_key: &str,
    mounted: &str,
    extra: &[&str],
    positions: Vec<PositionView>,
) -> VenueBlock {
    VenueBlock {
        venue: route_key.split('#').next().unwrap().into(),
        route_key: route_key.into(),
        symbol: mounted.into(),
        extra_symbols: extra.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
        positions,
        ..Default::default()
    }
}

fn position(symbol: &str, size: f64) -> PositionView {
    PositionView {
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
    }
}

fn order(account: Option<&str>, symbol: &str, status: OrderStatus) -> OrderView {
    OrderView {
        data_delay: None,
        client_order_id: "o-1".to_string(),
        venue: "binance".to_string(),
        account: account.map(|a| AccountLabel::parse(a).expect("a legal label")),
        symbol: symbol.to_string(),
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
        pending_price: None,
    }
}

fn refusal(plan: UnadmitPlan) -> String {
    match plan {
        UnadmitPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn every_other_verb_passes_the_gate_untouched() {
    let blocks = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![])];
    assert_eq!(plan_unadmit(&WireCommand::Cancel("c".into()), &blocks, &[]), UnadmitPlan::Pass);
    assert_eq!(
        plan_unadmit(&submit("binance", "ETHUSDT.P", None), &blocks, &[]),
        UnadmitPlan::Pass
    );
}

#[test]
fn a_quiet_admitted_symbol_is_released_in_the_engines_own_spelling() {
    let blocks = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![])];
    // Typed in any case, sent in the list's.
    assert_eq!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ethusdt.p"), &blocks, &[]),
        UnadmitPlan::Release { route_key: "binance".into(), symbol: "ETHUSDT.P".into() }
    );
    // A labelled account is its own book: its key, its block.
    let two = [
        block("binance", "BTCUSDT", &[], vec![]),
        block("binance#ALT", "BTCUSDT", &["SOLUSDT"], vec![]),
    ];
    assert_eq!(
        plan_unadmit(&unadmit("binance", "ALT", " SOLUSDT "), &two, &[]),
        UnadmitPlan::Release { route_key: "binance#ALT".into(), symbol: "SOLUSDT".into() }
    );
    // A flat position entry (the size a closed position leaves) and ended orders hold nothing.
    let flat = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![position("ETHUSDT.P", 0.0)])];
    let ended = [order(None, "ETHUSDT.P", OrderStatus::Canceled)];
    assert!(matches!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &flat, &ended),
        UnadmitPlan::Release { .. }
    ));
}

#[test]
fn the_engines_own_symbol_and_an_unlisted_one_are_refused_in_words() {
    let blocks = [block("binance", "BTCUSDT", &["ETHUSDT.P", "SOLUSDT"], vec![])];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "btcusdt"), &blocks, &[]));
    assert!(why.contains("started on") && why.contains("no slot"), "{why}");
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "XRPUSDT"), &blocks, &[]));
    assert!(
        why.contains("XRPUSDT")
            && why.contains("ETHUSDT.P, SOLUSDT")
            && why.contains("nothing to give back"),
        "it names what it did admit: {why}"
    );
    let none = [block("binance", "BTCUSDT", &[], vec![])];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT"), &none, &[]));
    assert!(why.contains("admitted none"), "{why}");
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "  "), &blocks, &[]));
    assert!(why.contains("empty symbol"), "{why}");
}

#[test]
fn a_position_or_a_live_order_in_the_symbol_refuses_it_naming_the_cause() {
    let held = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![position("ETHUSDT.P", -2.5)])];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &held, &[]));
    assert!(why.contains("position of -2.5") && why.contains("close it first"), "{why}");

    let quiet = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![])];
    // Resting, pre-ack, and a conditional held by the core: each is a live order in the symbol.
    for status in [OrderStatus::Accepted, OrderStatus::Initialized, OrderStatus::Emulated] {
        let orders = [order(None, "ETHUSDT.P", status)];
        let why =
            refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &quiet, &orders));
        assert!(why.contains("live order") && why.contains("o-1"), "{status:?}: {why}");
    }
    // An order of ANOTHER account of the exchange, or in another symbol, holds nothing of this one.
    let others = [
        order(Some("ALT"), "ETHUSDT.P", OrderStatus::Accepted),
        order(None, "SOLUSDT", OrderStatus::Accepted),
    ];
    assert!(matches!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &quiet, &others),
        UnadmitPlan::Release { .. }
    ));
}

#[test]
fn a_node_with_nothing_to_judge_by_lets_the_core_answer_and_the_newest_block_speaks() {
    assert_eq!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &[], &[]),
        UnadmitPlan::Pass
    );
    let elsewhere = [block("bybit", "BTCUSDT", &["ETHUSDT"], vec![])];
    assert_eq!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &elsewhere, &[]),
        UnadmitPlan::Pass,
        "no block for the key: the account gate's business, not this one's"
    );
    // After a restart of the account, the stopped engine's block sits before the new one.
    let restarted = [
        block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![]),
        block("binance", "BTCUSDT", &[], vec![]),
    ];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &restarted, &[]));
    assert!(why.contains("not one of the symbols"), "{why}");
    let why = refusal(plan_unadmit(&unadmit("binance", "no good", "ETHUSDT.P"), &restarted, &[]));
    assert!(why.contains("`account`"), "{why}");
}

/// The door asks the ENGINE'S question (`ExecutionEngine::unadmit_symbol`): an order in the symbol that has not
/// ENDED, whatever its status. `Liquidated` is the one status on which `is_live()` and `!is_terminal()` disagree, and
/// the engine refuses on it, so a door that read `is_live()` queued the frame and let the engine refuse it unseen.
/// A LABELLED account is judged by its own route key.
#[test]
fn every_status_that_has_not_ended_refuses_the_give_back_as_the_engine_does() {
    let quiet = [block("binance#ALT", "BTCUSDT", &["ETHUSDT.P"], vec![])];
    for status in [
        OrderStatus::Initialized,
        OrderStatus::Submitted,
        OrderStatus::Accepted,
        OrderStatus::Triggered,
        OrderStatus::PartiallyFilled,
        OrderStatus::PendingCancel,
        OrderStatus::Liquidated,
        OrderStatus::Emulated,
        OrderStatus::Released,
    ] {
        let orders = [order(Some("ALT"), "ETHUSDT.P", status)];
        let why = refusal(plan_unadmit(&unadmit("binance", "ALT", "ETHUSDT.P"), &quiet, &orders));
        assert!(
            why.contains("binance#ALT")
                && why.contains("o-1")
                && why.contains(status.as_str())
                && why.contains("cancel it first"),
            "{status:?} names the order and says what to do: {why}"
        );
    }
    for status in [
        OrderStatus::Filled,
        OrderStatus::Canceled,
        OrderStatus::Rejected,
        OrderStatus::Denied,
        OrderStatus::Expired,
    ] {
        let orders = [order(Some("ALT"), "ETHUSDT.P", status)];
        assert!(
            matches!(
                plan_unadmit(&unadmit("binance", "ALT", "ETHUSDT.P"), &quiet, &orders),
                UnadmitPlan::Release { .. }
            ),
            "{status:?} has ended and holds nothing"
        );
    }
}

/// A venue that takes an order and RESTS it (submitted, accepted, never filled). The suite's scripted venue fills
/// every order at once, which leaves no order to judge a give-back by.
struct RestingVenue {
    pending: VecDeque<Event>,
}

impl ExecutionClient for RestingVenue {
    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }
    fn submit(&mut self, request: &OrderRequest) {
        self.pending.push_back(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.pending.push_back(Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: Some(format!("v-{}", request.client_order_id).into()),
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn detach(&mut self) {}
}

/// A core of `bybit` engines on a resting venue: the default account on `BTCUSDT` and one LABELLED account of
/// the exchange per label (route key `bybit#LABEL`), as the node mounts them.
fn resting_node(labels: &[&str]) -> Node {
    let engine = |route_key: String| {
        let limits =
            RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, "bybit", None, BalanceMode::Delta),
            RiskGate::new(limits),
            RestingVenue { pending: VecDeque::new() },
            "bybit",
            "BTCUSDT",
        );
        engine.tier = AccountTier::Demo;
        engine.route_key = route_key;
        engine.gate.limits.deny_unadmitted_symbols = true;
        engine
    };
    let clock = Arc::new(AtomicI64::new(0));
    let core = vike_core::spawn_core_multi(
        engine("bybit".to_string()),
        labels.iter().map(|l| (10_000.0, engine(format!("bybit#{l}")))).collect::<Vec<_>>(),
        vike_core::CoreConfig {
            seed_cash: 10_000.0,
            clock: Box::new(move || clock.fetch_add(1, Ordering::Relaxed)),
            ..vike_core::CoreConfig::default()
        },
    );
    let node = Node {
        core,
        submitted: Arc::default(),
        limits: ControlLimits::new(ControlLimitsConfig { max_notional: None, rate_per_sec: 1e9 }),
    };
    node.publish("bybit");
    node
}

/// A limit buy of ETHUSDT on `account` (`DEFAULT` is the unlabelled book).
fn eth_limit_on(coid: &str, account: Option<&str>) -> WireCommand {
    let WireCommand::Submit(mut req) = limit(coid, "bybit", "ETHUSDT", 0.01, 3_000.0) else {
        unreachable!()
    };
    req.account = account.map(str::to_string);
    WireCommand::Submit(req)
}

/// **The live finding (v0.1.97, bybit demo, account `bybit#DEMO`)**: the verb was Acked ("the node queued the
/// give-back") with an order of ours resting in the symbol, and only the engine refused it in its log. The door
/// was handed no orders at all (`named_orders` named none for this verb), so it could not refuse a symbol with an
/// order in it on ANY engine, the default one included; the proof read the command line client's `"account": null`
/// on every order row (that client prints no account on an order) as a labelled-key mismatch. The published order
/// of a labelled engine DOES name its account, which this pins first.
///
/// Three accounts of one exchange hold ETHUSDT: the default and `DEMO` each with a resting order, `ALT` with none.
/// The first two are refused at the door BY NAME (the order, its status, "cancel it first"), the third is queued
/// although the others rest in the same symbol, and an account is never judged by another's order.
#[test]
fn a_resting_order_refuses_the_give_back_of_a_labelled_engine_as_of_the_default_one() {
    let mut node = resting_node(&["DEMO", "ALT"]);
    let facts = Table::eth();
    node.send_rostered(eth_limit_on("lab-1", Some("DEMO")), Some(&facts))
        .expect("admits and rests");
    // `DEFAULT` names the unlabelled book deliberately: an order naming no account is ambiguous on a venue that runs
    // several, and the core refuses it, so it would never rest.
    node.send_rostered(eth_limit_on("def-1", Some("DEFAULT")), Some(&facts))
        .expect("admits and rests");
    node.core.send_command(Command::AdmitSymbol(Box::new(admission_of("bybit#ALT", "ETHUSDT"))));
    let cell = node.core.snapshot_cell();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snap = cell.load();
        if snap.portfolio.venues.iter().filter(|b| b.trades("ETHUSDT")).count() == 3
            && snap.orders.iter().filter(|o| !o.status.is_terminal()).count() == 2
        {
            break;
        }
        let state = format!(
            "blocks {:?}; orders {:?}",
            snap.portfolio
                .venues
                .iter()
                .map(|b| format!("{} {:?}", b.route_key, b.extra_symbols))
                .collect::<Vec<_>>(),
            snap.orders
                .iter()
                .map(|o| format!(
                    "{} {} {:?} {}",
                    o.client_order_id,
                    o.symbol,
                    o.account,
                    o.status.as_str()
                ))
                .collect::<Vec<_>>()
        );
        assert!(
            Instant::now() < deadline,
            "the three engines never published ETHUSDT as traded with both orders resting: {state}"
        );
        drop(snap);
        std::thread::sleep(Duration::from_millis(5));
    }

    // The premise: a LABELLED engine's order names its account in the published snapshot.
    let published = node.core.snapshot_cell().load().orders.clone();
    let named = |coid: &str| {
        published
            .iter()
            .find(|o| o.client_order_id == coid)
            .and_then(|o| o.account.as_ref().and_then(|a| a.text()).map(str::to_string))
    };
    assert_eq!(named("lab-1").as_deref(), Some("DEMO"), "{published:?}");
    assert_eq!(named("def-1"), None, "the default account names none");

    for (account, coid, key) in [("DEMO", "lab-1", "bybit#DEMO"), ("DEFAULT", "def-1", "bybit")] {
        let status = published.iter().find(|o| o.client_order_id == coid).unwrap().status;
        let why = node
            .send_rostered_published(unadmit("bybit", account, "ethusdt"), Some(&facts))
            .expect_err("an order of the account rests in the symbol");
        assert!(
            why.contains(key)
                && why.contains(coid)
                && why.contains(status.as_str())
                && why.contains("cancel it first"),
            "{account}: refused by name at the door: {why}"
        );
    }
    assert!(
        !facts.events().iter().any(|e| e.starts_with("release_requested")),
        "a refused frame asks nothing of the source: {:?}",
        facts.events()
    );

    node.send_rostered_published(unadmit("bybit", "ALT", "ETHUSDT"), Some(&facts))
        .expect("ALT holds no order in the symbol: queued, whatever the other accounts rest");
    wait_until("ALT's engine to let ETHUSDT go", || {
        node.blocks().iter().all(|b| b.route_key != "bybit#ALT" || !b.trades("ETHUSDT"))
    });
    let traded: Vec<String> =
        node.blocks().iter().filter(|b| b.trades("ETHUSDT")).map(|b| b.route_key.clone()).collect();
    assert_eq!(traded, ["bybit", "bybit#DEMO"], "the two accounts with an order keep the symbol");
    node.core.shutdown_and_join();
}

fn admission_of(route_key: &str, symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        route_key: route_key.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(3_000.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
        release: false,
        margin_mode: None,
    }
}

/// **End to end**: the verb through `accept_command` into a real core. The engine admitted ETH, the verb is
/// accepted for the lower-case spelling, the facts source is told the request, the engine lets the symbol go
/// (the published block stops listing it), and the same verb again is refused in words because the block no
/// longer lists it.
#[test]
fn the_verb_gives_an_admitted_symbol_back_through_a_real_core() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let facts = Table::eth();
    node.core.send_command(Command::AdmitSymbol(Box::new(admission_of("bybit", "ETHUSDT"))));
    node.blocks_once_trading("bybit", "ETHUSDT");

    let why = node
        .send_rostered(unadmit("bybit", "DEFAULT", "SOLUSDT"), Some(&facts))
        .expect_err("a symbol it never admitted");
    assert!(why.contains("not one of the symbols") && why.contains("ETHUSDT"), "{why}");
    assert!(
        !facts.events().iter().any(|e| e.starts_with("release_requested")),
        "a refused frame asks nothing of the source"
    );

    node.send_rostered(unadmit("bybit", "DEFAULT", "ethusdt"), Some(&facts))
        .expect("an admitted, quiet symbol is given back");
    assert!(
        facts.events().iter().any(|e| e == "release_requested:ETHUSDT"),
        "the source is told, in the engine's spelling: {:?}",
        facts.events()
    );
    wait_until("the engine to let the symbol go", || {
        node.blocks().iter().all(|b| !b.trades("ETHUSDT"))
    });

    let why = node
        .send_rostered(unadmit("bybit", "DEFAULT", "ETHUSDT"), Some(&facts))
        .expect_err("it is no longer admitted");
    assert!(why.contains("not one of the symbols") && why.contains("admitted none"), "{why}");
    // An account the node does not run is the account gate's refusal, before this one.
    let why = node
        .send_rostered(unadmit("bybit", "NOSUCH", "ETHUSDT"), Some(&facts))
        .expect_err("no such account");
    assert!(why.contains("NOSUCH"), "{why}");
    node.core.shutdown_and_join();
}

/// A symbol the engine holds a position in is refused AT THE DOOR once the published block shows it, and the
/// symbol stays: the give-back of a held symbol is not a way to lose a position's pricing.
#[test]
fn a_symbol_with_a_position_is_refused_at_the_door_and_stays_admitted() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let facts = Table::eth();
    node.send(limit("open", "bybit", "ETHUSDT", 0.01, 3_000.0), Some(&facts)).expect("admitted");
    node.venue_saw("open");
    node.blocks_once_trading("bybit", "ETHUSDT");
    wait_until("the position to publish", || {
        node.blocks()
            .iter()
            .any(|b| b.positions.iter().any(|p| p.symbol.as_str() == "ETHUSDT" && p.size > 0.0))
    });
    let why = node
        .send_rostered(unadmit("bybit", "DEFAULT", "ETHUSDT"), Some(&facts))
        .expect_err("it holds a position");
    assert!(why.contains("holds a position") && why.contains("close it first"), "{why}");
    assert!(node.blocks().iter().any(|b| b.trades("ETHUSDT")), "the symbol stays admitted");
    node.core.shutdown_and_join();
}
