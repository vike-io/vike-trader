//! **What the OMS states after each fold** (`vike_journal::JournalRecord::ExecutionReport`): a live core journals one
//! self-contained report per order event, naming the account, the mount and the order after the fold, and only an
//! id this install provably minted is OURS. Driven through `spawn_core` on a journaling core, read back off the
//! journal.

use std::path::Path;
use std::sync::Arc;

use vike_core::testkit::AlwaysSubmitStrategy;

use vike_core::{CoreConfig, Mount, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Command, Engine, GatewayEvent, OrderIntent, RiskGate};
use vike_journal::JournalRecord;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderSubmitted, PositionSide, TradeId,
};
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent, OrderClientIdLabels,
    OrderClientIdPlan, OrderClientIdShape, OrderClientIdSource, OrderClientIdStrategyCode,
};
use vike_model::orders::{ExecutionOwnership, ExecutionReport};
use vike_model::{EventRingCursor, OrderStatus, RiskLimits};

use crate::kit::engines::{journaled_config, sim_engine};
use crate::kit::handle::wait_for_snapshot;
use crate::kit::journal::records;
use crate::kit::requests::sim_limit;
use crate::scratch::Scratch;
use vike_core::testkit::mount_of;

/// The core's own v2 session in these tests: untagged, base `abcd`, already 5 ids into its life, so an id this
/// session minted at a sequence below 5 is OURS though no registry holds it (a late fill after a restart).
const SESSION: &str = "W00000000abcd";

fn fill(order_client_id: &str, trade_id: &str, qty: f64, ts: i64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(trade_id).expect("non-empty"),
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.01,
        commission_asset: "USDT".into(),
        liquidity_side: "taker".into(),
        timestamp: ts,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// The id session `session` minted at sequence `seq` for strategy code `code`, full shape, account `#3`.
fn minted(session: &str, seq: u64, code: &str) -> String {
    let mut g = OrderClientIdGenerator::resume(session.into(), seq);
    let plan = OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(3));
    let labels = OrderClientIdLabels::new(
        OrderClientIdSource::Strategy,
        OrderClientIdStrategyCode::parse(code).expect("a code"),
        OrderClientIdIntent::OpenLong,
    );
    plan.mint(&mut g, labels)
}

/// Every `ExecutionReport` of the journal at `dir`, with its own sequence, in journal order.
fn reports(dir: &Path) -> Vec<(u64, ExecutionReport)> {
    records(dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ExecutionReport { sequence, report, .. } => Some((sequence, report)),
            _ => None,
        })
        .collect()
}

/// A journaling core on sim/BTCUSDT over `dir` (a resting `RecordingClient`), session [`SESSION`] at sequence 5,
/// cutting a `Snapshot` every `snapshot_every` records, with `mount` mounted when given.
fn config(dir: &Path, snapshot_every: u64, mount: Option<Mount>) -> CoreConfig {
    CoreConfig {
        order_client_id_session: Some((SESSION.into(), 5)),
        strategy: mount,
        ..journaled_config(dir, 1_000_000.0, snapshot_every)
    }
}

/// An inert mount on sim/BTCUSDT whose order client ids carry strategy code `0001`.
fn coded_mount() -> Mount {
    Mount {
        order_client_id_code: OrderClientIdStrategyCode::parse("0001"),
        // it acts on a quote tick only, and these tests send none
        ..mount_of("sim", "BTCUSDT", "1m", Box::new(AlwaysSubmitStrategy { calls: Arc::default() }))
    }
}

/// The gated path stays untouched: a fill naming no order (the latency gate's `NO_ORDER_CLIENT_ID`) journals
/// its write-ahead record and no report.
#[test]
fn a_fill_with_an_empty_order_client_id_journals_no_report() {
    let dir = Scratch::reserved("execution-report-empty-id");
    let handle = spawn_core(sim_engine(), config(&dir, u64::MAX, None));
    let events = handle.event_sender();
    for i in 0..3 {
        events.blocking_send(fill("", &format!("t{i}"), 1.0, 10)).unwrap();
    }
    handle.shutdown_and_join();
    assert!(reports(&dir).is_empty(), "no report: {:?}", reports(&dir));
}

