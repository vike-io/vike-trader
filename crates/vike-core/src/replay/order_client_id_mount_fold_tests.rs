//! White-box tests of [`fold_order_client_id_mounts`] — the pure order client id -> mount-id fold behind attribution
//! durability (gap D). The end-to-end restore is gated in `tests/mount_attribute_durability.rs`;
//! these pin the per-record pairing rule (open / pair / close) in isolation.

use super::fold_tests::cmd;
use super::*;

fn req(order_client_id: &str) -> OrderRequest {
    OrderRequest { venue: "sim".into(), ..vike_execution::testing::resting_limit(order_client_id) }
}

fn strategy_submit(seq: u64, mount_id: &str, order_client_id: &str) -> JournalRecord {
    JournalRecord::StrategySubmit {
        sequence: seq,
        now_ms: seq as i64,
        mount_id: mount_id.into(),
        // the runtime always journals the resolved intent with an EMPTY order client id (the mint happens
        // inside `apply_intent`, after this record) unless the caller supplied one
        intent: vike_execution::OrderIntent::Submit(Box::new(req(order_client_id))),
        route_key: None,
    }
}

fn minted(seq: u64, order_client_id: &str) -> JournalRecord {
    JournalRecord::MintedSubmit {
        sequence: seq,
        now_ms: seq as i64,
        request: req(order_client_id),
        route_key: None,
        replaces: None,
    }
}

/// The pairing rule: each `StrategySubmit` OPENS ownership and every `MintedSubmit` that follows
/// it (a bracket mints three) belongs to that mount — until a non-strategy write closes it.
#[test]
fn minted_submits_pair_with_the_open_strategy_submit() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "c1"),
        strategy_submit(3, "maker-b", ""),
        minted(4, "c2"),
        minted(5, "c3"), // a bracket's second leg — same owner
    ];
    assert_eq!(
        fold_order_client_id_mounts(&recs),
        vec![
            ("c1".to_string(), "maker-a".to_string()),
            ("c2".to_string(), "maker-b".to_string()),
            ("c3".to_string(), "maker-b".to_string()),
        ]
    );
}

/// A NON-strategy write closes ownership, so an operator command's minted order client id — and a
/// runtime-decided liquidation's — stay UNATTRIBUTED rather than being credited to whichever
/// mount happened to submit last.
#[test]
fn a_non_strategy_write_closes_ownership() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "c1"),
        cmd(3, vike_execution::OrderIntent::Submit(Box::new(req("")))),
        minted(4, "operator-order-client-id"),
        JournalRecord::MarginCallLiquidate {
            sequence: 5,
            now_ms: 5,
            request: req(""),
            mount_id: None,  // the ACCOUNT-wide margin-call sweep — owned by no mount
            route_key: None, // one account of this venue, so the venue id IS the route key
        },
        minted(6, "liquidation-order-client-id"),
    ];
    assert_eq!(
        fold_order_client_id_mounts(&recs),
        vec![("c1".to_string(), "maker-a".to_string())],
        "only the mount-minted order is attributed"
    );
}

/// **THE v14 ARM.** A `MarginCallLiquidate` that NAMES a mount — the per-mount budget latch's
/// flatten — OPENS ownership like a `StrategySubmit`, so the order client id `apply_intent` mints for it
/// restores into THAT mount's ledger.
///
/// Without the id (every pre-v14 frame, and the account-wide sweep above) the flatten's order client id came
/// back unattributed and its fill landed in the RESIDUAL row — under-reporting the exact realized
/// loss the budget latch exists to bound.
#[test]
fn an_owned_margin_call_liquidate_attributes_its_flatten_to_that_mount() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "c1"), // the mount's own resting order
        JournalRecord::MarginCallLiquidate {
            sequence: 3,
            now_ms: 3,
            request: req(""),
            mount_id: Some("maker-a".into()), // the BUDGET LATCH's flatten
            route_key: None, // one account of this venue, so the venue id IS the route key
        },
        minted(4, "flatten-order-client-id"),
    ];
    assert_eq!(
        fold_order_client_id_mounts(&recs),
        vec![
            ("c1".to_string(), "maker-a".to_string()),
            ("flatten-order-client-id".to_string(), "maker-a".to_string()),
        ],
        "the latch's flatten belongs to the mount whose budget breach released it"
    );
}

