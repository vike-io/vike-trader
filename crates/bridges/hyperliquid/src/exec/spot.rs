//! The SPOT half of the exec lane (decision 0218): what a spot pair needs that a perpetual does not. Pure
//! functions over a resolved [`InstrumentRef`], so every rule here is proven with no key and no network.
//!
//! * **A spot order carries no reduce-only flag** ([`reduce_only_on_wire`]). A spot sell cannot open a short (the
//!   venue refuses a sale of tokens the account does not hold), so the flag has nothing to guard, and the engine's
//!   closes (a flatten, a margin-call exit, a core-held stop's fire) are reduce-only MARKETS that must not be
//!   refused for a flag spot does not take.
//! * **A fee paid in the BASE token is netted out of the fill** ([`net_base_fee`]). HL charges a spot fee in the
//!   token it hands over: a BUY of HYPE/USDC pays it in HYPE, so the account receives `sz - fee` HYPE while the
//!   fill row says `sz`. Folded as it stands the engine would hold `sz` HYPE (a later full-size sale is refused for
//!   want of `fee`) and charge `fee` dollars where `fee x px` were paid. So the fill is reported net: the quantity
//!   the account really got or gave, and the commission in the QUOTE token at the fill price. The equity comes out
//!   identical to the venue's at every mark (`net_base_fee_keeps_the_equity_identical_at_every_mark`).
//!
//! Applied at the two places a fill row becomes an engine fill, so the live stream and the reconcile read agree to
//! the digit: [`crate::exec::user_data`]'s `userFills` mapping and [`crate::exec::recon_client`]'s fill read.

use vike_model::FillReport;
use vike_model::events::FillEvent;

use crate::base::config::Product;
use crate::base::symbology::InstrumentRef;

/// The `reduce_only` flag the order wire carries: the request's, except for a spot pair, which carries none.
pub(crate) fn reduce_only_on_wire(inst: &InstrumentRef, requested: bool) -> bool {
    requested && !matches!(inst.product, Product::Spot)
}

/// `(net quantity, commission in the quote token, the quote token)` of a spot fill whose fee is paid in the pair's
/// BASE token, or `None` when the fill needs no netting: not a spot pair, no fee, a fee in any other token (the
/// quote, as a sale pays it), or a row that would net to nothing.
///
/// The quantity moves AGAINST the fee: a buy receives `qty - fee`, a sell (were its fee ever taken in the base)
/// gives `qty + fee`. A negative fee (a maker rebate in the base) moves it the other way. The commission keeps the
/// fee's sign.
pub(crate) fn net_base_fee(
    inst: &InstrumentRef,
    side: i32,
    qty: f64,
    px: f64,
    fee: f64,
    fee_token: &str,
) -> Option<(f64, f64, String)> {
    if !matches!(inst.product, Product::Spot) || fee == 0.0 || !fee.is_finite() {
        return None;
    }
    if fee_token != inst.base {
        return None;
    }
    let quote = inst.quote.as_deref()?;
    let net = qty - f64::from(side.signum()) * fee;
    (net.is_finite() && net > 0.0 && px.is_finite() && px > 0.0)
        .then(|| (net, fee * px, quote.to_string()))
}

/// [`net_base_fee`] applied to a live fill, in place.
pub(crate) fn net_fill_event(fill: &mut FillEvent, inst: &InstrumentRef) {
    if let Some((qty, commission, asset)) = net_base_fee(
        inst,
        fill.side,
        fill.last_qty,
        fill.last_px,
        fill.commission,
        fill.commission_asset.as_str(),
    ) {
        fill.last_qty = qty;
        fill.commission = commission;
        fill.commission_asset = asset.as_str().into();
    }
}

/// [`net_base_fee`] applied to a reconcile fill report, in place.
pub(crate) fn net_fill_report(report: &mut FillReport, inst: &InstrumentRef) {
    if let Some((qty, commission, asset)) = net_base_fee(
        inst,
        report.side,
        report.last_qty,
        report.last_px,
        report.commission,
        &report.commission_asset,
    ) {
        report.last_qty = qty;
        report.commission = commission;
        report.commission_asset = asset;
    }
}

#[path = "spot_tests.rs"]
#[cfg(test)]
mod spot_tests;
