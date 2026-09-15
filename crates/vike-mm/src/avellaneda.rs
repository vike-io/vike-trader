//! Avellaneda–Stoikov pricing layer (audit mm-quote) — split out of the crate root; behavior is
//! byte-identical (whole items moved verbatim).
//!
//! A-S here is net-new Rust with NO Python twin (like `SpreadMaker` itself), so "parity" means
//! self-consistent reference-value pinning + neutral-reduction, not oracle bits. The math is
//! decomposed into pure `f64` free functions (naïve folds, no `mul_add`, per the crate rule) so each
//! is unit-tested in isolation exactly like `skew_multipliers` / `net_signed_fills` / `priced`. The
//! closed forms (A-S 2008), adapted to Polymarket's 0–1 bounded prices (§2 of the mm-quote spec):
//!
//!   reservation price   r      = s − q_norm · γ · V
//!   optimal half-spread δ_half = ½·γ·V + (1/γ)·ln(1 + γ/κ)      [ = ½·(γV + (2/γ)ln(1+γ/κ)) ]
//!   effective variance  V      = min(σ̂²·H, p·(1−p))            [ the 0–1 bounded-variance crux ]
//!
//! where `s` is the fair mid, `q_norm` the size-normalised signed inventory, `γ` risk aversion, `κ`
//! the fill-intensity decay, `H = min(τ_hold, T−t)` the effective horizon, and `p = s` the current
//! probability. The Bernoulli cap `p(1−p)` makes skew and spread vanish at the 0/1 walls (V→0) and
//! peak at p=0.5 — the defensible adaptation that keeps A-S from over-penalising inventory near a wall.
//!
//! ## Price-domain generalization (the crypto lift)
//! The 0–1 geometry above is now ONE case of a [`PriceDomain`] knob ([`domain_bounds`]): the wall
//! clamp `[tick, 1−tick]` is [`PriceDomain::UnitInterval`] (Polymarket, the default), while a
//! `$`-scale asset (a BTC perp mid ~$64k) runs [`PriceDomain::Unbounded`] — NO wall clamp, so a quote
//! is bounded only by the standoff + the `bid < s < ask` straddle. `UnitInterval` resolves to the
//! same `(tick, 1−tick)` bounds the code hard-coded before, so Polymarket pricing is byte-identical;
//! the caller ([`AsState::price`]) resolves the domain and threads the bounds into [`as_quotes`] /
//! [`widest_quote`]. Two pairings are load-bearing for a `$`-scale mount: [`VarianceMode::RawLocal`]
//! (the Bernoulli cap goes negative for `p > 1`) and an [`AsParams::min_half_spread_ticks`] floor (the
//! adverse-selection half-spread is sub-tick at `$`-scale, so without it bid/ask snap to one tick).

use std::collections::VecDeque;

use vike_model::{
    AsParams, HorizonMode, KappaMode, PriceDomain, ReservationModel, SpreadModel, SpreadSource,
    TradeTick, VarianceMode,
};

use crate::book::BookView;
use crate::fairvalue::lmsr_reservation;
use crate::fits::{fit_base_intensity, fit_kappa, fit_kappa_censored, shrink_kappa};
use crate::spread_source::{glosten_milgrom_half_spread, ls_lmsr_half_spread};

/// EWMA smoothing factor from a half-life in updates: `α = 1 − 0.5^(1/half_life)` (a weight decays
/// to ½ after `half_life` updates). `half_life <= 0` ⇒ `α = 1.0` (no smoothing; σ̂² tracks the latest
/// instantaneous sample). The recurrence it feeds mirrors vike-indicators' `ema` alpha recurrence.
pub(crate) fn ewma_alpha(half_life: f64) -> f64 {
    if half_life > 0.0 { 1.0 - libm::pow(0.5, 1.0 / half_life) } else { 1.0 }
}

/// One EWMA update of the per-ms mid variance σ̂² (spec §3): `inst = Δ²/Δt`, then
/// `σ̂² ← α·inst + (1−α)·prev`. `Δt <= 0` returns `prev` unchanged (the caller guards, but this is
/// self-contained too). Naïve folds, no `mul_add`.
pub(crate) fn update_sigma2(prev: f64, delta_s: f64, dt_ms: f64, alpha: f64) -> f64 {
    if dt_ms <= 0.0 {
        return prev;
    }
    let inst = (delta_s * delta_s) / dt_ms;
    alpha * inst + (1.0 - alpha) * prev
}

/// Effective price-move variance `V` to the horizon (spec §2.1), per [`VarianceMode`]:
/// `sigma2_inst` is the per-ms σ̂² estimate, `horizon_ms` the effective horizon `H` (ms), `p` the
/// current fair probability. Clamped `≥ 0` (defensive: a `p` outside `(0,1)` fed to a capped mode
/// cannot drive `V` negative). `p → 0`/`p → 1` ⇒ Bernoulli `p(1−p) → 0` ⇒ the capped/pure modes
/// give `V == 0` bit-for-bit.
pub(crate) fn bounded_variance(
    sigma2_inst: f64,
    horizon_ms: f64,
    p: f64,
    mode: VarianceMode,
) -> f64 {
    let local = sigma2_inst * horizon_ms;
    let bernoulli = p * (1.0 - p);
    let v = match mode {
        VarianceMode::LocalCapped => local.min(bernoulli),
        VarianceMode::PureBernoulli => bernoulli,
        VarianceMode::RawLocal => local,
    };
    v.max(0.0)
}

/// Terminal SETTLEMENT-variance penalty (arXiv 2607.17991): the ADDITIVE variance term folded into
/// `V` near a binary resolution. Returns `γ_term · w(τ) · p(1−p)`, where the ramp weight
/// `w(τ) = clamp(1 − τ/ramp_ms, 0, 1)` is `0` far out and `→ 1` at resolution (`τ` = ms to
/// resolution). Because the settlement variance `p(1−p)` does NOT vanish as `τ → 0` — unlike the
/// diffusion term `σ̂²·H` — adding it here STRENGTHENS the reservation skew into settlement and
/// WIDENS the half-spread at `p ≈ 0.5` (the published binary-market optimum, the opposite of a
/// fixed near-resolution stance that only widens/holds). Returns `0.0` (byte-identical) unless BOTH
/// `gamma_term > 0` AND `ramp_ms > 0`. Naïve left-to-right fold; `p` outside `[0,1]` can't drive it
/// negative (the `.max(0.0)` guard mirrors [`bounded_variance`]).
pub(crate) fn terminal_var(
    gamma_term: f64,
    ramp_ms: i64,
    time_to_resolution_ms: f64,
    p: f64,
) -> f64 {
    if gamma_term <= 0.0 || ramp_ms <= 0 {
        return 0.0;
    }
    let w = (1.0 - time_to_resolution_ms / ramp_ms as f64).clamp(0.0, 1.0);
    let settlement = (p * (1.0 - p)).max(0.0);
    gamma_term * w * settlement
}

/// A-S reservation price (spec §2.1): `r = s − q_norm·γ·V`. Naïve left-to-right fold, so `q_norm ==
/// 0` yields `r == s` bit-for-bit, and `r` is monotone non-increasing in `q_norm` (for `γ,V ≥ 0`).
pub(crate) fn as_reservation_price(s: f64, q_norm: f64, gamma: f64, v: f64) -> f64 {
    s - q_norm * gamma * v
}

/// The vol component of the A-S half-spread: `½·γ·V`. Vanishes with `V` (bit-exact `0` at the walls).
pub(crate) fn as_vol_halfspread(gamma: f64, v: f64) -> f64 {
    0.5 * gamma * v
}

/// The adverse-selection FLOOR of the A-S half-spread: `(1/γ)·ln(1 + γ/κ)`. What the half-spread
/// relaxes to when `V → 0` (near-certain markets). Needs `γ > 0`, `κ > 0`.
pub(crate) fn as_intensity_halfspread(gamma: f64, kappa: f64) -> f64 {
    (1.0 / gamma) * libm::log(1.0 + gamma / kappa)
}

/// A-S optimal half-spread (spec §2.1): the vol component `½·γ·V` plus the intensity floor
/// `(1/γ)·ln(1+γ/κ)` — algebraically the textbook `½·[γ·V + (2/γ)·ln(1+γ/κ)]`, decomposed so the
/// floor is a clean bit-anchor when `V == 0`. Increasing in `γ` and `V`, decreasing in `κ`.
pub(crate) fn as_optimal_half_spread(gamma: f64, v: f64, kappa: f64) -> f64 {
    as_vol_halfspread(gamma, v) + as_intensity_halfspread(gamma, kappa)
}

/// The Guéant–Lehalle–Fernandez-Tapia stationary skew/half-spread coefficient `S` (arXiv 1105.3115;
/// the `T → ∞` closed form): `S = √( σ̂²·γ / (2·κ·A) · (1 + γ/κ)^(1 + κ/γ) )`, where `A` is the base
/// fill-arrival intensity at the touch (per ms, matching the per-ms `σ̂²` — the units MUST agree).
/// Unlike the A-S `γ·V = γ·σ̂²·H`, `S` is HORIZON-FREE (no `H`): the GLFT solution is stationary. Fed
/// to [`as_quotes`] as an effective variance `V_eff = S/γ` (see [`effective_variance`]), which makes
/// the posted depths `c1 + (½ ± q)·S` — the SAME affine form A-S emits with `γ·V`. Needs `γ,κ,A,σ̂² >
/// 0`; any non-positive input ⇒ `0.0` (the caller then floors the half-spread at the intensity term
/// `c1`, so the maker still quotes). Naïve folds, no `mul_add`.
pub(crate) fn gueant_skew_coeff(sigma2: f64, gamma: f64, kappa: f64, a: f64) -> f64 {
    if gamma <= 0.0 || kappa <= 0.0 || a <= 0.0 || sigma2 <= 0.0 {
        return 0.0;
    }
    let ratio = gamma / kappa;
    (sigma2 * gamma / (2.0 * kappa * a) * libm::pow(1.0 + ratio, 1.0 + kappa / gamma)).sqrt()
}

/// The effective variance `V` fed to [`as_quotes`], per [`SpreadModel`]:
/// - [`SpreadModel::AvellanedaStoikov`] → the bounded diffusion variance `v_as` unchanged (byte-
///   identical to the pre-GLFT path).
/// - [`SpreadModel::Gueant`] → the GLFT coefficient converted to `V_eff = S/γ`, so the shared quote
///   assembly reproduces the GLFT depths `c1 + (½ ± q)·S`. The A-S `v_as`, the horizon `H`, and the
///   settlement/terminal term are all IGNORED here (GLFT is horizon-free) — they fold into `v_as`
///   which this arm discards. `γ ≤ 0` ⇒ fall back to `v_as` (defensive; the caller guards `γ > 0`).
pub(crate) fn effective_variance(
    model: SpreadModel,
    v_as: f64,
    sigma2: f64,
    gamma: f64,
    kappa: f64,
    a: f64,
) -> f64 {
    match model {
        SpreadModel::AvellanedaStoikov => v_as,
        SpreadModel::Gueant if gamma > 0.0 => gueant_skew_coeff(sigma2, gamma, kappa, a) / gamma,
        SpreadModel::Gueant => v_as,
    }
}

/// Snap a price to the tick grid (round-half-to-even), matching `L2Book` / own-order filtration
/// quantisation so A-S output maps back onto real book levels. `tick <= 0` ⇒ identity (unknown
/// grid). The half-to-even snap itself is the shared `vike_model::round_to_step` primitive.
pub(crate) fn snap_to_tick(px: f64, tick: f64) -> f64 {
    if tick > 0.0 { vike_model::round_to_step(px, tick) } else { px }
}

/// Resolve the posted-quote price bounds `(lo, hi)` for a [`PriceDomain`] on the tick grid `tick` —
/// the seam that generalizes the A-S wall clamp off Polymarket's fixed 0–1 interval:
/// - [`PriceDomain::UnitInterval`] → `(tick, 1 − tick)`: the Polymarket outcome-token walls, computed
///   with the SAME arithmetic the layer hard-coded before the generalization, so a `UnitInterval`
///   quote is byte-identical to the pre-knob path.
/// - [`PriceDomain::Unbounded`] → `(−∞, +∞)`: no wall clamp. Feeding these to [`as_quotes`]'s
///   `bid.max(lo)` / `ask.min(hi)` is an exact no-op (`x.max(−∞) == x`, `x.min(+∞) == x`), so the
///   quote is bounded only by the standoff + the `bid < s < ask` straddle — the classic `$`-scale A-S.
/// - [`PriceDomain::Band`]`{ lo, hi }` → `(lo, hi)`: an explicit floor/ceiling.
pub(crate) fn domain_bounds(domain: PriceDomain, tick: f64) -> (f64, f64) {
    match domain {
        PriceDomain::UnitInterval => (tick, 1.0 - tick),
        PriceDomain::Unbounded => (f64::NEG_INFINITY, f64::INFINITY),
        PriceDomain::Band { lo, hi } => (lo, hi),
    }
}

