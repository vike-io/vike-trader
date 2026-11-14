//! **The platform-invariance gate for the order language: the SOURCE half only.**
//!
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`: IEEE 754
//! requires `+ - * /` and `sqrt` to be correctly rounded and nothing of `ln`, `exp`, `pow` or their
//! relatives, so `f64::ln` is the PLATFORM's and the Windows and Linux boxes disagree in the last
//! bit. The vocabulary strategy files share (`sizing`'s floors, moved here from the strategy gallery,
//! plan 2b C3) works sizes out that decide orders, so it takes them from the `libm` CRATE.
//!
//! No runtime corpus: every rule written in this language is pinned bit for bit by the strategy
//! lab's per-row fill dumps (`crates/vike-tactics/CLAUDE.md` has the procedure). What a dump cannot
//! give is the scan below, which stops the next edit from bringing a platform call back.

use vike_model::libm_walk::{banned_needles, cfg_test_module_files_under, cfg_test_ranges};

/// Every `.rs` under `src/`, minus each file's `#[cfg(test)]` items, calls no platform
/// transcendental: `libm::log` for `.ln()`, `libm::pow` for `.powf()`, and a multiplication for a
/// square. `sqrt` stays: it is correctly rounded everywhere.
///
/// ⚠ The needles' blind spots (`vike_model::libm_walk::banned_needles`): the angle-bracket
/// spelling, a call through a generic bound, and a call split across two lines. None occurs under
/// this crate's `src/` today (no `f64>::`, no `Float` bound), an enumeration rather than a proof.
#[test]
fn production_code_calls_libm_not_the_platform() {
    let banned = banned_needles();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let test_files = cfg_test_module_files_under(&src);
    let (mut found, mut scanned, mut unclosed) = (Vec::new(), Vec::new(), Vec::new());
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a readable source directory") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") || test_files.contains(&path)
            {
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            let text = std::fs::read_to_string(&path).expect("a readable source file");
            let lines: Vec<&str> = text.lines().collect();
            let test_items = cfg_test_ranges(&lines);
            for &(start, _, closed) in &test_items {
                if !closed {
                    unclosed.push(format!("  {rel}:{}", start + 1));
                }
            }
            for (i, line) in lines.iter().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//")
                    || test_items.iter().any(|&(start, end, _)| i >= start && i < end)
                {
                    continue;
                }
                if banned.iter().any(|pat| line.contains(pat.as_str())) {
                    found.push(format!("  {rel}:{} {code}", i + 1));
                }
            }
            scanned.push(rel);
        }
    }
    // Non-vacuity: the files the floors and the frames live in were read.
    for must in ["src/lib.rs", "src/sizing.rs", "src/helpers.rs", "src/higher.rs"] {
        assert!(
            scanned.iter().any(|s| s == must),
            "the scan never read {must} ({} files)",
            scanned.len()
        );
    }
    assert!(
        unclosed.is_empty(),
        "a `#[cfg(test)]` item's end could not be located, so the rest of its file went unscanned:\n{}",
        unclosed.join("\n")
    );
    assert!(
        found.is_empty(),
        "the order language takes its transcendentals from the `libm` crate (`libm::log`, `libm::pow`, \
         `libm::exp`, ...), and a square as a multiplication, so its numbers are the same on every \
         platform (decision 0032):\n{}",
        found.join("\n")
    );
}
