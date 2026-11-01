//! The arithmetic every `libm_platform_probe`'s runtime half hashes and draws its inputs with:
//! [`Fnv`], [`lcg`] and [`decade`]. Each probe held a private copy, and the copies' own docs required
//! them identical ("the same hasher the sibling probes use, so a row moved between crates hashes
//! identically"); one copy holds that by construction.
//!
//! What stays in each crate is decision 0074's line: the rows, the inputs they draw, their `PINNED`
//! hashes and the source gate. Nothing here calls a transcendental: every step is an integer op, an
//! exact `as f64` of a 53-bit integer, or one correctly rounded IEEE `*` or `/`, so a hash that moves
//! is the function under test's and never the generator's.

use vike_util::fnv::Fnv1a64;

/// [`Fnv1a64`] over `f64::to_bits`, NaN canonicalised: a NaN's sign and payload are not pinned by
/// the architecture, so hashing them raw would report a divergence that is not one.
#[derive(Default)]
pub struct Fnv(Fnv1a64);

impl Fnv {
    /// A hash over no samples yet.
    pub fn new() -> Self {
        Fnv(Fnv1a64::new())
    }

    /// Folds `v`'s eight little-endian bytes in.
    pub fn push(&mut self, v: f64) {
        let bits = if v.is_nan() { 0x7ff8_0000_0000_0000u64 } else { v.to_bits() };
        self.0.write(&bits.to_le_bytes());
    }

    /// The hash as 16 lowercase hex digits, the spelling every `PINNED` table records.
    pub fn hex(&self) -> String {
        format!("{:016x}", self.0.finish())
    }
}

/// Deterministic pseudo-random `f64` in `[0, 1)` from a pure-integer LCG: the top 53 bits of the
/// state over 2^53, an exact ratio of integers and then one correctly rounded divide, so the corpus
/// is bit-identical on every platform.
pub fn lcg(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
}

/// `10^k` built by repeated multiplication or division from `1.0`, pure IEEE ops only.
///
/// ⚠ For `k < 0` this is NOT the correctly rounded `10^k`: repeated division accumulates rounding.
/// A corpus needs a value that is deterministic and bit-identical on both boxes, which every `/` is
/// by IEEE 754; `powi`/`powf` would put a platform call inside the generator.
pub fn decade(k: i32) -> f64 {
    let mut v = 1.0f64;
    for _ in 0..k.abs() {
        if k > 0 {
            v *= 10.0;
        } else {
            v /= 10.0;
        }
    }
    v
}