/// A client-SUPPLIED order client id writes no `MintedSubmit` (it skips the mint), so the intent itself is
/// the only place its origin exists.
#[test]
fn a_client_supplied_order_client_id_maps_from_the_intent() {
    let recs = vec![strategy_submit(1, "maker-a", "given-order-client-id")];
    assert_eq!(
        fold_order_client_id_mounts(&recs),
        vec![("given-order-client-id".to_string(), "maker-a".to_string())]
    );
}

fn armed(seq: u64, arm_id: &str) -> JournalRecord {
    JournalRecord::OrderEmulationArmed {
        sequence: seq,
        now_ms: seq as i64,
        arm_id: arm_id.into(),
        resolved: vike_journal::OrderEmulationRecord {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(90.0),
            trail: None,
            extreme: None,
            trigger_by: None,
            reduce_only: false,
            kind_terms: None,
        },
    }
}

fn fired(seq: u64, arm_id: &str) -> JournalRecord {
    JournalRecord::OrderEmulationFire {
        sequence: seq,
        now_ms: seq as i64,
        arm_id: arm_id.into(),
        trigger_price: 90.0,
        request: req(""),
        route_key: None,
    }
}

/// **A FIRED STOP BELONGS TO THE MOUNT THAT ARMED IT.** A `OrderEmulationArmed` journaled while a
/// mount's `StrategySubmit` is open records that arm's owner; the later `OrderEmulationFire` REOPENS
/// ownership with THAT owner (not the mount that happened to submit last), so the `MintedSubmit` the
/// release writes restores into the arming mount's ledger. Before, the fire closed ownership and
/// the restored fill landed in the residual row.
#[test]
fn a_fired_conditional_reopens_ownership_with_the_mount_that_armed_it() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""), // the ArmConditional intent of mount A
        armed(2, "arm-1"),
        strategy_submit(3, "maker-b", ""), // another mount submits in between
        minted(4, "c-b"),
        fired(5, "arm-1"),
        minted(6, "fired-order-client-id"),
    ];
    assert_eq!(
        fold_order_client_id_mounts(&recs),
        vec![
            ("c-b".to_string(), "maker-b".to_string()),
            ("fired-order-client-id".to_string(), "maker-a".to_string()),
        ],
        "the fire is credited to the ARMING mount, not to the last submitter"
    );
}

/// **A stop larger than its position fires as two records of one arm** (`CoreThread::submit_fired`), and both orders are
/// the arming mount's, as they are live. The arm has left the owner's table by the second; another arm's fire is not it.
#[test]
fn both_records_of_a_stop_larger_than_its_position_belong_to_the_mount_that_armed_it() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        armed(2, "arm-1"),
        fired(3, "arm-1"),
        minted(4, "the-close"),
        fired(5, "arm-1"),
        minted(6, "the-rest"),
        fired(7, "nobodys-arm"),
        minted(8, "unowned"),
    ];
    assert_eq!(
        fold_order_client_id_mounts(&recs),
        vec![
            ("the-close".to_string(), "maker-a".to_string()),
            ("the-rest".to_string(), "maker-a".to_string()),
        ]
    );
}

/// **A MOUNT'S CORE-HELD REDUCE-ONLY STOP IS OWNED THE SAME WAY** (`LiveBroker::submit_stop_reduce` on a venue
/// whose row refuses a reduce-only stop). The record is a `StrategySubmit` carrying a `Submit` of a stop-kind
/// reduce-only order (not an `ArmConditional`), then a `OrderEmulationArmed` and NO `MintedSubmit` (the core held
/// it, nothing was minted): the arm is the mount's all the same, and its fire is credited to it.
#[test]
fn a_mounts_core_held_reduce_only_stop_submit_owns_its_arm_and_its_fire() {
    let stop = OrderRequest {
        side: -1,
        order_type: vike_model::OrderType::Stop,
        price: None,
        trigger_price: Some(95.0),
        reduce_only: true,
        ..req("")
    };
    // JournalRecord is not `Clone`: build the same record per run.
    let submit = || JournalRecord::StrategySubmit {
        sequence: 1,
        now_ms: 1,
        mount_id: "guard".into(),
        intent: vike_execution::OrderIntent::Submit(Box::new(stop.clone())),
        route_key: None,
    };

    let resting = vec![submit(), armed(2, "arm-1")];
    let (order_client_ids, owners) = fold_order_client_id_origins(&resting);
    assert!(order_client_ids.is_empty(), "nothing was minted: {order_client_ids:?}");
    assert_eq!(owners.get("arm-1").map(String::as_str), Some("guard"), "the arm is the mount's");

    let fired_run =
        vec![submit(), armed(2, "arm-1"), fired(3, "arm-1"), minted(4, "close-order-client-id")];
    assert_eq!(
        fold_order_client_id_mounts(&fired_run),
        vec![("close-order-client-id".to_string(), "guard".to_string())],
        "the fire's market is the mount's order"
    );
}

