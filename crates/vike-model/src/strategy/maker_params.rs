//! The market-maker TUNING SURFACE — the `SpreadMaker` / Avellaneda-Stoikov parameter types.
//!
//! This is the DATA half of one strategy family: the knobs an operator tunes. The maker that
//! CONSUMES them lives a layer up in `vike-mm` (`SpreadMaker`), and the pricing arithmetic that
//! turns a [`QuoteStyle`] + a concrete book into `(bid_px, ask_px)` lives there with it.
//!
//! **Why the params live down here in the bottom domain layer, split from their maker:** they ride
//! the typed live-parameter payload [`crate::StrategyParams::SpreadMaker`], which is a serde field
//! of the runtime's journaled command lane (`vike_exec::Command::UpdateParams`) — a LOWER
//! layer than any strategy. The lane must be able to name them, so they cannot move up to the
//! maker. That is the same wire-schema pin that keeps `OrderRequest` in this crate.
//!
//! **Why they are split from [`super`]:** the parent module is the universal strategy SEAM (the
//! `Broker`/`Strategy` traits every strategy on both stacks is written against). This file is one
//! family's configuration. They changed in the same file only because they shared the wire-payload
//! constraint above, not because they are one concept — and the seam is the load-bearing
//! backtest=live contract, so a diff that touches it should read differently from a diff that
//! re-tunes a maker.
//!
//! Every type here is re-exported from [`super`] (and from the crate root), so this split moved no
//! public path. Net-new Rust surface — no Python twin.

mod as_params;

pub use as_params::{
    AsParams, HorizonMode, KappaMode, PriceDomain, ReservationModel, SpreadModel, SpreadSource,
    VarianceMode,
};

/// How a two-sided market maker derives its two quote PRICES off the (own-filtered) book — a small
/// registry of PURE styles. `Mid` is the default. This is the DATA half (the selectable style + its
/// docs); the pricing arithmetic that turns a style + a concrete order book into `(bid_px, ask_px)`
/// lives with the maker in `vike-core` (it needs that crate's private book view). It lives HERE in
/// the bottom domain layer so it can ride the typed live-params payload ([`SpreadMakerParams`]),
/// which the runtime's command lane (a lower layer than the strategy) must be able to name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum QuoteStyle {
    /// One tick IN FRONT of the best (gain queue priority): `bid = best_bid + tick`,
    /// `ask = best_ask − tick`. Needs the venue tick grid (the book's `tick_size`).
    Top,
    /// AT the current best (join the resting queue): `bid = best_bid`, `ask = best_ask`.
    Join,
    /// Midpoint ± `half_spread` — the ORIGINAL SpreadMaker behavior, hence the default.
    #[default]
    Mid,
    /// `depth_levels` levels INTO the book (rest deeper / more passive): `bid = bids[n].price`,
    /// `ask = asks[n].price`, clamped to the deepest available level (so on a 1-level L1 view it
    /// collapses to the touch).
    Depth,
}

