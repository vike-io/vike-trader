//! `ProfileRisk` / `GridSource` / `ProfileError` — the TOML `[risk]` → [`RiskLimits`] converter,
//! MOVED here from `vike-core::run_profile` (runprofile-wiring-step2) so `vike-backtest` — which
//! sits ALONGSIDE `vike-core`, not beneath it, and therefore cannot depend on it — can share the
//! ONE converter paper and live already use, rather than growing a second one that drifts.
//! `vike-core::run_profile` re-exports every name here verbatim, so every existing caller keeps
//! working unchanged; this is the same hoist shape [`vike_model::sizing`] established for
//! `units_from_percent`/`units_from_value` (moved down so `vike-core` could share one sizing law
//! with the backtest engines without a downward dependency on `vike-backtest`).
//!
//! ## Two owners, one struct: the venue's instrument grid vs the operator's risk budget
//!
//! [`RiskLimits`] holds two conceptually separate concerns in one struct:
//! `tick_size`/`lot_size`/`min_qty`/`min_notional` (the **venue**'s instrument grid, populated by
//! `RiskLimits::from_properties` from a real fetch at mount) and everything else —
//! `max_notional_per_order`/`max_total_exposure`/`max_orders_per_window`/`window_ms`/
//! `max_leverage`/`im_requirement`/`im_by_symbol`/`required_free_bp_pct`/
//! `block_reduce_only_overshoot` (the **operator**'s risk budget, which a `[risk]` TOML section
//! sets). **THE RULE: the venue owns the instrument grid; the operator owns the risk budget.
//! Neither overwrites the other.**
//!
//! This is not a vike invention — it mirrors how two other engines keep the same two concerns
//! apart:
//!   - **NautilusTrader's `RiskEngine`** checks instrument-level limits (`min_quantity`/
//!     `max_quantity`/`max_notional` off the instrument) AND engine-level limits
//!     (`max_notional_per_order` from `RiskEngineConfig`) as SEPARATE checks in one pass — an
//!     order must clear both, and neither overwrites the other; `set_max_notional_per_order` is a
//!     runtime override on the config side only.
//!   - **LEAN** keeps three distinct layers: `SymbolProperties` (`LotSize`/`MinimumOrderSize` —
//!     rounds and rejects), `BrokerageModel.CanSubmitOrder` (the venue veto), and
//!     `RiskManagementModel.ManageRisk` (operator policy over `PortfolioTarget`s). Separation of
//!     concerns between the instrument grid and operator policy is a stated design principle
//!     there, not an implementation accident.
//!
//! We only face a "who wins" question at all because [`RiskLimits`] is ONE struct holding both
//! concerns — a vike quirk, not a law. [`ProfileRisk::apply_to`] is the enforcement point: a
//! profile that sets a venue-owned field while a real grid was fetched is a **config error at
//! load** (`Err` naming the offending key) — never a silent clamp, because silently clamping is
//! exactly how an operator sets a limit that never takes effect and never learns. The one
//! exception — a profile MAY supply the instrument fields when no grid was fetched at all (the
//! `make_engine` permissive-default fallback after a fetch failure, or a backtest/paper mount,
//! which never fetches one) — is gated on the caller passing [`GridSource::NoGridFetched`], never
//! inferred from a field happening to be `None`.
//!
//! `vike-core::RunProfile::grid_source` derives the [`GridSource`] straight from its own `mode`
//! (`backtest`/`paper` ⇒ `NoGridFetched`, `live` ⇒ `VenueFetched`) and unconditionally rejects, at
//! load, a `live` profile whose `[risk]` sets ANY venue-owned instrument field — see that module
//! for the full derivation and its rationale. `vike-backtest`'s `BacktestProfile` uses the SAME
//! [`ProfileRisk`], always merged as [`GridSource::NoGridFetched`] (a backtest never fetches a
//! venue grid), but with its OWN mode-specific rejection: `max_orders_per_window` is a wall-clock
//! throttle that is meaningless in sim time (`SimBroker::build_risk_gate` always disarms it), so a
//! backtest profile that sets it is rejected at load rather than silently ignored.
//!
//! ## One leverage concept, one name: `max_leverage` (issue #822)
//!
//! [`RiskLimits`] used to carry TWO names for the same quantity — `max_leverage`, which
//! `RiskGate::check` never evaluated, and `im_requirement` (LEAN `1/leverage`), which did the
//! actual pre-trade buying-power work. The better-named one was the dead one. `[risk]` now exposes
//! **`max_leverage` only**; [`ProfileRisk::im_requirement`] converts it (`im = 1.0 / max_leverage`)
//! at THIS config edge, so the enforced storage field keeps the serde name it has always had —
//! renaming it would change [`crate::engine_snapshot::state_hash`] and break journal replay.
//!
//! `[risk] im_requirement` is GONE from the TOML surface. Because [`ProfileRisk`] is
//! `deny_unknown_fields`, a profile still setting it fails LOUDLY at load naming the key, rather
//! than being silently ignored — the same policy a typo'd key gets.
//!
//! The `im = 1/leverage` identity is not new here — `vike_backtest::SimBroker::build_risk_gate`
//! already maps its own operator-facing `EngineParams::leverage` onto `RiskLimits::im_requirement`
//! exactly this way (see that fn's doc for the algebraic equivalence proof against the retired
//! leverage-room formula). This makes the profile edge agree with the backtest edge, so "10x" means
//! the same thing in both — one more instance of the one-config-across-backtest/paper/live rule #816
//! established.
//!
//! Both competitor engines carry one leverage concept, not two: **NautilusTrader** puts it on
//! `MarginAccount` (`set_default_leverage`/`set_leverage`) feeding `margin_init`/`margin_maint` —
//! account state driving margin math, the role our `im_requirement` plays, and NOT a second knob in
//! `RiskEngineConfig`. **Hummingbot**'s `leverage` is a perpetual strategy parameter sent TO the
//! exchange as position leverage — a venue setting, not a local pre-trade gate. Neither carries a
//! "max leverage" pre-trade check alongside a margin fraction.

