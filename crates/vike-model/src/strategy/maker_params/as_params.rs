//! The Avellaneda-Stoikov sub-bag: `AsParams`, its mode and model enums, and its serde defaults.

#[cfg(doc)]
use super::{SpreadMakerParams, ToxicityParams};

/// The time-horizon model for the Avellaneda–Stoikov `(T − t)` term (see [`AsParams`]). The A-S
/// risk term scales with the time left before the maker expects to unwind / the market resolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum HorizonMode {
    /// Real time-to-resolution: the effective horizon is `H = min(τ_hold, T − t)` from
    /// [`AsParams::resolution_ts`] (for Polymarket, `GammaMarket.end_date`). As `t → T` the local
    /// risk term shrinks on its own. The DEFAULT. With no `resolution_ts` set it falls back to the
    /// constant `τ_hold`.
    #[default]
    TimeToResolution,
    /// A constant rolling tenor `H = τ_hold` — for open-ended / crypto markets that never resolve.
    ConstantTau,
}

/// Which effective-variance `V` form multiplies the A-S reservation price and spread (see
/// [`AsParams`]). `V` is the variance of the price move to the horizon; the modes trade the raw
/// local vol against the 0–1 Bernoulli ceiling `p(1−p)` that bounds any pre-resolution price move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum VarianceMode {
    /// `V = min(σ̂²·H, p·(1−p))` — local vol CAPPED by the Bernoulli outcome variance. The DEFAULT
    /// and the Polymarket-correct form: skew and spread vanish at the 0/1 walls and peak at `p = 0.5`.
    #[default]
    LocalCapped,
    /// `V = p·(1−p)` — pure Bernoulli; needs NO σ estimate or horizon at all (an ultra-robust
    /// fallback for thin books where σ cannot be estimated).
    PureBernoulli,
    /// `V = σ̂²·H` — raw local vol, IGNORING the walls (the classic unbounded A-S form; for
    /// open-ended / crypto markets whose prices are not 0–1 probabilities).
    RawLocal,
}

/// Where the A-S fill-intensity decay `κ` comes from (see [`AsParams`]). `κ` sets the spread's
/// adverse-selection floor `(1/γ)·ln(1+γ/κ)`; only `κ` (not the base rate `A`) enters the spread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum KappaMode {
    /// A FIXED `κ_default` (deterministic, safe). The DEFAULT. The online MLE is still maintained
    /// from the trade tape but is NOT used to price — "available but gated".
    #[default]
    Fixed,
    /// The online size-weighted MLE `κ̂ = Σw / Σ(w·δ)` over the event-time trade window, gated by the
    /// `n_min` sample floor (below it, or on a degenerate window, it falls back to `κ_default`).
    LiveFit,
    /// Fit `κ` from the maker's OWN order outcomes — a CENSORED exponential-hazard MLE over
    /// `(distance-to-mid, exposure, filled?)` tuples where a pulled/unfilled order is a
    /// RIGHT-CENSORED exposure — and SHRINK toward the public-print [`Self::LiveFit`] estimate below
    /// an `n_min` own-fill count. Where [`Self::LiveFit`] reads the whole tape's prints, this reads
    /// the fill intensity THIS maker actually realises at its own quoted distances, so `κ` reflects
    /// the adverse selection it truly faces. Additive: a payload predating the variant never has it,
    /// and with no own fills yet it prices identically to the shrink target (the public κ, which
    /// itself falls back to `κ_default`).
    OwnFillFit,
}

