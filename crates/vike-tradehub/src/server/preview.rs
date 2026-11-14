//! `server::preview` — **the ONE dry run**: what a command would get from
//! [`accept_command`](super::control::accept_command), asked without sending it. Read-only: nothing is lowered,
//! nothing reaches the core, nothing is audited as executed, no price feed is subscribed and no grid refreshed.
//!
//! MCP calls this answer "the verdict that counts", so it answers what the send will answer, in the send's order of
//! gates. It is the wire Preview (`crates/vike-tradehub/src/server/session.rs`'s `serve_session`), the one preview:
//! the Telegram channel (`crates/vike-telegram`) is a control-wire client and asks it like any other peer.
//! `accept_command` stays apart because it commits; `crates/vike-tradehub/src/server/tests/preview_door.rs` runs the
//! two over every wire verb.
//!
//! **What a preview cannot say**, each for its own reason:
//! - what only a commit learns: the first price of a held order (the send waits for it and may refuse;
//!   `symbol_admission.rs`'s `no_price_note` says so), a refresh that falls due and the exchange's leverage answer,
//!   and whether a release's price venue can bring a price at all (the send asks that engine, committing);
//! - a settings write's verdict (the loader judges the value, and it writes as it judges);
//! - an order frame `lower_command` refuses as malformed (a blank order client id, an unknown order word): no verdict
//!   about the order. What it refuses of a mount is one, so a preview owes that (`mount_refusal`), and so are an
//!   order's terms (a stop with no trigger, an attribute or an adjustment its rule refuses), so a preview says those
//!   too (`submit_terms_refusal`) when they are the lowering's first fault. A lifetime it does not judge: a frame whose lifetime is refused first gets no verdict, as
//!   above, never the order-terms one.

use std::sync::Arc;

use vike_execution::{ArmingNotRunningView, MountView, OrderView, VenueBlock};
use vike_model::OrderType;
use vike_tradehub_client::wire::CommandWire;

use super::control::{ControlLimits, lower_command, mount_refusal, submit_terms_refusal};
use super::refusal::{
    awaiting_balance_refusal, lane_twin_closing_refusal, modify_price_refusal,
    one_account_addressed, order_client_id_reuse_refusal, release_symbol_refusal,
    release_venue_refusal, submit_side_refusal,
};
use super::symbol_admission::{
    SymbolAdmitter, SymbolPlan, admission_refusal_while_not_active, foreign_contract_value,
    held_grid_refusal, lane_addressed, live_twin_unsized_open, needs_lookup, order_closes,
    outcome_name_resolved, plan_submit_symbol, preview_notes, priced_for_sizing,
    submit_symbol_refusal,
};
use super::unadmit::{UnadmitPlan, plan_unadmit};
use super::unlatch::{UnlatchPlan, plan_unlatch};
use super::{account_refusal, bracket_refusal, not_running_refusal, venue_refusal};
use crate::publish::PublisherHandle;

/// What a preview reads, off the snapshot cell its surface holds: the same reads `accept_command` is handed.
pub(crate) trait DoorReads {
    /// The venues this node runs an engine for.
    fn engine_venues(&self) -> Vec<String>;
    /// One route key per account that runs.
    fn engine_route_keys(&self) -> Vec<String>;
    /// The newest engine block of each route key.
    fn engine_blocks(&self) -> Vec<VenueBlock>;
    /// The accounts that do not run now.
    fn not_running(&self) -> Arc<Vec<ArmingNotRunningView>>;
    /// The orders `cmd` names.
    fn orders_named_by(&self, cmd: &CommandWire) -> Vec<OrderView>;
    /// The published mount rows `cmd` is judged against.
    fn mounts_named_by(&self, cmd: &CommandWire) -> Vec<MountView>;
    /// Where a new symbol's facts come from.
    fn symbol_admitter(&self) -> Option<&dyn SymbolAdmitter>;
}

