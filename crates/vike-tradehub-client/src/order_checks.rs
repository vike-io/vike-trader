//! `order_checks` — the client-side order checks a surface runs on an order BEFORE it reaches a node:
//! the self-evidently malformed shapes and this machine's own caps ([`OrderLimits`], refused with an
//! [`OrderLimitsError`]).
//!
//! ## A PREVIEW, not the risk authority
//! [`validate`] runs LOCAL checks only — no venue call, no account state. It catches the orders that
//! are *self-evidently* malformed (non-finite qty/price, non-positive qty, a limit with no price, a
//! qty/notional over a configured cap — notional measured as `|qty| × |price| × |multiplier|`, the
//! same magnitude `vike_execution::RiskGate` and `OrderBrokerSimulator` measure) so they are dropped
//! instead of round-tripping to a venue that would only reject them. It is NOT a replacement for the
//! venue-side `vike_execution::RiskGate` (which owns min-notional/position/leverage policy against
//! live `SymbolProperties`). Absent a policy cap the default is permissive: only the always-invalid
//! shapes are rejected.
//!
//! It lives in the shared client, below every surface, so the desktop's order dispatcher
//! (`crates/vike-order-entry/src/dispatch/mod.rs`) reads one cap with one error. PURE over
//! `vike-model` types: no I/O, no setting read (the cap arrives as a value).

use vike_model::{ContractValue, OrderRequest, OrderType};

/// Local, venue-free order caps for the [`validate`] preview. Permissive by default (only the
/// always-invalid shapes are rejected); tighten via [`OrderLimits::with_max_notional`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderLimits {
    pub min_qty: f64,
    pub max_qty: f64,
    pub max_notional: f64,
    /// If true (default), a `"limit"` order with no `price` is rejected as [`OrderLimitsError::MissingLimitPrice`].
    pub require_price_for_limit: bool,
}

impl Default for OrderLimits {
    /// Permissive: no upper qty/notional cap, but a limit still needs a price. `min_qty = 0.0`
    /// (non-positive qty is rejected regardless — see [`validate`]).
    fn default() -> Self {
        Self {
            min_qty: 0.0,
            max_qty: f64::INFINITY,
            max_notional: f64::INFINITY,
            require_price_for_limit: true,
        }
    }
}

impl OrderLimits {
    /// The default caps, tightened by the deployment's **policy ceiling**:
    /// `vike_config::Policy::max_notional_per_order`, i.e. the `policy.max_notional_per_order`
    /// settings row. `None` — no `policy` rows, or the key omitted — leaves the permissive default
    /// untouched, so an install with no policy rows behaves exactly as before.
    ///
    /// ⚠ **This used to be `from_max_notional(Option<&str>)`, fed from
    /// `VIKE_MAX_ORDER_NOTIONAL`** — an order-size ceiling any exported variable could raise, which
    /// is not a ceiling: a shell export, a stale systemd unit or an inherited parent widened it
    /// with no file changed and no diff. Phase 5 of the settings-unification design deleted that
    /// read; `vike_config::refuse_removed_env` makes a process that still finds the variable set
    /// refuse to start rather than silently ignore a limit its operator believes is active.
    ///
    /// Still takes the value as a PARAMETER rather than resolving policy itself: this is a
    /// LIBRARY, so the load belongs in the binary (`vike-desktop`'s `main.rs` resolves it once at
    /// startup) — the caller-owns-the-I/O rule every library here follows, and what keeps
    /// the cap unit-testable without touching any global state.
    ///
    /// A non-finite or non-positive ceiling is ignored rather than honoured — `0.0` would deny
    /// every order, a silent halt. (`vike_config` already rejects both when loading the `policy`
    /// rows, naming the key; this is the belt to that braces, for a caller that built a
    /// `Policy` some other way.)
    pub fn with_max_notional(max_notional_per_order: Option<f64>) -> Self {
        let mut l = Self::default();
        if let Some(n) = max_notional_per_order
            && n.is_finite()
            && n > 0.0
        {
            l.max_notional = n;
        }
        l
    }

    /// **The notional cap**: `notional` (an order's `ContractValue::order_notional`, measured by the
    /// caller) over [`OrderLimits::max_notional`] is [`OrderLimitsError::NotionalAboveMax`]; at the cap
    /// it passes (strict `>`). The ONE comparison [`validate_with_value`] makes.
    pub fn check_notional(&self, notional: f64) -> Result<(), OrderLimitsError> {
        if notional > self.max_notional {
            return Err(OrderLimitsError::NotionalAboveMax { notional, max: self.max_notional });
        }
        Ok(())
    }
}

/// Why a local preview rejected an order. Distinct variants so the warn log names the exact reason.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderLimitsError {
    /// qty, price, or trigger_price is NaN/infinite.
    NonFiniteQuantityOrPrice,
    /// qty is non-positive or below `min_qty`.
    QuantityBelowMin { qty: f64, min: f64 },
    /// qty exceeds `max_qty`.
    QuantityAboveMax { qty: f64, max: f64 },
    /// A limit order with no price (and `require_price_for_limit`).
    MissingLimitPrice,
    /// `|qty| × |price| × |multiplier|` exceeds `max_notional`.
    NotionalAboveMax { notional: f64, max: f64 },
}

impl std::fmt::Display for OrderLimitsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderLimitsError::NonFiniteQuantityOrPrice => write!(f, "non-finite qty or price"),
            OrderLimitsError::QuantityBelowMin { qty, min } => {
                write!(f, "qty {qty} below minimum {min}")
            }
            OrderLimitsError::QuantityAboveMax { qty, max } => {
                write!(f, "qty {qty} above maximum {max}")
            }
            OrderLimitsError::MissingLimitPrice => write!(f, "limit order missing price"),
            OrderLimitsError::NotionalAboveMax { notional, max } => {
                write!(f, "notional {notional} above maximum {max}")
            }
        }
    }
}