/// The PRICE DOMAIN the Avellaneda–Stoikov quoting layer operates in — the wall/clamp regime a
/// posted quote is confined to (see [`AsParams::price_domain`]). The A-S core was first written for
/// Polymarket's `[0,1]` outcome-token prices (the [`Self::UnitInterval`] default); this knob
/// GENERALIZES the same `vike_mm::SpreadMaker` pricing onto a `$`-scale asset (a BTC perp mid ~$64k)
/// WITHOUT disturbing that Polymarket behavior — an old journal / GUI payload carrying no
/// `price_domain` decodes to [`Self::UnitInterval`] and prices byte-identically. Net-new Rust
/// surface — no Python twin (like the rest of [`AsParams`]).
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum PriceDomain {
    /// Polymarket 0–1 outcome-token prices: a posted quote is clamped into `[tick, 1 − tick]` (the
    /// wall clamp). The DEFAULT and the only domain before the crypto generalization — pairs with
    /// [`VarianceMode::LocalCapped`]/[`VarianceMode::PureBernoulli`] (the Bernoulli cap `p(1−p)` is
    /// well-defined on `[0,1]`).
    #[default]
    UnitInterval,
    /// An UNBOUNDED `$`-scale price (a BTC/ETH perp mid, an equity, an FX rate): NO wall clamp — a
    /// posted quote is bounded only by the per-side standoff and the `bid < s < ask` straddle (the
    /// classic textbook A-S domain). MUST pair with [`VarianceMode::RawLocal`] (the Bernoulli cap
    /// `p(1−p)` goes NEGATIVE for a price `> 1`, collapsing `V` to `0` and the spread with it) and
    /// typically [`HorizonMode::ConstantTau`] (a market that never resolves). See
    /// [`AsParams::min_half_spread_ticks`] for the sub-tick-spread collapse this domain must guard.
    Unbounded,
    /// An explicit price BAND `[lo, hi]` — a capped-range instrument (e.g. a `0–100` bounded index
    /// future). Clamps exactly like [`Self::UnitInterval`] but onto caller-supplied walls instead of
    /// the `[tick, 1 − tick]` unit interval.
    Band {
        /// Lower price wall (a posted bid never clamps below this).
        lo: f64,
        /// Upper price wall (a posted ask never clamps above this).
        hi: f64,
    },
}

