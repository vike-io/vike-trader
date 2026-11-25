//! Bakes in the CORRECT abi version, fingerprint and layout (`ABI_VERSION` 14), and since 15 the
//! compatibility record (`$OUT_DIR/identity.rs`; every fixture runs this script), passed in the environment by
//! `crates/vike-rust-strategy/tests/load_refusals.rs`'s fixture builder — this fixture must
//! load cleanly, so both values must agree with the real crate's. The `panicking` fixture runs
//! this same file (its `Cargo.toml`'s `build = "../good/build.rs"`): it must load cleanly too, and
//! only its `on_bar` dispatch is where it deliberately misbehaves.
//!
//! ⚠ **`rerun-if-env-changed` is load-bearing, and its absence was a live defect.** A build
//! script with no `rerun-if-*` directive at all is rerun when a FILE in the package changes and
//! at no other time — cargo does not track the environment a script read. So when the outer test
//! rebuilt this fixture with a DIFFERENT `VIKE_TEST_FINGERPRINT` (a debug test binary after a
//! release one, or the reverse), cargo considered the script fresh, kept the previous
//! `rustc-env` value, and produced a strategy file carrying the OTHER profile's fingerprint. The loader
//! then refused it — correctly, and for a reason that had nothing to do with the test.
//! MEASURED on this branch: `a_well_formed_strategy_file_loads_successfully` and
//! `a_panicking_strategy_file_surfaces_an_error_and_does_not_abort_the_process` both failed with
//! `FingerprintMismatch { host: "…profile=debug…", strategy file: "…profile=release…" }` the first time
//! this crate's tests were run under two profiles against one cache. The outer content-hash
//! marker HAD invalidated correctly; it was cargo's own build-script freshness underneath it
//! that had not.

fn main() {
    // See this file's header: without these lines a changed value is silently ignored.
    println!("cargo:rerun-if-env-changed=VIKE_TEST_ABI_VERSION");
    println!("cargo:rerun-if-env-changed=VIKE_TEST_FINGERPRINT");
    println!("cargo:rerun-if-env-changed=VIKE_TEST_LAYOUT");
    let v = std::env::var("VIKE_TEST_ABI_VERSION").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=VIKE_TEST_ABI_VERSION={v}");
    let fp = std::env::var("VIKE_TEST_FINGERPRINT").unwrap_or_default();
    println!("cargo:rustc-env=VIKE_TEST_FINGERPRINT={fp}");
    let layout = std::env::var("VIKE_TEST_LAYOUT").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=VIKE_TEST_LAYOUT={layout}");

    // The compatibility record (`ABI_VERSION` 15), as the outer test encoded it with the real crate's
    // `vike_rust_strategy::identity::record` (hex): written as the `.vike_identity` static and the export
    // that keeps it linked, the shape the cdylib template bakes, into `$OUT_DIR/identity.rs`, which every
    // fixture's `lib.rs` includes. Empty: no record at all (the fixture a missing record is refused on).
    println!("cargo:rerun-if-env-changed=VIKE_TEST_IDENTITY");
    let hex = std::env::var("VIKE_TEST_IDENTITY").unwrap_or_default();
    let bytes: Vec<String> = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("VIKE_TEST_IDENTITY is hex"))
        .map(|b| b.to_string())
        .collect();
    let code = if bytes.is_empty() {
        String::new()
    } else {
        format!(
            "#[used]\n#[link_section = \".vike_identity\"]\nstatic VIKE_IDENTITY: [u8; {}] = [{}];\n\
             #[no_mangle]\npub extern \"C\" fn vike_strategy_identity() -> *const u8 {{\n    \
             VIKE_IDENTITY.as_ptr()\n}}\n",
            bytes.len(),
            bytes.join(", ")
        )
    };
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("identity.rs"), code).expect("write identity.rs");
}
