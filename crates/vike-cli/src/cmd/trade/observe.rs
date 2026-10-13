//! The REPL's observe-side reads: `status`, and the snapshot every other read verb renders from.

use vike_tradehub_client::wire::WireSnapshot;

use crate::cmd::nodekeys;

use super::describe::print_state;
use super::{Session, pre_fold_line, wait_for_first_frame};

/// `status` at the REPL: the trading MODE from the snapshot this session is already subscribed to,
/// then the mounted-strategy REGISTRY from one short-lived Observe request
/// ([`vike_tradehub_client::strategy_status`]).
///
/// ⚠ **The two halves fail independently and the mode is shown even when the registry read fails.**
/// The one-shot `vike-cli trade status` now behaves the same way — it re-asks for the mode after a
/// registry failure the node could still answer ([`crate::cmd::trade::status`]'s
/// `mode_read_worth_attempting`) and keeps the registry's exit code — so the two surfaces no longer
/// differ on WHAT is shown, only on the exit code a REPL does not have. It is here that the rule is
/// cheapest to obey and hardest to argue against: the mode is ALREADY IN HAND — the node pushed it
/// — so withholding it because a second request failed would be discarding an answer the operator
/// has, at exactly the moment (an old node, a half-open link) they most want to know whether
/// trading is halted.
pub(super) fn run_status(session: &Session<'_>) {
    with_snapshot(session, print_state);
    let Some((key, _origin)) = session.keys.observe() else {
        // Unreachable in practice: `with_snapshot` above already said reads are disabled, and it
        // says it better. Kept so the registry half never silently prints nothing.
        return;
    };
    // ⚠ `_with_features` rather than `strategy_status`, and the same call underneath: the PRODUCT
    // column can only be rendered honestly by a reader that knows whether the node advertised
    // `FEATURE_MOUNT_CLASS`. An absent class means "this node is too old to say" or "this mount is
    // not migrated yet", and only the capability tells them apart.
    match vike_tradehub_client::strategy_status_with_features(session.node.as_str(), key.as_bytes())
    {
        Ok((status, features)) => {
            let knows_class =
                features.iter().any(|f| f == vike_tradehub_client::proto::FEATURE_MOUNT_CLASS);
            for line in crate::cmd::trade::status::registry_lines(&status, knows_class) {
                println!("{line}");
            }
        }
        // Printed, not returned: a REPL has no exit code, and the mode above already landed.
        Err(e) => {
            for line in crate::cmd::trade::status::failure_lines(&session.node, &e) {
                println!("{line}");
            }
        }
    }
}

/// Fetch the freshest snapshot (waiting briefly for the node's first frame) and hand it to `f`.
/// Reports cleanly when the observe half is disabled or disconnected, and MARKS a frame that
/// carries nothing built yet ([`pre_fold_line`]) before `f` renders its placeholders.
pub(super) fn with_snapshot(session: &Session<'_>, f: impl FnOnce(&WireSnapshot)) {
    let Some(observe) = session.observe.as_ref() else {
        println!(
            "reads disabled: no {} — start with an observe key to read node state",
            nodekeys::OBSERVE_KEY_ENV
        );
        return;
    };
    let snap = wait_for_first_frame(observe);
    if !observe.is_connected() {
        println!("[warning] observe connection is down — showing the last snapshot seen");
    }
    if let Some(line) = pre_fold_line(&snap) {
        println!("{line}");
    }
    f(&snap);
}
