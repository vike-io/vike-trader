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
//! The venue gate (`CoreConfig::restore_venue_gate`) is opened by hand: the fixtures run on `sim`,
//! which is no roster venue and so no row of the declared table enables, so the default gate would
//! make every test below vacuous, and the one test that wants the default says so.

use super::order_owners_tests::{
    Events, NOW, NextQuote, TagSpy, assembled, binance_engine, cell, config_on, noted, q_key,
    quote, restored_q, spy_mount,
};
use super::runtime_mount_tests::{mount_command, spec, unmount_command};
use super::*;
use crate::order_owners::{ORDER_OWNERS_FILE, OrderOwnerEntry, OrderOwnerLog};
use crate::runtime::test_support::core_of;
use crate::scratch::Scratch;
use vike_execution::recon::{
    RESTORE_GONE_REASON, RESTORE_MIN_QUIET_SPAN_MS, RESTORE_QUIET_PASSES, ReconcileMode,
    ReconcilePolicy,
};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, RiskGate};
use vike_model::ReconcileDivergenceKind;
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{LiquiditySide, OrderAccepted, OrderCanceled, TradeId};
use vike_model::{FillReport, OrderStatus, OrderStatusReport};

type Core = CoreThread<RecordingClient>;

/// `config` with the venue gate open: the declared table answers `false` for every venue today.
pub(super) fn open(mut config: CoreConfig) -> CoreConfig {
    config.restore_venue_gate = |_| true;
    config
}

/// What the spy of `events` heard so far: `(Debug of the kind, tag)`.
pub(super) fn heard(events: &Events) -> Vec<(String, Option<String>)> {
    events.lock().unwrap().clone()
}

/// The `(kind, tag)` an `Accepted` carrying the restored `q` tag reads as.
fn accepted_q() -> (String, Option<String>) {
    ("Accepted".to_string(), Some("q".to_string()))
}

/// Boot 1: a `q` quote rests under mount `maker-b` on `sim`/`BTCUSDT`, then the process "dies".
pub(super) fn rest_one(dir: &Scratch) -> (String, NextQuote, Events) {
    let (nb, events) = (cell(), Events::default());
    let mut first = assembled(config_on(dir, vec![spy_mount(&nb, &events)]));
    let order_client_id = quote(&mut first, &nb, 1);
    drop(first);
    events.lock().unwrap().clear();
    (order_client_id, nb, events)
}

/// Boot 2 over the same file, gate open, same mount.
pub(super) fn restart(dir: &Scratch, nb: &NextQuote, events: &Events) -> Core {
    assembled(open(config_on(dir, vec![spy_mount(nb, events)])))
}

/// A venue open-order row for `order_client_id` on `venue`/`BTCUSDT`.
fn report_on(venue: &str, order_client_id: &str, status: OrderStatus) -> OrderStatusReport {
    OrderStatusReport {
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: format!("v-{order_client_id}").into(),
        order_client_id: Some(order_client_id.to_string()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status,
        timestamp: 5,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

pub(super) fn report(order_client_id: &str, status: OrderStatus) -> OrderStatusReport {
    report_on("sim", order_client_id, status)
}

/// A venue fill row echoing `order_client_id`, with a trade id local state has never folded.
pub(super) fn fill_of(order_client_id: &str) -> FillReport {
    FillReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        trade_id: TradeId::prefixed("t-", format_args!("{order_client_id}")),
        venue_order_id: format!("v-{order_client_id}").into(),
        order_client_id: Some(order_client_id.to_string()),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 5,
    }
}

/// One pass for `venue` (and `route_key`), `quarantine` so every divergence is HELD and visible.
pub(super) fn pass_for(
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
        policy: ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_execution::recon::ReconcileBalanceTolerance::default(),
        route_key: route_key.map(str::to_string),
        admitted_symbols: None,
        lists_trigger_orders: false,
    }
}

/// Fold one `sim` pass and deliver what it buffered, as `handle` does after the command.
pub(super) fn run(core: &mut Core, orders: Vec<OrderStatusReport>, fills: Vec<FillReport>) {
    core.reconcile_reports(pass_for("sim", None, orders, fills));
    core.dispatch_applied_fills();
}

/// [`run`] twice: a divergence is held only on its second pass in a row (decision 0043 as amended 2026-11-21).
pub(super) fn run_held(core: &mut Core, orders: Vec<OrderStatusReport>, fills: Vec<FillReport>) {
    run(core, orders.clone(), fills.clone());
    run(core, orders, fills);
}

/// How many silent passes turn a restored order's SILENCE into a proof (the audit's G2): the first
/// finds it absent and holds it UNPROVEN, each of the next [`RESTORE_QUIET_PASSES`] is a quiet pass
/// (these passes carry `since` 0, an unbounded fill window, which always counts), and the last of
/// them declares it gone.
pub(super) const PASSES_TO_PROOF: usize = RESTORE_QUIET_PASSES as usize + 1;

/// Run [`PASSES_TO_PROOF`] silent `sim` passes: a restored order the venue never lists ends gone.
pub(super) fn run_silence_until_gone(core: &mut Core) {
    for _ in 0..PASSES_TO_PROOF {
        run(core, Vec::new(), Vec::new());
    }
}

/// [`run`] for a pass whose client CLAIMS its open-order read lists trigger orders
/// (`ReconcileClient::lists_trigger_orders`, journaled as `ReconcileReports::lists_trigger_orders`).
pub(super) fn run_claiming_trigger_orders(
    core: &mut Core,
    orders: Vec<OrderStatusReport>,
    fills: Vec<FillReport>,
) {
    core.reconcile_reports(ReconcileReports {
        lists_trigger_orders: true,
        ..pass_for("sim", None, orders, fills)
    });
    core.dispatch_applied_fills();
}

/// The kinds of the held alerts.
fn alert_kinds(core: &Core) -> Vec<ReconcileDivergenceKind> {
    core.reconcile_alerts.values().map(|a| a.kind).collect()
}

/// How many recent-events lines are restore notes.
fn restore_notes(core: &Core) -> usize {
    core.recent.iter().filter(|l| l.starts_with("RESTORE ")).count()
}

/// The order client ids the ownership file would hand a NEW boot, after `core` has been dropped (which drains
/// and joins its writer).
fn file_owns(core: Core, dir: &Scratch) -> Vec<String> {
    drop(core);
    OrderOwnerLog::in_state_dir(dir)
        .start(NOW)
        .order_client_ids
        .into_iter()
        .map(|o| o.order_client_id)
        .collect()
}

/// 1 — **ADOPT.** The venue still reports the order: the registry holds it live, its mount hears
/// `Accepted` stamped with the RESTORED tag, the collection is spent, the counter and one note say
/// so, and the order raises no `UnknownOrder`.
#[test]
fn a_resting_order_is_adopted_and_its_mount_hears_accepted_with_its_tag() {
    let dir = Scratch::reserved("vco-restore-adopt");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert!(core.engine.registry.is_empty(), "precondition: the registry starts empty");

    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());

    assert!(core.engine.registry[&order_client_id].status.is_live(), "adopted live");
    assert_eq!(heard(&events), vec![accepted_q()], "the mount's liveness confirmation, tagged");
    assert!(core.restored_orders.is_empty(), "the collection is spent");
    assert_eq!(core.restore_counters.restored_orders_adopted, 1);
    assert_eq!(
        (
            core.restore_counters.restored_orders_gone,
            core.restore_counters.restored_orders_filled_while_down
        ),
        (0, 0)
    );
    assert_eq!(restore_notes(&core), 1, "recent: {:?}", core.recent);
    assert!(noted(&core, &order_client_id), "the note names the order client id");
    assert!(alert_kinds(&core).is_empty(), "adoption precedes the diff: {:?}", alert_kinds(&core));
    assert_eq!(
        core.strategy_tags.get(&q_key(0)),
        Some(&order_client_id),
        "the tag still names the order"
    );
}

/// 2 — **The headline.** After the adoption a REAL venue cancel folds against the adopted order and
/// reaches the mount's `on_order_event` stamped with the restored tag, and the tag entry is
/// retired: the delivery that restore exists for.
#[test]
fn a_real_cancel_after_the_adoption_reaches_the_mount_with_the_restored_tag() {
    let dir = Scratch::reserved("vco-restore-real-cancel");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());
    events.lock().unwrap().clear();

    core.bus.publish(
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.clone(),
            reason: String::new().into(),
            timestamp: 9,
            post_only_would_cross: false,
            refusal_id: None,
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
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run_held(
        &mut core,
        vec![
            report(&order_client_id, OrderStatus::Accepted),
            report("ext-1", OrderStatus::Accepted),
        ],
        Vec::new(),
    );

    assert!(core.engine.registry.contains_key(&order_client_id));
    assert!(!core.engine.registry.contains_key("ext-1"), "an external order is untouched");
    assert_eq!(alert_kinds(&core), vec![ReconcileDivergenceKind::UnknownOrder]);
    assert_eq!(heard(&events), vec![accepted_q()], "the external order reached no mount");
    assert_eq!(core.restore_counters.restored_orders_adopted, 1);
}

/// The venue's replay of one execution as a bridge emits it (`[Fill, wrap]`), folded.
fn replay_partial(core: &mut Core, order_client_id: &str, trade_id: &str, qty: f64) {
    let fill = vike_model::events::FillEvent {
        trade_id: TradeId::new(trade_id).unwrap(),
        ..super::runtime_mount_tests::order_client_id_fill(order_client_id, 1, qty, 100.0)
    };
    core.bus.publish(Event::Fill(fill.clone()), &mut core.engine);
    let wrap = vike_model::events::OrderPartiallyFilled {
        order_client_id: order_client_id.to_string(),
        fill,
        timestamp: 6,
    };
    core.bus.publish(Event::OrderPartiallyFilled(wrap), &mut core.engine);
    core.dispatch_applied_fills();
}

/// **A fill the adoption's report already holds is not counted twice** (an IBKR order partly filled while the node was
/// down, its execution replayed by the next rebuilt connection under an id the engine never saw). `filled_qty` stays
/// the venue's, the position books the execution once, and a genuinely new fill still counts.
#[test]
fn a_partial_fill_inside_the_adopted_report_is_not_counted_again_when_it_is_replayed() {
    let dir = Scratch::reserved("vco-restore-partial-replay");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    let row = OrderStatusReport {
        filled_qty: 0.4,
        average_price: 100.0,
        ..report(&order_client_id, OrderStatus::PartiallyFilled)
    };
    let fill = FillReport { last_qty: 0.4, ..fill_of(&order_client_id) };
    let replayed = fill.trade_id.to_string();

    run(&mut core, vec![row], vec![fill]);
    assert_eq!(core.engine.registry[&order_client_id].filled_qty, 0.4, "seeded by the report");

    replay_partial(&mut core, &order_client_id, &replayed, 0.4);
    let order = &core.engine.registry[&order_client_id];
    assert_eq!(order.filled_qty, 0.4, "the replay is inside the report");
    assert_eq!(order.average_fill_price, 100.0);
    let position = vike_model::events::PositionSide::Both;
    assert_eq!(core.engine.position_size(position), 0.4, "the position books it once");

    replay_partial(&mut core, &order_client_id, "t-new", 0.1);
    assert!(
        (core.engine.registry[&order_client_id].filled_qty - 0.5).abs() < 1e-12,
        "a new fill counts"
    );
    assert!((core.engine.position_size(position) - 0.5).abs() < 1e-12);
}

/// **A report older than its fill rows holds none of them**: its execution is counted when it is replayed, not lost.
#[test]
fn a_fill_row_beyond_the_adopted_report_is_counted_when_it_arrives() {
    let dir = Scratch::reserved("vco-restore-partial-lag");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    let fill = FillReport { last_qty: 0.4, ..fill_of(&order_client_id) };
    let replayed = fill.trade_id.to_string();

    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], vec![fill]);
    replay_partial(&mut core, &order_client_id, &replayed, 0.4);

    assert_eq!(core.engine.registry[&order_client_id].filled_qty, 0.4);
}

/// Boot 1 of a STOP: a reduce-only `Stop` rests under mount `maker-b` on `sim`/`BTCUSDT` under the
/// tag `q` (minted through the one writer, `own_order_client_id`), then the process "dies".
pub(super) fn rest_one_stop(dir: &Scratch) -> (String, NextQuote, Events) {
    rest_stop(dir, None)
}

/// [`rest_one_stop`] that, given `ack`, lets the venue ACKNOWLEDGE the stop with that venue order id
/// (the accept the engine folds) before the process "dies"; without it the stop is simply resting.
fn rest_stop(dir: &Scratch, ack: Option<&str>) -> (String, NextQuote, Events) {
    let (nb, events) = (cell(), Events::default());
    let mut first = assembled(config_on(dir, vec![spy_mount(&nb, &events)]));
    let order_client_id = "stop-q".to_string();
    let mut resting = vike_model::OrderRecord::new(vike_model::OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(90.0),
        reduce_only: true,
        ..Default::default()
    });
    if ack.is_none() {
        resting.status = OrderStatus::Accepted;
    }
    first.engine.registry.insert(order_client_id.clone(), resting);
    first.own_order_client_id(order_client_id.clone(), 0, Some("q"));
    if let Some(venue_order_id) = ack {
        first.bus.publish(
            Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.clone(),
                venue_order_id: Some(venue_order_id.into()),
                timestamp: 2,
            }),
            &mut first.engine,
        );
        first.dispatch_applied_fills();
    }
    drop(first);
    events.lock().unwrap().clear();
    (order_client_id, nb, events)
}

/// 3b — **A stop the report does not list is UNPROVEN, not gone, when the client lists no trigger
/// orders** (`ReconcileClient::lists_trigger_orders`, default `false`): the venue may keep it where the
/// client does not look (okx's algo list). No mount is told, the tag and the collection row stay,
/// the file keeps the entry, ONE note says so, and a repeat pass is silent.
#[test]
fn an_absent_stop_is_held_unproven_not_gone_when_the_client_lists_no_trigger_orders() {
    let dir = Scratch::reserved("vco-restore-unproven-stop");
    let (order_client_id, nb, events) = rest_one_stop(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, Vec::new(), Vec::new());

    assert!(heard(&events).is_empty(), "no mount is told: {:?}", heard(&events));
    assert!(
        core.restored_orders.contains_key(&order_client_id),
        "the entry stays for a later pass"
    );
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&order_client_id), "the tag is untouched");
    assert_eq!(core.restore_counters, RestoreCounters::default(), "nothing counted");
    assert!(noted(&core, "UNPROVEN 1 order(s)"), "the note: {:?}", core.recent);
    assert_eq!(restore_notes(&core), 1);

    run(&mut core, Vec::new(), Vec::new());
    assert_eq!(restore_notes(&core), 1, "once per order, not once per pass: {:?}", core.recent);
    assert!(file_owns(core, &dir).contains(&order_client_id), "the ownership file keeps the entry");
}

/// 3c — **…and a client that DOES list trigger orders speaks for it**: the same silence, on a pass
/// carrying the claim and after the quiet passes, is gone — the mount hears the restore reason with
/// its tag and the file forgets the entry — and the entry leaves the unproven edge set. Without the
/// claim the quiet passes alone never get there (the stop stays held however long the silence).
#[test]
fn an_absent_stop_is_gone_when_the_client_lists_trigger_orders() {
    let dir = Scratch::reserved("vco-restore-claimed-stop");
    let (order_client_id, nb, events) = rest_one_stop(&dir);
    let mut core = restart(&dir, &nb, &events);
    run_silence_until_gone(&mut core); // no claim: every quiet pass is counted, none proves a stop
    assert!(core.restore_unproven.contains_key(&order_client_id), "precondition: held first");
    assert!(heard(&events).is_empty() && core.restored_orders.contains_key(&order_client_id));

    run_claiming_trigger_orders(&mut core, Vec::new(), Vec::new());

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.contains(RESTORE_GONE_REASON), "{got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "{got:?}");
    assert!(core.restored_orders.is_empty() && core.restore_unproven.is_empty());
    assert_eq!(core.restore_counters.restored_orders_gone, 1);
    assert!(!file_owns(core, &dir).contains(&order_client_id), "the file forgets it");
}