/// The Avellaneda–Stoikov quoting knobs — a flat `Copy` scalar sub-bag nested as
/// `Option<AsParams>` inside [`SpreadMakerParams`] (`None` = A-S disabled, the default). When
/// present it turns the `vike-mm` `SpreadMaker` into an A-S reservation-price + optimal-spread
/// PRICING layer adapted to Polymarket's 0–1 bounded prices, whose output still flows through the
/// maker's existing size-skew → fill-rate breaker → own-order filtration → modify-in-place tail
/// (so A-S inherits the adverse-selection guard it classically lacks).
///
/// Reservation price `r = s − q_norm·γ·V`, optimal half-spread `δ = ½·[γ·V + (2/γ)·ln(1+γ/κ)]`,
/// where `s` = fair mid, `q_norm = position / q_scale`, `V` = effective variance ([`VarianceMode`]),
/// `κ` = fill intensity ([`KappaMode`]). All three modeling choices are runtime-tunable knobs
/// defaulting (via [`AsParams::default`]) to the recommended Polymarket configuration. Net-new Rust
/// surface — no Python twin.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AsParams {
    /// Risk aversion `γ` (must be `> 0`). Larger ⇒ stronger inventory skew AND wider spread.
    pub gamma: f64,
    /// Horizon model for `(T − t)` — real time-to-resolution vs a constant tenor.
    pub horizon_mode: HorizonMode,
    /// Holding tenor `τ_hold` (ms): the horizon in [`HorizonMode::ConstantTau`], and the CAP on the
    /// time-to-resolution in [`HorizonMode::TimeToResolution`] (`H = min(τ_hold, T − t)`).
    pub tau_hold_ms: i64,
    /// Resolution timestamp `T` (epoch-ms) for [`HorizonMode::TimeToResolution`] — for Polymarket,
    /// `GammaMarket.end_date` parsed to epoch-ms at mount. `None` ⇒ no resolution known ⇒ fall back
    /// to `τ_hold`. `(T − t)` is recomputed each tick from the event clock, so a reschedule only
    /// needs a live-params correction, not per-tick churn.
    pub resolution_ts: Option<i64>,
    /// Near-resolution BLACKOUT: within this many ms of `resolution_ts`, quote maximally WIDE (push
    /// fills out to the `[tick, 1−tick]` walls) to dodge the resolution-news vol spike the Bernoulli
    /// cap does not catch. `0` disables the blackout. (An active mass-cancel PULL is the mount's
    /// `on_feed_status` job; this knob is the pricing-layer stance.)
    pub resolution_blackout_ms: i64,
    /// Which effective-variance `V` form to use.
    pub variance_mode: VarianceMode,
    /// Where `κ` comes from (fixed vs online MLE).
    pub kappa_mode: KappaMode,
    /// Fixed `κ` — used directly in [`KappaMode::Fixed`], and the fallback below `n_min` / on a dead
    /// tape in [`KappaMode::LiveFit`].
    pub kappa_default: f64,
    /// Lower clamp on a fitted `κ` (`κ → 0` blows `ln(1+γ/κ)` → the spread explodes).
    pub kappa_min: f64,
    /// Upper clamp on a fitted `κ` (`κ → ∞` collapses the spread).
    pub kappa_max: f64,
    /// EWMA half-life (in updates) for the online σ̂² estimator (`α = 1 − 0.5^(1/half_life)`).
    pub sigma_half_life: f64,
    /// Event-time rolling window (ms) over which the κ MLE's trade tape is kept.
    pub trade_window_ms: i64,
    /// Minimum trades in the window before a live κ fit is trusted (else `κ_default`).
    pub n_min: usize,
    /// Inventory normaliser: `q_norm = position / q_scale`, keeping `γ` scale-stable. Must be `> 0`
    /// (a `0.0` is treated as `1.0`).
    pub q_scale: f64,
    /// Minimum standoff (in ticks) each POSTED quote keeps from the fair mid `s`, so a heavy
    /// inventory can pull the reservation price toward a wall but the posted quote never crosses `s`.
    pub min_standoff_ticks: f64,
    /// Use the imbalance-adjusted micro-price as the fair value `s` (else the raw mid).
    pub use_micro_price: bool,
    /// TERMINAL settlement-variance penalty coefficient `γ_term` (arXiv 2607.17991). Near a binary
    /// resolution the effective inventory-aversion variance must include the SETTLEMENT variance
    /// `p(1−p)`, which the diffusion term `σ̂²·H` misses as `H → 0`. This coefficient scales that
    /// settlement variance into `V`, folded in over the last [`Self::terminal_ramp_ms`] before
    /// `resolution_ts`, so the reservation skew STRENGTHENS as `t → T` (urgency to unwind) and the
    /// half-spread WIDENS at `p ≈ 0.5` — the published binary-market optimum. `0.0` (the default)
    /// disables it, byte-identical. Only active in [`HorizonMode::TimeToResolution`] with a known
    /// `resolution_ts`, and only when `terminal_ramp_ms > 0`.
    #[serde(default)]
    pub terminal_penalty_gamma: f64,
    /// Ramp-in window (ms) for the [`Self::terminal_penalty_gamma`] settlement penalty: the weight is
    /// `clamp(1 − (T − t)/ramp, 0, 1)` — `0` outside the window, ramping to `1` at resolution. `0`
    /// (the default) disables the penalty entirely, byte-identical.
    #[serde(default)]
    pub terminal_ramp_ms: i64,
    /// UNDERLYING-anchored fair mid (PM deep-dive #5). Blend weight in `[0,1]` for the model
    /// probability `p_up(underlying)` against the PM-book mid: the PRICED fair value becomes
    /// `(1−w)·book_mid + w·p_up`. `0.0` (the default) = book-only, byte-identical. The blend feeds
    /// ONLY the pricing (reservation + half-spread); the σ̂²/κ estimators keep tracking the raw book
    /// mid. Requires a live underlying (fed via the maker's `set_underlying` seam) AND
    /// `window_secs > 0` AND the [`HorizonMode::TimeToResolution`] regime with a known
    /// `resolution_ts`; otherwise the anchor falls back to the book mid.
    #[serde(default)]
    pub underlying_weight: f64,
    /// Drift-sensitivity `β` for the [`Self::underlying_weight`] model probability (the `p_up`
    /// Bachelier drift term). Ignored when `underlying_weight == 0.0`. Default `1.0`.
    #[serde(default = "default_underlying_beta")]
    pub underlying_beta: f64,
    /// Window length `h` in SECONDS for the [`Self::underlying_weight`] model (the rolling up/down
    /// window, e.g. `300.0` for a 5-minute market). `0.0` (the default) disables the blend (no
    /// window ⇒ no model probability). Seconds-into-window `t` is derived from `resolution_ts`.
    #[serde(default)]
    pub window_secs: f64,
    /// ATM settlement guard (PM deep-dive #4): scale for WIDENING the near-resolution blackout when
    /// the market is near-ATM. The vol-normalized moneyness `z = |ln(S/S_open)| / (σ·√τ)` is ~0 at a
    /// coin-flip (the settlement-manipulation danger zone) and grows as the outcome decides; the
    /// blackout window is multiplied by `1 + atm_blackout_scale·exp(−z²/2)`, so it starts EARLIEST at
    /// ATM (`base·(1 + scale)`) and relaxes to the base `resolution_blackout_ms` far from it. `0.0`
    /// (the default) = fixed blackout, byte-identical. Needs a live underlying (fed via the maker's
    /// `set_underlying` seam) AND the [`HorizonMode::TimeToResolution`] regime.
    #[serde(default)]
    pub atm_blackout_scale: f64,
    /// The PRICE DOMAIN the quote geometry is confined to (the crypto generalization). [`PriceDomain::
    /// UnitInterval`] (Polymarket `[0,1]`, the default) clamps a posted quote into `[tick, 1 − tick]`;
    /// [`PriceDomain::Unbounded`] (a `$`-scale crypto/equity asset) drops the wall clamp;
    /// [`PriceDomain::Band`] uses explicit walls. `#[serde(default)]` ⇒ [`PriceDomain::UnitInterval`],
    /// so an old journal / GUI payload predating this knob decodes to the Polymarket behavior and
    /// prices byte-identically. See [`PriceDomain`] for the mandatory [`VarianceMode`] pairing.
    #[serde(default)]
    pub price_domain: PriceDomain,
    /// Minimum half-spread FLOOR in ticks, applied to the A-S optimal half-spread `δ` BEFORE the
    /// quote snaps to the grid: `δ ← max(δ, min_half_spread_ticks · tick)`. At `$`-scale prices the
    /// adverse-selection floor `(1/γ)·ln(1+γ/κ)` can be SUB-TICK (e.g. ~0.02 at a $64k mid on a $1
    /// tick), so `r ± δ` snaps bid and ask onto the SAME tick and the two-sided quote collapses to
    /// `None`. Flooring `δ` keeps the sides at least this many ticks apart. `0.0` (the default) = no
    /// floor: the A-S half-spread is always `≥ 0`, so `δ.max(0.0) == δ` bit-for-bit and it stays
    /// byte-identical; the crypto mount sets it ~2. `#[serde(default)]` so an old payload decodes to
    /// `0.0`.
    #[serde(default)]
    pub min_half_spread_ticks: f64,
    /// Maximum half-spread CEILING in ticks, applied to the A-S optimal half-spread `δ` AFTER the
    /// [`min_half_spread_ticks`](AsParams::min_half_spread_ticks) floor and BEFORE the grid snap:
    /// `δ ← min(δ, max_half_spread_ticks · tick)` (only when `> 0`). The A-S half-spread grows as the
    /// SQUARE of realized volatility (`½·γ·σ̂²·H`), so a vol spike drives it arbitrarily wide — on a
    /// `$`-scale asset an unbounded `δ` posts a quote so far from the touch it can never fill, i.e. the
    /// maker silently LEAVES the market in exactly the volatile moment. Capping `δ` keeps the maker
    /// quoting a still-plausibly-fillable spread through a spike (the standard production A-S guard).
    /// `0.0` (the default) = NO ceiling: `δ.min(∞)`-equivalent, byte-identical to the pre-ceiling path;
    /// the crypto mount sets ~60 (a ~19 bp full-spread cap at a $64k mid). `#[serde(default)]` so an
    /// old payload decodes to `0.0` (uncapped, unchanged). Ignored if it would fall below the floor
    /// (`max < min` ⇒ the floor wins — a two-sided quote is never sacrificed to the ceiling).
    #[serde(default)]
    pub max_half_spread_ticks: f64,
    /// The venue's SINGLE-VENUE round-trip fee as a FRACTION OF PRICE
    /// ([`crate::maker_round_trip_fee`] — `maker + maker`, both legs resting), armed by the MOUNT.
    /// `Some(m)` floors the posted half-spread at the BREAK-EVEN width `½·m·s` (`s` the fair mid the
    /// quote is priced from), because a completed round trip captures `2δ` and pays `m·s`; a quote
    /// narrower than that loses money on every cycle it completes, at any tuning.
    ///
    /// ⚠ **Unlike [`Self::min_half_spread_ticks`], this floor does NOT beat
    /// [`Self::max_half_spread_ticks`] — it REFUSES.** When `½·m·s` exceeds the operator's ceiling
    /// there is no width that is both plausibly fillable (what the ceiling asserts) and profitable
    /// (what this floor asserts), so the maker posts NOTHING for that tick rather than either
    /// booking a known loss at the cap or silently overriding a cap the operator set. That is the
    /// same `None`-means-hold contract every other refusal in this layer uses (one-sided book,
    /// collapsed sides, pinned at a wall) — see `vike_mm::avellaneda::bounded_half_spread`.
    ///
    /// ⚠ **`None` means NO FLOOR IS ARMED, and it is NOT the same statement as `Some(0.0)`.** A
    /// `Some(0.0)` is a MEASURED zero-fee venue (aster's perp maker row); `None` is "nobody could
    /// name this venue's maker fee as a fraction of price" — every shape
    /// [`crate::maker_round_trip_fee`] refuses, which includes every `Free` FX/CFD venue that
    /// charges through the spread. The mount is what must make that loud; this field only records
    /// which of the two happened, and `vike-tradehub`'s startup `effective_params` line prints it.
    ///
    /// `#[serde(default)]` ⇒ `None`, so an old journal/GUI payload and every hand-built `AsParams`
    /// price byte-identically. `vike_mount::MakerMountConfig::crypto` ARMS it from the venue's fee
    /// schedule; `::outcome_token` (the Polymarket preset) cannot (the `Free`/`ProbabilityScaled`
    /// shapes are refused).
    #[serde(default)]
    pub round_trip_fee_rate: Option<f64>,
    /// Which optimal-market-making closed form prices the quotes ([`SpreadModel`]). Default
    /// [`SpreadModel::AvellanedaStoikov`] (byte-identical to the pre-GLFT path). Both models emit the
    /// SAME affine `reservation ± half-spread` quotes; [`SpreadModel::Gueant`] only swaps the
    /// coefficient on the inventory-skew / half-spread term for the horizon-free GLFT `S` (which reads
    /// [`Self::base_intensity_a`]). `#[serde(default)]` so an old payload decodes to A-S.
    #[serde(default)]
    pub spread_model: SpreadModel,
    /// Base fill-arrival intensity `A` for the [`SpreadModel::Gueant`] skew coefficient
    /// `S = √(σ̂²·γ/(2κA)·(1+γ/κ)^(1+κ/γ))` — orders per MILLISECOND, matching the per-ms `σ̂²`
    /// estimator (the units trap: `A` and `σ̂²` MUST share a time base). Larger `A` (more flow at the
    /// touch) ⇒ tighter `S`. Must be `> 0`; ignored entirely under A-S. Default
    /// [`default_base_intensity_a`] — a placeholder that MUST be calibrated to the venue's touch flow
    /// before a live GLFT mount (a future online OLS-intercept fit is the follow-up).
    #[serde(default = "default_base_intensity_a")]
    pub base_intensity_a: f64,
    /// GROUP-B reservation model (the inventory-skew RESERVATION the maker prices from).
    /// [`ReservationModel::AsLinear`] (default) = the A-S/GLFT linear `r = s − q_norm·γ·V`,
    /// byte-identical; [`ReservationModel::Lmsr`] = Hanson LMSR — walk the live mid in LOG-ODDS by net
    /// inventory over depth [`Self::lmsr_b`], `r = logistic(logit(mid) − q/b)`, so the reservation
    /// stays pinned to the market yet self-clamps to `(0,1)` and reproduces the LMSR "wall" (per-unit
    /// move `mid·(1−mid)/b`, vanishing at 0/1). BOUNDED-token only: on a `$`-scale (mid ∉ (0,1)) book
    /// the LMSR form is inert (returns the mid), so keep `AsLinear` for crypto. `#[serde(default)]` ⇒
    /// `AsLinear`.
    #[serde(default)]
    pub reservation_model: ReservationModel,
    /// LMSR liquidity depth `b` for [`ReservationModel::Lmsr`] — how fast the reservation walks per
    /// unit net inventory (larger ⇒ flatter walk, more subsidy at risk). Must be `> 0`; ignored under
    /// `AsLinear`. Default [`default_lmsr_b`].
    #[serde(default = "default_lmsr_b")]
    pub lmsr_b: f64,
    /// GROUP-B half-spread SOURCE. [`SpreadSource::AsOptimal`] (default) = the A-S/GLFT optimal
    /// half-spread `½·[γ·V + (2/γ)·ln(1+γ/κ)]`, byte-identical; [`SpreadSource::LsLmsr`] =
    /// Othman–Sandholm liquidity-sensitive overround `(Σp−1)/2` fed the probability split
    /// `(mid, 1−mid)` (widest at `p≈0.5`, tightening toward the 0/1 walls; scale-free, tuned by
    /// [`Self::ls_lmsr_alpha`]); [`SpreadSource::GlostenMilgrom`] = adverse-selection `μ̂·p(1−p)`
    /// (reads [`Self::gm_mu`]). Both alternatives are floored/capped by the existing
    /// `min/max_half_spread_ticks` so a zero read never silently collapses the quote (use a
    /// `min_half_spread_ticks` floor with GM/LS-LMSR). `#[serde(default)]` ⇒ `AsOptimal`.
    #[serde(default)]
    pub spread_source: SpreadSource,
    /// LS-LMSR spread constant `α` (vig ≈ `α·2·ln2` at the mid); ignored unless
    /// `spread_source == LsLmsr`. `0.0` (the default) ⇒ zero overround ⇒ the half-spread floor governs.
    #[serde(default)]
    pub ls_lmsr_alpha: f64,
    /// Glosten–Milgrom informed-trader fraction `μ̂ ∈ [0,1]`; ignored unless
    /// `spread_source == GlostenMilgrom`. `0.0` (the default) ⇒ zero adverse-selection spread ⇒ the
    /// floor governs. A live `μ̂` estimate (VPIN / OFI toxicity) is the follow-up feed.
    #[serde(default)]
    pub gm_mu: f64,
    /// GROUP-B short-alpha weight `β` on the top-of-book imbalance signal (Cartea–Jaimungal additive
    /// reservation drift `a·h`, `a = β·imbalance + λ·ofi`). `0.0` (the default) ⇒ the imbalance channel
    /// contributes nothing ⇒ byte-identical to the pre-Group-B reservation. Positive `β` leans the
    /// reservation into the heavier book side.
    #[serde(default)]
    pub alpha_beta_imbalance: f64,
    /// GROUP-B short-alpha weight `λ` on the Cont–Kukanov–Stoikov OFI meter (the other input to the
    /// CJ alpha `a = β·imbalance + λ·ofi`). `0.0` (the default) ⇒ the OFI channel is off AND the OFI
    /// tracker is never advanced ⇒ byte-identical + free. Positive `λ` leans the reservation with the
    /// running signed order flow.
    #[serde(default)]
    pub alpha_lambda_ofi: f64,
    /// EWMA decay `∈ [0,1)` for the [`OfiTracker`](crate) accumulator: `ofi ← ofi·decay + eₙ` per book
    /// update (`0` = memoryless latest-`eₙ`, near `1` = a long-memory sum). Read once at maker mount
    /// to seed the tracker; a live re-tune preserves the warm tracker (like σ̂²), so this is inert on a
    /// re-tune. Default [`default_ofi_decay`] (`0.9`). Only consulted when `alpha_lambda_ofi != 0`.
    #[serde(default = "default_ofi_decay")]
    pub ofi_decay: f64,
    /// GROUP-B Cartea–Jaimungal running inventory-penalty coefficient `φ` (≥ 0) — the additive
    /// reservation term `−φ·q·h` that leans the reservation to UNWIND standing inventory. `0.0` (the
    /// default) ⇒ no running penalty ⇒ byte-identical (the existing A-S `γ·V` skew is unchanged; this
    /// is an ADDITIONAL, horizon-scaled pull).
    #[serde(default)]
    pub running_penalty_phi: f64,
    /// SETTLEMENT force-flatten window (ms): be flat by this `τ = (T − t)` time-to-resolution before a
    /// binary market resolves — the [`flatten_weight`](crate) schedule ramps the target inventory
    /// toward ZERO as `τ → 0`. `0` (the default) ⇒ disabled ⇒ no force-flatten pressure, byte-
    /// identical. Requires a known `resolution_ts` to have any effect (`τ` is undefined otherwise).
    #[serde(default)]
    pub flatten_by_ms: i64,
    /// SETTLEMENT force-flatten skew strength — how hard the reservation leans toward flat at full
    /// [`flatten_weight`](crate) (`shift = −strength·flatten_w·q_norm`). `0.0` (the default) ⇒ opt-out
    /// ⇒ byte-identical, even with a `flatten_by_ms` window set.
    #[serde(default)]
    pub flatten_strength: f64,
    /// GROUP-B OFI-TOXICITY synthesis scale (PR-3): the `|OFI|` magnitude at which the INTERNALLY-
    /// synthesized flow-toxicity meter `1 − e^(−|ofi|/scale)` reaches ~0.63. When non-zero AND the
    /// `vike-mm` `SpreadMaker` has a [`ToxicityParams`] guard configured AND no EXTERNAL
    /// [`crate::Strategy::on_flow`] reading has been delivered, the maker feeds the tracked Cont–Kukanov–
    /// Stoikov OFI into its EXISTING flow-toxicity guard as a synthesized per-side reading (positive
    /// OFI = buy pressure ⇒ the ASK is the adverse side). An external `on_flow` reading always takes
    /// PRECEDENCE over the synthesized fallback. `0.0` (the default) ⇒ no internal synthesis ⇒
    /// byte-identical. NOTE the OFI tracker is only advanced while the OFI alpha channel is active
    /// (`alpha_lambda_ofi != 0`), so a live synthesized signal also requires that channel on.
    #[serde(default)]
    pub ofi_toxicity_scale: f64,
    /// GROUP-B ACCELERATED-PULL ramp window (ms) for the fill-rate breaker near a binary resolution
    /// (PR-3): the `τ = (resolution_ts − t)` window over which the per-side breaker's effective
    /// net-fill threshold is DIVIDED by an accelerating multiplier (`accelerated_pull`) so it trips
    /// SOONER as τ → 0. `0` (the default) ⇒ disabled ⇒ the breaker threshold is unchanged bit-for-bit.
    /// Requires a known `resolution_ts` to have any effect (τ is undefined otherwise).
    #[serde(default)]
    pub pull_accel_ramp_ms: i64,
    /// GROUP-B ACCELERATED-PULL max extra acceleration at `τ = 0` (PR-3): the breaker-pull multiplier
    /// tops out at `1 + pull_accel_max` at resolution, ramping down to `1.0` at the edge of the
    /// [`pull_accel_ramp_ms`](AsParams::pull_accel_ramp_ms) window (and `1.0` outside it). `0.0` (the
    /// default) ⇒ no acceleration ⇒ byte-identical, even with a `pull_accel_ramp_ms` window set.
    #[serde(default)]
    pub pull_accel_max: f64,
}

