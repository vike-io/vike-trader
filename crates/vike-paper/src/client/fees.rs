//! Commission: the `UnderlyingSource` seam and `commission_for`, the one fee decision every fill takes.

use std::sync::Arc;

use vike_fills::broker_sim::fee;
use vike_model::FeeSchedule;

use super::PaperExecutionClient;

/// An optional UNDERLYING/index-price source for the paper book: `(venue, symbol, ts) -> Option<f64>`,
/// mirroring `vike_sim::EngineParams::properties`' `(venue, symbol, ts) -> Option<_>` shape
/// (PR-2a) exactly. It supplies the underlying (index) price a Deribit-options
/// [`FeeSchedule::PercentOfUnderlying`] fill needs to book the ACCURATE
/// `min(0.03% × underlying, 12.5% × premium)` commission via
/// [`FeeSchedule::commission_with_underlying`] instead of the premium-only approximation the paper
/// fill site otherwise degrades to — which understates the true fee ~20x for options priced well
/// below their underlying (see [`vike_model::FeeSchedule`]'s module note on the residual paper gap).
///
/// `None` (every constructor today) is the byte-identical legacy path: the commission is computed
/// exactly as before. A `Some` source only ever changes the number for a `PercentOfUnderlying`
/// schedule whose `(venue, symbol, ts)` the source actually prices; any other fee shape, or a
/// source that returns `None`, still books the previous number bit-for-bit. See
/// [`PaperExecutionClient::commission_for`].
pub(super) type UnderlyingSource = Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>;

impl PaperExecutionClient {
    /// The commission for ONE leg fill of `qty` @ `px` on `symbol` at `ts`. This is the single
    /// commission decision both [`Self::emit_fill`] (single-symbol) and [`Self::emit_combo_fills`]
    /// (per combo leg) route through.
    ///
    /// - `fee_schedule = None` (the `new` path): the flat `maker_fee`/`taker_fee` × `broker_sim::fee`
    ///   primitive — byte-identical to before (the r7 gate path).
    /// - `fee_schedule = Some(PercentOfUnderlying)` AND an [`UnderlyingSource`] is wired AND it
    ///   prices `(venue, symbol, ts)`: the ACCURATE [`FeeSchedule::commission_with_underlying`]
    ///   (0.03%-of-underlying, premium-cap-bounded). `px` is the traded PREMIUM here (the fill
    ///   price), `underlying` comes from the source. THIS is the deliberate behavior change.
    /// - any other schedule, or no source, or a source that returns `None`: the previous
    ///   [`FeeSchedule::commission`] number, bit-for-bit.
    pub(super) fn commission_for(
        &self,
        is_maker: bool,
        qty: f64,
        px: f64,
        symbol: &str,
        ts: i64,
    ) -> f64 {
        match &self.fee_schedule {
            Some(sched) => {
                // ONLY the Deribit-options `PercentOfUnderlying` shape has an underlying dimension;
                // for every other shape the underlying is irrelevant and `commission` is exact.
                if matches!(sched, FeeSchedule::PercentOfUnderlying { .. })
                    && let Some(src) = &self.underlying_source
                    && let Some(underlying) = src(&self.venue, symbol, ts)
                {
                    // `px` is the traded premium (the fill price); `qty` the contract count.
                    return sched.commission_with_underlying(qty, px, underlying);
                }
                sched.commission(is_maker, qty, px)
            }
            None => {
                let rate = if is_maker { self.maker_fee } else { self.taker_fee };
                fee(qty, px, rate, self.multiplier)
            }
        }
    }
}
