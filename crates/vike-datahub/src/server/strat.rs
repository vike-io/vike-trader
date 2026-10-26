//! The strategy lane's DOOR: `Request::StratOpen`, the wire's second mode switch. What follows the
//! reply is `crate::strat`'s session; what is decided HERE is whether the door opens at all, and every
//! refusal is a positional `Response::Error` on a connection that stays usable (the same rule
//! `Request::MdSubscribe`'s refusals keep).
//!
//! # The gate order
//!
//! 1. **No hub mounted** - the lane is armed by the KEY, not by a build or a flag (`datahub_cli`'s
//!    `start_strategy_lane`), so a datahub without `VIKE_DATAHUB_TRADE_KEY` mounts nothing and answers
//!    [`NO_STRATEGY_LANE`], naming the command that mints it.
//! 2. **Key-less server** - a hub on a server that authenticates nothing would hand a price lane to
//!    whoever reaches the socket, and the scope that fences it ([`Scope::Trade`]) would be a word
//!    nothing enforces; the lane fails closed with [`KEYLESS_LANE_REFUSAL`].
//! 3. **The scope** was already checked by the connection loop (`required_scope` is
//!    `VerbScope::Trade`, admitted to `Scope::Trade` alone), before this function is reached.
//! 4. **The hub's own door** (`StratHub::open_session`): an unserved venue, a network the venue does
//!    not take, a full lane.
//!
//! [`Scope::Trade`]: vike_hub_proto::auth::Scope::Trade

use std::sync::Arc;

use vike_datahub_client::proto::Response;

use super::step::Step;
use crate::strat::StratHub;

/// The refusal a datahub with NO strategy lane mounted answers `StratOpen` with: leads with the key,
/// because the lane is armed by it and by nothing else. A `const` because the refusal and the tests that
/// prove it is the one a client reads both name it.
pub const NO_STRATEGY_LANE: &str = "this datahub serves no strategy lane. It is armed by a KEY, not by a build or a flag: \
     `vike-cli datahub setup --strategy-lane` mints VIKE_DATAHUB_TRADE_KEY in this server's hub-key \
     store, and a restart of the datahub mounts the lane. Nothing was opened and this connection is \
     unchanged: every other verb still works on it.";

/// The refusal a KEY-LESS datahub answers with, whatever is mounted: the lane is a keyed surface.
pub(super) const KEYLESS_LANE_REFUSAL: &str = "this datahub holds no hub keys, so it serves no strategy lane. A key-less server \
     authenticates nothing, so the Trade scope that fences the lane would be a word nothing \
     enforces. Run `vike-cli datahub setup` to mint the pair, then `vike-cli datahub setup \
     --strategy-lane` for the lane's own key, and restart the datahub. Nothing was opened.";

/// `Request::StratOpen`: refuse, or reserve a session and switch the connection.
pub(super) fn strat_open_verb(
    venue: &str,
    network: Option<&str>,
    keyed: bool,
    strat: Option<&Arc<StratHub>>,
) -> Step {
    let Some(hub) = strat else {
        return Step::reply(Response::Error(NO_STRATEGY_LANE.to_string()));
    };
    if !keyed {
        return Step::reply(Response::Error(KEYLESS_LANE_REFUSAL.to_string()));
    }
    match hub.open_session(venue, network) {
        Ok(guard) => Step::StratSwitch(guard),
        Err(why) => Step::reply(Response::Error(why)),
    }
}