fn default_underlying_beta() -> f64 {
    1.0
}

/// Default LMSR liquidity depth `b` for [`ReservationModel::Lmsr`] — a neutral placeholder; larger
/// walks the reservation more slowly per unit inventory. Only consulted under the LMSR model.
fn default_lmsr_b() -> f64 {
    100.0
}

/// Default EWMA decay for the Group-B OFI tracker ([`AsParams::ofi_decay`]) — a moderate long-memory
/// factor. Only consulted when the OFI alpha channel (`alpha_lambda_ofi != 0`) is active.
fn default_ofi_decay() -> f64 {
    0.9
}

/// Default base fill intensity `A` for [`SpreadModel::Gueant`] (orders/ms) — a neutral placeholder;
/// see [`AsParams::base_intensity_a`]. Only consulted when the GLFT model is selected.
fn default_base_intensity_a() -> f64 {
    1.0
}

/// Which closed-form optimal-market-making solution the A-S layer prices with. Both emit the SAME
/// affine bid/ask depths `c1 + (½ ± q)·(skew coefficient)` with the SAME adverse-selection floor
/// `c1 = (1/γ)·ln(1+γ/κ)`; they differ ONLY in the skew coefficient:
/// - [`Self::AvellanedaStoikov`] — the finite-horizon `γ·V = γ·σ̂²·H` (near-`T` Taylor form).
/// - [`Self::Gueant`] — the Guéant–Lehalle–Fernandez-Tapia stationary (`T→∞`) `S`.
///
/// So GLFT is fed to the shared quote assembly as an effective variance `V_eff = S/γ`, and A-S is the
/// `V_eff = S/γ` limit for the matching `S` — the "reduces to A-S" identity is exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SpreadModel {
    /// Avellaneda–Stoikov (the default) — byte-identical to the pre-GLFT pricing.
    #[default]
    AvellanedaStoikov,
    /// Guéant–Lehalle–Fernandez-Tapia stationary closed form: horizon-free skew coefficient
    /// `S = √(σ̂²·γ/(2κA)·(1+γ/κ)^(1+κ/γ))`, read with [`AsParams::base_intensity_a`]. For UNBOUNDED
    /// (`$`-scale crypto) books — keep A-S for the Bernoulli-capped Polymarket unit-interval domain,
    /// where the horizon/settlement features (inert under GLFT) do real work.
    Gueant,
}

