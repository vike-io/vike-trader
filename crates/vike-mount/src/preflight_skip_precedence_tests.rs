use super::*;

use crate::preflight::PREFLIGHT_SKIP_ENV as VAR;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// **THE PROPERTY.** Process env `=0` plus file `true` ⇒ the preflight RUNS (the guarded
/// state), because the fold resolves that file `true` to `"0"` before this reader sees it.
#[test]
fn the_preflight_is_not_skipped_when_the_environment_says_zero() {
    assert!(!preflight_skip_requested(&map(&[(VAR, "0")]), &map(&[(VAR, "0")])));
}

/// ⚠ **The regression, as the map the old `or_insert` fold produced**: env refusing, an
/// un-resolved store line arming. This function cannot tell where its second map came from, so
/// the caller may not hand it one the environment has not been applied over.
#[test]
fn a_second_source_that_was_not_resolved_would_skip_the_whole_preflight() {
    assert!(
        preflight_skip_requested(&map(&[(VAR, "0")]), &map(&[(VAR, "1")])),
        "documented, not endorsed — see `preflight_skip_requested`'s residual"
    );
}

/// The file alone skips; an exported `=1` with no file skips.
#[test]
fn either_source_alone_still_skips() {
    assert!(preflight_skip_requested(&HashMap::new(), &map(&[(VAR, "1")])));
    assert!(preflight_skip_requested(&map(&[(VAR, "1")]), &HashMap::new()));
}

/// Absent on both sides is the ordinary shape and must not skip — `false` is the guarded state.
#[test]
fn absent_on_both_sides_runs_the_preflight() {
    assert!(!preflight_skip_requested(&HashMap::new(), &HashMap::new()));
}

/// Exact-`"1"` on both sides, the folded map included.
#[test]
fn a_truthy_spelling_skips_nothing() {
    assert!(!preflight_skip_requested(&map(&[(VAR, "true")]), &map(&[(VAR, "yes")])));
}