use serde::Deserialize;
use std::fmt;

use crate::RiskLimits;

/// `[risk]` — the pre-trade gate limits. A field-for-field mirror of [`RiskLimits`] (kept as a
/// mirror, not the type itself, so unknown-key denial applies inside `[risk]` too);
/// [`ProfileRisk::to_risk_limits`] is the compile-checked conversion.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProfileRisk {
    pub tick_size: Option<f64>,
    pub lot_size: Option<f64>,
    pub min_notional: Option<f64>,
    /// per-order minimum quantity floor (venue lot-size grid); checked on the lot-rounded qty.
    pub min_qty: Option<f64>,
    pub max_notional_per_order: Option<f64>,
    /// `[risk] max_total_exposure` — cap on **ONE SYMBOL's** projected open notional at **ONE
    /// VENUE**, in the account's quote currency. Despite the name it is NOT an account-wide,
    /// cross-symbol or cross-venue total: it lands verbatim in
    /// [`RiskLimits::max_total_exposure`], whose doc is the authority on the scope, on why the
    /// name is kept, and on the test that pins it. Size it for a SINGLE instrument — an operator
    /// who sizes it for a whole book of N symbols gets an N× weaker cap than they wrote.
    pub max_total_exposure: Option<f64>,
    pub max_orders_per_window: Option<usize>,
    /// throttle window (ms); only active when `max_orders_per_window` is set
    #[serde(default)]
    pub window_ms: i64,
    /// THE leverage knob (>= 1.0), e.g. `10.0` ⇒ 10x. Arms the pre-trade buying-power check by
    /// converting to [`RiskLimits::im_requirement`] (`1.0 / max_leverage`) — see
    /// [`ProfileRisk::im_requirement`] and the module doc's "One leverage concept, one name".
    /// Omitted ⇒ the buying-power lane stays off here (a live mount then applies its own
    /// conservative 1× rescue).
    pub max_leverage: Option<f64>,
    #[serde(default)]
    pub block_reduce_only_overshoot: bool,
    /// LEAN free-buying-power haircut on equity, `[0.0, 1.0)`
    #[serde(default)]
    pub required_free_bp_pct: f64,
}

impl ProfileRisk {
    /// The initial-margin fraction [`RiskLimits::im_requirement`] stores, derived from this
    /// profile's operator-facing [`ProfileRisk::max_leverage`]: `im = 1.0 / max_leverage`
    /// (`10.0` ⇒ `0.1`). `None` ⇒ the buying-power check stays disarmed, exactly as an unset
    /// `im_requirement` did before this conversion existed.
    ///
    /// GUARD: a non-finite or `< 1.0` leverage is nonsense (`0.0` would divide by zero, a negative
    /// would produce a NEGATIVE margin requirement — i.e. unbounded buying power, the one failure
    /// mode that must never happen silently). Such a value is rejected at load by
    /// `vike_core::RunProfile::validate` AND by [`ProfileRisk::apply_to`]; here — on the two
    /// INFALLIBLE paths that a caller can reach with a hand-built `ProfileRisk` — it degrades to
    /// the most conservative reading, `Some(1.0)` (1×, no leverage), never to `None` (gate off)
    /// and never to a negative fraction.
    pub fn im_requirement(&self) -> Option<f64> {
        match self.max_leverage {
            Some(lev) if lev.is_finite() && lev >= 1.0 => Some(1.0 / lev),
            Some(_) => Some(1.0),
            None => None,
        }
    }