/// 3d — **A LISTED stop is adopted whatever the client claims**: the claim governs silence only.
#[test]
fn a_listed_stop_is_adopted_with_or_without_the_claim() {
    for claim in [false, true] {
        let dir = Scratch::reserved("vco-restore-listed-stop");
        let (order_client_id, nb, events) = rest_one_stop(&dir);
        let mut core = restart(&dir, &nb, &events);
        let row = OrderStatusReport {
            side: -1,
            order_type: vike_model::OrderType::Stop,
            ..report(&order_client_id, OrderStatus::Accepted)
        };
        if claim {
            run_claiming_trigger_orders(&mut core, vec![row], Vec::new());
        } else {
            run(&mut core, vec![row], Vec::new());
        }
        let adopted =
            core.engine.registry.get(&order_client_id).unwrap_or_else(|| panic!("claim {claim}"));
        assert!(adopted.request.reduce_only, "claim {claim}: terms from the file");
        assert_eq!(adopted.request.trigger_price, Some(90.0), "claim {claim}");
        assert_eq!(core.restore_counters.restored_orders_adopted, 1, "claim {claim}");
    }
}

/// 4 — **GONE needs proof; silence alone makes an order UNPROVEN** (the audit's G2). One pass that
/// does not list the order proves nothing: the mount hears no "cancelled", the tag, the collection
/// row and the file entry stay, nothing is counted gone, and one note says why. Only the quiet
/// passes ([`RESTORE_QUIET_PASSES`]) make it gone: then the mount hears a synthetic `Canceled` with
/// the restore reason, stamped with the restored tag, and the tag, the collection row and the FILE
/// entry are gone.
#[test]
fn an_absent_order_is_unproven_then_gone_the_mount_hears_the_reason_and_the_file_forgets_it() {
    let dir = Scratch::reserved("vco-restore-gone");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, Vec::new(), Vec::new());

    assert!(heard(&events).is_empty(), "silence alone cancels nothing: {:?}", heard(&events));
    assert!(core.restored_orders.contains_key(&order_client_id), "kept");
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&order_client_id), "the tag stays");
    assert_eq!(core.restore_counters.restored_orders_gone, 0);
    assert!(noted(&core, "UNPROVEN 1 order(s)"), "recent: {:?}", core.recent);
    for _ in 1..PASSES_TO_PROOF - 1 {
        run(&mut core, Vec::new(), Vec::new());
        assert!(heard(&events).is_empty() && core.restored_orders.contains_key(&order_client_id));
    }

    run(&mut core, Vec::new(), Vec::new());

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.starts_with("Canceled"), "{got:?}");
    assert!(got[0].0.contains(RESTORE_GONE_REASON), "the reason rides the event: {got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "stamped with its restored tag: {got:?}");
    assert!(core.strategy_tags.is_empty(), "the tag entry is dropped");
    assert!(core.restored_orders.is_empty() && core.restore_unproven.is_empty());
    assert!(core.engine.registry.is_empty(), "a gone order is never put in the registry");
    assert_eq!(core.restore_counters.restored_orders_gone, 1);
    assert_eq!(restore_notes(&core), 2, "the hold and the verdict: {:?}", core.recent);
    assert!(!file_owns(core, &dir).contains(&order_client_id), "the ownership entry is forgotten");
}

/// 4b — **The quiet passes are counted only past the lookback, and never on a lookback shorter than
/// [`RESTORE_MIN_QUIET_SPAN_MS`].** A pass whose fill window opened BEFORE the first silent pass
/// could still be missing a fill from the downtime, so it is not quiet: only passes with `since` at
/// or after that first pass count. And a window that reaches back one millisecond is read as ten
/// minutes: passes seconds after the first silence count nothing, however narrow their window, until
/// that span has gone by. (The core clock is stepped by hand below.)
#[test]
fn only_passes_whose_fill_window_opened_after_the_first_silence_count_as_quiet() {
    let dir = Scratch::reserved("vco-restore-quiet-lookback");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    let now = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(NOW));
    let stepping = now.clone();
    core.config.clock = Box::new(move || stepping.load(std::sync::atomic::Ordering::Relaxed));
    // One silent pass at `at` (ms after NOW) whose fill window reaches `lookback` ms back.
    let silent = |core: &mut Core, at: i64, lookback: i64| {
        now.store(NOW + at, std::sync::atomic::Ordering::Relaxed);
        core.reconcile_reports(ReconcileReports {
            since: NOW + at - lookback,
            ..pass_for("sim", None, Vec::new(), Vec::new())
        });
        core.dispatch_applied_fills();
    };
    let span = RESTORE_MIN_QUIET_SPAN_MS;

    silent(&mut core, 0, 3_600_000); // the first silence: UNPROVEN, quiet 0
    for step in 1..=PASSES_TO_PROOF as i64 + 2 {
        // the window opens after the first silence, but only a millisecond wide and seconds on
        silent(&mut core, 1_000 * step, 1);
    }
    assert!(
        core.restored_orders.contains_key(&order_client_id),
        "a floor of ten minutes, not a millisecond"
    );
    assert!(heard(&events).is_empty(), "{:?}", heard(&events));
    assert_eq!(core.restore_unproven[&order_client_id].quiet, 0, "nothing counted inside the span");

    for step in 1..=PASSES_TO_PROOF as i64 + 2 {
        // the span has gone by, but the window still reaches back before the first silence
        silent(&mut core, span + step, span + step + 1);
    }
    assert!(core.restored_orders.contains_key(&order_client_id), "uncounted passes never prove it");
    assert!(heard(&events).is_empty(), "{:?}", heard(&events));
    assert_eq!(core.restore_unproven[&order_client_id].quiet, 0);

    for counted in 1..=RESTORE_QUIET_PASSES as i64 {
        assert!(
            core.restored_orders.contains_key(&order_client_id),
            "{counted} quiet passes so far"
        );
        // ten minutes on: the window opens at the first silence (and a millisecond wide is enough)
        silent(&mut core, span + 100 * counted, 1);
    }
    assert!(core.restored_orders.is_empty(), "the counted passes made it gone");
    assert_eq!(heard(&events).len(), 1, "{:?}", heard(&events));
}

/// 5 — **FILLED WHILE DOWN** is neither adopted nor gone. The fill report in the pass window takes
/// the existing `MissingFill` path (held here), the mount hears nothing from the restore, the file
/// keeps its entry, and the planner stops seeing the order client id so a later pass cannot count it twice.
#[test]
fn a_fill_in_the_pass_window_is_counted_and_left_to_the_missing_fill_path() {
    let dir = Scratch::reserved("vco-restore-filled");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, Vec::new(), vec![fill_of(&order_client_id)]);

    assert!(heard(&events).is_empty(), "the restore tells the mount nothing: {:?}", heard(&events));
    assert!(core.engine.registry.is_empty(), "not adopted");
    assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1);
    assert_eq!(
        (core.restore_counters.restored_orders_adopted, core.restore_counters.restored_orders_gone),
        (0, 0)
    );
    assert!(core.restored_orders.is_empty(), "off the planner's set, so it is counted once");
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&order_client_id), "the tag is untouched");
    assert!(
        alert_kinds(&core).contains(&ReconcileDivergenceKind::MissingFill),
        "the existing path owns the fill: {:?}",
        alert_kinds(&core)
    );
    assert!(file_owns(core, &dir).contains(&order_client_id), "the file keeps the entry");
}

/// Boot 1 that also lets the venue ACKNOWLEDGE the quote with `venue_order_id` (the accept the
/// engine folds, carrying the venue's own id), so the ownership line learns it; then the process
/// "dies".
pub(super) fn rest_one_acknowledged(
    dir: &Scratch,
    venue_order_id: &str,
) -> (String, NextQuote, Events) {
    let (nb, events) = (cell(), Events::default());
    let mut first = assembled(config_on(dir, vec![spy_mount(&nb, &events)]));
    let order_client_id = quote(&mut first, &nb, 1);
    first.bus.publish(
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.clone(),
            venue_order_id: Some(venue_order_id.into()),
            timestamp: 2,
        }),
        &mut first.engine,
    );
    first.dispatch_applied_fills();
    drop(first);
    events.lock().unwrap().clear();
    (order_client_id, nb, events)
}

/// A venue fill row of the order the venue knows as `venue_order_id`, carrying NO order client id: what
/// binance, aster, cTrader, IG and Alpaca report, and what a triggered stop's child looks like.
fn order_client_id_less_fill(venue_order_id: &str) -> FillReport {
    FillReport { order_client_id: None, venue_order_id: venue_order_id.into(), ..fill_of("unused") }
}

/// 5b — **The venue order id the venue acknowledged rides the ownership line** (the audit's G2): the
/// engine folds the accept, the core writes the id on the entry's line (a record of its own, because
/// the `own` line is written at the mint, before any acknowledgement exists), and the next boot's
/// restored entry carries it.
#[test]
fn the_acknowledged_venue_order_id_rides_the_ownership_line() {
    let dir = Scratch::reserved("vco-restore-venue-id");
    let (order_client_id, nb, events) = rest_one_acknowledged(&dir, "7001");
    let core = restart(&dir, &nb, &events);
    let entry = core.restored_orders.get(&order_client_id).expect("restored");
    assert_eq!(entry.venue_order_id.as_deref(), Some("7001"), "{entry:?}");
}

/// 5c — **A fill that carries no order client id joins its order by the acknowledged venue id**: the order is
/// FILLED WHILE DOWN (counted once, off the planner's set, the existing `MissingFill` path owns the
/// fill) and its mount is NOT told "cancelled"; without the id on the line the same fill is a
/// stranger and the order stays merely unproven.
#[test]
fn an_order_client_id_less_fill_of_the_acknowledged_venue_id_is_filled_while_down_not_gone() {
    let dir = Scratch::reserved("vco-restore-venue-id-fill");
    let (order_client_id, nb, events) = rest_one_acknowledged(&dir, "7001");
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, Vec::new(), vec![order_client_id_less_fill("7001")]);

    assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1);
    assert_eq!(core.restore_counters.restored_orders_gone, 0);
    assert!(heard(&events).is_empty(), "no false cancel: {:?}", heard(&events));
    assert!(
        !core.restored_orders.contains_key(&order_client_id),
        "spent, so a later pass cannot recount it"
    );
    assert!(core.restore_unproven.is_empty());

    // the control: the same fill against an order whose acknowledgement was never learned
    let dir = Scratch::reserved("vco-restore-venue-id-fill-control");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, Vec::new(), vec![order_client_id_less_fill("7001")]);
    assert_eq!(core.restore_counters.restored_orders_filled_while_down, 0, "a stranger's fill");
    assert!(core.restore_unproven.contains_key(&order_client_id), "the order is merely unproven");
}

/// 5c2 — **A restart of BOTH daemons on a venue whose rows carry no order client id of ours** (fxcm's Orders table names an order
/// by a numeric order id, IG's working orders by a `dealId`): the open-order row lists the resting order with no order client id.
/// The pass names it by the venue order id the ownership line learned, so it is ADOPTED under our order client id, the mount hears
/// its liveness, the diff raises nothing for it, and the client is taught the order WITH that id (what lets the bridge
/// name it from then on). A row whose venue id is nobody's stays FOREIGN: an `UnknownOrder`, and our order unproven.
#[test]
fn an_order_client_id_less_row_of_the_acknowledged_venue_id_is_adopted_and_taught_and_a_strangers_stays_foreign()
 {
    let order_client_id_less = |order_client_id: &str, venue_id: &str| OrderStatusReport {
        order_client_id: None,
        venue_order_id: venue_id.into(),
        ..report(order_client_id, OrderStatus::Accepted)
    };
    for (shape, venue_id) in [("fxcm", "416262101"), ("ig", "DIAAA9")] {
        let dir = Scratch::reserved(&format!("vco-restore-order-client-id-less-{shape}"));
        let (order_client_id, nb, events) = rest_one_acknowledged(&dir, venue_id);
        let mut core = restart(&dir, &nb, &events);
        run(&mut core, vec![order_client_id_less(&order_client_id, venue_id)], Vec::new());

        assert!(
            core.engine.registry.contains_key(&order_client_id),
            "{shape}: adopted under our order client id"
        );
        assert_eq!(core.restore_counters.restored_orders_adopted, 1, "{shape}");
        assert_eq!(
            heard(&events),
            vec![accepted_q()],
            "{shape}: the mount's liveness confirmation"
        );
        assert_eq!(
            core.engine.client.adopted,
            vec![vike_execution::RestoredOrder {
                venue_order_id: Some(venue_id.to_string()),
                ..vike_execution::RestoredOrder::new(order_client_id.clone(), "BTCUSDT")
            }],
            "{shape}: the client is taught the order with its venue id"
        );
        assert!(
            alert_kinds(&core).is_empty(),
            "{shape}: the row is ours: {:?}",
            alert_kinds(&core)
        );

        // The control: a row of an id nobody acknowledged is a stranger's.
        let dir = Scratch::reserved(&format!("vco-restore-order-client-id-less-{shape}-stranger"));
        let (order_client_id, nb, events) = rest_one_acknowledged(&dir, venue_id);
        let mut core = restart(&dir, &nb, &events);
        run_held(&mut core, vec![order_client_id_less(&order_client_id, "not-ours-1")], Vec::new());
        assert!(!core.engine.registry.contains_key(&order_client_id), "{shape}: nothing adopted");
        assert!(core.engine.client.adopted.is_empty(), "{shape}: nothing taught");
        assert!(
            core.restore_unproven.contains_key(&order_client_id),
            "{shape}: ours is merely unproven"
        );
        assert!(
            alert_kinds(&core).contains(&ReconcileDivergenceKind::UnknownOrder),
            "{shape}: the stranger's row: {:?}",
            alert_kinds(&core)
        );
    }
}

/// 5d — **A trigger kind records no venue id**: the id a venue acknowledges a stop with is its
/// trigger-order id (binance's `algoId`), not the id the stop's fill carries (the child order's), so
/// a fill joined by it could be an unrelated order's. The same acknowledgement of a plain limit (5b)
/// does ride the line.
#[test]
fn a_stops_acknowledged_id_is_not_written_to_its_ownership_line() {
    let dir = Scratch::reserved("vco-restore-stop-no-venue-id");
    let (order_client_id, nb, events) = rest_stop(&dir, Some("9001"));
    let core = restart(&dir, &nb, &events);
    let entry = core.restored_orders.get(&order_client_id).expect("the stop is restored");
    assert_eq!(entry.order_type, Some(vike_model::OrderType::Stop), "precondition: {entry:?}");
    assert_eq!(entry.venue_order_id, None, "no id learned for a trigger kind: {entry:?}");
}

