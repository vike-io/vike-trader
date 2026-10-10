//! `RiskLimits` — the pre-trade gate's CONFIGURATION — with its per-symbol overrides
//! (`SymbolGrid`, the resolved `ResolvedGrid`) and the fat-finger `PriceCollar` band.
//!
//! Stateless data and pure lookups only: the gate that reads them (`vike_exec::RiskGate`), the
//! engine state it judges against (`vike_exec::RiskContext`, `vike_exec::TradingState`) and its
//! verdict stay in vike-exec. `RiskLimits` is serde-embedded in `vike_exec::EngineSnapshot`, whose
//! canonical `serde_json` bytes feed `vike_exec::state_hash` (the journal determinism fence,
//! compared ACROSS BINARY VERSIONS): those bytes carry field names and values, never a crate path,
//! so the type's home does not reach them, and every `skip_serializing_if` below is load-bearing
//! (`docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`).

use indexmap::IndexMap;

/// `skip_serializing_if` predicate for default-`false` bool knobs (see [`RiskLimits`]).
#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

/// `skip_serializing_if` predicate for the empty per-symbol collar map (see
/// [`RiskLimits::collar_by_symbol`]). A named fn rather than `IndexMap::is_empty` so the path
/// resolves without leaning on inference inside a serde attribute.
#[inline]
fn is_empty_collar_map(m: &IndexMap<String, PriceCollar>) -> bool {
    m.is_empty()
}

/// `skip_serializing_if` predicate for the empty per-symbol grid map (see
/// [`RiskLimits::grid_by_symbol`]) — same shape and reason as [`is_empty_collar_map`].
#[inline]
fn is_empty_grid_map(m: &IndexMap<String, SymbolGrid>) -> bool {
    m.is_empty()
}

/// A per-symbol override of the venue PRICE/SIZE GRID: the tick and lot an order is rounded
/// onto, and the floors it must clear.
///
/// [`RiskLimits`]'s own `tick_size`/`lot_size`/`min_notional`/`min_qty` are SCALARS built from
/// ONE symbol's `SymbolProperties`, which is WRONG the moment an engine admits a second symbol
/// (`extra_symbols`): a coarse mount lot destroys a valid finer-grid order —
/// `round_to(0.5, Some(1.0)) == 0.0`, then denied as `"non-positive-size"`, a reason naming
/// nothing about the real cause.
///
/// Each field falls back INDEPENDENTLY to the scalar, so an override that only pins `lot_size`
/// still inherits the engine's tick and floors rather than silently disabling them.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SymbolGrid {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lot_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_notional: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_qty: Option<f64>,
}

impl SymbolGrid {
    /// Build one symbol's override from THAT symbol's own venue-fetched instrument grid — the
    /// per-leg twin of [`RiskLimits::from_properties`], over the same four fields and the same
    /// `0.0`-means-unconstrained fold (`vike_model::nz_step`). Spelled HERE, beside the scalar
    /// builder rather than at the mount, so the two cannot drift:
    /// `a_symbol_grid_matches_the_scalar_builder_field_for_field` runs both over one input.
    ///
    /// ⚠ **A `0.0` field becomes `None`, and `None` here means INHERIT THE MOUNT SCALAR — not
    /// "unconstrained".** So a leg whose venue publishes no lot still rounds on the MOUNTED
    /// symbol's lot: a NARROWING of the defect, not a full repair, and deliberate. [`SymbolGrid`]
    /// has no spelling for "explicitly unconstrained", and adding one changes the serialized shape
    /// of [`RiskLimits`], whose canonical bytes feed `vike_exec::state_hash` (the
    /// journal determinism fence, compared ACROSS BINARY VERSIONS).
    /// `a_zero_field_from_the_venue_inherits_the_mount_scalar` pins the residual.
    pub fn from_properties(f: &crate::SymbolProperties) -> Self {
        use crate::nz_step as nz;
        SymbolGrid {
            tick_size: nz(f.tick_size),
            lot_size: nz(f.step_size),
            min_notional: nz(f.min_notional),
            min_qty: nz(f.min_qty),
        }
    }
}

