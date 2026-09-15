//! Position sizing: the swappable sizer framework (WealthLab's PosSizer model). Exact port
//! of `core/sizing.py`. The two pure unit converters (`units_from_percent`/`units_from_value`)
//! live in `vike_model::sizing` (accounting-upgrade Phase C — the live order_target verbs share
//! them); call sites spell `vike_model::` directly — no re-export here.

/// Live portfolio context handed to a sizer for one entry intent.
#[derive(Debug, Clone)]
pub struct SizeContext {
    pub symbol: String,
    /// +1 buy / -1 sell (opening direction)
    pub side: i32,
    /// the raw size the strategy passed (used only by PassThrough)
    pub intent: f64,
    /// entry basis (current close)
    pub basis_price: f64,
    pub equity: f64,
    pub cash: f64,
    pub multiplier: f64,
    /// recent average true range (0 = unavailable)
    pub atr: f64,
    /// current account drawdown fraction 0..1
    pub drawdown: f64,
    /// protective stop price for this entry (risk sizers need it)
    pub risk_stop: Option<f64>,
    /// sum of $ risk already open across the book (portfolio-heat caps)
    pub open_risk: f64,
}

/// Return the quantity (>= 0) to open for this entry intent.
///
/// `Send`-bounded (a supertrait, not per-field) so `Box<dyn PositionSizer>` is `Send` everywhere
/// it's embedded — notably `EngineParams.sizer`, which must itself be `Send` for callers that
/// move a whole `EngineParams` onto a worker thread (e.g. vike-studio's `spawn_run`). All shipped
/// sizers are plain data (f64/usize/nested `Box<dyn PositionSizer>`), so this is free today.
pub trait PositionSizer: Send {
    fn size(&self, ctx: &SizeContext) -> f64;
}

/// Default: the strategy's own size, unchanged.
pub struct PassThroughSizer;
impl PositionSizer for PassThroughSizer {
    fn size(&self, ctx: &SizeContext) -> f64 {
        ctx.intent
    }
}

/// Each entry is a fixed cash notional.
pub struct FixedDollarSizer {
    pub amount: f64,
}
impl PositionSizer for FixedDollarSizer {
    fn size(&self, ctx: &SizeContext) -> f64 {
        let denom = ctx.basis_price * ctx.multiplier;
        // Python `if denom` — truthiness = non-zero (negative allowed)
        if denom != 0.0 { self.amount / denom } else { 0.0 }
    }
}

pub struct FixedSharesSizer {
    pub shares: f64,
}
impl PositionSizer for FixedSharesSizer {
    fn size(&self, _ctx: &SizeContext) -> f64 {
        self.shares
    }
}

/// Each entry targets `pct` of current account equity.
pub struct PctEquitySizer {
    pub pct: f64,
}
impl PositionSizer for PctEquitySizer {
    fn size(&self, ctx: &SizeContext) -> f64 {
        let denom = ctx.basis_price * ctx.multiplier;
        if denom != 0.0 { (self.pct * ctx.equity) / denom } else { 0.0 }
    }
}

/// Size so the position's ATR-risk is `pct` of equity; %-equity fallback when ATR is 0.
pub struct PctVolatilitySizer {
    pub pct: f64,
}
impl PositionSizer for PctVolatilitySizer {
    fn size(&self, ctx: &SizeContext) -> f64 {
        // Python `if ctx.atr and ctx.multiplier` — both truthy (non-zero)
        if ctx.atr != 0.0 && ctx.multiplier != 0.0 {
            return (self.pct * ctx.equity) / (ctx.atr * ctx.multiplier);
        }
        let denom = ctx.basis_price * ctx.multiplier;
        if denom != 0.0 { (self.pct * ctx.equity) / denom } else { 0.0 }
    }
}

/// Size so the trade risks `pct` of equity given the stop distance; 0 without a stop.
pub struct MaxRiskPctSizer {
    pub pct: f64,
}
impl PositionSizer for MaxRiskPctSizer {
    fn size(&self, ctx: &SizeContext) -> f64 {
        let Some(stop) = ctx.risk_stop else {
            return 0.0;
        };
        let risk_per_unit = (ctx.basis_price - stop).abs() * ctx.multiplier;
        if risk_per_unit > 0.0 { (self.pct * ctx.equity) / risk_per_unit } else { 0.0 }
    }
}

/// Wrap a base sizer; cap TOTAL open risk to `max_heat * equity`.
pub struct PortfolioHeatSizer {
    pub base: Box<dyn PositionSizer>,
    pub max_heat: f64,
}
impl PositionSizer for PortfolioHeatSizer {
    fn size(&self, ctx: &SizeContext) -> f64 {
        let qty = self.base.size(ctx);
        let Some(stop) = ctx.risk_stop else {
            return qty;
        };
        let risk_per_unit = (ctx.basis_price - stop).abs() * ctx.multiplier;
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

/// Wrap a base sizer and scale down as drawdown deepens: `max(floor, 1 - sens*dd)`.
pub struct DrawdownThrottleSizer {
    pub base: Box<dyn PositionSizer>,
    pub sensitivity: f64,
    pub floor: f64,
}
impl PositionSizer for DrawdownThrottleSizer {
    fn size(&self, ctx: &SizeContext) -> f64 {
        let factor = self.floor.max(1.0 - self.sensitivity * ctx.drawdown);
        self.base.size(ctx) * factor
    }
}
