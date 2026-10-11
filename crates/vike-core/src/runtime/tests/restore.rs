//! The order-RESTORE hook of the reconcile pass (`crate::runtime::restore`): after a restart, the
//! orders a previous session left resting go back into the engine registry at the first successful
//! pass, and their owning mounts hear what became of them.
//!
//! Every "restart" is the real one minus the process, as in `order_owners_tests`: boot 1 mints a
//! tagged `q` order through a mount and is DROPPED (the order stays "resting at the venue"), boot 2
//! is assembled over the same ownership file, and the venue's side of the story is a hand-built
//! `ReconcileReports` handed to `reconcile_reports` followed by `dispatch_applied_fills`, which is
//! exactly what the fold's `handle` does after a `Command::ReconcileReports`. The mounts are
//! `order_owners_tests::TagSpy`s, which record every lifecycle event they hear WITH its tag.
//!
//! The venue gate (`CoreConfig::restore_venue_gate`) is opened by hand: every row of the declared
//! table is off today, so the default gate would make every test below vacuous, and the one test
//! that wants the default says so.

use super::order_owners_tests::{
    Events, NOW, NextQuote, TagSpy, assembled, binance_engine, cell, config_on, noted, q_key,
    quote, restored_q, spy_mount,
};
use super::runtime_mount_tests::{mount_cmd, spec, unmount_cmd};
use super::*;
use crate::order_owners::{ORDER_OWNERS_FILE, OrderOwnerLog, OwnedOrder};
use crate::runtime::test_support::core_of;
use crate::scratch::Scratch;
use vike_exec::recon::{DivergenceKind, RESTORE_GONE_REASON, ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{LiquiditySide, OrderCanceled, TradeId};
use vike_model::{FillReport, OrderStatusReport};

type Core = CoreThread<RecordingClient>;

/// `config` with the venue gate open: the declared table answers `false` for every venue today.
fn open(mut config: CoreConfig) -> CoreConfig {
    config.restore_venue_gate = |_| true;
    config
}

/// What the spy of `events` heard so far: `(Debug of the kind, tag)`.
fn heard(events: &Events) -> Vec<(String, Option<String>)> {
    events.lock().unwrap().clone()
}

/// The `(kind, tag)` an `Accepted` carrying the restored `q` tag reads as.
fn accepted_q() -> (String, Option<String>) {
    ("Accepted".to_string(), Some("q".to_string()))
}

/// Boot 1: a `q` quote rests under mount `maker-b` on `sim`/`BTCUSDT`, then the process "dies".
fn rest_one(dir: &Scratch) -> (String, NextQuote, Events) {
    let (nb, events) = (cell(), Events::default());
    let mut first = assembled(config_on(dir, vec![spy_mount(&nb, &events)]));
    let coid = quote(&mut first, &nb, 1);
    drop(first);
    events.lock().unwrap().clear();
    (coid, nb, events)
}

/// Boot 2 over the same file, gate open, same mount.
fn restart(dir: &Scratch, nb: &NextQuote, events: &Events) -> Core {
    assembled(open(config_on(dir, vec![spy_mount(nb, events)])))
}

/// A venue open-order row for `coid` on `venue`/`BTCUSDT`.
fn report_on(venue: &str, coid: &str, status: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: format!("v-{coid}").into(),
        client_order_id: Some(coid.to_string()),
        side: 1,
        order_type: "limit".into(),
        qty: 1.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: status.into(),
        ts: 5,
    }
}

fn report(coid: &str, status: &str) -> OrderStatusReport {
    report_on("sim", coid, status)
}

/// A venue fill row echoing `coid`, with a trade id local state has never folded.
fn fill_of(coid: &str) -> FillReport {
    FillReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        trade_id: TradeId::prefixed("t-", format_args!("{coid}")),
        venue_order_id: format!("v-{coid}").into(),
        client_order_id: Some(coid.to_string()),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}

/// One pass for `venue` (and `route_key`), `quarantine` so every divergence is HELD and visible.
fn pass_for(
    venue: &str,
    route_key: Option<&str>,
    orders: Vec<OrderStatusReport>,
    fills: Vec<FillReport>,
) -> ReconcileReports {
    ReconcileReports {
        venue: venue.into(),
        since: 0,
        orders,
        fills,
        positions: Vec::new(),
        policy: ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: route_key.map(str::to_string),
    }
}

/// Fold one `sim` pass and deliver what it buffered, as `handle` does after the command.
fn run(core: &mut Core, orders: Vec<OrderStatusReport>, fills: Vec<FillReport>) {
    core.reconcile_reports(pass_for("sim", None, orders, fills));
    core.dispatch_applied_fills();
}

/// The kinds of the held alerts.
fn alert_kinds(core: &Core) -> Vec<DivergenceKind> {
    core.recon_alerts.values().map(|a| a.kind).collect()
}

/// How many recent-events lines are restore notes.
fn restore_notes(core: &Core) -> usize {
    core.recent.iter().filter(|l| l.starts_with("RESTORE ")).count()
}

