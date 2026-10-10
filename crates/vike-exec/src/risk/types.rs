//! The gate's runtime data: `RiskContext` (what an order is judged against) and `RiskVerdict`
//! (the answer). The gate's CONFIGURATION, `RiskLimits` and its per-symbol overrides, is
//! vike-model's (`crates/vike-model/src/risk/limits.rs`).

use vike_model::OrderRequest;
#[cfg(doc)]
use vike_model::RiskLimits;

use super::TradingState;

/// Runtime state the gate evaluates an order against.
#[derive(Debug, Clone, Copy)]
pub struct RiskContext {
    /// Current SIGNED position in the ORDER's symbol, at the engine's OWN venue — exactly one
    /// `(venue, symbol)` bucket, never a book-wide total. `ExecutionEngine::gate_position_size` is
    /// the producer (it nets the hedge `LONG`/`SHORT` buckets when the one-way `BOTH` bucket is
    /// flat). Every position-derived verdict in this gate inherits that scope, including
    /// [`RiskLimits::max_total_exposure`]'s — see that field's doc.
    pub position_size: f64,
    /// price used for notional when the order has none
    pub mark_price: f64,
    pub trading_state: TradingState,
    /// injected clock (for the throttler)
    pub now_ms: i64,
    /// account equity (margin check; unused unless `im_requirement` is set)
    pub equity: f64,
    /// Σ **committed** initial margin, account currency (0.0 when unused): open positions PLUS this
    /// engine's live un-filled orders — without the orders term `free_buying_power` overstates
    /// what the account can back by everything in flight. `ExecutionEngine::live_order_margin` is
    /// that half; `RiskGate::check_combo`'s `committed_margin` is the same idea for legs admitted
    /// earlier in one combo.
    pub margin_used: f64,
    /// margin freed + re-open credit when this order reverses the position (LEAN
    /// `GetMarginRemaining` closing branch; 0.0 for same-direction opens)
    pub closing_credit: f64,
    /// contract multiplier of the order's symbol (1.0 when unused)
    pub multiplier: f64,
    /// **This ACCOUNT's gross exposure with the ORDER UNDER JUDGEMENT left out** — the other half
    /// of [`RiskLimits::max_account_exposure`]'s comparison, `0.0` when that ceiling is unarmed.
    ///
    /// TWO terms, both gross, never netting a long against a short:
    /// * Σ `|size| × resolver-price × multiplier` over every OPEN POSITION of this engine's own
    ///   account except the order symbol's, and
    /// * Σ `remaining × resolver-price × multiplier` over every LIVE, UN-FILLED ORDER of this
    ///   engine except the one being judged.
    ///
    /// `ExecutionEngine::resolved_account_exposure_excluding` is the producer and the authority on
    /// every skip (a flat leg, an unpriceable one, a foreign-venue row, a covered reduce).
    ///
    /// ⚠ **The RESTING-ORDER term is what makes this a ceiling rather than a suggestion**: without
    /// it, N orders submitted before any fills each see the same pre-order account and each passes
    /// (the hole `ExecutionEngine::live_order_margin` closes on the buying-power lane).
    ///
    /// ⚠ **The order's own SYMBOL's position is EXCLUDED because the gate re-adds it PROJECTED**
    /// (its resting orders are not: nothing projects those). Leaving it in would double-count the
    /// position the order is about — a reduce would report the account growing as it shrinks.
    ///
    /// ⚠ **The two halves use different bases for the one symbol they meet on, deliberately.**
    /// Everything here is GROSS (a hedge-mode LONG/SHORT pair sums to both legs), while the order
    /// symbol re-enters NET (`ExecutionEngine::gate_position_size`). Net is the only projectable
    /// basis: `vike_model::OrderRequest` names no position bucket, so which one an order lands in
    /// is the venue's routing decision. The difference is bounded by one symbol's hedged overlap
    /// and errs LOW there; nowhere else.
    pub account_exposure_excl_order: f64,
}

impl Default for RiskContext {
    fn default() -> Self {
        RiskContext {
            position_size: 0.0,
            mark_price: 0.0,
            trading_state: TradingState::Active,
            now_ms: 0,
            equity: 0.0,
            margin_used: 0.0,
            closing_credit: 0.0,
            multiplier: 1.0,
            // `0.0` is "this account holds nothing else", the only safe default for a term ADDED
            // to the projection: a non-zero default would deny orders on an undescribed account.
            account_exposure_excl_order: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RiskVerdict {
    pub ok: bool,
    /// normalized (rounded) request when ok
    pub request: Option<OrderRequest>,
    pub reason: String,
}
