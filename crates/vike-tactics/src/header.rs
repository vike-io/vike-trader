//! What a rule declares ONCE (decision 0269's agreed shape): the defaults a call states only where
//! it differs ([`RuleDefaults`]), and the commission and slippage it states, in THE cost type
//! (`vike_model::Commission` and `vike_model::Slippage`, the owner's word of 2026-11-19); the side an order
//! takes is the order layer's `vike_model::OrderSide`.

use vike_model::{Commission, Slippage};

/// The rule's defaults, declared once ([`crate::Rule::defaults`]); a run may state another
/// ([`crate::RuleRun::defaults`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuleDefaults {
    /// The size of an entry or a free order that states none.
    pub size: f64,
    /// The most entries one side may hold when another is asked for; `0` and `1` both mean one.
    pub pyramiding: usize,
    /// The stated commission, charged on each fill of a round trip: the default of the file's `commission`
    /// param, which a backtest charges unless the run sets that param (the owner's ruling of 2026-11-18) as
    /// [`Commission::schedule`] builds it; live, the venue charges what it charges. `Commission::None`
    /// states none, so a run that sets no `commission` pays none.
    pub fee: Commission,
    /// The stated slippage, against the trader on each fill: the default of the file's `slippage` param,
    /// which a backtest charges unless the run sets that param, as the table [`Slippage::table`] builds.
    /// There is no venue table of slippage, so with neither a fill slips nothing.
    pub slippage: Slippage,
}

impl RuleDefaults {
    /// One unit, one entry a side, no commission, no slippage.
    pub const PLAIN: RuleDefaults =
        RuleDefaults { size: 1.0, pyramiding: 1, fee: Commission::None, slippage: Slippage::None };
}