/// The per-side BREAK-EVEN half-spread in price units: `½ · round_trip_fee_rate · s`, or `0.0` (no
/// floor) when the mount armed no fee rate.
///
/// A two-sided maker resting at `s ± δ` captures `2δ` on a completed round trip and pays
/// `m·P_buy + m·P_sell ≈ m·s` in fees ([`vike_model::maker_round_trip_fee`] — BOTH legs maker, both
/// on this venue). Break-even is therefore `2δ ≥ m·s`, i.e. `δ ≥ ½·m·s` per side: **one leg's rate
/// times the price, even though the bar being cleared is the whole round trip.** The `½` lives here
/// and nowhere else.
///
/// `None` (no fee armed) and a non-finite / non-positive rate or mid all yield `0.0` — which
/// [`bounded_half_spread`] reads as "no floor", byte-identical to the pre-floor path. A `Some(0.0)`
/// (a measured zero-fee maker row) also yields `0.0`: the two are numerically the same floor and
/// deliberately so — what distinguishes them is what the MOUNT was able to say, which is recorded
/// on the params and printed at startup, not re-derived here.
pub(crate) fn break_even_half_spread(round_trip_fee_rate: Option<f64>, s: f64) -> f64 {
    match round_trip_fee_rate {
        Some(m) if m.is_finite() && m > 0.0 && s.is_finite() && s > 0.0 => 0.5 * m * s,
        _ => 0.0,
    }
}

/// TRUE when NO half-spread can be both at-or-under the operator's `max_half_spread_ticks` ceiling
/// and at-or-above the break-even `fee_floor` — i.e. there is no width that is simultaneously
/// plausibly fillable and profitable, so the maker has nothing honest to post.
///
/// The one predicate, consulted by [`bounded_half_spread`] (which refuses the quote) AND by
/// [`AsState::price`] (which warns on the EDGE into and out of that state). Spelling it twice is how
/// a refusal and its explanation drift apart, which is precisely the class of defect this whole
/// change is repairing.
///
/// An UNCAPPED maker (`max_half_spread_ticks == 0.0`, the default) is never in this state: with no
/// ceiling, the fee floor simply widens the quote and there is nothing to refuse.
pub(crate) fn fee_floor_exceeds_cap(fee_floor: f64, max_half_spread_ticks: f64, tick: f64) -> bool {
    fee_floor > 0.0 && max_half_spread_ticks > 0.0 && fee_floor > max_half_spread_ticks * tick
}

/// THE WIDTH LADDER — the single authority on how a raw model half-spread becomes the posted one,
/// shared by [`as_quotes`] (the A-S/GLFT fast path) and [`AsState::price`]'s Group-B override path,
/// which used to spell it twice. Three stages, in this order, and the ORDER is the whole design:
///
/// 1. **`min_half_spread_ticks` floor** — the sub-tick guard. Structural: without it `r ± δ` snaps
///    both sides onto one tick at `$`-scale and the two-sided quote collapses to `None`.
/// 2. **`max_half_spread_ticks` cap** — the spike guard, re-floored at stage 1 (`max(min, cap)`), so
///    a `max < min` misconfig can never sacrifice the two-sided quote. Unchanged.
/// 3. **the break-even `fee_floor`** ([`break_even_half_spread`]) — and this one is NOT part of the
///    `max(floor, cap)` idiom above. If it fits under the cap it simply governs; if it does NOT
///    ([`fee_floor_exceeds_cap`]) the answer is **`None`: post nothing**.
///
/// ## Why stage 3 REFUSES instead of widening past the cap, or quoting at the cap
///
/// The cap and the fee floor answer different questions. The cap answers *how wide may I go and
/// still hope to fill?*; the fee floor answers *how wide must I be to not lose money?* When the
/// floor exceeds the cap the two answers are incompatible, and that is a fact about the INSTRUMENT
/// (a book one tick wide against a 2 bp maker fee), not about the tuning — no `gamma`, no `kappa`
/// and no `tau_hold_ms` can reconcile them.
///
/// - **Quoting at the cap** is the status quo this replaces: every completed round trip is a
///   guaranteed loss, and nothing in any log, dashboard or summary distinguishes it from a working
///   maker. Measured on the CI box's bybit BTCUSDT mount: a 60-tick ceiling ⇒ ≤ 1.903 bp of capture
///   against a 4.000 bp round-trip fee, i.e. −3.94 bp per round trip, forever.
/// - **Quoting at the floor** silently overrides a ceiling the operator set. The ceiling is not
///   decoration: it exists so the maker stays fillable through a spike. A quote posted past it is
///   one the operator's own configuration says cannot fill — and it still shows up as `working: 2`
///   in every summary, so it buys the same invisibility as the loss.
/// - **Posting nothing** is the only answer that is both true and visible: it violates no operator
///   setting, it books no loss, and it makes the maker's silence attributable to a named cause
///   ([`fee_floor_exceeds_cap`], warned on the edge by [`AsState::price`]) instead of leaving an
///   operator to wonder why fills stopped.
///
/// It also needs no new contract: `None` here flows into the existing `Option<(f64, f64)>` that
/// already means *hold, do not quote* on a one-sided book, a collapsed straddle, or a wall pin. A
/// caller that handles those handles this.
///
/// ⚠ **This does not make a mount profitable, and must not be read as doing so.** It makes an
/// impossible mount refuse instead of bleed. The cure for `fee_floor_exceeds_cap` is a different
/// instrument or a different fee tier, never a wider cap.
pub(crate) fn bounded_half_spread(
    d_raw: f64,
    tick: f64,
    min_half_spread_ticks: f64,
    max_half_spread_ticks: f64,
    fee_floor: f64,
) -> Option<f64> {
    // Stage 1: floor the half-spread at `min_half_spread_ticks · tick` — the sub-tick-spread guard.
    // The default `0.0` gives `d.max(0.0)`, and the half-spread is `≥ 0`, so it is `== d` bit-for-bit.
    let min_floor = min_half_spread_ticks * tick;
    let d = d_raw.max(min_floor);
    // Stage 2: CAP it at `max_half_spread_ticks · tick` — the spike guard against the `½·γ·σ̂²·H`
    // quadratic blowing the quote off the book. `0.0` ⇒ no cap (byte-identical). The cap never wins
    // over the floor (`max(min, cap)`), so a two-sided quote is never sacrificed to the ceiling: if a
    // caller sets `max < min`, the floor governs and the ceiling is inert.
    let d = if max_half_spread_ticks > 0.0 {
        d.min(max_half_spread_ticks * tick).max(min_floor)
    } else {
        d
    };
    // Stage 3: the break-even fee floor — REFUSE rather than widen past the cap (see the doc).
    if fee_floor_exceeds_cap(fee_floor, max_half_spread_ticks, tick) {
        return None;
    }
    Some(d.max(fee_floor))
}

/// Compose the A-S quotes and enforce the bounded-price invariants (spec §2.2), GENERALIZED off the
/// fixed Polymarket 0–1 interval to caller-supplied bounds `(lo, hi)` (see [`domain_bounds`]):
/// reservation price ± half-spread, then a `standoff_ticks` minimum gap from the fair mid `s` (a heavy
/// inventory pulls `r` toward a wall but the POSTED quote never crosses `s`), a wall clamp into
/// `[lo, hi]`, and a snap to the grid. The optimal half-spread is put through the shared
/// [`bounded_half_spread`] ladder first (`min_half_spread_ticks` floor → `max_half_spread_ticks` cap
/// → the break-even `fee_floor`), which is also where the `fee_floor > cap` REFUSAL lives — read its
/// doc for the argument. Returns `None` when no valid two-sided quote fits — sides collapse, the
/// market is pinned at a wall, or no width is both under the cap and above break-even — mirroring
/// `BookView::priced` returning `None` on a one-sided book. On success `lo ≤ bid < s < ask ≤ hi`,
/// both on-grid. For [`PriceDomain::UnitInterval`] the caller passes `(lo, hi) = (tick, 1 − tick)`
/// with `min_half_spread_ticks = 0`, `max_half_spread_ticks = 0` and `fee_floor = 0.0`, so this is
/// byte-identical to the pre-generalization 0–1 path; [`PriceDomain::Unbounded`] passes `(−∞, +∞)`,
/// making the clamp an exact no-op.
#[allow(clippy::too_many_arguments)]
pub(crate) fn as_quotes(
    s: f64,
    q_norm: f64,
    gamma: f64,
    v: f64,
    kappa: f64,
    tick: f64,
    lo: f64,
    hi: f64,
    min_half_spread_ticks: f64,
    max_half_spread_ticks: f64,
    fee_floor: f64,
    standoff_ticks: f64,
) -> Option<(f64, f64)> {
    let r = as_reservation_price(s, q_norm, gamma, v);
    let d = bounded_half_spread(
        as_optimal_half_spread(gamma, v, kappa),
        tick,
        min_half_spread_ticks,
        max_half_spread_ticks,
        fee_floor,
    )?;
    assemble_quote(r, d, s, tick, lo, hi, standoff_ticks)
}

/// The affine two-sided posting geometry shared by A-S/GLFT and the Group-B model overrides. Given a
/// FINAL reservation `r` and half-spread `d`, apply the `standoff_ticks` minimum gap from the fair mid
/// `s` (a heavy inventory pulls `r` toward a wall but the posted quote never crosses `s`), the `[lo, hi]`
/// wall clamp, and the grid snap; accept only a genuine straddling two-sided quote (`lo ≤ bid < s < ask ≤
/// hi`, both on-grid), else `None`. Extracted VERBATIM from [`as_quotes`] (byte-identical — `as_quotes`
/// now delegates here), so a model that computes its OWN reservation (LMSR) or half-spread (LS-LMSR /
/// Glosten–Milgrom), plus additive alpha (Cartea–Jaimungal) / force-flatten (settlement) shifts on `r`,
/// reuses the exact posting geometry rather than re-deriving it.
pub(crate) fn assemble_quote(
    r: f64,
    d: f64,
    s: f64,
    tick: f64,
    lo: f64,
    hi: f64,
    standoff_ticks: f64,
) -> Option<(f64, f64)> {
    let standoff = standoff_ticks * tick;
    // keep each posted side on its own side of the fair mid by at least the standoff (min picks the
    // lower bid, max the higher ask), so inventory skew can't push a quote across `s`.
    let mut bid = (r - d).min(s - standoff);
    let mut ask = (r + d).max(s + standoff);
    // wall clamp into the price domain `[lo, hi]`: Polymarket `[tick, 1−tick]`; a `$`-scale Unbounded
    // domain passes `(−∞, +∞)`, for which `bid.max(−∞) == bid` and `ask.min(+∞) == ask` (no clamp).
    bid = bid.max(lo);
    ask = ask.min(hi);
    // snap both onto the venue grid, then accept only a genuine, correctly-ordered two-sided quote
    // that still straddles the fair mid — else the market is pinned at a wall / the sides collapsed.
    bid = snap_to_tick(bid, tick);
    ask = snap_to_tick(ask, tick);
    if bid < ask && bid < s && s < ask { Some((bid, ask)) } else { None }
}

/// The maximally-wide two-sided quote at the domain walls `(lo, hi)` — the near-resolution BLACKOUT
/// stance (spec §2.3): stay two-sided but push fills out to the extreme walls (on Polymarket a fill
/// there only ever happens at a near-certain, favourable price). Generalized off the fixed 0–1 walls
/// to caller-supplied `(lo, hi)` ([`domain_bounds`]) for consistency with [`as_quotes`]; a Polymarket
/// mount passes `(tick, 1−tick)` (byte-identical to the old `tick`-argument form). `None` if the walls
/// don't straddle the mid (already pinned). This is reached ONLY inside the `TimeToResolution`
/// blackout, which a `$`-scale [`HorizonMode::ConstantTau`] mount never enters, so the degenerate
/// [`PriceDomain::Unbounded`] `(−∞, +∞)` case is unreachable on the crypto path.
pub(crate) fn widest_quote(s: f64, lo: f64, hi: f64) -> Option<(f64, f64)> {
    if lo < s && s < hi { Some((lo, hi)) } else { None }
}

#[cfg(test)]
mod gueant_tests {
    //! GLFT (Guéant–Lehalle–Fernandez-Tapia) closed-form pricing: the skew coefficient `S`, its
    //! `V_eff = S/γ` routing through the SHARED `as_quotes`, and the "reduces to A-S" identity.
    use super::*;

    const EPS: f64 = 1e-12;

    // Representative `$`-scale-crypto GLFT inputs (per-ms σ̂², interior κ, unit base intensity).
    const SIG2: f64 = 1e-6;
    const GAMMA: f64 = 0.1;
    const KAPPA: f64 = 50.0;
    const A: f64 = 1.0;