impl DoorReads for PublisherHandle {
    fn engine_venues(&self) -> Vec<String> {
        PublisherHandle::engine_venues(self)
    }
    fn engine_route_keys(&self) -> Vec<String> {
        PublisherHandle::engine_route_keys(self)
    }
    fn engine_blocks(&self) -> Vec<VenueBlock> {
        PublisherHandle::engine_blocks(self)
    }
    fn not_running(&self) -> Arc<Vec<ArmingNotRunningView>> {
        PublisherHandle::not_running(self)
    }
    fn orders_named_by(&self, cmd: &CommandWire) -> Vec<OrderView> {
        PublisherHandle::orders_named_by(self, cmd)
    }
    fn mounts_named_by(&self, cmd: &CommandWire) -> Vec<MountView> {
        PublisherHandle::mounts_named_by(self, cmd)
    }
    fn symbol_admitter(&self) -> Option<&dyn SymbolAdmitter> {
        PublisherHandle::symbol_admitter(self)
    }
}

/// A preview's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewVerdict {
    /// Why the send would refuse, or `None`: it would pass.
    pub(crate) refusal: Option<String>,
    /// What the node says of a command it would pass ([`preview_notes`]). Empty beside a refusal.
    pub(crate) notes: Vec<String>,
}

/// One read of the rosters, so every step of one verdict judges the same frame: the ceiling sizes with the engine's
/// contract value and the bracket verdict reads what it trades.
struct Roster<'a> {
    blocks: Vec<VenueBlock>,
    route_keys: Vec<String>,
    not_running: Arc<Vec<ArmingNotRunningView>>,
    facts: Option<&'a dyn SymbolAdmitter>,
}

/// **The verdict `cmd` would get at the door.** `limits` is the session's own, the bucket its send draws on: a preview
/// takes no token but a lookup's.
pub(crate) fn preview_verdict(
    cmd: &CommandWire,
    reads: &dyn DoorReads,
    limits: &mut ControlLimits,
) -> PreviewVerdict {
    let refused = |refusal| PreviewVerdict { refusal: Some(refusal), notes: Vec::new() };
    let roster = Roster {
        blocks: reads.engine_blocks(),
        route_keys: reads.engine_route_keys(),
        not_running: reads.not_running(),
        facts: reads.symbol_admitter(),
    };
    let Roster { blocks, facts, .. } = &roster;
    let facts = *facts;
    // Addressed as the command will be, so the dry run and the send judge one address.
    let addressed = match submit_side_refusal(cmd).map_or_else(
        || {
            one_account_addressed(
                cmd.clone(),
                &roster.route_keys,
                &roster.not_running,
                blocks,
                facts,
            )
        },
        Err,
    ) {
        Ok(addressed) => addressed,
        Err(refusal) => return refused(refusal),
    };
    let mut cmd = lane_addressed(addressed, blocks, facts);
    let orders = reads.orders_named_by(&cmd);
    // A preview that costs the facts source a lookup (a gatewayhub round trip for a symbol the engine does not trade
    // yet) takes a rate token, like a command: otherwise a peer could flood lookups for free and starve the real
    // orders of the daemon's few lookup slots.
    if needs_lookup(&cmd, blocks)
        && let Some(refusal) = limits.vet_rate()
    {
        return refused(refusal);
    }
    // A market NAME on its token id, as the send has it before any gate asks whether it closes.
    if let Err(refusal) = outcome_name_resolved(&mut cmd, blocks, facts) {
        return refused(refusal);
    }
    match door_refusal(&cmd, &orders, &roster, reads, limits) {
        Some(refusal) => refused(refusal),
        None => {
            PreviewVerdict { refusal: None, notes: preview_notes(&cmd, &orders, blocks, facts) }
        }
    }
}

