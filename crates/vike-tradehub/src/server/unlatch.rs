//! `server::unlatch` — the control door's gate for the UN-LATCH of one mount's budget
//! (`WireCommand::UnlatchMount`, decision 0239).
//!
//! A mount that breached its `MountBudget` is latched liquidate-only for good. The verb clears it when the mount is
//! flat and quiet and re-arms its loss budget from where it stands. This module is the edge's half: it judges the
//! frame against the PUBLISHED mount rows BEFORE the `Ack`, so the operator reads the cause in words instead of a
//! note in recent-events.
//!
//! * **It is advice, the core is the authority.** The published rows carry the mount's latch flag and attributed
//!   position, not its live orders or armed conditionals (a mount's order rows are not attributed in the
//!   snapshot), so a frame this gate lets through can still be refused by the core, in a recent-events note and a
//!   WARN, and the CLI says `sent`, not `accepted`.
//! * **What it refuses**: an empty id, a mount the node does not run (the refusal lists the ones it does), a mount
//!   that is not latched, and a mount that still holds a position, saying whether its budget flattens it (the
//!   latch offers the close again at every sweep) or is cancel-only (the operator closes it by hand).
//! * **What it does not ask**: the tier and the trading state. An un-latch writes nothing at a venue, and every
//!   order the mount then emits passes the ordinary gates and its re-armed budget.
//! * **The core's spelling.** The id the operator typed is sanitized as the core stores it (`maker-a` finds
//!   `maker_a`) and the command carries the PUBLISHED id.

use vike_exec::MountView;
use vike_tradehub_client::wire::WireCommand;

/// What the gate decided for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlatchPlan {
    /// Not an un-latch (every other verb), or a node that cannot judge it (no mount row published, which also
    /// covers a node that runs none): the frame goes on, and the core's verdict stands.
    Pass,
    /// Refused before the `Ack`, with the cause in words.
    Refuse(String),
    /// Send it: `mount_id` is the published spelling of the mount's id.
    Send {
        /// The id as the mount rows publish it.
        mount_id: String,
    },
}

/// Judge `cmd` against the published `mounts` ([`UnlatchPlan`]). Pure: reads nothing but its arguments, so a
/// Preview surface may call it too.
#[must_use]
pub fn plan_unlatch(cmd: &WireCommand, mounts: &[MountView]) -> UnlatchPlan {
    let WireCommand::UnlatchMount { mount_id } = cmd else {
        return UnlatchPlan::Pass;
    };
    let typed = mount_id.trim();
    if typed.is_empty() {
        return UnlatchPlan::Refuse("unlatch: an empty mount id names no mount".into());
    }
    if mounts.is_empty() {
        // Nothing published (a core that has not published its mounts, or one that runs none): the core answers,
        // as it does for every verb this gate has no evidence about.
        return UnlatchPlan::Pass;
    }
    let want = vike_core::strategy_state::mount_id_with(Some(typed), "", "", "");
    let Some(m) = mounts.iter().find(|m| m.mount_id == want) else {
        let ids: Vec<&str> = mounts.iter().map(|m| m.mount_id.as_str()).collect();
        return UnlatchPlan::Refuse(format!(
            "`{typed}` is not a mount this node runs ({}); `vike-cli trade strategy ls` lists them",
            ids.join(", ")
        ));
    };
    let (id, venue, symbol) = (&m.mount_id, &m.venue, &m.symbol);
    if !m.latched {
        return UnlatchPlan::Refuse(format!(
            "mount `{id}` ({venue}/{symbol}) is not latched: there is nothing to clear"
        ));
    }
    if m.position.abs() > 1e-12 {
        let how = if m.budget.is_some_and(|b| b.flatten_on_breach) {
            "its budget flattens it: the latch offers the close again at every sweep (the 30 s clock and every \
             closed bar) until it is flat"
        } else {
            "its budget is cancel-only (`flatten_on_breach` is off), so nothing closes it: close it by hand \
             (`vike-cli trade position flatten`)"
        };
        return UnlatchPlan::Refuse(format!(
            "mount `{id}` ({venue}/{symbol}) still holds {}: unlatch it once it is flat; {how}",
            m.position
        ));
    }
    UnlatchPlan::Send { mount_id: m.mount_id.clone() }
}