/// An OPERATOR-armed conditional (its arm sits under a `Command`, which closed ownership) stays
/// unattributed when it fires, and so does a fire whose arm was disarmed or never seen (its
/// `OrderEmulationArmed` sat in a pruned segment): the fold never guesses an owner.
#[test]
fn an_operator_armed_conditional_fires_unattributed() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "c-a"),
        cmd(3, vike_execution::OrderIntent::Submit(Box::new(req("")))), // the operator arms
        armed(4, "arm-op"),
        fired(5, "arm-op"),
        minted(6, "operator-fired"),
        strategy_submit(7, "maker-a", ""),
        armed(8, "arm-2"),
        JournalRecord::OrderEmulationDisarmed { sequence: 9, now_ms: 9, arm_id: "arm-2".into() },
        fired(10, "arm-2"), // a fire after its own disarm names no live owner
        minted(11, "ghost-fired"),
        fired(12, "arm-pruned"), // its OrderEmulationArmed was in a pruned segment: not in view
        minted(13, "pruned-fired"),
    ];
    assert_eq!(
        fold_order_client_id_mounts(&recs),
        vec![("c-a".to_string(), "maker-a".to_string())],
        "only the mount-minted order is attributed"
    );
}

// ---------------------------------------------------------------------------------------------
// `resting_orders` (decision 0127): the journal contributes only RESTING orders as ownership
// candidates — not every order client id a mount ever minted.
// ---------------------------------------------------------------------------------------------

use super::fold::{fold_order_client_id_origins, resting_orders, tail_facts};