    /// `effective_variance` routes each model correctly: A-S passes the diffusion `v` through
    /// untouched (byte-identical), Gueant returns exactly `S/γ`, and a degenerate `γ ≤ 0` under
    /// Gueant falls back to the A-S `v` (defensive).
    #[test]
    fn effective_variance_routes_per_model() {
        let v_as = 4.2e-7;
        // A-S: identity, bit-for-bit.
        assert_eq!(
            effective_variance(SpreadModel::AvellanedaStoikov, v_as, SIG2, GAMMA, KAPPA, A)
                .to_bits(),
            v_as.to_bits(),
            "A-S must pass the diffusion variance through unchanged"
        );
        // Gueant: exactly S/γ, regardless of the A-S v_as it discards.
        let s = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A);
        assert_eq!(
            effective_variance(SpreadModel::Gueant, v_as, SIG2, GAMMA, KAPPA, A).to_bits(),
            (s / GAMMA).to_bits(),
            "Gueant must return S/γ, ignoring the A-S variance"
        );
        // Gueant with γ ≤ 0: defensive fallback to v_as.
        assert_eq!(
            effective_variance(SpreadModel::Gueant, v_as, SIG2, 0.0, KAPPA, A).to_bits(),
            v_as.to_bits(),
            "γ ≤ 0 under Gueant falls back to the A-S v"
        );
    }

    /// Non-positive `γ`, `κ`, `A`, or `σ̂²` ⇒ `S == 0` (the caller then floors the half-spread at the
    /// intensity term, so the maker still quotes rather than dividing by zero).
    #[test]
    fn gueant_skew_coeff_guards_nonpositive_inputs() {
        assert_eq!(gueant_skew_coeff(SIG2, 0.0, KAPPA, A), 0.0);
        assert_eq!(gueant_skew_coeff(SIG2, GAMMA, 0.0, A), 0.0);
        assert_eq!(gueant_skew_coeff(SIG2, GAMMA, KAPPA, 0.0), 0.0);
        assert_eq!(gueant_skew_coeff(0.0, GAMMA, KAPPA, A), 0.0);
        assert!(gueant_skew_coeff(SIG2, GAMMA, KAPPA, A) > 0.0, "positive inputs ⇒ positive S");
    }

    /// `S = √( σ̂²·γ / (2κA) · (1+γ/κ)^(1+κ/γ) )` — pinned against the formula recomputed
    /// independently (a regression against an accidental rearrangement of the closed form).
    #[test]
    fn gueant_skew_coeff_matches_closed_form() {
        let ratio = GAMMA / KAPPA;
        let expected =
            (SIG2 * GAMMA / (2.0 * KAPPA * A) * (1.0 + ratio).powf(1.0 + KAPPA / GAMMA)).sqrt();
        assert!((gueant_skew_coeff(SIG2, GAMMA, KAPPA, A) - expected).abs() < EPS);
    }

    /// THE AFFINE IDENTITY (the headline): feeding `as_quotes` the GLFT `V_eff = S/γ` on an UNBOUNDED
    /// domain (no wall clamp, no standoff, no floor/cap) yields the GLFT depths
    /// `bid_depth = c1 + (½+q)·S`, `ask_depth = c1 + (½−q)·S` — the SAME affine form A-S emits with
    /// `γ·V`. This proves GLFT prices through the shared assembly, and that A-S IS the `V=S/γ` limit.
    #[test]
    fn glft_depths_are_the_affine_form_through_as_quotes() {
        let s = 100.0;
        let q = 0.7;
        let big = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A);
        let v_eff = big / GAMMA;
        let c1 = as_intensity_halfspread(GAMMA, KAPPA);
        // Unbounded domain, no standoff/floor/cap, tick 0 (no snap) — isolate the pure affine math.
        let (bid, ask) = as_quotes(
            s,
            q,
            GAMMA,
            v_eff,
            KAPPA,
            0.0,
            f64::NEG_INFINITY,
            f64::INFINITY,
            0.0,
            0.0,
            0.0,
            0.0,
        )
        .expect("interior GLFT quote straddles the mid");
        let bid_depth = s - bid;
        let ask_depth = ask - s;
        assert!((bid_depth - (c1 + (0.5 + q) * big)).abs() < EPS, "bid depth = c1 + (½+q)·S");
        assert!((ask_depth - (c1 + (0.5 - q) * big)).abs() < EPS, "ask depth = c1 + (½−q)·S");
    }

    /// The GLFT skew is LINEAR in inventory and the base (q=0) half-spread is INDEPENDENT of it:
    /// `bid_depth − ask_depth = 2q·S`, and at `q=0` the two depths are equal (`c1 + ½·S`).
    #[test]
    fn glft_skew_is_linear_and_base_spread_is_inventory_independent() {
        let s = 100.0;
        let big = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A);
        let v_eff = big / GAMMA;
        let quote = |q: f64| {
            as_quotes(
                s,
                q,
                GAMMA,
                v_eff,
                KAPPA,
                0.0,
                f64::NEG_INFINITY,
                f64::INFINITY,
                0.0,
                0.0,
                0.0,
                0.0,
            )
            .unwrap()
        };
        // q = 0 ⇒ symmetric.
        let (b0, a0) = quote(0.0);
        assert!(((s - b0) - (a0 - s)).abs() < EPS, "at q=0 the depths are equal");
        // skew(q) = bid_depth − ask_depth = 2q·S.
        for q in [0.3, 0.7, 1.5] {
            let (b, a) = quote(q);
            let skew = (s - b) - (a - s);
            assert!((skew - 2.0 * q * big).abs() < EPS, "skew must be 2q·S (linear in q)");
        }
    }

    /// "Reduces to A-S": the GLFT-model effective variance fed to `as_quotes` is bit-identical to
    /// running the A-S assembly directly with `v = S/γ`. So a GLFT mount and an A-S mount tuned to the
    /// same `V` post the SAME quotes — the reduction is a code-level identity, not an approximation.
    #[test]
    fn glft_equals_avellaneda_stoikov_at_matching_variance() {
        let s = 100.0;
        let q = 0.4;
        let v_glft = effective_variance(SpreadModel::Gueant, 999.0, SIG2, GAMMA, KAPPA, A);
        let v_as = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A) / GAMMA; // the A-S maker's matching V
        let args = |v: f64| {
            as_quotes(
                s,
                q,
                GAMMA,
                v,
                KAPPA,
                0.01,
                f64::NEG_INFINITY,
                f64::INFINITY,
                0.0,
                0.0,
                0.0,
                1.0,
            )
        };
        assert_eq!(args(v_glft), args(v_as), "GLFT ≡ A-S at V = S/γ (bit-identical quotes)");
    }

    /// `γ → 0` limit: the adverse-selection floor `c1 = (1/γ)ln(1+γ/κ)` (shared by BOTH models)
    /// converges to `1/κ`, resolving the literature ambiguity (some sources print the `1/k` limit as
    /// the constant itself). And `S ∝ √γ → 0`, so a risk-neutral GLFT maker quotes the floor.
    #[test]
    fn gamma_to_zero_floor_is_one_over_kappa_and_skew_vanishes() {
        let tiny = 1e-9;
        assert!(
            (as_intensity_halfspread(tiny, KAPPA) - 1.0 / KAPPA).abs() < 1e-6,
            "c1 → 1/κ as γ → 0"
        );
        assert!(
            gueant_skew_coeff(SIG2, tiny, KAPPA, A) < 1e-6,
            "S ∝ √γ → 0 as γ → 0 (risk-neutral ⇒ no inventory skew)"
        );
    }
}

/// The online σ̂²/κ estimator state + params for the A-S pricing layer, held on [`SpreadMaker`] as
/// plain `&mut self` fields (the live core is single-writer, so no lock/arc-swap). Everything is
/// event-time driven (never wall-clock), matching the runtime's `now_ms`-fed determinism, and every
/// per-tick update is O(1) (EWMA recurrence + running sums + amortised `VecDeque` eviction).
pub(crate) struct AsState {
    /// The A-S knobs (see [`AsParams`]). Swapped in place on a live re-tune (estimators preserved).
    pub(crate) params: AsParams,
    /// Precomputed EWMA α from `params.sigma_half_life` (recomputed on a params swap). NOT
    /// persisted (durable-state DTO, portfolio-observer PR-4 T4) — it's a pure function of
    /// `params.sigma_half_life`, recomputed fresh from the live config at mount.
    alpha: f64,
    /// Online per-ms mid variance σ̂²; `None` until seeded by the first usable increment.
    /// `pub(crate)` so `strategy_impl.rs`'s durable-state DTO can read/restore it (a sibling
    /// module, so crate-visibility rather than module-privacy is needed — see `save_state`).
    pub(crate) sigma2: Option<f64>,
    /// Previous `(mid, ts)` for the σ increment; `None` until the first priced tick. `pub(crate)`
    /// — see `sigma2`.
    pub(crate) last_mid: Option<(f64, i64)>,
    /// The last mid `requote` priced off — the contemporaneous `s` a trade's distance is measured
    /// against for the κ fit. `None` until the first priced tick (trades before then are ignored).
    /// `pub(crate)` — see `sigma2`.
    pub(crate) last_quote_mid: Option<f64>,
    /// κ trade tape: `(δ, w, ts)` per executed trade, evicted by `ts` outside the event-time window.
    pub(crate) trades: VecDeque<(f64, f64, i64)>,
    /// Running `Σ w` over `trades` (the κ MLE numerator).
    pub(crate) sum_w: f64,
    /// Running `Σ (w·δ)` over `trades` (the κ MLE denominator).
    pub(crate) sum_w_delta: f64,
    /// [`KappaMode::OwnFillFit`](vike_model::KappaMode) own-order OUTCOME tape:
    /// `(δ, exposure_ms, filled, ts)` per terminated own order — δ the resting quote's distance to
    /// the fair mid it was priced against, `exposure_ms` the event-time it rested at its current
    /// price, `filled` a fill vs a censored (pulled/unfilled) exposure — evicted by `ts` outside the
    /// same `trade_window_ms` window the public tape uses. Populated ONLY when the maker feeds it
    /// (which it does solely under `OwnFillFit`), so every other maker leaves it empty ⇒
    /// byte-identical. `pub(crate)` like the public tape (for test accessors). NOT persisted — it
    /// re-warms after a restart, like the fill-rate breaker's fill window.
    pub(crate) own_obs: VecDeque<(f64, f64, bool, i64)>,
    /// Running count of FILLED entries in `own_obs` — the censored MLE's fill count `D` and the
    /// shrink-to-public weight's numerator, kept in step on every push/evict.
    pub(crate) own_n_fill: usize,
    /// Cached censored-hazard `κ̂` over the current `own_obs` window, recomputed ONLY on an
    /// own-outcome event (rare vs the per-tick requote), or `None` with no own fills yet.
    /// [`AsState::effective_kappa`] reads it and shrinks it toward the public-print κ below `n_min`.
    pub(crate) cached_own_kappa: Option<f64>,
    /// Cached censored-hazard base intensity `Â` (orders/ms) over the current `own_obs` window —
    /// [`fit_base_intensity`] recomputed alongside `cached_own_kappa` on each own-outcome event, or
    /// `None` with no own fills. [`AsState::effective_a`] reads it for the GLFT `S` under
    /// [`SpreadModel::Gueant`]; every other maker (empty tape / A-S) leaves it `None` ⇒ the fixed
    /// [`AsParams::base_intensity_a`], byte-identical.
    pub(crate) cached_own_a: Option<f64>,
    /// Latest UNDERLYING state `(s_now, s_open, sigma_per_sec)` fed via [`AsState::set_underlying`]
    /// — the input for the underlying-anchored fair mid ([`AsParams::underlying_weight`]). `None`
    /// until the maker routes an underlying mark in; a re-tune PRESERVES it (estimator-like state,
    /// like the trade tape). NOT persisted — it re-warms from the mark feed after a restart.
    pub(crate) underlying: Option<(f64, f64, f64)>,
    /// GROUP-B Cont–Kukanov–Stoikov OFI accumulator ([`crate::alpha::OfiTracker`]) feeding the
    /// short-alpha reservation drift. Advanced ONLY when the OFI alpha channel is active
    /// (`alpha_lambda_ofi != 0`) — otherwise never touched, so a maker without the knob is free +
    /// byte-identical. Estimator-like warm state: a live re-tune PRESERVES it (like σ̂²), so
    /// [`AsState::set_params`] does NOT reset it. NOT persisted — re-warms from the book after a restart.
    ofi: crate::alpha::OfiTracker,
    /// Latch for the break-even-fee REFUSAL edge ([`fee_floor_exceeds_cap`]): `true` while this
    /// maker is posting nothing because no width is both under `max_half_spread_ticks` and above
    /// break-even.
    ///
    /// It exists so the refusal can be SAID. A silent `None` per tick would trade one invisible
    /// failure (quoting at a guaranteed loss) for another (quoting nothing for an unstated reason) —
    /// and this repo has already lost weeks to the second shape (`working: 0` in 652 of 657 the CI box
    /// summaries). Warning per tick is not available either: the maker's requote is the vike-core
    /// hot fold, whose rule is "instrument per-order boundaries and fault transitions only". A
    /// TRANSITION into or out of the refusal IS a fault transition, so the warn fires on the EDGE —
    /// twice per episode, O(1), never per message. NOT persisted (a restart re-derives it from the
    /// first priced tick, like every other estimator on this struct).
    fee_floor_refusing: bool,
}

impl AsState {
    pub(crate) fn new(params: AsParams) -> Self {
        AsState {
            alpha: ewma_alpha(params.sigma_half_life),
            ofi: crate::alpha::OfiTracker::new(params.ofi_decay),
            params,
            sigma2: None,
            last_mid: None,
            last_quote_mid: None,
            trades: VecDeque::new(),
            sum_w: 0.0,
            sum_w_delta: 0.0,
            own_obs: VecDeque::new(),
            own_n_fill: 0,
            cached_own_kappa: None,
            cached_own_a: None,
            underlying: None,
            fee_floor_refusing: false,
        }
    }

    /// Warn on the EDGE into/out of the break-even refusal, and answer whether the maker is in it.
    /// See [`Self::fee_floor_refusing`] for why this is edge-triggered rather than per-tick, and
    /// [`bounded_half_spread`] for why the refusal is a refusal.
    fn note_fee_floor(&mut self, fee_floor: f64, tick: f64) -> bool {
        let refusing = fee_floor_exceeds_cap(fee_floor, self.params.max_half_spread_ticks, tick);
        if refusing != self.fee_floor_refusing {
            let cap = self.params.max_half_spread_ticks * tick;
            if refusing {
                tracing::warn!(
                    break_even_half_spread = fee_floor,
                    max_half_spread = cap,
                    max_half_spread_ticks = self.params.max_half_spread_ticks,
                    tick,
                    round_trip_fee_rate = ?self.params.round_trip_fee_rate,
                    "maker HOLDING: no half-spread is both under max_half_spread_ticks and above \
                     the round-trip fee — every completed round trip at this cap is a loss. This is \
                     the instrument, not the tuning; widening the cap does not fix it."
                );
            } else {
                tracing::warn!(
                    break_even_half_spread = fee_floor,
                    max_half_spread = cap,
                    "maker RESUMING: the break-even half-spread is back under the cap"
                );
            }
            self.fee_floor_refusing = refusing;
        }
        refusing
    }

