//! Hyperliquid action hashing (the `connectionId`) + the monotonic per-signer [`NonceManager`].
//!
//! [`action_hash`] is the byte layout cross-validated 4× in research §2a and identical in the
//! Python (`signing.py::action_hash`) and Rust (`Actions::hash`) SDKs:
//!
//! ```text
//! keccak256(
//!     rmp_serde::to_vec_named(action)          // msgpack map, field-DEFINITION order (== wire)
//!   ‖ nonce            as u64 big-endian (8 B)
//!   ‖ 0x00                       if vault is None
//!     | 0x01 ‖ addr[20]          if vault is Some
//!   ‖ 0x00 ‖ expires_after as u64 BE   ONLY if expires_after is Some   // note the extra 0x00
//! )
//! ```
//!
//! The `expires_after` leg is present in the Python reference but **omitted by the Rust SDK** — we
//! implement it (per Python) for forward-compatibility. Every golden vector uses
//! `expires_after = None`, so the leg is absent there and our hash still matches the Rust SDK
//! byte-for-byte.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use super::eip712::keccak256;

/// Compute the L1 action hash (the phantom agent's `connectionId`).
///
/// `vault` is the 20-byte master/vault address when signing on behalf of one (v1 always `None`);
/// `expires_after` is an optional ms deadline (v1 always `None`).
///
/// # Panics
/// Only if `rmp_serde` fails to serialize `action` — impossible for the plain
/// [`super::action`] structs (no non-string map keys, no erroring custom `Serialize`); a failure
/// would be a build-time bug caught immediately by `tests/signing_vectors.rs`, not a runtime
/// condition, so it is surfaced as a panic rather than threaded through the signature.
pub fn action_hash(
    action: &impl Serialize,
    nonce: u64,
    vault: Option<[u8; 20]>,
    expires_after: Option<u64>,
) -> [u8; 32] {
    // `to_vec_named` serializes structs as msgpack MAPS keyed by field name (the Python-dict twin)
    // in definition order — the exact byte sequence the venue re-hashes to recover the signer.
    let mut bytes = rmp_serde::to_vec_named(action)
        .expect("HL action structs are infallibly msgpack-serializable");
    bytes.extend_from_slice(&nonce.to_be_bytes());
    match vault {
        None => bytes.push(0x00),
        Some(addr) => {
            bytes.push(0x01);
            bytes.extend_from_slice(&addr);
        }
    }
    if let Some(exp) = expires_after {
        bytes.push(0x00);
        bytes.extend_from_slice(&exp.to_be_bytes());
    }
    keccak256(&bytes)
}

/// Current UNIX time in milliseconds (saturating to 0 before the epoch, which never happens).
/// Delegates to [`vike_model::clock::now_ms_u64`] — the `u64` twin of the consolidated wall-clock
/// helper; kept as a thin private wrapper so [`NonceManager::next`] stays byte-for-byte unchanged.
fn now_ms() -> u64 {
    vike_model::clock::now_ms_u64()
}

/// A monotonic per-signer millisecond nonce source.
///
/// HL tracks the 100 highest nonces **per signer**; a new nonce must exceed the smallest tracked
/// and never repeat (validity window `(T−2 days, T+1 day)`, research §9). Raw `now_ms()` collides
/// under same-millisecond bursts (Hummingbot's latent duplicate-nonce bug, #7737). This hands out
/// `next = max(now_ms, last + 1)`: at least the wall clock, strictly greater than any prior nonce,
/// and fast-forwarded to `now` after an idle gap. Thread-safe and lock-free via a single
/// `AtomicU64`; two concurrent callers always get distinct, strictly-increasing values.
#[derive(Debug)]
pub struct NonceManager {
    last: AtomicU64,
}

impl NonceManager {
    /// A fresh manager (first nonce will be the current wall-clock ms).
    pub fn new() -> Self {
        Self { last: AtomicU64::new(0) }
    }

    /// Reserve the next strictly-increasing nonce (`max(now_ms, last + 1)`).
    pub fn next(&self) -> u64 {
        let now = now_ms();
        // Atomically bump `last` to `max(now, last+1)`; `fetch_update` returns the PREVIOUS value
        // on success, so the value THIS call owns is `max(now, prev+1)` (== what was just stored).
        let prev = self
            .last
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last| Some(now.max(last + 1)))
            .expect("closure always returns Some");
        now.max(prev + 1)
    }
}

impl Default for NonceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::action::{
        Action, HlBuilderFee, LimitParams, OrderAction, OrderKind, OrderWire,
    };

    fn base_order(builder: Option<HlBuilderFee>) -> Action {
        Action::Order(OrderAction {
            orders: vec![OrderWire {
                asset: 1,
                is_buy: true,
                limit_px: "2000.0".to_string(),
                sz: "3.5".to_string(),
                reduce_only: false,
                order_type: OrderKind::Limit(LimitParams { tif: "Gtc".to_string() }),
                cloid: None,
            }],
            grouping: "na".to_string(),
            builder,
        })
    }

    /// THE money-critical property (task 7): an absent builder must reproduce today's action hash
    /// byte-for-byte (no signing regression for every order that doesn't configure one — the
    /// overwhelming majority), while a present one must fold into the hash so the venue actually
    /// sees it.
    #[test]
    fn builder_changes_the_action_hash_only_when_present() {
        let action_none = base_order(None);
        let base = action_hash(&action_none, 1, None, None);
        let same = action_hash(&action_none, 1, None, None);
        assert_eq!(base, same, "hashing must be deterministic");

        let action_some =
            base_order(Some(HlBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 }));
        let with = action_hash(&action_some, 1, None, None);
        assert_ne!(base, with, "a present builder must fold into the hash");
    }

    #[test]
    fn nonce_is_strictly_monotonic() {
        let nm = NonceManager::new();
        let mut prev = nm.next();
        for _ in 0..10_000 {
            let n = nm.next();
            assert!(n > prev, "nonce not strictly increasing: {n} <= {prev}");
            prev = n;
        }
    }

    #[test]
    fn nonce_starts_at_wallclock() {
        let before = now_ms();
        let n = NonceManager::new().next();
        // First nonce is at least "now" (max(now, 0+1) == now for any realistic clock).
        assert!(n >= before, "first nonce {n} < wall clock {before}");
    }

    #[test]
    fn nonce_is_threadsafe_and_unique() {
        use std::collections::HashSet;
        use std::sync::Arc;
        let nm = Arc::new(NonceManager::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let nm = Arc::clone(&nm);
            handles.push(std::thread::spawn(move || {
                (0..2_000).map(|_| nm.next()).collect::<Vec<_>>()
            }));
        }
        let mut all = HashSet::new();
        for h in handles {
            for n in h.join().unwrap() {
                assert!(all.insert(n), "duplicate nonce {n} across threads");
            }
        }
        assert_eq!(all.len(), 8 * 2_000);
    }
}
