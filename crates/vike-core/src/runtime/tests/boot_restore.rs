//! The ASSEMBLY half of the boot restore (`crate::runtime::boot_restore`, decision 0127), white-box:
//! a core assembled over what `crate::replay::BootRestore::apply_to` would put on its config. The
//! engine snapshots are built by hand from engines the test folds itself, so each case states the
//! journal it stands for.

use super::order_owners_tests::{Events, assembled, cell, config_on, noted, spy_mount};
use super::restore_tests::{fill_of, heard, open, report, rest_one, restart, run};
use super::*;
use crate::order_owners::ORDER_OWNERS_FILE;
use crate::replay::BootRestore;
use crate::runtime::test_support::{core_of, engine_on};
use crate::scratch::Scratch;
use vike_exec::recon::{BalanceTol, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_model::events::{OrderAccepted, OrderFilled, OrderPartiallyFilled, OrderSubmitted};
use vike_model::{FillReport, OrderStatus, OrderStatusReport};

type Core = CoreThread<RecordingClient>;

/// An engine on `sim`/`BTCUSDT` routed as `route_key` (the venue for the default account).
fn engine(route_key: &str) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_on("sim", "BTCUSDT", RecordingClient::default());
    e.route_key = route_key.to_string();
    e
}

/// The `Snap` entry of `route_key`'s engine after it folded a `size` position, one fill id and a
/// trading state — what a journal of that engine carries.
fn journaled(route_key: &str, size: f64, state: TradingState) -> vike_exec::EngineSnapshot {
    let mut e = engine(route_key);
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
        vike_exec::PositionEntry { size, avg_px: 100.0, ..Default::default() },
    );
    e.seed_seen_trade_ids([format!("t-{route_key}")]);
    e.trading_state = state;
    e.snapshot_state()
}

/// A two-account core (`sim` and `sim#ALT`) assembled over `restored`.
fn two_accounts(restored: Vec<vike_exec::EngineSnapshot>) -> Core {
    core_of(
        engine("sim"),
        vec![(1_000.0, engine("sim#ALT"))],
        CoreConfig { seed_cash: 1_000.0, restored_engines: restored, ..CoreConfig::default() },
    )
}

/// **Each journaled engine goes back onto the engine with ITS route key** — positions and fill
/// dedup together — whatever order the snapshots come in.
#[test]
fn a_multi_engine_journal_restores_each_engine_by_route_key() {
    let core = two_accounts(vec![
        journaled("sim#ALT", -2.0, TradingState::Active),
        journaled("sim", 3.0, TradingState::Active),
    ]);
    assert_eq!(
        core.engine.position_size(vike_model::events::PositionSide::Both),
        3.0,
        "the default account's position"
    );
    assert_eq!(
        core.extra_engines[0].1.position_size(vike_model::events::PositionSide::Both),
        -2.0,
        "ALT's, not the default's"
    );
    assert!(core.engine.seen_trade_ids().any(|t| t == "t-sim"), "with its own dedup");
    assert!(core.extra_engines[0].1.seen_trade_ids().any(|t| t == "t-sim#ALT"));
    assert!(noted(&core, "put back on 2 of 2"), "recent {:?}", core.recent);
}

/// **Two journaled engines of one venue without a route key (a v16 `Snap`) restore NEITHER**: which
/// is which is unknowable, and a guess books one account's position onto the other.
#[test]
fn a_snapshot_without_route_keys_on_a_two_account_venue_restores_neither_engine() {
    // Both snapshots read back `route_key: None`, the shape every pre-v17 `Snap` has.
    let mut a = journaled("sim", 3.0, TradingState::Active);
    let mut b = journaled("sim#ALT", -2.0, TradingState::Active);
    a.route_key = None;
    b.route_key = None;
    let core = two_accounts(vec![a, b]);
    assert_eq!(core.engine.position_size(vike_model::events::PositionSide::Both), 0.0);
    assert_eq!(core.extra_engines[0].1.position_size(vike_model::events::PositionSide::Both), 0.0);
    assert!(noted(&core, "RESTORE ENGINE SKIPPED sim: 2 journaled engines"), "{:?}", core.recent);
    assert!(noted(&core, "put back on 0 of 2"));
}

