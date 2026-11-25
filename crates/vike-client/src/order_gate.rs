//! **The ONE order gate both doors call** (the owner's D1-D5 of 2026-11-24): `vike-cli`'s REPL and one-shot verbs
//! and `vike-mcp`'s write tools ask [`judge`] after the order client id is minted and before the confirm, and the MCP
//! asks once more at the send (`Server::execute`), on the control link's real features. A command it refuses never
//! leaves this box: NOT SENT (decision 0303), an [`OrderGateRefusal`], and the same input is refused the same way
//! whichever door it came through. PURE: the node's frame and what the control link said arrive as parameters.
//!
//! # The routing rule, in order
//!
//! 1. A command that names no venue (`CommandWire::addressed_venue` is `None`: a cancel or a modify by id, the
//!    venue-less close-all and mass cancel, a mode change) passes, [`VenueCheck::NoVenue`]: the panic button has no
//!    prerequisite.
//! 2. A blank venue is refused: the node would apply it to whichever book it runs first.
//! 3. A frame that shows the node's engines judges the venue: a named account the node does not mount, a bare venue
//!    of several accounts, running or not (decision 0254, `crate::verbs::bare_venue_refusal`), and a venue the node
//!    does not mount are refused. The compare is EXACT, because routing compares route keys with `==`. A default
//!    account's order on a venue whose lane twin is up is judged as the account the door puts it on
//!    ([`lane_account`]). A venue mounted seconds ago is refused until the frame shows it.
//! 4. No such frame (none read, or one with no engine block yet) is no evidence: refused only against a node that does
//!    not route by venue, which would apply the command to its PRIMARY engine
//!    (`vike_tradehub_client::venue_routing::venue_routing_verdict`), else passed [`VenueCheck::Unverified`]
//!    (decision 0041 verdict 3, amended 2026-11-24).
//! 5. A bare-venue `Submit` to a node without the one-account door is refused: on a venue of several accounts such a
//!    node answers `accepted` and places nothing.
//!
//! A fact the control link has not given ([`NodeLink`]'s `None`, the MCP before its link is open) refuses nothing.
//!
//! # The ceiling
//!
//! `crate::addressed::guardrail_of` measures the order as the node's door does, and one over this box's
//! `policy.max_notional_per_order` is refused by the shared client's one cap
//! (`vike_tradehub_client::order_checks::OrderLimits::check_notional`, the desktop's). What it cannot price (a market
//! order on a linear contract, a symbol the frame carries no value for, no frame), what the door takes for a CLOSE
//! (a covered reduce, an exit list whose every leg is one, a modify that stays one) and every verb that states no size
//! (a cancel, a mass cancel, a flatten, a close-all, a mode change) is never refused here: no ceiling blocks an exit.
//! The quantity cap (`preferences.max_order_qty`) stays a printed line.

use vike_model::accounts::account_keys::{
    RESERVED_DEFAULT_LABEL, parse_wire_account, route_key_of,
};
use vike_tradehub_client::TradehubControlHandle;
use vike_tradehub_client::order_checks::{OrderLimits, OrderLimitsError};
use vike_tradehub_client::venue_routing::{BackendVenueRoutingVerdict, venue_routing_verdict};
use vike_tradehub_client::wire::{CommandWire, CoreSnapshotWire, block_key};

use crate::addressed::guardrail_of;
use crate::refusals::OrderGateRefusal;
use crate::verbs::{self, Guardrail, GuardrailCapabilities};

/// What the control link said of the node, the two facts the routing rule asks. `None` = not known (no control link
/// open yet), and a rule that needs the fact refuses only on `Some(false)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeLink {
    /// `TradehubControlHandle::routes_by_venue`.
    pub routes_by_venue: Option<bool>,
    /// `TradehubControlHandle::has_the_one_account_door`.
    pub one_account_door: Option<bool>,
}

impl NodeLink {
    /// What an open control link advertised.
    #[must_use]
    pub fn of(control: &TradehubControlHandle) -> Self {
        NodeLink {
            routes_by_venue: Some(control.routes_by_venue()),
            one_account_door: Some(control.has_the_one_account_door()),
        }
    }
}