/// 5e — **An entry that leaves the restored collection leaves the unproven state with it.** The
/// prune of a terminal order's attribution (past the linger) and an explicit unmount both drop the
/// row from `restored_orders`/`pending_owners`; `restore_unproven` must follow or it leaks an entry
/// per such order for the life of the process.
#[test]
fn an_entry_pruned_or_unmounted_leaves_the_unproven_state() {
    let dir = Scratch::reserved("vco-restore-unproven-pruned");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, Vec::new(), Vec::new());
    assert!(core.restore_unproven.contains_key(&order_client_id), "precondition: unproven");
    core.order_client_id_terminal.push_back((order_client_id.clone(), 0));
    core.engine.now_ms = ORDER_CLIENT_ID_PRUNE_LINGER_MS;
    core.dispatch_applied_fills();
    assert!(!core.restored_orders.contains_key(&order_client_id), "pruned past the linger");
    assert!(
        core.restore_unproven.is_empty(),
        "and out of the unproven set: {:?}",
        core.restore_unproven
    );

    let dir = Scratch::reserved("vco-restore-unproven-unmounted");
    let (next, events) = (cell(), Events::default());
    let order_client_id = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut core = restart_without_the_mount(&dir, &next, &events);
    run(&mut core, Vec::new(), Vec::new());
    assert!(
        core.restore_unproven.contains_key(&order_client_id),
        "precondition: a pending entry is unproven"
    );
    core.dispatch(mount_command(spec("rt-b")));
    core.dispatch(unmount_command("rt-b"));
    assert!(core.restore_unproven.is_empty(), "unmounted: {:?}", core.restore_unproven);
}

/// 6 — **A second pass changes nothing**, for each verdict: no second delivery, no recount, no
/// second note, no second registry write.
#[test]
fn a_second_pass_is_a_no_op_for_every_verdict() {
    for verdict in ["adopt", "gone", "filled", "unproven"] {
        let dir = Scratch::reserved("vco-restore-idem");
        let (order_client_id, nb, events) = rest_one(&dir);
        let mut core = restart(&dir, &nb, &events);
        let (orders, fills) = match verdict {
            "adopt" => (vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new()),
            // a terminal report is proof at once; silence ("unproven") needs the quiet passes
            "gone" => (vec![report(&order_client_id, OrderStatus::Canceled)], Vec::new()),
            "unproven" => (Vec::new(), Vec::new()),
            _ => (Vec::new(), vec![fill_of(&order_client_id)]),
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

/// 7 — **The gate off is inert**: with the DEFAULT gate (`sim` is no roster venue, so no row of the
/// table enables it) the entry waits, nothing is delivered or counted, and the venue order raises its
/// `UnknownOrder` exactly as before the feature. The default gate IS the declared table.
#[test]
fn a_venue_the_table_does_not_enable_restores_nothing() {
    let dir = Scratch::reserved("vco-restore-gate-off");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    assert!(core.restored_orders.contains_key(&order_client_id), "precondition: restored at boot");

    run_held(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());

    assert!(core.engine.registry.is_empty());
    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert!(
        core.restored_orders.contains_key(&order_client_id),
        "the entry waits for a later pass"
    );
    assert_eq!(restore_notes(&core), 0);
    assert_eq!(alert_kinds(&core), vec![ReconcileDivergenceKind::UnknownOrder]);

    let default_gate = CoreConfig::default().restore_venue_gate;
    for &v in vike_model::VENUES {
        assert_eq!(
            default_gate(v),
            vike_venues::venue_restore::restores_orders(v),
            "{v}: the default gate is the declared table"
        );
    }
}

/// 8 — **`restore_orders_off` is inert** even when the collection holds entries and the gate is open.
#[test]
fn the_operator_off_switch_stops_the_hook() {
    let dir = Scratch::reserved("vco-restore-off");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    core.config.restore_orders_off = true;

    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());

    assert!(core.engine.registry.is_empty());
    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert!(core.restored_orders.contains_key(&order_client_id));
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

    run_held(&mut core, vec![report("ext-1", OrderStatus::Accepted)], Vec::new());

    assert!(core.engine.registry.is_empty());
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert_eq!(restore_notes(&core), 0);
    assert_eq!(alert_kinds(&core), vec![ReconcileDivergenceKind::UnknownOrder]);
}

/// 10 — **Entries outside the pass's scope stay for a later pass**: an entry on a symbol the engine
/// does not serve, and an old line with no venue or symbol, are not declared gone by a pass that
/// says nothing about them; the covered entry beside them is.
#[test]
fn entries_the_pass_cannot_speak_for_stay() {
    let dir = Scratch::reserved("vco-restore-scope");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    let far = OrderOwnerEntry { symbol: Some("XYZUSDT".into()), tag: None, ..restored_q("far-1") };
    let old = OrderOwnerEntry {
        order_client_id: "old-1".into(),
        mount_id: "maker_b".into(),
        ..OrderOwnerEntry::default()
    };
    for o in [far, old] {
        core.order_client_id_mount.insert(o.order_client_id.clone(), 0);
        core.restore_owned_order(0, o);
    }

    run_silence_until_gone(&mut core);

    let left: Vec<&str> = core.restored_orders.keys().map(String::as_str).collect();
    assert_eq!(left, vec!["far-1", "old-1"], "only the covered entry was judged");
    assert_eq!(core.restore_counters.restored_orders_gone, 1);
    let got = heard(&events);
    assert_eq!(got.len(), 1, "one Canceled, for {order_client_id}: {got:?}");
}

/// A core with two accounts of `binance`: the default account (engine 0, mount `m-default`) and the
/// labelled `ALT` (engine 1, mount `m-alt`), each mount resting a `q` quote. Returns each mount's
/// events, the order client ids `(default, alt)`, and the boot-2 core over the same file.
fn two_accounts(dir: &Scratch) -> (Events, Events, (String, String), Core) {
    let (nd, na) = (cell(), cell());
    let (ed, ea) = (Events::default(), Events::default());
    let mount = |cid: &str, account: AccountLabel, next: &NextQuote, event: &Events| {
        let mut m = spy_mount(next, event);
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
    let default_order_client_id =
        first.engine.client.submissions.last().expect("default quoted").order_client_id.clone();
    let alt_order_client_id = first.extra_engines[0]
        .1
        .client
        .submissions
        .last()
        .expect("ALT quoted")
        .order_client_id
        .clone();
    drop(first);
    (ed, ea, (default_order_client_id, alt_order_client_id), build(true))
}

/// 11 — **The scope's account matches how the order's account was written, on both sides.** The
/// default account's entry carries no account and a labelled account's carries its label; a pass for
/// ONE account judges only its own entries (a silent `ALT` pass leaves the default entry alone), and
/// each adoption lands on its own engine.
#[test]
fn the_default_account_and_a_labelled_account_each_see_only_their_own_orders() {
    let dir = Scratch::reserved("vco-restore-accounts");
    let (ed, ea, (default_order_client_id, alt_order_client_id), mut core) = two_accounts(&dir);
    assert_eq!(core.restored_orders[&default_order_client_id].account, None);
    assert_eq!(core.restored_orders[&alt_order_client_id].account.as_deref(), Some("ALT"));

    // Silent ALT passes: ALT's order is gone; the default account's entry is not its business.
    for _ in 0..PASSES_TO_PROOF {
        core.reconcile_reports(pass_for("binance", Some("binance#ALT"), Vec::new(), Vec::new()));
        core.dispatch_applied_fills();
    }
    assert_eq!(heard(&ea).len(), 1, "ALT's mount hears its gone order: {:?}", heard(&ea));
    assert!(heard(&ed).is_empty(), "the default mount hears nothing from ALT's pass");
    assert!(core.restored_orders.contains_key(&default_order_client_id), "the default entry stays");
    assert!(!core.restored_orders.contains_key(&alt_order_client_id));

    // The default pass lists the default order: adopted onto engine 0 and nowhere else.
    core.reconcile_reports(pass_for(
        "binance",
        Some("binance"),
        vec![report_on("binance", &default_order_client_id, OrderStatus::Accepted)],
        Vec::new(),
    ));
    core.dispatch_applied_fills();
    assert!(core.engine.registry.contains_key(&default_order_client_id), "on the default engine");
    assert!(core.extra_engines[0].1.registry.is_empty(), "not on ALT's");
    assert_eq!(heard(&ed), vec![accepted_q()]);
    assert_eq!(core.restore_counters.restored_orders_adopted, 1);
    assert_eq!(core.restore_counters.restored_orders_gone, 1);
}

/// 12 — **A labelled account's adoption lands on its engine and keeps its engine row.** The ALT pass
/// lists ALT's order: engine 1's registry holds it, `order_client_id_venue` routes its events there, and a real
/// cancel folds into engine 1 and reaches ALT's mount tagged.
#[test]
fn an_adopted_order_of_a_labelled_account_lands_on_that_accounts_engine() {
    let dir = Scratch::reserved("vco-restore-alt-engine");
    let (_ed, ea, (default_order_client_id, alt_order_client_id), mut core) = two_accounts(&dir);

    core.reconcile_reports(pass_for(
        "binance",
        Some("binance#ALT"),
        vec![report_on("binance", &alt_order_client_id, OrderStatus::Accepted)],
        Vec::new(),
    ));
    core.dispatch_applied_fills();

    assert!(core.extra_engines[0].1.registry[&alt_order_client_id].status.is_live());
    assert!(core.engine.registry.is_empty(), "not on the default engine");
    assert_eq!(
        core.order_client_id_venue.get(&alt_order_client_id),
        Some(&1),
        "its events route to engine 1"
    );
    assert_eq!(heard(&ea), vec![accepted_q()]);
    assert!(core.restored_orders.contains_key(&default_order_client_id), "the default entry waits");

    let engine1 = &mut core.extra_engines[0].1;
    core.bus.publish(
        Event::OrderCanceled(OrderCanceled {
            order_client_id: alt_order_client_id.clone(),
            reason: String::new().into(),
            timestamp: 9,
            post_only_would_cross: false,
            refusal_id: None,
        }),
        engine1,
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
        Ok((Box::new(TagSpy { next: Arc::clone(&next), events: Arc::clone(&events) }), None))
    })
}

/// A boot-1 core with ONE runtime mount `rt-b` that rests a `q` order, then dies; returns the order client id.
fn rest_one_on_a_runtime_mount(dir: &Scratch, next: &NextQuote, events: &Events) -> String {
    let mut config = config_on(dir, Vec::new());
    config.strategy_factory = Some(spy_factory(next, events));
    let mut first = assembled(config);
    first.dispatch(mount_command(spec("rt-b")));
    let order_client_id = quote(&mut first, next, 1);
    drop(first);
    events.lock().unwrap().clear();
    order_client_id
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
    let order_client_id = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut core = restart_without_the_mount(&dir, &next, &events);
    assert!(core.pending_owners.contains_key("rt_b"), "precondition: waiting for its mount");

    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());
    assert!(
        core.engine.registry[&order_client_id].status.is_live(),
        "adopted though nobody owns it yet"
    );
    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters.restored_orders_adopted, 1);
    assert!(
        core.pending_owners["rt_b"]
            .order_client_ids
            .iter()
            .any(|o| o.order_client_id == order_client_id),
        "still pending"
    );

    core.dispatch(mount_command(spec("rt-b")));
    assert!(core.restored_orders.contains_key(&order_client_id), "bound with its mount");
    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());

    assert_eq!(heard(&events), vec![accepted_q()], "the new mount is told it is alive");
    assert_eq!(core.restore_counters.restored_orders_adopted, 1, "no second adoption");
    assert!(core.restored_orders.is_empty());
}

/// 14 — **A mount that is not mounted: GONE skips the delivery.** The pass forgets the entry in the
/// file and says so in a note; no mount is told anything, and a mount of that id landing later
/// binds nothing.
#[test]
fn a_gone_order_of_an_unmounted_mount_is_forgotten_without_telling_anyone() {
    let dir = Scratch::reserved("vco-restore-pending-gone");
    let (next, events) = (cell(), Events::default());
    let order_client_id = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut core = restart_without_the_mount(&dir, &next, &events);

    run_silence_until_gone(&mut core);

    assert!(heard(&events).is_empty());
    assert_eq!(core.restore_counters.restored_orders_gone, 1);
    assert!(!core.pending_owners.contains_key("rt_b"), "nothing left to wait for");
    assert!(noted(&core, "of unmounted mounts, no mount told"), "recent: {:?}", core.recent);

    core.dispatch(mount_command(spec("rt-b")));
    assert!(core.strategy_tags.is_empty() && core.restored_orders.is_empty(), "binds nothing");
    assert!(!file_owns(core, &dir).contains(&order_client_id), "and the file forgot it");
}

/// 15 — **An explicit unmount before the pass withdraws the order from the restore**: the operator
/// removed the strategy, its rows left the collection, so the venue order stays an external one.
#[test]
fn an_unmounted_mounts_order_is_not_adopted() {
    let dir = Scratch::reserved("vco-restore-unmounted");
    let (next, events) = (cell(), Events::default());
    let order_client_id = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut core = restart_without_the_mount(&dir, &next, &events);
    core.dispatch(mount_command(spec("rt-b")));
    core.dispatch(unmount_command("rt-b"));

    run_held(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());

    assert!(core.engine.registry.is_empty(), "not adopted");
    assert_eq!(core.restore_counters, RestoreCounters::default());
    assert_eq!(alert_kinds(&core), vec![ReconcileDivergenceKind::UnknownOrder]);
}

/// 16 — **After the adoption the restored order holds its tag as any working order does**: before
/// the pass a fresh quote over the restored tag counts nothing (`order_owners_tests`' test 15); after
/// the order is adopted the same quote is REFUSED as busy (P1), where it used to leave the adopted
/// order live and tag-less (counted): nothing reaches the client, nothing is orphaned, and the tag
/// keeps naming the adopted order.
#[test]
fn after_the_adoption_a_fresh_quote_over_the_restored_tag_is_refused_as_busy() {
    let dir = Scratch::reserved("vco-restore-overwrite");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());
    assert_eq!(core.restore_counters.tags_repointed_order_still_live, 0);
    events.lock().unwrap().clear();

    assert_eq!(try_quote(&mut core, &nb), 0, "the busy quote never reaches the client");

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.contains("q is busy: amend it, or cancel it first"), "{got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "the Denied carries the tag: {got:?}");
    assert_eq!(core.restore_counters.tags_repointed_order_still_live, 0, "nothing is orphaned");
    assert_eq!(
        core.strategy_tags.get(&q_key(0)),
        Some(&order_client_id),
        "the adopted order keeps its tag"
    );
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
        let order =
            OrderOwnerEntry { tag: Some(format!("t{i}")), ..restored_q(&format!("r{i:04}")) };
        core.order_client_id_mount.insert(order.order_client_id.clone(), 0);
        core.restore_owned_order(0, order);
    }
    let reports: Vec<_> =
        (0..N).map(|i| report(&format!("r{i:04}"), OrderStatus::Accepted)).collect();

    run(&mut core, reports, Vec::new());

    assert_eq!(core.engine.registry.len(), N);
    assert_eq!(core.restore_counters.restored_orders_adopted, N as u64);
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
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());
    drop(core);
    let on_disk = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap();
    assert!(
        on_disk.contains(&order_client_id) && !on_disk.contains("\"kind\":\"forget\""),
        "{on_disk}"
    );
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
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    assert_eq!(try_quote(&mut core, &nb), 0, "the submit never reaches the venue client");

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.starts_with("Denied"), "{got:?}");
    assert!(got[0].0.contains("still holds this tag"), "the reason rides the event: {got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "the Denied carries the tag: {got:?}");
    assert_eq!(
        core.strategy_tags.get(&q_key(0)),
        Some(&order_client_id),
        "the restored order keeps its tag"
    );
    assert!(core.restored_orders.contains_key(&order_client_id), "still unadjudicated");
    assert_eq!(core.restore_counters.tags_repointed_order_still_live, 0);
    assert_eq!(held_notes(&core), 1, "recent: {:?}", core.recent);

    assert_eq!(try_quote(&mut core, &nb), 0);
    assert_eq!(heard(&events).len(), 2, "a second refusal is a second event");
    assert_eq!(held_notes(&core), 1, "but not a second note: the ring is not flooded");
}

