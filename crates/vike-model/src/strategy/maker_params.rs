//! The market-maker TUNING SURFACE — the `SpreadMaker` / Avellaneda-Stoikov parameter types.
//!
//! This is the DATA half of one strategy family: the knobs an operator tunes. The maker that
//! CONSUMES them lives a layer up in `vike-mm` (`SpreadMaker`), and the pricing arithmetic that
//! turns a [`QuoteStyle`] + a concrete book into `(bid_px, ask_px)` lives there with it.
//!
//! **Why the params live down here in the bottom domain layer, split from their maker:** they ride
//! the typed live-parameter payload [`crate::StrategyParams::SpreadMaker`], which is a serde field
//! of the runtime's journaled command lane (`vike_exec::lanes::Command::UpdateParams`) — a LOWER
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

#[cfg(test)]
mod ladder_param_tests {
    use super::*;

    // `is_active` is the OFF gate: 0 or 1 level ⇒ inactive (the single-quote path), 2+ ⇒ laddered.
    #[test]
    fn is_active_only_at_two_or_more_levels() {
        let base = LadderParams {
            levels: 0,
            offset_step: 1.0,
            offset_unit: LadderOffsetUnit::HalfSpread,
            size_profile: LadderSizeProfile::Linear,
            size_ratio: 1.0,
        };
        assert!(!LadderParams { levels: 0, ..base }.is_active(), "0 levels ⇒ off");
        assert!(!LadderParams { levels: 1, ..base }.is_active(), "1 level ⇒ off");
        assert!(LadderParams { levels: 2, ..base }.is_active(), "2 levels ⇒ active");
        assert!(LadderParams { levels: 5, ..base }.is_active(), "5 levels ⇒ active");
    }

    // rungs(): level 0 is ALWAYS {0.0, 1.0} (today's quote); offsets are k·step; the size profile
    // shapes the multiplier (level 0 exactly 1.0 either way), and a decaying Linear rung floors at 0.
    #[test]
    fn rungs_expand_offsets_and_size_profiles() {
        let lin = LadderParams {
            levels: 3,
            offset_step: 2.0,
            offset_unit: LadderOffsetUnit::Ticks,
            size_profile: LadderSizeProfile::Linear,
            size_ratio: 2.0,
        };
        let r = lin.rungs();
        assert_eq!(r.len(), 3, "one rung per level");
        // offsets: 0, 2, 4 (k·offset_step); Linear ratio 2 sizes: 1, 2, 3
        for &(k, off, sz) in &[(0usize, 0.0f64, 1.0f64), (1, 2.0, 2.0), (2, 4.0, 3.0)] {
            assert_eq!(r[k].offset.to_bits(), off.to_bits(), "rung {k} offset");
            assert_eq!(r[k].size.to_bits(), sz.to_bits(), "rung {k} Linear size");
        }
        // Geometric ratio 2 sizes: 1, 2, 4
        let geo = LadderParams { size_profile: LadderSizeProfile::Geometric, ..lin };
        let g = geo.rungs();
        assert_eq!(g[0].size.to_bits(), 1.0_f64.to_bits(), "level 0 is 1.0×");
        assert_eq!(g[1].size.to_bits(), 2.0_f64.to_bits(), "geometric rung 1 = ratio");
        assert_eq!(g[2].size.to_bits(), 4.0_f64.to_bits(), "geometric rung 2 = ratio^2");
        // a decaying Linear ladder floors a would-be-negative multiplier at 0.0
        let decay = LadderParams { levels: 4, size_ratio: 0.5, ..lin };
        let d = decay.rungs();
        assert_eq!(d[0].size.to_bits(), 1.0_f64.to_bits(), "level 0 still 1.0×");
        assert_eq!(d[1].size.to_bits(), 0.5_f64.to_bits(), "rung 1 = 0.5×");
        assert_eq!(d[2].size.to_bits(), 0.0_f64.to_bits(), "rung 2 would be 0 → floored 0");
        assert_eq!(d[3].size.to_bits(), 0.0_f64.to_bits(), "rung 3 would be −0.5 → floored 0");
        // an off/1-level bag still yields exactly one rung (today's quote)
        assert_eq!(LadderParams { levels: 0, ..lin }.rungs().len(), 1, "0 levels ⇒ 1 rung");
        assert_eq!(LadderParams { levels: 1, ..lin }.rungs().len(), 1, "1 level ⇒ 1 rung");
    }
}

#[cfg(test)]
mod toxicity_param_tests {
    use super::*;

    // The all-zero bag is the OFF default — both knobs `0.0`.
    #[test]
    fn default_is_the_all_zero_off_bag() {
        let d = ToxicityParams::default();
        assert_eq!(d.widen.to_bits(), 0.0_f64.to_bits(), "widen defaults to 0.0");
        assert_eq!(d.size_cut.to_bits(), 0.0_f64.to_bits(), "size_cut defaults to 0.0");
        assert_eq!(d, ToxicityParams { widen: 0.0, size_cut: 0.0 }, "the OFF bag");
    }

    // A minimal SpreadMakerParams with `toxicity` set to `t` — every OTHER sub-bag left OFF/None so
    // the test isolates the toxicity serde contract.
    fn params_with_toxicity(t: Option<ToxicityParams>) -> SpreadMakerParams {
        SpreadMakerParams {
            qty: 1.0,
            half_spread: 0.5,
            target_inventory: 0.0,
            max_inventory: 1.0,
            skew: 0.0,
            fill_window_ms: 0,
            net_fill_threshold: 0.0,
            suppress_cooldown_ms: 0,
            style: QuoteStyle::Mid,
            depth_levels: 1,
            tick_size: 0.0,
            filter_own: false,
            avellaneda_stoikov: None,
            refresh_tolerance: None,
            ladder: None,
            reward: None,
            toxicity: t,
        }
    }

