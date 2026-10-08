//! Bakes in the CORRECT abi version and fingerprint — see `../good/build.rs`, including its
//! header on why the `rerun-if-env-changed` lines below are load-bearing rather than tidiness.
//! This fixture must load cleanly; only its `on_bar` dispatch is where it deliberately
//! misbehaves.

fn main() {
    println!("cargo:rerun-if-env-changed=VIKE_TEST_ABI_VERSION");
    println!("cargo:rerun-if-env-changed=VIKE_TEST_FINGERPRINT");
    let v = std::env::var("VIKE_TEST_ABI_VERSION").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=VIKE_TEST_ABI_VERSION={v}");
    let fp = std::env::var("VIKE_TEST_FINGERPRINT").unwrap_or_default();
    println!("cargo:rustc-env=VIKE_TEST_FINGERPRINT={fp}");
}
