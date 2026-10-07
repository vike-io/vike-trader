//! The plan, the import, and the one-import slot and its stop probe.

use super::*;

// ---- the plan ---------------------------------------------------------------------------------

#[test]
fn an_absent_dataset_plans_as_absent_and_names_the_servers_directory() {
    let tree = Tree::new();
    let (lane, _) = tree.lane();
    let mut s = spec(None, None, true, false);
    s.dataset = "GBPUSD".to_string();
    let done = done_of(import_archive_verb(&s, Some(&lane), &never));
    assert_eq!(done.plan.dir, DatasetDir::Absent);
    assert!(done.plan.server_dir.ends_with("GBPUSD"), "{}", done.plan.server_dir);
    assert!(done.plan.days.is_empty() && done.outcome.is_none());
}

/// The window, the gaps, the mixed-layout refusal, the header read and the plan-only answer.
#[test]
fn a_dry_run_plans_every_day_with_a_file_and_writes_nothing() {
    let tree = Tree::new();
    // Mon..Fri of one week; Wednesday is missing (a gap); Thursday holds both layouts.
    tree.daily(MON, b"\x05abc");
    tree.daily(MON + DAY, b"\x07abcdef");
    tree.daily(MON + 3 * DAY, b"\x01");
    fs::write(
        tree.dataset()
            .join(format!("{}", (MON + 3 * DAY) / DAY / 100))
            .join(format!("{}.hour", (MON + 3 * DAY) / DAY)),
        b"h",
    )
    .unwrap();
    tree.daily(MON + 4 * DAY, b"X-bad-header");
    fs::write(tree.dataset().join("README.txt"), b"hello").unwrap();
    let (lane, store) = tree.lane();
    store.classes.lock().unwrap().insert(MON + DAY, DayClass::HeldByArchive);

    let done = done_of(import_archive_verb(&spec(None, None, true, false), Some(&lane), &never));
    let plan = &done.plan;
    assert!(done.outcome.is_none(), "a plan-only dry run has no outcome");
    assert_eq!((plan.from_day, plan.to_day), (Some(MON), Some(MON + 4 * DAY)), "resolved");
    assert_eq!(plan.inventory.daily_files, 4);
    assert_eq!((plan.inventory.other_objects, plan.inventory.other_bytes), (1, 5));
    assert!(plan.inventory.other_layout_days.is_empty(), "a mixed day is a refused plan day");
    assert_eq!(plan.gaps, vec![MON + 2 * DAY], "Wednesday has no file");
    let classes: Vec<_> =
        plan.days.iter().map(|d| (d.day, d.class.clone(), d.declared_ticks)).collect();
    assert_eq!(classes[0], (MON, DayClass::Free, Some(5)), "the header was read");
    assert_eq!(
        classes[1],
        (MON + DAY, DayClass::HeldByArchive, None),
        "a held day's file is not opened"
    );
    assert!(matches!(&classes[2].1, DayClass::Refused(r) if r.class == MIXED_LAYOUT));
    assert!(matches!(&classes[3].1, DayClass::Refused(r) if r.class == "BadHeader"));
    assert_eq!(plan.importable_days(), 1);
    assert!(store.imported.lock().unwrap().is_empty(), "a dry run decodes and stores nothing");
}

/// The day cap is checked AGAIN once an omitted bound is filled in from the dataset — the case
/// the client cannot see.
#[test]
fn a_32_day_execute_with_omitted_bounds_is_refused_naming_the_cap() {
    let tree = Tree::new();
    tree.daily(MON, b"\x01");
    tree.daily(MON + 31 * DAY, b"\x01");
    let (lane, store) = tree.lane();
    for (from, to) in [(None, None), (Some(MON), None), (None, Some(MON + 31 * DAY))] {
        let msg = error_of(import_archive_verb(&spec(from, to, false, false), Some(&lane), &never));
        assert!(msg.contains("IMPORT_MAX_DAYS = 31") && msg.contains("32 days"), "{msg}");
    }
    // ...and a verify is held to it too, while a plan-only dry run is not.
    let msg = error_of(import_archive_verb(&spec(None, None, true, true), Some(&lane), &never));
    assert!(msg.contains("IMPORT_MAX_DAYS"), "{msg}");
    assert_eq!(store.sessions.load(Ordering::SeqCst), 0, "refused before the store session");
    let done = done_of(import_archive_verb(&spec(None, None, true, false), Some(&lane), &never));
    assert_eq!(done.plan.days.len(), 2);
    // 31 days is the cap itself, and passes.
    let ok =
        import_archive_verb(&spec(None, Some(MON + 30 * DAY), false, false), Some(&lane), &never);
    assert_eq!(done_of(ok).outcome.unwrap().days.len(), 1);
}

// ---- the import -------------------------------------------------------------------------------