    /// Whether the break-even refusal latch is currently engaged.
    ///
    /// Read by [`SpreadMaker::note_no_quote`] so the two HOLD warns never announce the SAME hold
    /// twice: when the fee floor is the reason `price` returned `None`, it has already said so with
    /// the numbers, and a second generic "no two-sided quote" line would only bury it.
    pub(crate) fn is_fee_floor_refusing(&self) -> bool {
        self.fee_floor_refusing
    }

    /// Feed the latest underlying snapshot for the anchored fair mid: `s_now` the current spot,
    /// `s_open` the window-open reference, `sigma` the per-second log-return stddev. The blend
    /// consumes it in [`AsState::price`] iff [`AsParams::underlying_weight`] `> 0`. Called from the
    /// maker's underlying-mark ROUTING (`SpreadMaker::on_mark` → `UnderlyingTracker::observe` → here,
    /// the "Option B" cross-symbol routing) once the tracker has a window-open reference + a σ estimate.
    pub(crate) fn set_underlying(&mut self, s_now: f64, s_open: f64, sigma: f64) {
        self.underlying = Some((s_now, s_open, sigma));
    }

    /// The model up-probability `p_up(underlying)` for this tick, or `None` when the blend can't be
    /// computed (no underlying, no window, or outside the time-to-resolution regime). Seconds-into-
    /// window `t = h − (T − now)` is derived from `resolution_ts`; the shared [`vike_model::p_up`]
    /// applies its own `max(h−t, 1e-9)` floor at the last instant.
    fn model_fair(&self, ts: i64) -> Option<f64> {
        let (s_now, s_open, sigma) = self.underlying?;
        let h = self.params.window_secs;
        if h <= 0.0 {
            return None;
        }
        let t_res = match (self.params.horizon_mode, self.params.resolution_ts) {
            (HorizonMode::TimeToResolution, Some(t)) => t,
            _ => return None,
        };
        let ttr_s = ((t_res - ts).max(0) as f64) / 1000.0; // seconds remaining
        let t = (h - ttr_s).clamp(0.0, h); // seconds into the window
        Some(vike_model::p_up(self.params.underlying_beta, s_now, s_open, sigma, t, h))
    }

    /// The fair value the pricing uses: the raw book mid blended toward the model `p_up` by
    /// [`AsParams::underlying_weight`]. `w == 0` or an uncomputable model ⇒ the book mid unchanged
    /// (byte-identical). Only the pricing sees this — the σ̂²/κ estimators keep the raw book mid.
    fn blended_anchor(&self, book_mid: f64, ts: i64) -> f64 {
        let w = self.params.underlying_weight;
        if w <= 0.0 {
            return book_mid;
        }
        match self.model_fair(ts) {
            Some(model) => {
                let w = w.clamp(0.0, 1.0);
                (1.0 - w) * book_mid + w * model
            }
            None => book_mid,
        }
    }

    /// Swap the params on a live re-tune, recomputing α. The estimator state (σ̂², last mid, trade
    /// tape) is DELIBERATELY preserved — a re-tune keeps the warm estimators, like the maker's
    /// resting orders / breaker window survive `apply_params`.
    pub(crate) fn set_params(&mut self, params: AsParams) {
        self.alpha = ewma_alpha(params.sigma_half_life);
        self.params = params;
    }

    /// The fair value `s`: the raw mid, or (opt-in) the size-weighted micro-price that leans toward
    /// the thinner side. `None` on a one-sided book (can't quote two-sided).
    fn fair_value(&self, view: &BookView) -> Option<f64> {
        let &(bid_px, bid_sz) = view.bids.first()?;
        let &(ask_px, ask_sz) = view.asks.first()?;
        if self.params.use_micro_price {
            let denom = bid_sz + ask_sz;
            if denom > 0.0 {
                // micro-price: (bid·askSize + ask·bidSize)/(bidSize+askSize) — weighted toward the
                // side with LESS resting size (the direction under more pressure).
                return Some((bid_px * ask_sz + ask_px * bid_sz) / denom);
            }
        }
        Some(0.5 * (bid_px + ask_px))
    }

    /// Advance the online σ̂² off a fresh mid `s` at event ts `ts` (spec §3): seed on the first usable
    /// increment, EWMA thereafter; `Δt <= 0` skips the update but still advances `last_mid`.
    fn update_sigma(&mut self, s: f64, ts: i64) {
        if let Some((s_prev, ts_prev)) = self.last_mid {
            let dt = (ts - ts_prev) as f64;
            if dt > 0.0 {
                self.sigma2 = Some(match self.sigma2 {
                    Some(prev) => update_sigma2(prev, s - s_prev, dt, self.alpha),
                    None => (s - s_prev) * (s - s_prev) / dt, // seed with the first instantaneous σ̂²
                });
            }
        }
        self.last_mid = Some((s, ts));
    }

    /// Effective horizon `H = min(τ_hold, T − t)` in ms (spec §2.1). Constant-τ mode ignores
    /// resolution; time-to-resolution caps τ_hold by the (floored) time left, so `t → T` shrinks it.
    fn horizon_ms(&self, ts: i64) -> f64 {
        let tau = self.params.tau_hold_ms.max(0) as f64;
        match self.params.horizon_mode {
            HorizonMode::ConstantTau => tau,
            HorizonMode::TimeToResolution => match self.params.resolution_ts {
                Some(t_res) => tau.min((t_res - ts).max(0) as f64),
                None => tau,
            },
        }
    }

    /// Whether `ts` is inside the near-resolution blackout window (spec §2.3): only in
    /// time-to-resolution mode with a known `resolution_ts` and a positive blackout. `pub(crate)` so
    /// the maker's `requote` can consult it to keep the reward-EV fold from clamping a blackout's
    /// wide wall-quote back into the band (the blackout stays the SAFETY authority).
    pub(crate) fn in_blackout(&self, ts: i64) -> bool {
        if self.params.resolution_blackout_ms <= 0 {
            return false;
        }
        match (self.params.horizon_mode, self.params.resolution_ts) {
            (HorizonMode::TimeToResolution, Some(t_res)) => {
                ts >= t_res - self.effective_blackout_ms(t_res, ts)
            }
            _ => false,
        }
    }

    /// The blackout window in ms — the base `resolution_blackout_ms`, WIDENED near ATM by the
    /// settlement guard ([`AsParams::atm_blackout_scale`]). Off (`0.0`), no underlying, or an
    /// uncomputable moneyness ⇒ the base, byte-identical. At ATM (`z → 0`) the window is
    /// `base·(1 + scale)`; far from ATM it relaxes to `base`.
    fn effective_blackout_ms(&self, t_res: i64, ts: i64) -> i64 {
        let base = self.params.resolution_blackout_ms;
        let scale = self.params.atm_blackout_scale;
        if scale <= 0.0 {
            return base;
        }
        match self.moneyness_z(t_res, ts) {
            Some(z) => {
                // ⚠ `libm::exp`, not `f64::exp` — and this is the FOURTEENTH site in a crate whose
                // module doc claimed "all THIRTEEN call sites now route through the `libm` crate".
                // It survived the #1544 conversion because `crates/vike-mm/src/platform_probe.rs`'s
                // source scan `break`s at the first `#[cfg(test)]` LINE, and this file carries a
                // test module in the MIDDLE with production code after it — so everything below
                // that marker, including this line, was outside the gate's view.
                //
                // The consequence here is DISCRETE rather than last-bit: `factor` feeds
                // `(base as f64 * factor).round() as i64`, and `.round()` quantises to whole
                // milliseconds. A one-ulp disagreement either side of a `.5` boundary moves the
                // blackout window by a full millisecond, which flips `in_blackout` on one box and
                // not the other at a boundary tick.
                let factor = 1.0 + scale * libm::exp(-0.5 * z * z);
                (base as f64 * factor).round() as i64
            }
            None => base,
        }
    }

    /// Vol-normalized moneyness `z = |ln(S/S_open)| / (σ·√τ)` of the stored underlying, `τ` seconds to
    /// resolution. `None` without a live underlying or a degenerate `σ`/`S_open`. Near ATM `z → 0`.
    fn moneyness_z(&self, t_res: i64, ts: i64) -> Option<f64> {
        let (s_now, s_open, sigma) = self.underlying?;
        if sigma <= 0.0 || s_open <= 0.0 {
            return None;
        }
        let tau_s = ((t_res - ts).max(0) as f64) / 1000.0;
        let denom = sigma * tau_s.max(1e-9).sqrt();
        // The FIFTEENTH site, invisible to the gate for the same reason as the one above.
        // ⚠ Keep the `.abs()`: it is `f64::abs`, a SIGN-BIT clear that IEEE 754 defines exactly,
        // not a transcendental — converting it would be a change with no argument behind it. Only
        // the logarithm moves.
        Some(libm::log(s_now / s_open).abs() / denom)
    }

    /// The terminal settlement-variance addend for the current tick — the arXiv-2607.17991
    /// binary-market penalty ([`terminal_var`]). `0.0` outside the
    /// `(TimeToResolution, Some(resolution_ts))` regime — where `τ = T − t` is undefined — and `0.0`
    /// whenever the knobs are off, so a maker that hasn't opted in prices byte-identically. `p` is the
    /// current fair probability (the mid `s`).
    fn terminal_var_now(&self, p: f64, ts: i64) -> f64 {
        match (self.params.horizon_mode, self.params.resolution_ts) {
            (HorizonMode::TimeToResolution, Some(t_res)) => terminal_var(
                self.params.terminal_penalty_gamma,
                self.params.terminal_ramp_ms,
                (t_res - ts).max(0) as f64,
                p,
            ),
            _ => 0.0,
        }
    }

    /// The κ used to price: the fixed default (κ available-but-gated), the live public-print MLE fit
    /// (which itself falls back to the default below `n_min`), or the own-fill censored-hazard fit
    /// shrunk toward that public fit below `n_min` own fills.
    pub(crate) fn effective_kappa(&self) -> f64 {
        match self.params.kappa_mode {
            KappaMode::Fixed => self.params.kappa_default,
            KappaMode::LiveFit => fit_kappa(
                self.sum_w,
                self.sum_w_delta,
                self.trades.len(),
                self.params.n_min,
                self.params.kappa_default,
                self.params.kappa_min,
                self.params.kappa_max,
            ),
            KappaMode::OwnFillFit => {
                // the public-print fit is the shrink target below `n_min` own fills (and itself
                // falls back to `kappa_default` below the public `n_min` / on a dead public tape).
                let public = fit_kappa(
                    self.sum_w,
                    self.sum_w_delta,
                    self.trades.len(),
                    self.params.n_min,
                    self.params.kappa_default,
                    self.params.kappa_min,
                    self.params.kappa_max,
                );
                match self.cached_own_kappa {
                    Some(own) => shrink_kappa(own, public, self.own_n_fill, self.params.n_min),
                    None => public,
                }
            }
        }
    }

    /// Fold one executed trade into the κ MLE's running sums over the event-time window (O(1)
    /// amortised). Ignored until the first priced tick supplies a contemporaneous mid.
    pub(crate) fn record_trade(&mut self, t: &TradeTick) {
        let Some(mid) = self.last_quote_mid else {
            return;
        };
        let delta = (t.price - mid).abs();
        let w = t.size;
        self.trades.push_back((delta, w, t.ts));
        self.sum_w += w;
        self.sum_w_delta += w * delta;
        let cutoff = t.ts - self.params.trade_window_ms;
        while let Some(&(d0, w0, ts0)) = self.trades.front() {
            if ts0 < cutoff {
                self.trades.pop_front();
                self.sum_w -= w0;
                self.sum_w_delta -= w0 * d0;
            } else {
                break;
            }
        }
    }

    /// Fold one terminated OWN order into the [`KappaMode::OwnFillFit`](vike_model::KappaMode)
    /// censored-hazard tape. `delta` is the resting quote's distance to the fair mid it was priced
    /// against, `exposure_ms` the event-time it rested, `filled` a fill vs a censored (pulled)
    /// exposure. O(1) amortised push + window evict; the bisection re-fit runs HERE — on the RARE
    /// own-outcome event, never on the per-tick requote path — and caches its result. Non-finite
    /// inputs are dropped; δ/exposure are floored at 0. The maker gates the call on `OwnFillFit`, so
    /// this stays untouched for every other maker.
    pub(crate) fn record_own_outcome(
        &mut self,
        delta: f64,
        exposure_ms: f64,
        filled: bool,
        ts: i64,
    ) {
        if !delta.is_finite() || !exposure_ms.is_finite() {
            return;
        }
        let delta = delta.max(0.0);
        let exposure_ms = exposure_ms.max(0.0);
        self.own_obs.push_back((delta, exposure_ms, filled, ts));
        if filled {
            self.own_n_fill += 1;
        }
        let cutoff = ts - self.params.trade_window_ms;
        while let Some(&(_d, _t, f0, ts0)) = self.own_obs.front() {
            if ts0 < cutoff {
                self.own_obs.pop_front();
                if f0 {
                    self.own_n_fill -= 1;
                }
            } else {
                break;
            }
        }
        // re-fit the censored hazard over the surviving window (rare → off the hot path). No fills
        // yet ⇒ unfittable (`None`) ⇒ `effective_kappa` falls back to the public-print κ.
        let obs: Vec<(f64, f64, bool)> =
            self.own_obs.iter().map(|&(d, t, f, _)| (d, t, f)).collect();
        self.cached_own_kappa =
            fit_kappa_censored(&obs, self.params.kappa_min, self.params.kappa_max);
        // Profile the base intensity `Â` off the SAME tape, at the κ this window prices with (the
        // shrink-blended `effective_kappa`, computed AFTER `cached_own_kappa` is set above). Rare
        // own-outcome event, so the O(n) refit is off the per-tick requote path — like the κ fit.
        let kappa = self.effective_kappa();
        self.cached_own_a = fit_base_intensity(&obs, kappa);
    }