/// An operator's order through its life (registered, submitted, accepted, a partial fill, canceled, then a late
/// fill): every report is OURS on the `DEFAULT` account with no mount, carries the venue order id from the accept
/// on, and states the order AFTER its fold; a fill carries the fill and the order both. No `Snapshot` ever
/// falls between a cause and its report (a `Snapshot` after every record).
#[test]
fn every_report_names_its_account_and_venue_order_id_and_follows_its_cause() {
    let dir = Scratch::reserved("execution-report-lifecycle");
    let handle = spawn_core(sim_engine(), config(&dir, 1, None));
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "op1", 1, 2.0, 100.0,
    )))));
    wait_for_snapshot(&cell, 5, "op1 registered", |s| {
        s.orders.iter().any(|o| o.order_client_id == "op1")
    });
    for event in [
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "op1".into(), timestamp: 20 }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: "op1".into(),
            venue_order_id: Some("V1".into()),
            timestamp: 21,
        }),
        fill("op1", "f1", 0.5, 22),
        Event::OrderCanceled(OrderCanceled {
            order_client_id: "op1".into(),
            reason: "by hand".into(),
            timestamp: 23,
            post_only_would_cross: false,
            refusal_id: None,
        }),
        fill("op1", "f2", 0.25, 24),
    ] {
        events.blocking_send(event).unwrap();
    }
    handle.shutdown_and_join();

    let all = reports(&dir);
    assert!(
        all.len() >= 6,
        "registration, submitted, accepted, fill, canceled, late fill: {all:#?}"
    );
    for (_, r) in &all {
        assert_eq!(
            (r.ownership, r.account.as_deref(), r.mount_id.as_deref(), r.strategy_code),
            (ExecutionOwnership::Ours, Some("DEFAULT"), None, None),
            "{r:?}"
        );
        assert_eq!(
            (r.venue.as_str(), r.symbol.as_str(), r.order_client_id.as_str()),
            ("sim", "BTCUSDT", "op1")
        );
    }
    let statuses: Vec<OrderStatus> =
        all.iter().filter_map(|(_, r)| r.order.as_ref().map(|o| o.status)).collect();
    assert_eq!(
        statuses.first(),
        Some(&OrderStatus::Initialized),
        "the registration first: {statuses:?}"
    );
    assert!(
        statuses.contains(&OrderStatus::Accepted) && statuses.contains(&OrderStatus::Canceled),
        "{statuses:?}"
    );
    let accepted = all
        .iter()
        .position(|(_, r)| r.order.as_ref().is_some_and(|o| o.status == OrderStatus::Accepted));
    for (_, r) in &all[accepted.expect("an accepted report")..] {
        assert_eq!(r.venue_order_id.as_deref(), Some("V1"), "from the accept on: {r:?}");
    }
    let fills: Vec<&ExecutionReport> =
        all.iter().map(|(_, r)| r).filter(|r| r.fill.is_some()).collect();
    assert_eq!(fills.len(), 2, "the partial and the late fill: {fills:?}");
    assert_eq!(
        (fills[0].at_ms, fills[0].fill.as_ref().unwrap().qty),
        (22, 0.5),
        "a fill's venue time"
    );
    assert!(fills[0].order.is_some(), "a fill on a known order carries the order too");

    let canceled = all.iter().find(|(_, r)| {
        r.order.as_ref().is_some_and(|o| o.status == OrderStatus::Canceled) && r.fill.is_none()
    });
    assert_eq!(
        canceled.and_then(|(_, r)| r.reason.as_deref()),
        Some("by hand"),
        "the venue's words"
    );
    assert_reports_follow_their_causes(&dir);
}

/// A fee correction is stamped at its FILL's time (`FeeAdjustmentEvent::timestamp`), never at the time the core
/// received it: the correction row the datahub writes from it lands where the materializer's did.
#[test]
fn a_fee_correction_report_carries_the_fills_time_not_the_receive_time() {
    const FILL_MS: i64 = 1_700_000_000_000;
    let dir = Scratch::reserved("execution-report-fee-time");
    let handle = spawn_core(sim_engine(), config(&dir, u64::MAX, None));
    let adjustment = vike_model::events::FeeAdjustmentEvent {
        trade_id: TradeId::new("f1").expect("non-empty"),
        order_client_id: "op1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        last_price: 100.0,
        extra_commission: 0.02,
        commission_asset: "USDT".into(),
        timestamp: FILL_MS,
    };
    handle.event_sender().blocking_send(Event::FeeAdjustment(adjustment)).unwrap();
    handle.shutdown_and_join();

    let all = reports(&dir);
    let [(_, report)] = all.as_slice() else { panic!("one report: {all:#?}") };
    assert_eq!(report.fee_correction.as_ref().map(|c| c.corrects_trade_id.as_str()), Some("f1"));
    let received_ms = records(&dir)
        .iter()
        .find_map(|r| match r {
            JournalRecord::Command { sequence, now_ms, .. }
                if *sequence == report.caused_by_sequence =>
            {
                Some(*now_ms)
            }
            _ => None,
        })
        .expect("the cause is journaled");
    assert_ne!(received_ms, FILL_MS, "the receive time differs from the fill's");
    assert_eq!(report.at_ms, FILL_MS, "the fill's time, not the receive time {received_ms}");
}

