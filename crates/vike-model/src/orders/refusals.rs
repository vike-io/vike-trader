//! The ORDER refusal vocabulary several crates raise: why the pre-trade gate denied an order
//! ([`OrderDenial`], the words an `OrderDenied` and a backtest's `OrderDrop` carry), and the families of
//! order, cancel and modify rejections that more than one producer synthesizes ([`OrderRefusal`]). One
//! table each through `vike_utility::refusal_enum!`, so every id is a variant's declared word.
//!
//! **An id rides the event as a SHOW-ONLY string** (`crate::events::OrderRejected::refusal_id`, and the
//! same field on `OrderCancelRejected`, `OrderModifyRejected` and `OrderDenied`): a person reads it
//! beside the event's `reason` and looks it up; no code decides on it (a refusal a reader ACTS on is a
//! typed `crate::RejectCause` instead). A reader that does not know an id shows it as it came, so a new
//! id needs no readers-first deploy, and an event without one is byte-identical to before the field.
//!
//! **Where an id lives.** Here when several crates raise it or a crate below the core does; a refusal
//! only one crate raises is that crate's own `refusals.rs` (`vike-core`'s `CoreOrderRefusal` in
//! `crates/vike-core/src/runtime/refusals.rs`). Every id is `order.`-dotted and unique across them.
//!
//! **The texts.** An [`OrderDenial`]'s literal IS the reason word the gate writes, byte-identical to the
//! words journals, results JSON and `vike_analytics`' zero-trade grouping already hold; a rule that adds
//! its figures appends them after the word (`over-account-exposure (projected …)`, `leg BTC-…: no-mark`),
//! so the word stays the reason's head. An [`OrderRefusal`]'s literal is its family's headline; the event's
//! `reason` keeps the producer's own words (the venue's, the link's, the capability table's).

vike_utility::refusal_enum! {
    /// Why the pre-trade gate DENIED (or a backtest DROPPED) an order: `vike_execution::RiskGate`'s lanes,
    /// the engine's own gates before it (the symbol admission, the spot cash rules), a mount's leverage
    /// cap, and the backtest's two drops of its own. The literal is the reason's head word.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum OrderDenial with ids OrderDenialId {
        /// The side is neither +1 nor −1.
        InvalidSide => "order.risk.invalid_side": "invalid-side",
        /// The trading state is HALTED and the order is no covered reduce.
        Halted => "order.risk.halted": "halted",
        /// The trading state is REDUCING and the order does not reduce.
        ReduceOnly => "order.risk.reduce_only": "reduce-only",
        /// A reduce-only order larger than the position it reduces.
        ReduceOnlyOvershoot => "order.risk.reduce_only_overshoot": "reduce-only-overshoot",
        /// The quantity, on the symbol's lot grid, is not above zero.
        NonPositiveSize => "order.risk.non_positive_size": "non-positive-size",
        /// The conversion rate of a foreign-quoted symbol has gone stale.
        QuoteRateStale => "order.risk.quote_rate_stale": "quote-rate-stale",
        /// The engine holds no mark at all for its mounted symbol.
        NoMarkForMountedSymbol => "order.risk.no_mark_for_mounted_symbol": "no-mark-for-mounted-symbol",
        /// The price is outside the collar around the mark.
        PriceCollar => "order.risk.price_collar": "price-collar",
        /// Below the symbol's minimum quantity.
        BelowMinQty => "order.risk.below_min_qty": "below-min-qty",
        /// A priceless order on a symbol whose reference price has not arrived.
        NoReferencePrice => "order.risk.no_reference_price": "no-reference-price",
        /// Below the symbol's minimum notional.
        BelowMinNotional => "order.risk.below_min_notional": "below-min-notional",
        /// Above the per-order notional cap.
        OverMaxNotional => "order.risk.over_max_notional": "over-max-notional",
        /// The symbol's projected exposure passes its cap.
        OverMaxExposure => "order.risk.over_max_exposure": "over-max-exposure",
        /// The account's exposure cannot be valued, so its ceiling cannot be judged.
        AccountExposureUnpriced => "order.risk.account_exposure_unpriced": "account-exposure-unpriced",
        /// The account's projected exposure passes its ceiling.
        OverAccountExposure => "order.risk.over_account_exposure": "over-account-exposure",
        /// Not enough free buying power for the order's margin.
        InsufficientMargin => "order.risk.insufficient_margin": "insufficient-margin",
        /// The order's projected slippage against the book passes the budget.
        ImpactOverSlippageBudget => "order.risk.impact_over_slippage_budget": "impact-over-slippage-budget",
        /// The displayed book cannot fill the whole order.
        ImpactNotFillable => "order.risk.impact_not_fillable": "impact-not-fillable",
        /// The session's order-rate window is full.
        RateLimited => "order.risk.rate_limited": "rate-limited",
        /// A combo check was asked of an order with no legs.
        NotACombo => "order.risk.not_a_combo": "not-a-combo",
        /// One symbol in two legs of a combo.
        RepeatedLegSymbol => "order.risk.repeated_leg_symbol": "repeated-leg-symbol",
        /// A combo leg with ratio zero.
        LegZeroRatio => "order.risk.leg_zero_ratio": "zero-ratio",
        /// A combo leg whose symbol has no mark.
        LegNoMark => "order.risk.leg_no_mark": "no-mark",
        /// A combo leg whose size is not a lot multiple.
        LegOffGridQty => "order.risk.leg_off_grid_qty": "off-grid-qty",
        /// The order's symbol was never admitted on this engine.
        SymbolNotAdmitted => "order.risk.symbol_not_admitted": "symbol-not-admitted",
        /// A spot (cash) account sells more than it holds.
        CashOversell => "order.risk.cash_oversell": "a spot account cannot sell more than it holds",
        /// A spot (cash) account buys more than its free cash pays for.
        CashOverbuy => "order.risk.cash_overbuy":
            "a spot account cannot buy more than its free cash pays for, the fee included",
        /// An opening order passes the mount's leverage cap.
        OverMountLeverage => "order.risk.over_mount_leverage": "over-mount-leverage",
        /// A backtest's fill took only the bar's volume cap; the rest was dropped.
        VolumeCap => "order.backtest.volume_cap": "volume_cap",
        /// A backtest's latency model refused the order.
        LatencyReject => "order.backtest.latency_reject": "latency_reject",
    }
}