/// 20 — **ADOPT ends the restore's refusal**: the pass spends the collection row and the mount hears
/// the tagged `Accepted` after its `Denied`. The same quote is then refused for the reason every
/// working order's tag gives (busy, P1), no longer the restore's; a real maker re-prices by tag from
/// here (the spy just submits, which used to orphan the adopted order, test 16).
#[test]
fn the_refusal_ends_when_the_pass_adopts_the_order() {
    let dir = Scratch::reserved("vco-restore-tag-denied-adopt");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0);
    events.lock().unwrap().clear();

    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());
    assert_eq!(heard(&events), vec![accepted_q()], "the maker is told the order lives");
    events.lock().unwrap().clear();

    assert_eq!(try_quote(&mut core, &nb), 0, "refused, now as busy");
    let got = heard(&events);
    assert!(got.len() == 1 && !got[0].0.contains("still holds this tag"), "{got:?}");
    assert!(got[0].0.contains("is busy"), "{got:?}");
    assert_eq!(core.restore_counters.tags_repointed_order_still_live, 0, "nothing is orphaned");
}

/// 21 — **GONE ends the refusal**: the mount hears the restore's `Canceled`, the tag is retired, and
/// the next quote is accepted with nothing orphaned. One pass is enough when the venue REPORTS the
/// order closed (a terminal row is proof).
#[test]
fn the_refusal_ends_when_the_pass_finds_the_order_gone() {
    let dir = Scratch::reserved("vco-restore-tag-denied-gone");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0);

    run(&mut core, vec![report(&order_client_id, OrderStatus::Canceled)], Vec::new());
    assert!(core.restored_orders.is_empty() && core.strategy_tags.is_empty());

    assert_eq!(try_quote(&mut core, &nb), 1, "a new quote is accepted");
    let fresh = core.engine.client.submissions.last().unwrap().order_client_id.clone();
    assert_eq!(core.strategy_tags.get(&q_key(0)), Some(&fresh));
    assert_eq!(core.restore_counters.tags_repointed_order_still_live, 0);
}

/// 21b — **A pass that finds a PLAIN order unlisted ends the refusal too, a stop's does not.** Silence
/// is no proof (the row stays, no mount is told), but a maker's plain quote the venue no longer lists
/// is re-quoted as it was when silence meant gone: the first silent pass is enough, not the quiet
/// passes. A stop may rest where the client does not look (and an entry that records no kind may be
/// one), so it stays refused until the fail-open window.
#[test]
fn a_pass_that_finds_a_plain_order_unlisted_ends_the_refusal_but_a_stops_does_not() {
    let dir = Scratch::reserved("vco-restore-tag-denied-unproven-plain");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0, "refused before any pass");

    run(&mut core, Vec::new(), Vec::new());
    assert!(
        core.restore_unproven.contains_key(&order_client_id),
        "precondition: unproven, not gone"
    );
    assert!(core.restored_orders.contains_key(&order_client_id), "the row stays for a later pass");

    assert_eq!(try_quote(&mut core, &nb), 1, "the plain order was judged: no longer refused");

    let dir = Scratch::reserved("vco-restore-tag-denied-unproven-stop");
    let (order_client_id, nb, events) = rest_one_stop(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0, "refused before any pass");

    run(&mut core, Vec::new(), Vec::new());
    assert!(
        core.restore_unproven.contains_key(&order_client_id),
        "precondition: unproven, not gone"
    );

    assert_eq!(try_quote(&mut core, &nb), 0, "a stop may rest where the client does not look");
}

/// 22 — **The refusal fails open.** A pass that never comes (reconcile off, a fetch that keeps
/// failing) must not silence the mount: the refusal holds through the last millisecond of
/// `RESTORED_TAG_DENY_WINDOW_MS` counted from the FIRST refusal, and from the window's end the
/// submit goes through, as it did before the check existed. The window is the model constant, not a
/// literal here: the settings loader bounds the first pass's delay against it
/// (`MAX_RECONCILE_STARTUP_DELAY_MS`), so a test that hard-coded its own number would keep passing
/// after the two had drifted apart.
#[test]
fn the_refusal_fails_open_when_no_pass_comes() {
    use vike_venues::venue_restore::RESTORED_TAG_DENY_WINDOW_MS;

    let dir = Scratch::reserved("vco-restore-tag-denied-valve");
    let (_order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert_eq!(try_quote(&mut core, &nb), 0, "refused first");

    core.engine.now_ms += RESTORED_TAG_DENY_WINDOW_MS - 1;
    assert_eq!(
        try_quote(&mut core, &nb),
        0,
        "one millisecond before the window ends the restored order still mutes the tag"
    );

    core.engine.now_ms += 1;
    assert_eq!(try_quote(&mut core, &nb), 1, "the window has passed: the submit goes through");
}

/// 23 — **A venue the gate has not opened never refuses**: its restored entry waits for ever (no
/// pass judges it), so a refusal would silence the mount for ever. Nothing is denied and nothing is
/// noted; the submit behaves exactly as before.
#[test]
fn a_closed_venue_gate_never_refuses_a_quote() {
    let dir = Scratch::reserved("vco-restore-tag-denied-gate-off");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    assert!(core.restored_orders.contains_key(&order_client_id), "precondition: restored at boot");

    assert_eq!(try_quote(&mut core, &nb), 1);

    assert!(heard(&events).is_empty(), "no Denied: {:?}", heard(&events));
    assert_eq!(held_notes(&core), 0);
}

/// 24 — **A venue the gate keeps closed says so once at boot, and names both causes.** The default
/// gate is the declared table, closed for `sim`; the live root closes it for every venue when the
/// operator refused reconcile, and the note cannot tell the two apart, so it names both. One line
/// per venue, with the count of the restored orders that wait.
#[test]
fn a_closed_venue_gate_notes_the_waiting_venue_once_and_names_both_causes() {
    let dir = Scratch::reserved("vco-restore-gate-note");
    let (order_client_id, nb, events) = rest_one(&dir);
    let core = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
    assert!(core.restored_orders.contains_key(&order_client_id), "precondition: restored at boot");

    assert!(
        noted(&core, "ORDER RESTORE OFF for sim: 1 restored order(s) stay unconfirmed"),
        "recent: {:?}",
        core.recent
    );
    assert!(noted(&core, "restore table"), "the table is a cause: {:?}", core.recent);
    assert!(noted(&core, "refused reconcile"), "a refusal is the other: {:?}", core.recent);
    let lines = core.recent.iter().filter(|l| l.starts_with("ORDER RESTORE OFF")).count();
    assert_eq!(lines, 1, "one line per venue: {:?}", core.recent);
}

/// A mount on the id and series of [`spy_mount`] that pulls its `q` quote on the next feed status:
/// what a `SpreadMaker` side does when its restored-quote clock runs out
/// (`strategies/market_making/spread_maker.rs`'s `restored_quote_expired`), which is a `cancel`
/// by tag.
struct Puller;

impl Strategy<LiveBroker> for Puller {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: vike_model::FeedStatus) {
        broker.cancel_tagged("q");
    }
}

/// [`Puller`] mounted where the ownership file's `q` order was placed (mount `maker-b`).
fn puller_mount() -> Mount {
    Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some("maker-b".to_string()),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(Puller),
    }
}

/// Run the pulling mount once and deliver what it buffered.
fn pull_q(core: &mut Core) {
    core.drive_strategy_feed_status("sim", "BTCUSDT", vike_model::FeedStatus::Live);
    core.dispatch_applied_fills();
}

/// 25 — **A cancel by tag of a restored order reaches no venue until the pass has registered it.**
/// This is why the maker's own restored-quote clock (the file's `RESTORED_QUOTE_VERIFY_MS`) is
/// not part of the first pass's deadline: if it runs out before the pass, the maker's
/// `cancel` resolves the tag to the restored order client id (boot filed it) and
/// `Engine::cancel_order_with_intent` finds no live order under that order client id, because boot
/// touches no registry. Nothing is sent, the tag and the collection row stay for the pass to
/// judge, and once the pass has adopted the order the same pull is a real cancel. A future change
/// that sent a cancel for an order the registry does not hold would make the early pull a real
/// hazard (the restored order cancelled by a clock nobody tied to the pass), and this test is the
/// tripwire.
#[test]
fn a_cancel_by_tag_of_a_restored_order_waits_for_the_pass_to_register_it() {
    let dir = Scratch::reserved("vco-restore-cancel-before-pass");
    let (order_client_id, _nb, _events) = rest_one(&dir);
    let mut core = assembled(open(config_on(&dir, vec![puller_mount()])));
    assert_eq!(
        core.strategy_tags.get(&q_key(0)),
        Some(&order_client_id),
        "precondition: the tag is back"
    );
    assert!(core.engine.registry.is_empty(), "precondition: boot touches no registry");

    pull_q(&mut core);
    assert!(
        core.engine.client.cancels.is_empty(),
        "an order the registry does not hold is never cancelled: {:?}",
        core.engine.client.cancels
    );
    assert_eq!(
        core.strategy_tags.get(&q_key(0)),
        Some(&order_client_id),
        "the pull took no tag away"
    );
    assert!(
        core.restored_orders.contains_key(&order_client_id),
        "the pass still has the order to judge"
    );

    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());
    assert!(
        core.engine.registry.contains_key(&order_client_id),
        "precondition: the pass adopted it"
    );
    pull_q(&mut core);
    assert_eq!(
        core.engine.client.cancels,
        vec![order_client_id.clone()],
        "registered, the same pull is a real cancel"
    );
}

// --- the wire-id translation (decision 0121): a venue whose reports name an order by a function of
// --- its order client id (Hyperliquid's keccak cloid), not by the order client id.

/// The wire id the fake client of the tests below derives for `order_client_id`.
const WIRE: &str = "0xw-";

fn wire_of(order_client_id: &str) -> String {
    format!("{WIRE}{order_client_id}")
}

/// Boot 2 whose client translates: its reports name an order by `wire_of(order client id)`.
fn restart_wire(dir: &Scratch, nb: &NextQuote, events: &Events) -> Core {
    let mut core = restart(dir, nb, events);
    core.engine.client.wire_prefix = Some(WIRE.to_string());
    core
}

/// A venue open-order row that carries ONLY the wire id, on the venue's own spelling of the symbol
/// (`@107`, a spot coin) rather than the unified one the ownership file remembers.
fn wire_report(order_client_id: &str, status: OrderStatus) -> OrderStatusReport {
    OrderStatusReport {
        order_client_id: Some(wire_of(order_client_id)),
        symbol: "@107".into(),
        ..report(order_client_id, status)
    }
}

/// 19 — **A report that carries only the wire id is matched to the restored order client id and ADOPTED**:
/// the registry holds the order under the REAL order client id and the UNIFIED symbol (the spot coin of the
/// report is remapped from the ownership record), the mount hears the tagged `Accepted`, the client
/// is told `(order client id, symbol)`, and the diff that follows raises no `UnknownOrder` (the wire spelling
/// is nowhere in the registry). An external order beside it keeps its own id and symbol untouched.
#[test]
fn a_report_carrying_only_the_wire_id_is_adopted_under_the_real_order_client_id() {
    let dir = Scratch::reserved("vco-restore-wire-adopt");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);

    let external = OrderStatusReport {
        order_client_id: Some(wire_of("someone-elses")),
        symbol: "@999".into(),
        ..report("x", OrderStatus::Accepted)
    };
    run_held(
        &mut core,
        vec![wire_report(&order_client_id, OrderStatus::Accepted), external],
        Vec::new(),
    );

    let adopted =
        core.engine.registry.get(&order_client_id).expect("adopted under the real order client id");
    assert!(adopted.status.is_live());
    assert_eq!(
        adopted.request.symbol, "BTCUSDT",
        "the spot coin is remapped to the unified symbol"
    );
    assert!(
        !core.engine.registry.contains_key(&wire_of(&order_client_id)),
        "the wire spelling never enters the registry"
    );
    assert!(
        !core.engine.registry.contains_key(&wire_of("someone-elses")),
        "an order the file does not know is untouched"
    );
    assert_eq!(heard(&events), vec![accepted_q()], "the mount's liveness confirmation, tagged");
    assert_eq!(core.restore_counters.restored_orders_adopted, 1);
    assert_eq!(
        core.engine.client.adopted,
        vec![vike_execution::RestoredOrder {
            venue_order_id: Some(format!("v-{order_client_id}")),
            ..vike_execution::RestoredOrder::new(order_client_id.clone(), "BTCUSDT")
        }],
        "the client is told the adopted (order client id, unified symbol, the report's venue order id)"
    );
    let kinds = alert_kinds(&core);
    assert_eq!(
        kinds,
        vec![ReconcileDivergenceKind::UnknownOrder],
        "only the external one: {kinds:?}"
    );
    assert!(core.restored_orders.is_empty());
}

/// 20 — **Absent with a translating client is still GONE**: the wire map changes how a report is
/// READ, not what silence means.
#[test]
fn an_absent_wire_id_is_gone() {
    let dir = Scratch::reserved("vco-restore-wire-gone");
    let (_order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);

    run_silence_until_gone(&mut core);

    let got = heard(&events);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.contains(RESTORE_GONE_REASON), "{got:?}");
    assert_eq!(core.restore_counters.restored_orders_gone, 1);
    assert!(core.engine.client.adopted.is_empty(), "nothing adopted, nothing announced");
}

/// 21 — **A fill that carries the wire id is a fill of the real order client id**: the restore counts the order
/// filled while down, and the existing `MissingFill` path books the fill under the REAL order client id (the
/// held alert's proposed fill names it), so `mount_for_order_client_id` can find the mount.
#[test]
fn a_fill_carrying_the_wire_id_becomes_a_fill_of_the_real_order_client_id() {
    let dir = Scratch::reserved("vco-restore-wire-fill");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);
    let wire_fill = FillReport {
        order_client_id: Some(wire_of(&order_client_id)),
        symbol: "@107".into(),
        ..fill_of(&order_client_id)
    };

    run(&mut core, Vec::new(), vec![wire_fill]);

    assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1, "filled while down");
    assert_eq!(
        core.restore_counters.restored_orders_gone, 0,
        "never gone: the fill is the evidence"
    );
    let missing = core
        .reconcile_alerts
        .values()
        .find(|a| a.kind == ReconcileDivergenceKind::MissingFill)
        .expect("the existing path holds the fill");
    let names: Vec<&str> = missing
        .proposed_events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.order_client_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![order_client_id.as_str()],
        "the proposed fill names the real order client id, not the wire id"
    );
}

/// 22 — **A client with the default `wire_id_for` (`None`) behaves exactly as today**: a report that
/// merely LOOKS wire-shaped is an external order, the restored order client id it never named is gone, and no
/// symbol is rewritten.
#[test]
fn a_client_without_a_wire_id_reads_the_reports_as_it_always_did() {
    let dir = Scratch::reserved("vco-restore-wire-default");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    assert!(core.engine.client.wire_prefix.is_none(), "precondition: the default client");

    run_held(&mut core, vec![wire_report(&order_client_id, OrderStatus::Accepted)], Vec::new());

    assert!(core.engine.registry.is_empty(), "nothing is translated, so nothing is adopted");
    assert_eq!(
        core.restore_counters.restored_orders_gone, 0,
        "the real order client id is absent: unproven, held"
    );
    assert!(core.restore_unproven.contains_key(&order_client_id), "and noted as such");
    assert_eq!(alert_kinds(&core), vec![ReconcileDivergenceKind::UnknownOrder]);
    assert!(core.engine.client.adopted.is_empty());
}

