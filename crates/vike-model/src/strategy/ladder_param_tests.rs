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