/// Which of the three answers the routing rule gave a command it passed. The MCP says it as its `venue_check`; the
/// CLI does not show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueCheck {
    /// The command names a venue and the node's frame shows it mounted.
    Mounted,
    /// The command names no venue the node could mount, so there was nothing to check.
    NoVenue,
    /// The command names a venue and the frame could not say (rule 4): passed, not checked.
    Unverified,
}

/// Whether a door must read the node's frame before it asks [`judge`]: a command that names a venue (the routing rule
/// reads it) or states a size (the ceiling reads it). Every other verb reads nothing, so `cancel` and the venue-less
/// close-all never wait on a read.
#[must_use]
pub fn reads_the_frame(cmd: &CommandWire) -> bool {
    cmd.addressed_venue().is_some() || verbs::states_a_size(cmd)
}

/// **The verdict on `cmd`** before its confirm: the guardrail each door renders and the routing answer, or the
/// refusal. Routing first, then the ceiling (this module's doc). `frame` is the node's frame or why there is none.
pub fn judge(
    cmd: &CommandWire,
    capabilities: GuardrailCapabilities,
    frame: Result<&CoreSnapshotWire, &str>,
    link: NodeLink,
) -> Result<(Guardrail, VenueCheck), OrderGateRefusal> {
    let check = venue_refusal(cmd, frame.ok(), link)?;
    let guard = guardrail_of(cmd, capabilities, frame);
    match notional_ceiling_refusal(&guard) {
        Some(refusal) => Err(refusal),
        None => Ok((guard, check)),
    }
}

/// The ceiling: a measured notional over this box's cap. An order not priced has no notional and is never refused.
fn notional_ceiling_refusal(guard: &Guardrail) -> Option<OrderGateRefusal> {
    let Guardrail::Order { notional: Some(notional), max_notional, .. } = guard else {
        return None;
    };
    match OrderLimits::with_max_notional(*max_notional).check_notional(*notional) {
        Err(OrderLimitsError::NotionalAboveMax { notional, max }) => {
            Some(OrderGateRefusal::OverMaxNotional { notional, max })
        }
        _ => None,
    }
}

/// Rules 1 to 5 of this module's doc.
fn venue_refusal(
    cmd: &CommandWire,
    frame: Option<&CoreSnapshotWire>,
    link: NodeLink,
) -> Result<VenueCheck, OrderGateRefusal> {
    let Some(venue) = cmd.addressed_venue() else {
        return Ok(VenueCheck::NoVenue);
    };
    if venue.trim().is_empty() {
        return Err(OrderGateRefusal::VenueBlank);
    }
    let check = match frame.map(|frame| judged_on_the_frame(cmd, venue, frame)).transpose()? {
        Some(Some(check)) => check,
        _ => match (link.routes_by_venue, venue_routing_verdict(venue, &[], false)) {
            (Some(false), BackendVenueRoutingVerdict::Refuse(why)) => {
                return Err(OrderGateRefusal::NodeDoesNotRoute { why });
            }
            _ => VenueCheck::Unverified,
        },
    };
    if let CommandWire::Submit(req) = cmd
        && req.account.is_none()
        && link.one_account_door == Some(false)
    {
        return Err(OrderGateRefusal::BareSubmitToOlderNode { venue: req.venue.clone() });
    }
    Ok(check)
}

/// Rule 3: `Some(Mounted)` or the refusal when the frame is evidence, `None` when it is not (no engine block yet and
/// nothing a NOT RUNNING row already shows).
fn judged_on_the_frame(
    cmd: &CommandWire,
    venue: &str,
    frame: &CoreSnapshotWire,
) -> Result<Option<VenueCheck>, OrderGateRefusal> {
    let mounted = mounted_keys(frame);
    if let Some(account) = commanded_account(cmd).or_else(|| lane_account(cmd, mounted.as_deref()))
    {
        let Some(mounted) = mounted else { return Ok(None) };
        // A label that does not parse names no route key, so nothing the node mounts.
        let key = parse_wire_account(account).map(|label| route_key_of(venue, &label));
        if key.is_ok_and(|key| mounted.contains(&key)) {
            return Ok(Some(VenueCheck::Mounted));
        }
        return Err(OrderGateRefusal::AccountNotMounted {
            venue: venue.to_string(),
            account: account.to_string(),
            mounted: mounted.join(", "),
        });
    }
    // Counted over the running AND the NOT RUNNING accounts, so a stopped one does not move what the words reach.
    if let Some(sentence) = verbs::bare_venue_refusal(cmd, frame) {
        return Err(OrderGateRefusal::BareVenueOfSeveralAccounts { sentence });
    }
    let Some(mounted) = mounted else { return Ok(None) };
    if !verbs::venue_keys_on_frame(venue, frame).is_empty() {
        return Ok(Some(VenueCheck::Mounted));
    }
    Err(OrderGateRefusal::VenueNotMounted { venue: venue.to_string(), mounted: mounted.join(", ") })
}