impl std::error::Error for OrderLimitsError {}

/// Local preview: reject the self-evidently malformed / over-cap orders before they reach the
/// command lane. LOCAL checks only (no venue call). Returns `Ok(())` for an order the UI may submit;
/// on `Err` the caller logs a warn and SKIPS the submit (today such an order would be sent and the
/// venue would reject it — this catches it one hop earlier). NOT a substitute for the venue-side
/// `vike_execution::RiskGate`.
///
/// Checks, in order: qty/price/trigger finite; qty strictly positive AND ≥ `min_qty`; qty ≤
/// `max_qty`; a `"limit"` order has a price (when `require_price_for_limit`); and, when a price is
/// present, `|qty| × |price| × |multiplier| ≤ max_notional`. Market orders carry no price, so their
/// notional is not checked here (the venue RiskGate owns that).
///
/// # The contract multiplier is assumed 1.0 here
/// This entry point cannot see the order's contract value, so it measures notional at the default
/// value (`Linear`, 1.0). That is EXACT for spot/linear instruments (the overwhelming majority) and
/// a wrong measure for options and inverse perps. Callers that can resolve the instrument's value
/// MUST use [`validate_with_value`] instead.
///
/// The value IS reachable at the UI submit sites now: each `vike_execution::VenueBlock::
/// contract_value_of(symbol)` publishes its engine's `vike_execution::Account::contract_value_of` (the
/// authority) onto the snapshot, minted at the mount from the venue's instrument row, and the
/// dispatcher every UI order leaves through (`vike_order_entry::dispatch`'s `OrderEntryDispatchPlanner::admit` and
/// `admit_bracket`: the Trade window's ladder and ticket, the deribit options confirm ticket) calls
/// [`validate_with_value`] with the value of the block of the ACCOUNT the order names
/// (`vike_order_entry::dispatch`'s `contract_value_at`) — a symbol absent from the grid resolves to the
/// 1.0 default, byte-identical to this entry point. `validate` remains for callers with no
/// snapshot in hand; `RiskGate` stays the venue-side backstop either way.
pub fn validate(req: &OrderRequest, limits: &OrderLimits) -> Result<(), OrderLimitsError> {
    validate_with_value(req, limits, ContractValue::default())
}

/// [`validate`], with the order's contract value supplied by the caller — the form that measures
/// notional the way `vike_execution::RiskGate` and `OrderBrokerSimulator` do, by the SAME function
/// (`ContractValue::order_notional`, decision 0170).
///
/// `value` is the instrument's contract value (`vike_execution::Account::contract_value_of`): linear
/// with a per-unit factor of 1.0 for spot/linear instruments and 100 for a typical equity option,
/// and INVERSE for a coin-margined perpetual, whose quantity is already a quote amount (the price
/// does not enter). The factor enters the notional as a MAGNITUDE. A non-finite factor is rejected
/// as [`OrderLimitsError::NonFiniteQuantityOrPrice`] rather than propagated: NaN would make every `>` cap
/// comparison false and so silently DISABLE the cap — the exact failure this check exists to stop.
pub fn validate_with_value(
    req: &OrderRequest,
    limits: &OrderLimits,
    value: ContractValue,
) -> Result<(), OrderLimitsError> {
    if !value.per_unit.is_finite() {
        return Err(OrderLimitsError::NonFiniteQuantityOrPrice);
    }
    if !req.qty.is_finite() {
        return Err(OrderLimitsError::NonFiniteQuantityOrPrice);
    }
    if let Some(p) = req.price
        && !p.is_finite()
    {
        return Err(OrderLimitsError::NonFiniteQuantityOrPrice);
    }
    if let Some(t) = req.trigger_price
        && !t.is_finite()
    {
        return Err(OrderLimitsError::NonFiniteQuantityOrPrice);
    }
    // Non-positive qty is always invalid (caught even with the permissive min_qty = 0.0 default).
    if req.qty <= 0.0 || req.qty < limits.min_qty {
        return Err(OrderLimitsError::QuantityBelowMin { qty: req.qty, min: limits.min_qty });
    }
    if req.qty > limits.max_qty {
        return Err(OrderLimitsError::QuantityAboveMax { qty: req.qty, max: limits.max_qty });
    }
    if limits.require_price_for_limit && req.order_type == OrderType::Limit && req.price.is_none() {
        return Err(OrderLimitsError::MissingLimitPrice);
    }
    if let Some(p) = req.price {
        // NOTIONAL IS A MAGNITUDE — all three factors enter ABSOLUTE. This used to be a third
        // hand-written copy of that product; it now CALLS the one helper
        // (`ContractValue::order_notional`), the same call `vike_execution::RiskGate` and
        // `OrderBrokerSimulator::apply_fill` make, so the UI cap and both engines can no longer drift apart.
        // A signed factor would let an arbitrarily large credit combo escape the cap entirely
        // (`notional > cap` could never trip), which is why the sign is stripped rather than
        // trusted. The hoist is bit-identical: the inline form took the same three magnitudes.
        //
        // THE CONTRACT VALUE IS PART OF THE NOTIONAL. Omitting it made this cap measure a
        // DIFFERENT quantity than both engines for every non-default instrument (options, inverse
        // perps). The default value is `Linear` 1.0 on the [`validate`] path, so every default
        // verdict is bit-identical (`x * 1.0` is an IEEE-754 no-op).
        limits.check_notional(value.order_notional(req.qty, p))?;
    }
    Ok(())
}

#[path = "order_checks_tests.rs"]
#[cfg(test)]
mod order_checks_tests;
