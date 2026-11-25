//! Binance's USDⓈ-M futures user-data frame -> vike event mapper: the venue face of the shared
//! [`crate::family::execution::event_mapper::perp`] (`ORDER_TRADE_UPDATE`, `ACCOUNT_UPDATE`, `TRADE_LITE`) plus the one
//! frame that is binance's alone, **`ALGO_UPDATE`** (the Algo Service's order events, [`crate::execution::rest::algo`]).
//!
//! Aster's perp user-data stream is Binance-verbatim, so the mapping logic itself lives once in
//! `family` and aster calls it directly. `ALGO_UPDATE` is handled HERE, before the family mapper sees
//! the frame (it would drop it: any other `e` maps to nothing), so `family/` — which aster shares — is
//! untouched. The contract, the golden fixtures (`tests/offline/r6_binance_perp_parity.rs`), and the
//! public path here are unchanged — see the family module for the liquidation/funding/dual-publish
//! notes.
//!
//! [`map_binance_perp_opts`] is the flag-aware twin the Binance perp pump uses to enable the opt-in
//! TRADE_LITE early fast-fill hint (`venue.binance.trade_lite_fill = 1`); [`map_binance_perp`]
//! hard-wires the hint OFF (byte-identical to before). Every one takes `link_id`, the broker link id
//! the account's orders carry (`None` = none configured).

use serde_json::Value;
use vike_model::events::Event;

/// `ALGO_UPDATE` -> [`crate::execution::rest::algo::map_algo_update`]; every other frame -> the family's
/// [`crate::family::execution::event_mapper::perp::map_perp_opts`] (see its doc for `early_trade_lite_fill`).
pub fn map_binance_perp_opts(
    frame: &Value,
    venue: &str,
    symbol: &str,
    early_trade_lite_fill: bool,
    link_id: Option<&str>,
) -> Vec<Event> {
    if frame.get("e").and_then(Value::as_str) == Some("ALGO_UPDATE") {
        return crate::execution::rest::algo::map_algo_update(frame, link_id);
    }
    crate::family::execution::event_mapper::perp::map_perp_opts(
        frame,
        venue,
        symbol,
        early_trade_lite_fill,
        link_id,
    )
}

/// Whether `frame` is the stream's statement of a FUNDING payment, an `ACCOUNT_UPDATE` whose reason `a.m` is
/// `FUNDING_FEE`, which binance's live stream does not fold ([`map_binance_perp_stream`]): the payment is
/// `crate::execution::funding`'s poll of the income history (an id, a symbol, no loss across a reconnect), and the frame's
/// wallet figure already holds the payment the poll books up to a cadence later, so folding it would count the
/// payment twice until the next reconcile pass. Binance's alone: aster's stream keeps the family's funding arm.
pub fn is_funding_wallet_frame(frame: &Value) -> bool {
    frame.get("e").and_then(Value::as_str) == Some("ACCOUNT_UPDATE")
        && frame.pointer("/a/m").and_then(Value::as_str) == Some("FUNDING_FEE")
}

/// What the live perp stream folds: [`map_binance_perp_opts`] less the wallet statement of a fill
/// ([`crate::family::execution::event_mapper::perp::is_fill_wallet_frame`]) and of a funding payment ([`is_funding_wallet_frame`]).
pub fn map_binance_perp_stream(
    frame: &Value,
    venue: &str,
    symbol: &str,
    early_trade_lite_fill: bool,
    link_id: Option<&str>,
) -> Vec<Event> {
    if crate::family::execution::event_mapper::perp::is_fill_wallet_frame(frame)
        || is_funding_wallet_frame(frame)
    {
        return Vec::new();
    }
    map_binance_perp_opts(frame, venue, symbol, early_trade_lite_fill, link_id)
}

/// [`map_binance_perp_opts`] with the TRADE_LITE early-fill hint OFF — the default path.
pub fn map_binance_perp(
    frame: &Value,
    venue: &str,
    symbol: &str,
    link_id: Option<&str>,
) -> Vec<Event> {
    map_binance_perp_opts(frame, venue, symbol, false, link_id)
}
