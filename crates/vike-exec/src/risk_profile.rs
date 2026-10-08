//! `ProfileRisk` / `GridSource` / `ProfileError` — the TOML `[risk]` → [`RiskLimits`] converter.
//! It lives here, not in `vike-core::run_profile` (which re-exports every name verbatim), so
//! `vike-backtest` — which sits ALONGSIDE `vike-core` and cannot depend on it — shares the ONE
//! converter paper and live use rather than growing a second one that drifts.
//!
//! ## Two owners, one struct: the venue's instrument grid vs the operator's risk budget
//!
//! [`RiskLimits`] holds two separate concerns in one struct: `tick_size`/`lot_size`/`min_qty`/
//! `min_notional` (the **venue**'s instrument grid, populated by `RiskLimits::from_properties`
//! from a real fetch at mount) and everything else (the **operator**'s risk budget, which a
//! `[risk]` TOML section sets). **THE RULE: the venue owns the instrument grid; the operator owns
//! the risk budget. Neither overwrites the other.** NautilusTrader's `RiskEngine` (instrument
//! limits and `RiskEngineConfig` limits as separate checks in one pass) and LEAN
//! (`SymbolProperties` vs `RiskManagementModel`) keep the same two concerns apart.
//!
//! [`ProfileRisk::apply_to`] is the enforcement point: a profile that sets a venue-owned field
//! while a real grid was fetched is a **config error at load** (`Err` naming the offending key) —
//! never a silent clamp, which is how an operator sets a limit that never takes effect and never
//! learns. The one exception — a profile MAY supply the instrument fields when no grid was fetched
//! (the `make_engine` permissive-default fallback after a fetch failure, or a backtest/paper
//! mount) — is gated on the caller passing [`GridSource::NoGridFetched`], never inferred from a
//! field happening to be `None`.
//!
//! `vike-core::RunProfile::grid_source` derives the [`GridSource`] from its `mode` (`backtest`/
//! `paper` ⇒ `NoGridFetched`, `live` ⇒ `VenueFetched`) and rejects at load a `live` profile whose
//! `[risk]` sets ANY venue-owned field. `vike-backtest`'s `BacktestProfile` always merges as
//! [`GridSource::NoGridFetched`] and rejects `max_orders_per_window` at load: a wall-clock
//! throttle is meaningless in sim time (`SimBroker::build_risk_gate` always disarms it).
//!
//! ## One leverage concept, one name: `max_leverage` (issue #822)
//!
//! `[risk]` exposes **`max_leverage` only**; [`ProfileRisk::im_requirement`] converts it
//! (`im = 1.0 / max_leverage`) at THIS config edge into the field the buying-power check reads,
//! which keeps the serde name it has always had — renaming it would change
//! [`crate::engine_snapshot::state_hash`] and break journal replay. `[risk] im_requirement` is
//! GONE: [`ProfileRisk`] is `deny_unknown_fields`, so a profile still setting it fails LOUDLY at
//! load naming the key. `vike_sim::SimBroker::build_risk_gate` maps `EngineParams::leverage` onto
//! `RiskLimits::im_requirement` the same way, so "10x" means the same thing in backtest, paper
//! and live. NautilusTrader and Hummingbot likewise carry one leverage concept, never a "max
//! leverage" pre-trade check beside a margin fraction.

use serde::Deserialize;
use std::fmt;

use crate::RiskLimits;