/// The coids the ownership file would hand a NEW boot, after `core` has been dropped (which drains
/// and joins its writer).
fn file_owns(core: Core, dir: &Scratch) -> Vec<String> {
    drop(core);
    OrderOwnerLog::in_state_dir(dir).start(NOW).coids.into_iter().map(|o| o.coid).collect()
}

/// 1 — **ADOPT.** The venue still reports the order: the registry holds it live, its mount hears
/// `Accepted` stamped with the RESTORED tag, the collection is spent, the counter and one note say
/// so, and the order raises no `UnknownOrder`.
#[test]
fn a_resting_order_is_adopted_and_its_mount_hears_accepted_with_its_tag() {
    let dir = Scratch::reserved("vco-restore-adopt");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert!(core.engine.registry.is_empty(), "precondition: the registry starts empty");

    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());

    assert!(core.engine.registry[&coid].status.is_live(), "adopted live");
    assert_eq!(heard(&events), vec![accepted_q()], "the mount's liveness confirmation, tagged");
    assert!(core.restored_orders.is_empty(), "the collection is spent");
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert_eq!(
        (core.restore_counters.restored_gone, core.restore_counters.restored_filled_while_down),
        (0, 0)
    );
    assert_eq!(restore_notes(&core), 1, "recent: {:?}", core.recent);
    assert!(noted(&core, &coid), "the note names the coid");
    assert!(alert_kinds(&core).is_empty(), "adoption precedes the diff: {:?}", alert_kinds(&core));
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&coid), "the tag still names the order");
}

/// 2 — **The headline.** After the adoption a REAL venue cancel folds against the adopted order and
/// reaches the mount's `on_order_event` stamped with the restored tag, and the tag entry is
/// retired: the delivery that restore exists for.
#[test]
fn a_real_cancel_after_the_adoption_reaches_the_mount_with_the_restored_tag() {
    let dir = Scratch::reserved("vco-restore-real-cancel");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());
    events.lock().unwrap().clear();

    core.bus.publish(
        Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.clone(),
            reason: String::new().into(),
            ts: 9,
        }),
        &mut core.engine,
    );
    core.dispatch_applied_fills();

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.starts_with("Canceled"), "{got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "stamped with the restored tag: {got:?}");
    assert!(core.strategy_tags.is_empty(), "the terminal retired the tag");
}

/// 3 — **Group 2 stays external.** A venue order the file does not know is not adopted and still
/// raises its `UnknownOrder`; the restored order beside it is adopted and raises none.
#[test]
fn a_venue_order_the_file_does_not_know_stays_external() {
    let dir = Scratch::reserved("vco-restore-external");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, vec![report(&coid, "ACCEPTED"), report("ext-1", "ACCEPTED")], Vec::new());

    assert!(core.engine.registry.contains_key(&coid));
    assert!(!core.engine.registry.contains_key("ext-1"), "an external order is untouched");
    assert_eq!(alert_kinds(&core), vec![DivergenceKind::UnknownOrder]);
    assert_eq!(heard(&events), vec![accepted_q()], "the external order reached no mount");
    assert_eq!(core.restore_counters.restored_adopted, 1);
}

/// 4 — **GONE.** The venue reports nothing for the order: the mount hears a synthetic `Canceled`
/// with the restore reason, stamped with the restored tag; the tag, the collection row and the
/// FILE entry are gone; the counter says so.
#[test]
fn an_absent_order_is_gone_the_mount_hears_the_reason_and_the_file_forgets_it() {
    let dir = Scratch::reserved("vco-restore-gone");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, Vec::new(), Vec::new());

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.starts_with("Canceled"), "{got:?}");
    assert!(got[0].0.contains(RESTORE_GONE_REASON), "the reason rides the event: {got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "stamped with its restored tag: {got:?}");
    assert!(core.strategy_tags.is_empty(), "the tag entry is dropped");
    assert!(core.restored_orders.is_empty());
    assert!(core.engine.registry.is_empty(), "a gone order is never put in the registry");
    assert_eq!(core.restore_counters.restored_gone, 1);
    assert_eq!(restore_notes(&core), 1, "recent: {:?}", core.recent);
    assert!(!file_owns(core, &dir).contains(&coid), "the ownership entry is forgotten");
}

/// 5 — **FILLED WHILE DOWN** is neither adopted nor gone. The fill report in the pass window takes
/// the existing `MissingFill` path (held here), the mount hears nothing from the restore, the file
/// keeps its entry, and the planner stops seeing the coid so a later pass cannot count it twice.
#[test]
fn a_fill_in_the_pass_window_is_counted_and_left_to_the_missing_fill_path() {
    let dir = Scratch::reserved("vco-restore-filled");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, Vec::new(), vec![fill_of(&coid)]);

    assert!(heard(&events).is_empty(), "the restore tells the mount nothing: {:?}", heard(&events));
    assert!(core.engine.registry.is_empty(), "not adopted");
    assert_eq!(core.restore_counters.restored_filled_while_down, 1);
    assert_eq!(
        (core.restore_counters.restored_adopted, core.restore_counters.restored_gone),
        (0, 0)
    );
    assert!(core.restored_orders.is_empty(), "off the planner's set, so it is counted once");
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&coid), "the tag is untouched");
    assert!(
        alert_kinds(&core).contains(&DivergenceKind::MissingFill),
        "the existing path owns the fill: {:?}",
        alert_kinds(&core)
    );
    assert!(file_owns(core, &dir).contains(&coid), "the file keeps the entry");
}

