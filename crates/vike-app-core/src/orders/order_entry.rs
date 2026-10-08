//! `order_entry` — the single home for turning a UI order intent into a
//! [`vike_model::OrderRequest`], plus a **local** preview/safety layer that runs BEFORE the
//! command reaches the core command lane.
//!
//! This is the CI-testable extraction of the order-write construction that used to live inline,
//! hand-rolled, at each scattered submit site in `vike-app`'s `main.rs` (the DOM ladder actions
//! loop, the options confirm-ticket submit, the options-chain cancel). Those sites each rebuilt an
//! `OrderRequest { .. }` literal and minted a `client_order_id` with an ad-hoc `format!` — untested
//! and impossible to reach in CI (the GUI crate is excluded for wgpu build weight). This module has
//! NO Python twin: it is the vike-native "order-entry" seam, mirroring the CLI/MCP competitor
//! study's preview + local-check + env-caps safety idea.
//!
//! Layering: egui-free (pure logic), depends on `vike-model` only — so it builds and unit-tests on
//! CI exactly like the other `vike-app-core` modules. The shell (`vike-desktop`; `vike-app` when
//! this was written) reaches [`build_order_request`] + [`validate`] where it used to construct the
//! literal, through [`crate::orders::order_dispatch`].
//!
//! ## The safety layer is a PREVIEW, not the risk authority
//! [`validate`] runs LOCAL checks only — no venue call, no account state. It catches the orders that
//! are *self-evidently* malformed (non-finite qty/price, non-positive qty, a limit with no price, a
//! qty/notional over a configured cap — notional measured as `|qty| × |price| × |multiplier|`, the
//! same magnitude `vike_exec::RiskGate` and `SimBroker` measure) so they are dropped with a warn instead of round-tripping to
//! a venue that would only reject them. It is NOT a replacement for the venue-side
//! [`vike_exec::RiskGate`] (which owns min-notional/position/leverage policy against live
//! `SymbolProperties`). Absent an env cap the default is permissive: only the always-invalid shapes
//! are rejected.

use vike_model::OrderRequest;

/// The three order types the live venues accept, matching the string `OrderRequest.order_type`
/// expects (`"limit" | "market" | "stop"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderKind {
    #[default]
    Limit,
    Market,
    Stop,
}

impl OrderKind {
    /// The wire string written into `OrderRequest.order_type`.
    pub fn as_str(self) -> &'static str {
        match self {
            OrderKind::Limit => "limit",
            OrderKind::Market => "market",
            OrderKind::Stop => "stop",
        }
    }
}

/// A UI order intent, venue-agnostic. Carries exactly the fields the scattered submit sites set:
/// the ladder's Place shape (limit/stop with `trigger_price` + `reduce_only`) AND the options-ticket
/// shape (a plain deribit limit). Everything else on `OrderRequest` comes from its `Default`.
///
/// `Default`-derived so callers build with struct-update syntax; the [`OrderTicket::limit`] /
/// [`OrderTicket::stop`] / [`OrderTicket::market`] constructors cover the common shapes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderTicket {
    pub venue: String,
    pub instrument: String,
    /// vike order side: +1 Buy / -1 Sell.
    pub side: i32,
    pub qty: f64,
    pub price: Option<f64>,
    pub order_type: OrderKind,
    /// stop trigger price (set for `OrderKind::Stop`; `None` otherwise).
    pub trigger_price: Option<f64>,
    pub reduce_only: bool,
}

impl OrderTicket {
    /// A limit order (`price` = the limit price, no trigger).
    pub fn limit(
        venue: impl Into<String>,
        instrument: impl Into<String>,
        side: i32,
        qty: f64,
        price: f64,
    ) -> Self {
        Self {
            venue: venue.into(),
            instrument: instrument.into(),
            side,
            qty,
            price: Some(price),
            order_type: OrderKind::Limit,
            trigger_price: None,
            reduce_only: false,
        }
    }

    /// A stop order (`trigger_price` = the stop trigger; no resting limit `price`) — the shape a
    /// Stop from the Trade window's ladder or ticket takes.
    pub fn stop(
        venue: impl Into<String>,
        instrument: impl Into<String>,
        side: i32,
        qty: f64,
        trigger: f64,
    ) -> Self {
        Self {
            venue: venue.into(),
            instrument: instrument.into(),
            side,
            qty,
            price: None,
            order_type: OrderKind::Stop,
            trigger_price: Some(trigger),
            reduce_only: false,
        }
    }

