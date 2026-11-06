//! **The headless render suites, ONE test binary** (`--test headless`): every integration suite of
//! this crate that renders or judges a frame on the CPU — the suites that drive `chart::draw` or the
//! in-chart dialogs through the shared harness (`common`: the `Case` scenario builder, the fixtures,
//! the geometry reader; `dialog_harness`: the `egui_kittest` accessibility harness), the kill proofs
//! for `vike-ui-theme`'s frame helpers (`frame_record_gate`, `frame_sanity_gate`,
//! `pixel_liveness_gate`), and the indicator-continuation check.
//!
//! Grouped per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section: the harness
//! compiles ONCE and the crate links one test binary where it linked twelve. Every suite is a module
//! named after the file it was, so a test's name is unchanged after its module prefix and only the
//! `--test` slot moved: `cargo test -p vike-chart --test headless -- tessellation_goldens::`. Every
//! member is plain (no feature gate, no `#[ignore]`, no proptest sidecar, no
//! `env::set_var`/`set_current_dir`). `tests/libm_platform_probe.rs` stays its own binary: decision
//! 0074 keeps each crate's libm gate as its own `tests/libm_platform_probe.rs`, as `vike-sim` does.

mod common;
mod dialog_harness;

mod chart_chrome_a11y;
mod chart_geometry_gate;
mod chart_settings_a11y;
mod chart_sizes;
mod draw_characterization;
mod frame_record_gate;
mod frame_sanity_gate;
mod indicator_continuation;
mod indicator_settings_a11y;
mod options_chain_characterization;
mod pixel_liveness_gate;
mod tessellation_goldens;