    /// The base fill intensity `A` (orders/ms) the GLFT `S` prices with: the online censored-hazard
    /// `Â` ([`fit_base_intensity`]) once the own-fill tape carries fills (i.e. under
    /// [`KappaMode::OwnFillFit`]), else the fixed [`AsParams::base_intensity_a`]. Only consulted under
    /// [`SpreadModel::Gueant`] (A-S ignores `A`), so an A-S maker is byte-identical either way.
    pub(crate) fn effective_a(&self) -> f64 {
        self.cached_own_a.unwrap_or(self.params.base_intensity_a)
    }

    /// The current EWMA-decayed Cont–Kukanov–Stoikov OFI magnitude off the internal tracker — the
    /// input the maker's OFI-toxicity synthesis reads (Group-B, PR-3). `0.0` until the tracker is
    /// advanced, and the tracker is advanced ONLY while the OFI alpha channel is active
    /// (`alpha_lambda_ofi != 0`), so a maker that never runs that channel reads `0.0` here.
    pub(crate) fn ofi(&self) -> f64 {
        self.ofi.ofi()
    }

    /// Produce the A-S `(bid, ask)` for this tick, advancing the online σ̂² first — or `None` to hold
    /// (one-sided book, blackout-widen invalid, or sides collapsed at a wall). `position` is the
    /// signed inventory; `ts` the triggering event time.
    pub(crate) fn price(&mut self, view: &BookView, position: f64, ts: i64) -> Option<(f64, f64)> {
        let s = self.fair_value(view)?;
        self.update_sigma(s, ts);
        self.last_quote_mid = Some(s);
        // Resolve the posted-quote price bounds for the configured domain, threaded into BOTH the
        // blackout wall-quote and the normal `as_quotes` below. `UnitInterval` (the default) yields
        // `(tick, 1−tick)` — the same walls this code hard-coded before — so Polymarket is byte-
        // identical; `Unbounded` yields `(−∞, +∞)` (no wall clamp) for a `$`-scale asset.
        let (lo, hi) = domain_bounds(self.params.price_domain, view.tick_size);
        // near-resolution blackout: quote maximally wide (or hold if pinned) — dodge the resolution
        // vol spike the Bernoulli cap doesn't catch.
        if self.in_blackout(ts) {
            return widest_quote(s, lo, hi);
        }
        // Underlying-anchored fair mid (PM deep-dive #5): blend the book mid toward the model
        // p_up(underlying) by `underlying_weight`. `anchor == s` when off/unavailable ⇒ byte-
        // identical; the anchor feeds ONLY the pricing below — the σ̂²/κ estimators (updated above off
        // the raw book mid `s`) stay on the book.
        let anchor = self.blended_anchor(s, ts);
        let h = self.horizon_ms(ts);
        let sigma2 = self.sigma2.unwrap_or(0.0);
        let v = bounded_variance(sigma2, h, anchor, self.params.variance_mode);
        // Terminal settlement-variance penalty (arXiv 2607.17991): fold the settlement variance
        // p(1−p) into V near a binary resolution, ramped as τ→0. This term FEEDS BOTH the reservation
        // skew and the half-spread (via `as_quotes`), so it strengthens the skew as t→T (where the
        // diffusion term vanishes) and widens the spread at p≈0.5. OFF (`0.0`) ⇒ `v` unchanged
        // bit-for-bit — a maker that hasn't opted in prices exactly as before.
        let v = v + self.terminal_var_now(anchor, ts);
        let kappa = self.effective_kappa();
        // SpreadModel::Gueant: replace the finite-horizon A-S variance with the GLFT stationary
        // effective variance `V_eff = S/γ`, so `as_quotes` emits the GLFT depths `c1 + (½±q)·S`
        // through the SAME affine assembly (skew, standoff, wall clamp, floor/cap, snap all reused).
        // A-S (the default) leaves `v` untouched — byte-identical to the pre-GLFT path.
        let v = effective_variance(
            self.params.spread_model,
            v,
            sigma2,
            self.params.gamma,
            kappa,
            self.effective_a(),
        );
        let q_scale = if self.params.q_scale != 0.0 { self.params.q_scale } else { 1.0 };
        let q_norm = position / q_scale;
        // The BREAK-EVEN half-spread this venue's round-trip fee demands, priced against the same
        // anchor the quote is built from. `0.0` (no fee armed — the default) is a no-op through the
        // whole ladder below, so an un-armed maker prices byte-identically. `note_fee_floor` warns
        // on the edge into/out of the refusal; `bounded_half_spread` is what actually refuses.
        let fee_floor = break_even_half_spread(self.params.round_trip_fee_rate, anchor);
        self.note_fee_floor(fee_floor, view.tick_size);
        // DEFAULT (A-S/GLFT linear reservation + optimal spread): the verbatim pre-Group-B call, so the
        // fast path is byte-identical BY CONSTRUCTION — `as_quotes` stays the single source of truth.
        if self.params.reservation_model == ReservationModel::AsLinear
            && self.params.spread_source == SpreadSource::AsOptimal
            && self.params.alpha_beta_imbalance == 0.0
            && self.params.alpha_lambda_ofi == 0.0
            && self.params.running_penalty_phi == 0.0
            && !(self.params.flatten_by_ms > 0 && self.params.flatten_strength != 0.0)
        {
            return as_quotes(
                anchor,
                q_norm,
                self.params.gamma,
                v,
                kappa,
                view.tick_size,
                lo,
                hi,
                self.params.min_half_spread_ticks,
                self.params.max_half_spread_ticks,
                fee_floor,
                self.params.min_standoff_ticks,
            );
        }
        // GROUP-B override path (opt-in): swap the reservation and/or half-spread source, then reuse the
        // SAME floor/cap and the shared `assemble_quote` posting geometry the default path runs through.
        let mut r = match self.params.reservation_model {
            ReservationModel::AsLinear => {
                as_reservation_price(anchor, q_norm, self.params.gamma, v)
            }
            // LMSR walks the live mid in log-odds by SIGNED net inventory (`position`, not `q_norm`)
            // over depth `b`; inert (returns `anchor`) off the unit interval, so a $-scale book is
            // unchanged.
            ReservationModel::Lmsr => lmsr_reservation(anchor, position, self.params.lmsr_b),
        };
        // GROUP-B additive reservation shifts (all inert at 0 ⇒ the fast path above returned, so
        // reaching here with every shift knob off leaves `r` unchanged). Read the top of book once
        // for both the imbalance signal and the OFI fold.
        let (bid_px, bid_sz) = *view.bids.first()?;
        let (ask_px, ask_sz) = *view.asks.first()?;
        // Cartea–Jaimungal short-alpha drift `a·h − φ·q·h`, `a = β·imbalance + λ·ofi`. Advance the OFI
        // tracker ONLY when λ is active, so an imbalance-only (or penalty-only) maker never pays for it.
        let imbalance = crate::alpha::book_imbalance(bid_sz, ask_sz);
        if self.params.alpha_lambda_ofi != 0.0 {
            self.ofi.on_book(bid_px, bid_sz, ask_px, ask_sz);
        }
        let ofi = self.ofi.ofi();
        let alpha_sig = crate::alpha::combine_alpha(
            imbalance,
            self.params.alpha_beta_imbalance,
            ofi,
            self.params.alpha_lambda_ofi,
        );
        // For a continuously-requoting live maker the CJ time horizon is folded into the gains
        // (β/λ/φ), so the normalized unit horizon h = 1.0 is used rather than a wall-clock (T−t).
        r += crate::alpha::cj_reservation_shift(
            alpha_sig,
            1.0,
            q_norm,
            self.params.running_penalty_phi,
        );
        // Settlement force-flatten: ramp the target inventory toward FLAT as τ = (T − t) → 0. Inert
        // without a `resolution_ts` (τ = i64::MAX ⇒ outside any window ⇒ weight 0) or with the knobs off.
        let tau = self.params.resolution_ts.map(|t| t - ts).unwrap_or(i64::MAX);
        let fw = crate::settlement::flatten_weight(tau, self.params.flatten_by_ms);
        r += crate::settlement::force_flatten_skew(q_norm, fw, self.params.flatten_strength);
        let d_raw = match self.params.spread_source {
            SpreadSource::AsOptimal => as_optimal_half_spread(self.params.gamma, v, kappa),
            // LS-LMSR fed the probability split `(mid, 1−mid)`: widest at p≈0.5, tight toward the walls.
            SpreadSource::LsLmsr => {
                ls_lmsr_half_spread(anchor, 1.0 - anchor, self.params.ls_lmsr_alpha)
            }
            SpreadSource::GlostenMilgrom => glosten_milgrom_half_spread(anchor, self.params.gm_mu),
        };
        // The SAME width ladder `as_quotes` runs — one shared `bounded_half_spread` rather than a
        // second hand copy of floor-then-cap (which is how this path would drift out of step with the
        // break-even refusal). The stage-1 floor still guards a zero LS-LMSR/GM read from collapsing
        // the two-sided quote; stage 3 refuses when no width clears the round-trip fee under the cap.
        let d = bounded_half_spread(
            d_raw,
            view.tick_size,
            self.params.min_half_spread_ticks,
            self.params.max_half_spread_ticks,
            fee_floor,
        )?;
        assemble_quote(r, d, anchor, view.tick_size, lo, hi, self.params.min_standoff_ticks)
    }
}

/// The terminal settlement-variance penalty (arXiv 2607.17991): the pure ramp/peak shape, its regime
/// gate, its effect on the reservation skew + half-spread as `t → T`, and the OFF byte-identity.
#[cfg(test)]
mod terminal_penalty_tests {
    use super::*;

    const EPS: f64 = 1e-12;

    #[test]
    fn terminal_var_ramps_in_and_peaks_at_half() {
        let g = 1.0_f64;
        // OFF gates: non-positive γ_term OR non-positive ramp ⇒ exactly 0.
        assert!(terminal_var(0.0, 200, 50.0, 0.5).abs() < EPS);
        assert!(terminal_var(g, 0, 50.0, 0.5).abs() < EPS);
        assert!(terminal_var(g, -5, 50.0, 0.5).abs() < EPS);
        // Ramp weight: `τ ≥ ramp` ⇒ 0 (clamped even beyond); `τ = 0` ⇒ full `g·p(1−p)`; half at ramp/2.
        let full = g * 0.25; // p(1−p) at p = 0.5
        assert!(terminal_var(g, 200, 200.0, 0.5).abs() < EPS);
        assert!(terminal_var(g, 200, 400.0, 0.5).abs() < EPS);
        assert!((terminal_var(g, 200, 0.0, 0.5) - full).abs() < EPS);
        assert!((terminal_var(g, 200, 100.0, 0.5) - full * 0.5).abs() < EPS);
        // Monotone: strengthens as `τ` falls toward resolution.
        assert!(terminal_var(g, 200, 50.0, 0.5) > terminal_var(g, 200, 150.0, 0.5));
        // Peaks at p = 0.5, → 0 at the walls.
        assert!(terminal_var(g, 200, 0.0, 0.5) > terminal_var(g, 200, 0.0, 0.1));
        assert!(terminal_var(g, 200, 0.0, 0.1) > terminal_var(g, 200, 0.0, 0.01));
    }

    #[test]
    fn penalty_strengthens_skew_where_diffusion_vanishes() {
        // The headline property: near resolution the diffusion variance → 0, so WITHOUT the penalty
        // the A-S skew term vanishes (r == s) — the wrong shape for a binary market.
        let (s, q_norm, gamma) = (0.5_f64, 1.0_f64, 0.5_f64);
        let v_diffusion = 0.0_f64; // H → 0
        assert!((as_reservation_price(s, q_norm, gamma, v_diffusion) - s).abs() < EPS);
        // WITH the penalty, the settlement variance keeps a live (and strengthening) skew.
        let tvar = terminal_var(0.8, 200, 100.0, s); // τ = 100 of a 200 ms ramp, p = 0.5
        assert!(tvar > 0.0);
        assert!(as_reservation_price(s, q_norm, gamma, v_diffusion + tvar) < s - 1e-9);
        // …and the half-spread widens with the added variance.
        let kappa = 50.0_f64;
        assert!(
            as_optimal_half_spread(gamma, v_diffusion + tvar, kappa)
                > as_optimal_half_spread(gamma, v_diffusion, kappa) + 1e-9
        );
    }

