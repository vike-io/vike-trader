//! FNV-1a, 64-bit: the one spelling of the non-cryptographic hash the workspace uses as a CHANGE
//! DETECTOR and an IDENTITY TAG (decision 0141).
//!
//! Its outputs are PERSISTED and compared across processes, restarts and upgrades: the backtest
//! search's profile fingerprint in `trials.jsonl` (`--resume`), the incident bundle's profile hash,
//! the engine's `state_hash` and the conditional books' hash in the journal's `Snap`, the
//! Hyperliquid settlement fill id, the core snapshot's accounts epoch. So the algorithm is FROZEN:
//! offset basis `0xcbf2_9ce4_8422_2325`, prime `0x0000_0100_0000_01b3`, xor-then-multiply per byte,
//! wrapping. `std::hash::DefaultHasher` is not a substitute: it is documented as unstable across Rust
//! releases. It is not a security primitive, and nothing here pretends it is.
//!
//! Two doors over one state:
//! - [`fnv1a64`] / [`fnv1a64_hex`] for one byte slice;
//! - [`Fnv1a64`] for a digest fed in pieces (several fields, or keys with terminators) without
//!   gathering them into a buffer first. Feeding pieces is the same as hashing their concatenation.

/// FNV-1a 64 offset basis.
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 prime.
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// An FNV-1a 64 digest in progress. [`Fnv1a64::write`] every piece in order, then
/// [`Fnv1a64::finish`]; the value equals [`fnv1a64`] over the pieces concatenated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fnv1a64(u64);

impl Fnv1a64 {
    /// A digest over no bytes yet: the offset basis.
    #[inline]
    pub const fn new() -> Self {
        Self(OFFSET)
    }

    /// Fold `bytes` into the digest.
    #[inline]
    pub fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
        self.0 = h;
    }

    /// The digest of every byte written so far.
    #[inline]
    pub const fn finish(self) -> u64 {
        self.0
    }
}

impl Default for Fnv1a64 {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a 64 of `bytes`.
#[inline]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = Fnv1a64::new();
    h.write(bytes);
    h.finish()
}

/// [`fnv1a64`] of `bytes` as exactly 16 lowercase hex digits, zero-padded (`format!("{:016x}")`).
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a64(bytes))
}

#[cfg(test)]
#[path = "fnv_tests.rs"]
mod tests;
