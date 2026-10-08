//! The DISK bound on the log directory (`vike_log::DEFAULT_MAX_LOG_FILES`), and the prefix rule
//! that makes it safe to have one. Two properties, and the second is why the first can be a
//! DEFAULT:
//!
//!   1. Retention actually deletes. `tracing-appender` prunes inside `Inner::new`, i.e. at BUILD
//!      time, so this is deterministic and needs no clock and no rotation wait.
//!   2. Pruning is scoped to ONE binary. It matches `filename.starts_with(prefix)`, so a shared
//!      prefix makes one binary delete another's logs — and a prefix that is a PREFIX OF ANOTHER
//!      (`vike` vs `vike-tradehub.<date>`) does it silently.

use std::io::Write;
use std::path::Path;

use vike_log::{DEFAULT_MAX_LOG_FILES, LogConfig, effective_file_prefix};

/// A scratch directory that removes itself (a `tempfile::TempDir`, named O_EXCL).
struct Scratch(tempfile::TempDir);

impl Scratch {
    fn new(tag: &str) -> Self {
        Self(
            tempfile::Builder::new()
                .prefix(&format!("vike-log-retention-{tag}-"))
                .tempdir()
                .expect("scratch dir"),
        )
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.path())
            .expect("read scratch")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

fn plant(dir: &Path, name: &str) {
    let mut f = std::fs::File::create(dir.join(name)).expect("plant log file");
    writeln!(f, "{{\"old\":true}}").expect("write planted file");
    // tracing-appender prunes by `metadata.created()` (the filename date only when that is
    // unavailable), so planted files need DISTINCT creation timestamps or the prune order is
    // arbitrary; files created in one tight loop collided on a loaded box. Not a timing guess:
    // any nonzero gap separates the timestamps.
    std::thread::sleep(std::time::Duration::from_millis(10));
}

/// Build the appender the way `init` does, so the test exercises the real construction path rather
/// than a hand-rolled imitation of it.
fn build(
    dir: &Path,
    prefix: &str,
    keep: Option<usize>,
) -> tracing_appender::rolling::RollingFileAppender {
    let mut b = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(prefix);
    if let Some(k) = keep {
        b = b.max_log_files(k);
    }
    b.build(dir).expect("appender builds in a writable scratch dir")
}

/// The bound BITES: with six files already present and a retention of three, building the appender
/// leaves three — the two newest survivors plus the one it is about to write.
///
/// ⚠ The planted dates are in 2020 on purpose: the appender names its new file for TODAY, so a
/// planted file dated today would be reopened instead of a further one created, and the count
/// would be off by one. A fixed past window can never collide with the day the suite runs.
#[test]
fn retention_prunes_old_files_on_build() {
    let s = Scratch::new("prunes");
    for d in ["2020-01-01", "2020-01-02", "2020-01-03", "2020-01-04", "2020-01-05", "2020-01-06"] {
        plant(s.path(), &format!("app.{d}"));
    }
    assert_eq!(s.names().len(), 6, "planted files: {:?}", s.names());

    let appender = build(s.path(), "app", Some(3));
    drop(appender);

    let after = s.names();
    assert_eq!(
        after.len(),
        3,
        "a retention of 3 must leave 3 files (2 survivors + the newly opened one); got {after:?}"
    );
    assert!(
        !after.iter().any(|n| n.ends_with("2020-01-01") || n.ends_with("2020-01-02")),
        "the OLDEST files must be the ones pruned; got {after:?}"
    );
}

/// `None` means "keep everything", so a deployment that archives its own logs is not forced into
/// a retention.
#[test]
fn no_retention_keeps_every_file() {
    let s = Scratch::new("keeps");
    for d in ["2020-01-01", "2020-01-02", "2020-01-03", "2020-01-04", "2020-01-05", "2020-01-06"] {
        plant(s.path(), &format!("app.{d}"));
    }
    let appender = build(s.path(), "app", None);
    drop(appender);
    assert!(s.names().len() >= 6, "with no retention nothing may be deleted; got {:?}", s.names());
}

/// ⚠ THE ONE THAT JUSTIFIES THE PREFIX RULE. Pruning matches `starts_with`, so a binary whose
/// prefix is a PREFIX of another binary's deletes that other binary's files.
#[test]
fn a_prefix_of_another_prefix_prunes_the_other_binarys_logs() {
    let s = Scratch::new("collide");
    // Two binaries' files in one shared log directory.
    for d in ["2026-08-01", "2026-08-02", "2026-08-03"] {
        plant(s.path(), &format!("vike-app.{d}"));
        plant(s.path(), &format!("vike-tradehub.{d}"));
    }
    assert_eq!(s.names().len(), 6);

    // A one-shot tool opening its log under the shared literal prefix `vike`.
    let appender = build(s.path(), "vike", Some(3));
    drop(appender);

    let after = s.names();
    let foreign = after
        .iter()
        .filter(|n| n.starts_with("vike-app.") || n.starts_with("vike-tradehub."))
        .count();
    assert!(
        foreign < 6,
        "this test documents the HAZARD: a `vike` prefix must be seen to prune `vike-app.*` and \
         `vike-tradehub.*`. If nothing was deleted, `tracing-appender`'s matching rule changed and \
         `effective_file_prefix`'s justification needs re-reading, not deleting; got {after:?}"
    );
}

/// ...and the fix: an unset prefix resolves to THIS executable's name, so two binaries sharing a
/// log directory cannot share a prefix by default.
#[test]
fn the_default_prefix_is_the_executable_name() {
    assert_eq!(effective_file_prefix("", Some("vike-recorder")), "vike-recorder");
    assert_eq!(effective_file_prefix("   ", Some("backtest")), "backtest");
    // An explicit prefix always wins — the binaries that already set one are unaffected.
    assert_eq!(effective_file_prefix("vike-app", Some("vike-app.exe")), "vike-app");
    // Last resort when the executable cannot be read at all.
    assert_eq!(effective_file_prefix("", None), "vike");
    assert_eq!(effective_file_prefix("", Some("  ")), "vike");
}

/// The default config arms the bound and leaves the prefix to the executable.
#[test]
fn retention_is_on_by_default_and_the_prefix_is_unset() {
    let cfg = LogConfig::default();
    assert_eq!(
        cfg.file_max_files,
        Some(DEFAULT_MAX_LOG_FILES),
        "the default must ARM the disk bound; an opt-in retention is the failure being fixed"
    );
    assert!(
        cfg.file_prefix.is_empty(),
        "the default prefix must be empty (derive from the executable). A shared literal default \
         plus a retention default is how one binary deletes another's logs"
    );
}
