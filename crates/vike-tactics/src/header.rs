//! What a rule declares ONCE (decision 0269's agreed shape): the defaults a call states only where
//! it differs ([`RuleDefaults`]) and the fee it states ([`RuleDefaultsFee`]); the side an order takes
//! is the order layer's `vike_model::OrderSide`. Slippage is never here: it is the run's (owner,
//! 2026-11-10).

/// A commission the rule states, charged on each fill of a round trip. The crate charges nothing: a
/// backtest takes it when its profile states none, and live the venue charges what it charges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RuleDefaultsFee {
    Free,
    /// Of the fill's value, in percent as a rule states it: `0.05` is 0.05%.
    Percent(f64),
    PerContract(f64),
    PerOrder(f64),
}

/// The rule's defaults, declared once ([`crate::Rule::defaults`]); a run may state another
/// ([`crate::RuleRun::defaults`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuleDefaults {
    /// The size of an entry or a free order that states none.
    pub size: f64,
    /// The most entries one side may hold when another is asked for; `0` and `1` both mean one.
    pub pyramiding: usize,
    /// The stated cost.
    pub fee: RuleDefaultsFee,
}

impl RuleDefaults {
    /// One unit, one entry a side, no commission.
    pub const PLAIN: RuleDefaults =
        RuleDefaults { size: 1.0, pyramiding: 1, fee: RuleDefaultsFee::Free };
}