/// **The ROUTE KEYS `frame` shows mounted, one per engine**, or `None` for a frame with no engine block: a node that
/// has not folded publishes `venues: []` (`vike_core`'s `CoreSnapshot::empty`), which is no node that mounts nothing,
/// and its top-level `venue` is only the PRIMARY engine's, never the whole set. An old node omits `route_key`, and
/// `block_key` reads that as the venue, which is a one-account node's route key.
fn mounted_keys(frame: &CoreSnapshotWire) -> Option<Vec<String>> {
    let keys: Vec<String> =
        frame.venues.iter().map(block_key).filter(|key| !key.is_empty()).collect();
    (!keys.is_empty()).then_some(keys)
}

/// WHICH ACCOUNT of `CommandWire::addressed_venue`'s venue this command names, if any. Kept apart from a route key
/// because a NAMED `DEFAULT` (whose key is the bare venue) is never ambiguous, and an absent account is a different
/// row of the routing table.
fn commanded_account(cmd: &CommandWire) -> Option<&str> {
    match cmd {
        CommandWire::Submit(o) => o.account.as_deref(),
        CommandWire::Bracket(b) => b.account.as_deref(),
        CommandWire::OrderList(l) => l.account.as_deref(),
        CommandWire::Combo(c) => c.account.as_deref(),
        CommandWire::MassCancel { account, .. }
        | CommandWire::Flatten { account, .. }
        | CommandWire::MarketExit { account, .. }
        | CommandWire::MountStrategy { account, .. }
        | CommandWire::SetAccountTradingState { account, .. }
        | CommandWire::UnadmitSymbol { account, .. } => account.as_deref(),
        // NO WILDCARD: order-scoped by id, or not an addressing question at all.
        CommandWire::Cancel(_)
        | CommandWire::Modify { .. }
        | CommandWire::SetTradingState(_)
        | CommandWire::UpdateParams { .. }
        | CommandWire::UnmountStrategy { .. }
        | CommandWire::ReconcileConfirm { .. }
        | CommandWire::ReconcileCancel { .. }
        | CommandWire::ResumeVenue { .. }
        | CommandWire::UnlatchMount { .. }
        | CommandWire::SetSetting { .. } => None,
    }
}

/// **The account the NODE gives a command that names none, when its venue runs a lane twin**: one account row's two
/// engines (binance's spot and `binance#PERP`, `vike_venues::lane_twin`) are no two accounts, and the door puts a
/// perpetual order of the default account on the twin and every other order, and an account-less mount, on the
/// default engine (`vike-tradehub`'s `lane_addressed`). `None` unless the command is an order or a mount naming no
/// account and BOTH engines are mounted.
fn lane_account(cmd: &CommandWire, mounted: Option<&[String]>) -> Option<&'static str> {
    let mounted = mounted?;
    let (venue, symbol) = match cmd {
        CommandWire::Submit(o) if o.account.is_none() => {
            (o.venue.as_str(), Some(o.symbol.as_str()))
        }
        CommandWire::Bracket(b) if b.account.is_none() => {
            (b.venue.as_str(), Some(b.symbol.as_str()))
        }
        CommandWire::OrderList(l) if l.account.is_none() => {
            (l.venue.as_str(), Some(l.symbol.as_str()))
        }
        CommandWire::MountStrategy { venue, account: None, .. } => (venue.as_str(), None),
        _ => return None,
    };
    let twin = vike_venues::lane_twin(venue)?;
    if !mounted.iter().any(|m| m == venue) || !mounted.contains(&twin.route_key()) {
        return None;
    }
    let perp = symbol.is_some_and(|symbol| vike_catalog::on_futures_lane(venue, symbol));
    Some(if perp { twin.label } else { RESERVED_DEFAULT_LABEL })
}

#[path = "order_gate_tests.rs"]
#[cfg(test)]
mod order_gate_tests;