    // Backward-compat: a payload that OMITS `toxicity` decodes to `toxicity: None` (the additive
    // `#[serde(default)]` contract, so old journals / GUI payloads still decode).
    #[test]
    fn omitted_toxicity_decodes_to_none() {
        let json = serde_json::json!({
            "qty": 1.0, "half_spread": 0.5, "target_inventory": 0.0, "max_inventory": 1.0,
            "skew": 0.0, "fill_window_ms": 0, "net_fill_threshold": 0.0, "suppress_cooldown_ms": 0,
            "style": "Mid", "depth_levels": 1, "tick_size": 0.0, "filter_own": false
        });
        let p: SpreadMakerParams = serde_json::from_value(json).unwrap();
        assert_eq!(p.toxicity, None, "an absent toxicity key decodes as None");
    }

    // OFF (`None`) serializes WITHOUT a `toxicity` key (the `skip_serializing_if` contract keeps an
    // OFF maker's payload byte-identical to its pre-feature bytes).
    #[test]
    fn off_toxicity_is_skipped_on_serialize() {
        let js = serde_json::to_string(&params_with_toxicity(None)).unwrap();
        assert!(!js.contains("toxicity"), "None omits the toxicity key entirely: {js}");
    }

    // A `Some(ToxicityParams{..})` bag round-trips through JSON unchanged (and DOES emit the key).
    #[test]
    fn some_toxicity_round_trips() {
        let p = params_with_toxicity(Some(ToxicityParams { widen: 1.5, size_cut: 0.8 }));
        let js = serde_json::to_string(&p).unwrap();
        assert!(js.contains("toxicity"), "an ON bag emits the key: {js}");
        let back: SpreadMakerParams = serde_json::from_str(&js).unwrap();
        assert_eq!(back.toxicity, Some(ToxicityParams { widen: 1.5, size_cut: 0.8 }));
    }
}

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
    /// price byte-identically. `vike_run::MakerMountConfig::crypto` ARMS it from the venue's fee
    /// schedule; `::polymarket` cannot (the `Free`/`ProbabilityScaled` shapes are refused).
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
    /// Hanson LMSR log-odds walk `r = logistic(logit(mid) − q/b)` for a bounded [0,1] outcome token —
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

#[cfg(test)]
mod as_params_price_domain_tests {
    use super::*;

    // The generalization default: the domain is UnitInterval (Polymarket 0–1) with a zero
    // half-spread floor, so a fresh A-S maker prices in the exact domain it always did.
    #[test]
    fn default_domain_is_the_unit_interval_with_no_floor() {
        assert_eq!(PriceDomain::default(), PriceDomain::UnitInterval);
        let p = AsParams::default();
        assert_eq!(p.price_domain, PriceDomain::UnitInterval, "default domain = Polymarket [0,1]");
        assert_eq!(p.min_half_spread_ticks.to_bits(), 0.0_f64.to_bits(), "default floor = 0 (off)");
    }

    // Backward-compat: an AsParams blob that OMITS `price_domain`/`min_half_spread_ticks` (an old
    // journal / GUI payload predating the crypto generalization) decodes to the byte-identical
    // defaults — the additive `#[serde(default)]` contract the other A-S knobs
    // (terminal/underlying/atm) keep. Every value here matches `AsParams::default`, so the decoded
    // struct must EQUAL it.
    #[test]
    fn omitted_domain_fields_decode_to_the_byte_identical_defaults() {
        let json = serde_json::json!({
            "gamma": 0.1, "horizon_mode": "TimeToResolution", "tau_hold_ms": 3_600_000,
            "resolution_ts": null, "resolution_blackout_ms": 60_000, "variance_mode": "LocalCapped",
            "kappa_mode": "Fixed", "kappa_default": 50.0, "kappa_min": 1.0, "kappa_max": 1_000.0,
            "sigma_half_life": 32.0, "trade_window_ms": 60_000, "n_min": 20, "q_scale": 100.0,
            "min_standoff_ticks": 1.0, "use_micro_price": false
        });
        let p: AsParams = serde_json::from_value(json).unwrap();
        assert_eq!(p.price_domain, PriceDomain::UnitInterval, "absent price_domain ⇒ UnitInterval");
        assert_eq!(p.min_half_spread_ticks.to_bits(), 0.0_f64.to_bits(), "absent floor ⇒ 0.0");
        assert_eq!(
            p.max_half_spread_ticks.to_bits(),
            0.0_f64.to_bits(),
            "absent ceiling ⇒ 0.0 (uncapped)"
        );
        assert_eq!(p, AsParams::default(), "the omitted-field payload equals the default AsParams");
    }

    // The Unbounded / Band domains round-trip through JSON unchanged (Band carries its f64 walls),
    // and a full AsParams tuned for the crypto domain (Unbounded + a 2-tick floor) round-trips too.
    #[test]
    fn domain_variants_round_trip() {
        for d in [
            PriceDomain::UnitInterval,
            PriceDomain::Unbounded,
            PriceDomain::Band { lo: 64_000.0, hi: 65_000.0 },
        ] {
            let js = serde_json::to_string(&d).unwrap();
            assert_eq!(serde_json::from_str::<PriceDomain>(&js).unwrap(), d, "round-trip: {js}");
        }
        let p = AsParams {
            price_domain: PriceDomain::Unbounded,
            min_half_spread_ticks: 2.0,
            ..AsParams::default()
        };
        let back: AsParams = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p, "AsParams with the crypto domain round-trips");
    }
}
