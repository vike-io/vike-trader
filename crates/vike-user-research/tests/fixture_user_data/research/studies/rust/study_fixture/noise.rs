//! The seeded noise the fixture studies' synthetic markets are drawn from, ONE copy: `funding_carry`'s and
//! `pairs_reversion`'s `fixture_store.rs` each include it by `#[path]`, inside their `#[cfg(test)]` module, so it
//! compiles in the test binaries that compile a study (`crates/vike-user-research/tests/pipeline.rs`,
//! `crates/vike-strategy-sandbox/tests/study_child.rs`) and never in a study's jailed build, which leaves that module
//! out. Not a study: this folder holds no `study_fixture.rs`, so nothing takes it for one.

/// A 64-bit LCG, and an approximately standard normal draw from twelve of its uniforms.
pub struct Noise(pub u64);

impl Noise {
    /// The next uniform in `[0, 1)`: the top 53 bits of the next state.
    pub fn uniform(&mut self) -> f64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Twelve uniforms summed, less six (the Irwin–Hall approximation of a standard normal).
    pub fn normal(&mut self) -> f64 {
        let mut total = 0.0;
        for _ in 0..12 {
            total += self.uniform();
        }
        total - 6.0
    }
}
