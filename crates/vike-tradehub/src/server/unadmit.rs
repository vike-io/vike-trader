//! `server::unadmit` — the control door's gate for the GIVE-BACK of an admitted symbol
//! (`WireCommand::UnadmitSymbol`, decision 0205).
//!
//! An account's engine takes a symbol on demand (decision 0173) and holds sixteen at most. The verb gives one
//! back: the engine takes it out (`SymbolAdmission::release`, journaled as an `AdmitSymbol` record with one more
//! key), the gatewayhub's reconcile client stops reading it and the node forgets what it kept for it
//! (`LeaseFacts::settle_releases`). This module is the edge's half: it judges the frame against the PUBLISHED
//! snapshot BEFORE the `Ack`, so the operator reads the cause in words instead of a note in recent-events.
//!
//! * **It is advice, the engine is the authority.** The published snapshot is coalesced and the core keeps books
//!   outside the engine's fenced state (an armed conditional, a held exit); the engine refuses by its own folded
//!   state (`ExecutionEngine::unadmit_symbol`), so a frame this gate lets through can still be refused there, and
//!   the node then finds the symbol still listed (`LeaseFacts::settle_releases`) and forgets nothing.
//! * **What it refuses**: a symbol the engine's block does not list as one of its extra symbols (the engine's own
//!   symbol is no slot), a non-flat position in it on any leg, and any order in it that has not ended, an armed
//!   conditional included (the published orders carry it as `Emulated`). The orders arrive through
//!   `crate::publish::named_orders`, which names them for this verb; they are matched to the account by
//!   `route_key_of(order.venue, order.account)`, the spelling every door gate uses, so a LABELLED engine
//!   (`bybit#DEMO`) is judged like the default one. ⚠ A bracket exit held off the venue and a core-held stop are
//!   NOT in the published orders (`HeldOrderView`, `CoreHeldStopView`): this gate does not see them (the unfilled
//!   entry order or the position they hang on refuses first), and the core denies a fire for a symbol given back
//!   since (decision 0205 item 3).
//! * **What it does not ask**: the tier and the trading state. A give-back writes nothing at the venue and opens
//!   nothing, so it is taken on a `live` account and while the node is halted.
//! * **The engine's spelling.** The symbol the operator typed is matched against the block's list in any case,
//!   and the statement carries the LIST's spelling (`ETHUSDT.P`, never `ethusdt.p`), which is what the engine
//!   compares and the reconcile client was told.

use vike_exec::{OrderView, VenueBlock};
use vike_model::accounts::account_keys::{parse_wire_account, route_key_of};
use vike_tradehub_client::wire::WireCommand;

/// What the gate decided for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnadmitPlan {
    /// Not a give-back (every other verb), or a node that cannot judge it (no engine block published yet): the
    /// frame goes on, and the core's verdict stands.
    Pass,
    /// Refused before the `Ack`, with the cause in words.
    Refuse(String),
    /// Send it: `symbol` is the engine's own spelling of the symbol to give back.
    Release {
        /// The account's routing key (`route_key_of(venue, account)`).
        route_key: String,
        /// The symbol, spelled as the engine's block lists it.
        symbol: String,
    },
}

/// Judge `cmd` against the published `blocks` and `orders` ([`UnadmitPlan`]). Pure: reads nothing but its
/// arguments, so a Preview surface may call it too.
#[must_use]
pub fn plan_unadmit(cmd: &WireCommand, blocks: &[VenueBlock], orders: &[OrderView]) -> UnadmitPlan {
    let WireCommand::UnadmitSymbol { venue, account, symbol } = cmd else {
        return UnadmitPlan::Pass;
    };
    let typed = symbol.trim();
    if typed.is_empty() {
        return UnadmitPlan::Refuse("unadmit: an empty symbol names nothing to give back".into());
    }
    let label = match parse_wire_account(account) {
        Ok(label) => label,
        Err(e) => return UnadmitPlan::Refuse(format!("unadmit: `account` — {e}")),
    };
    let route_key = route_key_of(venue, &label);
    // The NEWEST block of the key: after a restart of the account a stopped engine's block may sit before it.
    let Some(block) = blocks.iter().rev().find(|b| b.route_key == route_key) else {
        // No block published for it (an empty roster, or an account the roster gate already judged): the core
        // answers, as it does for every verb this gate has no evidence about.
        return UnadmitPlan::Pass;
    };
    if block.symbol.eq_ignore_ascii_case(typed) {
        return UnadmitPlan::Refuse(format!(
            "`{}` is the symbol `{route_key}`'s engine was started on, not one it admitted on demand: it holds \
             no slot, so there is nothing to give back",
            block.symbol
        ));
    }
    let Some(listed) = block.extra_symbols.iter().find(|s| s.eq_ignore_ascii_case(typed)) else {
        let held: Vec<&str> = block.extra_symbols.iter().map(String::as_str).collect();
        return UnadmitPlan::Refuse(format!(
            "`{typed}` is not one of the symbols `{route_key}`'s engine admitted ({}); nothing to give back",
            if held.is_empty() { "it has admitted none".to_string() } else { held.join(", ") }
        ));
    };
    if let Some(p) = block
        .positions
        .iter()
        .find(|p| p.symbol.as_str().eq_ignore_ascii_case(listed) && p.size.abs() > 1e-12)
    {
        return UnadmitPlan::Refuse(format!(
            "`{route_key}` holds a position of {} in `{listed}`; close it first (an engine gives a symbol back \
             only when it is flat in it)",
            p.size
        ));
    }
    // The engine's own question (`ExecutionEngine::unadmit_symbol`): an order that has not ENDED, which is
    // `!is_terminal()` and so counts a `Liquidated` one that `is_live()` does not.
    let live: Vec<&OrderView> = orders
        .iter()
        .filter(|o| {
            !o.status.is_terminal()
                && o.symbol.eq_ignore_ascii_case(listed)
                && route_key_of(&o.venue, &o.account.clone().unwrap_or_default()) == route_key
        })
        .collect();
    if let Some(first) = live.first() {
        return UnadmitPlan::Refuse(format!(
            "`{route_key}` has {} live order(s) in `{listed}` (for one: `{}`, {}; a conditional counts); cancel {} first",
            live.len(),
            first.client_order_id,
            first.status.as_str(),
            if live.len() == 1 { "it" } else { "them" }
        ));
    }
    UnadmitPlan::Release { route_key, symbol: listed.clone() }
}