/// 6 — **A second pass changes nothing**, for each verdict: no second delivery, no recount, no
/// second note, no second registry write.
#[test]
fn a_second_pass_is_a_no_op_for_every_verdict() {
    for verdict in ["adopt", "gone", "filled"] {
        let dir = Scratch::reserved("vco-restore-idem");
        let (coid, nb, events) = rest_one(&dir);
        let mut core = restart(&dir, &nb, &events);
        let (orders, fills) = match verdict {
            "adopt" => (vec![report(&coid, "ACCEPTED")], Vec::new()),
            "gone" => (Vec::new(), Vec::new()),
            _ => (Vec::new(), vec![fill_of(&coid)]),
        };
        run(&mut core, orders.clone(), fills.clone());
        let (counters, got, notes, registry) = (
            core.restore_counters,
            heard(&events),
            restore_notes(&core),
            core.engine.registry.len(),
        );
        assert_eq!(notes, 1, "{verdict}: the first pass left one note");

        run(&mut core, orders, fills);

        assert_eq!(core.restore_counters, counters, "{verdict}: no recount");
        assert_eq!(heard(&events), got, "{verdict}: no second delivery");
        assert_eq!(restore_notes(&core), notes, "{verdict}: no second note");
        assert_eq!(core.engine.registry.len(), registry, "{verdict}: no second registry write");
    }
}

/// 7 — **The gate off is inert**: with the DEFAULT gate (every row of the table is off, and `sim` is
/// no roster venue) the entry waits, nothing is delivered or counted, and the venue order raises its
/// `UnknownOrder` exactly as before the feature. The default gate IS the declared table.
#[test]
fn a_venue_the_table_does_not_enable_restores_nothing() {
    let dir = Scratch::reserved("vco-restore-gate-off");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    assert!(core.restored_orders.contains_key(&coid), "precondition: restored at boot");

    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());

    assert!(core.engine.registry.is_empty());
    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert!(core.restored_orders.contains_key(&coid), "the entry waits for a later pass");
    assert_eq!(restore_notes(&core), 0);
    assert_eq!(alert_kinds(&core), vec![DivergenceKind::UnknownOrder]);

    let default_gate = CoreConfig::default().restore_venue_gate;
    for &v in vike_model::VENUES {
        assert_eq!(
            default_gate(v),
            vike_model::venues::venue_restore::restores_orders(v),
            "{v}: the default gate is the declared table"
        );
    }
}

/// 8 — **`restore_orders_off` is inert** even when the collection holds entries and the gate is open.
#[test]
fn the_operator_off_switch_stops_the_hook() {
    let dir = Scratch::reserved("vco-restore-off");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    core.config.restore_orders_off = true;

    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());

    assert!(core.engine.registry.is_empty());
    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert!(core.restored_orders.contains_key(&coid));
    assert_eq!(restore_notes(&core), 0);
}

/// 9 — **Nothing restored costs nothing and changes nothing**: a first boot, a pass with a venue
/// order: no note, no counter, no registry write, and the `UnknownOrder` as before.
#[test]
fn a_core_with_nothing_restored_leaves_the_pass_as_it_was() {
    let dir = Scratch::reserved("vco-restore-empty");
    let (nb, events) = (cell(), Events::default());
    let mut core = assembled(open(config_on(&dir, vec![spy_mount(&nb, &events)])));
    assert!(core.restored_orders.is_empty() && core.pending_owners.is_empty());

    run(&mut core, vec![report("ext-1", "ACCEPTED")], Vec::new());

    assert!(core.engine.registry.is_empty());
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert_eq!(restore_notes(&core), 0);
    assert_eq!(alert_kinds(&core), vec![DivergenceKind::UnknownOrder]);
}

/// 10 — **Entries outside the pass's scope stay for a later pass**: an entry on a symbol the engine
/// does not serve, and an old line with no venue or symbol, are not declared gone by a pass that
/// says nothing about them; the covered entry beside them is.
#[test]
fn entries_the_pass_cannot_speak_for_stay() {
    let dir = Scratch::reserved("vco-restore-scope");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    let far = OwnedOrder { symbol: Some("XYZUSDT".into()), tag: None, ..restored_q("far-1") };
    let old =
        OwnedOrder { coid: "old-1".into(), mount_id: "maker_b".into(), ..OwnedOrder::default() };
    for o in [far, old] {
        core.coid_mount.insert(o.coid.clone(), 0);
        core.restore_owned_order(0, o);
    }

    run(&mut core, Vec::new(), Vec::new());

    let left: Vec<&str> = core.restored_orders.keys().map(String::as_str).collect();
    assert_eq!(left, vec!["far-1", "old-1"], "only the covered entry was judged");
    assert_eq!(core.restore_counters.restored_gone, 1);
    let got = heard(&events);
    assert_eq!(got.len(), 1, "one Canceled, for {coid}: {got:?}");
}

