//! The DISK bound on the log directory, and the prefix rule that makes it safe to have one.
//!
//! `VIKE_LOG_FILE_LEVEL` bounds the WRITE RATE and nothing ever bounded the SIZE, so a long-running
//! daemon's log directory grew without limit — one bulk backfill wrote 341 GB and nearly filled the
//! disk hosting a live trading node. A level an operator must remember to lower is not a bound; a
//! retention that prunes while nobody is watching is.
//!
//! Two properties, and the second is why the first can be a DEFAULT:
//!
//!   1. Retention actually deletes. `tracing-appender` prunes inside `Inner::new`, i.e. at BUILD
//!      time, so this is deterministic and needs no clock and no rotation wait.
//!   2. Pruning is scoped to ONE binary. It matches `filename.starts_with(prefix)`, so a shared
//!      prefix makes one binary delete another's logs — and a prefix that is a PREFIX OF ANOTHER
//!      does it silently. The old default (`"vike"`, left in place by twenty binaries, all sharing
//!      `<project>/settings/state/logs`) starts both `vike-app.<date>` and `vike-tradehub.<date>`,
//!      so arming retention without this change would have let a one-shot backfill delete the GUI's
//!      and the live daemon's logs.

use std::io::Write;
use std::path::Path;

use vike_log::{DEFAULT_MAX_LOG_FILES, LogConfig, effective_file_prefix};

/// A scratch directory that removes itself.
///
/// ⚠ The body used to be hand-rolled — a pid+counter name under the system temp directory, cleared
/// in front and removed on drop — justified by "this crate has no `tempfile` dev-dependency and
/// adds none for a test". That justification expired: `crates/vike-log/Cargo.toml` now carries
/// `tempfile` for `crates/vike-log/tests/unwritable_log_dir.rs`, so the hand-rolled name is a
/// second spelling of something the workspace already has. The `Drop` semantics are unchanged;
/// what improves is that the name is claimed O_EXCL rather than argued to be collision-free.
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
    // tracing-appender prunes by `metadata.created()` and falls back to the filename date ONLY
    // when creation time is unavailable, so planted files need DISTINCT creation timestamps or
    // the prune order is arbitrary. Six files created in one tight loop collided on a loaded box
    // -- the coverage lane's whole-workspace run kept `2020-01-02` and dropped newer files -- so
    // this pause makes the ordering real rather than incidental. It is not a timing guess: any
    // nonzero gap separates the timestamps, and the whole test plants six files.
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
/// ⚠ The planted dates are in 2020, and that is load-bearing rather than arbitrary. The first
/// version of this test planted `2026-08-01..06` and failed in CI with two files where it expected
/// three: the appender names its new file for TODAY, today WAS `2026-08-06`, so it reopened a
/// planted file instead of creating a further one. A fixed past window can never collide with the
/// day the suite runs — and a test whose result depends on the date is a flake waiting for that
/// date to come round.
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

/// `None` is still available and still means "keep everything" — the pre-existing behaviour, so a
/// deployment that archives its own logs is not forced into a retention.
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

/// ⚠ THE ONE THAT JUSTIFIES THE PREFIX CHANGE. Pruning matches `starts_with`, so a binary whose
/// prefix is a PREFIX of another binary's deletes that other binary's files. This is the exact
/// shape of the old default and the reason retention could not simply be switched on.
#[test]
fn a_prefix_of_another_prefix_prunes_the_other_binarys_logs() {
    let s = Scratch::new("collide");
    // What the shared log directory looks like with the OLD default in play.
    for d in ["2026-08-01", "2026-08-02", "2026-08-03"] {
        plant(s.path(), &format!("vike-app.{d}"));
        plant(s.path(), &format!("vike-tradehub.{d}"));
    }
    assert_eq!(s.names().len(), 6);

    // A one-shot tool opening its log under the OLD default prefix.
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

/// The default config arms the bound. A retention nobody switches on is the state this replaces.
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
