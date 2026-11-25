use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

/// A private directory under the system temp dir, unique per process and per call (this crate
/// takes no `tempfile`), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("vike-shipped-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }

    /// The reference folder beside the shipped tree, as a project lays them out.
    fn reference(&self) -> PathBuf {
        self.0.join("vendor/vike-strategies-seeded")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn put(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

fn read(root: &Path, relative: &str) -> String {
    std::fs::read_to_string(root.join(relative)).expect("read")
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

/// A shipped tree of two files in two folders, a licence beside one, and the deploy's marker.
fn shipped(root: &Path) -> PathBuf {
    let s = root.join("vendor/vike-strategies");
    put(&s, "trend/momentum.rs", "// momentum\n");
    put(&s, "other/example.rs", "// example\n");
    put(&s, "other/NOTICE", "licence text\n");
    put(&s, ".release-asset", "abc v0.1.99\n");
    s
}

/// The tree [`shipped`] lays down, as the PREVIOUS release shipped it: every file one version older.
fn previous_release(d: &Scratch) {
    let r = d.reference();
    put(&r, "trend/momentum.rs", "// momentum, the old release\n");
    put(&r, "other/example.rs", "// example, the old release\n");
    put(&r, "other/NOTICE", "licence text, the old release\n");
}

#[test]
fn no_shipped_tree_is_nothing_to_seed() {
    let d = Scratch::new();
    let got = seed_shipped_strategies(
        &d.0.join("absent"),
        &d.reference(),
        &d.0.join("strategies"),
        false,
    );
    assert_eq!(got.expect("no error"), None);
    assert!(!d.0.join("strategies").exists(), "nothing is created when nothing is shipped");
    assert!(!d.reference().exists(), "no reference is laid down when nothing is shipped");
}

/// Row 1, a missing file: copied; the reference then holds what was shipped.
#[test]
fn absent_files_are_copied_and_the_marker_is_not() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    let report = report.expect("a tree");
    assert_eq!(report.copied, paths(&["other/NOTICE", "other/example.rs", "trend/momentum.rs"]));
    assert!(report.kept.is_empty() && report.replaced.is_empty());
    assert!(report.new_version_beside.is_empty());
    assert_eq!(read(&mine, "trend/momentum.rs"), "// momentum\n");
    assert!(!mine.join(".release-asset").exists(), "a dot-name is never copied");
    assert_eq!(read(&d.reference(), "trend/momentum.rs"), "// momentum\n", "the reference moved");
    assert!(!d.reference().join(".release-asset").exists());
}

/// Row 2, a file byte-identical to the previous shipped version (the operator never edited it):
/// replaced with the new one, a licence file like any other.
#[test]
fn a_file_never_edited_takes_the_new_shipped_version() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    previous_release(&d);
    put(&mine, "trend/momentum.rs", "// momentum, the old release\n");
    put(&mine, "other/NOTICE", "licence text, the old release\n");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    let report = report.expect("a tree");
    assert_eq!(report.replaced, paths(&["other/NOTICE", "trend/momentum.rs"]));
    assert_eq!(report.copied, paths(&["other/example.rs"]));
    assert!(report.new_version_beside.is_empty() && report.kept.is_empty(), "{report:?}");
    assert_eq!(read(&mine, "trend/momentum.rs"), "// momentum\n");
    assert_eq!(read(&mine, "other/NOTICE"), "licence text\n");
    assert!(!mine.join("trend/momentum.rs.new").exists(), "a replace writes no .new");
    assert_eq!(read(&d.reference(), "other/NOTICE"), "licence text\n", "the reference moved");

    // The next seed of the same release finds every file as shipped.
    let again = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    assert_eq!(again.expect("a tree").kept.len(), 3);
}

/// Row 3, a file that differs from the previous shipped version (the operator edited it): left as
/// it is, the new version written beside it as `<name>.new`; a later seed of the same release has
/// nothing new to say about it.
#[test]
fn an_edited_file_is_kept_and_the_new_version_goes_beside_it() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    previous_release(&d);
    put(&mine, "trend/momentum.rs", "// my edit\n");
    put(&mine, "other/NOTICE", "licence text, edited\n");
    put(&mine, "other/example.rs", "// example\n");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    let report = report.expect("a tree");
    assert_eq!(report.new_version_beside, paths(&["other/NOTICE", "trend/momentum.rs"]));
    assert_eq!(report.kept, paths(&["other/example.rs"]), "already the new version");
    assert!(report.copied.is_empty() && report.replaced.is_empty(), "{report:?}");
    assert_eq!(read(&mine, "trend/momentum.rs"), "// my edit\n", "the edit survives a seed");
    assert_eq!(read(&mine, "trend/momentum.rs.new"), "// momentum\n");
    assert_eq!(read(&mine, "other/NOTICE"), "licence text, edited\n");
    assert_eq!(read(&mine, "other/NOTICE.new"), "licence text\n");
    assert_eq!(read(&d.reference(), "trend/momentum.rs"), "// momentum\n", "the reference moved");

    // The same release again: the edit is kept, no new version came, nothing is written.
    std::fs::remove_file(mine.join("trend/momentum.rs.new")).expect("the operator read it");
    let again = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    let again = again.expect("a tree");
    assert_eq!(again.kept.len(), 3, "{again:?}");
    assert!(!mine.join("trend/momentum.rs.new").exists(), "no new version, no .new");
}