/// A core with two accounts of `binance`: the default account (engine 0, mount `m-default`) and the
/// labelled `ALT` (engine 1, mount `m-alt`), each mount resting a `q` quote. Returns each mount's
/// events, the coids `(default, alt)`, and the boot-2 core over the same file.
fn two_accounts(dir: &Scratch) -> (Events, Events, (String, String), Core) {
    let (nd, na) = (cell(), cell());
    let (ed, ea) = (Events::default(), Events::default());
    let mount = |cid: &str, account: AccountLabel, next: &NextQuote, ev: &Events| {
        let mut m = spy_mount(next, ev);
        m.venue = "binance".into();
        m.controller_id = Some(cid.to_string());
        m.account = Some(account);
        m
    };
    let build = {
        let (nd, na, ed, ea) = (Arc::clone(&nd), Arc::clone(&na), Arc::clone(&ed), Arc::clone(&ea));
        move |open_gate: bool| {
            let mut config = config_on(
                dir,
                vec![
                    mount("m-default", AccountLabel::Default, &nd, &ed),
                    mount("m-alt", AccountLabel::parse("ALT").unwrap(), &na, &ea),
                ],
            );
            config.seed_cash = 1_000.0;
            if open_gate {
                config = open(config);
            }
            core_of(
                binance_engine("binance", 1_000.0),
                vec![(2_000.0, binance_engine("binance#ALT", 2_000.0))],
                config,
            )
        }
    };
    let mut first = build(false);
    *nd.lock().unwrap() = Some(1);
    *na.lock().unwrap() = Some(1);
    first.drive_strategy_feed_status("binance", "BTCUSDT", FeedStatus::Live);
    let default_coid =
        first.engine.client.submissions.last().expect("default quoted").client_order_id.clone();
    let alt_coid = first.extra_engines[0]
        .1
        .client
        .submissions
        .last()
        .expect("ALT quoted")
        .client_order_id
        .clone();
    drop(first);
    (ed, ea, (default_coid, alt_coid), build(true))
}

/// 11 — **The scope's account matches how the order's account was written, on both sides.** The
/// default account's entry carries no account and a labelled account's carries its label; a pass for
/// ONE account judges only its own entries (a silent `ALT` pass leaves the default entry alone), and
/// each adoption lands on its own engine.
#[test]
fn the_default_account_and_a_labelled_account_each_see_only_their_own_orders() {
    let dir = Scratch::reserved("vco-restore-accounts");
    let (ed, ea, (default_coid, alt_coid), mut core) = two_accounts(&dir);
    assert_eq!(core.restored_orders[&default_coid].account, None);
    assert_eq!(core.restored_orders[&alt_coid].account.as_deref(), Some("ALT"));

    // A silent ALT pass: ALT's order is gone; the default account's entry is not its business.
    core.reconcile_reports(pass_for("binance", Some("binance#ALT"), Vec::new(), Vec::new()));
    core.dispatch_applied_fills();
    assert_eq!(heard(&ea).len(), 1, "ALT's mount hears its gone order: {:?}", heard(&ea));
    assert!(heard(&ed).is_empty(), "the default mount hears nothing from ALT's pass");
    assert!(core.restored_orders.contains_key(&default_coid), "the default entry stays");
    assert!(!core.restored_orders.contains_key(&alt_coid));

    // The default pass lists the default order: adopted onto engine 0 and nowhere else.
    core.reconcile_reports(pass_for(
        "binance",
        Some("binance"),
        vec![report_on("binance", &default_coid, "ACCEPTED")],
        Vec::new(),
    ));
    core.dispatch_applied_fills();
    assert!(core.engine.registry.contains_key(&default_coid), "on the default engine");
    assert!(core.extra_engines[0].1.registry.is_empty(), "not on ALT's");
    assert_eq!(heard(&ed), vec![accepted_q()]);
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert_eq!(core.restore_counters.restored_gone, 1);
}