/// **A journaled engine no engine of this core matches is skipped and said**, and the rest apply.
#[test]
fn an_unmatched_snapshot_is_skipped_and_the_others_apply() {
    let core = two_accounts(vec![
        journaled("sim", 1.0, TradingState::Active),
        journaled("sim#GONE", 5.0, TradingState::Active),
    ]);
    assert_eq!(core.engine.position_size(vike_model::events::PositionSide::Both), 1.0);
    assert_eq!(
        core.extra_engines[0].1.position_size(vike_model::events::PositionSide::Both),
        0.0,
        "ALT had no snapshot"
    );
    assert!(noted(&core, "RESTORE ENGINE SKIPPED sim#GONE"), "{:?}", core.recent);
}

/// **The settings' limits stay**: a journaled `RiskLimits` is never applied (decisions 0086, 0111).
#[test]
fn restored_limits_never_override_the_settings() {
    let mut journal_side = engine("sim");
    journal_side.gate.limits.max_notional_per_order = Some(1.0);
    let snap = journal_side.snapshot_state();
    let mut live = engine("sim");
    live.gate.limits.max_notional_per_order = Some(50_000.0);
    let core = core_of(
        live,
        Vec::new(),
        CoreConfig { restored_engines: vec![snap], ..CoreConfig::default() },
    );
    assert_eq!(core.engine.gate.limits.max_notional_per_order, Some(50_000.0));
}

/// **A journaled trading state applies only when STRICTER** than the boot's: a latched `Reducing`
/// survives the restart, a journaled `Active` does not loosen a boot `Halted`.
#[test]
fn a_stricter_restored_trading_state_is_kept_and_a_looser_one_is_not() {
    let core = core_of(
        engine("sim"),
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled("sim", 0.0, TradingState::Reducing)],
            ..CoreConfig::default()
        },
    );
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "the latch survives");
    assert!(noted(&core, "RESTORE TRADING STATE sim: Reducing"), "{:?}", core.recent);

    let mut halted = engine("sim");
    halted.trading_state = TradingState::Halted;
    let core = core_of(
        halted,
        Vec::new(),
        CoreConfig {
            restored_engines: vec![journaled("sim", 0.0, TradingState::Active)],
            ..CoreConfig::default()
        },
    );
    assert_eq!(core.engine.trading_state, TradingState::Halted, "never loosened");
    assert!(!noted(&core, "RESTORE TRADING STATE"));
}

/// **The journal off is today's core**: `BootRestore::off()` writes nothing onto a config, and a
/// core assembled over it carries no restore line.
#[test]
fn a_journal_off_core_is_byte_identical() {
    let mut config = CoreConfig::default();
    BootRestore::off().apply_to(&mut config);
    assert!(config.coid_session.is_none());
    assert_eq!(config.arm_seq, 0);
    assert!(config.conditionals.is_empty() && config.contingencies.is_empty());
    assert!(config.restored_engines.is_empty() && config.journal_orders.is_empty());
    assert!(config.mount_attr.is_empty() && config.conditional_owners.is_empty());
    assert!(config.lost_held_exits.is_empty() && config.restore_notes.is_empty());
    assert!(!config.restored_paper_orders_gone, "the paper rule is the ROOT's statement");
    let core = core_of(engine("sim"), Vec::new(), config);
    assert!(core.recent.is_empty(), "no line: {:?}", core.recent);
}
// --- the restore at the PASS and at boot (decision 0127): the journal's price at adoption, a paper
// --- book, and a held exit a broken journal lost. Driven like `restore_tests`, over its helpers.