/// **Record order**: every report's cause is journaled before it and is no report, and nothing but other reports
/// lies between them, so no `Snapshot` ever splits a cause from its report.
fn assert_reports_follow_their_causes(dir: &Path) {
    let recs = records(dir);
    let seq_of = |r: &JournalRecord| vike_journal::record_sequence(r);
    for (sequence, report) in reports(dir) {
        let cause = recs
            .iter()
            .position(|r| seq_of(r) == report.caused_by_sequence)
            .expect("the cause is journaled");
        let at = recs.iter().position(|r| seq_of(r) == sequence).unwrap();
        assert!(cause < at, "a report follows its cause");
        assert!(
            !matches!(recs[cause], JournalRecord::ExecutionReport { .. }),
            "a cause is no report: {report:?}"
        );
        assert!(
            recs[cause + 1..at].iter().all(|r| matches!(r, JournalRecord::ExecutionReport { .. })),
            "only reports between cause {} and its report {sequence}",
            report.caused_by_sequence
        );
    }
}

/// A fill naming a v2 id this install's session minted (its sequence below the session's), on an order no
/// registry holds (it ended before a restart): OURS, its mount named from the id's strategy code. A code no mount
/// carries keeps its strategy code for the publisher to name, and no mount.
#[test]
fn a_late_fill_keeps_its_mount_from_its_strategy_code() {
    let dir = Scratch::reserved("execution-report-late-fill");
    let handle = spawn_core(sim_engine(), config(&dir, u64::MAX, Some(coded_mount())));
    let events = handle.event_sender();
    events.blocking_send(fill(&minted(SESSION, 1, "0001"), "f1", 1.0, 30)).unwrap();
    events.blocking_send(fill(&minted(SESSION, 2, "0002"), "f2", 1.0, 31)).unwrap();
    handle.shutdown_and_join();
    let all = reports(&dir);
    let [(_, coded), (_, removed)] = all.as_slice() else { panic!("two reports: {all:#?}") };
    let mount = vike_core::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m");
    assert_eq!(coded.ownership, ExecutionOwnership::Ours);
    assert_eq!(coded.mount_id.as_deref(), Some(mount.as_str()));
    assert_eq!(coded.strategy_code, OrderClientIdStrategyCode::parse("0001"));
    assert_eq!(coded.account.as_deref(), Some("DEFAULT"));
    assert_eq!(
        (coded.order.as_ref(), coded.venue_order_id.as_ref()),
        (None, None),
        "no record after a restart"
    );
    assert_eq!((removed.ownership, removed.mount_id.as_deref()), (ExecutionOwnership::Ours, None));
    assert_eq!(removed.strategy_code, OrderClientIdStrategyCode::parse("0002"));
}

/// Another install's id with OUR strategy code, on our account's ring: FOREIGN, the ring's account, no mount and
/// no strategy. The same id with no ring names no account; an id of a PAST session of this install is ours.
#[test]
fn a_foreign_fill_on_our_account_has_no_mount_and_is_marked_foreign() {
    let dir = Scratch::reserved("execution-report-foreign");
    let past = "Wzzzzzzz00000";
    let config = CoreConfig {
        order_client_id_past_sessions: vec![(past.into(), 3)],
        ..config(&dir, u64::MAX, Some(coded_mount()))
    };
    let handle = spawn_core(sim_engine(), config);
    let events = handle.event_sender();
    let theirs = minted("W0000000zz000", 1, "0001");
    let cursor = EventRingCursor { gateway_epoch: 1, sequence: 1, timestamp_ms: 10 };
    events
        .blocking_send_gateway(GatewayEvent {
            event: fill(&theirs, "f1", 1.0, 40),
            route_key: "sim".into(),
            cursor,
        })
        .unwrap();
    events.blocking_send(fill(&theirs, "f2", 1.0, 41)).unwrap();
    events.blocking_send(fill(&minted(past, 2, "0001"), "f3", 1.0, 42)).unwrap();
    events.blocking_send(fill("EXT-7", "f4", 1.0, 43)).unwrap();
    handle.shutdown_and_join();
    let all = reports(&dir);
    let [(_, ring), (_, bare), (_, past_ours), (_, external)] = all.as_slice() else {
        panic!("four reports: {all:#?}")
    };
    assert_eq!(
        (ring.ownership, ring.account.as_deref(), ring.mount_id.as_deref(), ring.strategy_code),
        (ExecutionOwnership::Foreign, Some("DEFAULT"), None, None)
    );
    assert_eq!((bare.ownership, bare.account.as_deref()), (ExecutionOwnership::Foreign, None));
    assert_eq!(
        past_ours.ownership,
        ExecutionOwnership::Ours,
        "a past session's id below its last sequence"
    );
    assert_eq!(external.ownership, ExecutionOwnership::Foreign, "an id no session reads");
}

