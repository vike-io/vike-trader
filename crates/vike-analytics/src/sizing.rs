//! Position sizing: the swappable sizer framework (WealthLab's PosSizer model). Exact port
//! of `core/sizing.py`. What one unit is worth is the ONE sizing law,
//! `vike_model::ContractValue::unit_value` (decision 0170, item 9), which the live order_target verbs
//! and the simulator's target verb read too; the value and percent sizers below divide by it. The
//! context carries the symbol's `ContractValue` and its money (equity, cash, open risk) in the
//! symbol's QUOTE currency at par: a coin ledger's coin is valued at `basis_price` by the caller
//! (decision 0258). The risk sizers (ATR, stop distance) price a price MOVE through
//! `ContractValue::move_risk` (it was `move_risk` here, moved bit for bit on the owner's word of
//! 2026-11-09: contract money has one home): linear `|Δp|·u` as they always did, inverse the
//! contract's own PnL of the move.

use vike_model::ContractValue;

/// One unit's worth at the entry basis by the one sizing law: `basis_price · u` on a linear value, bit
/// for bit, and the face `u` on an inverse one.
fn unit_value(ctx: &PositionSizerContext) -> f64 {
    // at par (r = 1.0) the law always answers
    ctx.contract_value.unit_value(ctx.basis_price, 1.0).unwrap_or(0.0)
}

/// Live portfolio context handed to a sizer for one entry intent.
#[derive(Debug, Clone)]
pub struct PositionSizerContext {
    pub symbol: String,
    /// +1 buy / -1 sell (opening direction)
    pub side: i32,
    /// the raw size the strategy passed (used only by PassThrough)
    pub intent: f64,
    /// entry basis (current close)
    pub basis_price: f64,
    /// the account's equity, in the symbol's quote currency (a coin ledger's coin valued at `basis_price`)
    pub equity: f64,
    /// the account's cash, in the same money as `equity`
    pub cash: f64,
    /// what one unit of the symbol is worth (decision 0170): its kind and unit
    pub contract_value: ContractValue,
    /// recent average true range (0 = unavailable)
    pub atr: f64,
    /// current account drawdown fraction 0..1
    pub drawdown: f64,
    /// protective stop price for this entry (risk sizers need it)
    pub risk_stop: Option<f64>,
    /// sum of risk already open across the book (portfolio-heat caps), in the same money as `equity`
    pub open_risk: f64,
}

/// Return the quantity (>= 0) to open for this entry intent.
///
/// `Send`-bounded (a supertrait, not per-field) so `Box<dyn PositionSizer>` is `Send` everywhere
/// it's embedded — notably `StrategyEngineConfig.sizer`, which must itself be `Send` for callers that
/// move a whole `StrategyEngineConfig` onto a worker thread (e.g. vike-studio-core's `spawn_outcome`). All shipped
/// sizers are plain data (f64/usize/nested `Box<dyn PositionSizer>`), so this is free today.
pub trait PositionSizer: Send {
    fn size(&self, ctx: &PositionSizerContext) -> f64;
}

/// Default: the strategy's own size, unchanged.
pub struct PositionSizerPassThrough;
impl PositionSizer for PositionSizerPassThrough {
    fn size(&self, ctx: &PositionSizerContext) -> f64 {
        ctx.intent
    }
}

/// `qty` floored to `step` when one is given (`vike_model::floor_to_step`), else as it is.
fn floored(qty: f64, step: Option<f64>) -> f64 {
    step.map_or(qty, |step| vike_model::floor_to_step(qty, step))
}

/// Each entry is a fixed cash notional, floored to `step` when one is given. `Default` is every option off.
#[derive(Default)]
pub struct PositionSizerFixedDollar {
    pub amount: f64,
    /// The quantity step the size is floored to (`vike_model::floor_to_step`); `None` leaves it as worked out.
    pub step: Option<f64>,
}
impl PositionSizer for PositionSizerFixedDollar {
    fn size(&self, ctx: &PositionSizerContext) -> f64 {
        let denominator = unit_value(ctx);
        // Python `if denominator` — truthiness = non-zero (negative allowed)
        floored(if denominator != 0.0 { self.amount / denominator } else { 0.0 }, self.step)
    }
}

pub struct PositionSizerFixedShares {
    pub shares: f64,
}
impl PositionSizer for PositionSizerFixedShares {
    fn size(&self, _context: &PositionSizerContext) -> f64 {
        self.shares
    }
}