/// The live tunables of the `vike-core` `SpreadMaker`, as a flat typed bag — the payload of a
/// [`crate::StrategyParams::SpreadMaker`] live-params update AND the shape the maker reports its current
/// settings as. Every field mirrors a `SpreadMaker` knob 1:1 (see that type's field docs); the
/// maker hot-swaps ALL of them atomically on an update. Kept here (bottom layer) so the runtime's
/// command lane can carry it as a typed value rather than a stringly-typed blob. `f64` fields ⇒
/// `PartialEq` (no `Eq`).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpreadMakerParams {
    /// Base quote size per side (size at flat/neutral inventory).
    pub qty: f64,
    /// Half the quoted spread (used by [`QuoteStyle::Mid`]).
    pub half_spread: f64,
    /// Inventory the skew biases toward (`0.0` = stay flat).
    pub target_inventory: f64,
    /// Inventory band the skew ramps over (`> 0` to engage; `<= 0` disables skew).
    pub max_inventory: f64,
    /// Skew intensity in `[0, 1]` (`0.0` disables skew → fixed-size behavior).
    pub skew: f64,
    /// Fill-rate breaker window (epoch-ms); `0` disables the breaker.
    pub fill_window_ms: i64,
    /// Net one-directional fill SIZE that trips per-side suppression; `<= 0.0` disables the breaker.
    pub net_fill_threshold: f64,
    /// How long (epoch-ms) a tripped side stays suppressed; `0` disables the breaker.
    pub suppress_cooldown_ms: i64,
    /// Which [`QuoteStyle`] prices the two quotes.
    pub style: QuoteStyle,
    /// Levels into the book [`QuoteStyle::Depth`] rests.
    pub depth_levels: usize,
    /// Venue price grid for [`QuoteStyle::Top`] / L1 own-order filtration (`0.0` = unknown).
    pub tick_size: f64,
    /// Subtract our own resting quotes from the public book before pricing.
    pub filter_own: bool,
    /// Avellaneda–Stoikov quoting knobs, or `None` (the default) to leave A-S OFF — the maker then
    /// prices off the fixed [`QuoteStyle`] `half_spread` exactly as before, byte-identical. Additive
    /// `#[serde(default)]` so old journals / GUI payloads that predate A-S decode as `None`, mirroring
    /// the additive-serde contract the other live-params fields keep.
    #[serde(default)]
    pub avellaneda_stoikov: Option<AsParams>,
    /// Order-refresh TOLERANCE — the anti-churn gate on re-quoting an already-resting side (see
    /// [`RefreshTolerance`]). `None` (the default) ⇒ OFF: every quote/book tick re-issues a modify
    /// for BOTH sides exactly as before, byte-identical. Additive
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` so old journals / GUI payloads
    /// that predate the knob decode as `None` AND an OFF maker's serialized payload keeps its
    /// pre-feature bytes (no state-hash / fixture churn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_tolerance: Option<RefreshTolerance>,
    /// LADDER quoting — rest N rungs per side stepping out from the reservation price instead of a
    /// single quote (see [`LadderParams`]). `None` (the default), and any bag with `levels <= 1`,
    /// ⇒ OFF: the maker rests exactly today's one order per side tagged `"bid"`/`"ask"`,
    /// byte-identical. Additive `#[serde(default, skip_serializing_if = "Option::is_none")]` so old
    /// journals / GUI payloads that predate the knob decode as `None` AND an OFF maker's serialized
    /// payload keeps its pre-feature bytes (no state-hash / fixture churn), the same contract the
    /// `avellaneda_stoikov` / `refresh_tolerance` sub-bags keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder: Option<LadderParams>,
    /// LIQUIDITY-REWARDS-aware quoting knobs (steal/mm-rewards-quoting) — fold a reward-EV term into
    /// the quote so it stays in a venue's reward band at `min_size` on both sides (see
    /// [`RewardParams`]). `None` (the default), and any `weight <= 0`, ⇒ OFF: the quote is
    /// byte-identical to before. Additive `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// so old journals / GUI payloads that predate the knob decode as `None` AND an OFF maker's
    /// serialized payload keeps its pre-feature bytes (no state-hash / fixture churn) — the same
    /// contract the `avellaneda_stoikov` / `refresh_tolerance` / `ladder` sub-bags keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward: Option<RewardParams>,
    /// FLOW-TOXICITY guard knobs (RTDS wallet-toxicity) — fold a per-side widen + size-cut into the
    /// quote driven by the [`crate::Strategy::on_flow`] toxicity reading (see [`ToxicityParams`]). `None`
    /// (the default), and any bag with both knobs `0.0`, ⇒ OFF: the quote is byte-identical to
    /// before. Additive `#[serde(default, skip_serializing_if = "Option::is_none")]` so old journals /
    /// GUI payloads that predate the knob decode as `None` AND an OFF maker's serialized payload keeps
    /// its pre-feature bytes (no state-hash / fixture churn) — the same contract the `avellaneda_stoikov`
    /// / `refresh_tolerance` / `ladder` / `reward` sub-bags keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toxicity: Option<ToxicityParams>,
}