/// A `sim`/`BTCUSDT` engine snapshot (routed as `route_key`) whose registry holds `orders`.
fn engine_snap(
    route_key: &str,
    orders: &[(&str, vike_model::OrderStatus)],
) -> vike_execution::EngineSnapshot {
    let mut e = vike_execution::Engine::new(
        vike_execution::Account::new(1.0, "sim", None, vike_execution::BalanceMode::Delta),
        vike_execution::RiskGate::new(vike_model::RiskLimits::new()),
        vike_execution::testing::RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    e.route_key = route_key.to_string();
    for (order_client_id, status) in orders {
        let mut order = vike_model::OrderRecord::new(req(order_client_id));
        order.status = *status;
        e.registry.insert(order_client_id.to_string(), order);
    }
    e.snapshot_state()
}

/// `order_client_ids`, each owned by mount `m`.
fn owned_by_m(order_client_ids: &[&str]) -> std::collections::HashMap<String, String> {
    order_client_ids.iter().map(|c| (c.to_string(), "m".to_string())).collect()
}

fn order_client_ids_of(orders: &[super::OrderOwnerCandidate]) -> Vec<&str> {
    orders.iter().map(|o| o.order_client_id.as_str()).collect()
}

/// A journaled venue event ending `order_client_id`.
fn canceled(seq: u64, order_client_id: &str) -> JournalRecord {
    JournalRecord::Command {
        sequence: seq,
        now_ms: seq as i64,
        message: Ingest::Event(vike_model::events::Event::OrderCanceled(
            vike_model::events::OrderCanceled {
                order_client_id: order_client_id.into(),
                reason: "venue cancel".into(),
                timestamp: seq as i64,
            },
        )),
    }
}

#[test]
fn resting_order_client_id_mounts_drops_filled_canceled_rejected_expired_and_denied_orders() {
    use vike_model::OrderStatus as S;
    let snap = engine_snap(
        "sim",
        &[
            ("open", S::Accepted),
            ("unacked", S::Submitted),
            ("filled", S::Filled),
            ("canceled", S::Canceled),
            ("rejected", S::Rejected),
            ("expired", S::Expired),
            ("denied", S::Denied),
            ("local", S::Emulated),
        ],
    );
    let owners = owned_by_m(&[
        "open", "unacked", "filled", "canceled", "rejected", "expired", "denied", "local",
    ]);
    let orders = resting_orders(std::slice::from_ref(&snap), None, &[], &owners);
    assert_eq!(order_client_ids_of(&orders), vec!["open", "unacked"], "only what may still rest");
    let open = &orders[0];
    assert_eq!(open.mount_id, "m");
    assert_eq!(open.venue.as_deref(), Some("sim"));
    assert_eq!(open.symbol.as_deref(), Some("BTCUSDT"));
    assert_eq!(open.account, None, "the default account has no label");
    assert_eq!(open.price, Some(100.0), "its limit price, for the adoption");

    // The snap-only reading: the tail ended `open` and minted `fresh`.
    let tail = vec![canceled(10, "open"), minted(11, "fresh")];
    let facts = tail_facts(&tail);
    let mut owners = owners;
    owners.insert("fresh".into(), "m".into());
    let orders = resting_orders(std::slice::from_ref(&snap), Some(&facts), &[], &owners);
    assert_eq!(order_client_ids_of(&orders), vec!["unacked", "fresh"], "ended out, minted in");
}

#[test]
fn resting_order_client_id_mounts_keeps_a_held_exit_that_is_not_at_the_venue() {
    use vike_model::OrderStatus as S;
    let snap = engine_snap("sim#ALT", &[("entry", S::Accepted)]);
    let held = vike_journal::JournalSnapshotContingency {
        order_client_id: "exit".into(),
        parent: Some("entry".into()),
        linked: Vec::new(),
        active: false,
        held_request: Some(req("exit")),
        contingency_type: None,
    };
    let owners = owned_by_m(&["entry", "exit"]);
    let orders =
        resting_orders(std::slice::from_ref(&snap), None, std::slice::from_ref(&held), &owners);
    assert_eq!(
        order_client_ids_of(&orders),
        vec!["entry", "exit"],
        "the held exit is a candidate too"
    );
    assert_eq!(orders[0].account.as_deref(), Some("ALT"), "the labelled engine's account");
    assert_eq!(orders[1].account.as_deref(), Some("ALT"), "the exit takes its parent's account");

    // Its parent ended in the tail: the exit was released (or cascaded away), not held.
    let tail = vec![canceled(10, "entry")];
    let facts = tail_facts(&tail);
    let orders = resting_orders(std::slice::from_ref(&snap), Some(&facts), &[held], &owners);
    assert!(orders.is_empty(), "{orders:?}");
}

#[test]
fn resting_order_client_id_mounts_keeps_an_order_minted_before_the_base_snap_that_still_rests() {
    use vike_model::OrderStatus as S;
    let snap = engine_snap("sim", &[("old-1", S::Accepted), ("operator", S::Accepted)]);
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "old-1"),
        cmd(3, vike_execution::OrderIntent::Submit(Box::new(req("")))),
        minted(4, "operator"),
        JournalRecord::Snapshot {
            sequence: 5,
            now_ms: 5,
            engines: vec![snap.clone()],
            order_client_id_session: "s".into(),
            order_client_id_sequence: 2,
            arm_sequence: Some(0),
            order_emulation_arms: Vec::new(),
            contingencies: Vec::new(),
            mount_attribution: Vec::new(),
            gateway_cursors: Vec::new(),
            held_releases: Vec::new(),
            recent_order_client_ids: Vec::new(),
            emulated: Vec::new(),
            closed_trades: Vec::new(),
            hash: 0,
        },
    ];
    let owners: std::collections::HashMap<String, String> =
        fold_order_client_id_origins(&recs).0.into_iter().collect();
    let orders = resting_orders(std::slice::from_ref(&snap), None, &[], &owners);
    assert_eq!(
        order_client_ids_of(&orders),
        vec!["old-1"],
        "the operator's order has no mount to go to"
    );
    assert_eq!(orders[0].mount_id, "maker-a");
}

/// The arm owners `fold_order_client_id_origins` hands the boot restore: an arm journaled under a mount's open
/// `StrategySubmit` is that mount's until it fires or is disarmed; an operator's arm is nobody's.
#[test]
fn the_arm_owners_are_the_arms_a_mount_still_holds() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        armed(2, "arm-a"),
        armed(3, "arm-gone"),
        JournalRecord::OrderEmulationDisarmed { sequence: 4, now_ms: 4, arm_id: "arm-gone".into() },
        cmd(5, vike_execution::OrderIntent::Submit(Box::new(req("")))),
        armed(6, "arm-op"),
    ];
    let (_, arm_owners) = fold_order_client_id_origins(&recs);
    assert_eq!(arm_owners.get("arm-a").map(String::as_str), Some("maker-a"));
    assert!(!arm_owners.contains_key("arm-gone"), "disarmed");
    assert!(!arm_owners.contains_key("arm-op"), "the operator's arm has no owner");
}

