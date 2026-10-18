//! The proofs that the scanner and the gate's searches can actually fail.

use std::path::Path;

use vike_model::libm_walk::reinline_test_modules;

use super::scanner::{TestItem, call_line, contains_code_in, strip_test_only_cfg, test_item_at};
use super::workspace_root;

/// The no-caller search's ALGORITHM, proved to reject and to accept — a reachability check nobody
/// has watched reject anything is the shape `verify the tool before believing its answer` is for.
///
/// ⚠ **These are three-line synthetic strings and that is a declared limit, not an oversight.**
/// This test passed green through the entire period in which the real check was disabled on
/// `crates/vike-tradehub/src/tradehub_cli.rs` and 822 other files, because the cut that disabled
/// it is triggered by text no synthetic string contains. A kill proof over a fixture proves the
/// algorithm and nothing about the tree —
/// [`the_scanner_reads_past_a_cfg_test_attribute_in_a_real_file`] is the half that proves the
/// tree, and neither substitutes for the other.
#[test]
fn the_no_caller_search_can_actually_fail() {
    let call = "Poller::spawn";
    assert_eq!(
        call_line("fn main() {\n    Poller::spawn(cfg);\n}\n", call),
        Some(2),
        "a real call site must be found, or this gate rubber-stamps every Uncalled row"
    );
    // The three shapes that are NOT calls, each of which appears in the real tree today.
    assert_eq!(call_line("//! [`Poller::spawn`] returns None unless …\n", call), None, "doc link");
    assert_eq!(call_line("pub use settlement::{Poller, Deps};\n", call), None, "re-export");
    assert_eq!(call_line("    pub fn spawn(x: u8) {}\n", call), None, "its own definition");
    // …and the test module is below the cut, which is what makes "nothing but its own tests
    // constructs it" a statement the gate can hold.
    assert_eq!(
        call_line("fn live() {}\n#[cfg(test)]\nmod t {\n  fn x() { Poller::spawn(1); }\n}\n", call),
        None,
        "a call inside the file's own test module is not a production caller"
    );
    // ⚠ THE REGRESSION, in the smallest form it has: a `#[cfg(test)]` on a NON-module item cuts
    // nothing. This is the shape that blinded the gate over 231,897 lines of `src/`.
    assert_eq!(
        call_line("#[cfg(test)]\nuse crate::feeds::CexBars;\nfn m() { Poller::spawn(1); }\n", call),
        Some(3),
        "a `#[cfg(test)] use` is an attribute on an ITEM, not a module boundary — everything below \
         one is still production code"
    );
    // …the same for a `#[cfg(test)] fn`, and for the `all(test, …)` spelling this tree also uses.
    assert_eq!(
        call_line("#[cfg(test)]\nfn helper() {}\nfn m() { Poller::spawn(1); }\n", call),
        Some(3),
        "a `#[cfg(test)] fn` is not a module boundary either"
    );
    assert_eq!(
        call_line(
            "#[cfg(all(test, feature = \"x\"))]\nmod t {\n  fn x() { Poller::spawn(1); }\n}\n",
            call
        ),
        None,
        "`all(test, …)` can only hold in a test build, so the module it guards is test code"
    );
    // …but `any(test, …)` is NOT test-only: a `test-support` build ships that module.
    assert_eq!(
        call_line(
            "#[cfg(any(test, feature = \"test-support\"))]\nmod s {\n  fn x() { Poller::spawn(1); \
             }\n}\n",
            call
        ),
        Some(3),
        "`any(test, feature = …)` also compiles in a feature build, which is shipping code"
    );
    // …and an OUT-OF-LINE test module declaration steps over one line rather than ending the scan.
    assert_eq!(
        call_line("#[cfg(test)]\nmod t;\nfn m() { Poller::spawn(1); }\n", call),
        Some(3),
        "`#[cfg(test)] mod t;` has its body in a sibling FILE — nothing below it is test code"
    );
    // …and the scan RESUMES after an inline test module rather than stopping at it, so a test
    // module placed mid-file no longer hides the rest.
    assert_eq!(
        call_line("#[cfg(test)]\nmod t {\n  fn a() {}\n}\nfn m() { Poller::spawn(1); }\n", call),
        Some(5),
        "production code BELOW a test module is still production code"
    );
}

