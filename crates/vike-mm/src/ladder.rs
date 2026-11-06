//! Ladder quoting — the PURE per-side desired-rung builder behind [`SpreadMaker`](crate::SpreadMaker)'s
//! N-order-per-side mode. Isolated here (like `skew`/`book`/`refresh`) so the whole geometry
//! unit-tests without a runtime or a broker. This is the PRICING half of the ladder (rungs → absolute
//! `(price, size)`); the DATA half — [`LadderParams`]/[`LadderLevel`] and the size-profile expansion
//! [`LadderParams::rungs`] — lives in `vike-model` so it can ride the typed live-params payload, the
//! exact same split `QuoteStyle` (data in vike-model) / `book::priced` (arithmetic here) already keeps.
//!
//! ## Contract
//!
//! [`desired_side`] maps a [`LadderParams`] onto ONE side of the book, given that side's level-0
//! context (the price + size today's single quote would use). Rung 0 is ALWAYS `(level0_px,
//! base_qty)` — so a laddered maker's innermost rung is bit-for-bit today's quote — and rung `k`
//! steps `k · offset_step` further OUT (away from the reservation) at a [`LadderSizeProfile`]-shaped
//! size. The step's absolute size is the ladder's [`LadderOffsetUnit`] scale (the level-0 half-spread,
//! or the venue tick) times `offset_step`.
//!
//! ## Guards (never place a nonsensical rung)
//!
//! - **No usable price scale** (`unit_scale <= 0` — a `Ticks` unit on an unknown grid, or a zero/
//!   crossed half-spread): deeper rungs would collapse onto level 0's price, so the ladder is cut to
//!   level 0 only.
//! - **A non-positive rung size** (a decaying `Linear` profile past zero, or a skew-floored base):
//!   the rung is not placeable, and both profiles are monotone in `k`, so the walk STOPS at the first
//!   one — the returned set is the contiguous placeable prefix `0..m` (`m <= levels`).
//!
//! Nothing here logs or allocates on the default path — it is only reached when the ladder is ACTIVE
//! (`levels >= 2`), an opt-in the single-quote maker never touches.

use vike_model::{LadderOffsetUnit, LadderParams};

