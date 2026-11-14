//! Every shipped strategy file has a header that parses, and every folder of the shipped set reads.
//!
//! Reads `strategies/` at RUN time: a tree without it (the public mirror until the shipped set is
//! published there) skips loudly rather than failing to compile.

use std::path::Path;

use vike_strategy::file::StrategyFileRunsOn;
use vike_strategy::file::scan::scan;

#[test]
fn every_shipped_file_has_a_header_that_parses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../strategies");
    if !root.is_dir() {
        eprintln!("SKIPPED: {} is absent from this tree", root.display());
        return;
    }
    let listed = scan(&root, None, None);
    assert!(listed.unreadable.is_empty(), "unreadable folders: {:?}", listed.unreadable);
    assert!(
        listed.files.iter().any(|f| f.path == "other/example.rs"),
        "the scan did not see other/example.rs: it is reading nothing ({:?})",
        listed.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    let broken: Vec<String> = listed
        .files
        .iter()
        .filter_map(|f| f.header.as_ref().err().map(|e| format!("  {}: {e}", f.path)))
        .collect();
    assert!(broken.is_empty(), "shipped files whose header is refused:\n{}", broken.join("\n"));
}

/// The shipped files that run live and state no `why`: a RATCHET (the registry's `UNARGUED_LIVE`,
/// moved here when the registry was deleted). It may shrink — write that file's `why` and drop its
/// path here, shrinking the array's declared length — and never grow: a new file carries its own
/// argument. `trend/momentum.rs` is here because its registry row never carried one.
const UNARGUED_LIVE: [&str; 1] = ["trend/momentum.rs"];

#[test]
fn the_unargued_live_set_does_not_grow() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../strategies");
    if !root.is_dir() {
        eprintln!("SKIPPED: {} is absent from this tree", root.display());
        return;
    }
    let mut found: Vec<String> = scan(&root, None, None)
        .files
        .into_iter()
        .filter(|f| {
            f.header.as_ref().is_ok_and(|h| {
                h.strategy.runs_on == StrategyFileRunsOn::Live && h.strategy.why.is_none()
            })
        })
        .map(|f| f.path)
        .collect();
    found.sort();
    assert_eq!(
        found, UNARGUED_LIVE,
        "the unargued-live set changed. A file may LEAVE it (write its `why`); none may JOIN it"
    );
}