/// A RiskGate deny never registers its order: its report is built from the request (status `DENIED`, the
/// request's side, quantity and price, nothing filled, the gate's reason), OURS on its account.
#[test]
fn a_deny_is_built_from_its_request() {
    let dir = Scratch::reserved("execution-report-deny");
    let limits = RiskLimits { max_notional_per_order: Some(1.0), ..RiskLimits::new() };
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, config(&dir, u64::MAX, None));
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "big", -1, 2.0, 100.0,
    )))));
    handle.shutdown_and_join();
    let all = reports(&dir);
    let [(_, deny)] = all.as_slice() else { panic!("one report, the deny: {all:#?}") };
    let order = deny.order.as_ref().expect("built from the request");
    assert_eq!(order.status, OrderStatus::Denied);
    assert_eq!((order.side, order.qty, order.price, order.filled_qty), (-1, 2.0, Some(100.0), 0.0));
    assert!(deny.reason.as_deref().is_some_and(|r| !r.is_empty()), "the gate's words: {deny:?}");
    assert_eq!(
        (deny.ownership, deny.account.as_deref()),
        (ExecutionOwnership::Ours, Some("DEFAULT"))
    );
}

/// The one report of order `id` whose order reached `status`, of the journal at `dir`.
fn report_at(dir: &Path, id: &str, status: OrderStatus) -> ExecutionReport {
    reports(dir)
        .into_iter()
        .map(|(_, r)| r)
        .find(|r| r.order_client_id == id && r.order.as_ref().is_some_and(|o| o.status == status))
        .unwrap_or_else(|| panic!("{id} reported {status:?}: {:#?}", reports(dir)))
}