/// The per-side desired rung set: `(price, size)` deepest-LAST, rung 0 first. `outward_sign` is
/// `-1.0` for the bid (rungs step DOWN in price) and `+1.0` for the ask (rungs step UP); `half` is
/// the level-0 half-spread (`(ask_px − bid_px)/2`) used by the `HalfSpread` unit, `tick` the venue
/// grid used by the `Ticks` unit. `base_qty` is that side's already-skewed level-0 size, so the
/// inventory skew flows through every rung's multiplier. PURE; see the module doc for the two guards.
pub(crate) fn desired_side(
    lp: LadderParams,
    level0_px: f64,
    outward_sign: f64,
    half: f64,
    tick: f64,
    base_qty: f64,
) -> Vec<(f64, f64)> {
    let unit_scale = match lp.offset_unit {
        LadderOffsetUnit::HalfSpread => half,
        LadderOffsetUnit::Ticks => tick,
    };
    let mut out = Vec::new();
    for (k, rung) in lp.rungs().into_iter().enumerate() {
        // no usable price step ⇒ deeper rungs would pile onto level 0's price; keep level 0 only.
        if k >= 1 && unit_scale <= 0.0 {
            break;
        }
        // rung 0's offset is 0.0, so `level0_px + sign·0·scale == level0_px` bit-for-bit (today's
        // price); deeper rungs step `k · offset_step · unit_scale` away from the reservation.
        let px = level0_px + outward_sign * rung.offset * unit_scale;
        let sz = base_qty * rung.size;
        // a non-placeable (<= 0) size — a decayed rung or a floored base; sizes are monotone in `k`,
        // so stop at the first, returning the placeable prefix.
        if sz <= 0.0 {
            break;
        }
        out.push((px, sz));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::LadderSizeProfile;

    fn params(
        levels: usize,
        offset_step: f64,
        offset_unit: LadderOffsetUnit,
        size_profile: LadderSizeProfile,
        size_ratio: f64,
    ) -> LadderParams {
        LadderParams { levels, offset_step, offset_unit, size_profile, size_ratio }
    }

    // The core geometry on the BID side: level 0 is today's quote bit-for-bit; each deeper rung
    // steps one `offset_step`-scaled half-spread DOWN; flat sizes with ratio 1.0.
    #[test]
    fn bid_rungs_step_down_in_half_spreads() {
        let lp = params(3, 1.0, LadderOffsetUnit::HalfSpread, LadderSizeProfile::Linear, 1.0);
        // level0 bid 99.6, half 0.5, base 1.0
        let d = desired_side(lp, 99.6, -1.0, 0.5, 0.0, 1.0);
        assert_eq!(d.len(), 3, "three rungs");
        // level 0 is EXACTLY today's (price, size)
        assert_eq!(d[0].0.to_bits(), 99.6_f64.to_bits(), "rung 0 = level-0 bid px, bit-for-bit");
        assert_eq!(d[0].1.to_bits(), 1.0_f64.to_bits(), "rung 0 = base qty, bit-for-bit");
        // rung k steps k·(offset_step·half) = k·0.5 DOWN
        assert_eq!(d[1].0.to_bits(), (99.6_f64 - 0.5).to_bits(), "rung 1 one half-spread down");
        assert_eq!(d[2].0.to_bits(), (99.6_f64 - 1.0).to_bits(), "rung 2 two half-spreads down");
        // flat sizes
        assert_eq!(d[1].1.to_bits(), 1.0_f64.to_bits(), "flat rung 1 size");
        assert_eq!(d[2].1.to_bits(), 1.0_f64.to_bits(), "flat rung 2 size");
    }

    // The ASK side is the mirror: rungs step UP by the same scale, symmetric about the reservation.
    #[test]
    fn ask_rungs_step_up_symmetrically() {
        let lp = params(3, 1.0, LadderOffsetUnit::HalfSpread, LadderSizeProfile::Linear, 1.0);
        let d = desired_side(lp, 100.6, 1.0, 0.5, 0.0, 1.0);
        assert_eq!(d[0].0.to_bits(), 100.6_f64.to_bits(), "rung 0 = level-0 ask px");
        assert_eq!(d[1].0.to_bits(), (100.6_f64 + 0.5).to_bits(), "rung 1 one half-spread up");
        assert_eq!(d[2].0.to_bits(), (100.6_f64 + 1.0).to_bits(), "rung 2 two half-spreads up");
    }

    // The `Ticks` unit steps by the venue grid, ignoring the half-spread; sizes can grow geometrically.
    #[test]
    fn ticks_unit_and_geometric_sizes() {
        let lp = params(3, 2.0, LadderOffsetUnit::Ticks, LadderSizeProfile::Geometric, 2.0);
        // tick 0.25, so each step is 2·0.25 = 0.5; half is ignored (pass a bogus 999.0 to prove it)
        let d = desired_side(lp, 99.6, -1.0, 999.0, 0.25, 1.0);
        assert_eq!(d[0].0.to_bits(), 99.6_f64.to_bits(), "rung 0 unchanged");
        assert_eq!(d[1].0.to_bits(), (99.6_f64 - 0.5).to_bits(), "rung 1 two ticks down");
        assert_eq!(d[2].0.to_bits(), (99.6_f64 - 1.0).to_bits(), "rung 2 four ticks down");
        // geometric sizes 1, 2, 4 off base 1.0
        assert_eq!(d[0].1.to_bits(), 1.0_f64.to_bits(), "size 1");
        assert_eq!(d[1].1.to_bits(), 2.0_f64.to_bits(), "size 2");
        assert_eq!(d[2].1.to_bits(), 4.0_f64.to_bits(), "size 4");
    }

    // The base size scales EVERY rung (so the inventory skew flows through): a base of 3.0 with
    // geometric ×2 gives 3, 6, 12.
    #[test]
    fn base_qty_scales_all_rungs() {
        let lp = params(3, 1.0, LadderOffsetUnit::HalfSpread, LadderSizeProfile::Geometric, 2.0);
        let d = desired_side(lp, 100.0, -1.0, 0.5, 0.0, 3.0);
        assert_eq!(d[0].1.to_bits(), 3.0_f64.to_bits(), "rung 0 = base");
        assert_eq!(d[1].1.to_bits(), 6.0_f64.to_bits(), "rung 1 = base·2");
        assert_eq!(d[2].1.to_bits(), 12.0_f64.to_bits(), "rung 2 = base·4");
    }

    // A decaying Linear ladder returns only the placeable prefix — the zero-and-below rungs are cut.
    #[test]
    fn decaying_sizes_truncate_to_placeable_prefix() {
        let lp = params(5, 1.0, LadderOffsetUnit::HalfSpread, LadderSizeProfile::Linear, 0.5);
        // sizes would be 1, 0.5, 0, -0.5, -1 → only the first two are placeable
        let d = desired_side(lp, 100.0, -1.0, 0.5, 0.0, 2.0);
        assert_eq!(d.len(), 2, "decayed rungs are dropped");
        assert_eq!(d[0].1.to_bits(), 2.0_f64.to_bits(), "rung 0 = base 2.0");
        assert_eq!(d[1].1.to_bits(), 1.0_f64.to_bits(), "rung 1 = base·0.5");
    }

    // No usable price scale (zero half-spread on a HalfSpread ladder, or no tick grid on a Ticks
    // ladder) collapses to level 0 only, so rungs never pile onto one price.
    #[test]
    fn no_usable_scale_collapses_to_level_zero() {
        let hs = params(4, 1.0, LadderOffsetUnit::HalfSpread, LadderSizeProfile::Linear, 1.0);
        assert_eq!(desired_side(hs, 100.0, -1.0, 0.0, 0.0, 1.0).len(), 1, "zero half-spread ⇒ 1");
        let tk = params(4, 1.0, LadderOffsetUnit::Ticks, LadderSizeProfile::Linear, 1.0);
        assert_eq!(desired_side(tk, 100.0, -1.0, 0.5, 0.0, 1.0).len(), 1, "no tick grid ⇒ 1");
    }

    // An off/1-level bag yields exactly today's single quote (rung 0 only) — the byte-identical floor.
    #[test]
    fn off_or_single_level_yields_only_todays_quote() {
        for levels in [0usize, 1] {
            let lp =
                params(levels, 1.0, LadderOffsetUnit::HalfSpread, LadderSizeProfile::Linear, 1.0);
            let d = desired_side(lp, 99.6, -1.0, 0.5, 0.0, 1.0);
            assert_eq!(d.len(), 1, "levels {levels} ⇒ one rung");
            assert_eq!(d[0].0.to_bits(), 99.6_f64.to_bits(), "and it is today's price");
            assert_eq!(d[0].1.to_bits(), 1.0_f64.to_bits(), "and today's size");
        }
    }
}