/// `accept_command`'s refusals of an addressed command, in its order: the ceiling at what the frame names, the routing
/// gates, the verbs that judge a published row, the symbol plan, the ceiling at the market, then what the plan
/// would commit and what the order's own grid says. ⚠ A close passes every one of them that it passes at the send.
fn door_refusal(
    cmd: &CommandWire,
    orders: &[OrderView],
    roster: &Roster<'_>,
    reads: &dyn DoorReads,
    limits: &ControlLimits,
) -> Option<String> {
    let (blocks, facts) = (&roster.blocks[..], roster.facts);
    let foreign =
        if limits.has_ceiling() { foreign_contract_value(cmd, blocks, facts) } else { Ok(None) };
    // `Err`: the door refuses the symbol, so the ceiling does not size it, and its refusal is said below in its place.
    if let Ok(value) = &foreign
        && let Some(refusal) = limits.preview_vet_foreign(cmd, blocks, orders, *value)
    {
        return Some(refusal);
    }
    let gated = not_running_refusal(cmd, &roster.not_running)
        .or_else(|| awaiting_balance_refusal(cmd, blocks, facts))
        .or_else(|| lane_twin_closing_refusal(cmd, orders, blocks, facts))
        .or_else(|| venue_refusal(cmd, &reads.engine_venues()))
        .or_else(|| account_refusal(cmd, &roster.route_keys))
        .or_else(|| order_client_id_reuse_refusal(cmd, orders, limits.boot_order_client_ids()))
        .or_else(|| modify_price_refusal(cmd, orders))
        .or_else(|| release_symbol_refusal(cmd, blocks))
        // The price venue's engine is judged, never asked: the send's committing price ask opens a lane.
        .or_else(|| release_venue_refusal(cmd, blocks).err())
        .or_else(|| bracket_refusal(cmd, blocks))
        .or_else(|| super::order_list::order_list_refusal(cmd, blocks))
        .or_else(|| super::refusal::combo_refusal(cmd))
        .or_else(|| match plan_unadmit(cmd, blocks, orders) {
            UnadmitPlan::Refuse(why) => Some(why),
            UnadmitPlan::Pass | UnadmitPlan::Release { .. } => None,
        })
        .or_else(|| match plan_unlatch(cmd, &reads.mounts_named_by(cmd)) {
            UnlatchPlan::Refuse(why) => Some(why),
            UnlatchPlan::Pass | UnlatchPlan::Send { .. } => None,
        })
        // The send's step 1e lowers the order here. Of its refusals the dry run says only the order terms, and only
        // when they are the lowering's FIRST fault: a frame malformed before them (a bad lifetime, a blank id) keeps
        // the documented no-verdict below rather than be told a fault the send would not name first.
        .or_else(|| match cmd {
            CommandWire::Submit(req) => {
                let terms = OrderType::parse(&req.order_type)
                    .ok()
                    .and_then(|kind| submit_terms_refusal(&kind, req))?;
                lower_command(cmd.clone()).err().filter(|first| *first == terms)
            }
            _ => None,
        });
    if gated.is_some() {
        return gated;
    }
    let foreign = match foreign {
        Ok(value) => value,
        Err(why) => return Some(why),
    };
    let mut planned = cmd.clone();
    let mut admit = None;
    match plan_submit_symbol(cmd, blocks, facts, false) {
        SymbolPlan::Pass => {}
        SymbolPlan::Refuse(why) => return Some(why),
        SymbolPlan::Place { symbol, admit: admission, .. } => {
            // A live twin's own symbol sized at the price the door holds, as the send sizes it; no price yet is the
            // send's to wait for.
            if limits.has_ceiling()
                && let Some((route_key, own)) = live_twin_unsized_open(cmd, blocks, facts)
                && let Some(venue) = cmd.addressed_venue()
                && let Some(px) = facts
                    .and_then(|source| source.price(&route_key, venue, &own, false).ok().flatten())
                    .filter(|px| px.is_finite() && *px > 0.0)
                && let Some(sized) = priced_for_sizing(cmd, px)
                && let Some(refusal) = limits.preview_vet(&sized, blocks, orders)
            {
                return Some(refusal);
            }
            if let CommandWire::Submit(req) = &mut planned {
                req.symbol = symbol;
            }
            admit = admission;
        }
    }
    if let Some(refusal) = limits.market_reason(&planned, blocks, orders, foreign, facts, None) {
        return Some(refusal);
    }
    // A close goes without the admission, so nothing here can hold one up.
    if let Some(admission) = admit.as_deref()
        && !matches!(&planned, CommandWire::Submit(req) if order_closes(req, blocks))
        && let Some(refusal) = admission_refusal_while_not_active(admission, blocks)
    {
        return Some(refusal);
    }
    submit_symbol_refusal(cmd, blocks, facts)
        // The step that reads the order a `Modify` names (a Submit's was judged just above).
        .or_else(|| held_grid_refusal(cmd, None, orders, blocks, facts))
        // A mount's profile refusals and its `rhai` path (which asks the filesystem and opens nothing).
        .or_else(|| mount_refusal(cmd))
}