/// **A journal candidate is adopted with its journaled price.** The venue's report carries no
/// price; the candidate the journal supplied does, so the registry entry the adoption rebuilds rests
/// at it (decision 0120's verdict 8, amended for a box with a journal).
#[test]
fn a_journal_candidate_is_adopted_with_its_journaled_price() {
    let dir = Scratch::reserved("vco-restore-journal-price");
    let (nb, events) = (cell(), Events::default());
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.journal_orders = vec![crate::replay::JournalOrder {
        coid: "j-px".into(),
        mount_id: "maker_b".into(),
        venue: Some("sim".into()),
        symbol: Some("BTCUSDT".into()),
        account: None,
        price: Some(101.25),
    }];
    let mut core = assembled(config);
    assert!(core.restored_orders.contains_key("j-px"), "precondition: a restored entry");

    run(&mut core, vec![report("j-px", vike_model::OrderStatus::Accepted)], Vec::new());

    let order = core.engine.registry.get("j-px").expect("adopted");
    assert_eq!(order.request.price, Some(101.25), "the journal's price, which no report carries");
    assert_eq!(core.restore_counters.restored_adopted, 1);
    assert_eq!(heard(&events), vec![("Accepted".to_string(), None)], "its mount is told");
}

/// **A restored order on a PAPER book is gone at boot** under
/// `CoreConfig::restored_paper_orders_gone`: the book lived in the dead process's memory and no
/// pass will ever judge it. The mount hears `Canceled` with the paper reason and its tag, the
/// entry is spent and counted, and the file forgets it.
#[test]
fn a_paper_engine_restored_order_is_gone_at_boot() {
    let dir = Scratch::reserved("vco-restore-paper-gone");
    let (coid, nb, events) = rest_one(&dir);
    let mut config = config_on(&dir, vec![spy_mount(&nb, &events)]);
    config.restored_paper_orders_gone = true;
    let mut core = assembled(config);
    assert_eq!(core.engine.tier, vike_model::AccountTier::Paper, "precondition: a paper engine");
    assert!(core.restored_orders.is_empty(), "nothing waits for a pass");
    assert_eq!(core.restore_counters.restored_gone, 1);
    assert!(noted(&core, "RESTORE PAPER BOOK"), "recent {:?}", core.recent);

    core.dispatch_applied_fills();
    let canceled = vike_model::OrderEventKind::Canceled {
        reason: crate::replay::RESTORE_PAPER_BOOK_LOST_REASON.to_string(),
    };
    assert_eq!(heard(&events), vec![(format!("{canceled:?}"), Some("q".to_string()))]);
    drop(core);
    let text = std::fs::read_to_string(dir.join(ORDER_OWNERS_FILE)).unwrap();
    assert!(text.contains("\"forget\"") && text.contains(&coid), "the file forgets it: {text}");
}

