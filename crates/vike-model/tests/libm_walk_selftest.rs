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

use std::path::PathBuf;

use vike_model::libm_walk::{
    banned_needles, cfg_test_module_files, cfg_test_ranges, fold_test_module_files,
    reinline_test_modules,
};

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

/// `cfg_test_module_files` resolves every shape a `#[cfg(test)] mod NAME;` declaration takes in
/// this tree — and nothing else. Planted in memory, because a fixture file under `src/` would be
/// read by every crate's scan.
#[test]
fn a_declared_test_module_file_is_found_and_nothing_else_is() {
    let src = PathBuf::from("crates/x/src");
    let sources = vec![
        // A non-mod-rs parent with an explicit `#[path]` ABOVE the marker (the Task 8 shape), and
        // a second declaration with the marker on the same line as `mod`.
        (
            src.join("server.rs"),
            "pub fn serve() {}\n\n#[path = \"server_tests.rs\"]\n#[cfg(test)]\nmod server_tests;\n\
             #[cfg(test)] mod other;\n"
                .to_string(),
        ),
        // A `mod.rs` parent without `#[path]`: the file sits beside it.
        (src.join("harness/mod.rs"), "#[cfg(test)]\nmod harness_tests;\n".to_string()),
        // An `all(test, …)` marker with a further attribute BELOW it.
        (
            src.join("lib.rs"),
            "#[cfg(all(test, feature = \"x\"))]\n#[allow(dead_code)]\npub mod gated_tests;\n"
                .to_string(),
        ),
        // Every visibility spelling of the declaration: `pub(crate)` with an explicit path (this
        // task's `egress_tests` shape) and `pub(super)` without one.
        (
            src.join("egress.rs"),
            "#[path = \"egress_tests.rs\"]\n#[cfg(test)]\npub(crate) mod egress_tests;\n\
             #[cfg(test)]\npub(super) mod scoped;\n"
                .to_string(),
        ),
        // `#[path]` BELOW the marker. rustc reads the whole attribute block in either order, and the
        // vike-core layout split (#2493) wrote eight real declarations this way, against the house
        // order (path ABOVE), until they were normalised (2026-10-05) — so this row is now the only
        // thing that pins the order. A resolver that looked only above the marker would answer the
        // implicit `below/below_tests.rs` and miss the real file.
        (
            src.join("below.rs"),
            "#[cfg(test)]\n#[path = \"below_tests.rs\"]\nmod below_tests;\n".to_string(),
        ),
        // Must NOT count: an inline test module, an `any(test, …)` module (it ships), a prose
        // mention, and an ordinary declaration.
        (
            src.join("clean.rs"),
            "// see #[cfg(test)] mod prose;\n#[cfg(test)]\nmod inline {\n}\n\
             #[cfg(any(test, feature = \"y\"))]\nmod ships;\nmod ordinary;\n\
             pub(crate) mod ordinary_pub;\n"
                .to_string(),
        ),
    ];
    let found = cfg_test_module_files(&sources);
    for want in [
        src.join("server_tests.rs"),
        src.join("server/other.rs"),
        src.join("harness/harness_tests.rs"),
        src.join("gated_tests.rs"),
        src.join("egress_tests.rs"),
        src.join("egress/scoped.rs"),
        src.join("below_tests.rs"),
    ] {
        assert!(found.contains(&want), "{} was not found in {found:?}", want.display());
    }
    assert!(
        !found.contains(&src.join("below/below_tests.rs")),
        "a `#[path]` below the marker is still the declaration's path; the implicit candidate must \
         not be answered beside it: {found:?}"
    );
    for never in ["prose", "inline", "ships", "ordinary", "ordinary_pub"] {
        assert!(
            !found.iter().any(|p| p.to_string_lossy().contains(never)),
            "`{never}` must not be classified as a test module file: {found:?}"
        );
    }
}