/// 23 — **Every adoption is announced to the client, with or without a wire id**, once: a second
/// pass over the same reports announces nothing (the order is already in the registry).
#[test]
fn the_adopted_pairs_reach_the_client_once() {
    let dir = Scratch::reserved("vco-restore-adopt-announce");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());
    assert_eq!(
        core.engine.client.adopted,
        vec![vike_execution::RestoredOrder {
            venue_order_id: Some(format!("v-{order_client_id}")),
            ..vike_execution::RestoredOrder::new(order_client_id.clone(), "BTCUSDT")
        }],
        "the report's venue order id rides with the pair"
    );
    run(&mut core, vec![report(&order_client_id, OrderStatus::Accepted)], Vec::new());

    assert_eq!(core.engine.client.adopted.len(), 1, "a repeat pass announces nothing");
}

/// 24 — **A FILLED report under the wire id is filled while down**: the terminal row is rewritten
/// too, so the verdict names the real order client id; it is neither adopted nor announced to the client.
#[test]
fn a_filled_report_under_the_wire_id_is_filled_while_down() {
    let dir = Scratch::reserved("vco-restore-wire-filled-status");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_wire(&dir, &nb, &events);

    run(&mut core, vec![wire_report(&order_client_id, OrderStatus::Filled)], Vec::new());

    assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1);
    assert_eq!(core.restore_counters.restored_orders_gone, 0);
    assert!(core.engine.registry.is_empty(), "a filled order is not adopted");
    assert!(core.engine.client.adopted.is_empty());
}

// ---- the scope of a pass is what the client READS, not what the engine serves ----------------------

/// The symbol the engine below ADMITTED on demand (decision 0173): the engine serves it, the
/// ownership file remembers a restored order on it, and whether the venue's reports span it is the
/// client's to say (`ReconcileReports::admitted_symbols`).
const ETH: &str = "ETHUSDT";

/// Boot 2 whose engine admitted [`ETH`] and whose one restored order rests there.
fn restart_on_admitted_eth(dir: &Scratch, nb: &NextQuote, events: &Events) -> Core {
    let mut core = restart(dir, nb, events);
    let outcome = core.engine.admit_symbol(&vike_execution::SymbolAdmission {
        route_key: "sim".into(),
        symbol: ETH.into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.01),
            lot_size: Some(0.001),
            ..Default::default()
        },
        contract_value: None,
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    });
    assert!(matches!(outcome, vike_execution::AdmitOutcome::Admitted), "{outcome:?}");
    for entry in core.restored_orders.values_mut() {
        entry.symbol = Some(ETH.into());
    }
    core
}

/// The venue's open-order row for `order_client_id` on [`ETH`].
fn eth_report(order_client_id: &str) -> OrderStatusReport {
    OrderStatusReport { symbol: ETH.into(), ..report(order_client_id, OrderStatus::Accepted) }
}

/// One pass whose client says it was told `admitted` (`None`: it keeps no list).
fn run_told(core: &mut Core, admitted: Option<&[&str]>, orders: Vec<OrderStatusReport>) {
    let pass = ReconcileReports {
        admitted_symbols: admitted.map(|l| l.iter().map(ToString::to_string).collect()),
        lists_trigger_orders: false,
        ..pass_for("sim", None, orders, Vec::new())
    };
    core.reconcile_reports(pass);
    core.dispatch_applied_fills();
}

/// How many recent-events lines say `order_client_id` is HELD for a symbol the client was not told about.
fn unread_notes(core: &Core, order_client_id: &str) -> usize {
    core.recent
        .iter()
        .filter(|l| l.starts_with("RESTORE ") && l.contains("HELD") && l.contains(order_client_id))
        .count()
}

/// **Admitted, and the client was told: the order is adopted** and a cancel of it reaches the venue.
#[test]
fn an_order_on_an_admitted_symbol_the_client_was_told_about_is_adopted_and_cancellable() {
    let dir = Scratch::reserved("vco-restore-told-adopt");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);

    run_told(&mut core, Some(&[ETH]), vec![eth_report(&order_client_id)]);

    assert!(core.engine.registry[&order_client_id].status.is_live(), "adopted live");
    assert_eq!(heard(&events), vec![accepted_q()]);
    assert!(core.restored_orders.is_empty());
    core.engine.cancel_order(&order_client_id);
    assert_eq!(
        core.engine.client.cancels,
        vec![order_client_id],
        "the adopted order is cancellable"
    );
}

/// **Re-admitted and told, but no price feed** (IBKR on 2026-11-06, the boot's `no price feed for the re-admitted`
/// and the bridge's market-data cap behind it): the symbol's price is the door's to refuse an opening order over,
/// and nothing about a resting order needs one. So the pass adopts it, the sweeps read no price for it and count
/// nothing, and each registry-driven cancel (the operator's, a mass cancel, the panic button, the safe state) reaches
/// it. The cancels go out one after the other because the recording client answers none of them.
#[test]
fn an_order_on_a_told_symbol_with_no_price_is_adopted_and_every_door_cancels_it() {
    let dir = Scratch::reserved("vco-restore-told-unpriced");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);
    core.engine.price_board = Default::default();

    run_told(&mut core, Some(&[ETH]), vec![eth_report(&order_client_id)]);

    assert!(
        core.engine.registry[&order_client_id].status.is_live(),
        "adopted with no price on the board"
    );
    assert!(alert_kinds(&core).is_empty(), "nothing is held: {:?}", alert_kinds(&core));
    assert_eq!(
        core.closes_in_flight(0, ETH, 1, None),
        0.0,
        "a sweep counts nothing it cannot price"
    );

    let doors = [
        OrderIntent::Cancel(order_client_id.clone()),
        OrderIntent::MassCancel { venue: None, symbol: None, account: None },
        OrderIntent::MarketExit { venue: None, account: None },
    ];
    for (asked, door) in doors.into_iter().enumerate() {
        core.dispatch(Ingest::Command(Command::Order(door)));
        assert_eq!(core.engine.client.cancels.len(), asked + 1, "{:?}", core.recent);
    }
    core.enter_safe_state("the test's fault".to_string());
    assert_eq!(core.engine.client.cancels, vec![order_client_id; 4], "{:?}", core.recent);
}

/// **Admitted, the client was told, and the venue does not list it: gone**, as for a mounted symbol.
#[test]
fn an_order_on_a_told_symbol_the_venue_does_not_list_is_gone() {
    let dir = Scratch::reserved("vco-restore-told-gone");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);

    for _ in 0..PASSES_TO_PROOF {
        run_told(&mut core, Some(&[ETH]), Vec::new());
    }

    assert_eq!(core.restore_counters.restored_orders_gone, 1);
    let got = heard(&events);
    assert!(got.len() == 1 && got[0].0.contains(RESTORE_GONE_REASON), "{got:?}");
    assert!(!file_owns(core, &dir).contains(&order_client_id), "the file forgot it");
}

/// **THE FIX. Admitted, and the client was NOT told: the order is HELD, never retired.** The venue's
/// silence about a symbol the client did not read says nothing, so the mount hears nothing, the file
/// keeps the entry, nothing is counted gone, and one note names the order. A LATER pass whose client
/// has been told adopts it.
#[test]
fn an_order_on_an_admitted_symbol_the_client_was_not_told_about_is_held_and_adopted_once_told() {
    let dir = Scratch::reserved("vco-restore-untold-held");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);

    run_told(&mut core, Some(&[]), Vec::new());

    assert!(heard(&events).is_empty(), "no phantom Canceled: {:?}", heard(&events));
    assert_eq!(core.restore_counters.restored_orders_gone, 0);
    assert!(core.restored_orders.contains_key(&order_client_id), "still restored, still waiting");
    assert!(core.engine.registry.is_empty(), "and not adopted from a report that cannot list it");
    assert_eq!(unread_notes(&core, &order_client_id), 1, "recent: {:?}", core.recent);

    run_told(&mut core, Some(&[ETH]), vec![eth_report(&order_client_id)]);

    assert!(
        core.engine.registry[&order_client_id].status.is_live(),
        "adopted once the client is told"
    );
    assert_eq!(heard(&events), vec![accepted_q()]);
    assert_eq!(
        file_owns(core, &dir),
        vec![order_client_id],
        "an adoption leaves the file as it was"
    );
}

/// The hold is said ONCE per order, not once per pass (a pass runs every interval for as long as the
/// client is not told).
#[test]
fn a_held_order_is_announced_once_not_every_pass() {
    let dir = Scratch::reserved("vco-restore-untold-once");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);

    for _ in 0..3 {
        run_told(&mut core, Some(&[]), Vec::new());
    }

    assert_eq!(unread_notes(&core, &order_client_id), 1, "recent: {:?}", core.recent);
}

/// A held order's file entry survives the process: the next boot still knows whose it was.
#[test]
fn a_held_orders_file_entry_survives() {
    let dir = Scratch::reserved("vco-restore-untold-file");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);

    run_told(&mut core, Some(&[]), Vec::new());

    assert_eq!(file_owns(core, &dir), vec![order_client_id]);
}

/// **A pass that names no list judges the engine's scope, as it always did**: a client that keeps
/// none (every venue but the three that admit symbols) and every journal written before the field.
#[test]
fn a_pass_with_no_list_judges_the_engines_symbols_as_before() {
    let dir = Scratch::reserved("vco-restore-nolist");
    let (_order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);

    for _ in 0..PASSES_TO_PROOF {
        run_told(&mut core, None, Vec::new());
    }

    assert_eq!(
        core.restore_counters.restored_orders_gone, 1,
        "the engine serves the symbol: judged"
    );
    assert_eq!(heard(&events).len(), 1);
}

/// The MOUNTED symbol is always read, whatever the list says: an empty list holds nothing there.
#[test]
fn the_mounted_symbol_is_judged_whatever_the_list_says() {
    let dir = Scratch::reserved("vco-restore-mounted");
    let (_order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    for _ in 0..PASSES_TO_PROOF {
        run_told(&mut core, Some(&[]), Vec::new());
    }

    assert_eq!(core.restore_counters.restored_orders_gone, 1);
}

/// A symbol DECLARED at assembly (a leg, not an admission) is the engine's scope as before, list or
/// no list: only an on-demand row depends on what the client was told.
#[test]
fn a_declared_extra_symbol_is_judged_whatever_the_list_says() {
    let dir = Scratch::reserved("vco-restore-declared");
    let (_order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    core.engine.extra_symbols.push("DECLUSDT".into());
    for entry in core.restored_orders.values_mut() {
        entry.symbol = Some("DECLUSDT".into());
    }

    for _ in 0..PASSES_TO_PROOF {
        run_told(&mut core, Some(&[]), Vec::new());
    }

    assert_eq!(core.restore_counters.restored_orders_gone, 1);
}

/// **A symbol never admitted stays out**, even when a client claims it was told: the engine does not
/// serve it, so the pass cannot speak for it.
#[test]
fn a_symbol_the_engine_never_admitted_is_not_judged_even_if_a_client_lists_it() {
    let dir = Scratch::reserved("vco-restore-never-admitted");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    for entry in core.restored_orders.values_mut() {
        entry.symbol = Some(ETH.into());
    }

    for _ in 0..PASSES_TO_PROOF {
        run_told(&mut core, Some(&[ETH]), Vec::new());
    }

    assert_eq!(core.restore_counters.restored_orders_gone, 0);
    assert!(core.restored_orders.contains_key(&order_client_id));
    assert!(core.restore_unproven.is_empty(), "not this engine's symbol: not even held");
    assert!(heard(&events).is_empty());
    assert_eq!(
        unread_notes(&core, &order_client_id),
        0,
        "nothing held: it is not this engine's symbol"
    );
}

/// The held divergence kinds of the two LOCAL-side sweeps (an order or a position the venue's report does
/// not mention), sorted.
fn orphan_kinds(core: &Core) -> Vec<ReconcileDivergenceKind> {
    let mut kinds: Vec<ReconcileDivergenceKind> = alert_kinds(core)
        .into_iter()
        .filter(|k| {
            matches!(
                k,
                ReconcileDivergenceKind::OrphanLocalOrder
                    | ReconcileDivergenceKind::OrphanLocalPosition
            )
        })
        .collect();
    kinds.sort();
    kinds
}

/// **The diff speaks only for the symbols the client READ** (decision 0216, the half the restore already
/// had). The engine holds a live order and a position in the admitted [`ETH`]; the pass's client was told
/// nothing and its report mentions neither (it never fetched the symbol): no `OrphanLocalOrder` and no
/// `OrphanLocalPosition` is held for what it could not have seen. Once a pass's client WAS told the
/// symbol, the same silence is the venue's answer and both rows are held, and a pass that names no list
/// sweeps as it always did.
#[test]
fn a_pass_holds_no_orphan_for_a_symbol_its_client_was_not_told_about() {
    let dir = Scratch::reserved("vco-diff-scope-unread");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);
    run_told(&mut core, Some(&[ETH]), vec![eth_report(&order_client_id)]);
    assert!(
        core.engine.registry[&order_client_id].status.is_live(),
        "premise: a live local order in ETH"
    );
    core.dispatch(Ingest::Event(Event::Fill(vike_model::events::FillEvent {
        trade_id: "t-eth".into(),
        order_client_id: "c-eth".into(),
        venue: "sim".into(),
        symbol: ETH.into(),
        side: 1,
        last_qty: 2.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })));
    assert!(
        core.engine.account.positions.values().any(|p| p.size.abs() > 1.0),
        "premise: a local position in ETH"
    );
    // The venue's report carries a position row (so the position sweep is armed) for the mounted symbol only.
    let flat_btc = vike_model::PositionStatusReport::flat("sim", "BTCUSDT");
    let pass = |admitted: Option<Vec<String>>| ReconcileReports {
        admitted_symbols: admitted,
        positions: vec![flat_btc.clone()],
        ..pass_for("sim", None, Vec::new(), Vec::new())
    };

    core.reconcile_reports(pass(Some(Vec::new())));
    assert_eq!(orphan_kinds(&core), Vec::new(), "ETH was not read: {:?}", alert_kinds(&core));

    core.reconcile_reports(pass(Some(vec![ETH.to_string()])));
    core.reconcile_reports(pass(Some(vec![ETH.to_string()]))); // held on its second pass in a row
    assert_eq!(
        orphan_kinds(&core),
        vec![
            ReconcileDivergenceKind::OrphanLocalOrder,
            ReconcileDivergenceKind::OrphanLocalPosition
        ],
        "ETH was read and the venue said nothing: both are the venue's answer"
    );
}

/// A pass whose client keeps no list (`None`: every venue but the three that admit symbols) sweeps the
/// engine's symbols as it always did: the scope narrows nothing it was not asked to.
#[test]
fn a_pass_with_no_list_sweeps_an_admitted_symbol_as_before() {
    let dir = Scratch::reserved("vco-diff-scope-nolist");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_on_admitted_eth(&dir, &nb, &events);
    run_told(&mut core, Some(&[ETH]), vec![eth_report(&order_client_id)]);

    core.reconcile_reports(pass_for("sim", None, Vec::new(), Vec::new()));
    core.reconcile_reports(pass_for("sim", None, Vec::new(), Vec::new())); // held on its second pass

    assert_eq!(
        orphan_kinds(&core),
        vec![ReconcileDivergenceKind::OrphanLocalOrder],
        "{:?}",
        alert_kinds(&core)
    );
}

// --- an order list's legs across a restart (decision 0269's W3c): each leg's tag rides into its own
// --- ownership record, so a restart files it back as a tagged submit's is, and a list naming a tag a
// --- restored order still holds is refused whole.

/// What the [`Lister`] mount does on its next feed-status call.
#[derive(Clone, Copy)]
enum ListAct {
    /// a sell limit at 110 (`tp`) and a sell stop at 95 (`sl`), one cancelling the other
    Place,
    Cancel(&'static str),
}

type NextList = std::sync::Arc<std::sync::Mutex<Option<ListAct>>>;

/// A mount (`maker-b` on `sim`/`BTCUSDT`) that places or cancels by tag through
/// `vike_model::OrderBroker`, and records every lifecycle event it hears with its tag.
struct Lister {
    next: NextList,
    events: Events,
}

impl Strategy<LiveBroker> for Lister {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: vike_model::FeedStatus) {
        let act = self.next.lock().unwrap().take();
        match act {
            Some(ListAct::Place) => vike_model::OrderBroker::place_group(broker, "", oco_legs()),
            Some(ListAct::Cancel(tag)) => vike_model::OrderBroker::cancel(broker, tag),
            None => {}
        }
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &vike_model::OrderEvent) {
        self.events.lock().unwrap().push((format!("{:?}", event.kind), event.tag.clone()));
    }
}

fn oco_legs() -> Vec<(String, vike_model::OrderListLeg)> {
    let leg = |order_type, price, trigger_price, other| vike_model::OrderListLeg {
        side: -1,
        qty: 1.0,
        order_type,
        price,
        trigger_price,
        trail: None,
        reduce_only: false,
        time_in_force: vike_model::TimeInForce::Gtc,
        parent: None,
        contingency: Some(vike_model::ContingencyType::Oco),
        linked: vec![other],
        order_client_id: None,
    };
    vec![
        ("tp".to_string(), leg(vike_model::OrderType::Limit, Some(110.0), None, 1)),
        ("sl".to_string(), leg(vike_model::OrderType::Stop, None, Some(95.0), 0)),
    ]
}

fn lister_mount(next: &NextList, events: &Events) -> Mount {
    Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some("maker-b".to_string()),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(Lister {
            next: std::sync::Arc::clone(next),
            events: std::sync::Arc::clone(events),
        }),
    }
}

