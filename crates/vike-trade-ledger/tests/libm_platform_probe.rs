//! **The platform-invariance probe for this crate — the SOURCE half only.**
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`: IEEE 754
//! requires `+ - * /` and `sqrt` to be correctly rounded and NOTHING of `ln`, `exp`, `pow` or their
//! relatives, so `f64::powf` resolves to the PLATFORM's libm and the Windows and Linux boxes disagree
//! in the last bit. The cure is the `libm` CRATE, compiled into the binary.
//!
//! ⚠ **Why this file exists: the coverage MOVED with the code.** The ledger was `vike-analytics`'
//! `closed_trades` module, under `crates/vike-analytics/tests/libm_platform_probe.rs`'s walk of that
//! crate's `src/`. Without this walk a `.powf(` written here would ship into the CAGR and the monthly
//! ratios of every backtest report and every live mount unseen. No runtime table: the outputs are
//! pinned by the unit tests beside the code, as `crates/vike-math/tests/libm_platform_probe.rs` argues
//! for its own crate.
//!
//! Structure the walk rests on (2026-11-18): `src/` is FLAT; each `#[cfg(test)]` module is its
//! file's LAST item and lives in its own `*_tests.rs` file, so the shared walk skips them whole.
//!
//! ```text
//! <cargo> test -p vike-trade-ledger --test libm_platform_probe
//! ```

use vike_utility::libm_walk::{banned_needles, cfg_test_module_files_under, cfg_test_ranges};

/// Production code in this crate must reach a transcendental through the `libm` CRATE. Scope: every
/// `.rs` under `src/` MINUS its `#[cfg(test)]` items and the files a `#[cfg(test)] mod NAME;` pulls in.
/// The shared needle set's blind spots (`f64>::exp(`, a generic bound, a call wrapped over two lines)
/// occur nowhere here today — an enumeration, never a proof.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let test_files = cfg_test_module_files_under(&src);
    let (mut found, mut scanned, mut unclosed) = (Vec::new(), Vec::new(), Vec::new());
    for entry in std::fs::read_dir(&src).expect("crate has a src/ directory") {
        let path = entry.expect("readable dir entry").path();
        assert!(!path.is_dir(), "src/ grew a folder, which this walk does not read: {path:?}");
        if path.extension().and_then(|e| e.to_str()) != Some("rs") || test_files.contains(&path) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).expect("readable source file");
        let lines: Vec<&str> = text.lines().collect();
        let test_items = cfg_test_ranges(&lines);
        unclosed
            .extend(test_items.iter().filter(|t| !t.2).map(|t| format!("  {name}:{}", t.0 + 1)));
        for (i, line) in lines.iter().enumerate() {
            let in_test = test_items.iter().any(|&(start, end, _)| i >= start && i < end);
            if in_test || line.trim_start().starts_with("//") {
                continue;
            }
            if banned.iter().any(|pat| line.contains(pat.as_str())) {
                found.push(format!("  {name}:{} {}", i + 1, line.trim_start()));
            }
        }
        scanned.push(name);
    }
    // Non-vacuity: the walk opened both production files (`account.rs` calls `libm::pow`) and skipped
    // both test files whole.
    for must in ["lib.rs", "account.rs"] {
        assert!(scanned.iter().any(|s| s == must), "never opened {must}; scanned {scanned:?}");
    }
    assert!(
        test_files.len() >= 2 && !scanned.iter().any(|s| s.ends_with("_tests.rs")),
        "the `*_tests.rs` children must be skipped whole: derived {test_files:?}, scanned {scanned:?}"
    );
    assert!(
        unclosed.is_empty(),
        "a `#[cfg(test)]` item's end was not found:\n{}",
        unclosed.join("\n")
    );
    assert!(
        found.is_empty(),
        "production code must call the `libm` CRATE rather than `f64`'s platform-libm methods \
         (`x.exp()` and `f64::exp(x)` alike; `powi` too, which a Windows `dev` build makes a `pow()` \
         libcall):\n{}",
        found.join("\n")
    );
}
