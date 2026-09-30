//! Bakes in the CORRECT abi version, passed in the environment by
//! `crates/vike-strategy-plugin/tests/load_refusals.rs`'s fixture builder — this fixture's
//! fingerprint is deliberately wrong regardless, so only that one value needs to agree with the
//! real crate for `loader::load` to reach the fingerprint check at all.
//!
//! ⚠ `rerun-if-env-changed` for the same reason as `../good/build.rs` (read its header). This
//! fixture is the least exposed of the three — a stale abi version would make it fail the
//! version check instead of the fingerprint check, which is a WRONG-REASON pass rather than a
//! failure, and therefore worse to leave unguarded, not better.

fn main() {
    println!("cargo:rerun-if-env-changed=VIKE_TEST_ABI_VERSION");
    let v = std::env::var("VIKE_TEST_ABI_VERSION").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=VIKE_TEST_ABI_VERSION={v}");
}