/// Order-refresh TOLERANCE — the market maker's anti-churn gate on RE-QUOTING an order that is
/// already resting, nested as `Option<RefreshTolerance>` inside [`SpreadMakerParams`] (`None`, the
/// default, = OFF ⇒ today's per-tick re-price, byte-identical). Without it a maker emits two venue
/// modifies per book delta forever — on a rate-limited CLOB reached over a long-haul link that is
/// the maker's biggest wire cost, and it needlessly risks queue position wherever a venue treats a
/// modify as a re-place.
///
/// Both thresholds are RELATIVE, in BASIS POINTS (1 bp = 1e-4) of what is CURRENTLY RESTING —  the
/// one unit that reads sanely on BOTH a 0..1-priced prediction market (a 0.41 quote drifting to
/// 0.4101 is ~2.4 bp) and a five-figure crypto book (100_000 → 100_001 is 0.1 bp) without the
/// strategy knowing either venue's tick grid. A side is left ALONE only when BOTH axes are inside
/// tolerance; anything larger re-prices exactly as before. `0.0` on an axis means "no tolerance on
/// this axis": only an EXACTLY unchanged value passes it, so a price-only tolerance still re-issues
/// on any size change.
///
/// SAFETY CONTRACT: the gate is consulted ONLY on the re-price (modify) path. Placing a new quote,
/// and PULLING one (the fill-rate breaker's suppression cancel), are NEVER tolerance-gated — a
/// quote that must come off the book always comes off, whatever the tolerance is set to. A FILL
/// likewise invalidates that side's resting snapshot, so a partial fill's size top-up is always
/// re-issued (the snapshot is the maker's INTENDED quote, not the smaller venue-side remainder).
///
/// TUNING vs THE MAKER'S EDGE — keep `price_bps` WELL BELOW the maker's `half_spread` in RELATIVE
/// terms. The tolerance is a fraction of the RESTING price, while the edge the maker is being paid
/// for is `half_spread` away from mid; if `price_bps · 1e-4 · price` approaches or exceeds
/// `half_spread`, then a market move that stays inside the tolerance — and therefore sends nothing —
/// can leave the resting quote AT or THROUGH the new mid, i.e. quoting at zero or negative edge and
/// inviting exactly the adverse fill the spread exists to price. A useful ceiling is a small
/// fraction (say a tenth) of `half_spread / price · 1e4` bp; past that the gate is no longer only
/// suppressing churn, it is silently widening the maker's fill risk.
///
/// REWARD-BEARING VENUES: on venues that pay liquidity rewards the tolerance also protects reward
/// ELIGIBILITY, not just wire cost — Polymarket's rewards program enforces a MINIMUM ORDER AGE of
/// 30 s (`moas: 30` on `GET /clob-markets/{condition_id}`), so an order re-quoted more often than
/// every 30 s scores ZERO rewards. Operators on such a venue may want the tolerance tuned wide
/// enough (within the edge ceiling above) to keep quotes resting ≥ 30 s.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RefreshTolerance {
    /// Price drift tolerance, in basis points of the RESTING price. `0.0` (the [`Default`]) ⇒ only a
    /// bit-identical price passes this axis.
    pub price_bps: f64,
    /// Size drift tolerance, in basis points of the RESTING size. `0.0` (the [`Default`]) ⇒ only a
    /// bit-identical size passes this axis.
    pub size_bps: f64,
}

/// LIQUIDITY-REWARDS-aware quoting knobs (steal/mm-rewards-quoting) — nested as `Option<RewardParams>`
/// inside [`SpreadMakerParams`] (`None`, the default, and any `weight <= 0`, = OFF ⇒ the quote is
/// byte-identical to before). When active the `vike-mm` `SpreadMaker` folds a reward-EV term into its
/// quote: it CLAMPS the candidate two-sided quote into the venue's qualifying band (each side within
/// `max_spread_cents` of the book midpoint), floors each side's size UP to `min_size` (reward
/// eligibility), and — trading reward closeness against the A-S adverse-selection cost — pulls the
/// quote `weight`-deep toward the mid (closer = more reward, more adverse selection). The A-S
/// near-resolution blackout / fill-rate breaker stay the SAFETY authority: a rewards-shaped quote
/// never quotes through a blackout to farm rewards, and a suppressed side is still pulled.
///
/// It models Polymarket's sampling-reward SHAPE — the per-order score `S(v,s) = ((v−s)/v)²` (quadratic
/// in closeness), the two-sided `min(Q_one, Q_two)` requirement, and the `moas` min-order-age floor —
/// see `vike_mm::reward` for the pure, unit-tested scoring functions. It is the STRATEGY half: a
/// Polymarket mount parses its live market config (`max_spread`, `min_size`, `min_order_age`/`moas`)
/// and feeds it here as PLAIN params, so `vike-mm` needs no venue dependency. `f64` fields ⇒
/// `PartialEq` (no `Eq`). Net-new Rust surface — no Python twin.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RewardParams {
    /// Reward-chasing intensity in `[0, 1]`. `0.0` (the [`Default`]) ⇒ OFF: the quote is byte-identical
    /// to before. `> 0` clamps the quote into the band and floors each side's size at `min_size`; a
    /// HIGHER value pulls the quote CLOSER to the mid (more reward, more adverse selection — `weight`
    /// IS the reward-vs-A-S trade-off dial). Values outside `[0, 1]` are clamped.
    pub weight: f64,
    /// The reward band half-width: `max_spread` from the midpoint in CENTS (e.g. `3.0` = ±3¢ = ±0.03
    /// on a 0..1-priced market). An order farther than this from the midpoint scores ZERO, so the
    /// maker keeps each side within it. `<= 0` ⇒ no band ⇒ OFF (nothing to clamp into).
    pub max_spread_cents: f64,
    /// Minimum order size (shares) for reward eligibility — an order smaller than this scores zero, so
    /// a reward-active side is floored UP to it. `0.0` (the [`Default`]) imposes no floor.
    pub min_size: f64,
    /// The reward program's MINIMUM ORDER AGE (moas) in ms (Polymarket ~`30_000`): an order re-quoted
    /// more often than this scores zero rewards. While reward chasing is active AND the resting quote
    /// is still safely IN-BAND, the maker will NOT re-price it until it has rested this long — so it
    /// never churns below the reward floor. Safety re-prices still fire (a mid that runs the quote
    /// out-of-band, a fill, a breaker pull). `<= 0` disables the age gate. COMPOSES with (does not
    /// replace) [`RefreshTolerance`].
    pub min_order_age_ms: i64,
}