/// **A held exit a broken journal lost is a terminal reject its mount hears**: nothing holds
/// or releases it any more, so it may not vanish in silence. The owner comes from the ownership
/// file; the entry is spent, so no pass can tell the mount a second time.
#[test]
fn a_lost_held_exit_tells_its_mount_with_a_terminal_reject() {
    let dir = Scratch::reserved("vco-restore-lost-held-exit");
    let (coid, nb, events) = rest_one(&dir);
    let mut config = open(config_on(&dir, vec![spy_mount(&nb, &events)]));
    config.lost_held_exits = vec![crate::replay::LostHeldExit {
        coid: coid.clone(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
    }];
    let mut core = assembled(config);
    assert!(!core.restored_orders.contains_key(&coid), "spent at boot");

    core.dispatch_applied_fills();
    let rejected = vike_model::OrderEventKind::Rejected {
        reason: crate::replay::RESTORE_LOST_REASON.to_string(),
    };
    assert_eq!(heard(&events), vec![(format!("{rejected:?}"), Some("q".to_string()))]);
    run(&mut core, Vec::new(), Vec::new());
    assert_eq!(heard(&events).len(), 1, "a later pass tells it nothing more");
}

/// The note sample both restore notes print (the boot notes and the reconcile pass's RESTORE
/// note): the first eight coids joined by `, `, then ` (N more)`; nothing at all for an empty list.
#[test]
fn a_restore_note_lists_eight_coids_then_counts_the_rest() {
    let coids = |n: usize| (0..n).map(|i| format!("c{i}")).collect::<Vec<String>>();
    assert_eq!(super::boot_restore::sample(&coids(0)), "");
    assert_eq!(super::boot_restore::sample(&coids(2)), "c0, c1");
    assert_eq!(super::boot_restore::sample(&coids(8)), "c0, c1, c2, c3, c4, c5, c6, c7");
    assert_eq!(super::boot_restore::sample(&coids(10)), "c0, c1, c2, c3, c4, c5, c6, c7 (2 more)");
}

// --- the GATEWAY CURSOR (journal v18): `Ingest::GatewayEvent` folds as the event it carries, holds
// --- its ring's cursor for the next `Snap`, and what a replayed event can still do to a book the
// --- restore and a reconcile pass already settled (the interplay the ring replay is owed).

fn cursor(gw_epoch: u64, seq: u64) -> GatewayCursor {
    GatewayCursor { gw_epoch, seq, ts_ms: seq as i64 }
}

fn via_gateway(event: Event, route_key: &str, c: GatewayCursor) -> Ingest {
    Ingest::GatewayEvent(Box::new(GatewayEvent { event, route_key: route_key.into(), cursor: c }))
}

/// A `sim` buy fill of `coid`: `qty` at `px`, under the trade id `tid`.
fn fill_event(coid: &str, tid: &str, qty: f64, px: f64) -> vike_model::events::FillEvent {
    vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::new(tid).expect("a non-empty trade id"),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 5,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

fn fill_ev(tid: &str) -> Event {
    Event::Fill(fill_event("", tid, 1.0, 100.0))
}

/// One `sim` pass under `hybrid`, so a `MissingFill` FOLDS (the other tests hold it under
/// `quarantine`), delivered as `handle` does after the command.
fn hybrid_pass(core: &mut Core, orders: Vec<OrderStatusReport>, fills: Vec<FillReport>) {
    core.reconcile_reports(ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders,
        fills,
        positions: Vec::new(),
        policy: ReconPolicy::hybrid(),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
    });
    core.dispatch_applied_fills();
}

fn position(core: &Core) -> f64 {
    core.engine.position_size(vike_model::events::PositionSide::Both)
}

/// **A gateway event folds exactly as the plain event it carries.** The same fill through each door
/// leaves the same position, balance and dedup ledger: the `GatewayEvent` arm and the `Event` arm
/// share one fold, and the cursor is bookkeeping beside it.
#[test]
fn a_gateway_event_folds_as_the_event_it_carries() {
    let mut plain = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    let mut gateway = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    plain.dispatch(Ingest::Event(Event::Fill(fill_event("", "t-1", 2.0, 100.0))));
    gateway.dispatch(via_gateway(
        Event::Fill(fill_event("", "t-1", 2.0, 100.0)),
        "sim",
        cursor(7, 1),
    ));
    assert_eq!(position(&gateway), 2.0);
    assert_eq!(position(&gateway).to_bits(), position(&plain).to_bits());
    assert_eq!(
        gateway.engine.account.balance.to_bits(),
        plain.engine.account.balance.to_bits(),
        "the same money"
    );
    assert_eq!(
        gateway.engine.seen_trade_ids().collect::<Vec<_>>(),
        plain.engine.seen_trade_ids().collect::<Vec<_>>(),
        "and the same dedup ledger"
    );
    assert!(gateway.dirty, "and the fold marked the core for a publish, as an Event does");
}

/// **The core holds each ring's cursor by the one rule** (`GatewayCursor::after`): the larger `seq`
/// inside an epoch, the later arrival across epochs, one entry per account in first-seen order. A
/// straggler a replay re-delivers never rewinds the resume point.
#[test]
fn the_core_holds_each_rings_cursor_by_the_one_rule() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    let held = |core: &Core| -> Vec<(String, GatewayCursor)> {
        core.gateway_cursors.iter().map(|(k, c)| (k.to_string(), *c)).collect()
    };
    assert!(held(&core).is_empty(), "a core that never heard a gateway holds no cursor");

    core.dispatch(via_gateway(fill_ev("a1"), "sim", cursor(7, 5)));
    core.dispatch(via_gateway(fill_ev("b1"), "sim#ALT", cursor(7, 2)));
    core.dispatch(via_gateway(fill_ev("a2"), "sim", cursor(7, 3))); // a straggler
    assert_eq!(
        held(&core),
        vec![("sim".to_string(), cursor(7, 5)), ("sim#ALT".to_string(), cursor(7, 2))]
    );
    core.dispatch(via_gateway(fill_ev("a3"), "sim", cursor(7, 6)));
    core.dispatch(via_gateway(fill_ev("b2"), "sim#ALT", cursor(9, 1))); // a new epoch
    assert_eq!(
        held(&core),
        vec![("sim".to_string(), cursor(7, 6)), ("sim#ALT".to_string(), cursor(9, 1))]
    );
}