/// The grid actually applied to ONE order — a per-symbol override resolved field-by-field over
/// the engine's scalars. Returned by [`RiskLimits::grid_for`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedGrid {
    pub tick_size: Option<f64>,
    pub lot_size: Option<f64>,
    pub min_notional: Option<f64>,
    pub min_qty: Option<f64>,
}

/// A fat-finger PRICE COLLAR band: how far an order's own limit/trigger price may sit from the
/// mark before the gate refuses it (the `risk` module doc's collar section).
///
/// The band is `max(pct × mark, abs_floor)`: the percentage governs expensive instruments, the
/// absolute floor cheap ones. Values are in the instrument's quote units; `pct` is a FRACTION
/// (`0.10` = 10%), not basis points and not a percent number.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PriceCollar {
    /// fraction of the mark the price may deviate by, e.g. `0.10` ⇒ ±10%
    pub pct: f64,
    /// absolute band floor in quote units, e.g. `0.02` ⇒ always at least ±0.02
    pub abs_floor: f64,
}

impl PriceCollar {
    /// `max(pct × mark, abs_floor)` — the half-width of the admissible price band around `mark`.
    /// Callers only ever reach this with a finite, positive `mark` (the gate skips otherwise).
    ///
    /// TOTAL AND NON-NEGATIVE BY CONSTRUCTION: each half is clamped to `0.0` unless finite and
    /// strictly positive. The gate's test is `|p − mark| > band`, so a negative or `NaN` band from
    /// a garbage config would make this opt-in knob a TOTAL KILL SWITCH denying every priced order;
    /// clamped, the worst is a `0.0` band (an un-configured collar is `None`, not a zero band).
    #[inline]
    pub fn band(&self, mark: f64) -> f64 {
        // each half contributes nothing unless it is finite AND strictly positive
        let sane = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
        // `mark` is finite and > 0 at every call site, so `pct * mark` is finite and >= 0.0.
        (sane(self.pct) * mark).max(sane(self.abs_floor))
    }
}