/// Each entry targets `pct` of current account equity. With a commission stated, the size and its commission spend that
/// together: `(pct · equity − fee_per_order) / (unit · (1 + fee_rate) + fee_per_unit)`, the unit being one unit's worth
/// (`vike_model::share_quantity`, the one percent-of-equity law).
/// Floored to `step` when one is given. `Default` is every option off, and a zero fee subtracts, scales and adds nothing (an
/// IEEE no-op on each term), so with none the size is `pct · equity / unit` bit for bit.
#[derive(Default)]
pub struct PositionSizerPercentEquity {
    pub percent: f64,
    /// The commission as a fraction of the entry's value (0.00075 is 0.075%).
    pub fee_rate: f64,
    /// The commission per unit of quantity, in the money of `PositionSizerContext::equity`.
    pub fee_per_unit: f64,
    /// The commission per order, in the same money.
    pub fee_per_order: f64,
    /// The quantity step the size is floored to (`vike_model::floor_to_step`); `None` leaves it as worked out.
    pub step: Option<f64>,
}
impl PositionSizer for PositionSizerPercentEquity {
    fn size(&self, ctx: &PositionSizerContext) -> f64 {
        let (unit, rate) = (unit_value(ctx), self.fee_rate);
        let qty = vike_model::share_quantity(
            self.percent,
            ctx.equity,
            unit,
            rate,
            self.fee_per_unit,
            self.fee_per_order,
        );
        floored(qty, self.step)
    }
}

/// Size so the position's ATR-risk is `pct` of equity; %-equity fallback when ATR is 0.
pub struct PositionSizerPercentVolatility {
    pub percent: f64,
}
impl PositionSizer for PositionSizerPercentVolatility {
    fn size(&self, ctx: &PositionSizerContext) -> f64 {
        // Python `if ctx.atr and ctx.multiplier` — both truthy (non-zero)
        let cv = ctx.contract_value;
        if ctx.atr != 0.0 && cv.per_unit != 0.0 {
            // one ATR against the position: `atr · u` on a linear unit, the contract's PnL on an inverse
            // one (none when the move would carry the price to zero: the %-equity fallback sizes it)
            if let Some(risk) = cv.move_risk_against(ctx.basis_price, ctx.side, ctx.atr) {
                return (self.percent * ctx.equity) / risk;
            }
        }
        let denominator = unit_value(ctx);
        if denominator != 0.0 { (self.percent * ctx.equity) / denominator } else { 0.0 }
    }
}

/// Size so the trade risks `pct` of equity given the stop distance; 0 without a stop.
pub struct PositionSizerMaxRiskPercent {
    pub percent: f64,
}
impl PositionSizer for PositionSizerMaxRiskPercent {
    fn size(&self, ctx: &PositionSizerContext) -> f64 {
        let Some(stop) = ctx.risk_stop else {
            return 0.0;
        };
        let risk_per_unit = ctx.contract_value.move_risk(ctx.basis_price, stop);
        if risk_per_unit > 0.0 { (self.percent * ctx.equity) / risk_per_unit } else { 0.0 }
    }
}

/// Wrap a base sizer; cap TOTAL open risk to `max_heat * equity`.
pub struct PositionSizerPortfolioHeat {
    pub base: Box<dyn PositionSizer>,
    pub max_heat: f64,
}
impl PositionSizer for PositionSizerPortfolioHeat {
    fn size(&self, ctx: &PositionSizerContext) -> f64 {
        let qty = self.base.size(ctx);
        let Some(stop) = ctx.risk_stop else {
            return qty;
        };
        let risk_per_unit = ctx.contract_value.move_risk(ctx.basis_price, stop);
        if risk_per_unit <= 0.0 {
            return qty;
        }
        let budget = self.max_heat * ctx.equity - ctx.open_risk;
        if budget <= 0.0 {
            return 0.0;
        }
        let max_qty = budget / risk_per_unit;
        qty.min(max_qty)
    }
}

/// Wrap a base sizer and scale down as drawdown deepens: `max(floor, 1 - sens*drawdown)`.
pub struct PositionSizerDrawdownThrottle {
    pub base: Box<dyn PositionSizer>,
    pub sensitivity: f64,
    pub floor: f64,
}
impl PositionSizer for PositionSizerDrawdownThrottle {
    fn size(&self, ctx: &PositionSizerContext) -> f64 {
        let factor = self.floor.max(1.0 - self.sensitivity * ctx.drawdown);
        self.base.size(ctx) * factor
    }
}