    /// A market order (no price, no trigger).
    pub fn market(
        venue: impl Into<String>,
        instrument: impl Into<String>,
        side: i32,
        qty: f64,
    ) -> Self {
        Self {
            venue: venue.into(),
            instrument: instrument.into(),
            side,
            qty,
            price: None,
            order_type: OrderKind::Market,
            trigger_price: None,
            reduce_only: false,
        }
    }
}

/// The ONE `OrderRequest` constructor. Fills the seven live-path fields the UI sets and leaves the
/// rest to `OrderRequest::default()`, so it is byte-identical to the `OrderRequest { .., ..Default::default() }`
/// literals it replaces.
pub fn build_order_request(ticket: &OrderTicket, coid: String) -> OrderRequest {
    OrderRequest {
        client_order_id: coid,
        venue: ticket.venue.clone(),
        symbol: ticket.instrument.clone(),
        side: ticket.side,
        qty: ticket.qty,
        order_type: ticket.order_type.as_str().to_string(),
        price: ticket.price,
        trigger_price: ticket.trigger_price,
        reduce_only: ticket.reduce_only,
        ..Default::default()
    }
}

/// The one client-order-id generator: `"{prefix}-{seq}"`. Centralizes the ad-hoc `format!("dom-…")`
/// / `format!("opt-…")` sites. The coid is an opaque, unique routing tag; the engine keys resting
/// orders by it (it never leaves vike as a semantic id).
pub fn next_client_order_id(prefix: &str, seq: u64) -> String {
    format!("{prefix}-{seq}")
}

/// Local, venue-free order caps for the [`validate`] preview. Permissive by default (only the
/// always-invalid shapes are rejected); tighten via [`OrderLimits::with_max_notional`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderLimits {
    pub min_qty: f64,
    pub max_qty: f64,
    pub max_notional: f64,
    /// If true (default), a `"limit"` order with no `price` is rejected as [`OrderReject::MissingLimitPrice`].
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
    /// startup) — the same caller-owns-the-I/O rule as
    /// `vike_tradehub_client::auth::from_vars`, and what keeps the cap unit-testable
    /// without touching any global state.
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
}

/// Why a local preview rejected an order. Distinct variants so the warn log names the exact reason.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderReject {
    /// qty, price, or trigger_price is NaN/infinite.
    NonFiniteQtyOrPrice,
    /// qty is non-positive or below `min_qty`.
    QtyBelowMin { qty: f64, min: f64 },
    /// qty exceeds `max_qty`.
    QtyAboveMax { qty: f64, max: f64 },
    /// A limit order with no price (and `require_price_for_limit`).
    MissingLimitPrice,
    /// `|qty| × |price| × |multiplier|` exceeds `max_notional`.
    NotionalAboveMax { notional: f64, max: f64 },
}

impl std::fmt::Display for OrderReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderReject::NonFiniteQtyOrPrice => write!(f, "non-finite qty or price"),
            OrderReject::QtyBelowMin { qty, min } => write!(f, "qty {qty} below minimum {min}"),
            OrderReject::QtyAboveMax { qty, max } => write!(f, "qty {qty} above maximum {max}"),
            OrderReject::MissingLimitPrice => write!(f, "limit order missing price"),
            OrderReject::NotionalAboveMax { notional, max } => {
                write!(f, "notional {notional} above maximum {max}")
            }
        }
    }
}

impl std::error::Error for OrderReject {}