/// `[risk]` — the pre-trade gate limits. A field-for-field mirror of [`RiskLimits`] (kept as a
/// mirror, not the type itself, so unknown-key denial applies inside `[risk]` too);
/// [`ProfileRisk::to_risk_limits`] is the compile-checked conversion.
///
/// ⚠ The FIRST paragraph of each field's doc is published verbatim into
/// `crates/vike-backtest/tests/fixtures/profile.json` (through `crate::risk_surface`), and the
/// body may hold only blank lines, `///` docs, one-line attributes and `pub name: Type,` fields.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProfileRisk {
    pub tick_size: Option<f64>,
    pub lot_size: Option<f64>,
    pub min_notional: Option<f64>,
    /// per-order minimum quantity floor (venue lot-size grid); checked on the lot-rounded qty.
    pub min_qty: Option<f64>,
    /// `[risk] max_notional_per_order` — the per-order notional cap the pre-trade gate evaluates
    /// on **EVERY order this engine admits**, whatever its origin (strategy-emitted,
    /// control-socket-originated, reconcile-originated). It lands verbatim in
    /// [`RiskLimits::max_notional_per_order`] and is denied as `over-max-notional`.
    ///
    /// ⚠ **The `policy.max_notional_per_order` row carries a ceiling of this exact NAME that judges
    /// a DIFFERENT ACT**: it guards only the EDGE surfaces a human types at, and
    /// `vike_mount::MountPolicy::from` drops it before it can reach a [`RiskLimits`]. Nothing
    /// compares the two numbers at load, mount or submit;
    /// `vike_config::ceilings::PRE_TRADE_CEILINGS` is the authority for both rows, and
    /// `vike-cli config show` renders it.
    ///
    /// Absent is NOT "uncapped" on a live-intent venue: `vike_mount::require_live_risk_budget`
    /// refuses the mount pre-connect naming this key.
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
    /// (`10.0` ⇒ `0.1`). `None` ⇒ the buying-power check stays disarmed.
    ///
    /// GUARD: a non-finite or `< 1.0` leverage is nonsense (`0.0` divides by zero; a negative gives
    /// a NEGATIVE margin requirement, i.e. unbounded buying power). It is rejected at load by
    /// `vike_core::RunProfile::validate` AND by [`ProfileRisk::apply_to`]; here, on the two
    /// INFALLIBLE paths a hand-built `ProfileRisk` reaches, it degrades to the most conservative
    /// reading, `Some(1.0)` (1×), never to `None` (gate off) and never to a negative fraction.
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
            // Pre-trade impact knobs stay OFF here: only `RiskGate::check_with_book` consults
            // them and the runtime hands the gate no book yet, so a `[risk]` knob would be inert
            // and misleading. Surfacing them belongs with that wiring.
            max_slippage_bps: None,
            require_fillable: false,
            // The price collar stays OFF too, for the same reason: surfacing it belongs with the
            // operator-facing wiring (a per-symbol band table).
            price_collar: None,
            collar_by_symbol: Default::default(),
            grid_by_symbol: Default::default(),
            // ⚠ NOT a profile field, and deliberately never one: the ACCOUNT-aggregate ceiling is
            // a property of the BOX and its wallet, not of the strategy run pointed at it. It
            // arrives from the `policy.max_account_exposure` row through `vike_mount::MountPolicy`
            // (`vike_config::Policy::max_account_exposure` carries the argument); accepting it
            // here too would give one ceiling two authorities, one swappable with `--profile`.
            max_account_exposure: None,
            // ⚠ NOT a profile field either, for the same reason
            // (`vike_config::Policy::max_sizing_equity`).
            max_sizing_equity: None,
        }
    }

    /// The `risk.*` keys naming the venue-owned instrument fields THIS profile sets — a list, so
    /// the split from the operator-owned fields is visible as data rather than branch logic.
    /// `pub`: `vike-core::RunProfile::validate`'s `mode = "live"` structural gate calls it too.
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
    /// either side silently overwrite the other's concern (the module doc's "Two owners, one
    /// struct").
    ///
    /// Behavior, by field group:
    /// - **Venue-owned** (`tick_size`/`lot_size`/`min_qty`/`min_notional`): under
    ///   [`GridSource::VenueFetched`], this profile setting ANY of them is a config error — `Err`
    ///   naming every offending key — and a successful result keeps `base`'s. Under
    ///   [`GridSource::NoGridFetched`] the profile's own instrument fields become the result's.
    /// - **Operator-owned** (everything [`ProfileRisk::to_risk_limits`] also sets from this
    ///   profile, plus the `im_requirement` DERIVED from `max_leverage`): always this profile's.
    /// - **Neither side's concern** (`im_by_symbol` — a live `Command::SetMargin` override; the
    ///   book-impact knobs; the price collar; and `max_account_exposure`/`max_sizing_equity`, which
    ///   belong to a THIRD owner, the policy file via `vike_mount::MountPolicy`): carried over
    ///   from `base` untouched.
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

        // `window_ms` is taken from THIS profile only when it sets the throttle; otherwise
        // `base`'s window carries through. It is `#[serde(default)]` on an `i64`, so an unset key
        // parses to `0`: taking it unconditionally would zero `base`'s real window
        // (`RiskLimits::new`'s 1000) whenever ANY profile merges in — cutoff == now, a throttle
        // that looks armed and never trips. The positivity check mirrors `RunProfile::validate`'s,
        // which lives in another crate and is skippable by building a bare `ProfileRisk`.
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

        // `max_leverage` is ENFORCED (it becomes the `im_requirement` the buying-power check
        // reads): `0.0` divides by zero and a negative yields a NEGATIVE margin requirement, i.e.
        // unbounded buying power. Rejected here as well as at load, for `window_ms`'s reason.
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

        // Field-by-field (NO `..base`/`..Default`), so a new `RiskLimits` field is a compile
        // error HERE too: a second, independent drift alarm.
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
            // Carried from `base`, never from this profile — a THIRD owner's field (the policy
            // file's, via `vike_mount::MountPolicy`). Were this `None`, merging a profile that
            // never mentions the key would silently disarm the box's account ceiling.
            max_account_exposure: base.max_account_exposure,
            // Carried from `base` for the same reason (the box's equity ceiling).
            max_sizing_equity: base.max_sizing_equity,
        })
    }

    /// Merge ONLY this profile's OPERATOR-owned fields onto `base`, ignoring — never rejecting —
    /// any venue-owned instrument field this profile might also (illegally) set; `base`'s own
    /// `tick_size`/`lot_size`/`min_qty`/`min_notional` are always kept. Infallible, unlike
    /// [`ProfileRisk::apply_to`].
    ///
    /// `vike-mount`'s live merge site falls back to this when `apply_to` rejects a profile under
    /// [`GridSource::VenueFetched`]: one illegal venue field must drop only itself, never mount the
    /// operator's ENTIRE risk budget unarmed.
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
            // Only reached when `apply_to` already errored on an UNRELATED field, so a throttle
            // whose window is still non-positive degrades to disarmed (`None`) rather than
            // raising a second error.
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
            // From `base`, as in `apply_to`: a REJECTED profile must not take the box's account
            // ceiling down with it.
            max_account_exposure: base.max_account_exposure,
            // From `base`, likewise for the box's equity ceiling.
            max_sizing_equity: base.max_sizing_equity,
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
    /// failed fetch, or no fetch attempted at all — every backtest/paper mount). The ONLY case
    /// where the profile may supply the instrument-grid fields, as their sole source of truth.
    NoGridFetched,
}

/// A run-profile loader failure — distinct kinds so a caller can tell I/O from a bad key from a
/// bad value. ONE type shared with `vike-core::RunProfile`, whose re-export stays a no-op for
/// every caller (`vike-backtest::BacktestProfile` uses its own `HarnessError`); this crate needs it
/// only as [`ProfileRisk::apply_to`]'s error type.
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

#[path = "risk_profile_tests.rs"]
#[cfg(test)]
mod risk_profile_tests;