    #[test]
    fn terminal_var_now_only_fires_in_time_to_resolution_regime() {
        let on = AsParams {
            terminal_penalty_gamma: 1.0,
            terminal_ramp_ms: 200,
            resolution_ts: Some(1_000),
            horizon_mode: HorizonMode::TimeToResolution,
            ..AsParams::default()
        };
        assert!(AsState::new(on).terminal_var_now(0.5, 900) > 0.0); // τ = 100 in a 200 ms ramp
        // ConstantTau ⇒ τ = (T − t) undefined ⇒ 0, even with the knobs set.
        let ct = AsParams { horizon_mode: HorizonMode::ConstantTau, ..on };
        assert!(AsState::new(ct).terminal_var_now(0.5, 900).abs() < EPS);
        // TimeToResolution but no known resolution_ts ⇒ 0.
        let no_res = AsParams { resolution_ts: None, ..on };
        assert!(AsState::new(no_res).terminal_var_now(0.5, 900).abs() < EPS);
    }

    #[test]
    fn price_penalty_widens_and_skews_near_resolution() {
        let view =
            BookView { tick_size: 0.01, bids: vec![(0.49, 100.0)], asks: vec![(0.51, 100.0)] };
        // T = 1000; price at ts = 900 (τ = 100) inside a 200 ms ramp; p = mid = 0.50 (peak). No
        // blackout so the terminal term (not the wall) governs; long inventory skews it toward selling.
        let base = AsParams {
            horizon_mode: HorizonMode::TimeToResolution,
            resolution_ts: Some(1_000),
            resolution_blackout_ms: 0,
            variance_mode: VarianceMode::LocalCapped,
            kappa_mode: KappaMode::Fixed,
            gamma: 0.5,
            q_scale: 1.0,
            min_standoff_ticks: 1.0,
            ..AsParams::default()
        };
        let (ob, oa) = AsState::new(base).price(&view, 1.0, 900).expect("two-sided (off)");
        let on = AsParams { terminal_penalty_gamma: 0.8, terminal_ramp_ms: 200, ..base };
        let (nb, na) = AsState::new(on).price(&view, 1.0, 900).expect("two-sided (on)");
        // Wider posted spread (settlement variance widened the half-spread AND skewed the reservation).
        assert!((na - nb) > (oa - ob) + 1e-9, "on {nb}..{na} vs off {ob}..{oa}");
        // Long-inventory skew pulls the ask no higher than the OFF case (urgency to sell down).
        assert!(na <= oa + 1e-9);
    }

    #[test]
    fn price_is_byte_identical_outside_the_ramp_window() {
        let view =
            BookView { tick_size: 0.01, bids: vec![(0.49, 100.0)], asks: vec![(0.51, 100.0)] };
        let base = AsParams {
            horizon_mode: HorizonMode::TimeToResolution,
            resolution_ts: Some(10_000),
            resolution_blackout_ms: 0,
            kappa_mode: KappaMode::Fixed,
            gamma: 0.5,
            q_scale: 1.0,
            ..AsParams::default()
        };
        // Control (penalty OFF) vs penalty CONFIGURED but inactive — τ = 10_000 ≫ ramp 200 ⇒ weight 0.
        let off_q = AsState::new(base).price(&view, 3.0, 0).expect("off");
        let cfg = AsParams { terminal_penalty_gamma: 0.9, terminal_ramp_ms: 200, ..base };
        let in_q = AsState::new(cfg).price(&view, 3.0, 0).expect("configured-but-inactive");
        assert_eq!(off_q, in_q, "outside the ramp window the penalty is byte-identical");
    }
}

/// The underlying-anchored fair mid (PM deep-dive #5): the book-mid ⇄ `p_up(underlying)` blend, its
/// gates, and byte-identity when off/unavailable.
#[cfg(test)]
mod underlying_anchor_tests {
    use super::*;

    const EPS: f64 = 1e-12;

    /// Blend `weight`, in the time-to-resolution regime with a 300 s window closing at T = 300_000 ms.
    fn blend_params(weight: f64) -> AsParams {
        AsParams {
            underlying_weight: weight,
            underlying_beta: 1.0,
            window_secs: 300.0,
            horizon_mode: HorizonMode::TimeToResolution,
            resolution_ts: Some(300_000),
            resolution_blackout_ms: 0,
            gamma: 0.5,
            kappa_mode: KappaMode::Fixed,
            q_scale: 1.0,
            min_standoff_ticks: 1.0,
            ..AsParams::default()
        }
    }