/// GROUP-B reservation model: which inventory-skew RESERVATION the maker prices from (orthogonal to
/// [`SpreadModel`], which sets the half-spread coefficient). Both feed the shared affine posting
/// geometry (`assemble_quote`); this only swaps how `r` responds to inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ReservationModel {
    /// Avellaneda–Stoikov / GLFT linear inventory skew `r = s − q_norm·γ·V` (the default,
    /// byte-identical to the pre-Group-B path).
    #[default]
    AsLinear,
    /// Hanson LMSR log-odds walk `r = logistic(logit(mid) − q/b)` for a bounded \[0,1\] outcome token —
    /// self-clamping to `(0,1)`, with the `mid·(1−mid)/b` wall. Reads [`AsParams::lmsr_b`]. Inert
    /// (returns the mid) off the unit interval, so it is a no-op on a `$`-scale book.
    Lmsr,
}

/// GROUP-B half-spread source: where the posted half-spread comes from (orthogonal to
/// [`ReservationModel`]). All three feed the same `half_spread` slot, then the shared
/// `min/max_half_spread_ticks` floor/cap and grid snap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SpreadSource {
    /// The Avellaneda–Stoikov / GLFT optimal half-spread `½·[γ·V + (2/γ)·ln(1+γ/κ)]` (the default,
    /// byte-identical).
    #[default]
    AsOptimal,
    /// Othman–Sandholm LS-LMSR volume-scaled overround `(Σp−1)/2`, fed the probability split
    /// `(mid, 1−mid)`: widest at `p≈0.5`, tightening toward the 0/1 walls. Reads
    /// [`AsParams::ls_lmsr_alpha`].
    LsLmsr,
    /// Glosten–Milgrom adverse-selection half-spread `μ̂·p(1−p)`-shaped, from the informed-flow mix.
    /// Reads [`AsParams::gm_mu`].
    GlostenMilgrom,
}

