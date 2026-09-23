//! Bakes in the CORRECT abi version and fingerprint, passed in the environment by
//! `crates/vike-strategy-plugin/tests/load_refusals.rs`'s fixture builder — this fixture must
//! load cleanly, so both values must agree with the real crate's.
//!
//! ⚠ **`rerun-if-env-changed` is load-bearing, and its absence was a live defect.** A build
//! script with no `rerun-if-*` directive at all is rerun when a FILE in the package changes and
//! at no other time — cargo does not track the environment a script read. So when the outer test
//! rebuilt this fixture with a DIFFERENT `VIKE_TEST_FINGERPRINT` (a debug test binary after a
//! release one, or the reverse), cargo considered the script fresh, kept the previous
//! `rustc-env` value, and produced a plugin carrying the OTHER profile's fingerprint. The loader
//! then refused it — correctly, and for a reason that had nothing to do with the test.
//! MEASURED on this branch: `a_well_formed_plugin_loads_successfully` and
//! `a_panicking_plugin_surfaces_an_error_and_does_not_abort_the_process` both failed with
//! `FingerprintMismatch { host: "…profile=debug…", plugin: "…profile=release…" }` the first time
//! this crate's tests were run under two profiles against one cache. The outer content-hash
//! marker HAD invalidated correctly; it was cargo's own build-script freshness underneath it
//! that had not.

fn main() {
    // See this file's header: without these two lines a changed value is silently ignored.
    println!("cargo:rerun-if-env-changed=VIKE_TEST_ABI_VERSION");
    println!("cargo:rerun-if-env-changed=VIKE_TEST_FINGERPRINT");
    let v = std::env::var("VIKE_TEST_ABI_VERSION").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=VIKE_TEST_ABI_VERSION={v}");
    let fp = std::env::var("VIKE_TEST_FINGERPRINT").unwrap_or_default();
    println!("cargo:rustc-env=VIKE_TEST_FINGERPRINT={fp}");
}