/// Row 4, a file the operator added that was never shipped: not read, not touched, nothing beside it.
#[test]
fn a_file_the_operator_added_is_never_touched() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    previous_release(&d);
    put(&mine, "trend/mine.rs", "// written by the operator\n");
    put(&mine, "notes.txt", "my notes\n");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    let report = report.expect("a tree");
    assert_eq!(report.copied.len(), 3, "{report:?}");
    assert_eq!(read(&mine, "trend/mine.rs"), "// written by the operator\n");
    assert_eq!(read(&mine, "notes.txt"), "my notes\n");
    assert!(!mine.join("trend/mine.rs.new").exists() && !mine.join("notes.txt.new").exists());
    assert!(
        !d.reference().join("trend/mine.rs").exists(),
        "the reference holds shipped files only"
    );
}

/// No reference on this box for a file that differs from the shipped one: never overwritten blind,
/// so the new version goes beside it; a file already equal to the shipped one stays quiet.
#[test]
fn with_no_reference_a_differing_file_is_kept_and_the_new_version_goes_beside_it() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    put(&mine, "trend/momentum.rs", "// momentum, some older release\n");
    put(&mine, "other/example.rs", "// example\n");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    let report = report.expect("a tree");
    assert_eq!(report.new_version_beside, paths(&["trend/momentum.rs"]));
    assert_eq!(report.kept, paths(&["other/example.rs"]));
    assert_eq!(report.copied, paths(&["other/NOTICE"]));
    assert_eq!(read(&mine, "trend/momentum.rs"), "// momentum, some older release\n");
    assert_eq!(read(&mine, "trend/momentum.rs.new"), "// momentum\n");
    assert!(!mine.join("other/example.rs.new").exists());

    // A reference twin that is not a plain file is no reference either.
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    std::fs::create_dir_all(d.reference().join("trend/momentum.rs"))
        .expect("a folder in its place");
    put(&mine, "trend/momentum.rs", "// momentum, some older release\n");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, true).expect("seed");
    assert_eq!(report.expect("a tree").new_version_beside, paths(&["trend/momentum.rs"]));
}

/// A `.new` file is never loaded as a strategy: the folder scan does not list it and a profile
/// cannot name it.
#[test]
fn a_new_version_beside_is_not_a_strategy_file() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    put(&mine, "trend/momentum.rs", "// my edit\n");
    seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    assert!(mine.join("trend/momentum.rs.new").is_file());
    let listed: Vec<String> =
        crate::file::scan::scan(&mine, None, None).files.into_iter().map(|f| f.path).collect();
    assert_eq!(listed, vec!["other/example.rs".to_string(), "trend/momentum.rs".to_string()]);
    let named = crate::file::scan::resolve(&mine, "trend/momentum.rs.new");
    assert!(named.is_err(), "{named:?}");
}

#[test]
fn a_dry_run_reports_the_same_and_writes_nothing() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    previous_release(&d);
    put(&mine, "trend/momentum.rs", "// my edit\n");
    put(&mine, "other/NOTICE", "licence text, the old release\n");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, true).expect("seed");
    let report = report.expect("a tree");
    assert_eq!(report.copied, paths(&["other/example.rs"]));
    assert_eq!(report.replaced, paths(&["other/NOTICE"]));
    assert_eq!(report.new_version_beside, paths(&["trend/momentum.rs"]));
    assert!(!mine.join("other/example.rs").exists(), "a dry run copied a file");
    assert_eq!(read(&mine, "other/NOTICE"), "licence text, the old release\n");
    assert!(!mine.join("trend/momentum.rs.new").exists(), "a dry run wrote a .new");
    assert_eq!(read(&d.reference(), "trend/momentum.rs"), "// momentum, the old release\n");
}

#[test]
fn the_edited_mark_is_a_shipped_twin_with_other_bytes() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    put(&mine, "trend/momentum.rs", "// my edit\n");
    put(&mine, "mine/own.rs", "// written by the user\n");
    assert!(differs_from_shipped(&s, &mine, Path::new("trend/momentum.rs")));
    assert!(!differs_from_shipped(&s, &mine, Path::new("other/example.rs")), "as shipped");
    assert!(!differs_from_shipped(&s, &mine, Path::new("mine/own.rs")), "no twin, no mark");
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_shipped_tree_is_not_followed() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    put(&d.0, "outside/secret", "not a strategy\n");
    std::os::unix::fs::symlink(d.0.join("outside/secret"), s.join("other/link.rs")).expect("link");
    std::os::unix::fs::symlink(d.0.join("outside"), s.join("linked_dir")).expect("link");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    assert_eq!(report.expect("a tree").copied.len(), 3);
    assert!(!mine.join("other/link.rs").exists() && !mine.join("linked_dir").exists());
}

/// A link in the strategies folder where a shipped file goes is never written through, even when
/// its target holds the previous shipped bytes.
#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_strategies_folder_is_kept() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    previous_release(&d);
    put(&d.0, "outside/momentum.rs", "// momentum, the old release\n");
    std::fs::create_dir_all(mine.join("trend")).expect("mkdir");
    std::os::unix::fs::symlink(d.0.join("outside/momentum.rs"), mine.join("trend/momentum.rs"))
        .expect("link");
    let report = seed_shipped_strategies(&s, &d.reference(), &mine, false).expect("seed");
    assert_eq!(report.expect("a tree").kept, paths(&["trend/momentum.rs"]));
    assert_eq!(read(&d.0, "outside/momentum.rs"), "// momentum, the old release\n");
    assert!(mine.join("trend/momentum.rs").is_symlink(), "the link itself stays");
}