/// 12 — **A labelled account's adoption lands on its engine and keeps its engine row.** The ALT pass
/// lists ALT's order: engine 1's registry holds it, `coid_venue` routes its events there, and a real
/// cancel folds into engine 1 and reaches ALT's mount tagged.
#[test]
fn an_adopted_order_of_a_labelled_account_lands_on_that_accounts_engine() {
    let dir = Scratch::reserved("vco-restore-alt-engine");
    let (_ed, ea, (default_coid, alt_coid), mut core) = two_accounts(&dir);

    core.reconcile_reports(pass_for(
        "binance",
        Some("binance#ALT"),
        vec![report_on("binance", &alt_coid, "ACCEPTED")],
        Vec::new(),
    ));
    core.dispatch_applied_fills();

    assert!(core.extra_engines[0].1.registry[&alt_coid].status.is_live());
    assert!(core.engine.registry.is_empty(), "not on the default engine");
    assert_eq!(core.coid_venue.get(&alt_coid), Some(&1), "its events route to engine 1");
    assert_eq!(heard(&ea), vec![accepted_q()]);
    assert!(core.restored_orders.contains_key(&default_coid), "the default entry waits");

    let eng1 = &mut core.extra_engines[0].1;
    core.bus.publish(
        Event::OrderCanceled(OrderCanceled {
            client_order_id: alt_coid.clone(),
            reason: String::new().into(),
            ts: 9,
        }),
        eng1,
    );
    core.dispatch_applied_fills();
    let got = heard(&ea);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[1].0.starts_with("Canceled") && got[1].1.as_deref() == Some("q"), "{got:?}");
}

/// A runtime-mount factory whose strategies are [`TagSpy`]s over `next` and `events`.
fn spy_factory(next: &NextQuote, events: &Events) -> StrategyFactory {
    let (next, events) = (Arc::clone(next), Arc::clone(events));
    Box::new(move |_spec| {
        Ok(Box::new(TagSpy { next: Arc::clone(&next), events: Arc::clone(&events) }))
    })
}

/// A boot-1 core with ONE runtime mount `rt-b` that rests a `q` order, then dies; returns the coid.
fn rest_one_on_a_runtime_mount(dir: &Scratch, next: &NextQuote, events: &Events) -> String {
    let mut config = config_on(dir, Vec::new());
    config.strategy_factory = Some(spy_factory(next, events));
    let mut first = assembled(config);
    first.dispatch(mount_cmd(spec("rt-b")));
    let coid = quote(&mut first, next, 1);
    drop(first);
    events.lock().unwrap().clear();
    coid
}

/// A boot-2 core with NO mount yet (the runtime mount is resurrected later), gate open.
fn restart_without_the_mount(dir: &Scratch, next: &NextQuote, events: &Events) -> Core {
    let mut config = open(config_on(dir, Vec::new()));
    config.strategy_factory = Some(spy_factory(next, events));
    assembled(config)
}

/// 13 — **A mount that is not mounted yet: adopted, and told when it lands.** The order is adopted
/// into the registry by the pass (nobody to tell, the pending entry stays for the mount), and
/// when the runtime mount of that id lands, the next pass confirms it to the new mount WITHOUT
/// counting a second adoption.
#[test]
fn an_order_of_an_unmounted_mount_is_adopted_and_confirmed_when_its_mount_lands() {
    let dir = Scratch::reserved("vco-restore-pending-adopt");
    let (next, events) = (cell(), Events::default());
    let coid = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut core = restart_without_the_mount(&dir, &next, &events);
    assert!(core.pending_owners.contains_key("rt_b"), "precondition: waiting for its mount");

    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());
    assert!(core.engine.registry[&coid].status.is_live(), "adopted though nobody owns it yet");
    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert!(core.pending_owners["rt_b"].coids.iter().any(|o| o.coid == coid), "still pending");

    core.dispatch(mount_cmd(spec("rt-b")));
    assert!(core.restored_orders.contains_key(&coid), "bound with its mount");
    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());

    assert_eq!(heard(&events), vec![accepted_q()], "the new mount is told it is alive");
    assert_eq!(core.restore_counters.restored_adopted, 1, "no second adoption");
    assert!(core.restored_orders.is_empty());
}

/// 14 — **A mount that is not mounted: GONE skips the delivery.** The pass forgets the entry in the
/// file and says so in a note; no mount is told anything, and a mount of that id landing later
/// binds nothing.
#[test]
fn a_gone_order_of_an_unmounted_mount_is_forgotten_without_telling_anyone() {
    let dir = Scratch::reserved("vco-restore-pending-gone");
    let (next, events) = (cell(), Events::default());
    let coid = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut core = restart_without_the_mount(&dir, &next, &events);

    run(&mut core, Vec::new(), Vec::new());

    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters.restored_gone, 1);
    assert!(!core.pending_owners.contains_key("rt_b"), "nothing left to wait for");
    assert!(noted(&core, "no mount told"), "recent: {:?}", core.recent);

    core.dispatch(mount_cmd(spec("rt-b")));
    assert!(core.strategy_tags.is_empty() && core.restored_orders.is_empty(), "binds nothing");
    assert!(!file_owns(core, &dir).contains(&coid), "and the file forgot it");
}

