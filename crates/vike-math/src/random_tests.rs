use super::*;

/// The published SplitMix64 stream for seed 0 (the reference `splitmix64.c` of Vigna): the generator
/// is the canonical one, not a look-alike.
#[test]
fn the_generator_is_splitmix64() {
    let mut generator = SplitMix64::new(0);
    for want in [
        0xE220_A839_7B1D_CDAF_u64,
        0x6E78_9E6A_A1B9_65F4,
        0x06C4_5D18_8009_454F,
        0xF88B_B8A8_724C_81EC,
    ] {
        assert_eq!(generator.next_u64(), want);
    }
}

#[test]
fn one_seed_is_one_stream_and_the_unit_draw_is_in_range() {
    let (mut a, mut b) = (SplitMix64::new(12345), SplitMix64::new(12345));
    for _ in 0..128 {
        assert_eq!(a.next_u64(), b.next_u64(), "same seed must yield the same stream");
    }
    let mut generator = SplitMix64::new(1);
    for _ in 0..1000 {
        let x = generator.next_unit();
        assert!((0.0..1.0).contains(&x), "next_unit out of [0, 1): {x}");
    }
}

/// The two Fisher–Yates loops this one replaced (the typed-frame kit's, over its own SplitMix64
/// step, and the permutation importance's) spelled out: the same draws in the same order, so the
/// same permutation for every seed and length.
#[test]
fn the_shuffle_is_the_loop_it_replaced() {
    fn replaced(values: &mut [usize], seed: u64) {
        let mut state = seed;
        for i in (1..values.len()).rev() {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            let j = ((z ^ (z >> 31)) % (i as u64 + 1)) as usize;
            values.swap(i, j);
        }
    }
    for seed in 0..64_u64 {
        for len in 0..20_usize {
            let mut got: Vec<usize> = (0..len).collect();
            let mut want = got.clone();
            shuffle(&mut got, &mut SplitMix64::new(seed));
            replaced(&mut want, seed);
            assert_eq!(got, want, "seed {seed}, length {len}");
            got.sort_unstable();
            assert_eq!(got, (0..len).collect::<Vec<_>>(), "a shuffle is a permutation");
        }
    }
}

/// The expression both copies spelled (`(-2.0 * libm::log(u1)).sqrt() * libm::cos(TAU * u2)`), to
/// the bit, and the transform's fixed point.
#[test]
fn box_muller_is_the_expression_it_replaced() {
    let mut generator = SplitMix64::new(7);
    for _ in 0..1000 {
        let u1 = generator.next_unit().max(f64::MIN_POSITIVE);
        let u2 = generator.next_unit();
        let want = (-2.0 * libm::log(u1)).sqrt() * libm::cos(std::f64::consts::TAU * u2);
        assert_eq!(box_muller(u1, u2).to_bits(), want.to_bits(), "u1 {u1}, u2 {u2}");
    }
    assert_eq!(box_muller(1.0, 0.3), 0.0, "u1 = 1 has radius zero");
}

#[test]
fn the_draw_is_the_first_bin_strictly_above_u() {
    let cumulative = [0.25, 0.5, 1.0];
    assert_eq!(draw_from_cumulative(&cumulative, 0.0), 0);
    assert_eq!(draw_from_cumulative(&cumulative, 0.249), 0);
    assert_eq!(draw_from_cumulative(&cumulative, 0.25), 1, "a boundary belongs to the next bin");
    assert_eq!(draw_from_cumulative(&cumulative, 0.5), 2);
    assert_eq!(draw_from_cumulative(&cumulative, 0.999), 2);
    // A running total that ends below 1 leaves the shortfall to the last bin.
    assert_eq!(draw_from_cumulative(&[0.2, 0.9], 0.95), 1);
    // A first bin of probability zero is never drawn, even by `u = 0`.
    assert_eq!(draw_from_cumulative(&[0.0, 1.0], 0.0), 1);
}

#[test]
#[should_panic(expected = "at least one bin")]
fn an_empty_distribution_is_refused() {
    draw_from_cumulative(&[], 0.5);
}
