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
//! CI exactly like the other `vike-app-core` modules. `vike-app` calls [`build_order_request`] +
//! [`validate`] where it used to construct the literal.
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
/// the DOM Place shape (limit/stop with `trigger_price` + `reduce_only`) AND the options-ticket
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

    /// A stop order (`trigger_price` = the stop trigger; no resting limit `price`) — the DOM Place
    /// `stop == true` shape.
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
    /// `vike_config::Policy::max_notional_per_order`, i.e. `max_notional_per_order` in
    /// `<vike home>/policy.toml`. `None` — no `policy.toml`, or the key omitted — leaves the
    /// permissive default untouched, so an install with no policy file behaves exactly as before.
    ///
    /// ⚠ **This used to be `from_max_notional(Option<&str>)`, fed from
    /// `VIKE_MAX_ORDER_NOTIONAL`** — an order-size ceiling any exported variable could raise, which
    /// is not a ceiling: a shell export, a stale systemd unit or an inherited parent widened it
    /// with no file changed and no diff. Phase 5 of the settings-unification design deleted that
    /// read; `vike_config::refuse_removed_env` makes a process that still finds the variable set
    /// refuse to start rather than silently ignore a limit its operator believes is active.
    ///
    /// Still takes the value as a PARAMETER rather than resolving policy itself: this is a
    /// LIBRARY, so the load belongs in the binary (`vike-app`'s `main.rs` resolves it once at
    /// startup) — the same caller-owns-the-I/O rule as
    /// `vike_tradehub_client::auth::from_vars`, and what keeps the cap unit-testable
    /// without touching any global state.
    ///
    /// A non-finite or non-positive ceiling is ignored rather than honoured — `0.0` would deny
    /// every order, a silent halt. (`vike_config` already rejects both when loading `policy.toml`,
    /// naming the file and key; this is the belt to that braces, for a caller that built a
    /// `Policy` some other way.)
    pub fn with_max_notional(max_notional_per_order: Option<f64>) -> Self {
        let mut l = Self::default();
        if let Some(n) = max_notional_per_order {
            if n.is_finite() && n > 0.0 {
                l.max_notional = n;
            }
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
/// onto the snapshot, fed from `SymbolProperties.contract_size` at instrument fetch, and both
/// vike-app submit sites (the DOM ladder `Place` and the deribit options confirm ticket) call
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
    if let Some(p) = req.price {
        if !p.is_finite() {
            return Err(OrderReject::NonFiniteQtyOrPrice);
        }
    }
    if let Some(t) = req.trigger_price {
        if !t.is_finite() {
            return Err(OrderReject::NonFiniteQtyOrPrice);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_limit_buy_matches_literal() {
        let ticket = OrderTicket::limit("binance", "BTCUSDT", 1, 0.5, 30_000.0);
        let req = build_order_request(&ticket, "dom-7".to_string());
        // Exactly what the inline `OrderRequest { .. }` literal produced at the DOM Place site.
        let expected = OrderRequest {
            client_order_id: "dom-7".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 0.5,
            order_type: "limit".to_string(),
            price: Some(30_000.0),
            trigger_price: None,
            reduce_only: false,
            ..Default::default()
        };
        assert_eq!(req, expected);
    }

    #[test]
    fn build_stop_matches_dom_place_shape() {
        // DOM Place with stop == true: order_type "stop", price None, trigger_price Some(px).
        let ticket = OrderTicket::stop("okx", "BTC-USDT", -1, 2.0, 25_000.0);
        let req = build_order_request(&ticket, "dom-9".to_string());
        assert_eq!(req.order_type, "stop");
        assert_eq!(req.price, None);
        assert_eq!(req.trigger_price, Some(25_000.0));
        assert_eq!(req.side, -1);
        assert_eq!(req.venue, "okx");
    }

    #[test]
    fn build_reduce_only_flag_carried() {
        let ticket = OrderTicket {
            reduce_only: true,
            ..OrderTicket::limit("bybit", "ETHUSDT", -1, 1.0, 2000.0)
        };
        let req = build_order_request(&ticket, "dom-1".to_string());
        assert!(req.reduce_only);
    }

    #[test]
    fn build_options_ticket_shape() {
        // Mirrors the options confirm-ticket site: venue "deribit", limit, price Some, no trigger.
        let ticket = OrderTicket::limit("deribit", "BTC-28MAR25-100000-C", 1, 3.0, 0.045);
        let req = build_order_request(&ticket, "opt-4".to_string());
        let expected = OrderRequest {
            client_order_id: "opt-4".to_string(),
            venue: "deribit".to_string(),
            symbol: "BTC-28MAR25-100000-C".to_string(),
            side: 1,
            qty: 3.0,
            order_type: "limit".to_string(),
            price: Some(0.045),
            ..Default::default()
        };
        assert_eq!(req, expected);
    }

    #[test]
    fn coid_format() {
        assert_eq!(next_client_order_id("dom", 42), "dom-42");
        assert_eq!(next_client_order_id("opt", 0), "opt-0");
    }

    #[test]
    fn validate_accepts_good_limit() {
        let req = build_order_request(
            &OrderTicket::limit("binance", "BTCUSDT", 1, 0.01, 30_000.0),
            "c-1".into(),
        );
        assert_eq!(validate(&req, &OrderLimits::default()), Ok(()));
    }

    #[test]
    fn validate_accepts_good_market() {
        // Market has no price → notional not checked, no MissingLimitPrice.
        let req =
            build_order_request(&OrderTicket::market("binance", "BTCUSDT", -1, 0.01), "c-2".into());
        assert_eq!(validate(&req, &OrderLimits::default()), Ok(()));
    }

    #[test]
    fn validate_rejects_below_min() {
        let limits = OrderLimits { min_qty: 0.1, ..OrderLimits::default() };
        let req = build_order_request(
            &OrderTicket::limit("binance", "BTCUSDT", 1, 0.05, 30_000.0),
            "c".into(),
        );
        assert!(matches!(validate(&req, &limits), Err(OrderReject::QtyBelowMin { .. })));
    }

    #[test]
    fn validate_rejects_non_positive_qty_even_with_permissive_min() {
        let req =
            build_order_request(&OrderTicket::market("binance", "BTCUSDT", 1, 0.0), "c".into());
        assert!(matches!(
            validate(&req, &OrderLimits::default()),
            Err(OrderReject::QtyBelowMin { .. })
        ));
        let req_neg =
            build_order_request(&OrderTicket::market("binance", "BTCUSDT", 1, -1.0), "c".into());
        assert!(matches!(
            validate(&req_neg, &OrderLimits::default()),
            Err(OrderReject::QtyBelowMin { .. })
        ));
    }

    #[test]
    fn validate_rejects_above_max() {
        let limits = OrderLimits { max_qty: 1.0, ..OrderLimits::default() };
        let req =
            build_order_request(&OrderTicket::market("binance", "BTCUSDT", 1, 5.0), "c".into());
        assert!(matches!(validate(&req, &limits), Err(OrderReject::QtyAboveMax { .. })));
    }

    #[test]
    fn validate_rejects_non_finite() {
        let req = build_order_request(
            &OrderTicket::market("binance", "BTCUSDT", 1, f64::NAN),
            "c".into(),
        );
        assert_eq!(validate(&req, &OrderLimits::default()), Err(OrderReject::NonFiniteQtyOrPrice));
        let req_inf_px = build_order_request(
            &OrderTicket::limit("binance", "BTCUSDT", 1, 1.0, f64::INFINITY),
            "c".into(),
        );
        assert_eq!(
            validate(&req_inf_px, &OrderLimits::default()),
            Err(OrderReject::NonFiniteQtyOrPrice)
        );
    }

    #[test]
    fn validate_rejects_missing_limit_price() {
        // A limit with no price. Build the request directly since the limit ctor always sets a price.
        let req = OrderRequest {
            client_order_id: "c".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: None,
            ..Default::default()
        };
        assert_eq!(validate(&req, &OrderLimits::default()), Err(OrderReject::MissingLimitPrice));
    }

    #[test]
    fn validate_rejects_over_notional() {
        let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
        // 0.01 * 30_000 = 300 > 100.
        let req = build_order_request(
            &OrderTicket::limit("binance", "BTCUSDT", 1, 0.01, 30_000.0),
            "c".into(),
        );
        assert!(matches!(validate(&req, &limits), Err(OrderReject::NotionalAboveMax { .. })));
    }

    /// COMPAT PIN: `multiplier = 1.0` — the default, i.e. every spot/linear instrument —
    /// reproduces the pre-fix `qty * price` verdict exactly, on both sides of the cap.
    /// `x * 1.0` is an IEEE-754 no-op, so this is bit-identical, not merely close.
    #[test]
    fn multiplier_one_is_byte_identical_to_the_bare_qty_times_price() {
        for (qty, px, cap) in [
            (0.01, 30_000.0, 100.0), // 300 > 100 → reject
            (0.01, 30_000.0, 500.0), // 300 < 500 → accept
            (0.01, 30_000.0, 300.0), // exactly at the cap → accept (strict >)
            (3.0, 0.045, 1.0),       // the options-ticket shape
            (1.0, f64::MAX, 1.0),    // saturating
        ] {
            let limits = OrderLimits { max_notional: cap, ..OrderLimits::default() };
            let req =
                build_order_request(&OrderTicket::limit("binance", "S", 1, qty, px), "c".into());
            let bare = qty * px;
            let expected = if bare > cap {
                Err(OrderReject::NotionalAboveMax { notional: bare, max: cap })
            } else {
                Ok(())
            };
            assert_eq!(validate(&req, &limits), expected, "qty={qty} px={px} cap={cap}");
            // the explicit-1.0 entry point agrees with the defaulting one
            assert_eq!(validate_with_multiplier(&req, &limits, 1.0), expected);
        }
    }

    /// THE BUG: an option with a 100x contract multiplier. `qty × price` measures 300 and slips
    /// under a 1,000 cap, but the order's real notional — what `RiskGate` and `SimBroker` measure —
    /// is 30,000. The cap must now see it.
    #[test]
    fn multiplier_gt_one_is_measured_and_blocked() {
        let limits = OrderLimits { max_notional: 1_000.0, ..OrderLimits::default() };
        let req = build_order_request(
            &OrderTicket::limit("deribit", "BTC-28MAR25-100000-C", 1, 2.0, 150.0),
            "opt-1".into(),
        );
        // Pre-fix behaviour, pinned as the thing that was WRONG: 2 × 150 = 300 < 1,000 → passed.
        assert_eq!(validate(&req, &limits), Ok(()));
        // With the real 100x multiplier: 2 × 150 × 100 = 30,000 > 1,000 → blocked.
        match validate_with_multiplier(&req, &limits, 100.0) {
            Err(OrderReject::NotionalAboveMax { notional, max }) => {
                assert_eq!(notional, 30_000.0);
                assert_eq!(max, 1_000.0);
            }
            other => panic!("multiplier-inclusive cap must block the option: {other:?}"),
        }
        // and it still ACCEPTS when the multiplier-inclusive notional genuinely fits.
        let roomy = OrderLimits { max_notional: 50_000.0, ..OrderLimits::default() };
        assert_eq!(validate_with_multiplier(&req, &roomy, 100.0), Ok(()));
    }

    /// A multiplier BELOW 1 (a fractional contract size) shrinks the notional, so an order the
    /// bare `qty × price` would have rejected is correctly admitted.
    #[test]
    fn multiplier_lt_one_shrinks_the_notional() {
        let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
        let req = build_order_request(&OrderTicket::limit("okx", "X", 1, 1.0, 500.0), "c".into());
        // bare: 1 × 500 = 500 > 100 → rejected
        assert!(matches!(validate(&req, &limits), Err(OrderReject::NotionalAboveMax { .. })));
        // with a 0.1 contract size: 1 × 500 × 0.1 = 50 < 100 → admitted
        assert_eq!(validate_with_multiplier(&req, &limits, 0.1), Ok(()));
    }

    /// Notional is a MAGNITUDE: a negative multiplier enters absolute, so it can never make the
    /// cap un-trippable (the sign-flip hole `RiskGate`'s `.abs()` closes for the same reason).
    #[test]
    fn negative_multiplier_enters_absolute() {
        let limits = OrderLimits { max_notional: 1_000.0, ..OrderLimits::default() };
        let req =
            build_order_request(&OrderTicket::limit("deribit", "OPT", 1, 2.0, 150.0), "c".into());
        let neg = validate_with_multiplier(&req, &limits, -100.0);
        assert_eq!(neg, validate_with_multiplier(&req, &limits, 100.0));
        assert!(matches!(neg, Err(OrderReject::NotionalAboveMax { .. })));
    }

    /// A NaN/infinite multiplier must REJECT, not propagate: NaN makes every `>` comparison false,
    /// which would silently disable the cap entirely.
    #[test]
    fn non_finite_multiplier_is_rejected_not_propagated() {
        let limits = OrderLimits { max_notional: 100.0, ..OrderLimits::default() };
        let req =
            build_order_request(&OrderTicket::limit("binance", "S", 1, 1.0, 50.0), "c".into());
        for m in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                validate_with_multiplier(&req, &limits, m),
                Err(OrderReject::NonFiniteQtyOrPrice),
                "multiplier {m} must reject"
            );
        }
    }

    /// The POLICY ceiling reaching the preview caps — the "value flows" half of Phase 5's
    /// contract, at this end of the wire. A PURE function of the caller-supplied value, so this
    /// test never touches process-global env (`set_var` is unsound from parallel test threads,
    /// and the version of this test that read `VIKE_MAX_ORDER_NOTIONAL` serialized itself by hand
    /// to work around exactly that).
    #[test]
    fn the_policy_ceiling_becomes_the_preview_notional_cap() {
        assert_eq!(OrderLimits::with_max_notional(Some(1234.5)).max_notional, 1234.5);
    }

    /// No `policy.toml` (or no `max_notional_per_order` key) ⇒ TODAY'S DEFAULT: permissive.
    /// A nonsense ceiling is ignored the same way — `0.0` would deny every order, a silent halt.
    #[test]
    fn an_absent_or_nonsense_ceiling_leaves_the_permissive_default() {
        for v in [None, Some(0.0), Some(-1.0), Some(f64::INFINITY), Some(f64::NAN)] {
            assert_eq!(
                OrderLimits::with_max_notional(v).max_notional,
                f64::INFINITY,
                "ceiling {v:?} must not tighten the cap"
            );
        }
    }

    #[test]
    fn a_ceiling_moves_nothing_else_about_the_permissive_default() {
        let l = OrderLimits::with_max_notional(Some(10.0));
        assert_eq!(l.min_qty, 0.0);
        assert_eq!(l.max_qty, f64::INFINITY);
        assert!(l.require_price_for_limit);
    }
}