impl Default for AsParams {
    /// The recommended Polymarket starting configuration (every value runtime-tunable): real
    /// time-to-resolution horizon with a 1-minute blackout, the bounded `min(σ²·H, p(1−p))`
    /// variance, and a FIXED κ with the online fit available but off.
    fn default() -> Self {
        AsParams {
            gamma: 0.1,
            horizon_mode: HorizonMode::TimeToResolution,
            tau_hold_ms: 3_600_000,
            resolution_ts: None,
            resolution_blackout_ms: 60_000,
            variance_mode: VarianceMode::LocalCapped,
            kappa_mode: KappaMode::Fixed,
            kappa_default: 50.0,
            kappa_min: 1.0,
            kappa_max: 1_000.0,
            sigma_half_life: 32.0,
            trade_window_ms: 60_000,
            n_min: 20,
            q_scale: 100.0,
            min_standoff_ticks: 1.0,
            use_micro_price: false,
            terminal_penalty_gamma: 0.0,
            terminal_ramp_ms: 0,
            underlying_weight: 0.0,
            underlying_beta: 1.0,
            window_secs: 0.0,
            atm_blackout_scale: 0.0,
            // Polymarket 0–1 outcome-token domain with no half-spread floor — the pre-generalization
            // behavior, byte-identical.
            price_domain: PriceDomain::UnitInterval,
            min_half_spread_ticks: 0.0,
            max_half_spread_ticks: 0.0,
            // No break-even fee floor armed: a hand-built `AsParams` prices exactly as it always
            // did. `None` is "nobody named this venue's maker fee", NOT "the fee is zero" — the
            // MOUNT arms it (see the field doc).
            round_trip_fee_rate: None,
            // A-S by default (byte-identical); the GLFT intensity is a calibrate-before-use placeholder.
            spread_model: SpreadModel::AvellanedaStoikov,
            base_intensity_a: default_base_intensity_a(),
            // Group-B model pluggables: all default to the A-S/GLFT path (byte-identical).
            reservation_model: ReservationModel::AsLinear,
            lmsr_b: default_lmsr_b(),
            spread_source: SpreadSource::AsOptimal,
            ls_lmsr_alpha: 0.0,
            gm_mu: 0.0,
            // Group-B reservation shifts: all inert at 0 (no alpha drift, no running penalty, no
            // force-flatten) ⇒ byte-identical; the OFI decay is a placeholder read only when λ ≠ 0.
            alpha_beta_imbalance: 0.0,
            alpha_lambda_ofi: 0.0,
            ofi_decay: default_ofi_decay(),
            running_penalty_phi: 0.0,
            flatten_by_ms: 0,
            flatten_strength: 0.0,
            // Group-B PR-3 breaker/toxicity refinements: all inert at 0 ⇒ byte-identical.
            ofi_toxicity_scale: 0.0,
            pull_accel_ramp_ms: 0,
            pull_accel_max: 0.0,
        }
    }
}