/// Local preview: reject the self-evidently malformed / over-cap orders before they reach the
/// command lane. LOCAL checks only (no venue call). Returns `Ok(())` for an order the UI may submit;
/// on `Err` the caller logs a warn and SKIPS the submit (today such an order would be sent and the
/// venue would reject it — this catches it one hop earlier). NOT a substitute for the venue-side
/// [`vike_exec::RiskGate`].
///
/// Checks, in order: qty/price/trigger finite; qty strictly positive AND ≥ `min_qty`; qty ≤
/// `max_qty`; a `"limit"` order has a price (when `require_price_for_limit`); and, when a price is
/// present, `|qty| × |price| × |multiplier| ≤ max_notional`. Market orders carry no price, so their
/// notional is not checked here (the venue RiskGate owns that).
///
/// # The contract multiplier is assumed 1.0 here
/// This entry point cannot see the order's contract multiplier, so it measures notional at
/// `multiplier = 1.0`. That is EXACT for spot/linear instruments (the overwhelming majority) and
/// an UNDER-measure by the multiplier factor for options and inverse perps. Callers that can
/// resolve the instrument's multiplier MUST use [`validate_with_multiplier`] instead.
///
/// The multiplier IS reachable at the UI submit sites now: `vike_core::CoreSnapshot::
/// multiplier_of(venue, symbol)` publishes `vike_exec::Account::multiplier_of` (the authority)
/// onto the snapshot, fed from `SymbolProperties.contract_size` at instrument fetch, and the
/// dispatcher every UI order leaves through (`order_dispatch`'s `Planner::admit` and
/// `admit_bracket`: the Trade window's ladder and ticket, the deribit options confirm ticket) calls
/// [`validate_with_multiplier`] through it — a venue/symbol absent from the grid resolves to the
/// 1.0 default, byte-identical to this entry point. `validate` remains for callers with no
/// snapshot in hand; `RiskGate` stays the venue-side backstop either way.
pub fn validate(req: &OrderRequest, limits: &OrderLimits) -> Result<(), OrderReject> {
    validate_with_multiplier(req, limits, 1.0)
}

/// [`validate`], with the order's contract multiplier supplied by the caller — the form that
/// measures notional the way `vike_exec::RiskGate` and `SimBroker` do.
///
/// `multiplier` is the instrument's contract multiplier (`vike_exec::Account::multiplier_of`;
/// 1.0 for spot/linear instruments, 100 for a typical equity option, and the contract size for an
/// inverse perp). It enters the notional as a MAGNITUDE. A non-finite `multiplier` is rejected as
/// [`OrderReject::NonFiniteQtyOrPrice`] rather than propagated: NaN would make every `>` cap
/// comparison false and so silently DISABLE the cap — the exact failure this check exists to stop.
pub fn validate_with_multiplier(
    req: &OrderRequest,
    limits: &OrderLimits,
    multiplier: f64,
) -> Result<(), OrderReject> {
    if !multiplier.is_finite() {
        return Err(OrderReject::NonFiniteQtyOrPrice);
    }
    if !req.qty.is_finite() {
        return Err(OrderReject::NonFiniteQtyOrPrice);
    }
    if let Some(p) = req.price
        && !p.is_finite()
    {
        return Err(OrderReject::NonFiniteQtyOrPrice);
    }
    if let Some(t) = req.trigger_price
        && !t.is_finite()
    {
        return Err(OrderReject::NonFiniteQtyOrPrice);
    }
    // Non-positive qty is always invalid (caught even with the permissive min_qty = 0.0 default).
    if req.qty <= 0.0 || req.qty < limits.min_qty {
        return Err(OrderReject::QtyBelowMin { qty: req.qty, min: limits.min_qty });
    }
    if req.qty > limits.max_qty {
        return Err(OrderReject::QtyAboveMax { qty: req.qty, max: limits.max_qty });
    }
    if limits.require_price_for_limit && req.order_type == "limit" && req.price.is_none() {
        return Err(OrderReject::MissingLimitPrice);
    }
    if let Some(p) = req.price {
        // NOTIONAL IS A MAGNITUDE — all three factors enter ABSOLUTE. This used to be a third
        // hand-written copy of that product; it now CALLS the one helper
        // (`vike_model::order_notional`), the same call `vike_exec::RiskGate` and
        // `SimBroker::apply_fill` make, so the UI cap and both engines can no longer drift apart.
        // A signed factor would let an arbitrarily large credit combo escape the cap entirely
        // (`notional > cap` could never trip), which is why the sign is stripped rather than
        // trusted. The hoist is bit-identical: the inline form took the same three magnitudes.
        //
        // THE CONTRACT MULTIPLIER IS PART OF THE NOTIONAL. Omitting it made this cap measure a
        // DIFFERENT quantity than both engines for every multiplier != 1 instrument (options,
        // inverse perps): the UI under-measured by the multiplier factor and passed orders it
        // should have stopped. `multiplier` is 1.0 on the [`validate`] path, so every
        // multiplier-1 verdict is bit-identical (`x * 1.0` is an IEEE-754 no-op).
        let notional = vike_model::order_notional(req.qty, p, multiplier);
        if notional > limits.max_notional {
            return Err(OrderReject::NotionalAboveMax { notional, max: limits.max_notional });
        }
    }
    Ok(())
}

#[path = "order_entry_tests.rs"]
#[cfg(test)]
mod order_entry_tests;