/// Gate configuration. All limits optional; `None` disables that check.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RiskLimits {
    pub tick_size: Option<f64>,
    pub lot_size: Option<f64>,
    pub min_notional: Option<f64>,
    /// per-order minimum quantity floor (venue lot-size grid); checked on the lot-rounded qty.
    pub min_qty: Option<f64>,
    pub max_notional_per_order: Option<f64>,
    /// Cap on **ONE SYMBOL's** projected open notional at **ONE VENUE** — despite the name, NOT
    /// an account-wide, cross-symbol or cross-venue total.
    ///
    /// `check_inner`'s `over-max-exposure` lane is the only site that evaluates it, as
    /// `|ctx.position_size + side × qty| × ctx.mark_price × ctx.multiplier > cap`, and
    /// `vike_exec::RiskContext::position_size` is the signed position in the ORDER's symbol at this
    /// engine's own venue (`ExecutionEngine::gate_position_size`). The gate holds no cross-symbol
    /// book and the mount builds one engine per venue, so an operator trading N symbols is
    /// protected by this number N times over and never once in aggregate: a number sized for a
    /// whole book is an N× weaker cap than they believe they set.
    ///
    /// The scope is MACHINE-PINNED: `crates/vike-exec/tests/risk/risk_lane_pricing.rs`'s
    /// `max_total_exposure_is_scoped_to_one_venue_and_one_symbol` proves a position in another
    /// symbol, or the same symbol at another venue, contributes NOTHING.
    ///
    /// **The name is deliberately KEPT** (`max_symbol_exposure` was considered and rejected). The
    /// same word is the operator-facing `[risk]` TOML key (`ProfileRisk::max_total_exposure`), the
    /// `&'static str` `vike_mount::require_live_risk_budget` puts in its refusal, and a serde key
    /// (no `skip_serializing_if`) inside `vike_exec::EngineSnapshot`, whose canonical bytes feed
    /// `vike_exec::state_hash` across binary versions, so a rename changes every
    /// recorded hash. A `#[serde(rename)]` would only make the identifier disagree with the string
    /// the OPERATOR reads. So the truth is carried where it is read: here, in `ProfileRisk`'s twin,
    /// in the refusal's `BUDGET_EXAMPLES` row, and in `docs/ops/run-profile-live.toml`. Same trade
    /// [`RiskLimits::max_leverage`] records.
    ///
    /// The account-AGGREGATE ceiling is a different lane, not a rename:
    /// [`RiskLimits::max_account_exposure`].
    pub max_total_exposure: Option<f64>,
    /// **Cap on the WHOLE ACCOUNT's projected gross open notional** — the axis
    /// [`RiskLimits::max_total_exposure`] is NAMED for and is not.
    ///
    /// Evaluated by `check_inner`'s `over-account-exposure` lane as `projected +
    /// ctx.account_exposure_excl_order > cap`, where `projected` is the per-symbol lane's identical
    /// term. The `risk` module doc says what "the account" is (one `(venue, AccountLabel)` ENGINE,
    /// with the shared-BOOK residual) and why the axis BYPASSES a covered reduce.
    ///
    /// **It can only ever REFUSE.** `None` (the default) skips the comparison and the producer
    /// skips the fold that feeds it.
    ///
    /// The operator writes it as the `policy.max_account_exposure` row, NOT in a run profile's
    /// `[risk]` table: an account ceiling is a property of the BOX and its wallet, not of the
    /// strategy run pointed at it (`vike_config::Policy::max_account_exposure` carries the
    /// argument; `vike_mount::MountPolicy` brings it here).
    ///
    /// ⚠ It is NOT part of `vike_mount::require_live_risk_budget`'s refusal, deliberately: a third
    /// mandatory cap would stop every existing live deployment from starting on upgrade. Absent
    /// stays absent, and loudly: `docs/ops/kill-switches.md` documents the key and
    /// `vike-cli config show` discloses it.
    ///
    /// SERDE: `skip_serializing_if` is LOAD-BEARING, for the reason on
    /// [`RiskLimits::max_slippage_bps`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_account_exposure: Option<f64>,
    /// **The largest EQUITY FIGURE this engine's sizing and admission lanes may see**, in quote
    /// currency. `None` — the default — leaves `vike_exec::ExecutionEngine::sizing_equity`
    /// bit-identical to `vike_exec::ExecutionEngine::resolved_equity`.
    ///
    /// ⚠ **It is not a limit lane, and `check()` never reads it**: it bounds an INPUT the gate
    /// (and the sizer above it) is fed. It lives here because this is the per-engine risk
    /// configuration the mount already folds a policy onto, with the same `min` fold
    /// ([`RiskLimits::narrow_sizing_equity`]) as [`RiskLimits::max_account_exposure`].
    ///
    /// The operator writes it as the `policy.max_sizing_equity` row, NOT in `[risk]`, for the
    /// account ceiling's reason (`vike_config::Policy::max_sizing_equity`), and it is NOT part of
    /// `vike_mount::require_live_risk_budget`'s refusal for the same reason as that ceiling.
    ///
    /// SERDE: `skip_serializing_if` is LOAD-BEARING, for the reason on
    /// [`RiskLimits::max_slippage_bps`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sizing_equity: Option<f64>,
    pub max_orders_per_window: Option<usize>,
    pub window_ms: i64,
    /// The operator's DECLARED leverage cap, recorded for the snapshot/audit surface — NOT
    /// evaluated in `check()`. Enforcement is [`RiskLimits::im_requirement`], which the config
    /// edge derives from this same number (`im = 1.0 / max_leverage`, see `ProfileRisk`), so the
    /// two agree by construction (issue #822).
    ///
    /// FROZEN, deliberately NOT deleted: `RiskLimits` is serde-embedded in
    /// `vike_exec::EngineSnapshot`, whose canonical bytes feed
    /// `vike_exec::state_hash`. Removing the field changes that shape and, by
    /// `vike_journal`'s version contract, would force `MIN_READABLE_VERSION` up to `VERSION`,
    /// turning every existing journal into a cold-start error.
    pub max_leverage: Option<f64>,
    pub block_reduce_only_overshoot: bool,
    /// RUST-NATIVE (no Python twin — risk.py has no margin checks): initial-margin fraction (LEAN
    /// `1/leverage`, e.g. 0.1 ⇒ 10x). `Some` enables the pre-trade buying-power check with LEAN
    /// `BuyingPowerModel` semantics (covered reduce/close orders bypass; flips get the closing
    /// credit). `None` (default) = off.
    ///
    /// This is the INTERNAL STORAGE form only — the operator-facing name is `max_leverage`
    /// (`[risk] max_leverage = 10.0` in a profile TOML), converted by `ProfileRisk` at the config
    /// edge. `im_requirement` keeps its wire name because it is part of the
    /// `vike_exec::state_hash` surface; see [`RiskLimits::max_leverage`].
    pub im_requirement: Option<f64>,
    /// Per-symbol initial-margin override (`1/leverage`), set live via `Command::SetMargin`.
    /// Falls back to `im_requirement` (the venue default) when a symbol is absent; empty by
    /// default. `IndexMap` keeps insertion order (bit-parity rule), though this is a lookup map.
    #[serde(default)]
    pub im_by_symbol: IndexMap<String, f64>,
    /// LEAN `RequiredFreeBuyingPowerPercent` haircut on equity (0.0 default).
    pub required_free_bp_pct: f64,
    /// OPT-IN pre-trade market-impact budget in basis points vs mid, evaluated against the
    /// DISPLAYED book (see the module doc). `None` (default) = off: no book is ever walked.
    /// Only consulted by `vike_exec::RiskGate::check_with_book` with a book.
    ///
    /// SERDE: `skip_serializing_if` is LOAD-BEARING, not cosmetic. `RiskLimits` is embedded in
    /// `vike_exec::EngineSnapshot`, whose canonical `serde_json` bytes feed
    /// `vike_exec::state_hash` — the journal determinism fence, compared ACROSS
    /// BINARY VERSIONS. Emitting `"max_slippage_bps":null` would change that hash for every
    /// snapshot with the knob OFF, so a journal recorded before this field existed would fail
    /// replay's fence with a spurious "replayed hash != recorded". Off ⇒ absent ⇒ same bytes.
    /// Every other skipped field below and above cites this paragraph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_slippage_bps: Option<f64>,
    /// OPT-IN own knob (default `false`): deny when displayed depth cannot cover the full order
    /// size, regardless of `max_slippage_bps`. Either knob alone arms the book walk.
    ///
    /// SERDE: `skip_serializing_if` is load-bearing (see [`RiskLimits::max_slippage_bps`]).
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_fillable: bool,
    /// OPT-IN fat-finger PRICE COLLAR — the venue-wide default band (the `risk` module doc's
    /// collar section). `None` (default) = the axis does not exist.
    ///
    /// SERDE: `skip_serializing_if` is load-bearing (see [`RiskLimits::max_slippage_bps`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_collar: Option<PriceCollar>,
    /// Per-symbol price-collar override, mirroring [`RiskLimits::im_by_symbol`]: falls back to
    /// [`RiskLimits::price_collar`] (the venue default) when a symbol is absent. Empty by default.
    ///
    /// SERDE: unlike `im_by_symbol` (which predates the determinism fence, so its `{}` is already
    /// baked into the pinned hash) this map is SKIPPED when empty, same reason as `price_collar`.
    #[serde(default, skip_serializing_if = "is_empty_collar_map")]
    pub collar_by_symbol: IndexMap<String, PriceCollar>,
    /// Per-symbol PRICE/SIZE GRID overrides (see [`SymbolGrid`]) — the precondition for admitting
    /// an order in a symbol other than the engine's own. Empty means every order is rounded onto
    /// the scalars above.
    ///
    /// SERDE: SKIPPED when empty, same determinism-fence reason as `collar_by_symbol`.
    #[serde(default, skip_serializing_if = "is_empty_grid_map")]
    pub grid_by_symbol: IndexMap<String, SymbolGrid>,
}

