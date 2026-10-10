//! White-box tests of [`fold_coid_mounts`] — the pure coid -> mount-id fold behind attribution
//! durability (gap D). The end-to-end restore is gated in `tests/mount_attr_durability.rs`;
//! these pin the per-record pairing rule (open / pair / close) in isolation.

use super::*;

fn req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

fn strategy_submit(seq: u64, mount_id: &str, coid: &str) -> JournalRecord {
    JournalRecord::StrategySubmit {
        seq,
        now_ms: seq as i64,
        mount_id: mount_id.into(),
        // the runtime always journals the resolved intent with an EMPTY coid (the mint happens
        // inside `apply_intent`, after this record) unless the caller supplied one
        intent: vike_exec::OrderIntent::Submit(Box::new(req(coid))),
    }
}

fn minted(seq: u64, coid: &str) -> JournalRecord {
    JournalRecord::MintedSubmit { seq, now_ms: seq as i64, req: req(coid), route_key: None }
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
        fold_coid_mounts(&recs),
        vec![
            ("c1".to_string(), "maker-a".to_string()),
            ("c2".to_string(), "maker-b".to_string()),
            ("c3".to_string(), "maker-b".to_string()),
        ]
    );
}

/// A NON-strategy write closes ownership, so an operator command's minted coid — and a
/// runtime-decided liquidation's — stay UNATTRIBUTED rather than being credited to whichever
/// mount happened to submit last.
#[test]
fn a_non_strategy_write_closes_ownership() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "c1"),
        cmd_order(3, vike_exec::OrderIntent::Submit(Box::new(req("")))),
        minted(4, "operator-coid"),
        JournalRecord::MarginCallLiquidate {
            seq: 5,
            now_ms: 5,
            req: req(""),
            mount_id: None,  // the ACCOUNT-wide margin-call sweep — owned by no mount
            route_key: None, // one account of this venue, so the venue id IS the route key
        },
        minted(6, "liquidation-coid"),
    ];
    assert_eq!(
        fold_coid_mounts(&recs),
        vec![("c1".to_string(), "maker-a".to_string())],
        "only the mount-minted order is attributed"
    );
}

/// **THE v14 ARM.** A `MarginCallLiquidate` that NAMES a mount — the per-mount budget latch's
/// flatten — OPENS ownership like a `StrategySubmit`, so the coid `apply_intent` mints for it
/// restores into THAT mount's ledger.
///
/// Without the id (every pre-v14 frame, and the account-wide sweep above) the flatten's coid came
/// back unattributed and its fill landed in the RESIDUAL row — under-reporting the exact realized
/// loss the budget latch exists to bound.
#[test]
fn an_owned_margin_call_liquidate_attributes_its_flatten_to_that_mount() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "c1"), // the mount's own resting order
        JournalRecord::MarginCallLiquidate {
            seq: 3,
            now_ms: 3,
            req: req(""),
            mount_id: Some("maker-a".into()), // the BUDGET LATCH's flatten
            route_key: None, // one account of this venue, so the venue id IS the route key
        },
        minted(4, "flatten-coid"),
    ];
    assert_eq!(
        fold_coid_mounts(&recs),
        vec![
            ("c1".to_string(), "maker-a".to_string()),
            ("flatten-coid".to_string(), "maker-a".to_string()),
        ],
        "the latch's flatten belongs to the mount whose budget breach released it"
    );
}

/// A client-SUPPLIED coid writes no `MintedSubmit` (it skips the mint), so the intent itself is
/// the only place its origin exists.
#[test]
fn a_client_supplied_coid_maps_from_the_intent() {
    let recs = vec![strategy_submit(1, "maker-a", "given-coid")];
    assert_eq!(fold_coid_mounts(&recs), vec![("given-coid".to_string(), "maker-a".to_string())]);
}

fn armed(seq: u64, arm_id: &str) -> JournalRecord {
    JournalRecord::ConditionalArmed {
        seq,
        now_ms: seq as i64,
        arm_id: arm_id.into(),
        resolved: vike_journal::ConditionalRecord {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(90.0),
            trail: None,
            extreme: None,
            trigger_by: None,
        },
    }
}

fn fired(seq: u64, arm_id: &str) -> JournalRecord {
    JournalRecord::ConditionalFire {
        seq,
        now_ms: seq as i64,
        arm_id: arm_id.into(),
        trigger_px: 90.0,
        req: req(""),
    }
}

/// **A FIRED STOP BELONGS TO THE MOUNT THAT ARMED IT.** A `ConditionalArmed` journaled while a
/// mount's `StrategySubmit` is open records that arm's owner; the later `ConditionalFire` REOPENS
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
        minted(6, "fired-coid"),
    ];
    assert_eq!(
        fold_coid_mounts(&recs),
        vec![
            ("c-b".to_string(), "maker-b".to_string()),
            ("fired-coid".to_string(), "maker-a".to_string()),
        ],
        "the fire is credited to the ARMING mount, not to the last submitter"
    );
}

/// An OPERATOR-armed conditional (its arm sits under a `Cmd`, which closed ownership) stays
/// unattributed when it fires, and so does a fire whose arm was disarmed or never seen (its
/// `ConditionalArmed` sat in a pruned segment): the fold never guesses an owner.
#[test]
fn an_operator_armed_conditional_fires_unattributed() {
    let recs = vec![
        strategy_submit(1, "maker-a", ""),
        minted(2, "c-a"),
        cmd_order(3, vike_exec::OrderIntent::Submit(Box::new(req("")))), // the operator arms
        armed(4, "arm-op"),
        fired(5, "arm-op"),
        minted(6, "operator-fired"),
        strategy_submit(7, "maker-a", ""),
        armed(8, "arm-2"),
        JournalRecord::ConditionalDisarmed { seq: 9, now_ms: 9, arm_id: "arm-2".into() },
        fired(10, "arm-2"), // a fire after its own disarm names no live owner
        minted(11, "ghost-fired"),
        fired(12, "arm-pruned"), // its ConditionalArmed was in a pruned segment: not in view
        minted(13, "pruned-fired"),
    ];
    assert_eq!(
        fold_coid_mounts(&recs),
        vec![("c-a".to_string(), "maker-a".to_string())],
        "only the mount-minted order is attributed"
    );
}

fn cmd_order(seq: u64, intent: vike_exec::OrderIntent) -> JournalRecord {
    JournalRecord::Cmd {
        seq,
        now_ms: seq as i64,
        msg: Ingest::Command(vike_exec::Command::Order(intent)),
    }
}
