//! The planted-fixture self-test for [`vike_model::libm_walk`] — ONE copy, replacing the ten that
//! stood in ten crates' `libm_platform_probe.rs` before decision 0074.
//!
//! ⚠ **Why it is HERE and not beside the parser it tests.** Every fixture row below is a string
//! literal that CONTAINS a banned needle as text. That is safe for exactly one reason: this file
//! sits under `tests/`, and every crate's `production_code_calls_libm_not_the_platform` reads
//! `src/` only. `crates/vike-model/src/libm_walk.rs` is under `src/`, and the
//! `any(test, feature = "...")` gate it wears is deliberately NOT one of `cfg_test_ranges`'s
//! test-item markers — that spelling marks code which SHIPS when its feature is on. So a fixture
//! moved in beside the parser would be scanned as production and this crate's own gate would fail
//! on it. The split is load-bearing, not tidiness.
//!
//! ⚠ **And this cannot be replaced by the tree-shaped evidence each gate already collects.** That
//! evidence is real, but it is about particular files AS THEY ARE TODAY — reorganise one so its
//! test modules go last and the assertion passes vacuously while the mechanism goes unproven.
//! This fixture holds the mechanism itself, and every property the walk needs: production before a
//! test module, a banned call INSIDE it, production after it, the semicolon header form, the
//! `all(test, ...)` spelling, and the `any(test, ...)` spelling that must NOT be excluded because
//! it ships.

use vike_model::libm_walk::{banned_needles, cfg_test_ranges};

#[test]
fn the_test_module_cut_excludes_only_the_test_module() {
    // Every fixture row is a string literal, so the needles inside them are data rather than
    // calls; this file lives under `tests/`, which no crate's scan reads. See the module doc.
    let file = [
        "//! House style: naïve f64 folds, pure, in-file `#[cfg(test)]`.",
        "fn shipped_before() -> f64 {",
        "    x.exp()",
        "}",
        "",
        "#[cfg(test)]",
        "mod first_tests {",
        "    fn fixture() -> f64 {",
        "        y.ln()",
        "    }",
        "}",
        "",
        "fn shipped_after() -> f64 {",
        "    f64::powf(z, 2.0)",
        "}",
        "",
        "#[cfg(all(test, feature = \"whatever\"))]",
        "mod gated_tests {",
        "    fn fixture() -> f64 {",
        "        y.log10()",
        "    }",
        "}",
        "",
        "#[cfg(any(test, feature = \"test-support\"))]",
        "pub fn a_shipped_double() -> f64 {",
        "    q.tanh()",
        "}",
        "",
        "#[cfg(test)]",
        "mod tests;",
        "",
        "fn shipped_last() -> f64 {",
        "    w.sin_cos().0",
        "}",
    ];
    let items = cfg_test_ranges(&file);
    assert_eq!(items.len(), 3, "all three TEST-ONLY items must be found: {items:?}");
    assert_eq!(items[0], (5, 11, true), "the braced module spans its own lines only");
    assert_eq!(items[1], (16, 22, true), "the `all(test, …)` module is test-only and is a range");
    assert_eq!(items[2], (28, 30, true), "the `mod tests;` declaration ends at its semicolon");
    // The control arm: the module doc on line 1 NAMES the attribute in backticks. If prose could
    // open a range, `items[0]` would start at 0 and every assertion here would still pass while
    // the gate scanned nothing at all.
    assert_eq!(items[0].0, 5, "a `#[cfg(test)]` inside a comment must not open a range");

    let banned = banned_needles();
    let hits: Vec<usize> = file
        .iter()
        .enumerate()
        .filter(|(i, line)| {
            !items.iter().any(|&(s, e, _)| *i >= s && *i < e)
                && !line.trim_start().starts_with("//")
                && banned.iter().any(|p| line.contains(p.as_str()))
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits,
        [2, 13, 25, 32],
        "the scan must see the production call BEFORE the first test module (line 3), the one \
         AFTER it (line 14), the one inside the `any(test, …)` item that SHIPS (line 26) — this \
         crate's `MockBroker`, and `libm_walk` itself, are exactly that shape — and the one after \
         the `mod tests;` declaration (line 33); and must NOT see either fixture call (lines 9 \
         and 20). A `break` at the first marker sees only line 3. Saw (zero-based): {hits:?}"
    );
}

/// The shared list is a RATCHET in one direction only: a name may JOIN it, and a name leaving it
/// is a claim that the platform is now required to agree about that function, which IEEE 754 does
/// not say of any of them.
///
/// ⚠ This is not a restatement of the length. It pins the two properties every caller's gate
/// depends on and neither of which the length can see: that `sqrt` is ABSENT (IEEE 754 requires
/// it correctly rounded, so there is nothing to convert it to, and banning it would send eleven
/// crates hunting for a cure that does not exist), and that each name renders into BOTH spellings
/// — the method one and the fully-qualified one — because a gate that knows only the first walks
/// straight past the second.
#[test]
fn the_shared_list_keeps_the_two_properties_every_gate_rests_on() {
    let needles = banned_needles();
    assert!(
        !vike_model::libm_walk::BANNED_FNS.contains(&"sqrt"),
        "`sqrt` must stay absent: IEEE 754 requires it correctly rounded, so there is no libm \
         crate call to convert it TO and every caller would be sent after a cure that does not \
         exist"
    );
    assert_eq!(
        needles.len(),
        vike_model::libm_walk::BANNED_FNS.len() * 2,
        "every name must render into BOTH spellings; a gate that knows only the method form walks \
         straight past the fully-qualified one, which is the same inherent method and the same \
         libcall"
    );
    for name in vike_model::libm_walk::BANNED_FNS {
        assert!(
            needles.contains(&format!(".{name}(")) && needles.contains(&format!("f64::{name}(")),
            "{name} is missing one of its two spellings"
        );
    }
}
