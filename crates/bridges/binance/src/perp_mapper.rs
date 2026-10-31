//! Binance's USDⓈ-M futures user-data frame -> vike event mapper: the venue face of the shared
//! [`crate::family::perp_mapper`] (`ORDER_TRADE_UPDATE`, `ACCOUNT_UPDATE`, `TRADE_LITE`) plus the one
//! frame that is binance's alone, **`ALGO_UPDATE`** (the Algo Service's order events, [`crate::algo`]).
//!
//! Aster's perp user-data stream is Binance-verbatim, so the mapping logic itself lives once in
//! `family` and aster calls it directly. `ALGO_UPDATE` is handled HERE, before the family mapper sees
//! the frame (it would drop it: any other `e` maps to nothing), so `family/` — which aster shares — is
//! untouched. The contract, the golden fixtures (`tests/offline/r6_binance_perp_parity.rs`), and the
//! public path here are unchanged — see the family module for the liquidation/funding/dual-publish
//! notes.
//!
//! [`map_binance_perp_opts`] is the flag-aware twin the Binance perp pump uses to enable the opt-in
//! TRADE_LITE early fast-fill hint (`venue.binance.trade_lite_fill = 1`); the 3-arg
//! [`map_binance_perp`] hard-wires the hint OFF (byte-identical to before).

use serde_json::Value;
use vike_model::events::Event;

/// `ALGO_UPDATE` -> [`crate::algo::map_algo_update`]; every other frame -> the family's
/// [`crate::family::perp_mapper::map_perp_opts`] (see its doc for `early_trade_lite_fill`).
pub fn map_binance_perp_opts(
    frame: &Value,
    venue: &str,
    symbol: &str,
    early_trade_lite_fill: bool,
) -> Vec<Event> {
    if frame.get("e").and_then(Value::as_str) == Some("ALGO_UPDATE") {
        return crate::algo::map_algo_update(frame);
    }
    crate::family::perp_mapper::map_perp_opts(frame, venue, symbol, early_trade_lite_fill)
}

/// [`map_binance_perp_opts`] with the TRADE_LITE early-fill hint OFF — the default path.
pub fn map_binance_perp(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    map_binance_perp_opts(frame, venue, symbol, false)
}