/// **Every terminal the core makes up is reported, with its words** (C.1's site 3): a capability reject (a post-only
/// limit on a venue with no post-only order), a combo the gate denies, a held order cancelled before its release,
/// and the watchdog's reject of an order no venue ever acknowledged. Each is OURS on its account and carries the
/// order after the fold and the reason the core gave.
#[test]
fn every_terminal_the_core_makes_up_is_reported() {
    use vike_execution::testing::engine_on;
    use vike_model::{OrderRelease, OrderRequest, OrderType, TimeInForce};

    let journaled = |dir: &Path| crate::kit::journal::journal_at(dir, 1024 * 1024, u64::MAX);

    // a capability reject: oanda has no post-only order
    let dir = Scratch::reserved("execution-report-capability-reject");
    let handle = spawn_core(
        engine_on("oanda", "EUR_USD", RecordingClient::default()),
        CoreConfig { journal: Some(journaled(&dir)), ..CoreConfig::default() },
    );
    let post_only = OrderRequest {
        order_client_id: "po1".into(),
        venue: "oanda".into(),
        symbol: "EUR_USD".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(1.1),
        time_in_force: TimeInForce::PostOnly,
        ..Default::default()
    };
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(post_only))));
    handle.shutdown_and_join();
    let rejected = report_at(&dir, "po1", OrderStatus::Rejected);
    assert!(
        rejected.reason.as_deref().is_some_and(|r| r.starts_with("TIF_UNSUPPORTED")),
        "{rejected:?}"
    );
    assert_eq!(
        (rejected.ownership, rejected.account.as_deref()),
        (ExecutionOwnership::Ours, Some("DEFAULT"))
    );

    // a combo the gate denies: the engine is halted, so the kill switch refuses it before anything else
    let dir = Scratch::reserved("execution-report-combo-deny");
    let mut deribit = engine_on("deribit", "BTC-27MAR26-100000-C", RecordingClient::default());
    deribit.trading_state = vike_model::TradingState::Halted;
    let handle =
        spawn_core(deribit, CoreConfig { journal: Some(journaled(&dir)), ..CoreConfig::default() });
    let combo = vike_model::OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::OrderComboLeg {
                symbol: "BTC-27MAR26-100000-C".into(),
                ratio: 1,
                limit: None,
            },
            vike_model::OrderComboLeg {
                symbol: "BTC-27MAR26-120000-C".into(),
                ratio: -1,
                limit: None,
            },
        ],
        net_limit: Some(-0.0125),
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    handle.send_command(Command::Order(OrderIntent::Combo(Box::new(combo))));
    handle.shutdown_and_join();
    let all = reports(&dir);
    let [(_, denied)] = all.as_slice() else { panic!("one report, the combo's deny: {all:#?}") };
    let order = denied.order.as_ref().expect("built from the request");
    assert_eq!(
        (order.status, order.side, order.qty, order.filled_qty),
        (OrderStatus::Denied, 1, 1.0, 0.0)
    );
    assert!(
        denied.reason.as_deref().is_some_and(|r| !r.is_empty()),
        "the gate's words: {denied:?}"
    );
    assert_eq!(denied.ownership, ExecutionOwnership::Ours);

    // a held order cancelled before its release (2100-01-01)
    let dir = Scratch::reserved("execution-report-held-cancel");
    let handle = spawn_core(
        sim_engine(),
        CoreConfig { journal: Some(journaled(&dir)), ..CoreConfig::default() },
    );
    let held = OrderRequest {
        release: Some(OrderRelease { at: 4_102_444_800_000, price: None }),
        ..sim_limit("h1", 1, 1.0, 100.0)
    };
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(held))));
    let cell = handle.snapshot_cell();
    wait_for_snapshot(&cell, 5, "the held row", |s| {
        s.held_exits.iter().any(|h| h.release.is_some())
    });
    handle.send_command(Command::Order(OrderIntent::Cancel("h1".into())));
    wait_for_snapshot(&cell, 5, "the held row is gone", |s| s.held_exits.is_empty());
    handle.shutdown_and_join();
    let canceled = report_at(&dir, "h1", OrderStatus::Canceled);
    assert!(
        canceled.reason.as_deref().is_some_and(|r| !r.is_empty()),
        "the core's words: {canceled:?}"
    );
    assert_eq!(
        (canceled.ownership, canceled.account.as_deref()),
        (ExecutionOwnership::Ours, Some("DEFAULT"))
    );

    // the watchdog's reject of an order no venue ever acknowledged (real time: a 50 ms timeout and grace)
    let dir = Scratch::reserved("execution-report-watchdog-reject");
    let handle = spawn_core(
        sim_engine(),
        CoreConfig {
            submit_ack_timeout: Some(std::time::Duration::from_millis(50)),
            submit_ack_confirm_grace: std::time::Duration::from_millis(50),
            journal: Some(journaled(&dir)),
            ..CoreConfig::default()
        },
    );
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "w1", 1, 1.0, 100.0,
    )))));
    let cell = handle.snapshot_cell();
    wait_for_snapshot(&cell, 5, "the watchdog's reject", |s| {
        s.orders.iter().any(|o| o.order_client_id == "w1" && o.status == OrderStatus::Rejected)
    });
    handle.shutdown_and_join();
    let rejected = report_at(&dir, "w1", OrderStatus::Rejected);
    assert!(
        rejected.reason.as_deref().is_some_and(|r| r.starts_with("watchdog")),
        "the watchdog's words: {rejected:?}"
    );
    assert_eq!(rejected.ownership, ExecutionOwnership::Ours);
}

/// **A paper fill is reported before its cadence `Snapshot`** (C.1's site 2): an in-process client that fills at
/// once (`TestExecutionClient`) hands its events to `pump_client`, which journals each, folds it, reports it and only
/// then asks for the cadence `Snapshot` (here after every record), so no `Snapshot` splits a pumped event from its
/// report.
#[test]
fn a_paper_fill_is_reported_before_its_cadence_snapshot() {
    let dir = Scratch::reserved("execution-report-paper-cadence");
    let engine = crate::kit::engines::sim_engine_with(
        vike_execution::testing::TestExecutionClient::new("sim", 100.0),
    );
    let handle = spawn_core(engine, config(&dir, 1, None));
    let market = vike_model::OrderRequest {
        order_type: vike_model::OrderType::Market,
        price: None,
        ..sim_limit("pm1", 1, 1.0, 100.0)
    };
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(market))));
    handle.shutdown_and_join();
    let all = reports(&dir);
    assert!(all.iter().any(|(_, r)| r.order_client_id == "pm1" && r.fill.is_some()), "{all:#?}");
    let filled = report_at(&dir, "pm1", OrderStatus::Filled);
    assert_eq!(filled.ownership, ExecutionOwnership::Ours);
    assert_reports_follow_their_causes(&dir);
    assert!(
        records(&dir).iter().filter(|r| matches!(r, JournalRecord::Snapshot { .. })).count() > 1,
        "a Snapshot after every record"
    );
}
