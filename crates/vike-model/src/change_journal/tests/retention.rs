use super::*;

/// Records land in the file for their OWN month, so a backdated record does not pollute the
/// current one and the retention bound stays a calendar bound.
#[test]
fn each_record_lands_in_its_own_month() {
    let r = root();
    let j = journal(r.path());
    let jan = 1_767_225_600_000; // 2026-01-01T00:00:00Z
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1");
    assert_eq!(j.append(jan, &c).unwrap().file_name().unwrap(), "changes-2026-01.jsonl");
    assert_eq!(j.append(T, &c).unwrap().file_name().unwrap(), "changes-2026-08.jsonl");
    assert_eq!(month_file_name(-1), "changes-1969-12.jsonl", "pre-1970 floors toward -inf");
}

/// Retention: the newest `max_files` months survive, oldest first. Names sort in calendar
/// order, so this needs no clock and no `stat`.
#[test]
fn prune_keeps_the_newest_months_and_removes_the_oldest() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    for (y, m) in [(2024, 11), (2024, 12), (2025, 1), (2025, 2), (2026, 8)] {
        std::fs::write(r.path().join(format!("changes-{y:04}-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    let pruned = j.prune(Some(2));
    assert_eq!((pruned.found, pruned.removed, pruned.failed), (5, 3, 0), "{pruned:?}");
    assert!(!r.path().join("changes-2024-11.jsonl").exists());
    assert!(!r.path().join("changes-2025-01.jsonl").exists());
    assert!(r.path().join("changes-2025-02.jsonl").exists(), "second-newest kept");
    assert!(r.path().join("changes-2026-08.jsonl").exists(), "newest kept");
}

/// Under the limit the prune removes NOTHING — the anti-vacuity twin. A prune that deleted on
/// every call would take out the month currently being written.
#[test]
fn prune_below_the_limit_removes_nothing() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    for m in 1..=3 {
        std::fs::write(r.path().join(format!("changes-2026-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    assert_eq!(j.prune(Some(DEFAULT_MAX_CHANGE_FILES)), Pruned { found: 3, removed: 0, failed: 0 });
    assert!(r.path().join("changes-2026-01.jsonl").exists());
}

/// `None` is retention OFF, asserted against a population that WOULD be pruned under the
/// default — so this cannot pass by a limit nobody reached.
#[test]
fn prune_with_no_limit_keeps_everything() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    let n = DEFAULT_MAX_CHANGE_FILES + 4;
    for i in 0..n {
        let (y, m) = (2000 + i / 12, i % 12 + 1);
        std::fs::write(r.path().join(format!("changes-{y:04}-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    assert_eq!(j.prune(None), Pruned::default(), "no limit means no work at all");
    assert_eq!(std::fs::read_dir(r.path()).unwrap().count(), n);
    assert_eq!(j.prune(Some(DEFAULT_MAX_CHANGE_FILES)).removed, 4, "…and the default bites");
}

/// An absent directory is the ordinary state of a project that has changed nothing — a startup
/// must not fail over housekeeping.
#[test]
fn prune_of_an_absent_directory_is_silent() {
    let r = root();
    let j = ChangeJournal::new(r.path().join("never"), Proc::new("t", 1, "0"));
    assert_eq!(j.prune(Some(1)), Pruned::default());
}

/// ⚠ The prune DELETES what [`is_month_file_name`] accepts, so anything it is unsure about must
/// fall outside — and must not be counted either, or the bound bites early on files it will
/// never remove.
#[test]
fn prune_leaves_anything_that_is_not_a_monthly_file_alone() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    let strangers = [
        "changes-old.jsonl",
        "changes-2026-08.jsonl.bak",
        "changes-2026-8.jsonl",
        "changes-20260-8.jsonl",
        "export.csv",
        "README",
        // ⚠ The append lock's own sentinel. Deleting the file every writer coordinates on
        // would be housekeeping breaking the serialisation — see [`CHANGES_LOCK_FILE`].
        CHANGES_LOCK_FILE,
    ];
    for name in strangers {
        std::fs::write(r.path().join(name), b"not mine\n").unwrap();
    }
    for m in 1..=4 {
        std::fs::write(r.path().join(format!("changes-2026-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    let pruned = j.prune(Some(1));
    assert_eq!(pruned.found, 4, "only real monthly files are counted: {pruned:?}");
    assert_eq!(pruned.removed, 3);
    for name in strangers {
        assert!(r.path().join(name).exists(), "{name} must survive housekeeping");
    }
}

/// The file-name matcher, at the boundaries the prune turns on.
#[test]
fn the_month_file_matcher_is_strict() {
    assert!(is_month_file_name("changes-2026-08.jsonl"));
    assert!(is_month_file_name("changes-0001-01.jsonl"));
    assert!(!is_month_file_name("changes-2026-8.jsonl"), "the month must be two digits");
    assert!(!is_month_file_name("changes-2026_08.jsonl"), "the separator is a hyphen");
    assert!(!is_month_file_name("changes-2026-08.jsonl.bak"));
    assert!(!is_month_file_name("changes-2026-08.json"));
    assert!(!is_month_file_name("2026-08.jsonl"));
    assert!(!is_month_file_name("changes-.jsonl"));
    // Every name this module MINTS must be accepted — the two halves cannot drift apart.
    for ts in [-1i64, 0, T, 4_102_444_800_000] {
        assert!(is_month_file_name(&month_file_name(ts)), "{ts}");
    }
}