impl Default for RewardParams {
    /// A Polymarket-shaped starting point, but OFF (`weight = 0.0`): a `Some(RewardParams::default())`
    /// is still inert until `weight` is raised above zero.
    fn default() -> Self {
        RewardParams { weight: 0.0, max_spread_cents: 3.0, min_size: 0.0, min_order_age_ms: 30_000 }
    }
}

/// FLOW-TOXICITY guard knobs (RTDS wallet-toxicity) — a flat `Copy` scalar sub-bag nested as
/// `Option<ToxicityParams>` inside [`SpreadMakerParams`] (`None`, the default, and both knobs `0.0`,
/// = OFF ⇒ the quote is byte-identical to before). When active the `vike-mm` `SpreadMaker` reads the
/// current per-side toxic-flow intensity (`tox_bid`/`tox_ask` in `[0, 1]`, set by
/// [`crate::Strategy::on_flow`]) and, on the side under toxic pressure, WIDENS the quote away from mid by
/// `tox · widen · half_spread` and CUTS its size by a factor `(1 − tox · size_cut)` (floored at `0`).
///
/// It is the STRATEGY half: a Polymarket mount classifies the wallet-attributed activity tape and
/// aggregates it into a plain-f64 [`crate::FlowToxicity`], so `vike-mm` needs no venue dependency. `f64`
/// fields ⇒ `PartialEq` (no `Eq`). `Default` is the all-zero OFF bag (both knobs `0.0`), mirroring
/// [`RefreshTolerance`]'s derived default. Net-new Rust surface — no Python twin.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToxicityParams {
    /// How far to widen the toxic side, as a fraction of the current half-spread PER UNIT toxicity:
    /// the side is pushed `tox · widen · half_spread` further from mid. `0.0` (the default value in an
    /// all-zero bag) ⇒ no widening.
    pub widen: f64,
    /// How much of the toxic side's size to remove PER UNIT toxicity: the size is scaled by
    /// `(1 − tox · size_cut).max(0.0)`. `0.0` ⇒ no size cut; `1.0` fully withdraws the side at
    /// `tox == 1`.
    pub size_cut: f64,
}

/// The UNIT a ladder rung's price offset is measured in (see [`LadderParams::offset_step`]). Kept
/// grid-agnostic so ONE tuning reads sanely across venues: `HalfSpread` (the default) needs no tick
/// grid at all, `Ticks` snaps to the venue grid the maker already quotes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum LadderOffsetUnit {
    /// Multiples of the level-0 HALF-SPREAD (the gap between the reservation price and level 0's
    /// quote). Grid-free — a `1.0` step puts rung 1 one half-spread further out than level 0, which
    /// reads identically on a 0..1 prediction market and a five-figure crypto book. The DEFAULT.
    #[default]
    HalfSpread,
    /// Absolute venue TICKS: rung `k` steps `k · offset_step` ticks out from level 0. Needs the
    /// book's positive `tick_size`; on an unknown grid (`<= 0`) the ladder collapses to level 0 only
    /// (deeper rungs would otherwise pile onto level 0's price).
    Ticks,
}

