//! The strategy lane's DOOR: `Request::StrategyOpen`, the wire's second mode switch. What follows the
//! reply is `crate::strat`'s session; what is decided HERE is whether the door opens at all, and every
//! refusal is a positional `Response::Error` on a connection that stays usable (the same rule
//! `Request::MarketDataSubscribe`'s refusals keep).
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
//! 4. **The hub's own door** (`StrategyLane::open_session`): an unserved venue, a network the venue does
//!    not take, a full lane. The full lane is a CAP and answers the refusal frame
//!    (`vike_datahub_client::refusals::DatahubRefusal::StrategyLaneFull`) to a client that reads it.
//!
//! [`Scope::Trade`]: vike_hub_protocol::auth::Scope::Trade

use std::sync::Arc;

use vike_data::HistoryStore;
use vike_datahub_client::proto::Response;
use vike_model::SymbolProperties;

use super::operator_instruments::InstrumentLookup;
use super::step::Step;
use crate::strat::StrategyLane;

/// The refusal a datahub with NO strategy lane mounted answers `StrategyOpen` with: leads with the key,
/// because the lane is armed by it and by nothing else. A `const` because the refusal and the tests that
/// prove it is the one a client reads both name it.
pub const NO_STRATEGY_LANE: &str = "this datahub serves no strategy lane. It is armed by a KEY, not by a build or a flag: \
     `vike-cli backend mint-missing` mints VIKE_DATAHUB_TRADE_KEY in this server's hub-key store \
     (every server start runs it), and a restart of the datahub mounts the lane. Nothing was opened and this connection is \
     unchanged: every other verb still works on it.";

/// The refusal a KEY-LESS datahub answers with, whatever is mounted: the lane is a keyed surface.
pub(super) const KEYLESS_LANE_REFUSAL: &str = "this datahub holds no hub keys, so it serves no strategy lane. A key-less server \
     authenticates nothing, so the Trade scope that fences the lane would be a word nothing \
     enforces. A served datahub always authenticates (decision 0288): `vike-cli backend \
     mint-missing` mints the lane's own key, and a restart of the datahub mounts the lane. Nothing \
     was opened.";

/// `Request::StrategyOpen`: refuse, or reserve a session and switch the connection.
pub(super) fn strategy_open_verb(
    venue: &str,
    network: Option<&str>,
    keyed: bool,
    strat: Option<&Arc<StrategyLane>>,
) -> Step {
    let Some(hub) = strat else {
        return Step::reply(Response::Error(NO_STRATEGY_LANE.to_string()));
    };
    if !keyed {
        return Step::reply(Response::Error(KEYLESS_LANE_REFUSAL.to_string()));
    }
    match hub.open_session(venue, network) {
        Ok(guard) => Step::StrategySwitch(guard),
        // The lane's cap is the refusal frame (the connection loop answers an older client its `Error`).
        Err(refused) => Step::Reply(refused),
    }
}

/// **The gatewayhub's KEYLESS catalog search as a look-up** (`vike_gatewayhub_client::catalog::keyless_search`, the Read
/// scope this datahub holds), given up after the family resolve's own bound
/// (`vike_gatewayhub_client::resolve::RESOLVE_REPLY_BOUND`): what PM-2's recorder reads a token's facts with, and what
/// [`strategy_symbol_facts_verb`] asks once for a symbol the store has no facts of.
#[must_use]
pub fn keyless_lookup(link: vike_gatewayhub_client::client::GatewayLink) -> InstrumentLookup {
    Arc::new(move |venue: &str, symbol: &str| {
        let deadline =
            std::time::Instant::now() + vike_gatewayhub_client::resolve::RESOLVE_REPLY_BOUND;
        let stop = || std::time::Instant::now() >= deadline;
        vike_gatewayhub_client::catalog::keyless_search(&link, venue, symbol, &stop)
    })
}

/// `Request::StrategySymbolFacts` (decision 0285's amendment of 2026-11-16): the facts of `(venue, symbol)` for a
/// tradehub paper account that holds no trade lease, `symbol` its USER name (decision 0290). The store's row as of now
/// (the rows PM-2's recorder writes, under the store's name of it), else ONE keyless catalog search through `lookup`
/// for the venue's own name of it, which the venue's KEPT row states (`vike_catalog::kept_row_of_user_name` over the
/// list in `instruments`: the name stored there, never spelled here), its class set as the recorder sets it; none of
/// them is the refusal in words. Nothing is written. A key-less server refuses it, as it refuses the lane: the look-up
/// spends this datahub's own Read key on a caller nothing authenticated.
pub(super) fn strategy_symbol_facts_verb(
    venue: &str,
    symbol: &str,
    keyed: bool,
    store: &dyn HistoryStore,
    lookup: Option<&InstrumentLookup>,
    instruments: Option<&std::path::Path>,
) -> Response {
    if !keyed {
        return Response::Error(KEYLESS_LANE_REFUSAL.to_string());
    }
    let series = vike_paths::store_path::store_name_of_user_name(symbol);
    let stored = match store.properties_as_of(venue, &series, vike_model::now_ms()) {
        Ok(Some(facts)) => return Response::Properties(Some(Box::new(facts))),
        Ok(None) => {
            format!("this datahub's store holds no recorded facts of `{symbol}` on {venue}")
        }
        Err(e) => format!("this datahub's store could not be read for `{symbol}` on {venue} ({e})"),
    };
    let Some(lookup) = lookup else {
        return Response::Error(format!(
            "no facts of `{symbol}` on {venue}: {stored}, and it holds no gatewayhub link to read the venue's public \
             catalog"
        ));
    };
    let list = instruments.and_then(|dir| crate::instrument_lists::parsed_kept_list(dir, venue));
    let kept = list.as_ref().map_or(&[][..], |list| list.instruments.as_slice());
    let wire = match vike_catalog::kept_row_of_user_name(kept, venue, symbol) {
        Ok(row) => row.raw_symbol.clone(),
        Err(why) => {
            return Response::Error(format!(
                "no facts of `{symbol}` on {venue}: {stored}, and {why}"
            ));
        }
    };
    let row = lookup(venue, &wire).and_then(|rows| {
        rows.into_iter().find(|row| row.raw_symbol == wire).ok_or_else(|| {
            format!("{venue}'s public catalog lists no `{wire}` (`{symbol}`'s venue name)")
        })
    });
    match row {
        Ok(row) => Response::Properties(Some(Box::new(SymbolProperties {
            asset_class: Some(row.asset_class()),
            ..row.properties
        }))),
        Err(why) => {
            Response::Error(format!("no facts of `{symbol}` on {venue}: {stored}, and {why}"))
        }
    }
}
