//! The seeded series the suites of this binary share: one uniform stream and the random walk over it.

/// A fixed uniform sequence in `[-0.5, 0.5)` (no `rand` here: a 64-bit LCG, its start scrambled so that two
/// seeds a step apart give streams that share nothing).
pub fn noise(seed: u64, n: usize) -> Vec<f64> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0x1234_5678_9ABC_DEF1);
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect()
}

/// A random walk from zero whose steps are uniform in `[-0.5, 0.5)`.
pub fn walk(seed: u64, n: usize) -> Vec<f64> {
    let mut level = 0.0;
    noise(seed, n)
        .into_iter()
        .map(|u| {
            level += u;
            level
        })
        .collect()
}