/// 15 — **An explicit unmount before the pass withdraws the order from the restore**: the operator
/// removed the strategy, its rows left the collection, so the venue order stays an external one.
#[test]
fn an_unmounted_mounts_order_is_not_adopted() {
    let dir = Scratch::reserved("vco-restore-unmounted");
    let (next, events) = (cell(), Events::default());
    let coid = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut core = restart_without_the_mount(&dir, &next, &events);
    core.dispatch(mount_cmd(spec("rt-b")));
    core.dispatch(unmount_cmd("rt-b"));

    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());

    assert!(core.engine.registry.is_empty(), "not adopted");
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert_eq!(alert_kinds(&core), vec![DivergenceKind::UnknownOrder]);
}

/// 16 — **The adoption is what makes the tag-overwrite counter bite**: before the pass a fresh quote
/// over the restored tag counts nothing (`order_owners_tests`' test 15); after the order is adopted
/// the same quote leaves a live, tag-less order behind and is counted.
#[test]
fn after_the_adoption_a_fresh_quote_over_the_restored_tag_is_counted() {
    let dir = Scratch::reserved("vco-restore-overwrite");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());
    assert_eq!(core.restore_counters.tag_overwrite_orphaned, 0);

    let fresh = quote(&mut core, &nb, 1);

    assert_ne!(fresh, coid);
    assert_eq!(core.restore_counters.tag_overwrite_orphaned, 1, "the adopted order is orphaned");
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&fresh));
}

/// 17 — **A burst of 1,000 restored orders adopts in one pass**: every order lands in the registry,
/// every mount-side confirmation is delivered with its own tag, the note lists a bounded sample, and
/// nothing is left buffered on the engine. (Correctness only; the core-hop tail is the latency gate's.)
#[test]
fn a_thousand_restored_orders_adopt_in_one_pass() {
    const N: usize = 1_000;
    let dir = Scratch::reserved("vco-restore-burst");
    let (nb, events) = (cell(), Events::default());
    let mut core = assembled(open(config_on(&dir, vec![spy_mount(&nb, &events)])));
    for i in 0..N {
        let order = OwnedOrder { tag: Some(format!("t{i}")), ..restored_q(&format!("r{i:04}")) };
        core.coid_mount.insert(order.coid.clone(), 0);
        core.restore_owned_order(0, order);
    }
    let reports: Vec<_> = (0..N).map(|i| report(&format!("r{i:04}"), "ACCEPTED")).collect();

    run(&mut core, reports, Vec::new());

    assert_eq!(core.engine.registry.len(), N);
    assert_eq!(core.restore_counters.restored_adopted, N as u64);
    assert!(core.restored_orders.is_empty());
    assert!(core.engine.order_events.is_empty(), "everything was delivered");
    let got = heard(&events);
    assert_eq!(got.len(), N);
    assert!(got.iter().all(|(kind, tag)| kind == "Accepted" && tag.is_some()), "every one tagged");
    assert_eq!(restore_notes(&core), 1);
    assert!(noted(&core, "992 more"), "the sample is bounded: {:?}", core.recent.back());
}

/// 18 — **The ownership file is untouched by an adoption**: the entry is still there for the next
/// boot until the order dies and is pruned the ordinary way.
#[test]
fn an_adoption_does_not_touch_the_ownership_file() {
    let dir = Scratch::reserved("vco-restore-file-kept");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());
    drop(core);
    let on_disk = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap();
    assert!(on_disk.contains(&coid) && !on_disk.contains("\"kind\":\"forget\""), "{on_disk}");
}

/// Drive the `q` mount's quote and return how many orders it put on the venue client. Unlike
/// `quote`, which expects the order to exist, a REFUSED submit leaves the count at zero.
fn try_quote(core: &mut Core, next: &NextQuote) -> usize {
    *next.lock().unwrap() = Some(1);
    let before = core.engine.client.submissions.len();
    core.drive_strategy_feed_status("sim", "BTCUSDT", vike_model::FeedStatus::Live);
    core.dispatch_applied_fills();
    core.engine.client.submissions.len() - before
}

/// How many recent-events lines say a restored order holds a tag.
fn held_notes(core: &Core) -> usize {
    core.recent.iter().filter(|l| l.contains("is held by a restored order")).count()
}

/// 19 — **A submit over a restored, unadjudicated tag is refused** and the mount hears it as a
/// tagged terminal `Denied`: the order never reaches the venue client, the restored order keeps its
/// tag and its collection row, nothing is orphaned or counted, and the second refusal adds an event
/// for the mount but no second note (a maker re-quotes every tick until the pass).
#[test]
fn a_quote_over_a_restored_unadjudicated_tag_is_denied_with_the_tag() {
    let dir = Scratch::reserved("vco-restore-tag-denied");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    assert_eq!(try_quote(&mut core, &nb), 0, "the submit never reaches the venue client");

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.starts_with("Denied"), "{got:?}");
    assert!(got[0].0.contains("still holds this tag"), "the reason rides the event: {got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "the Denied carries the tag: {got:?}");
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&coid), "the restored order keeps its tag");
    assert!(core.restored_orders.contains_key(&coid), "still unadjudicated");
    assert_eq!(core.restore_counters.tag_overwrite_orphaned, 0);
    assert_eq!(held_notes(&core), 1, "recent: {:?}", core.recent);

    assert_eq!(try_quote(&mut core, &nb), 0);
    assert_eq!(heard(&events).len(), 2, "a second refusal is a second event");
    assert_eq!(held_notes(&core), 1, "but not a second note: the ring is not flooded");
}

