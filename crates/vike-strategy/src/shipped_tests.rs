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

#[test]
fn no_shipped_tree_is_nothing_to_seed() {
    let d = Scratch::new();
    let got = seed_shipped_strategies(&d.0.join("absent"), &d.0.join("strategies"), false);
    assert_eq!(got.expect("no error"), None);
    assert!(!d.0.join("strategies").exists(), "nothing is created when nothing is shipped");
}

#[test]
fn absent_files_are_copied_and_the_marker_is_not() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    let report = seed_shipped_strategies(&s, &mine, false).expect("seed").expect("a tree");
    assert_eq!(report.copied, paths(&["other/NOTICE", "other/example.rs", "trend/momentum.rs"]));
    assert!(report.kept.is_empty());
    assert_eq!(read(&mine, "trend/momentum.rs"), "// momentum\n");
    assert!(!mine.join(".release-asset").exists(), "a dot-name is never copied");
}

#[test]
fn an_existing_file_edited_or_not_is_never_touched() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    put(&mine, "trend/momentum.rs", "// my edit\n");
    put(&mine, "other/example.rs", "// example\n");
    let report = seed_shipped_strategies(&s, &mine, false).expect("seed").expect("a tree");
    assert_eq!(report.copied, paths(&["other/NOTICE"]));
    assert_eq!(report.kept, paths(&["other/example.rs", "trend/momentum.rs"]));
    assert_eq!(read(&mine, "trend/momentum.rs"), "// my edit\n", "the edit survives a seed");

    // A second run copies nothing: every file is the user's now.
    let again = seed_shipped_strategies(&s, &mine, false).expect("seed").expect("a tree");
    assert!(again.copied.is_empty(), "{again:?}");
    assert_eq!(again.kept.len(), 3);
}

#[test]
fn a_dry_run_reports_the_same_and_writes_nothing() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    put(&mine, "trend/momentum.rs", "// my edit\n");
    let report = seed_shipped_strategies(&s, &mine, true).expect("seed").expect("a tree");
    assert_eq!(report.copied, paths(&["other/NOTICE", "other/example.rs"]));
    assert_eq!(report.kept, paths(&["trend/momentum.rs"]));
    assert!(!mine.join("other").exists(), "a dry run created a folder");
}

#[test]
fn the_edited_mark_is_a_shipped_twin_with_other_bytes() {
    let d = Scratch::new();
    let (s, mine) = (shipped(&d.0), d.0.join("strategies"));
    seed_shipped_strategies(&s, &mine, false).expect("seed");
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
    let report = seed_shipped_strategies(&s, &mine, false).expect("seed").expect("a tree");
    assert_eq!(report.copied.len(), 3, "{report:?}");
    assert!(!mine.join("other/link.rs").exists() && !mine.join("linked_dir").exists());
}
