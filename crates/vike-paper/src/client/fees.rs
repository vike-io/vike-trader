//! Commission: the `UnderlyingSource` seam and `commission_for`, the one fee decision every fill takes.

use std::sync::Arc;

use vike_fills::broker_sim::commission;
use vike_model::{ContractValue, FeeSchedule, NotionalKind, OrderFees};

use super::{ExecutionClientPaper, paper_contract_value};

/// An optional UNDERLYING/index-price source for the paper book: `(venue, symbol, ts) -> Option<f64>`,
/// mirroring `vike_simulator::StrategyEngineConfig::properties`' `(venue, symbol, ts) -> Option<_>` shape
/// (PR-2a) exactly. It supplies the underlying (index) price a Deribit-options
/// [`FeeSchedule::PercentOfUnderlying`] fill needs to book the ACCURATE
/// `min(0.03% × underlying, 12.5% × premium)` commission via
/// [`FeeSchedule::commission_with_underlying`] instead of the premium-only approximation the paper
/// fill site otherwise degrades to — which understates the true fee ~20x for options priced well
/// below their underlying (see [`vike_model::FeeSchedule`]'s module note on the residual paper gap).
///
/// `None` (every constructor today), or a source that does not price an option at submit, makes the
/// book REFUSE an order on an option its `PercentOfUnderlying` schedule charges
/// ([`super::unpriced_option_refusal`]): never charged on the premium. Every other fee shape and
/// symbol books the previous number bit-for-bit. See [`ExecutionClientPaper::commission_for`].
pub(super) type UnderlyingSource = Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>;

impl ExecutionClientPaper {
    /// **What one unit of `symbol` is worth to THIS book, or why it refuses it**: [`paper_contract_value`]'s
    /// answer by the name, else the unit the engine stated from the venue's facts for a symbol whose name
    /// fixes none (`stated_values`: an IG epic, the owner's ruling of 2026-11-16). The refusal stands
    /// while nothing is stated.
    pub(super) fn unit(&self, symbol: &str) -> Result<ContractValue, String> {
        paper_contract_value(&self.venue, symbol)
            .or_else(|why| self.stated_values.get(symbol).copied().ok_or(why))
    }

    /// [`super::unpriced_option_refusal`] for THIS book at `ts`: `None` when its [`UnderlyingSource`]
    /// prices `symbol`'s underlying then. `submit` refuses an option it would otherwise charge on the
    /// premium. shortcut: a source that prices at submit and not at the fill still charges the
    /// premium there; upgrade when a production source exists.
    pub(super) fn option_fee_refusal(&self, symbol: &str, ts: i64) -> Option<String> {
        let why = super::unpriced_option_refusal(&self.venue, symbol, self.fee_schedule)?;
        let priced = self
            .underlying_source
            .as_ref()
            .is_some_and(|src| src(&self.venue, symbol, ts).is_some());
        (!priced).then_some(why)
    }

    /// The commission for ONE leg fill of `qty` @ `px` on `symbol` at `ts`. This is the single
    /// commission decision both [`Self::emit_fill`] (single-symbol) and [`Self::emit_combo_fills`]
    /// (per combo leg) route through.
    ///
    /// - an INVERSE contract ([`paper_contract_value`]): the schedule's rate on the order's dollars,
    ///   `|qty|·u`, priced at 1, which is price-free like its notional (`ContractValue::order_notional`).
    ///   The venue charges `rate·|qty|·u/px` in the settle coin and the engine values that at the fill
    ///   price `px`: this number. It is stated in `USD` ([`Self::commission_asset_for`]).
    /// - `fee_schedule = Some(PercentOfUnderlying)` AND an [`UnderlyingSource`] is wired AND it
    ///   prices `(venue, symbol, ts)`: the ACCURATE [`FeeSchedule::commission_with_underlying`]
    ///   (0.03%-of-underlying, premium-cap-bounded). `px` is the traded PREMIUM here (the fill
    ///   price), `underlying` comes from the source. THIS is the deliberate behavior change.
    /// - anything else: [`vike_fills::broker_sim::commission`], the simulator's own decision. With
    ///   `fee_schedule = None` (the `new` path, the r7 gate's) that is the flat `maker_fee`/`taker_fee`
    ///   rate; with a schedule it is [`FeeSchedule::commission_on`].
    ///
    /// Every linear rate term scales by `self.multiplier` (a schedule's per-share fee and minimum do
    /// not), which every constructor leaves at `1.0`.
    ///
    /// Each call is a whole order's ONLY fill, so a per-order minimum is charged once: a single-symbol
    /// order fills once and leaves the book (`on_bar` never re-rests a partly filled one), and each
    /// combo leg is its own instrument's execution.
    pub(super) fn commission_for(
        &self,
        is_maker: bool,
        qty: f64,
        px: f64,
        symbol: &str,
        ts: i64,
    ) -> f64 {
        let order = &mut OrderFees::default();
        if let Ok(value) = self.unit(symbol)
            && value.kind == NotionalKind::Inverse
        {
            // The simulator's one fee, which it books at `in_settle(fee, px)`.
            return commission(
                self.fee_schedule,
                self.maker_fee,
                self.taker_fee,
                is_maker,
                qty,
                px,
                value,
                None,
                order,
            );
        }
        // ONLY the Deribit-options `PercentOfUnderlying` shape has an underlying dimension; for every
        // other shape the underlying is irrelevant and `commission` is exact.
        // A unit the engine stated (an IG epic's `contractSize`) scales the rate as the venue's would.
        let value = self
            .stated_values
            .get(symbol)
            .copied()
            .unwrap_or(ContractValue::linear(self.multiplier));
        // `px` is the traded premium (the fill price); `qty` the contract count. The underlying is
        // priced for a `PercentOfUnderlying` schedule only, and `commission` charges it on it.
        let underlying = match self.fee_schedule {
            Some(FeeSchedule::PercentOfUnderlying { .. }) => {
                self.underlying_source.as_ref().and_then(|src| src(&self.venue, symbol, ts))
            }
            _ => None,
        };
        commission(
            self.fee_schedule,
            self.maker_fee,
            self.taker_fee,
            is_maker,
            qty,
            px,
            value,
            underlying,
            order,
        )
    }

    /// The currency a fill of `symbol` states its commission in: `USD` for an inverse contract, whose
    /// fee [`Self::commission_for`] gives in dollars (left empty, the engine would read the settle coin
    /// and multiply it by the price again: `vike_execution::Account::commission_in_quote`); empty for every
    /// other symbol, as every paper fill always said.
    pub(super) fn commission_asset_for(&self, symbol: &str) -> &'static str {
        match self.unit(symbol) {
            Ok(value) if value.kind == NotionalKind::Inverse => vike_model::DEFAULT_CURRENCY,
            _ => "",
        }
    }
}