/// How a ladder's rung SIZES scale with depth, from the base (level-0) quote size. Both profiles put
/// level 0 at exactly `1.0 ×` the base size, so level 0 is always today's quote bit-for-bit; a
/// `size_ratio` of `1.0` makes every rung the base size under either profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum LadderSizeProfile {
    /// ARITHMETIC growth: `size_k = base · (1 + k·(ratio − 1))` — each rung adds `(ratio−1)×` the
    /// base size (e.g. `ratio = 2` ⇒ multipliers `1, 2, 3, 4…`). The DEFAULT. A `ratio < 1` shrinks
    /// deeper rungs; a rung whose multiplier reaches `<= 0` is dropped (never placed at zero size).
    #[default]
    Linear,
    /// GEOMETRIC growth: `size_k = base · ratio^k` (e.g. `ratio = 2` ⇒ multipliers `1, 2, 4, 8…`).
    /// A `ratio` in `(0, 1)` decays deeper rungs without ever reaching zero.
    Geometric,
}

/// One computed rung of a quoting ladder — the pure output of expanding a [`LadderParams`] via
/// [`LadderParams::rungs`]. Grid-free by construction: `offset` is still in the ladder's
/// [`LadderOffsetUnit`] and `size` is still a MULTIPLIER of the base quote size, so mapping a rung to
/// an absolute `(price, size)` (the maker's job) needs the level-0 price, the half-spread/tick scale,
/// and the base size. Level 0 is always `{ offset: 0.0, size: 1.0 }` — today's single quote. This is
/// the `Vec<LadderLevel>` a `SpreadMaker` diffs its resting orders against each tick; it is a
/// computed intermediate, never persisted (which is why it carries no serde, unlike [`LadderParams`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LadderLevel {
    /// Price offset of this rung BEYOND level 0, in the ladder's [`LadderOffsetUnit`] (`0.0` at
    /// level 0, `k · offset_step` at rung `k`). Applied AWAY from the reservation: a bid rung sits
    /// this far BELOW the level-0 bid, an ask rung this far ABOVE the level-0 ask.
    pub offset: f64,
    /// Size of this rung as a MULTIPLIER of the base (level-0) quote size (`1.0` at level 0), so the
    /// inventory skew that shapes the base size flows through to every rung. Floored at `0.0`.
    pub size: f64,
}

/// LADDER quoting knobs — a flat `Copy` scalar sub-bag nested as `Option<LadderParams>` inside
/// [`SpreadMakerParams`] (`None` = a single quote per side, the default). When active
/// ([`levels`](LadderParams::levels) `>= 2`) the `SpreadMaker` rests N rungs per side (tags
/// `"bid0".."bidN"` / `"ask0".."askN"`) stepping out from the same reservation price + half-spread
/// its single quote uses: rung 0 IS today's `(bid, ask)` quote, rung `k` sits `k · offset_step`
/// further out (in [`LadderOffsetUnit`]) at a [`LadderSizeProfile`]-shaped size.
///
/// It is a PROCEDURAL scalar bag (not a `Vec<LadderLevel>`) on purpose: [`SpreadMakerParams`] is
/// `Copy` so it can ride the typed live-params payload cheaply, and a `Vec` would forfeit that. The
/// rungs are generated on demand by [`LadderParams::rungs`], which is the `Vec<LadderLevel>` the
/// maker actually diffs against. Net-new Rust surface — no Python twin.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LadderParams {
    /// Total rungs per side, INCLUDING level 0 (today's single quote). `<= 1` ⇒ the ladder is OFF
    /// and the maker rests exactly today's one order per side (`"bid"`/`"ask"`), byte-identical — so
    /// an empty/1-level ladder reproduces the pre-feature verbs.
    pub levels: usize,
    /// Per-rung price step OUTWARD from level 0, in [`offset_unit`](LadderParams::offset_unit). Rung
    /// `k` sits `k · offset_step` beyond level 0 (further from the reservation price). A non-positive
    /// step (or an unusable scale — a `Ticks` unit with no grid, or a zero half-spread) collapses the
    /// ladder to level 0 only, so rungs never pile onto one price.
    pub offset_step: f64,
    /// The unit [`offset_step`](LadderParams::offset_step) is measured in. `#[serde(default)]`
    /// ([`LadderOffsetUnit::HalfSpread`]) so a payload predating the field still decodes.
    #[serde(default)]
    pub offset_unit: LadderOffsetUnit,
    /// How rung sizes scale with depth. `#[serde(default)]` ([`LadderSizeProfile::Linear`]) so a
    /// payload predating the field still decodes.
    #[serde(default)]
    pub size_profile: LadderSizeProfile,
    /// The [`LadderSizeProfile`] ratio: `1.0` ⇒ every rung is the base size; `> 1` grows deeper
    /// rungs, `< 1` shrinks them (see the profile variants for the exact per-rung multiplier).
    pub size_ratio: f64,
}