// ---------------------------------------------------------------------------------------------
// The protective stops (restart-safe stops): what a candidate carries that a venue report does not,
// and the operator's stop no mount minted.
// ---------------------------------------------------------------------------------------------

use crate::order_owners::PROTECTIVE_STOP_MOUNT;
use vike_model::{AccountTier, OrderStatus, OrderType};

/// A snapshot of an engine that STATES `tier` (`None`: nobody told it) holding one order `order_client_id`.
fn stop_snap(
    tier: Option<AccountTier>,
    order_type: OrderType,
    reduce_only: bool,
    status: OrderStatus,
) -> vike_execution::EngineSnapshot {
    let mut e = vike_execution::Engine::new(
        vike_execution::Account::new(1.0, "sim", None, vike_execution::BalanceMode::Delta),
        vike_execution::RiskGate::new(vike_model::RiskLimits::new()),
        vike_execution::testing::RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    if let Some(tier) = tier {
        e.set_tier(tier);
    }
    let mut order = vike_model::OrderRecord::new(OrderRequest {
        side: -1,
        order_type,
        reduce_only,
        trigger_price: Some(90.0),
        ..req("stop-1")
    });
    order.status = status;
    e.registry.insert("stop-1".to_string(), order);
    e.snapshot_state()
}

/// **A candidate carries the terms a venue report lacks**: the `reduce_only` flag and the trigger
/// price of a mount's stop, so the adopted order is still a reduce-only stop at its level; and the
/// kind and the tier of the engine's snapshot ride along.
#[test]
fn a_mount_owned_stop_carries_its_reduce_only_flag_and_trigger_price() {
    let snap = stop_snap(Some(AccountTier::Demo), OrderType::Stop, true, OrderStatus::Accepted);
    let orders = resting_orders(std::slice::from_ref(&snap), None, &[], &owned_by_m(&["stop-1"]));
    assert_eq!(order_client_ids_of(&orders), vec!["stop-1"]);
    assert_eq!(orders[0].mount_id, "m", "its mount, not the pseudo owner");
    assert!(orders[0].reduce_only);
    assert_eq!(orders[0].trigger_price, Some(90.0));
    assert_eq!(orders[0].order_type, Some(OrderType::Stop));
    assert_eq!(orders[0].tier, Some(AccountTier::Demo), "the snapshot's tier, in memory");
}

/// **An operator's resting protective stop becomes a candidate** under the reserved pseudo mount,
/// so the pass adopts it silently instead of leaving it an `UnknownOrder` — and only that shape, on
/// an engine that rests something at a venue.
#[test]
fn an_operators_protective_stop_is_filed_under_the_pseudo_mount_and_nothing_else_is() {
    let demo = Some(AccountTier::Demo);
    let operator = std::collections::HashMap::new();
    let stop = stop_snap(demo, OrderType::Stop, true, OrderStatus::Accepted);
    let orders = resting_orders(std::slice::from_ref(&stop), None, &[], &operator);
    assert_eq!(order_client_ids_of(&orders), vec!["stop-1"]);
    assert_eq!(orders[0].mount_id, PROTECTIVE_STOP_MOUNT);
    assert_eq!((orders[0].reduce_only, orders[0].trigger_price), (true, Some(90.0)));
    assert_eq!(orders[0].venue.as_deref(), Some("sim"));

    let stop_limit = stop_snap(demo, OrderType::StopLimit, true, OrderStatus::PartiallyFilled);
    assert_eq!(
        order_client_ids_of(&resting_orders(
            std::slice::from_ref(&stop_limit),
            None,
            &[],
            &operator
        )),
        vec!["stop-1"],
        "a stop-limit and a partly filled stop are the same shape"
    );

    type Case = (&'static str, Option<AccountTier>, OrderType, bool, OrderStatus);
    // (a resting limit is a candidate of its own: the next test)
    #[rustfmt::skip]
    const NOT_PROTECTIVE: [Case; 7] = [
        ("not reduce-only: it can OPEN a position", Some(AccountTier::Demo), OrderType::Stop, false, OrderStatus::Accepted),
        ("a take-profit is not protection", Some(AccountTier::Demo), OrderType::TakeProfit, true, OrderStatus::Accepted),
        ("not acknowledged: its rest cannot be claimed", Some(AccountTier::Demo), OrderType::Stop, true, OrderStatus::Submitted),
        ("being cancelled", Some(AccountTier::Demo), OrderType::Stop, true, OrderStatus::PendingCancel),
        ("already over", Some(AccountTier::Demo), OrderType::Stop, true, OrderStatus::Canceled),
        ("a paper book is emptied by a restart", Some(AccountTier::Paper), OrderType::Stop, true, OrderStatus::Accepted),
        ("an engine that states no tier is an old or paper one", None, OrderType::Stop, true, OrderStatus::Accepted),
    ];
    for (why, tier, order_type, reduce_only, status) in NOT_PROTECTIVE {
        let snap = stop_snap(tier, order_type, reduce_only, status);
        let orders = resting_orders(std::slice::from_ref(&snap), None, &[], &operator);
        assert!(orders.is_empty(), "{why}: {orders:?}");
    }
}

/// **An operator's resting LIMIT becomes a candidate too** (the control door's order, the CI box
/// 2026-11-01: four such limits came back from a clean restart as `UnknownOrder`, because only a
/// mount's order client id or a protective stop was a candidate): under the pseudo mount, with its price, on an
/// engine that rests something at a venue, reduce-only or not; and only while it rests.
#[test]
fn an_operators_resting_limit_is_filed_under_the_pseudo_mount() {
    let demo = Some(AccountTier::Demo);
    let operator = std::collections::HashMap::new();
    for (why, reduce_only, status) in [
        ("a plain resting limit", false, OrderStatus::Accepted),
        ("a reduce-only limit", true, OrderStatus::Accepted),
        ("a partly filled limit", false, OrderStatus::PartiallyFilled),
    ] {
        let snap = stop_snap(demo, OrderType::Limit, reduce_only, status);
        let orders = resting_orders(std::slice::from_ref(&snap), None, &[], &operator);
        assert_eq!(order_client_ids_of(&orders), vec!["stop-1"], "{why}");
        assert_eq!(orders[0].mount_id, PROTECTIVE_STOP_MOUNT, "{why}");
        assert_eq!(orders[0].price, Some(100.0), "{why}: the journal's price rides along");
        assert_eq!(orders[0].order_type, Some(OrderType::Limit), "{why}");
        assert_eq!(orders[0].tier, demo, "{why}");
    }

    type Case = (&'static str, Option<AccountTier>, OrderType, OrderStatus);
    #[rustfmt::skip]
    const NOT_FILED: [Case; 6] = [
        ("not acknowledged: its rest cannot be claimed", Some(AccountTier::Demo), OrderType::Limit, OrderStatus::Submitted),
        ("being cancelled", Some(AccountTier::Demo), OrderType::Limit, OrderStatus::PendingCancel),
        ("already over", Some(AccountTier::Demo), OrderType::Limit, OrderStatus::Filled),
        ("a market order does not rest", Some(AccountTier::Demo), OrderType::Market, OrderStatus::Accepted),
        ("a paper book is emptied by a restart", Some(AccountTier::Paper), OrderType::Limit, OrderStatus::Accepted),
        ("an engine that states no tier is an old or paper one", None, OrderType::Limit, OrderStatus::Accepted),
    ];
    for (why, tier, order_type, status) in NOT_FILED {
        let snap = stop_snap(tier, order_type, false, status);
        let orders = resting_orders(std::slice::from_ref(&snap), None, &[], &operator);
        assert!(orders.is_empty(), "{why}: {orders:?}");
    }
}

/// The pseudo owner can never be a real mount: a mount id is alphanumerics and `_`
/// (`mount_id_with` sanitizes everything else), and the reserved id is neither.
#[test]
fn the_pseudo_mount_cannot_collide_with_a_real_mount_id() {
    assert!(PROTECTIVE_STOP_MOUNT.chars().any(|c| !(c.is_alphanumeric() || c == '_')));
    let spelled =
        crate::strategy_state::mount_id_with(Some(PROTECTIVE_STOP_MOUNT), "sim", "BTCUSDT", "1m");
    assert_ne!(
        spelled, PROTECTIVE_STOP_MOUNT,
        "a controller id of that spelling is sanitized away"
    );
}
