//! The store arm of a fixture that plants no settings database — ONE spelling, included by
//! `#[path = "common/store_layer.rs"] mod store_layer;` in each test binary that hands one to
//! [`vike_config::boot_lines`] (a `tests/*.rs` file is the crate root of its own binary, so a bare
//! `mod` would resolve beside it).

use vike_config::StoreLayer;

/// [`StoreLayer::NotConsulted`] for a fixture that plants no settings database, its reason naming
/// the fixture's own file: pass `file!()`, the parameter that replaced each copy's hand-typed path.
///
/// [`vike_config::boot_lines`] takes a store arm since 2026-09-22 so a disclosure cannot describe a
/// source the process did not resolve from (`crates/vike-config/src/boot.rs`'s module doc).
/// `NotConsulted` is the arm it was hard-wired to before that, so an assertion over it measures the
/// rendering it was written against; `crates/vike-config/tests/boot.rs` proves that rendering, arm
/// by arm.
///
/// The arm holds a `&'static str` and the reason is built from `file` at run time, so each call
/// leaks one short string: a test process, bounded by its call count.
pub fn no_store(file: &str) -> StoreLayer<'static> {
    let why = format!("{file} — this fixture plants no settings database");
    StoreLayer::NotConsulted(Box::leak(why.into_boxed_str()))
}