impl RiskLimits {
    /// The price/size grid to judge an order in `symbol` on: its per-symbol override resolved
    /// field-by-field over the engine's scalars (the scalars verbatim when no override exists).
    /// Field-by-field is deliberate: a half-specified grid must not be a way to disable a floor.
    #[inline]
    pub fn grid_for(&self, symbol: &str) -> ResolvedGrid {
        match self.grid_by_symbol.get(symbol) {
            None => ResolvedGrid {
                tick_size: self.tick_size,
                lot_size: self.lot_size,
                min_notional: self.min_notional,
                min_qty: self.min_qty,
            },
            Some(g) => ResolvedGrid {
                tick_size: g.tick_size.or(self.tick_size),
                lot_size: g.lot_size.or(self.lot_size),
                min_notional: g.min_notional.or(self.min_notional),
                min_qty: g.min_qty.or(self.min_qty),
            },
        }
    }

    pub fn new() -> Self {
        RiskLimits { window_ms: 1000, ..Default::default() }
    }

    /// **Fold an ACCOUNT ceiling in so that it can only ever NARROW** — `min` when both sides carry
    /// a number, the one that exists when only one does, `None` when neither.
    ///
    /// The arming sites (`vike_mount::make_engine_for_account`, and the paper assembly beside it)
    /// go through here rather than assigning the field: "it can only ever refuse" must be a
    /// property of the OPERATION, not of nobody else writing the field. A plain assignment would
    /// silently erase a later conservative default or a second fold, including a `Some`
    /// overwritten with `None`. `vike_config::VenueMode::cap` is the precedent.
    pub fn narrow_account_exposure(&mut self, cap: Option<f64>) {
        self.max_account_exposure = match (self.max_account_exposure, cap) {
            (Some(held), Some(incoming)) => Some(held.min(incoming)),
            (held, incoming) => held.or(incoming),
        };
    }