vike_utility::refusal_enum! {
    /// The order, cancel and modify rejections that more than one producer synthesizes, by family. The
    /// literal is the family's headline; the event's `reason` keeps the producer's own words.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum OrderRefusal with ids OrderRefusalId {
        /// The HALT file is present (`vike_execution::halt`): no order leaves the process. Its literal is
        /// the event's reason itself (`vike_execution::halt::HALT_REJECT_REASON` reads it).
        HaltFile => "order.halted": "halted: HALT file present",
        /// It never left: no venue path could take it (the exec thread, the adapter, the venue session or
        /// the gateway link is down, or a pre-send check failed), so it is safe to send again.
        NotSent => "order.not_sent": "the order was not sent",
        /// The command journal could not record it (a full, read-only or failing disk), so nothing that can
        /// open risk leaves (`vike_execution::execution_engine::JOURNAL_CANNOT_WRITE_REASON` is the reason).
        JournalCannotWrite => "order.journal_cannot_write": "the command journal could not record the order",
        /// It may have reached the venue (the adapter failed while handling it): reconcile finds it.
        OutcomeUnknown => "order.outcome_unknown": "the order may have reached the venue",
        /// The venue answered no; the reason is the venue's own words (its codes are mapped per venue later).
        VenueRefused => "order.venue_refused": "the venue refused the order",
        /// The venue cannot do what was asked (an order kind, a lifetime, an amend, a combo): refused
        /// before anything was sent, the reason naming the capability.
        VenueCapability => "order.venue_capability": "the venue does not offer this",
        /// The account is ARMED with its orders HELD (`vike_bridge_core`'s `ExecutionClientOrdersHeld`): its
        /// order path is not proven at its tier, so it serves data and reconcile and refuses every order.
        OrdersHeld => "order.orders_held": "this account's orders are held at its arming",
        /// An amend the node refused, or could not complete, for a reason of the order's own (it no longer
        /// rests, nothing of it is left, another amend is in flight, the node holds it itself, the
        /// cancel-then-new did not end with the new order resting).
        AmendRefused => "order.amend_refused": "the amend was refused",
    }
}

#[path = "refusals_tests.rs"]
#[cfg(test)]
mod refusals_tests;