/// A core assembled over the restore's cursors holds them (the next `Snap` will carry them), and a
/// plain `Ingest::Event` never touches them.
#[test]
fn the_restored_cursors_seed_the_core_and_a_plain_event_leaves_them_alone() {
    let config = CoreConfig {
        gateway_cursors: vec![("sim#ALT".to_string(), cursor(4, 40))],
        ..CoreConfig::default()
    };
    let mut core = core_of(engine("sim"), Vec::new(), config);
    assert_eq!(core.gateway_cursors.len(), 1);
    core.dispatch(Ingest::Event(fill_ev("p1")));
    assert_eq!(core.gateway_cursors.len(), 1);
    assert_eq!(core.gateway_cursors[0].0.as_str(), "sim#ALT");
    assert_eq!(core.gateway_cursors[0].1, cursor(4, 40));
}

/// **THE OWED INTERPLAY TEST, order one: the reconcile pass books the fill first, the ring then
/// replays it.** The boot restore left an order resting; while the tradehub was down it filled. The
/// first pass finds it `filled_while_down` and the `MissingFill` path (folding, under `hybrid`)
/// books the venue's fill under its REAL trade id. The resume then replays what the ring held --
/// the very same fill, its lifecycle wrap, and the order's earlier events -- over the same trade
/// link: one fill, one position change.
#[test]
fn a_fill_the_pass_booked_is_not_booked_again_by_the_ring_replay() {
    let dir = Scratch::reserved("gw-interplay-pass-then-replay");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);

    hybrid_pass(&mut core, Vec::new(), vec![fill_of(&coid)]);
    assert_eq!(core.restore_counters.restored_filled_while_down, 1, "precondition: filled down");
    assert_eq!(position(&core), 1.0, "precondition: the MissingFill path booked the fill");
    let tid = fill_of(&coid).trade_id.to_string();
    let fill = || fill_event(&coid, &tid, 1.0, 100.0);
    let book_before = (position(&core).to_bits(), core.engine.account.balance.to_bits());

    // The replay: the order's events as the ring held them, `replayed:true` on the wire.
    let replay = [
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.clone(), ts: 1 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.clone(),
            venue_order_id: None,
            ts: 2,
        }),
        Event::Fill(fill()),
        Event::OrderFilled(OrderFilled { client_order_id: coid.clone(), fill: fill(), ts: 5 }),
    ];
    for (i, event) in replay.into_iter().enumerate() {
        core.dispatch(via_gateway(event, "sim", cursor(7, 1 + i as u64)));
    }

    assert_eq!(position(&core), 1.0, "ONE position change, not two");
    assert_eq!(
        core.engine.account.duplicate_fills_refused, 1,
        "the replayed fill was offered and refused by the ledger, not folded"
    );
    assert_eq!(
        (position(&core).to_bits(), core.engine.account.balance.to_bits()),
        book_before,
        "the book is exactly what the pass left"
    );
    assert_eq!(core.engine.dropped_terminal_on_live, 0, "no terminal is read as lost");
    assert_eq!(core.engine.stranded_terminal_drops, 0, "and nothing is read as stranded");
    assert_eq!(core.gateway_cursors.len(), 1);
    assert_eq!(core.gateway_cursors[0].1, cursor(7, 4), "the cursor advanced");
}