/// 20 — **ADOPT ends the refusal**: the pass spends the collection row, the mount hears the tagged
/// `Accepted` after its `Denied`, and the same quote is no longer refused. (A real maker re-prices
/// by tag from here; the spy just submits, which is the old orphaning path test 16 counts.)
#[test]
fn the_refusal_ends_when_the_pass_adopts_the_order() {
    let dir = Scratch::reserved("vco-restore-tag-denied-adopt");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0);
    events.lock().unwrap().clear();

    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());
    assert_eq!(heard(&events), vec![accepted_q()], "the maker is told the order lives");

    assert_eq!(try_quote(&mut core, &nb), 1, "no longer refused");
    assert_eq!(
        core.restore_counters.tag_overwrite_orphaned, 1,
        "the old, adopted order is orphaned"
    );
}

/// 21 — **GONE ends the refusal**: the mount hears the restore's `Canceled`, the tag is retired, and
/// the next quote is accepted with nothing orphaned.
#[test]
fn the_refusal_ends_when_the_pass_finds_the_order_gone() {
    let dir = Scratch::reserved("vco-restore-tag-denied-gone");
    let (_coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0);

    run(&mut core, Vec::new(), Vec::new());
    assert!(core.restored_orders.is_empty() && core.strategy_tags.is_empty());

    assert_eq!(try_quote(&mut core, &nb), 1, "a new quote is accepted");
    let fresh = core.engine.client.submissions.last().unwrap().client_order_id.clone();
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&fresh));
    assert_eq!(core.restore_counters.tag_overwrite_orphaned, 0);
}

/// 22 — **The refusal fails open.** A pass that never comes (reconcile off, a fetch that keeps
/// failing) must not silence the mount: past the window from the first refusal the submit goes
/// through, as it did before the check existed.
#[test]
fn the_refusal_fails_open_when_no_pass_comes() {
    let dir = Scratch::reserved("vco-restore-tag-denied-valve");
    let (_coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0, "refused first");

    core.engine.now_ms += 200_000;

    assert_eq!(try_quote(&mut core, &nb), 1, "the window has passed: the submit goes through");
}

/// 23 — **A venue the gate has not opened never refuses**: its restored entry waits for ever (no
/// pass judges it), so a refusal would silence the mount for ever. Nothing is denied and nothing is
/// noted; the submit behaves exactly as before.
#[test]
fn a_closed_venue_gate_never_refuses_a_quote() {
    let dir = Scratch::reserved("vco-restore-tag-denied-gate-off");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    assert!(core.restored_orders.contains_key(&coid), "precondition: restored at boot");

    assert_eq!(try_quote(&mut core, &nb), 1);

    assert!(heard(&events).is_empty(), "no Denied: {:?}", heard(&events));
    assert_eq!(held_notes(&core), 0);
}

// --- the wire-id translation (decision 0121): a venue whose reports name an order by a function of
// --- its coid (Hyperliquid's keccak cloid), not by the coid.

/// The wire id the fake client of the tests below derives for `coid`.
const WIRE: &str = "0xw-";

fn wire_of(coid: &str) -> String {
    format!("{WIRE}{coid}")
}

/// Boot 2 whose client translates: its reports name an order by `wire_of(coid)`.
fn restart_wire(dir: &Scratch, nb: &NextQuote, events: &Events) -> Core {
    let mut core = restart(dir, nb, events);
    core.engine.client.wire_prefix = Some(WIRE.to_string());
    core
}

/// A venue open-order row that carries ONLY the wire id, on the venue's own spelling of the symbol
/// (`@107`, a spot coin) rather than the unified one the ownership file remembers.
fn wire_report(coid: &str, status: &str) -> OrderStatusReport {
    OrderStatusReport {
        client_order_id: Some(wire_of(coid)),
        symbol: "@107".into(),
        ..report(coid, status)
    }
}