/// The rows three `vike-ops` gates pinned against their OWN copies of this resolver, ported here
/// when `clock_pin`, `compile_time_path_gate` and `system_temp_gate` switched to
/// `vike_model::libm_walk::cfg_test_module_rel_files` (2026-10-05) — through that string-keyed entry
/// point, because it is the one those gates call. One row was NOT ported: a `#[cfg(test)]` +
/// `mod NAME;` pair quoted inside a raw string must not count. The copies satisfied it with a
/// code-or-string line mask this resolver does not have;
/// `crates/vike-ops/tests/hygiene/clock_pin/call_scanner.rs`'s `observed_clock_reads` declares that
/// residual.
///
/// ⚠ Every fixture below is ONE line with `\n` escapes. This file sits inside the clock gate's walk,
/// which reads it through this very resolver, and that resolver has no string mask: a `\`-continued
/// literal whose next line STARTED with the attribute and ENDED in `mod NAME;` would be read as a
/// real declaration of this file's.
#[test]
fn the_rows_the_retired_gate_copies_pinned_still_hold() {
    use vike_model::libm_walk::cfg_test_module_rel_files;
    let derive =
        |rel: &str, text: &str| cfg_test_module_rel_files(&[(rel.to_string(), text.to_string())]);

    // A `mod.rs` declarer: both layouts, beside it — and an INLINE module is not a file.
    let found = derive(
        "crates/x/src/runtime/mod.rs",
        "mod real;\n#[cfg(test)]\nmod probe_tests;\n#[cfg(test)]\nmod tests {\n}\n",
    );
    assert!(found.contains("crates/x/src/runtime/probe_tests.rs"), "{found:?}");
    assert!(
        found.contains("crates/x/src/runtime/probe_tests/mod.rs"),
        "Rust accepts either layout: {found:?}"
    );
    assert!(
        !found.contains("crates/x/src/runtime/tests.rs"),
        "an inline module declares no file: {found:?}"
    );
    assert!(!found.contains("crates/x/src/runtime/real.rs"), "an UNGATED `mod` ships: {found:?}");

    // An ungated `mod NAME;` beside an inline test module is production, and must not be skipped.
    let found =
        derive("crates/x/src/runtime/mod.rs", "mod probe_tests;\n#[cfg(test)]\nmod inline {}\n");
    assert!(found.is_empty(), "a `mod NAME;` with no `#[cfg(test)]` above it ships: {found:?}");

    // The one-line spelling of a FILE declaration.
    let found = derive("crates/x/src/lib.rs", "#[cfg(test)] mod one_liner;\n");
    assert!(found.contains("crates/x/src/one_liner.rs"), "{found:?}");

    // #2497's row: a `store.rs` declarer's module is `store/tests.rs`, one directory DOWN — and,
    // unlike the copy #2497 patched, NOT also `tests.rs` beside `store.rs`, where rustc never looks.
    let found = derive("crates/x/src/store.rs", "mod real;\n#[cfg(test)]\nmod tests;\n");
    assert!(found.contains("crates/x/src/store/tests.rs"), "{found:?}");
    assert!(
        !found.contains("crates/x/src/tests.rs"),
        "a `foo.rs` declarer never looks beside itself: {found:?}"
    );

    // ...while a `lib.rs`/`main.rs`/`mod.rs` declarer owns its OWN directory: `src/t.rs`, never
    // `src/lib/t.rs`.
    for root in ["lib", "main", "mod"] {
        let found = derive(&format!("crates/x/src/{root}.rs"), "#[cfg(test)]\nmod t;\n");
        assert!(found.contains("crates/x/src/t.rs"), "{root}.rs resolves beside itself: {found:?}");
        assert!(
            !found.contains(&format!("crates/x/src/{root}/t.rs")),
            "{root}.rs must NOT nest: {found:?}"
        );
    }
}

/// Folding and re-inlining are the two ways a gate gets back the text it was written against, and
/// both must round-trip a module file into exactly one owner.
#[test]
fn a_moved_test_module_folds_and_reinlines_into_its_owner() {
    let parent = "pub fn f() {}\n\n#[path = \"f_tests.rs\"]\n#[cfg(test)]\nmod f_tests;\n";
    let child = "use super::*;\n\n#[test]\nfn t() {\n    f();\n}\n";
    let sources = vec![
        ("crates/x/src/f.rs".to_string(), parent.to_string()),
        ("crates/x/src/f_tests.rs".to_string(), child.to_string()),
        ("crates/x/src/g.rs".to_string(), "pub fn g() {}\n".to_string()),
    ];

    let folded = fold_test_module_files(sources);
    let names: Vec<&str> = folded.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        names,
        ["crates/x/src/f.rs", "crates/x/src/g.rs"],
        "the module file is not an entry"
    );
    assert!(folded[0].1.contains("fn t()"), "the owner carries its test module's text");
    assert!(
        folded[0].1.contains("#[cfg(test)]\nmod f_tests {\n"),
        "folded back INLINE at the declaration, marker and all: {}",
        folded[0].1
    );
    assert_eq!(folded[1].1, "pub fn g() {}\n", "a file with no test module is untouched");

    let inline = reinline_test_modules(std::path::Path::new("crates/x/src/f.rs"), parent, |p| {
        (p == std::path::Path::new("crates/x/src/f_tests.rs")).then(|| child.to_string())
    });
    assert!(!inline.contains("#[path"), "the `#[path]` line goes with the move: {inline}");
    assert!(inline.contains("#[cfg(test)]\nmod f_tests {\n    use super::*;"), "{inline}");
    assert!(inline.trim_end().ends_with('}'), "the module is closed: {inline}");
    // A module whose file cannot be read stays a declaration rather than becoming an empty block.
    let unread = reinline_test_modules(std::path::Path::new("crates/x/src/f.rs"), parent, |_| None);
    assert_eq!(unread.trim_end(), parent.trim_end());
}

/// The attribute guard in front of `test_module_decls` is EXACT: every spelling the declaration
/// scan accepts still declares, and a text spelling neither attribute declares nothing. The guard
/// exists because the scan split and trimmed every line of every file in the tree, two to three
/// times per fold, once per test process. The fourth spelling puts other `#`s before the attribute,
/// because the guard hops from `#` to `#` and must not stop at the first one.
#[test]
fn the_attribute_guard_keeps_every_declaration_spelling() {
    use vike_model::libm_walk::cfg_test_module_children;
    let p = std::path::Path::new("crates/x/src/lib.rs");
    for text in [
        "#[cfg(test)]\nmod tests;\n",
        "#[cfg(all(test, feature = \"x\"))]\nmod probe;\n",
        "#[path = \"lib_tests.rs\"]\n#[cfg(test)]\nmod lib_tests;\n",
        "#[derive(Debug)]\nstruct S; // #1\n#![x]\n#[cfg(test)]\nmod after_other_hashes;\n",
        "    #[cfg(test)] mod same_line;\n",
    ] {
        assert!(
            !cfg_test_module_children(p, text).is_empty(),
            "{text:?} must still declare a test module"
        );
    }
    for text in ["mod tests;\n", "#[cfg(feature = \"x\")]\nmod y;\n", "// #[cfg(tset)]\nmod z;\n"] {
        assert!(cfg_test_module_children(p, text).is_empty(), "{text:?} declares no test module");
    }
}