/// Run the [`Lister`] mount once with `act`, deliver what it buffered, and answer how many orders it
/// put on the venue client.
fn list_act(core: &mut Core, next: &NextList, act: ListAct) -> usize {
    *next.lock().unwrap() = Some(act);
    let before = core.engine.client.submissions.len();
    core.drive_strategy_feed_status("sim", "BTCUSDT", vike_model::FeedStatus::Live);
    core.dispatch_applied_fills();
    core.engine.client.submissions.len() - before
}

/// The order client id the [`Lister`] mount's tag `tag` names.
fn leg_order_client_id(core: &Core, tag: &str) -> Option<String> {
    core.strategy_tags.get(&format!("0|sim|BTCUSDT|{tag}")).cloned()
}

/// Boot 1: the [`Lister`] mount rests both legs, then the process "dies". Answers their order client ids
/// `(tp, sl)`.
fn rest_list(dir: &Scratch) -> (String, String, NextList, Events) {
    let (next, events) = (NextList::default(), Events::default());
    let mut first = assembled(config_on(dir, vec![lister_mount(&next, &events)]));
    assert_eq!(list_act(&mut first, &next, ListAct::Place), 2, "both legs go to the venue");
    let take_profit = leg_order_client_id(&first, "tp").expect("filed");
    let stop_loss = leg_order_client_id(&first, "sl").expect("filed");
    drop(first);
    events.lock().unwrap().clear();
    (take_profit, stop_loss, next, events)
}

/// 26 — **A tagged list's legs carry their own tags into the ownership file**, and a restart files
/// each back under its tag, as a tagged submit's is.
#[test]
fn a_tagged_lists_legs_are_owned_under_their_tags_and_restored_so() {
    let dir = Scratch::reserved("vco-restore-list-tags");
    let (take_profit, stop_loss, next, events) = rest_list(&dir);
    let mut owned: Vec<(String, Option<String>)> = OrderOwnerLog::in_state_dir(&dir)
        .start(NOW)
        .order_client_ids
        .into_iter()
        .map(|o| (o.order_client_id, o.tag))
        .collect();
    owned.sort();
    let mut want = vec![
        (take_profit.clone(), Some("tp".to_string())),
        (stop_loss.clone(), Some("sl".to_string())),
    ];
    want.sort();
    assert_eq!(owned, want, "each leg owned under its own tag");

    let core = restart_list(&dir, &next, &events);
    assert_eq!(leg_order_client_id(&core, "tp"), Some(take_profit));
    assert_eq!(leg_order_client_id(&core, "sl"), Some(stop_loss));
}

/// Boot 2 over the same file, gate open, the same [`Lister`] mount.
fn restart_list(dir: &Scratch, next: &NextList, events: &Events) -> Core {
    assembled(open(config_on(dir, vec![lister_mount(next, events)])))
}

/// 27 — **After a restart a cancel by one leg's tag reaches that leg**, once the pass has adopted
/// both, and the other leg rests on.
#[test]
fn after_a_restart_a_cancel_by_a_legs_tag_reaches_that_leg() {
    let dir = Scratch::reserved("vco-restore-list-cancel");
    let (take_profit, stop_loss, next, events) = rest_list(&dir);
    let mut core = restart_list(&dir, &next, &events);
    let row = |order_client_id: &str, order_type| OrderStatusReport {
        side: -1,
        order_type,
        ..report(order_client_id, OrderStatus::Accepted)
    };
    let rows = vec![
        row(&take_profit, vike_model::OrderType::Limit),
        row(&stop_loss, vike_model::OrderType::Stop),
    ];
    run(&mut core, rows, Vec::new());
    assert!(core.engine.registry[&stop_loss].status.is_live(), "precondition: adopted");

    list_act(&mut core, &next, ListAct::Cancel("sl"));
    assert_eq!(core.engine.client.cancels, vec![stop_loss], "the leg its tag names, and no other");
    assert!(core.engine.registry[&take_profit].status.is_live(), "the other leg rests on");
}

/// 28 — **A list naming a tag a restored, unjudged order holds is refused WHOLE** inside the window:
/// nothing reaches the venue client, every leg hears the restart's `Denied` under its own tag, and the
/// restored legs keep their tags and their collection rows.
#[test]
fn a_list_naming_a_held_tag_is_refused_whole() {
    use vike_venues::venue_restore::RESTORED_TAG_DENY_REASON;
    let dir = Scratch::reserved("vco-restore-list-held");
    let (take_profit, stop_loss, next, events) = rest_list(&dir);
    let mut core = restart_list(&dir, &next, &events);

    assert_eq!(list_act(&mut core, &next, ListAct::Place), 0, "nothing reaches the venue client");
    let denied = format!(
        "{:?}",
        vike_model::OrderEventKind::Denied { reason: RESTORED_TAG_DENY_REASON.to_string() }
    );
    assert_eq!(
        heard(&events),
        vec![(denied.clone(), Some("tp".to_string())), (denied, Some("sl".to_string()))],
        "one Denied per leg, each with its tag"
    );
    assert_eq!(
        leg_order_client_id(&core, "tp"),
        Some(take_profit),
        "the restored legs keep their tags"
    );
    assert_eq!(leg_order_client_id(&core, "sl"), Some(stop_loss));
    assert_eq!(core.restored_orders.len(), 2, "still unjudged");
}

// ---- The pass books OUR OWN order's fill that landed while the node was down (decisions 0043 and 0120, amended
// 2026-11-20; `crates/vike-core/src/runtime/reconcile.rs`'s `book_own_fills_filled_while_down`) ----

/// The restored journal's last record in these restarts: before [`fill_of`]'s stamp (5).
const JOURNAL_END: i64 = 4;

/// The downtime of a boot at `NOW` (the clock these cores read) whose restore folded a journal ending at
/// [`JOURNAL_END`] (`CoreConfig::restored_downtime_ms`).
const DOWNTIME: Option<(i64, i64)> = Some((JOURNAL_END, NOW));

/// [`restart`] at `NOW` on a boot whose restore folded a journal ending at `end` (`CoreConfig::restored_downtime_ms`;
/// `None`: no journal, or a fresh one after a deploy's journal move) and holding the book `restored`.
fn restart_after_journal(
    dir: &Scratch,
    nb: &NextQuote,
    events: &Events,
    end: Option<i64>,
    restored: Vec<vike_execution::EngineSnapshot>,
) -> Core {
    let mut config = open(config_on(dir, vec![spy_mount(nb, events)]));
    config.restored_downtime_ms = end.map(|end| (end, NOW));
    config.restored_engines = restored;
    assembled(config)
}

/// `(size, average price)` of the engine's open position, `(0, 0)` while flat (one symbol trades here).
fn open_position(core: &Core) -> (f64, f64) {
    core.engine
        .account
        .positions
        .values()
        .find(|p| p.size != 0.0)
        .map_or((0.0, 0.0), |p| (p.size, p.average_price))
}

/// The order client ids of the fills the held `MissingFill` rows propose.
fn held_fills(core: &Core) -> Vec<String> {
    core.reconcile_alerts
        .values()
        .filter(|a| a.kind == ReconcileDivergenceKind::MissingFill)
        .flat_map(|a| a.proposed_events.iter())
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.order_client_id.clone()),
            _ => None,
        })
        .collect()
}

/// 29 — **Our own order that rested through the stop and filled while the node was down is booked ONCE, at the
/// first pass, under `quarantine`**: the book takes the venue's fill (size, price, fee), the mount that placed it
/// hears it, and nothing is held for it. A second pass books nothing more, and neither does a restart from the
/// book that pass left (its ledger holds the trade, even with the same journal end).
#[test]
fn our_order_that_filled_while_down_is_booked_once_at_the_first_pass() {
    let dir = Scratch::reserved("vco-restore-book-own");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
    let fill = FillReport { commission: 0.05, ..fill_of(&order_client_id) };
    let booked = |core: &Core, when: &str| {
        assert_eq!(open_position(core), (1.0, 100.0), "{when}: the venue's buy of 1 at 100");
        assert!(
            (core.engine.account.fees_paid - 0.05).abs() < 1e-12,
            "{when}: its fee, once: {}",
            core.engine.account.fees_paid
        );
        assert!(held_fills(core).is_empty(), "{when}: nothing held: {:?}", alert_kinds(core));
    };

    run(&mut core, Vec::new(), vec![fill.clone()]);
    booked(&core, "the first pass");
    assert_eq!(core.mount_attribution[0].size, 1.0, "its mount heard the fill");
    assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1);
    assert!(noted(&core, "RECON booked 1 fill(s)"), "recent: {:?}", core.recent);

    run(&mut core, Vec::new(), vec![fill.clone()]);
    booked(&core, "a second pass");

    let book = core.engine.journal_snapshot();
    drop(core);
    let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), vec![book]);
    assert!(core.restored_orders.contains_key(&order_client_id), "the file still owns the order");
    run(&mut core, Vec::new(), vec![fill]);
    booked(&core, "the pass after a restart");
}

/// 30 — **Everything else stays HELD.** A foreign order's missing fill (an order client id the ownership file does
/// not know) is held beside our booked one. Our own fill is held, and nothing booked, when it is stamped before the
/// restored journal's last record (the previous process may have folded it, or missed it while it ran), and when the
/// boot stated no journal end: no journal, or a fresh one after a deploy's journal move, whose empty ledger reads
/// every older fill as missing.
#[test]
fn a_foreign_fill_and_ours_before_the_journal_end_or_after_a_journal_move_stay_held() {
    let dir = Scratch::reserved("vco-restore-book-own-foreign");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
    // Ours is booked at the first pass (the restore proves it); the stranger's is held on its second.
    run_held(&mut core, Vec::new(), vec![fill_of(&order_client_id), fill_of("stranger")]);
    assert_eq!(open_position(&core), (1.0, 100.0), "ours is booked, the stranger's is not");
    assert_eq!(held_fills(&core), vec!["stranger".to_string()]);

    for (n, end) in [(1, Some(fill_of("any").timestamp + 5)), (2, None)] {
        let dir = Scratch::reserved(&format!("vco-restore-book-own-held-{n}"));
        let (order_client_id, nb, events) = rest_one(&dir);
        let mut core = restart_after_journal(&dir, &nb, &events, end, Vec::new());
        run(&mut core, Vec::new(), vec![fill_of(&order_client_id)]);
        assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1, "end {end:?}");
        assert_eq!(open_position(&core), (0.0, 0.0), "end {end:?}: nothing booked");
        assert_eq!(held_fills(&core), vec![order_client_id], "end {end:?}: held for the operator");
    }
}

/// 31 — **An order of a mount not mounted yet is booked by the pass after its mount binds**, so the mount hears the
/// fill: the first pass holds it, as before, and the pass after the runtime mount of that id lands books it and
/// drops the row the first one held (the 2026-11-19 bybit maker mounted 18 s after the first pass).
#[test]
fn a_pending_mounts_fill_is_booked_by_the_pass_after_its_mount_binds() {
    let dir = Scratch::reserved("vco-restore-book-own-pending");
    let (next, events) = (cell(), Events::default());
    let order_client_id = rest_one_on_a_runtime_mount(&dir, &next, &events);
    let mut config = open(config_on(&dir, Vec::new()));
    config.strategy_factory = Some(spy_factory(&next, &events));
    config.restored_downtime_ms = DOWNTIME;
    let mut core = assembled(config);
    let fill = fill_of(&order_client_id);

    // No mount yet, so the restore does not spend it: an ordinary missed fill, held on its second pass.
    run_held(&mut core, Vec::new(), vec![fill.clone()]);
    assert_eq!(held_fills(&core), vec![order_client_id.clone()], "no mount yet: held");
    assert_eq!(open_position(&core), (0.0, 0.0));

    core.dispatch(mount_command(spec("rt-b")));
    run(&mut core, Vec::new(), vec![fill]);
    assert_eq!(open_position(&core), (1.0, 100.0), "booked");
    assert!(held_fills(&core).is_empty(), "the first pass's row went: {:?}", alert_kinds(&core));
    assert!(core.mount_attribution.iter().any(|m| m.size == 1.0), "its mount heard the fill");
}

/// A venue fill row of `order_client_id` under trade id `trade`: `side` `qty` at `price`, stamped `at`.
fn fill_row(
    order_client_id: &str,
    trade: &str,
    side: i32,
    qty: f64,
    price: f64,
    at: i64,
) -> FillReport {
    FillReport {
        trade_id: TradeId::new(trade).expect("a non-empty trade id"),
        side,
        last_qty: qty,
        last_price: price,
        timestamp: at,
        ..fill_of(order_client_id)
    }
}

/// 32 — **The pass books oldest first, as the stream would have**: bybit's `execution/list` lists newest first.
/// Three fills of two restored orders (buy 0.5 at 100, sell 0.5 at 110, buy 0.5 at 120) handed newest first leave
/// the book the live fold leaves, +5 and long 0.5 at 120, not -5 and long 0.5 at 100. The two orders are two
/// mounts' (`maker-b` buys, `maker-c` sells): one mount's `q` tag holds one live order.
#[test]
fn fills_booked_from_a_newest_first_history_fold_oldest_first() {
    let dir = Scratch::reserved("vco-restore-book-own-order");
    let (nb, nc, events) = (cell(), cell(), Events::default());
    let mounts = || {
        let seller =
            Mount { controller_id: Some("maker-c".to_string()), ..spy_mount(&nc, &events) };
        vec![spy_mount(&nb, &events), seller]
    };
    let mut first = assembled(config_on(&dir, mounts()));
    let buy = quote(&mut first, &nb, 1);
    let sell = quote(&mut first, &nc, -1);
    drop(first);
    assert_ne!(buy, sell, "two orders rest");
    let mut config = open(config_on(&dir, mounts()));
    config.restored_downtime_ms = DOWNTIME;
    let mut core = assembled(config);

    let newest_first = vec![
        fill_row(&buy, "t-3", 1, 0.5, 120.0, 7),
        fill_row(&sell, "t-2", -1, 0.5, 110.0, 6),
        fill_row(&buy, "t-1", 1, 0.5, 100.0, 5),
    ];
    run(&mut core, Vec::new(), newest_first);

    assert!(held_fills(&core).is_empty(), "all three booked: {:?}", alert_kinds(&core));
    assert_eq!(open_position(&core), (0.5, 120.0));
    let realized = core.engine.account.realized_pnl;
    assert!((realized - 5.0).abs() < 1e-9, "the live fold's PnL: {realized}");
}