/// 19 — **A report that carries only the wire id is matched to the restored coid and ADOPTED**:
/// the registry holds the order under the REAL coid and the UNIFIED symbol (the spot coin of the
/// report is remapped from the ownership record), the mount hears the tagged `Accepted`, the client
/// is told `(coid, symbol)`, and the diff that follows raises no `UnknownOrder` (the wire spelling
/// is nowhere in the registry). An external order beside it keeps its own id and symbol untouched.
#[test]
fn a_report_carrying_only_the_wire_id_is_adopted_under_the_real_coid() {
    let dir = Scratch::reserved("vco-restore-wire-adopt");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);

    let external = OrderStatusReport {
        client_order_id: Some(wire_of("someone-elses")),
        symbol: "@999".into(),
        ..report("x", "ACCEPTED")
    };
    run(&mut core, vec![wire_report(&coid, "ACCEPTED"), external], Vec::new());

    let adopted = core.engine.registry.get(&coid).expect("adopted under the real coid");
    assert!(adopted.status.is_live());
    assert_eq!(
        adopted.request.symbol, "BTCUSDT",
        "the spot coin is remapped to the unified symbol"
    );
    assert!(
        !core.engine.registry.contains_key(&wire_of(&coid)),
        "the wire spelling never enters the registry"
    );
    assert!(
        !core.engine.registry.contains_key(&wire_of("someone-elses")),
        "an order the file does not know is untouched"
    );
    assert_eq!(heard(&events), vec![accepted_q()], "the mount's liveness confirmation, tagged");
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert_eq!(
        core.engine.client.adopted,
        vec![(coid.clone(), "BTCUSDT".to_string())],
        "the client is told the adopted (coid, unified symbol)"
    );
    let kinds = alert_kinds(&core);
    assert_eq!(kinds, vec![DivergenceKind::UnknownOrder], "only the external one: {kinds:?}");
    assert!(core.restored_orders.is_empty());
}

/// 20 — **Absent with a translating client is still GONE**: the wire map changes how a report is
/// READ, not what silence means.
#[test]
fn an_absent_wire_id_is_gone() {
    let dir = Scratch::reserved("vco-restore-wire-gone");
    let (_coid, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);

    run(&mut core, Vec::new(), Vec::new());

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.contains(RESTORE_GONE_REASON), "{got:?}");
    assert_eq!(core.restore_counters.restored_gone, 1);
    assert!(core.engine.client.adopted.is_empty(), "nothing adopted, nothing announced");
}

/// 21 — **A fill that carries the wire id is a fill of the real coid**: the restore counts the order
/// filled while down, and the existing `MissingFill` path books the fill under the REAL coid (the
/// held alert's proposed fill names it), so `mount_for_coid` can find the mount.
#[test]
fn a_fill_carrying_the_wire_id_becomes_a_fill_of_the_real_coid() {
    let dir = Scratch::reserved("vco-restore-wire-fill");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);
    let wire_fill = FillReport {
        client_order_id: Some(wire_of(&coid)),
        symbol: "@107".into(),
        ..fill_of(&coid)
    };

    run(&mut core, Vec::new(), vec![wire_fill]);

    assert_eq!(core.restore_counters.restored_filled_while_down, 1, "filled while down");
    assert_eq!(core.restore_counters.restored_gone, 0, "never gone: the fill is the evidence");
    let missing = core
        .recon_alerts
        .values()
        .find(|a| a.kind == DivergenceKind::MissingFill)
        .expect("the existing path holds the fill");
    let names: Vec<&str> = missing
        .proposed_events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![coid.as_str()],
        "the proposed fill names the real coid, not the wire id"
    );
}

/// 22 — **A client with the default `wire_id_for` (`None`) behaves exactly as today**: a report that
/// merely LOOKS wire-shaped is an external order, the restored coid it never named is gone, and no
/// symbol is rewritten.
#[test]
fn a_client_without_a_wire_id_reads_the_reports_as_it_always_did() {
    let dir = Scratch::reserved("vco-restore-wire-default");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert!(core.engine.client.wire_prefix.is_none(), "precondition: the default client");

    run(&mut core, vec![wire_report(&coid, "ACCEPTED")], Vec::new());

    assert!(core.engine.registry.is_empty(), "nothing is translated, so nothing is adopted");
    assert_eq!(core.restore_counters.restored_gone, 1, "the real coid is absent from the venue");
    assert_eq!(alert_kinds(&core), vec![DivergenceKind::UnknownOrder]);
    assert!(core.engine.client.adopted.is_empty());
}

/// 23 — **Every adoption is announced to the client, with or without a wire id**, once: a second
/// pass over the same reports announces nothing (the order is already in the registry).
#[test]
fn the_adopted_pairs_reach_the_client_once() {
    let dir = Scratch::reserved("vco-restore-adopt-announce");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());
    assert_eq!(core.engine.client.adopted, vec![(coid.clone(), "BTCUSDT".to_string())]);
    run(&mut core, vec![report(&coid, "ACCEPTED")], Vec::new());

    assert_eq!(core.engine.client.adopted.len(), 1, "a repeat pass announces nothing");
}

/// 24 — **A FILLED report under the wire id is filled while down**: the terminal row is rewritten
/// too, so the verdict names the real coid; it is neither adopted nor announced to the client.
#[test]
fn a_filled_report_under_the_wire_id_is_filled_while_down() {
    let dir = Scratch::reserved("vco-restore-wire-filled-status");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);

    run(&mut core, vec![wire_report(&coid, "FILLED")], Vec::new());

    assert_eq!(core.restore_counters.restored_filled_while_down, 1);
    assert_eq!(core.restore_counters.restored_gone, 0);
    assert!(core.engine.registry.is_empty(), "a filled order is not adopted");
    assert!(core.engine.client.adopted.is_empty());
}
