//! The un-latch of a mount's budget at the control door (`WireCommand::UnlatchMount`, decision 0239).
//!
//! Two layers, as the give-back's suite has. The PLAN ([`plan_unlatch`]) is a pure function of the published mount
//! rows, so each rule has a row and each refusal is read in words: an empty id, a mount the node does not run (the
//! refusal lists the ones it does), a mount that is not latched, a mount that still holds a position (saying
//! whether its budget flattens it or is cancel-only), the sanitized id, the published spelling on the frame, and a
//! node with no rows to judge by that lets the core answer. The THROUGH-`accept_command` tests drive the one
//! acceptance path into a REAL core (a paper maker core supplies the sink): a refusal is answered BEFORE the `Ack`
//! and reaches the core never, and a latched flat mount is accepted and audited.

use super::control::{ControlLimits, ControlLimitsConfig, accept_command};
use crate::server::unlatch::{UnlatchPlan, plan_unlatch};
use vike_exec::{MountBudget, MountRowKind, MountView};
use vike_tradehub_client::wire::WireCommand;

fn unlatch(mount_id: &str) -> WireCommand {
    WireCommand::UnlatchMount { mount_id: mount_id.into() }
}

fn row(mount_id: &str, latched: bool, position: f64, flatten: bool) -> MountView {
    MountView {
        kind: MountRowKind::Mount,
        mount_id: mount_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        ready: true,
        position,
        realized_pnl: -60.0,
        unrealized_pnl: 0.0,
        notional: 0.0,
        budget: Some(MountBudget {
            max_loss: Some(50.0),
            max_notional: None,
            flatten_on_breach: flatten,
        }),
        latched,
        params: None,
        ccy: Default::default(),
    }
}

fn refusal(plan: UnlatchPlan) -> String {
    match plan {
        UnlatchPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn every_other_verb_passes_the_gate_untouched() {
    let rows = [row("m_a", true, 0.0, true)];
    assert_eq!(plan_unlatch(&WireCommand::Cancel("c".into()), &rows), UnlatchPlan::Pass);
    assert_eq!(
        plan_unlatch(&WireCommand::UnmountStrategy { controller_id: "m_a".into() }, &rows),
        UnlatchPlan::Pass
    );
}

#[test]
fn a_latched_flat_mount_is_sent_under_its_published_id() {
    let rows = [row("binance__BTCUSDT__1m", true, 0.0, true), row("m_b", false, 0.0, false)];
    assert_eq!(
        plan_unlatch(&unlatch("binance__BTCUSDT__1m"), &rows),
        UnlatchPlan::Send { mount_id: "binance__BTCUSDT__1m".into() }
    );
    // A controller id is read the way the core stores it: `m-a` is `m_a`.
    let rows = [row("m_a", true, 0.0, true)];
    assert_eq!(
        plan_unlatch(&unlatch(" m-a "), &rows),
        UnlatchPlan::Send { mount_id: "m_a".into() },
        "the frame carries the PUBLISHED spelling"
    );
}

#[test]
fn an_empty_id_and_an_unknown_mount_are_refused_in_words() {
    let rows = [row("m_a", true, 0.0, true), row("m_b", true, 0.0, true)];
    assert!(refusal(plan_unlatch(&unlatch("  "), &rows)).contains("empty mount id"));
    let said = refusal(plan_unlatch(&unlatch("nope"), &rows));
    assert!(
        said.contains("`nope`") && said.contains("m_a, m_b"),
        "the mounts it runs are named: {said}"
    );
    assert!(said.contains("trade strategy ls"), "{said}");
}

#[test]
fn a_mount_that_is_not_latched_is_refused() {
    let rows = [row("m_a", false, 0.0, true)];
    let said = refusal(plan_unlatch(&unlatch("m_a"), &rows));
    assert!(said.contains("is not latched") && said.contains("nothing to clear"), "{said}");
    assert!(said.contains("binance/BTCUSDT"), "the mount is named by its series: {said}");
}

#[test]
fn a_mount_that_holds_a_position_is_refused_and_the_words_say_what_closes_it() {
    let flattening = [row("m_a", true, 1.5, true)];
    let said = refusal(plan_unlatch(&unlatch("m_a"), &flattening));
    assert!(said.contains("still holds 1.5") && said.contains("once it is flat"), "{said}");
    assert!(said.contains("offers the close again at every sweep"), "{said}");

    let cancel_only = [row("m_a", true, -2.0, false)];
    let said = refusal(plan_unlatch(&unlatch("m_a"), &cancel_only));
    assert!(said.contains("still holds -2") && said.contains("cancel-only"), "{said}");
    assert!(said.contains("by hand") && said.contains("trade position flatten"), "{said}");

    // Dust below the engine's own tolerance is flat.
    let dust = [row("m_a", true, 1e-13, true)];
    assert!(matches!(plan_unlatch(&unlatch("m_a"), &dust), UnlatchPlan::Send { .. }));
}

#[test]
fn a_node_with_no_mount_rows_lets_the_core_answer_and_the_residual_row_is_no_mount() {
    assert_eq!(
        plan_unlatch(&unlatch("m_a"), &[]),
        UnlatchPlan::Pass,
        "nothing published to judge by"
    );
    // `named_mounts` leaves the residual row out, so a command naming no mount cannot match it.
    let mut residual = row("", true, 0.0, true);
    residual.kind = MountRowKind::Residual;
    let rows = crate::publish::named_mounts(
        &unlatch("m_a"),
        &[residual.clone(), row("m_a", true, 0.0, true)],
    );
    assert_eq!(rows.len(), 1, "the residual row is left out");
    assert_eq!(rows[0].mount_id, "m_a");
    assert!(
        crate::publish::named_mounts(&WireCommand::Cancel("c".into()), &[residual]).is_empty(),
        "a command that names no mount clones nothing"
    );
}

/// The verb through `accept_command` into a real core: a refusal is answered in the door's words and never
/// reaches the core; a latched flat mount is accepted (an `Ack` with no coid), and the frame the core is sent
/// carries the published id. The paper core only supplies a real `CommandSink`.
#[test]
fn the_door_refuses_before_the_ack_and_accepts_a_latched_flat_mount() {
    let mount = vike_tradehub_core::build_paper_maker_core(
        &vike_tradehub_core::MakerConfig::outcome_token(
            "polymarket",
            "UNLATCH_DOOR_TOKEN",
            Some(3_000_000_000),
        ),
    );
    let sink = mount.handle.command_sink();
    let mut limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: None, rate_per_sec: 1e9 });
    let mut send = |cmd: WireCommand, mounts: &[MountView]| {
        accept_command(
            cmd,
            None,
            &mut limits,
            &sink,
            None,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            mounts,
            None,
            None,
            None,
        )
        .map(crate::server::control::Accepted::into_coid)
        .map_err(|e| e.message())
    };

    let free = [row("m_a", false, 0.0, true)];
    let err = send(unlatch("m_a"), &free).expect_err("not latched: refused before the Ack");
    assert!(err.contains("is not latched"), "{err}");
    let open = [row("m_a", true, 1.0, true)];
    let err = send(unlatch("m_a"), &open).expect_err("a position: refused before the Ack");
    assert!(err.contains("still holds"), "{err}");

    let ready = [row("m_a", true, 0.0, true)];
    let coid = send(unlatch("m-a"), &ready).expect("a latched flat mount is accepted");
    assert_eq!(coid, "", "the account-wide-verb convention: no coid to echo");
    // An empty id never lowers, whatever the rows say.
    assert!(send(unlatch(" "), &ready).is_err());
}