/// **The A/B proof, over a REAL file.**
///
/// [`the_no_caller_search_can_actually_fail`] exercises the algorithm on three-line synthetic
/// strings, and that is exactly how the hole it was written to close survived it: the scanner's
/// cut is triggered by text that appears 300 lines into a 7,684-line file, and no synthetic string
/// ever contains one. The proof that matters is therefore performed here, against
/// `crates/vike-tradehub/src/tradehub_cli.rs` itself — the daemon's composition root, the file a
/// mount would be wired in, and the one whose `#[cfg(test)] use` blinded the gate.
///
/// Three assertions, in the order they must hold:
///
/// 1. the TRAP SHAPE is really in that file — a test-only `cfg` attribute whose item is not a
///    module, with the file's own test module far below it. If somebody deletes that `use`, this
///    fails loudly rather than passing for a reason that has evaporated;
/// 2. a call planted in PRODUCTION code below the trap is FOUND (it was not, before this change);
/// 3. the same call planted INSIDE the file's own `#[cfg(test)] mod tests` is NOT.
#[test]
fn the_scanner_reads_past_a_cfg_test_attribute_in_a_real_file() {
    let root = workspace_root();
    let rel = "crates/vike-tradehub/src/tradehub_cli.rs";
    let raw = std::fs::read_to_string(root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
        .expect("the daemon's composition root must be readable — it is what this test is about");
    // The file's test module lives in `tradehub_cli/tests.rs` since the code-layout plan's Task 8.
    // Assertion 3 is about the INLINE shape — the one every file below the extraction threshold
    // still has — so the module is put back inline, at its own position, before planting.
    let source =
        reinline_test_modules(Path::new(rel), &raw, |p| std::fs::read_to_string(root.join(p)).ok());
    let lines: Vec<&str> = source.lines().collect();

    // 1. the trap: a test-only cfg attribute guarding something that is NOT a module.
    let trap = (0..lines.len())
        .find(|&i| {
            strip_test_only_cfg(lines[i].trim_start()).is_some()
                && test_item_at(&lines, i).is_none()
        })
        .expect(
            "`crates/vike-tradehub/src/tradehub_cli.rs` no longer carries a `#[cfg(test)]` on a \
             non-module item. That is the shape this test exists to prove the scanner survives — \
             point it at another file that has one rather than deleting the test.",
        );
    let module = (0..lines.len())
        .find_map(|i| match test_item_at(&lines, i) {
            Some(TestItem::Inline { at }) => Some((i, at)),
            _ => None,
        })
        .expect("that file's own inline `#[cfg(test)] mod tests` must still be there");
    let (module_attr, module_at) = module;
    assert!(trap < module_attr, "the trap must sit ABOVE the file's test module to blind anything");
    assert!(
        module_attr - trap > 500,
        "the blind span is down to {} lines, so this file no longer demonstrates the failure — \
         find one that does rather than lowering the number",
        module_attr - trap
    );

    // 2. production code below the trap is SEEN.
    let planted = plant(&lines, module_attr, "    let _ = AutoRedeemPoller::spawn(deps);");
    assert_eq!(
        call_line(&planted, "AutoRedeemPoller::spawn"),
        Some(module_attr + 1),
        "a call planted {} lines below the `#[cfg(test)]` trap was not found. That is the exact \
         state this gate shipped in: `flags.poly_auto_redeem` and `flags.poly_redeem_halt` both \
         claim nothing calls `AutoRedeemPoller::spawn`, and the daemon's whole mount was invisible \
         to the search backing that claim.",
        module_attr - trap
    );

    // 3. …and the file's own test module still is not.
    let in_tests = plant(&lines, module_at + 1, "    let _ = AutoRedeemPoller::spawn(deps);");
    assert_eq!(
        call_line(&in_tests, "AutoRedeemPoller::spawn"),
        None,
        "a call inside the file's own `#[cfg(test)] mod tests` counted as a production caller — \
         the cut moved too far the other way"
    );
}

/// **THE direction's half of the same rule — and it was the LAST site still holding its own.**
///
/// [`every_claimed_consumer_really_reads_it`] used a raw `source.contains(needle)` while the module
/// doc above already claimed [`production_lines`] was *"the one notion of production code every
/// direction now shares"*. It was not, and the gap was the wrong way round: the hole had been
/// closed on the rows that admit NOTHING reads a key, and left open on the rows that CERTIFY one
/// does — which is the stronger claim and the one an operator's `config show` repeats.
///
/// Measured on `config.tradehub_addr`, whose row names a single read in the daemon. With the raw
/// `contains`, BOTH of these left the gate green while the setting became genuinely unread:
/// commenting the read out (the needle still matches, inside a comment) and moving the identical
/// text into that file's own `#[cfg(test)] mod tests`. In each case `vike-cli config show` goes on
/// printing the file as the key's ORIGIN — `Policy::max_total_exposure`'s defect exactly.
///
/// This asserts the property directly rather than by mutation, because the mutation form would
/// have to edit a 7,600-line daemon file: a needle that exists ONLY in a comment, and one that
/// exists ONLY inside a test module, must both read as absent; one in production code must not.
#[test]
fn a_claimed_consumer_inside_a_comment_or_a_test_module_does_not_count() {
    let source = "\
fn live() {
    let a = settings.config.tradehub_addr.as_deref();
}
// settings.config.commented_out.as_deref()
#[cfg(test)]
mod tests {
    fn t() {
        let b = settings.config.only_in_tests.as_deref();
    }
}
";
    assert!(
        contains_code_in(source, "settings.config.tradehub_addr.as_deref()"),
        "a read in production code must count — otherwise THE direction fails every honest row"
    );
    assert!(
        !contains_code_in(source, "settings.config.commented_out.as_deref()"),
        "a needle that exists only inside a COMMENT must not satisfy a Consumer::At row: the \
         setting is unread in every shipped build while the row certifies it consumed"
    );
    assert!(
        !contains_code_in(source, "settings.config.only_in_tests.as_deref()"),
        "a needle that exists only inside `#[cfg(test)] mod tests` must not satisfy a Consumer::At \
         row: the read is compiled out of every shipped build, so the operator configures a key \
         that nothing they can run will read"
    );
}