    /// Build the real [`RiskLimits`]. Constructed field-by-field (NO `..Default`) so a
    /// new `RiskLimits` field is a compile error here — the intended drift alarm.
    pub fn to_risk_limits(&self) -> RiskLimits {
        RiskLimits {
            tick_size: self.tick_size,
            lot_size: self.lot_size,
            min_notional: self.min_notional,
            min_qty: self.min_qty,
            max_notional_per_order: self.max_notional_per_order,
            max_total_exposure: self.max_total_exposure,
            max_orders_per_window: self.max_orders_per_window,
            window_ms: self.window_ms,
            max_leverage: self.max_leverage,
            block_reduce_only_overshoot: self.block_reduce_only_overshoot,
            im_requirement: self.im_requirement(),
            im_by_symbol: Default::default(),
            required_free_bp_pct: self.required_free_bp_pct,
            // Pre-trade impact knobs stay OFF here (the drift alarm fired as designed). They are
            // only consulted by `RiskGate::check_with_book`, and the runtime does not yet hand
            // the gate a book — surfacing them in `[risk]` TOML is a follow-up that belongs with
            // that wiring, not before it (an inert profile knob would mislead).
            max_slippage_bps: None,
            require_fillable: false,
            // The fat-finger price collar stays OFF here too (the drift alarm fired as designed).
            // Surfacing it in `[risk]` TOML is a follow-up that belongs with the operator-facing
            // wiring (a per-symbol band table), not ahead of it — and an inert profile knob would
            // mislead exactly the way the impact knobs above would.
            price_collar: None,
            collar_by_symbol: Default::default(),
            grid_by_symbol: Default::default(),
        }
    }