/// **…and the other order: the ring replay delivers the fill first, the pass then finds it already
/// booked.** The pass still counts the order `filled_while_down` (the restore's own bookkeeping),
/// but its `MissingFill` check consults the engine's seen trade ids, so the fill the replay booked
/// is no divergence and nothing folds a second time.
#[test]
fn a_fill_the_ring_replay_booked_is_not_booked_again_by_the_pass() {
    let dir = Scratch::reserved("gw-interplay-replay-then-pass");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    let tid = fill_of(&coid).trade_id.to_string();

    core.dispatch(via_gateway(
        Event::Fill(fill_event(&coid, &tid, 1.0, 100.0)),
        "sim",
        cursor(7, 1),
    ));
    assert_eq!(position(&core), 1.0, "precondition: the replay booked the fill");

    hybrid_pass(&mut core, Vec::new(), vec![fill_of(&coid)]);

    assert_eq!(position(&core), 1.0, "ONE position change, not two");
    assert_eq!(
        core.engine.account.duplicate_fills_refused, 0,
        "the pass never even OFFERED the booked fill: the diff saw it in the seen ids, not the fold's dedup"
    );
    assert_eq!(core.restore_counters.restored_filled_while_down, 1);
    assert!(
        core.recon_alerts.values().all(|a| a.kind != vike_exec::recon::DivergenceKind::MissingFill),
        "the booked fill is no divergence: {:?}",
        core.recon_alerts.values().map(|a| a.kind).collect::<Vec<_>>()
    );
}

/// **THE OWED TOLERANCE TEST: a re-delivered prefix of an order's events changes nothing.** An
/// adopted order takes its fills; then the whole venue story is delivered AGAIN from the top (the
/// shape a ring replay or a reconnect resync takes). The order, the position, the balance and the
/// seen ledgers are exactly what the first delivery left, and neither the lost-terminal counter
/// (`dropped_terminal_on_live`) nor the stranded-position counter moves: a benign replay is not
/// counted as a fault.
#[test]
fn a_redelivered_prefix_of_order_events_changes_nothing() {
    let dir = Scratch::reserved("gw-redelivered-prefix");
    let (coid, nb, events) = rest_one(&dir);
    let mut core = restart(&dir, &nb, &events);
    run(&mut core, vec![report(&coid, OrderStatus::Accepted)], Vec::new());
    assert!(core.engine.registry.contains_key(&coid), "precondition: the order is adopted");

    let story = |with_prefix: bool| -> Vec<Event> {
        let (f1, f2) = (fill_event(&coid, "r-1", 0.4, 100.0), fill_event(&coid, "r-2", 0.6, 110.0));
        let mut out = Vec::new();
        if with_prefix {
            out.push(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: coid.clone(),
                ts: 1,
            }));
            out.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: coid.clone(),
                venue_order_id: None,
                ts: 2,
            }));
        }
        out.push(Event::Fill(f1.clone()));
        out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid.clone(),
            fill: f1,
            ts: 6,
        }));
        out.push(Event::Fill(f2.clone()));
        out.push(Event::OrderFilled(OrderFilled {
            client_order_id: coid.clone(),
            fill: f2,
            ts: 7,
        }));
        out
    };
    let observe = |core: &Core| {
        let o = &core.engine.registry[&coid];
        (
            o.status,
            o.filled_qty.to_bits(),
            o.avg_fill_px.to_bits(),
            position(core).to_bits(),
            core.engine.account.balance.to_bits(),
            core.engine.account.realized_pnl.to_bits(),
        )
    };

    for (i, event) in story(false).into_iter().enumerate() {
        core.dispatch(via_gateway(event, "sim", cursor(3, 10 + i as u64)));
    }
    let first = observe(&core);
    assert_eq!(first.0, OrderStatus::Filled, "precondition: the story completed the order");
    assert_eq!(f64::from_bits(first.1), 1.0, "precondition: 0.4 + 0.6, once");

    for (i, event) in story(true).into_iter().enumerate() {
        core.dispatch(via_gateway(event, "sim", cursor(3, 20 + i as u64)));
    }
    assert_eq!(observe(&core), first, "the replay moved nothing");
    assert_eq!(
        core.engine.dropped_terminal_on_live, 0,
        "no benign replay is counted as a lost terminal"
    );
    assert_eq!(core.engine.stranded_terminal_drops, 0, "nor as a stranded position");
}