    /// **Fold a SIZING-EQUITY ceiling in so that it can only ever LOWER the figure** — the exact
    /// twin of [`Self::narrow_account_exposure`], for the identical reason; the arming sites go
    /// through here rather than assigning [`RiskLimits::max_sizing_equity`].
    ///
    /// ⚠ **"Lower" is conservative HERE and nowhere else on this struct**, which is why the seam
    /// that consumes this field is a separate resolver rather than a substitution:
    /// `vike_exec::ExecutionEngine::sizing_equity` applies it and
    /// `vike_exec::ExecutionEngine::resolved_equity` — what the margin-CALL sweep judges against —
    /// does not. A `min` that reached the liquidation decision would be strictly destructive.
    pub fn narrow_sizing_equity(&mut self, cap: Option<f64>) {
        self.max_sizing_equity = match (self.max_sizing_equity, cap) {
            (Some(held), Some(incoming)) => Some(held.min(incoming)),
            (held, incoming) => held.or(incoming),
        };
    }

    /// Resolve the initial-margin fraction for `symbol`: the per-symbol override if present,
    /// else the venue default `im_requirement`, else `None` (buying-power gate off).
    #[inline]
    pub fn im_for(&self, symbol: &str) -> Option<f64> {
        self.im_by_symbol.get(symbol).copied().or(self.im_requirement)
    }

    /// Resolve the fat-finger price collar for `symbol`: the per-symbol override if present, else
    /// the venue default [`RiskLimits::price_collar`], else `None` (the axis is off). Same
    /// override-then-default shape as [`RiskLimits::im_for`].
    #[inline]
    pub fn collar_for(&self, symbol: &str) -> Option<PriceCollar> {
        self.collar_by_symbol.get(symbol).copied().or(self.price_collar)
    }

    /// Build limits from a venue's fetched instrument grid (`0.0` fields = unconstrained → None).
    /// tick_size→tick_size, step_size→lot_size, min_qty→min_qty, min_notional→min_notional; other
    /// knobs inherit `new()` defaults (window_ms=1000, everything else off).
    pub fn from_properties(f: &crate::SymbolProperties) -> Self {
        use crate::nz_step as nz;
        RiskLimits {
            tick_size: nz(f.tick_size),
            lot_size: nz(f.step_size),
            min_notional: nz(f.min_notional),
            min_qty: nz(f.min_qty),
            ..RiskLimits::new()
        }
    }
}

#[path = "limits_tests.rs"]
#[cfg(test)]
mod limits_tests;