    /// The `risk.*` keys naming the venue-owned instrument fields THIS profile sets — an explicit
    /// list (not spelled inline at the one call site in [`ProfileRisk::apply_to`]) so the split
    /// from the operator-owned fields in [`ProfileRisk::to_risk_limits`] is visible as data, not
    /// buried in branch logic. `pub` (not crate-private): `vike-core::RunProfile::validate`'s
    /// unconditional `mode = "live"` structural gate calls this too, from the other crate.
    pub fn venue_owned_fields_set(&self) -> Vec<&'static str> {
        [
            ("risk.tick_size", self.tick_size.is_some()),
            ("risk.lot_size", self.lot_size.is_some()),
            ("risk.min_qty", self.min_qty.is_some()),
            ("risk.min_notional", self.min_notional.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
    }

    /// Merge this profile's OPERATOR-owned risk budget onto `base` — a [`RiskLimits`]
    /// already built by the mount (typically `RiskLimits::from_properties`, or the
    /// permissive `RiskLimits::new` default after a failed fetch) — without letting
    /// either side silently overwrite the other's concern. See the module doc's "Two owners, one
    /// struct" section for the rule and its NautilusTrader/LEAN precedent.
    ///
    /// Behavior, by field group:
    /// - **Venue-owned** (`tick_size`/`lot_size`/`min_qty`/`min_notional`): when `grid` is
    ///   [`GridSource::VenueFetched`], this profile setting ANY of them is a config error — `Err`
    ///   naming every offending key, never a silent clamp — and the venue fields in a successful
    ///   result are `base`'s, untouched. When `grid` is [`GridSource::NoGridFetched`] (the one
    ///   exception, explicitly flagged by the caller), the profile's own instrument fields become
    ///   the result's venue fields instead — the sole source of truth when no grid exists to
    ///   defer to.
    /// - **Operator-owned** (everything [`ProfileRisk::to_risk_limits`] also sets from this
    ///   profile: `max_notional_per_order`/`max_total_exposure`/`max_orders_per_window`/
    ///   `window_ms`/`max_leverage`/`required_free_bp_pct`/`block_reduce_only_overshoot`, plus the
    ///   `im_requirement` DERIVED from `max_leverage`): always taken from this profile, regardless
    ///   of `grid`.
    /// - **Neither side's concern** (`im_by_symbol` — a live `Command::SetMargin` override, not a
    ///   profile field; the book-impact knobs `max_slippage_bps`/`require_fillable`; the price
    ///   collar `price_collar`/`collar_by_symbol`): carried over from `base` untouched. This
    ///   function only ever narrows what it touches to the two owned field groups above.
    pub fn apply_to(&self, base: RiskLimits, grid: GridSource) -> Result<RiskLimits, ProfileError> {
        if grid == GridSource::VenueFetched {
            let offending = self.venue_owned_fields_set();
            if !offending.is_empty() {
                return Err(ProfileError::Validation(format!(
                    "profile sets venue-owned instrument field(s) [{}] but a real venue grid was \
                     fetched at mount — the venue owns tick_size/lot_size/min_qty/min_notional, \
                     the operator owns the risk budget, and neither may silently overwrite the \
                     other. Remove {} from `[risk]`, or mount with no fetched grid if you meant \
                     the profile to be authoritative for the instrument fields.",
                    offending.join(", "),
                    if offending.len() == 1 { "it" } else { "them" }
                )));
            }
        }

        // `window_ms` is only MEANINGFUL when `max_orders_per_window` is armed (see the field
        // doc) — so it is only taken from THIS profile when this profile actually sets the
        // throttle. A profile that never mentions `max_orders_per_window` carries `base`'s own
        // `window_ms` through untouched, same as any other field neither side asked to change.
        //
        // This is the fix for the divergent-default class BLOCKING-1 caught: `window_ms` is
        // `#[serde(default)]` on an `i64`, so an unset TOML key parses to `0`; taking it
        // unconditionally from `self` would silently zero `base`'s real window (`RiskLimits::new`'s
        // 1000) the moment ANY profile — even one that only sets, say, `max_notional_per_order` —
        // is merged in, making the throttle's cutoff == now and evicting every stamp: a rate limit
        // that looks armed (`max_orders_per_window` populated) but never trips. Requiring a
        // positive `window_ms` alongside `max_orders_per_window` (mirroring
        // `RunProfile::validate`'s load-time check, which this closes the gap around — that check
        // lives in a different crate and is skippable by constructing a bare `ProfileRisk`
        // directly) makes the same invariant hold no matter how a caller reaches `apply_to`.
        let window_ms = match self.max_orders_per_window {
            Some(n) => {
                if self.window_ms <= 0 {
                    return Err(ProfileError::Validation(format!(
                        "risk.max_orders_per_window = {n} is set but risk.window_ms = {} — the \
                         sliding-window throttle needs a positive window (ms) to have any cutoff; \
                         set risk.window_ms > 0 alongside max_orders_per_window (or omit both to \
                         leave the throttle disabled)",
                        self.window_ms
                    )));
                }
                self.window_ms
            }
            None => base.window_ms,
        };

        // `max_leverage` is now ENFORCED (it converts to the `im_requirement` the buying-power
        // check reads — see the module doc), so a nonsensical value can no longer be shrugged off
        // as inert the way the dead knob's could. `0.0` divides by zero and a negative would yield
        // a NEGATIVE margin requirement, i.e. unbounded buying power. Rejected here as well as at
        // load, for the same reason `window_ms` is: `RunProfile::validate` lives in another crate
        // and is skippable by constructing a bare `ProfileRisk` and calling this directly.
        if let Some(lev) = self.max_leverage
            && (!lev.is_finite() || lev < 1.0)
        {
            return Err(ProfileError::Validation(format!(
                "risk.max_leverage = {lev} is not a usable leverage — it must be finite and \
                     >= 1.0 (1.0 = no leverage, 10.0 = 10x). It arms the pre-trade buying-power \
                     check as an initial-margin requirement of 1/max_leverage, so a zero or \
                     negative value would mean infinite or negative buying power; omit the key to \
                     leave the check disarmed"
            )));
        }

        let (tick_size, lot_size, min_qty, min_notional) = match grid {
            GridSource::VenueFetched => {
                (base.tick_size, base.lot_size, base.min_qty, base.min_notional)
            }
            GridSource::NoGridFetched => {
                (self.tick_size, self.lot_size, self.min_qty, self.min_notional)
            }
        };

        // Constructed field-by-field (NO `..base`/`..Default`), same as `to_risk_limits` above and
        // for the same reason: a new `RiskLimits` field is a compile error HERE too, so this
        // function stays a second, independent drift alarm rather than a silent pass-through.
        Ok(RiskLimits {
            tick_size,
            lot_size,
            min_notional,
            min_qty,
            max_notional_per_order: self.max_notional_per_order,
            max_total_exposure: self.max_total_exposure,
            max_orders_per_window: self.max_orders_per_window,
            window_ms,
            max_leverage: self.max_leverage,
            block_reduce_only_overshoot: self.block_reduce_only_overshoot,
            im_requirement: self.im_requirement(),
            im_by_symbol: base.im_by_symbol,
            required_free_bp_pct: self.required_free_bp_pct,
            max_slippage_bps: base.max_slippage_bps,
            require_fillable: base.require_fillable,
            price_collar: base.price_collar,
            collar_by_symbol: base.collar_by_symbol,
            grid_by_symbol: base.grid_by_symbol,
        })
    }

    /// Merge ONLY this profile's OPERATOR-owned fields onto `base`, ignoring — never rejecting —
    /// any venue-owned instrument field this profile might also (illegally) set; `base`'s own
    /// `tick_size`/`lot_size`/`min_qty`/`min_notional` are always kept. Infallible, unlike
    /// [`ProfileRisk::apply_to`].
    ///
    /// This is the fallback `vike-mount`'s live merge site uses when `apply_to` rejects a profile
    /// under [`GridSource::VenueFetched`]: an `Err` from `apply_to` must never mean the operator's
    /// ENTIRE risk budget silently mounts unarmed (`max_notional_per_order`/`max_total_exposure`/
    /// `max_orders_per_window`/`window_ms`/`max_leverage` (and the `im_requirement` derived from
    /// it)/`required_free_bp_pct` all still fail to `None`) just because the SAME profile also set
    /// one venue-owned field it should not have — only the offending venue field(s) are dropped,
    /// not the whole budget.
    pub fn apply_operator_budget_only(&self, base: RiskLimits) -> RiskLimits {
        let window_ms = match self.max_orders_per_window {
            Some(_) if self.window_ms > 0 => self.window_ms,
            _ => base.window_ms,
        };
        RiskLimits {
            tick_size: base.tick_size,
            lot_size: base.lot_size,
            min_notional: base.min_notional,
            min_qty: base.min_qty,
            max_notional_per_order: self.max_notional_per_order,
            max_total_exposure: self.max_total_exposure,
            // A throttle whose window would still be non-positive after the fallback is a
            // config error `apply_to` already would have caught on a clean profile, but this path
            // is reached ONLY when `apply_to` already errored on an UNRELATED (venue-owned) field
            // — so here we degrade safely instead of raising a second error: an inconsistent
            // throttle request is disarmed (`None`) rather than left to silently misbehave.
            max_orders_per_window: if window_ms > 0 { self.max_orders_per_window } else { None },
            window_ms,
            max_leverage: self.max_leverage,
            block_reduce_only_overshoot: self.block_reduce_only_overshoot,
            im_requirement: self.im_requirement(),
            im_by_symbol: base.im_by_symbol,
            required_free_bp_pct: self.required_free_bp_pct,
            max_slippage_bps: base.max_slippage_bps,
            require_fillable: base.require_fillable,
            price_collar: base.price_collar,
            collar_by_symbol: base.collar_by_symbol,
            grid_by_symbol: base.grid_by_symbol,
        }
    }
}

/// Where the venue-owned portion of the `base` [`RiskLimits`] passed to
/// [`ProfileRisk::apply_to`] came from — the EXPLICIT flag the module doc's exception case is
/// gated on. Never infer this from "the field happened to be `None`": a fetch that legitimately
/// returned zeroed/absent tick fields (folded to `None` by `nz_step`) must still be treated as
/// [`GridSource::VenueFetched`], not silently reopened for the profile to fill in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridSource {
    /// `base` was built by `RiskLimits::from_properties` from a REAL venue-fetched
    /// instrument grid. The venue owns `tick_size`/`lot_size`/`min_qty`/`min_notional`; a profile
    /// that also sets one of them is a config error (see [`ProfileRisk::apply_to`]).
    VenueFetched,
    /// No grid was fetched for this mount (`make_engine`'s permissive-default fallback after a
    /// failed fetch, or no fetch attempted at all — every backtest/paper mount). This is the ONLY
    /// case where the profile may supply the instrument-grid fields itself — it is then the sole
    /// source of truth for them.
    NoGridFetched,
}