// --- the cursor's two guards: a REDELIVERY is dropped before the journal and the fold, and a late
// --- straggler of an OLD epoch is not (the epoch is random: nothing says it is the older one).

/// A funding payment on `sim`/`BTCUSDT`: no id, no dedup, `Account::apply_funding` is a bare `+=`.
fn funding_ev(amount: f64) -> Event {
    Event::Funding(vike_model::events::FundingEvent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        funding_rate: 0.0001,
        amount,
        mark_price: None,
        ts: 1,
        route_key: None,
    })
}

/// Everything a redelivery could double: position, money, funding, the dedup ledger and the fill
/// dedup's own counter.
fn book(core: &Core) -> (u64, u64, u64, usize, u64) {
    (
        position(core).to_bits(),
        core.engine.account.balance.to_bits(),
        core.engine.account.funding_paid.to_bits(),
        core.engine.seen_trade_ids().count(),
        core.engine.account.duplicate_fills_refused,
    )
}

/// **A redelivered prefix of a ring (same epoch, `seq` at or below the held cursor) changes
/// nothing**: not the position, the money, the funding (a bare `+=` no dedup could save), the dedup
/// ledger, the counter of refused duplicates, the held cursor or the publish flag. The event AFTER
/// the cursor still folds, and a different epoch is never a redelivery.
#[test]
fn a_redelivered_prefix_of_the_ring_is_dropped_before_the_fold() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    let story = || [(fill_ev("r1"), 1), (funding_ev(-1.25), 2), (fill_ev("r2"), 3)];
    for (event, seq) in story() {
        core.dispatch(via_gateway(event, "sim", cursor(7, seq)));
    }
    let first = book(&core);
    assert_eq!(position(&core), 2.0, "precondition: both fills folded");
    assert_eq!(core.engine.account.funding_paid, -1.25, "precondition: the funding folded");

    core.dirty = false;
    for (event, seq) in story() {
        core.dispatch(via_gateway(event, "sim", cursor(7, seq)));
    }
    assert_eq!(
        book(&core),
        first,
        "the redelivered prefix, the held `seq` included, moved nothing"
    );
    assert_eq!(core.gateway_cursors[0].1, cursor(7, 3), "and the cursor stayed");
    assert!(!core.dirty, "nothing was folded, so nothing needs a publish");

    core.dispatch(via_gateway(funding_ev(-0.5), "sim", cursor(7, 4)));
    assert_eq!(core.engine.account.funding_paid, -1.75, "the event after the cursor folds");
    core.dispatch(via_gateway(funding_ev(-0.25), "sim", cursor(9, 1)));
    assert_eq!(core.engine.account.funding_paid, -2.0, "a new epoch is a new ring, not a replay");
}

