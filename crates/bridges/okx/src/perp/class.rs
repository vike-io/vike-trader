//! `OkxPerpRest`'s class-specific order wire (decision 0232): what a SPOT pair's and a dated future's
//! order differs by from a SWAP's, in one place. The class is the instId's own
//! ([`crate::inst_class::InstClass`]); every body of the mounted SWAP keeps its bytes.
//!
//! * a dated FUTURE is written exactly like a SWAP (`tdMode` cross or isolated, `posSide` net,
//!   `reduceOnly`, `sz` in contracts): MEASURED on `QTUM-USDT-261225`, 2026-10-30.
//! * a SPOT pair states no `posSide` and no `reduceOnly` (the venue echoes `net` and `false` itself), a
//!   MARKET order states `tgtCcy=base_ccy` (without it `sz` is the QUOTE amount: `sz 5` bought 5 USDT of
//!   BTC), and `tdMode` is `cross` where the account's level takes it and `cash` where it does not
//!   ([`post_order_params`](OkxPerpRest::post_order_params)). A spot STOP is written alike, on the algo
//!   endpoint (MEASURED: accepted as `cross`, refused as `cash` at level 3, cancelled by `cancel-algos`).

use serde_json::{Value, json};

use vike_bridge_core::transport::VenueApiError;
use vike_model::{MarginMode, OrderRequest};

use super::{OkxPerpRest, swap_td_mode};
use crate::inst_class::{InstClass, SpotMode, is_td_mode_refusal};
use crate::transport::OkxTransport;

/// The `tdMode` a body of `class` carries. A SWAP or FUTURE: [`swap_td_mode`] (unset = the historical
/// `cross`). A SPOT pair: `spot_mode` (the attempt: cross first, cash on the account level that refuses it)
/// unless the request NAMES a mode: `Cash` is `cash`, `Cross` is the attempt's, and `Isolated` is refused in
/// words (a spot pair's isolated margin is a borrowing mode the engine does not model).
pub(super) fn td_mode(
    class: InstClass,
    margin_mode: Option<MarginMode>,
    spot_mode: SpotMode,
) -> Result<&'static str, String> {
    match class {
        InstClass::Swap | InstClass::Futures => swap_td_mode(margin_mode),
        InstClass::Spot => match margin_mode {
            None | Some(MarginMode::Cross) => Ok(spot_mode.wire()),
            Some(MarginMode::Cash) => Ok(SpotMode::Cash.wire()),
            Some(MarginMode::Isolated) => Err(
                "okx: margin_mode=Isolated is denied on a spot pair: isolated spot margin borrows, \
                 and the engine books a spot pair fully funded (cross or cash)"
                    .to_string(),
            ),
        },
    }
}

/// Shape a SPOT order body: no `posSide`, no `reduceOnly`, and `tgtCcy=base_ccy` on a MARKET order so `sz`
/// is the base quantity the engine asked for and not a quote amount.
pub(super) fn shape_spot_body(params: &mut Vec<(&'static str, Value)>, is_limit: bool) {
    params.retain(|(key, _)| !matches!(*key, "posSide" | "reduceOnly"));
    if !is_limit {
        params.push(("tgtCcy", json!("base_ccy")));
    }
}

/// Shape a SPOT stop body (the algo endpoint): no `posSide`, no `reduceOnly`; the trigger fires a market
/// order whose `sz` is already base (a stop's `sz` is not a quote amount, MEASURED on a far sell stop).
pub(super) fn shape_spot_stop(params: &mut Vec<(&'static str, Value)>) {
    params.retain(|(key, _)| !matches!(*key, "posSide" | "reduceOnly"));
}

impl<T: OkxTransport> OkxPerpRest<T> {
    /// POST `first` to `path`. A SPOT order whose request names no margin mode and which the venue refuses
    /// for its `tdMode` (`51000`, "Parameter tdMode error": the account's level does not take the mode just
    /// sent) is sent ONCE more as the other mode (`rebuild(SpotMode::Cash)`); the first answer was a refusal,
    /// so nothing rests and the same client order id is free. If the second is refused too, its refusal is
    /// the answer and carries the first's words. Every other order, and every other refusal, is `first`'s
    /// answer untouched.
    pub(super) fn post_order_params(
        &self,
        path: &str,
        request: &OrderRequest,
        first: Vec<(&'static str, Value)>,
        rebuild: impl Fn(SpotMode) -> Result<Vec<(&'static str, Value)>, String>,
    ) -> Result<Value, VenueApiError> {
        let sent = self.call(path, "POST", &first);
        let Err(refusal) = &sent else { return sent };
        if InstClass::of_inst_id(&self.symbol) != Some(InstClass::Spot)
            || request.margin_mode.is_some()
            || !is_td_mode_refusal(refusal.code, &refusal.msg)
        {
            return sent;
        }
        let Ok(second) = rebuild(SpotMode::Cash) else { return sent };
        tracing::info!(
            target: "vike_okx::perp", symbol = %self.symbol,
            "spot order refused for its tdMode as cross: the account's level takes cash; sending it once more as cash"
        );
        let first_msg = refusal.msg.trim().to_string();
        self.call(path, "POST", &second).map_err(|again| VenueApiError {
            code: again.code,
            msg: format!("{} (the same order as tdMode cross: {first_msg})", again.msg.trim()),
        })
    }
}