/// **The [`Reader::Live`] half of the same rule, mutation-proved on the file that carried it.**
///
/// `preferences.sweep_threads` WAS the table's one `Reader::Live` row: it told an operator that
/// `VIKE_SWEEP_THREADS` still worked and that only the file key was inert, and its evidence was a
/// caller of `install_bounded` in `crates/vike-backtest/src/harness/sweep.rs`. Before the two
/// scans were unified, that evidence could have been a call inside that file's OWN test module —
/// which would make the advice `vike-cli config show` prints to an operator true of `cargo test`
/// and of nothing they can run.
///
/// ⚠ **That key is WIRED now (`Consumer::At`), so no row is in the `Live` state and this test no
/// longer has a row to name.** It is KEPT, pointed at the same real file, because what it proves is
/// a property of the SCANNER rather than of that row: a caller inside a `#[cfg(test)] mod tests`
/// must not satisfy [`contains_code_in`]. Every `Uncalled` row becomes `Live` the moment something
/// calls the entry point it names, and on that day this rule is what decides whether the evidence
/// offered is real — so retiring the check with the row would take the gate away exactly when the
/// next row needs it.
///
/// The mutation edits PRODUCTION code, not the harness: every production `install_bounded(`
/// renamed away, one planted back inside `#[cfg(test)] mod tests`. The scanner must then answer
/// FALSE.
#[test]
fn a_reader_live_caller_inside_a_test_module_does_not_count() {
    let root = workspace_root();
    let rel = "crates/vike-backtest/src/harness/sweep.rs";
    let raw = std::fs::read_to_string(root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
        .expect("the production sweep-pool caller this rule was proved on must be readable");
    // Its test module lives in `sweep_tests.rs`; the plant below needs the INLINE shape, so the
    // module is put back inline at its own position.
    let source =
        reinline_test_modules(Path::new(rel), &raw, |p| std::fs::read_to_string(root.join(p)).ok());
    assert!(
        contains_code_in(&source, "install_bounded("),
        "the production `install_bounded` caller is gone from {rel} — that is a real finding, not \
         a test fixture problem"
    );

    let renamed = source.replace("install_bounded(", "install_bounded_RENAMED_BY_THIS_TEST(");
    let lines: Vec<&str> = renamed.lines().collect();
    let module_at = (0..lines.len())
        .find_map(|i| match test_item_at(&lines, i) {
            Some(TestItem::Inline { at }) => Some(at),
            _ => None,
        })
        .expect("that file's own inline `#[cfg(test)] mod tests` must still be there");
    let mutated = plant(&lines, module_at + 1, "    let _ = install_bounded(|| ());");

    assert!(
        !contains_code_in(&mutated, "install_bounded("),
        "a call inside `{rel}`'s own `#[cfg(test)] mod tests` would satisfy a `Reader::Live` \
         claim. That makes `Live` and `Uncalled` stop being opposites: the same caller would \
         count as evidence the variable works while not counting as a caller that would redden \
         the `Uncalled` row opposite it, and `config show` would go on telling an operator to \
         export a variable nothing outside `cargo test` reaches."
    );
}

/// Insert `text` so it occupies 0-indexed line `at` of `lines`, and give back the whole source.
fn plant(lines: &[&str], at: usize, text: &str) -> String {
    let mut out: Vec<&str> = lines.to_vec();
    out.insert(at.min(out.len()), text);
    out.join("\n")
}

/// The env-shaped-needle refusal has to be able to FAIL, or it is decoration on a trap that has
/// already been sprung once.
///
/// ⚠ **The bad spellings are ASSEMBLED, never written out, and that is the test demonstrating its
/// own rule.** A literal read-shaped string in THIS file is the exact thing being refused, and
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs` scans test sources too — so the first version of
/// this test, which spelled the two offenders as plain literals, reddened
/// `every_read_variable_is_declared` from inside a test whose whole subject is not doing that.
/// Measured, twice, which is why the note is here and not merely in `Reader`'s doc.
#[test]
fn the_env_shaped_needle_refusal_can_actually_fail() {
    let call = format!("std::{}::var", "env");
    for bad in [format!("{call}(RECORD_CHAINS_ENV)"), format!("{call}_os(\"SOME_HALT_FLAG\")")] {
        assert!(is_env_shaped(&bad), "{bad:?} must be caught — this shape reddened three gates");
    }
    // …and the signature needles every row carries now must NOT be caught, or the rule would have
    // no legal spelling at all. `fn env_snapshot()` is the interesting one: it CONTAINS "env".
    for good in ["pub fn sweep_threads() -> usize", "fn env_snapshot()"] {
        assert!(!is_env_shaped(good), "{good:?} was rejected — no legal needle would remain");
    }
}

/// `true` when `needle` would read as an environment access to `settings_registry.rs`'s literal
/// sweep. One check covers `var_os` too, since it contains the same prefix.
pub(super) fn is_env_shaped(needle: &str) -> bool {
    needle.contains(&format!("{}::var", "env"))
}