/// **The OWED interplay, `PositionDrift` then ring replay: DOES NOT HOLD (`#[ignore]`d, reported).**
/// Under `hybrid`/`synthesize` a position the venue holds and the engine does not is rewritten by
/// `PositionDrift` through SYNTHESIZED legs (their own trade ids). The two fills that caused the
/// drift are then replayed from the ring under their REAL trade ids, which no ledger has seen, so
/// they fold on top: the position counts them twice. `MissingFill` is safe (its fill carries the
/// real id: see `a_fill_the_pass_booked_is_not_booked_again_by_the_ring_replay`); `PositionDrift`
/// has no id to dedup on, and the cursor cannot help (the replayed seqs are above the held one).
/// The root `CLAUDE.md` rollout rule keeps `hybrid`/`synthesize` off every gateway venue, which is
/// all that protects this today. The cure is a design decision (hold the ring replay until the pass
/// ends, or have the pass retire the ring's range it covered), not a line.
#[test]
#[ignore = "known double count: PositionDrift synthesizes ids, the ring replay brings the real ones (DOUBTS, w2_w3c)"]
fn a_position_the_pass_synthesized_is_not_booked_again_by_the_ring_replay() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    core.dispatch(via_gateway(fill_ev("a1"), "sim", cursor(7, 1)));
    assert_eq!(position(&core), 1.0, "precondition: one fill folded live");

    // Two more fills happened while the link was down (the pass's fill window did not reach them);
    // the venue's position report says 3.
    let venue_position = vike_model::PositionStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        qty: 3.0,
        avg_px: 100.0,
        ts: 5,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };
    core.reconcile_reports(ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: vec![venue_position],
        policy: ReconPolicy::hybrid(),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
    });
    core.dispatch_applied_fills();
    assert_eq!(position(&core), 3.0, "precondition: PositionDrift rewrote the size to the venue's");

    // The resume replays the two real fills from the ring.
    core.dispatch(via_gateway(fill_ev("a2"), "sim", cursor(7, 2)));
    core.dispatch(via_gateway(fill_ev("a3"), "sim", cursor(7, 3)));
    assert_eq!(position(&core), 3.0, "the replayed fills were already counted by the drift");
}

/// **A late straggler of the OLD epoch rewinds the held cursor to it -- pinned, not cured.** An
/// epoch is a random number, so nothing here can tell that `epoch 7` is the older one once `epoch 9`
/// has spoken: `GatewayCursor::after` takes the later arrival across epochs, and the redelivery
/// guard only judges the SAME epoch. The straggler is a real event and still folds (a fill is not
/// lost); only the resume point is wrong, and it errs to the safe side: the next resume presents an
/// epoch the gateway does not hold, which it must answer as "no cursor" (a reconcile then recovers
/// what the ring would have). The cure would be per-account memory of retired epochs that the
/// restore's journal fold shares, which is the gateway producer's contract to make unnecessary
/// (one ordered link per account), so it is left to the wave that writes the producer.
#[test]
fn a_late_straggler_of_the_old_epoch_rewinds_the_cursor_and_still_folds() {
    let mut core = core_of(engine("sim"), Vec::new(), CoreConfig::default());
    core.dispatch(via_gateway(fill_ev("old-1"), "sim", cursor(7, 5)));
    core.dispatch(via_gateway(fill_ev("new-1"), "sim", cursor(9, 1)));
    assert_eq!(core.gateway_cursors[0].1, cursor(9, 1), "precondition: the new epoch holds");

    core.dispatch(via_gateway(fill_ev("old-2"), "sim", cursor(7, 6)));
    assert_eq!(position(&core), 3.0, "the straggler is a real fill: folded, not lost");
    assert_eq!(
        core.gateway_cursors[0].1,
        cursor(7, 6),
        "and the cursor now names the old epoch (the next resume is answered `no cursor`)"
    );
}