/// A run-profile loader failure — distinct kinds so a caller can tell I/O from a bad key from a
/// bad value. Shared by every profile loader in the workspace (`vike-core::RunProfile`,
/// `vike-backtest::BacktestProfile` reuses its own `HarnessError` instead, since it predates this
/// hoist and already has a wider Data variant) — this crate only needs it as
/// [`ProfileRisk::apply_to`]'s error type, but keeping ONE type (rather than a narrower
/// risk-only enum) means `vike-core::run_profile`'s re-export is a byte-identical no-op for
/// every existing caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// reading the file failed
    Io(String),
    /// TOML syntax / type / unknown-field error (the DENY policy surfaces here)
    Parse(String),
    /// syntactically valid but semantically nonsensical (e.g. live mode with a paper broker)
    Validation(String),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::Io(m) => write!(f, "run profile I/O error: {m}"),
            ProfileError::Parse(m) => write!(f, "run profile parse error: {m}"),
            ProfileError::Validation(m) => write!(f, "run profile validation error: {m}"),
        }
    }
}

impl std::error::Error for ProfileError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `RiskLimits` shaped like a real `from_properties` fetch: a populated instrument grid,
    /// operator budget still at the `new()`/`Default` off-state.
    fn venue_fetched_base() -> RiskLimits {
        RiskLimits {
            tick_size: Some(0.5),
            lot_size: Some(0.01),
            min_qty: Some(0.01),
            min_notional: Some(10.0),
            ..RiskLimits::new()
        }
    }

    /// A profile with ONLY operator-budget fields set (no venue-owned instrument fields).
    fn operator_only_profile() -> ProfileRisk {
        ProfileRisk {
            max_notional_per_order: Some(25_000.0),
            max_total_exposure: Some(100_000.0),
            max_orders_per_window: Some(5),
            window_ms: 2000,
            max_leverage: Some(4.0),
            block_reduce_only_overshoot: true,
            required_free_bp_pct: 0.1,
            ..ProfileRisk::default()
        }
    }

    #[test]
    fn apply_to_operator_only_leaves_venue_fields_byte_identical() {
        let base = venue_fetched_base();
        let profile = operator_only_profile();

        let got = profile
            .apply_to(base.clone(), GridSource::VenueFetched)
            .expect("no venue field set -> Ok");

        assert_eq!(got.tick_size, base.tick_size);
        assert_eq!(got.lot_size, base.lot_size);
        assert_eq!(got.min_qty, base.min_qty);
        assert_eq!(got.min_notional, base.min_notional);
        assert_eq!(got.max_notional_per_order, Some(25_000.0));
        assert_eq!(got.max_total_exposure, Some(100_000.0));
        assert_eq!(got.max_orders_per_window, Some(5));
        assert_eq!(got.window_ms, 2000);
        assert_eq!(got.max_leverage, Some(4.0));
        assert!(got.block_reduce_only_overshoot);
        assert_eq!(got.im_requirement, Some(0.25), "4x leverage ⇒ 25% initial margin");
        assert_eq!(got.required_free_bp_pct, 0.1);
    }

    #[test]
    fn apply_to_each_venue_owned_field_is_config_error_when_grid_fetched() {
        let cases: &[(&str, ProfileRisk)] = &[
            ("risk.tick_size", ProfileRisk { tick_size: Some(1.0), ..ProfileRisk::default() }),
            ("risk.lot_size", ProfileRisk { lot_size: Some(1.0), ..ProfileRisk::default() }),
            ("risk.min_qty", ProfileRisk { min_qty: Some(1.0), ..ProfileRisk::default() }),
            (
                "risk.min_notional",
                ProfileRisk { min_notional: Some(1.0), ..ProfileRisk::default() },
            ),
        ];
        for (key, profile) in cases {
            let err = profile
                .apply_to(venue_fetched_base(), GridSource::VenueFetched)
                .expect_err(&format!("{key} set alongside a fetched grid must be Err"));
            match err {
                ProfileError::Validation(m) => {
                    assert!(m.contains(key), "error must name `{key}`: {m}")
                }
                other => panic!("expected Validation error for {key}, got {other:?}"),
            }
        }
    }

    #[test]
    fn apply_to_names_every_offending_field_in_one_error() {
        let profile =
            ProfileRisk { tick_size: Some(1.0), min_qty: Some(1.0), ..ProfileRisk::default() };
        let err = profile
            .apply_to(venue_fetched_base(), GridSource::VenueFetched)
            .expect_err("multiple venue fields set must still be Err");
        let ProfileError::Validation(m) = err else { panic!("expected Validation error") };
        assert!(m.contains("risk.tick_size"), "message: {m}");
        assert!(m.contains("risk.min_qty"), "message: {m}");
        assert!(!m.contains("risk.lot_size"), "message must not name an unset field: {m}");
    }

    #[test]
    fn apply_to_no_grid_fetched_profile_supplies_instrument_fields() {
        let profile = ProfileRisk {
            tick_size: Some(0.25),
            lot_size: Some(0.5),
            min_qty: Some(0.75),
            min_notional: Some(2.5),
            ..ProfileRisk::default()
        };
        let got = profile
            .apply_to(RiskLimits::new(), GridSource::NoGridFetched)
            .expect("no venue grid was fetched -> profile instrument fields are allowed");
        assert_eq!(got.tick_size, Some(0.25));
        assert_eq!(got.lot_size, Some(0.5));
        assert_eq!(got.min_qty, Some(0.75));
        assert_eq!(got.min_notional, Some(2.5));
    }

    #[test]
    fn apply_to_no_grid_fetched_ignores_bases_venue_fields() {
        let profile = ProfileRisk::default(); // no instrument fields set
        let got = profile
            .apply_to(venue_fetched_base(), GridSource::NoGridFetched)
            .expect("empty profile risk is always Ok");
        assert_eq!(
            got.tick_size, None,
            "NoGridFetched must use the profile's None, not base's Some"
        );
        assert_eq!(got.lot_size, None);
        assert_eq!(got.min_qty, None);
        assert_eq!(got.min_notional, None);
    }

    #[test]
    fn apply_to_matches_to_risk_limits_when_no_grid_fetched() {
        let p = ProfileRisk {
            max_notional_per_order: Some(50_000.0),
            max_total_exposure: Some(200_000.0),
            max_orders_per_window: Some(20),
            window_ms: 1000,
            max_leverage: Some(5.0),
            block_reduce_only_overshoot: true,
            required_free_bp_pct: 0.05,
            ..ProfileRisk::default()
        };
        let via_apply = p.apply_to(RiskLimits::new(), GridSource::NoGridFetched).unwrap();
        let via_to_risk_limits = p.to_risk_limits();
        assert_eq!(via_apply, via_to_risk_limits);
    }

    #[test]
    fn apply_to_preserves_fields_neither_side_owns() {
        use crate::PriceCollar;
        let mut base = venue_fetched_base();
        base.im_by_symbol.insert("BTCUSDT".to_string(), 0.05);
        base.max_slippage_bps = Some(12.0);
        base.require_fillable = true;
        base.price_collar = Some(PriceCollar { pct: 0.1, abs_floor: 0.01 });
        base.collar_by_symbol
            .insert("ETHUSDT".to_string(), PriceCollar { pct: 0.2, abs_floor: 0.02 });

        let got = operator_only_profile().apply_to(base.clone(), GridSource::VenueFetched).unwrap();

        assert_eq!(got.im_by_symbol, base.im_by_symbol);
        assert_eq!(got.max_slippage_bps, base.max_slippage_bps);
        assert_eq!(got.require_fillable, base.require_fillable);
        assert_eq!(got.price_collar, base.price_collar);
        assert_eq!(got.collar_by_symbol, base.collar_by_symbol);
    }

    // ---------------------------------------------------------------------------------------
    // BLOCKING-1 regression: `window_ms`'s serde default (0) must never silently clobber a real
    // `window_ms` (e.g. `RiskLimits::new()`'s 1000) just because SOME profile was merged in.
    // ---------------------------------------------------------------------------------------

    /// A profile that arms `max_orders_per_window` but leaves `window_ms` at its serde default (0,
    /// since a TOML profile that never mentions the key parses to that) must be REJECTED, not
    /// silently merged with a zero window (cutoff == now ⇒ the throttle never trips even though
    /// `max_orders_per_window` reads as armed).
    #[test]
    fn apply_to_rejects_armed_throttle_with_non_positive_window_ms() {
        let profile = ProfileRisk { max_orders_per_window: Some(5), ..ProfileRisk::default() };
        let err = profile
            .apply_to(venue_fetched_base(), GridSource::VenueFetched)
            .expect_err("max_orders_per_window set with window_ms <= 0 must be Err");
        match err {
            ProfileError::Validation(m) => {
                assert!(m.contains("window_ms"), "error must name window_ms: {m}");
                assert!(m.contains("max_orders_per_window"), "error must name the throttle: {m}");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    /// A profile that never mentions `max_orders_per_window` at all must NOT touch `window_ms` —
    /// `base`'s own window (e.g. a real venue-fetched grid's `RiskLimits::new()`-derived 1000) is
    /// carried through untouched, exactly like any other field neither side asked to change.
    #[test]
    fn apply_to_inherits_bases_window_ms_when_throttle_is_untouched() {
        let profile = ProfileRisk::default();
        let got = profile
            .apply_to(venue_fetched_base(), GridSource::VenueFetched)
            .expect("no max_orders_per_window is always Ok");
        assert_eq!(
            got.window_ms, 1000,
            "must inherit base's window_ms (1000), not the serde default 0"
        );
    }

    /// A profile that DOES arm the throttle with a valid positive window still wins over `base`'s
    /// own window — the operator's explicit choice is respected, not silently ignored in favor of
    /// the inherited default.
    #[test]
    fn apply_to_uses_profiles_window_ms_when_throttle_is_armed() {
        let profile = ProfileRisk {
            max_orders_per_window: Some(3),
            window_ms: 250,
            ..ProfileRisk::default()
        };
        let got = profile
            .apply_to(venue_fetched_base(), GridSource::VenueFetched)
            .expect("a positive window_ms alongside max_orders_per_window is Ok");
        assert_eq!(got.window_ms, 250);
        assert_eq!(got.max_orders_per_window, Some(3));
    }

    // ---------------------------------------------------------------------------------------
    // Issue #822: `max_leverage` is the ONE operator-facing leverage name, and it is ENFORCED —
    // it converts to the `im_requirement` the buying-power check actually reads.
    // ---------------------------------------------------------------------------------------

    /// The conversion itself, across the whole plausible range, on all three converters — the
    /// point of the change: `max_leverage = N` must arm the buying-power check at `1/N`, not sit
    /// inert the way `RiskLimits::max_leverage` always did.
    #[test]
    fn max_leverage_converts_to_the_enforced_im_requirement() {
        for (lev, im) in [(1.0, 1.0), (2.0, 0.5), (4.0, 0.25), (10.0, 0.1), (100.0, 0.01)] {
            let p = ProfileRisk { max_leverage: Some(lev), ..ProfileRisk::default() };
            assert_eq!(p.im_requirement(), Some(im), "{lev}x ⇒ im {im}");
            assert_eq!(p.to_risk_limits().im_requirement, Some(im));
            assert_eq!(
                p.apply_to(RiskLimits::new(), GridSource::NoGridFetched).unwrap().im_requirement,
                Some(im)
            );
            assert_eq!(p.apply_operator_budget_only(RiskLimits::new()).im_requirement, Some(im));
        }
    }

    /// An UNSET `max_leverage` must leave the buying-power lane disarmed — byte-identical to the
    /// pre-#822 behavior of an unset `im_requirement`, and the precondition for `vike-mount`'s
    /// conservative 1× rescue (`.or(Some(1.0))`) still firing on a profile that never mentions
    /// leverage.
    #[test]
    fn absent_max_leverage_leaves_the_buying_power_check_disarmed() {
        let p = ProfileRisk::default();
        assert_eq!(p.im_requirement(), None);
        assert_eq!(p.to_risk_limits().im_requirement, None);
        assert_eq!(p.apply_operator_budget_only(RiskLimits::new()).im_requirement, None);
    }

    /// The guard. A leverage below 1.0 (or non-finite) is a config error `apply_to` REJECTS —
    /// `0.0` would divide by zero and a negative would produce a NEGATIVE margin requirement, i.e.
    /// unbounded buying power, which is the one direction this must never fail in.
    #[test]
    fn apply_to_rejects_a_max_leverage_below_one() {
        for lev in [0.0, 0.5, -2.0, f64::NAN, f64::INFINITY] {
            let p = ProfileRisk { max_leverage: Some(lev), ..ProfileRisk::default() };
            let err = p
                .apply_to(venue_fetched_base(), GridSource::VenueFetched)
                .expect_err(&format!("max_leverage = {lev} must be Err"));
            match err {
                ProfileError::Validation(m) => {
                    assert!(m.contains("risk.max_leverage"), "error must name the key: {m}")
                }
                other => panic!("expected Validation error for {lev}, got {other:?}"),
            }
        }
    }

    /// The infallible converters cannot return an error, so they must degrade CONSERVATIVELY on
    /// the same nonsense input: 1× (`im 1.0`), never `None` (gate silently off) and never a
    /// negative fraction (unbounded buying power).
    #[test]
    fn infallible_converters_degrade_a_bad_max_leverage_to_one_x() {
        for lev in [0.0, 0.5, -2.0, f64::NAN, f64::INFINITY] {
            let p = ProfileRisk { max_leverage: Some(lev), ..ProfileRisk::default() };
            assert_eq!(p.im_requirement(), Some(1.0), "{lev} must degrade to 1x, not disarm");
            assert_eq!(p.to_risk_limits().im_requirement, Some(1.0));
            assert_eq!(p.apply_operator_budget_only(RiskLimits::new()).im_requirement, Some(1.0));
        }
    }

    // NOTE: the twin property — `[risk] im_requirement` is now a LOUD `deny_unknown_fields` parse
    // error rather than a silently-ignored key — is pinned where the TOML actually gets parsed,
    // in `vike_core::run_profile`'s tests (`retired_im_requirement_key_is_rejected_by_name`).
    // This crate has no `toml` dependency and should not grow one for a single test.

    // ---------------------------------------------------------------------------------------
    // BLOCKING-2(b) regression: `apply_operator_budget_only` — the fallback merge `vike-mount`
    // uses when `apply_to` rejects a profile, so an illegal venue-owned field never costs the
    // operator their ENTIRE risk budget.
    // ---------------------------------------------------------------------------------------

    /// The operator-owned fields must ALL still arm even though this profile illegally also sets
    /// a venue-owned field (the exact shape that makes `apply_to` return `Err` under
    /// `VenueFetched`) — `apply_operator_budget_only` is the fallback that must never leave the
    /// budget at "zero caps".
    #[test]
    fn apply_operator_budget_only_arms_the_budget_despite_an_illegal_venue_field() {
        let profile = ProfileRisk {
            tick_size: Some(999.0), // illegal under VenueFetched — must be dropped, not honored
            max_notional_per_order: Some(100.0),
            max_total_exposure: Some(500.0),
            max_orders_per_window: Some(5),
            window_ms: 2000,
            max_leverage: Some(5.0),
            required_free_bp_pct: 0.1,
            ..ProfileRisk::default()
        };
        let base = venue_fetched_base();
        // Confirm this profile really would be rejected by `apply_to` first (the scenario this
        // fallback exists for).
        assert!(profile.apply_to(base.clone(), GridSource::VenueFetched).is_err());

        let got = profile.apply_operator_budget_only(base.clone());
        assert_eq!(got.tick_size, base.tick_size, "venue field must be base's, not the profile's");
        assert_eq!(got.max_notional_per_order, Some(100.0));
        assert_eq!(got.max_total_exposure, Some(500.0));
        assert_eq!(got.max_orders_per_window, Some(5));
        assert_eq!(got.window_ms, 2000);
        assert_eq!(got.max_leverage, Some(5.0));
        assert_eq!(got.im_requirement, Some(0.2), "5x leverage ⇒ 20% initial margin");
        assert_eq!(got.required_free_bp_pct, 0.1);
    }

    /// A profile with an invalid `window_ms` (the serde default 0) that also arms
    /// `max_orders_per_window` falls back to `base`'s window rather than zeroing it — and since
    /// `base` here has a REAL positive window (`venue_fetched_base`'s inherited 1000, same as any
    /// `RiskLimits::new()`/`from_properties` base ever is), arming with that inherited window is
    /// safe: it is exactly the same window `apply_to` would have used had the profile simply not
    /// mentioned `window_ms` at all.
    #[test]
    fn apply_operator_budget_only_falls_back_to_bases_window_when_profiles_own_is_invalid() {
        let profile = ProfileRisk {
            tick_size: Some(999.0),
            max_orders_per_window: Some(5),
            ..ProfileRisk::default() // window_ms stays the serde default (0)
        };
        let base = venue_fetched_base();
        let got = profile.apply_operator_budget_only(base.clone());
        assert_eq!(got.window_ms, base.window_ms, "must inherit base's real window, never 0");
        assert_eq!(
            got.max_orders_per_window,
            Some(5),
            "arming with the inherited (valid, positive) window is safe"
        );
    }

    /// The degenerate case the guard above exists for: if `base` ITSELF carries a non-positive
    /// window (never true of a real `RiskLimits::new()`/`from_properties` base, but not something
    /// this infallible fallback can rule out), falling back to it would reproduce the exact
    /// BLOCKING-1 hazard (armed count, zero-width window, cutoff == now). The fallback must
    /// disarm the throttle instead of silently merging a zero window.
    #[test]
    fn apply_operator_budget_only_disarms_throttle_if_even_bases_window_is_non_positive() {
        let profile = ProfileRisk {
            tick_size: Some(999.0),
            max_orders_per_window: Some(5),
            ..ProfileRisk::default() // window_ms stays the serde default (0)
        };
        let base = RiskLimits { tick_size: Some(0.5), window_ms: 0, ..RiskLimits::default() };
        let got = profile.apply_operator_budget_only(base);
        assert_eq!(got.window_ms, 0);
        assert_eq!(
            got.max_orders_per_window, None,
            "an inconsistent throttle request must disarm, not silently merge a zero window"
        );
    }
}