/// A bare fill on `sim`/`BTCUSDT` naming no order: the long a restored sell then closes.
fn local_long(qty: f64, price: f64) -> Event {
    Event::Fill(vike_model::events::FillEvent {
        trade_id: TradeId::new("local-long").expect("a non-empty trade id"),
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_price: price,
        commission: 0.0,
        commission_asset: vike_marketdata::Symbol::new("USDT"),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
        mark_price: Some(price),
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// 33 — **The venue's whole cash figure lands ON TOP of the fills the pass folds, never under them**: it already
/// holds their fee and PnL. A restored sell closes a long 1 at 100 at 110 with a fee of 0.05, and the venue says
/// 10 009.95. With `reconcile_balance` off (the seed every pass writes) and on (the first sync), the book's cash is
/// that figure and its equity too (no fee or PnL counted twice), and the next pass raises no `BalanceDrift`. A
/// `hybrid` pass that folds the same fill itself takes the same order.
#[test]
fn the_venues_cash_lands_on_top_of_the_fills_the_pass_folds() {
    const VENUE_CASH: f64 = 10_009.95;
    for (n, mode, reconcile_balance, end) in [
        (1, ReconcileMode::Quarantine, false, Some(JOURNAL_END)),
        (2, ReconcileMode::Quarantine, true, Some(JOURNAL_END)),
        (3, ReconcileMode::Hybrid, false, None),
    ] {
        let dir = Scratch::reserved(&format!("vco-restore-book-own-cash-{n}"));
        let (nb, events) = (cell(), Events::default());
        let mut first = assembled(config_on(&dir, vec![spy_mount(&nb, &events)]));
        let sell = quote(&mut first, &nb, -1);
        drop(first);
        let mut core = restart_after_journal(&dir, &nb, &events, end, Vec::new());
        core.publish_to(0, local_long(1.0, 100.0));
        let pass = || ReconcileReports {
            balance: Some(VENUE_CASH),
            reconcile_balance,
            policy: ReconcilePolicy { default: mode, ..Default::default() },
            ..pass_for(
                "sim",
                None,
                Vec::new(),
                vec![FillReport {
                    commission: 0.05,
                    ..fill_row(&sell, "t-close", -1, 1.0, 110.0, 5)
                }],
            )
        };

        core.reconcile_reports(pass());
        core.dispatch_applied_fills();
        let account = &core.engine.account;
        assert_eq!(open_position(&core), (0.0, 0.0), "case {n}: the sell closed the long");
        assert!((account.realized_pnl - 10.0).abs() < 1e-9, "case {n}: {}", account.realized_pnl);
        assert!((account.balance - VENUE_CASH).abs() < 1e-9, "case {n}: cash {}", account.balance);
        let equity = account.equity_all(0.0);
        assert!((equity - VENUE_CASH).abs() < 1e-9, "case {n}: equity {equity}");

        core.reconcile_reports(pass());
        core.dispatch_applied_fills();
        let kinds = alert_kinds(&core);
        assert!(!kinds.contains(&ReconcileDivergenceKind::BalanceDrift), "case {n}: {kinds:?}");
        let equity = core.engine.account.equity_all(0.0);
        assert!(
            (equity - VENUE_CASH).abs() < 1e-9,
            "case {n}: equity after the next pass {equity}"
        );
    }
}

/// 34 — **A row stamped when it was READ is never booked by the pass, and the operator's confirm books it as it always
/// did.** IBKR's socket client stamps every fill row with the time of the read, which is after this boot, so the
/// stamp says nothing about whether the fill landed while the node was down. The pass holds the fill of our restored
/// order, and `recon confirm` of that row books it, also once the ledger's fill window holds a floor (a day back).
#[test]
fn a_fill_stamped_when_read_is_held_and_the_operators_confirm_books_it() {
    for (n, floor) in [(1, None), (2, Some(NOW - 86_400_000))] {
        let dir = Scratch::reserved(&format!("vco-restore-book-own-read-{n}"));
        let (order_client_id, nb, events) = rest_one(&dir);
        let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
        core.engine.account.adopt_fill_floor(floor);
        let read_ms = NOW + 1_000; // the pass read the row a second after the boot
        let row = FillReport { timestamp: read_ms, ..fill_of(&order_client_id) };

        run(&mut core, Vec::new(), vec![row]);

        assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1, "case {n}");
        assert_eq!(open_position(&core), (0.0, 0.0), "case {n}: the pass booked nothing");
        assert_eq!(held_fills(&core), vec![order_client_id.clone()], "case {n}: held");
        let id = core
            .reconcile_alerts
            .iter()
            .find(|(_, a)| a.kind == ReconcileDivergenceKind::MissingFill)
            .map(|(id, _)| *id)
            .expect("the held row");
        core.confirm_reconcile(id);
        core.dispatch_applied_fills();
        assert_eq!(
            open_position(&core),
            (1.0, 100.0),
            "case {n}: the operator's confirm booked it"
        );
        assert!(held_fills(&core).is_empty(), "case {n}: {:?}", alert_kinds(&core));
    }
}

/// 35 — **The production sequence of the 2026-11-20 the CI box deploy**: the profile mount is PARKED while its file builds
/// (`CoreConfig::mounts_awaiting_strategy`), the first pass holds its order's fill, the strategy arrives through
/// `AccountChange::AddMounts` (`add_mounts_hot`, the door production uses, not a runtime mount) and the next pass books
/// a fill stamped inside the window. the CI box's sell of `S000204gbqnkk7c0568OS` was stamped 5.6 s after the boot restore
/// read the clock and 11.7 s before the account's bybit adapter was up ([`PROD2_LINK_START`]): with that link start
/// known it is booked (case 3); with none known the window ends at the boot and it stays held at both passes, its order
/// spent all the same (case 2), as it was on 2026-11-20.
#[test]
fn a_parked_mounts_fill_is_booked_after_its_strategy_arrives_only_inside_the_window() {
    let parked = crate::strategy_state::mount_id_with(Some("maker-b"), "sim", "BTCUSDT", "1m");
    for (n, at, link, booked) in [
        (1, fill_of("any").timestamp, None, true),
        (2, PROD2_FILL, None, false),
        (3, PROD2_FILL, Some(PROD2_LINK_START), true),
    ] {
        let dir = Scratch::reserved(&format!("vco-restore-book-own-parked-{n}"));
        let (order_client_id, nb, events) = rest_one(&dir);
        let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
        config.mounts_awaiting_strategy = vec![parked.clone()];
        config.restored_downtime_ms = DOWNTIME;
        let mut core = assembled(config);
        core.engine.venue_link_started_ms = link;
        assert!(!core.mount_ids.contains(&parked), "case {n}: the mount is parked");
        let fill = FillReport { timestamp: at, ..fill_of(&order_client_id) };

        run_held(&mut core, Vec::new(), vec![fill.clone()]);
        assert_eq!(held_fills(&core), vec![order_client_id.clone()], "case {n}: parked: held");

        let mounts = Box::new(vec![spy_mount(&nb, &events)]);
        core.apply_account_change(vike_execution::AccountChange::AddMounts { mounts }, NOW);
        assert!(core.mount_ids.contains(&parked), "case {n}: the strategy arrived");
        run(&mut core, Vec::new(), vec![fill]);

        assert_eq!(
            core.restore_counters.restored_orders_filled_while_down, 1,
            "case {n}: spent once"
        );
        if booked {
            assert_eq!(open_position(&core), (1.0, 100.0), "case {n}: booked");
            assert!(held_fills(&core).is_empty(), "case {n}: {:?}", alert_kinds(&core));
            assert!(core.mount_attribution.iter().any(|m| m.size == 1.0), "its mount heard it");
        } else {
            assert_eq!(open_position(&core), (0.0, 0.0), "case {n}: after the boot: not booked");
            assert_eq!(held_fills(&core), vec![order_client_id], "case {n}: held");
        }
    }
}

/// the CI box's sell of 2026-11-20 (09:28:36.138 UTC), on the clock of these cores: 5.567 s after the boot restore's
/// (09:28:30.571).
const PROD2_FILL: i64 = NOW + 5_567;

/// When the 2026-11-20 boot started bybit DEMO's venue link (09:28:47.8, the first `TRADE-OPEN` line), on the same
/// clock: 17.229 s after the boot, 11.662 s after [`PROD2_FILL`].
const PROD2_LINK_START: i64 = NOW + 17_229;

/// 36 — **The window ends at the account's venue link start, not at the boot** (the owner's second ruling of
/// 2026-11-20): our own order's fill stamped after the boot and before the account's link to its venue was started
/// is booked by the first pass, its mount hears it and nothing is held.
#[test]
fn a_fill_after_the_boot_before_the_accounts_link_start_is_booked() {
    let dir = Scratch::reserved("vco-restore-book-own-link");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
    core.engine.venue_link_started_ms = Some(PROD2_LINK_START);

    run(
        &mut core,
        Vec::new(),
        vec![FillReport { timestamp: PROD2_FILL, ..fill_of(&order_client_id) }],
    );

    assert_eq!(open_position(&core), (1.0, 100.0), "booked");
    assert!(held_fills(&core).is_empty(), "nothing held: {:?}", alert_kinds(&core));
    assert_eq!(core.mount_attribution[0].size, 1.0, "its mount heard the fill");
}

/// 37 — **A fill stamped at or after the account's link start is HELD**: it may have come over that link and been
/// missed while the node ran, which is no fill that landed while the node was down. Three partial fills of one
/// restored order, a millisecond before, at and after the start: only the first is booked.
#[test]
fn a_fill_at_or_after_the_accounts_link_start_is_held() {
    let dir = Scratch::reserved("vco-restore-book-own-link-edge");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
    let start = PROD2_LINK_START;
    core.engine.venue_link_started_ms = Some(start);

    run(
        &mut core,
        Vec::new(),
        vec![
            fill_row(&order_client_id, "t-before", 1, 0.5, 100.0, start - 1),
            fill_row(&order_client_id, "t-at", 1, 0.25, 100.0, start),
            fill_row(&order_client_id, "t-after", 1, 0.25, 100.0, start + 1),
        ],
    );

    assert_eq!(open_position(&core), (0.5, 100.0), "only the fill before the link start");
    assert_eq!(held_fills(&core), vec![order_client_id.clone(), order_client_id], "two held");
}

/// 38 — **A row stamped when it was READ stays held under the link-start bound too.** IBKR's socket client stamps a
/// fill row with `vike_model::now_ms()` at the pass's fetch, and a pass reads only once the account's engine exists,
/// so after the instant its venue link was started, on one clock (`vike_tradehub_core`'s `make_engine_for_account`
/// takes it before it asks for the adapter; `crates/vike-tradehub-core/src/backend_tests.rs`'s
/// `the_venue_link_start_is_taken_before_the_adapter_start_is_asked` pins that order). Read at the start itself or a
/// minute later, it is held for the operator. A gatewayhub on another host whose clock runs behind is the declared
/// residual (`book_own_fills_filled_while_down`'s doc).
#[test]
fn a_fill_stamped_when_read_after_the_link_start_is_held() {
    for (n, read_ms) in [(1, PROD2_LINK_START), (2, PROD2_LINK_START + 60_000)] {
        let dir = Scratch::reserved(&format!("vco-restore-book-own-link-read-{n}"));
        let (order_client_id, nb, events) = rest_one(&dir);
        let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
        core.engine.venue_link_started_ms = Some(PROD2_LINK_START);

        run(
            &mut core,
            Vec::new(),
            vec![FillReport { timestamp: read_ms, ..fill_of(&order_client_id) }],
        );

        assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1, "case {n}");
        assert_eq!(open_position(&core), (0.0, 0.0), "case {n}: the pass booked nothing");
        assert_eq!(held_fills(&core), vec![order_client_id], "case {n}: held");
    }
}

/// The two `binance` accounts of [`two_accounts`] with their venue links started at boot + `default_s` and
/// boot + `alt_s` on the books `default_book` and `alt_book` (`None`: the table could not tell), the boot's restore
/// having folded the journal through [`JOURNAL_END`]; returns the order client ids `(default, alt)` and the core.
fn two_linked_accounts(
    dir: &Scratch,
    (default_s, default_book): (i64, Option<&str>),
    (alt_s, alt_book): (i64, Option<&str>),
) -> ((String, String), Core) {
    let (_ed, _ea, order_client_ids, mut core) = two_accounts(dir);
    core.config.restored_downtime_ms = DOWNTIME;
    let book = |b: Option<&str>| b.and_then(|b| vike_model::BookId::new("binance", b));
    core.engine.venue_link_started_ms = Some(NOW + default_s * 1_000);
    core.engine.venue_book = book(default_book);
    core.extra_engines[0].1.venue_link_started_ms = Some(NOW + alt_s * 1_000);
    core.extra_engines[0].1.venue_book = book(alt_book);
    (order_client_ids, core)
}

/// One `binance` pass of the account `route_key` reporting `order_client_id`'s fill, stamped at boot + `at_s`.
fn binance_fill_pass(core: &mut Core, route_key: &str, order_client_id: &str, at_s: i64) {
    let row = FillReport {
        venue: "binance".into(),
        timestamp: NOW + at_s * 1_000,
        ..fill_of(order_client_id)
    };
    core.reconcile_reports(pass_for("binance", Some(route_key), Vec::new(), vec![row]));
    core.dispatch_applied_fills();
}

/// 39 — **Each account ends its window at ITS OWN link start.** Two accounts of `binance` on two books, the default
/// started at boot + 10 s and `ALT` at boot + 30 s, each with a restored order that filled at boot + 20 s: `ALT`'s
/// fill is booked on `ALT`'s engine, the default's is held (it came after that account's link start).
#[test]
fn two_accounts_each_end_the_window_at_their_own_link_start() {
    let dir = Scratch::reserved("vco-restore-book-own-two-links");
    let ((default_order_client_id, alt_order_client_id), mut core) =
        two_linked_accounts(&dir, (10, Some("book-default")), (30, Some("book-alt")));

    binance_fill_pass(&mut core, "binance", &default_order_client_id, 20);
    binance_fill_pass(&mut core, "binance#ALT", &alt_order_client_id, 20);

    assert_eq!(open_position(&core), (0.0, 0.0), "the default account booked nothing");
    assert_eq!(held_fills(&core), vec![default_order_client_id], "the default's fill is held");
    let alt = core.extra_engines[0].1.account.positions.values().find(|p| p.size != 0.0);
    assert_eq!(alt.map(|p| (p.size, p.average_price)), Some((1.0, 100.0)), "ALT booked its own");
    assert!(core.mount_attribution.iter().any(|m| m.size == 1.0), "ALT's mount heard the fill");
}

/// 40 — **A SHARED BOOK ends the window at the earliest link start on that book, so a fill is never booked twice.**
/// Two labels on ONE venue account (the shared-book warning's case, which starts both): the default `A` linked at
/// boot + 10 s, `ALT` (`B`) inactive at the boot and started three hours later. `B`'s restored order fills an hour
/// after the boot; `A`'s link carried it and `A`'s engine folded it, so `B`'s first pass finds it missing from `B`'s
/// own ledger. That pass holds it while the two books are the same or either cannot be told (a venue whose
/// credentials name no account, with no recorded book), and books it only when the books are known to differ.
#[test]
fn a_shared_books_window_ends_at_the_earliest_link_start_on_that_book() {
    for (n, default_book, alt_book, booked) in [
        (1, Some("one-wallet"), Some("one-wallet"), false),
        (2, Some("one-wallet"), None, false),
        (3, None, Some("one-wallet"), false),
        (4, Some("one-wallet"), Some("another-wallet"), true),
    ] {
        let dir = Scratch::reserved(&format!("vco-restore-book-own-shared-{n}"));
        let ((_, alt_order_client_id), mut core) =
            two_linked_accounts(&dir, (10, default_book), (3 * 3_600, alt_book));

        binance_fill_pass(&mut core, "binance#ALT", &alt_order_client_id, 3_600);

        let alt = core.extra_engines[0].1.account.positions.values().find(|p| p.size != 0.0);
        if booked {
            assert_eq!(alt.map(|p| p.size), Some(1.0), "case {n}: another book: booked");
            assert!(held_fills(&core).is_empty(), "case {n}: {:?}", alert_kinds(&core));
        } else {
            assert!(alt.is_none(), "case {n}: B booked nothing");
            assert_eq!(held_fills(&core), vec![alt_order_client_id], "case {n}: held");
        }
    }
}

/// 41 — **The FIRST link start of the account in this process bounds the window; a later one never moves it.** A
/// stopped engine of the same account (its link started at boot + 10 s) beside the running one (started again at
/// boot + 30 s): a fill at boot + 20 s is held, as the first start says.
#[test]
fn a_later_link_start_of_the_same_account_does_not_move_the_window() {
    let dir = Scratch::reserved("vco-restore-book-own-first-link");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
    core.engine.venue_link_started_ms = Some(NOW + 30_000);
    let mut stopped = Engine::new(
        Account::new(10_000.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    stopped.venue_link_started_ms = Some(NOW + 10_000);
    core.extra_engines.push((10_000.0, stopped));
    core.retired_engines.push(1);

    core.reconcile_reports(pass_for(
        "sim",
        Some("sim"),
        Vec::new(),
        vec![FillReport { timestamp: NOW + 20_000, ..fill_of(&order_client_id) }],
    ));
    core.dispatch_applied_fills();

    assert_eq!(
        core.restore_counters.restored_orders_filled_while_down, 1,
        "the running engine's pass"
    );
    assert_eq!(open_position(&core), (0.0, 0.0), "after the first link start: not booked");
    assert_eq!(held_fills(&core), vec![order_client_id], "held");
}

/// 42 — **The pass that books our fill judges the position AFTER the booking** (bybit#DEMO, 2026-11-22 02:34:39: the
/// pass that booked one fill of 0.001 also held a `PositionDrift` of `venue_qty=0.249 local_qty=0.248`, measured
/// before the booking, and the next pass healed it). The book holds `0.1 + 0.2` (its float residue,
/// `0.30000000000000004`), the venue `0.4`: the first pass after the restart reads the position with our fill in it but
/// no fill row yet, so the drift is pending; the second books the fill the restore found filled while down and raises
/// the drift a second time. Nothing is held, and a third pass holds and books nothing.
#[test]
fn a_restart_pass_that_books_our_fill_holds_no_drift_of_its_size() {
    let dir = Scratch::reserved("vco-restore-book-own-drift");
    let (order_client_id, nb, events) = rest_one(&dir);
    let mut core = restart_after_journal(&dir, &nb, &events, Some(JOURNAL_END), Vec::new());
    for (trade, qty) in [("t-open-1", 0.1), ("t-open-2", 0.2)] {
        core.publish_to(
            0,
            Event::Fill(vike_model::events::FillEvent {
                trade_id: TradeId::new(trade).expect("a non-empty trade id"),
                order_client_id: String::new(),
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                last_qty: qty,
                last_price: 100.0,
                commission: 0.0,
                commission_asset: vike_marketdata::Symbol::new(""),
                liquidity_side: LiquiditySide::Taker,
                timestamp: 1,
                mark_price: None,
                position_side: vike_model::events::PositionSide::Both,
            }),
        );
    }
    assert_eq!(open_position(&core).0, 0.1 + 0.2, "PREMISE: the book's own residue");
    let venue = vike_model::PositionStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        qty: 0.4,
        average_price: 100.0,
        timestamp: 5,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };
    let fill = fill_row(&order_client_id, "t-missed", 1, 0.1, 100.0, 5);
    let pass = |core: &mut Core, fills: Vec<FillReport>| {
        core.reconcile_reports(ReconcileReports {
            positions: vec![venue.clone()],
            ..pass_for("sim", None, Vec::new(), fills)
        });
        core.dispatch_applied_fills();
    };

    pass(&mut core, Vec::new());
    assert!(alert_kinds(&core).is_empty(), "one pass is no proof: {:?}", alert_kinds(&core));

    pass(&mut core, vec![fill.clone()]);
    assert!(noted(&core, "RECON booked 1 fill(s)"), "recent: {:?}", core.recent);
    assert!((open_position(&core).0 - 0.4).abs() < 1e-12, "booked: {:?}", open_position(&core));
    assert!(alert_kinds(&core).is_empty(), "no drift of the booked size: {:?}", alert_kinds(&core));

    pass(&mut core, vec![fill]);
    assert!(alert_kinds(&core).is_empty(), "{:?}", alert_kinds(&core));
    assert_eq!(core.recent.iter().filter(|l| l.contains("RECON booked")).count(), 1, "booked once");
}

// ---- THE SHUTDOWN SEAM (the owner's "yes" of 2026-11-22, decisions 0043 and 0120 amended): our restored resting
// order's fill the restored ledger lacks is booked on its second pass in a row, whatever its stamp
// (`crates/vike-core/src/runtime/reconcile.rs`'s `book_own_fills_filled_while_down`) ----

/// The restored journal's last record in the seam's restarts (the old tradehub's shutdown `Snapshot`), a minute before
/// this boot (`CoreConfig::restored_downtime_ms`'s first half).
const SEAM_JOURNAL_END: i64 = NOW - 60_000;

/// the CI box's fill of `S000204gbqnkk7c0jcnFS` on bybit#DEMO (venue time 09:25:19.548), on the clock of these cores: 401 ms
/// BEFORE the restored journal's last record (09:25:19.949), so outside the down window.
const SEAM_FILL: i64 = SEAM_JOURNAL_END - 401;

/// The trade id bybit gave that fill.
const SEAM_TRADE: &str = "18758aca-7873-41e8-9181-4a1f75294bbc";

/// A core on `venue`/`BTCUSDT`, the venue's one account, whose mount `maker-b` quotes through `next` over the ownership
/// file in `dir`; `restored`: the gate open and a restore that folded a journal ending at [`SEAM_JOURNAL_END`].
fn seam_boot(
    dir: &Scratch,
    venue: &str,
    next: &NextQuote,
    events: &Events,
    restored: bool,
) -> Core {
    let mut mount = spy_mount(next, events);
    mount.venue = venue.into();
    let mut config = config_on(dir, vec![mount]);
    if restored {
        config = open(config);
        config.restored_downtime_ms = Some((SEAM_JOURNAL_END, NOW));
    }
    let mut engine = Engine::new(
        Account::new(10_000.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        "BTCUSDT",
    );
    engine.collect_applied_fills = true;
    core_of(engine, Vec::new(), config)
}

/// Tell `core`'s mount to quote a `q` buy of 1 at 100 on `venue`, and return the order client id it was given.
fn quote_on(core: &mut Core, next: &NextQuote, venue: &str) -> String {
    *next.lock().unwrap() = Some(1);
    core.drive_strategy_feed_status(venue, "BTCUSDT", FeedStatus::Live);
    core.engine.client.submissions.last().expect("the mount quoted").order_client_id.clone()
}

/// Boot 1 on `venue` rests a `q` buy and dies; boot 2 restores it over the same file ([`seam_boot`]). Returns the
/// order client id and boot 2.
fn rest_one_on(dir: &Scratch, venue: &str) -> (String, Core) {
    let (next, events) = (cell(), Events::default());
    let mut first = seam_boot(dir, venue, &next, &events, false);
    let order_client_id = quote_on(&mut first, &next, venue);
    drop(first);
    (order_client_id, seam_boot(dir, venue, &next, &events, true))
}

/// The seam row on `venue`: `order_client_id`'s buy of 1 at 100 under [`SEAM_TRADE`], stamped [`SEAM_FILL`].
fn seam_row(venue: &str, order_client_id: &str) -> FillReport {
    FillReport {
        venue: venue.into(),
        trade_id: TradeId::new(SEAM_TRADE).expect("a non-empty trade id"),
        timestamp: SEAM_FILL,
        ..fill_of(order_client_id)
    }
}

/// The same execution as a ledger that folded it holds: [`SEAM_TRADE`], a buy of 1 at 100 on `venue`.
fn seam_fold(venue: &str) -> Event {
    Event::Fill(vike_model::events::FillEvent {
        trade_id: TradeId::new(SEAM_TRADE).expect("a non-empty trade id"),
        order_client_id: String::new(),
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: vike_marketdata::Symbol::new("USDT"),
        liquidity_side: LiquiditySide::Taker,
        timestamp: SEAM_FILL,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// One pass of `venue`'s one account reporting `fills`, delivered.
fn venue_pass(core: &mut Core, venue: &str, fills: Vec<FillReport>) {
    core.reconcile_reports(pass_for(venue, None, Vec::new(), fills));
    core.dispatch_applied_fills();
}

/// How many passes wrote the booking line.
fn bookings(core: &Core) -> usize {
    core.recent.iter().filter(|l| l.contains("RECON booked")).count()
}

/// 43 — **THE 2026-11-22 SEAM: our restored resting order's fill stamped BEFORE the restored journal's last record is
/// booked on its SECOND pass in a row.** the CI box's buy of `S000204gbqnkk7c0jcnFS` on bybit#DEMO filled 401 ms before the
/// old tradehub's shutdown `Snapshot`, which still listed the order resting, and its ledger lacked the trade: outside
/// the down window and in no registry, so it was held for an operator. On a venue whose stream and history share the
/// trade id, the first pass holds it as before (the restore's proof alone books only inside the window), the second
/// books it, drops the row the first held and tells the mount, and a third books nothing more.
#[test]
fn our_restored_orders_fill_at_the_shutdown_seam_is_booked_on_the_second_pass() {
    let dir = Scratch::reserved("vco-restore-seam");
    let (order_client_id, mut core) = rest_one_on(&dir, "bybit");
    let row = seam_row("bybit", &order_client_id);

    venue_pass(&mut core, "bybit", vec![row.clone()]);
    assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1, "the restore spent it");
    assert_eq!(open_position(&core), (0.0, 0.0), "the first pass books nothing");
    assert_eq!(held_fills(&core), vec![order_client_id.clone()], "held, as before");

    venue_pass(&mut core, "bybit", vec![row.clone()]);
    assert_eq!(open_position(&core), (1.0, 100.0), "booked on the second pass");
    assert!(held_fills(&core).is_empty(), "the first pass's row went: {:?}", alert_kinds(&core));
    assert_eq!(core.mount_attribution[0].size, 1.0, "its mount heard the fill");
    assert!(
        noted(&core, "and 1 of an order the restored state held resting"),
        "recent: {:?}",
        core.recent
    );

    venue_pass(&mut core, "bybit", vec![row]);
    assert_eq!(open_position(&core), (1.0, 100.0), "booked once");
    assert_eq!(bookings(&core), 1, "booked once");
}

/// 44 — **A trade id a ledger on the account's book already holds is never booked again.** The old process folded the
/// seam fill, so the restored ledger holds the trade: the passes raise nothing and the book stays long 1. On a SHARED
/// BOOK (two `binance` labels on one venue account) the other label's engine folded it: the label whose order it was
/// holds the row, as before, and books nothing.
#[test]
fn a_seam_fill_a_ledger_on_the_book_holds_is_not_booked_again() {
    let dir = Scratch::reserved("vco-restore-seam-folded");
    let (order_client_id, mut core) = rest_one_on(&dir, "bybit");
    core.publish_to(0, seam_fold("bybit"));
    for _ in 0..3 {
        venue_pass(&mut core, "bybit", vec![seam_row("bybit", &order_client_id)]);
    }
    assert_eq!(open_position(&core), (1.0, 100.0), "the restored ledger's fill, once");
    assert!(held_fills(&core).is_empty(), "{:?}", alert_kinds(&core));
    assert_eq!(bookings(&core), 0);

    let dir = Scratch::reserved("vco-restore-seam-shared");
    let ((_, alt_order_client_id), mut core) =
        two_linked_accounts(&dir, (10, Some("one-wallet")), (30, Some("one-wallet")));
    core.config.restored_downtime_ms = Some((SEAM_JOURNAL_END, NOW));
    core.publish_to(0, seam_fold("binance"));
    for _ in 0..3 {
        let row = seam_row("binance", &alt_order_client_id);
        core.reconcile_reports(pass_for("binance", Some("binance#ALT"), Vec::new(), vec![row]));
        core.dispatch_applied_fills();
    }
    let alt = core.extra_engines[0].1.account.positions.values().find(|p| p.size != 0.0);
    assert!(alt.is_none(), "ALT booked nothing");
    assert_eq!(held_fills(&core), vec![alt_order_client_id], "held, as before");
    assert_eq!(bookings(&core), 0);
}

/// 45 — **A fill before the window of an order the restore did not find resting stays HELD**, stamped at the seam and
/// seen on every pass: a stranger's order (the ownership file does not know it), and an order this process placed
/// (in its registry, minted by its mount, never restored).
#[test]
fn a_seam_fill_of_an_order_not_restored_resting_stays_held() {
    let dir = Scratch::reserved("vco-restore-seam-not-restored");
    let (next, events) = (cell(), Events::default());
    let mut core = seam_boot(&dir, "bybit", &next, &events, true);
    assert!(core.restored_orders.is_empty(), "PREMISE: a first boot restores nothing");
    let placed = quote_on(&mut core, &next, "bybit");
    let rows = vec![
        FillReport {
            trade_id: TradeId::new("t-placed").expect("a non-empty trade id"),
            ..seam_row("bybit", &placed)
        },
        seam_row("bybit", "stranger"),
    ];
    for _ in 0..3 {
        venue_pass(&mut core, "bybit", rows.clone());
    }
    assert_eq!(open_position(&core), (0.0, 0.0), "nothing booked");
    let mut held = held_fills(&core);
    held.sort_unstable();
    let mut expected = vec![placed, "stranger".to_string()];
    expected.sort_unstable();
    assert_eq!(held, expected, "both held for the operator");
    assert_eq!(bookings(&core), 0);
}

/// 46 — **A venue whose stream and history do not name one fill by one trade id keeps the seam row HELD**, however many
/// passes see it: IBKR's rows carry the time of the READ, and `sim` is no roster venue (an unknown venue fails
/// closed, `vike_venues::venue_fill_identity::stream_and_history_share_the_trade_id`).
#[test]
fn a_seam_fill_on_a_venue_without_one_trade_id_stays_held() {
    for venue in ["ibkr", "sim"] {
        let dir = Scratch::reserved(&format!("vco-restore-seam-unproven-{venue}"));
        let (order_client_id, mut core) = rest_one_on(&dir, venue);
        for _ in 0..3 {
            venue_pass(&mut core, venue, vec![seam_row(venue, &order_client_id)]);
        }
        assert_eq!(core.restore_counters.restored_orders_filled_while_down, 1, "{venue}: spent");
        assert_eq!(open_position(&core), (0.0, 0.0), "{venue}: nothing booked");
        assert_eq!(held_fills(&core), vec![order_client_id], "{venue}: held");
        assert_eq!(bookings(&core), 0, "{venue}");
    }
}