#[test]
fn an_import_decodes_the_free_days_tops_up_the_held_ones_and_echoes_plan_refusals() {
    let tree = Tree::new();
    tree.daily(MON, b"\x05abc");
    tree.daily(MON + DAY, b"\x05abc");
    tree.daily(MON + 2 * DAY, b"X");
    let (lane, store) = tree.lane();
    store.classes.lock().unwrap().insert(MON + DAY, DayClass::HeldByHttp);
    let done = done_of(import_archive_verb(&spec(None, None, false, false), Some(&lane), &never));
    let results: Vec<_> =
        done.outcome.unwrap().days.into_iter().map(|d| (d.day, d.result)).collect();
    assert_eq!(results[0], (MON, DayResult::Imported { ticks: 4, bars: Vec::new() }));
    assert_eq!(results[1], (MON + DAY, DayResult::ToppedUp { bars: Vec::new() }));
    assert!(matches!(&results[2].1, DayResult::Refused(r) if r.class == "BadHeader"));
    let imported = store.imported.lock().unwrap();
    assert_eq!(imported.as_slice(), &[(MON, b"\x05abc".to_vec())], "only the FREE day was read");
}

#[test]
fn a_verify_decodes_only_the_importable_days_and_stores_nothing() {
    let tree = Tree::new();
    tree.daily(MON, b"\x05abc");
    tree.daily(MON + DAY, b"\x05abc");
    let (lane, store) = tree.lane();
    store.classes.lock().unwrap().insert(MON + DAY, DayClass::HeldByArchive);
    let done = done_of(import_archive_verb(&spec(None, None, true, true), Some(&lane), &never));
    let days = done.outcome.expect("a verify has an outcome").days;
    assert_eq!(days, vec![DayOutcome { day: MON, result: DayResult::Verified { ticks: 4 } }]);
    assert!(store.imported.lock().unwrap().is_empty());
}

/// A file that GREW past the format's cap after the walk — appended to in place, so it is still
/// the object the walk vetted — is refused at the read, having buffered at most one byte past
/// the cap. Unix: elsewhere the weaker identity counts a changed length as a different file.
#[cfg(unix)]
#[test]
fn a_file_over_the_formats_cap_at_read_time_is_refused() {
    let tree = Tree::new();
    let path = tree.daily(MON, b"\x05abc");
    let w = walk_of(&tree, &WalkCaps::DEFAULT);
    let file = &w.daily[&MON];
    fs::write(&path, vec![b'\x05'; 65]).unwrap(); // same inode, now one byte over the 64 cap
    let refusal = walk::read_vetted(file, 64).expect_err("over the cap");
    assert_eq!(refusal.class, walk::FILE_TOO_LARGE);
    assert!(walk::read_vetted(file, 65).is_ok(), "at the cap it reads");
}

// ---- the slot and the stop probe --------------------------------------------------------------

/// ONE decoding request at a time: a second import while one runs is REFUSED (not queued), a
/// plan-only dry run is not held to the slot, and the slot frees when the first one answers.
#[test]
fn a_second_concurrent_import_is_refused_and_the_slot_frees_afterwards() {
    let tree = Tree::new();
    tree.daily(MON, b"\x05abc");
    let (lane, store) = tree.lane();
    let lane = Arc::new(lane);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *store.park.lock().unwrap() = Some((started_tx, release_rx));

    let first = {
        let lane = Arc::clone(&lane);
        std::thread::spawn(move || {
            import_archive_verb(&spec(None, None, false, false), Some(&lane), &never)
        })
    };
    started_rx.recv_timeout(WAIT).expect("the first import is running");
    let second = import_archive_verb(&spec(None, None, false, false), Some(&lane), &never);
    assert_eq!(error_of(second), IMPORT_BUSY);
    let verify = import_archive_verb(&spec(None, None, true, true), Some(&lane), &never);
    assert_eq!(error_of(verify), IMPORT_BUSY, "a verify decodes, so it is held to the slot");
    let plan = import_archive_verb(&spec(None, None, true, false), Some(&lane), &never);
    assert!(done_of(plan).outcome.is_none(), "a plan-only dry run is not held to the slot");

    release_tx.send(()).unwrap();
    let first = done_of(first.join().expect("the first import answers"));
    assert_eq!(first.outcome.unwrap().days.len(), 1);
    let again = import_archive_verb(&spec(None, None, false, false), Some(&lane), &never);
    assert!(done_of(again).outcome.is_some(), "the slot freed when the first answered");
}

/// A client that goes away stops the import AT A DAY BOUNDARY: the days before it are done, the
/// days after it are never read.
#[test]
fn a_stop_probe_that_fires_stops_the_import_at_the_next_day() {
    let tree = Tree::new();
    for i in 0..10 {
        tree.daily(MON + i * DAY, b"\x05abc");
    }
    let (lane, store) = tree.lane();
    let asked = AtomicUsize::new(0);
    // The plan asks once per day (10), then the import asks before each day: stop before the
    // fourth decoded day.
    let should_stop = || asked.fetch_add(1, Ordering::SeqCst) >= 10 + 3;
    let msg =
        error_of(import_archive_verb(&spec(None, None, false, false), Some(&lane), &should_stop));
    assert!(msg.contains("stopped between days") && msg.contains("3 days done"), "{msg}");
    let imported: Vec<i64> = store.imported.lock().unwrap().iter().map(|(d, _)| *d).collect();
    assert_eq!(imported, vec![MON, MON + DAY, MON + 2 * DAY], "three days, then the boundary");
}
