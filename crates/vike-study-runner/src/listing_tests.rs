use super::*;

/// Write `text` to `path`, creating its parent directories: how these tests lay a study tree on disk.
fn write_file(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

// The RUN half's tests moved with the scan rules to `crates/vike-runs/src/tests/scan.rs`; what is
// left here is the STUDY half, which this module still owns.

/// Both tiers, one listing, each row saying which tier it came from — the tiers exist because
/// a compiled study needs a checkout and an interpreted one does not, and that is exactly the
/// difference a user needs to see in the list.
#[test]
fn both_study_tiers_are_listed_with_the_tier_each_was_found_in() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        &root.join("rhai").join("vol-clustering").join("vol-clustering.rhai"),
        "fn go() {}\n",
    );
    write_file(&root.join("rust").join("cohort-decay").join("cohort-decay.rs"), "fn main() {}\n");

    let listing = list_studies(root);

    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
    assert_eq!(
        listing.studies.iter().map(|s| (s.name.as_str(), s.tier)).collect::<Vec<_>>(),
        vec![("vol-clustering", StudyTier::Rhai), ("cohort-decay", StudyTier::Rust)],
        "the rhai tier first, each tier in name order"
    );
    assert_eq!(listing.studies[0].dir, root.join("rhai").join("vol-clustering"));
}

/// Listing is LISTING. A study whose script could not compile still appears, because nothing
/// here compiles or runs anything — a Study execution contract does not exist yet, and a
/// listing that quietly depended on one would hide every study the day it did.
#[test]
fn a_study_that_could_never_compile_still_lists_because_nothing_here_runs_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(&root.join("rhai").join("broken").join("broken.rhai"), "fn on_bar( {\n");

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 1);
    assert_eq!(listing.studies[0].name, "broken");
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
}

/// A folder with no script at all is still a study folder. The entry-file rule belongs to
/// whatever eventually RUNS a study; asserting one here would be inventing that contract, and
/// the folder would be reported as broken for failing a rule nobody has written.
#[test]
fn a_study_folder_holding_no_script_is_listed_rather_than_called_broken() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("rhai").join("just-started")).unwrap();

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 1);
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
}

/// One name in BOTH tiers lists TWICE, each with its tier. Strategies share one namespace
/// because something resolves a strategy BY NAME; nothing resolves a study by name yet, so a
/// duplicate rule here would be a namespace invented ahead of the thing that needs it.
#[test]
fn a_name_used_in_both_tiers_lists_twice_because_nothing_resolves_a_study_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(&root.join("rhai").join("decay").join("decay.rhai"), "fn go() {}\n");
    write_file(&root.join("rust").join("decay").join("decay.rs"), "fn main() {}\n");

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 2, "both rows survive");
    assert_eq!(listing.studies[0].tier, StudyTier::Rhai);
    assert_eq!(listing.studies[1].tier, StudyTier::Rust);
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
}

/// The same two rulings as the runs root, over the studies tree: absent is silent, and an
/// absent TIER is absent too — a user who writes only Rhai studies has no `rust/` directory.
#[test]
fn an_absent_studies_root_or_tier_lists_nothing_and_says_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("user_data").join("research").join("studies");
    assert!(!root.exists(), "precondition");

    let listing = list_studies(&root);
    assert!(listing.studies.is_empty());
    assert!(listing.diagnostics.is_empty(), "absence is not a failure");

    write_file(&root.join("rhai").join("only-rhai").join("only-rhai.rhai"), "fn go() {}\n");
    let listing = list_studies(&root);
    assert_eq!(listing.studies.len(), 1);
    assert!(listing.diagnostics.is_empty(), "a missing rust/ tier is not a failure");
}

/// …and the other half: a TIER that exists and cannot be read is a named row, because a
/// permissions bug there hides every study in it while looking like a user who writes only
/// Rhai.
#[test]
fn a_study_tier_that_exists_and_cannot_be_read_is_its_own_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(&root.join("rhai").join("ok").join("ok.rhai"), "fn go() {}\n");
    write_file(&root.join("rust"), "a file where the rust tier belongs");

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 1, "the readable tier still lists");
    match &listing.diagnostics[..] {
        [StudyDiagnostic::RootUnreadable { root: reported, .. }] => {
            assert_eq!(reported, &root.join("rust"));
        }
        other => panic!("expected one RootUnreadable, got {other:?}"),
    }
}

/// The first-time mistake, mirrored from the strategy loader because the layout is mirrored: a
/// script dropped straight into a tier root. Reported with the folder it belongs in — never
/// silently ignored, which is what makes "I dropped a file in and nothing happened" answerable.
#[test]
fn a_study_script_loose_in_a_tier_root_is_reported_with_where_it_belongs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(&root.join("rhai").join("mystudy.rhai"), "fn go() {}\n");
    // Anything else loose in the tier is NOT a stray study: a README is not a study that went
    // missing, and reporting it would be noise that trains a user to ignore the list.
    write_file(&root.join("rhai").join("README.md"), "# my studies\n");

    let listing = list_studies(root);

    assert!(listing.studies.is_empty());
    match &listing.diagnostics[..] {
        [StudyDiagnostic::StrayStudy { path }] => {
            assert_eq!(path, &root.join("rhai").join("mystudy.rhai"));
        }
        other => panic!("expected one StrayStudy, got {other:?}"),
    }
    let msg = listing.diagnostics[0].to_string();
    assert!(msg.contains("mystudy"), "the message must name the fix as a path: {msg}");
    assert_eq!(listing.diagnostics[0].severity(), DiagnosticSeverity::Error);
}