    #[test]
    fn blend_off_returns_book_mid() {
        // weight 0 ⇒ book mid, even with a live underlying set.
        let mut st = AsState::new(blend_params(0.0));
        st.set_underlying(100.5, 100.0, 1e-4);
        assert!((st.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
    }

    #[test]
    fn blend_pulls_the_anchor_toward_the_model() {
        // ts = 150_000 ⇒ t = 150 s of the 300 s window; a spot above the open ⇒ p_up ≈ 1, so the
        // w = 0.5 blend pulls the 0.50 book mid up, landing strictly between the mid and the model.
        let mut st = AsState::new(blend_params(0.5));
        st.set_underlying(100.5, 100.0, 1e-4);
        let model = st.model_fair(150_000).expect("model available");
        assert!(model > 0.9, "up-drift ⇒ high p_up, got {model}");
        let anchor = st.blended_anchor(0.50, 150_000);
        assert!(anchor > 0.50 && anchor < model, "anchor {anchor} in (0.50, {model})");
        assert!((anchor - (0.5 * 0.50 + 0.5 * model)).abs() < EPS);
    }

    #[test]
    fn falls_back_to_book_mid_when_uncomputable() {
        // weight > 0 but no underlying fed ⇒ book mid.
        let st = AsState::new(blend_params(0.5));
        assert!((st.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
        // underlying set but window_secs == 0 ⇒ book mid.
        let mut no_window = AsState::new(AsParams { window_secs: 0.0, ..blend_params(0.5) });
        no_window.set_underlying(100.5, 100.0, 1e-4);
        assert!((no_window.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
        // ConstantTau (τ undefined) ⇒ book mid.
        let mut const_tau =
            AsState::new(AsParams { horizon_mode: HorizonMode::ConstantTau, ..blend_params(0.5) });
        const_tau.set_underlying(100.5, 100.0, 1e-4);
        assert!((const_tau.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
    }

    #[test]
    fn model_fair_matches_the_shared_p_up() {
        let mut st = AsState::new(blend_params(0.5));
        st.set_underlying(100.5, 100.0, 1e-4);
        // t = window − (T − now)/1000 = 300 − (300_000 − 150_000)/1000 = 150 s.
        let expected = vike_model::p_up(1.0, 100.5, 100.0, 1e-4, 150.0, 300.0);
        assert!((st.model_fair(150_000).unwrap() - expected).abs() < EPS);
    }

    #[test]
    fn price_without_an_underlying_is_byte_identical() {
        let view =
            BookView { tick_size: 0.01, bids: vec![(0.49, 100.0)], asks: vec![(0.51, 100.0)] };
        // Blend configured (weight 0.5) but no underlying fed ⇒ same quote as blend-off.
        let on = AsState::new(blend_params(0.5)).price(&view, 1.0, 150_000);
        let off = AsState::new(blend_params(0.0)).price(&view, 1.0, 150_000);
        assert_eq!(on, off, "no underlying ⇒ anchor is the book mid ⇒ byte-identical");
    }
}

/// The ATM settlement guard (PM deep-dive #4): the moneyness-widened near-resolution blackout, and
/// byte-identity when off/unavailable.
#[cfg(test)]
mod atm_guard_tests {
    use super::*;

    /// `scale` guard, `blackout_ms` base window, resolution at T = 1_000 ms.
    fn guard_params(scale: f64, blackout_ms: i64) -> AsParams {
        AsParams {
            atm_blackout_scale: scale,
            resolution_blackout_ms: blackout_ms,
            horizon_mode: HorizonMode::TimeToResolution,
            resolution_ts: Some(1_000),
            ..AsParams::default()
        }
    }

    #[test]
    fn blackout_off_is_the_base_window() {
        // scale 0 ⇒ base window, even with a live ATM underlying.
        let mut st = AsState::new(guard_params(0.0, 200));
        st.set_underlying(100.0, 100.0, 1e-4);
        assert_eq!(st.effective_blackout_ms(1_000, 500), 200);
        // fixed base: 500 < 1000−200 = 800 ⇒ not yet; 850 ≥ 800 ⇒ in blackout.
        assert!(!st.in_blackout(500));
        assert!(st.in_blackout(850));
    }

    #[test]
    fn near_atm_widens_the_blackout_earlier() {
        // scale 2 at ATM (s_now == s_open ⇒ z = 0) ⇒ window = 200·(1 + 2) = 600.
        let mut st = AsState::new(guard_params(2.0, 200));
        st.set_underlying(100.0, 100.0, 1e-4);
        assert_eq!(st.effective_blackout_ms(1_000, 500), 600);
        // blacks out at ts ≥ 1000 − 600 = 400 — EARLIER than the base (which needs ts ≥ 800).
        assert!(st.in_blackout(500), "near ATM blacks out earlier");
        let mut base = AsState::new(guard_params(0.0, 200));
        base.set_underlying(100.0, 100.0, 1e-4);
        assert!(!base.in_blackout(500), "the base window is not yet in blackout at the same ts");
    }

    #[test]
    fn far_from_atm_relaxes_to_the_base_window() {
        // a deep-ITM underlying ⇒ z huge ⇒ exp(−z²/2) ≈ 0 ⇒ window ≈ base.
        let mut st = AsState::new(guard_params(2.0, 200));
        st.set_underlying(120.0, 100.0, 1e-4);
        assert_eq!(st.effective_blackout_ms(1_000, 500), 200);
    }

    #[test]
    fn no_underlying_uses_the_base_window() {
        // scale > 0 but no underlying fed ⇒ base, byte-identical.
        let st = AsState::new(guard_params(2.0, 200));
        assert_eq!(st.effective_blackout_ms(1_000, 500), 200);
    }
}

/// CRYPTO-DOMAIN WIDTH TUNING WORKBENCH — the `$`-scale A-S mount's economic transfer function.
///
/// `maker_price_scale.rs` proves the crypto mount QUOTES; this proves the quote WIDTH is SENSIBLE
/// under real vol — which the constant-mid mount test cannot, because a flat mid gives `σ̂² = 0`,
/// `V = 0`, and the half-spread floor-locks at `min_half_spread_ticks`. It mirrors the exact
/// `MakerMountConfig::crypto` `AsParams` (RawLocal / ConstantTau / Unbounded / `min_half_spread_ticks
/// = 2` / `γ = 5e-4` / `q_scale = 1e-2`, on the shared `κ = 50` / `H = 1h` defaults) and drives the
/// pure `as_quotes` with `V = σ̂²·H` computed straight from a target per-tick move (no EWMA warm-up
/// noise), so the width vs. volatility curve is exact and deterministic.
///
/// The economics it pins (mid `s = $64k`, `$1` tick, `H = 1h`, HL cadence `Δt ≈ 1.3s`):
/// - intensity floor `(1/γ)ln(1+γ/κ) ≈ $0.02` — sub-tick at `$`-scale, hence the tick floor governs
///   the dead-calm spread, NOT the A-S adverse-selection term (which is 0–1-grid-calibrated);
/// - vol half-spread `½·γ·σ̂²·H = 900·σ̂²` — so the posted spread is `clamp(1800·σ̂², $4, $120)` (both
///   sides), widening as the square of realized move: floor-locked (`min_half_spread_ticks`) in dead
///   calm, a few bps at typical BTC vol, and CAPPED (`max_half_spread_ticks = 60`, ~19 bp) in a spike
///   instead of running quadratically off the book — the intended, BOUNDED A-S vol response the
///   flat-mid test can't exercise;
/// - inventory skew `q_norm·γ·V` with `q_norm = clip/q_scale = 0.005/0.03 ≈ 0.167` per clip — the
///   per-clip reservation shift is `≈ 0.167·γV`, about a THIRD of the `0.5·γV` vol-half-spread, so it
///   takes ~3 clips to move the reservation a full half-spread and ~6 to pull the risk-reducing side
///   to the mid (a modest-warehouse stance, a step looser than the initial strict `q_scale = 0.01`
///   where one clip pinned the ask to the mid; the `$1` standoff still keeps a leaned quote off the mid).
#[cfg(test)]
mod crypto_width_tuning_tests {
    use super::*;

    /// The `AsParams` the crypto mount prices with — MUST mirror
    /// `vike_run::MakerMountConfig::crypto` (that crate can't be a dev-dep here without a cycle, so
    /// this is the pinned twin; if the mount's knobs change, this test's numbers move with them).
    ///
    /// ⚠ **ONE DELIBERATE DIFFERENCE: `round_trip_fee_rate` stays `None` here.** The mount ARMS it
    /// from the venue's fee schedule, and on every `$`-scale venue that floor DOMINATES the A-S
    /// response in dead calm (hyperliquid's 3 bps round trip floors the half-spread at `$9.60` at a
    /// `$64k` mid, against the `$2` tick floor these numbers pin). This workbench measures the
    /// VOLATILITY transfer function, which a constant floor only masks — so the fee axis is left off
    /// here and covered on its own in [`super::fee_floor_tests`], which pins both the floor's width
    /// and the refusal. Do not read this function as "what the mount posts"; read it as "what the vol
    /// term contributes". [`super::break_even_half_spread`] is threaded through `width_and_skew`
    /// below, so arming this field here would move these numbers with it rather than being ignored.
    fn crypto_params() -> AsParams {
        AsParams {
            variance_mode: VarianceMode::RawLocal,
            horizon_mode: HorizonMode::ConstantTau,
            price_domain: PriceDomain::Unbounded,
            min_half_spread_ticks: 2.0,
            max_half_spread_ticks: 60.0,
            gamma: 5e-4,
            q_scale: 3e-2,
            ..AsParams::default()
        }
    }

    /// The half-spread `$` and inventory-skew `$` the crypto config produces for a given per-tick BTC
    /// move `d_move` ($) at the live HL cadence `dt_ms` (~1.3s), at mid `s` on a `$1` grid. Exact:
    /// `σ̂² = Δ²/Δt` (the value a constant-|Δ| EWMA converges to), `V = σ̂²·H`, then the pure
    /// `as_quotes` for the flat-inventory half-spread and `q_norm·γ·V` for the one-clip skew.
    fn width_and_skew(p: &AsParams, s: f64, tick: f64, d_move: f64, dt_ms: f64) -> (f64, f64) {
        let sigma2 = d_move * d_move / dt_ms;
        let h = p.tau_hold_ms as f64; // ConstantTau ⇒ H = τ_hold
        let v = bounded_variance(sigma2, h, s, p.variance_mode);
        let (lo, hi) = domain_bounds(p.price_domain, tick);
        let (bid, ask) = as_quotes(
            s,
            0.0,
            p.gamma,
            v,
            p.kappa_default,
            tick,
            lo,
            hi,
            p.min_half_spread_ticks,
            p.max_half_spread_ticks,
            break_even_half_spread(p.round_trip_fee_rate, s),
            p.min_standoff_ticks,
        )
        .expect("crypto domain quotes two-sided");
        let half = 0.5 * (ask - bid);
        let q_norm = 0.005 / p.q_scale; // one 0.005-BTC clip
        let skew = q_norm * p.gamma * v;
        (half, skew)
    }

    /// Print + pin the width/skew transfer function across a realistic BTC vol range. The assertions
    /// are the REGRESSION GUARD: dead-calm floor-locks, the spread widens monotonically with vol and
    /// lands in a sensible single-digit-bps band at typical move, and one clip skews the reservation
    /// by ~one half-spread. A knob change that breaks the economics (e.g. a γ·H that floor-locks even
    /// in vol, or a spread that blows past 100 bps at ordinary move) trips here.
    #[test]
    fn crypto_width_transfer_function_is_sensible() {
        let p = crypto_params();
        let (s, tick, dt) = (64_000.0_f64, 1.0_f64, 1_300.0_f64);
        let bps = |half: f64| (2.0 * half) / s * 1e4; // full-spread bps of the mid

        println!(
            "crypto A-S width (mid ${s}, tick ${tick}, H={}ms, γ={}, q_scale={}, κ={}, Δt={dt}ms):",
            p.tau_hold_ms, p.gamma, p.q_scale, p.kappa_default
        );
        println!("  move/tick    half$     spread_bps   skew$/clip");
        let mut last_half = 0.0;
        for &mv in &[1.0, 2.0, 3.0, 5.0, 10.0, 20.0] {
            let (half, skew) = width_and_skew(&p, s, tick, mv, dt);
            println!("   ${mv:>5.0}     {half:>7.2}     {:>7.3}     {skew:>8.2}", bps(half));
            // widen monotonically with realized move (non-decreasing; ties only at the floor).
            assert!(half >= last_half - 1e-9, "half-spread must not shrink as vol rises");
            last_half = half;
        }

        // Dead-calm ($1 move) floor-locks at min_half_spread_ticks·tick = $2 (a $4 / ~0.6 bp spread).
        let (calm_half, _) = width_and_skew(&p, s, tick, 1.0, dt);
        assert!(
            (calm_half - 2.0).abs() < 1e-6,
            "dead-calm half-spread floors at $2, got {calm_half}"
        );

        // Typical BTC move (~$3/tick) lands in a sane low-single-digit-bps band (not floor-locked,
        // not absurdly wide) — the headline "the vol term activates under real vol" property.
        let (typ_half, typ_skew) = width_and_skew(&p, s, tick, 3.0, dt);
        assert!(typ_half > 2.0, "typical vol clears the floor: {typ_half}");
        assert!(
            (1.0..12.0).contains(&bps(typ_half)),
            "typical spread in a sane bps band: {}",
            bps(typ_half)
        );

        // One clip skews the reservation by ~a THIRD of a vol-half-spread (the modest-warehouse lean):
        // per-clip skew `= q_norm·γV = (0.005/0.03)·γV ≈ 0.167·γV`, vs the vol half-spread `0.5·γV` — so
        // ~3 clips move the reservation a full half-spread (q_scale = 0.03; strict `0.01` was 1 clip).
        assert!(
            (typ_skew - typ_half / 3.0).abs() < 0.15 * typ_half,
            "one-clip skew ≈ a third of a half-spread at q_scale=0.03: skew={typ_skew} half={typ_half}"
        );

        // A vol spike ($20/tick) is CAPPED by max_half_spread_ticks (60·$1 = $60 half / ~18.75 bp)
        // instead of the uncapped ~$277 (~86 bp) — the maker stays plausibly fillable through the
        // spike rather than posting off the book. Still wider than the typical-vol quote.
        let (spike_half, _) = width_and_skew(&p, s, tick, 20.0, dt);
        assert!(spike_half > typ_half, "spike still widens past the typical quote");
        assert!(
            (spike_half - 60.0 * tick).abs() < 1e-6,
            "spike half-spread is capped at max_half_spread_ticks·tick = $60, got {spike_half}"
        );
        assert!(bps(spike_half) < 20.0, "capped spike stays under ~20 bp: {}", bps(spike_half));
    }

    /// The full `AsState::price` path WARMS the online EWMA σ̂² to the same width the closed-form
    /// transfer function predicts — proving the workbench's exact-`V` shortcut matches what the live
    /// maker actually converges to when fed a real varying feed (the gap the constant-mid test left).
    #[test]
    fn price_path_warms_to_the_transfer_function_width() {
        let p = crypto_params();
        let (s0, tick, dt) = (64_000.0_f64, 1.0_f64, 1_300_i64);
        let mv = 5.0_f64; // $5/tick move
        let mut st = AsState::new(p);
        // Feed a converging constant-|Δ| walk (alternating ±$5) at the HL cadence to warm the
        // 32-half-life EWMA toward σ̂² = Δ²/Δt; read the flat-inventory half-spread at the end.
        let mut ts = 0_i64;
        let mut half = 0.0;
        for i in 0..600 {
            let s = if i % 2 == 0 { s0 } else { s0 + mv }; // mid oscillates $64000 ↔ $64005
            let view = BookView {
                tick_size: tick,
                bids: vec![(s - 1.0, 100.0)],
                asks: vec![(s + 1.0, 100.0)],
            };
            if let Some((bid, ask)) = st.price(&view, 0.0, ts) {
                half = 0.5 * (ask - bid);
            }
            ts += dt;
        }
        // The book mid alternates by $5 too, so the realized per-tick move ≈ $5; the warmed width
        // should land near the closed-form prediction for a $5 move (within a generous band — the
        // EWMA of an oscillating series isn't a perfect delta, and the micro-price/mid interact).
        let (predicted, _) = width_and_skew(&p, s0 + 2.5, tick, mv, dt as f64);
        println!("warmed half-spread ${half:.2} vs closed-form ${predicted:.2} ($5/tick)");
        assert!(
            half > 2.0,
            "the warmed live path clears the floor under $5 vol (not floor-locked): {half}"
        );
        assert!(half.is_finite(), "warmed half-spread is finite");
    }

    /// The half-spread ceiling in isolation: a high-`V` quote that would post far off the book is
    /// CLAMPED to `max_half_spread_ticks · tick`; `0.0` leaves it uncapped (byte-identical); and a
    /// `max < min` misconfig can never collapse the two-sided quote — the floor always wins.
    #[test]
    fn max_half_spread_caps_without_beating_the_floor() {
        let (s, g, k, tick) = (64_000.0_f64, 5e-4_f64, 50.0_f64, 1.0_f64);
        let (lo, hi) = domain_bounds(PriceDomain::Unbounded, tick);
        let v = 300_000.0_f64; // ½·γ·V = 0.5·5e-4·300000 = $75 vol half-spread — deliberately wide.

        // UNCAPPED (max = 0.0): the half-spread runs to the full ~$75.
        let (ub, ua) = as_quotes(s, 0.0, g, v, k, tick, lo, hi, 2.0, 0.0, 0.0, 1.0)
            .expect("uncapped two-sided");
        let uncapped_half = 0.5 * (ua - ub);
        assert!(uncapped_half > 60.0, "uncapped high-V half-spread is wide: {uncapped_half}");

        // CAPPED at 60 ticks: the half-spread clamps to exactly $60 (18.75 bp of the mid).
        let (cb, ca) = as_quotes(s, 0.0, g, v, k, tick, lo, hi, 2.0, 60.0, 0.0, 1.0)
            .expect("capped two-sided");
        let capped_half = 0.5 * (ca - cb);
        assert!((capped_half - 60.0).abs() < 1e-6, "capped at max·tick = $60, got {capped_half}");

        // max (1 tick) < min (2 ticks): the FLOOR governs — a dead-calm quote keeps its $2 half-spread,
        // never a $1 collapse, so a misconfigured ceiling can't strand the maker with a sub-floor quote.
        let (lb, la) = as_quotes(s, 0.0, g, 0.0, k, tick, lo, hi, 2.0, 1.0, 0.0, 1.0)
            .expect("floor-wins two-sided");
        let floor_half = 0.5 * (la - lb);
        assert!(
            (floor_half - 2.0).abs() < 1e-6,
            "max<min ⇒ the $2 floor wins, not the $1 cap: {floor_half}"
        );
    }
}

/// THE BREAK-EVEN FEE FLOOR — the maker's knowledge of what a round trip costs it.
///
/// ## The measurement these tests encode
///
/// the CI box's live `spread_maker` on bybit BTCUSDT was **arithmetically incapable of a profitable round
/// trip at any tuning**, and every number below is from that measurement rather than from a model:
///
/// - `vike_run::MakerMountConfig::crypto` sets `max_half_spread_ticks = 60`; on bybit BTCUSDT's
///   `0.1` tick that is `δ ≤ 6.0` USDT, i.e. a maximum round-trip capture of **1.903 bp**;
/// - `vike_model::fee_schedule_for("bybit")` is 2.0 bps maker per leg ⇒ a round-trip fee of
///   **4.000 bp** (`maker + maker`, both legs resting — `vike_model::maker_round_trip_fee`);
/// - break-even therefore needs `δ ≥ ½·m·P` = **12.61 USDT = 126 ticks**, more than double the cap.
///
/// Verified against five real round trips: the venue's fee matched `2e-4 × q × (P_buy + P_sell)` to
/// 8 decimal places on all five, realized was exactly `q × (sell − buy)`, and the observed
/// **−3.94 bp per round trip** landed within 0.01 bp of the −3.937 predicted at the 2-tick floor.
///
/// ## What these tests DO and DO NOT prove
///
/// They prove the maker now REFUSES that mount instead of quoting it. They do **not** prove any
/// mount is profitable, and no widening of the cap would make this one so: market making at 2 bp
/// against a book the repo itself measured **one tick wide (0.016 bp)** is impossible — the
/// instrument is wrong, not the tuning. The change makes the loss visible and refused rather than
/// silent.
#[cfg(test)]
mod fee_floor_tests {
    use super::*;

    /// bybit BTCUSDT as the CI box mounted it: `MakerMountConfig::crypto`'s knobs on the real `0.1` grid.
    const TICK: f64 = 0.1;
    const MID: f64 = 63_050.0;
    /// `maker + maker` at bybit VIP0's 2.0 bps maker — what `vike_model::maker_round_trip_fee`
    /// returns for `fee_schedule_for("bybit")` (pinned there, in that crate's own tests).
    const BYBIT_ROUND_TRIP: f64 = 4e-4;

    fn prod2_params(max_ticks: f64, fee: Option<f64>) -> AsParams {
        AsParams {
            variance_mode: VarianceMode::RawLocal,
            horizon_mode: HorizonMode::ConstantTau,
            price_domain: PriceDomain::Unbounded,
            min_half_spread_ticks: 2.0,
            max_half_spread_ticks: max_ticks,
            round_trip_fee_rate: fee,
            gamma: 5e-4,
            q_scale: 3e-2,
            ..AsParams::default()
        }
    }

    fn book() -> BookView {
        BookView {
            tick_size: TICK,
            bids: vec![(MID - 0.5 * TICK, 5.0)],
            asks: vec![(MID + 0.5 * TICK, 5.0)],
        }
    }

    /// The shared tracing-capture helper — see [`crate::tests::captured_logs`] for why the warn is
    /// tested as behaviour and why the helper has exactly one home.
    use crate::tests::captured_logs;

    /// **The the CI box refusal is SAID, not merely returned.** `#1336` pinned that the mount posts
    /// nothing; nothing pinned that an operator is told why, and on 2026-08-17 that gap cost a whole
    /// investigation — the CI box ran `orders:0 fault:null` across ~112k summary `seq` with **zero**
    /// non-summary log lines, and the absence of this very warn is what proved the fee floor was NOT
    /// the cause and sent the search upstream (it was a bybit market-data socket subscribed to
    /// nothing). The warn is load-bearing DIAGNOSTIC EVIDENCE, so it is tested as behaviour.
    ///
    /// KILL PROOF: delete the `warn!` in [`AsState::note_fee_floor`] and this reddens while
    /// `the_prod2_bybit_mount_posts_nothing_because_no_width_is_profitable` stays green.
    #[test]
    fn the_prod2_refusal_is_said_out_loud_not_only_returned() {
        let mut armed = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
        let logs = captured_logs(|| {
            assert_eq!(armed.price(&book(), 0.0, 0), None, "the mount must still refuse");
        });
        assert!(
            logs.contains("maker HOLDING"),
            "the refusal must be announced, not silent — got: {logs:?}"
        );
        // The operator needs the two NUMBERS that make the refusal actionable (which is which, and
        // by how much), not just the fact of it.
        for field in ["break_even_half_spread", "max_half_spread", "max_half_spread_ticks"] {
            assert!(logs.contains(field), "the warn must carry `{field}` — got: {logs:?}");
        }
        assert!(
            logs.contains("the instrument, not the tuning"),
            "…and must say the cap is not the cure, or an operator widens it — got: {logs:?}"
        );
    }

    /// EDGE-triggered, not per-tick — the property that makes the warn admissible in the vike-core
    /// hot fold at all (CLAUDE.md: "instrument per-order boundaries and fault transitions only").
    /// A per-tick warn on a book feed is an unbounded log write on the money path, so this is a
    /// PERFORMANCE contract as much as a legibility one.
    #[test]
    fn the_hold_warn_fires_once_per_episode_never_once_per_tick() {
        let mut armed = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
        let logs = captured_logs(|| {
            for ts in 0..64 {
                assert_eq!(armed.price(&book(), 0.0, ts), None, "still refusing at ts={ts}");
            }
        });
        assert_eq!(
            logs.matches("maker HOLDING").count(),
            1,
            "64 refusing ticks must produce exactly ONE warn — got: {logs:?}"
        );
    }

    /// The NEGATIVE control, and the half that keeps the test above honest: a mount whose ceiling
    /// clears break-even quotes and says NOTHING. Without this, a `warn!` fired unconditionally on
    /// every tick would satisfy the two tests above.
    #[test]
    fn a_mount_whose_ceiling_covers_the_fee_holds_nothing_and_says_nothing() {
        let mut st = AsState::new(prod2_params(200.0, Some(BYBIT_ROUND_TRIP)));
        let logs = captured_logs(|| {
            st.price(&book(), 0.0, 0).expect("a ceiling above break-even still quotes");
        });
        assert!(
            !logs.contains("maker HOLDING"),
            "a maker that CAN quote must not announce a hold — got: {logs:?}"
        );
    }

    /// The other EDGE: a mount that recovers says so. Drives the latch down and back up by
    /// re-tuning the ceiling under a live state, which is the real path (`set_params` on a
    /// live re-tune), so the maker's silence never outlives its cause.
    #[test]
    fn leaving_the_refusal_is_announced_too() {
        let mut st = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
        let logs = captured_logs(|| {
            assert_eq!(st.price(&book(), 0.0, 0), None, "starts refusing");
            st.params.max_half_spread_ticks = 200.0;
            st.price(&book(), 0.0, 1).expect("a widened ceiling quotes again");
        });
        assert!(logs.contains("maker HOLDING"), "the entry edge — got: {logs:?}");
        assert!(
            logs.contains("maker RESUMING"),
            "…and the EXIT edge, or a recovered maker looks permanently broken — got: {logs:?}"
        );
    }

    /// The `½` is the ONLY place a factor of two can hide, so pin the arithmetic on the real numbers
    /// before pinning anything that depends on it: `½ · 4e-4 · 63050 = 12.61` USDT = 126 ticks.
    #[test]
    fn break_even_half_spread_is_half_the_round_trip_fee_times_the_mid() {
        let d = break_even_half_spread(Some(BYBIT_ROUND_TRIP), MID);
        assert!((d - 12.61).abs() < 1e-9, "½·m·P must be $12.61, got {d}");
        assert!((d / TICK - 126.1).abs() < 1e-6, "…which is 126 ticks, got {}", d / TICK);
        // An UNARMED fee is `0.0` — "no floor", and the ONLY value that means it.
        assert_eq!(break_even_half_spread(None, MID).to_bits(), 0.0_f64.to_bits());
        // A measured zero-fee maker row (aster-perp) is also `0.0`: numerically the same floor, and
        // deliberately so — what distinguishes it from `None` is what the MOUNT could say.
        assert_eq!(break_even_half_spread(Some(0.0), MID).to_bits(), 0.0_f64.to_bits());
        // Defensive: a non-finite/negative rate or a degenerate mid can never manufacture a floor.
        for (m, s) in [(f64::NAN, MID), (-1e-4, MID), (BYBIT_ROUND_TRIP, 0.0)] {
            assert_eq!(break_even_half_spread(Some(m), s).to_bits(), 0.0_f64.to_bits(), "{m}/{s}");
        }
    }

    /// **THE HEADLINE, and the test that reddens without the floor.** The the CI box mount — bybit's 4 bp
    /// round trip against a 60-tick (`$6.00`) ceiling — POSTS NOTHING, because no half-spread is both
    /// under the operator's cap and above the `$12.61` break-even. The same mount with the fee
    /// UNARMED posts happily, which is exactly the silent-loss behaviour being removed.
    #[test]
    fn the_prod2_bybit_mount_posts_nothing_because_no_width_is_profitable() {
        let mut armed = AsState::new(prod2_params(60.0, Some(BYBIT_ROUND_TRIP)));
        assert_eq!(
            armed.price(&book(), 0.0, 0),
            None,
            "a 60-tick ceiling cannot cover a 126-tick break-even — the maker must post NOTHING"
        );
        // The CONTROL: identical mount, fee unarmed ⇒ it quotes, at a width that loses money on
        // every completed round trip. This is the measured status quo, pinned so the fix has a
        // before/after rather than an assertion about itself.
        let mut unarmed = AsState::new(prod2_params(60.0, None));
        let (bid, ask) = unarmed.price(&book(), 0.0, 0).expect("the unarmed mount quotes");
        let half = 0.5 * (ask - bid);
        assert!(half < 12.61, "the status-quo quote is narrower than break-even: ${half}");
        let capture_bp = 2.0 * half / MID * 1e4;
        assert!(
            capture_bp < BYBIT_ROUND_TRIP * 1e4,
            "…and its round-trip capture ({capture_bp:.3} bp) is under the 4.000 bp fee"
        );
    }

    /// The other half of the requirement: a mount whose ceiling DOES cover the fee still posts — and
    /// the floor is what sets its width, since the A-S half-spread at `$`-scale is far below it.
    #[test]
    fn a_mount_whose_ceiling_covers_the_fee_still_posts_at_break_even() {
        // 200 ticks = $20.00 ceiling, comfortably above the $12.61 break-even.
        let mut st = AsState::new(prod2_params(200.0, Some(BYBIT_ROUND_TRIP)));
        let (bid, ask) =
            st.price(&book(), 0.0, 0).expect("a ceiling above break-even still quotes");
        let half = 0.5 * (ask - bid);
        assert!(
            half >= 12.61 - TICK,
            "the posted half-spread must clear break-even (± one grid snap): ${half}"
        );
        assert!(half <= 20.0, "…and must still honour the ${:.2} ceiling: ${half}", 200.0 * TICK);
        // It is the FEE that set this width, not the A-S term: the same mount unarmed is far tighter.
        let mut unarmed = AsState::new(prod2_params(200.0, None));
        let (ub, ua) = unarmed.price(&book(), 0.0, 0).expect("quotes");
        assert!(
            0.5 * (ua - ub) < half,
            "the fee floor WIDENED the quote, it did not merely pass it"
        );
    }

    /// An UNARMED maker is byte-identical, which is what makes the knob safe to add to a shared
    /// pricing layer: every Polymarket mount, every hand-built `AsParams`, every old journal payload.
    #[test]
    fn an_unarmed_fee_prices_bit_for_bit_as_before() {
        let view =
            BookView { tick_size: 0.01, bids: vec![(0.49, 100.0)], asks: vec![(0.51, 100.0)] };
        let base = AsParams { kappa_mode: KappaMode::Fixed, q_scale: 1.0, ..AsParams::default() };
        assert_eq!(base.round_trip_fee_rate, None, "the DEFAULT is unarmed");
        let armed_zero = AsParams { round_trip_fee_rate: Some(0.0), ..base };
        assert_eq!(
            AsState::new(base).price(&view, 1.0, 0),
            AsState::new(armed_zero).price(&view, 1.0, 0),
            "an unarmed fee and a measured-zero fee price identically"
        );
    }

    /// **The rejected alternative, pinned as rejected.** The floor does NOT join the
    /// `max(min_floor, cap)` idiom — it never posts a quote wider than the operator's ceiling. Any
    /// future edit that "fixes" the refusal by widening past the cap trips here.
    #[test]
    fn the_fee_floor_never_posts_wider_than_the_operator_s_ceiling() {
        // Sweep ceilings from far below break-even to far above it.
        for max_ticks in [10.0, 60.0, 126.0, 127.0, 200.0, 1_000.0] {
            let mut st = AsState::new(prod2_params(max_ticks, Some(BYBIT_ROUND_TRIP)));
            let Some((bid, ask)) = st.price(&book(), 0.0, 0) else {
                // A refusal is always safe; this test is about what it DOES post.
                assert!(
                    max_ticks * TICK < 12.61,
                    "a ceiling of {max_ticks} ticks covers break-even, so it must not refuse"
                );
                continue;
            };
            let half = 0.5 * (ask - bid);
            assert!(
                half <= max_ticks * TICK + 1e-9,
                "ceiling {max_ticks} ticks (${:.2}) violated by a ${half} half-spread",
                max_ticks * TICK
            );
            assert!(
                half >= 12.61 - TICK,
                "…and anything it DOES post clears break-even: ${half} at {max_ticks} ticks"
            );
        }
    }

    /// An UNCAPPED maker (`max_half_spread_ticks == 0.0`, the library default) is never refused: with
    /// no ceiling the fee floor simply widens the quote, and there is nothing to refuse. This is the
    /// arm that keeps the refusal narrow — it fires only where an operator's own cap contradicts the
    /// economics, never merely because a fee exists.
    #[test]
    fn an_uncapped_maker_widens_instead_of_refusing() {
        assert!(!fee_floor_exceeds_cap(12.61, 0.0, TICK), "no ceiling ⇒ nothing to contradict");
        let mut st = AsState::new(prod2_params(0.0, Some(BYBIT_ROUND_TRIP)));
        let (bid, ask) = st.price(&book(), 0.0, 0).expect("an uncapped maker still quotes");
        assert!(
            0.5 * (ask - bid) >= 12.61 - TICK,
            "…at or above break-even: ${}",
            0.5 * (ask - bid)
        );
    }

    /// The GROUP-B override path (a non-`AsOptimal` half-spread source) runs the SAME ladder — it
    /// used to spell floor-then-cap by hand, which is precisely how it would have kept quoting at a
    /// loss after the fast path stopped. Glosten–Milgrom with `μ̂ = 0` reads a ZERO half-spread, so
    /// this also proves the refusal survives a source that contributes nothing of its own.
    #[test]
    fn the_group_b_override_path_refuses_on_the_same_bar() {
        let gb = |max_ticks: f64, fee: Option<f64>| AsParams {
            spread_source: SpreadSource::GlostenMilgrom,
            gm_mu: 0.0,
            ..prod2_params(max_ticks, fee)
        };
        assert_eq!(
            AsState::new(gb(60.0, Some(BYBIT_ROUND_TRIP))).price(&book(), 0.0, 0),
            None,
            "the Group-B path must refuse the the CI box mount too"
        );
        let (bid, ask) = AsState::new(gb(200.0, Some(BYBIT_ROUND_TRIP)))
            .price(&book(), 0.0, 0)
            .expect("a ceiling above break-even quotes on the Group-B path too");
        assert!(0.5 * (ask - bid) >= 12.61 - TICK, "…at break-even, not at the 2-tick floor");
    }

    /// `bounded_half_spread` in isolation — the three stages and their ORDER, which is the design.
    /// Stage 1 (the sub-tick floor) still BEATS the cap; stage 3 (break-even) does not, it refuses.
    #[test]
    fn the_width_ladder_orders_its_three_stages() {
        let t = 1.0;
        // No fee ⇒ the pre-existing ladder, unchanged: floor 2, cap 60, raw 75 ⇒ 60.
        assert_eq!(bounded_half_spread(75.0, t, 2.0, 60.0, 0.0), Some(60.0));
        // Stage 1 beats stage 2 (`max < min` ⇒ the floor governs) — UNCHANGED behaviour.
        assert_eq!(bounded_half_spread(0.0, t, 2.0, 1.0, 0.0), Some(2.0));
        // Stage 3 under the cap: it governs over both the raw value and the min floor.
        assert_eq!(bounded_half_spread(3.0, t, 2.0, 60.0, 12.61), Some(12.61));
        // Stage 3 ABOVE the cap: REFUSE, never `Some(60.0)` (the loss) and never `Some(12.61)` (the
        // cap violation).
        assert_eq!(bounded_half_spread(3.0, t, 2.0, 6.0, 12.61), None);
        // A fee floor EXACTLY at the cap is satisfiable, so it is not a refusal.
        assert_eq!(bounded_half_spread(3.0, t, 2.0, 12.61, 12.61), Some(12.61));
    }
}