impl LadderParams {
    /// Whether this ladder actually quotes more than one rung per side. `false` for `levels <= 1`,
    /// which the maker treats identically to `None` — the single-quote path, byte-identical to
    /// before the feature.
    pub fn is_active(&self) -> bool {
        self.levels >= 2
    }

    /// Expand this spec into its per-side rungs (the `Vec<LadderLevel>` the maker diffs against),
    /// deepest-last. PURE and grid-free: `offset` stays in [`LadderOffsetUnit`] and `size` stays a
    /// base-size multiplier — the maker maps them to absolute prices/sizes with the tick's level-0
    /// context. Always at least one rung (level 0 = `{ offset: 0.0, size: 1.0 }`), so an off/1-level
    /// bag still yields today's single quote. Rung multipliers are floored at `0.0`.
    pub fn rungs(&self) -> Vec<LadderLevel> {
        let n = self.levels.max(1);
        (0..n)
            .map(|k| LadderLevel {
                offset: k as f64 * self.offset_step,
                size: ladder_size_mult(self.size_profile, self.size_ratio, k),
            })
            .collect()
    }
}

/// The base-size MULTIPLIER for rung `k` under a [`LadderSizeProfile`] (`1.0` at `k = 0` for either
/// profile), floored at `0.0` so a decaying `Linear` ladder never yields a negative size. Naïve
/// folds (no `mul_add`), matching the crate's arithmetic convention.
///
/// ⚠ **`Geometric` goes through `libm::pow`, not `f64::powi`, and it is the ONE site in this
/// workspace's `powi` census where that substitution was needed.** Measured 2026-08-26 over 500
/// ratios × 12 rungs: `ratio.powi(k)` hashes `e77cac46a86678b1` on Windows/MSVC `dev` and
/// `c42a98c72a627fca` on Linux/glibc `dev`. Two boxes running one ladder therefore sized its rungs
/// differently, and this function's output is an ORDER QUANTITY.
///
/// The census's other six production `powi` sites are all `10f64.powi(n)` — a LITERAL power of ten
/// with a runtime exponent — and those are exact on both platforms (measured: zero mismatches
/// against the true power of ten across `n` in `-22..=22`, because `10ⁿ` is exactly representable
/// in f64 and `10⁻ⁿ` is one correctly-rounded divide of an exact numerator). So the classification
/// that matters is the BASE, not the exponent: a literal power of ten is portable, an arbitrary
/// runtime `f64` is not. `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`
/// carries the table.
///
/// ⚠ This CHANGES the geometric ladder's numbers on both platforms — the `libm` crate agrees with
/// neither MSVC nor glibc — which 0032 states as the price of reproducibility rather than denying.
/// `Linear` is untouched: it is `+`, `-` and `*` only, all correctly rounded by IEEE 754.
fn ladder_size_mult(profile: LadderSizeProfile, ratio: f64, k: usize) -> f64 {
    let m = match profile {
        LadderSizeProfile::Linear => 1.0 + (k as f64) * (ratio - 1.0),
        LadderSizeProfile::Geometric => libm::pow(ratio, k as f64),
    };
    m.max(0.0)
}

#[path = "ladder_param_tests.rs"]
#[cfg(test)]
mod ladder_param_tests;

#[path = "toxicity_param_tests.rs"]
#[cfg(test)]
mod toxicity_param_tests;

#[path = "as_params_price_domain_tests.rs"]
#[cfg(test)]
mod as_params_price_domain_tests;
