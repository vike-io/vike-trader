use super::*;

fn warning() -> SplitWarning {
    SplitWarning {
        venue: "dukascopy".into(),
        symbol: "NVDA.CFD".into(),
        date: "2026-11-20".into(),
        new_shares: 10,
        old_shares: 1,
        price_before: 1400.0,
        price_after: 140.0,
    }
}

/// `view.poll` at `now` with this test's `egui::Context`.
fn poll(
    view: &mut DataManagerSplitWarningsView,
    address: &str,
    now: Instant,
    again: bool,
    fetch: Option<impl FnOnce() -> SplitWarningsAnswer + Send + 'static>,
) -> bool {
    view.poll(address, now, again, fetch, &egui::Context::default())
}

/// Poll until no read is in flight (each answer lands from a thread of its own).
fn settle(view: &mut DataManagerSplitWarningsView, address: &str) {
    let none: Option<fn() -> SplitWarningsAnswer> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while poll(view, address, Instant::now(), false, none) {
        assert!(Instant::now() < deadline, "the read never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **The desktop lists the datahub's warning and drops it once a split row covers it** (the datahub then answers
/// none); a read that fails keeps the list, and the list is not read again before [`DataManagerSplitWarningsView::REREAD`].
#[test]
fn the_view_lists_a_warning_and_drops_it_after_a_split_row_appears() {
    let mut view = DataManagerSplitWarningsView::default();
    let start = Instant::now();
    assert!(
        poll(&mut view, "dh", start, false, Some(|| Ok(Some(vec![warning()])))),
        "asked at once"
    );
    settle(&mut view, "dh");
    assert_eq!(view.open(), [warning()]);
    assert!(!poll(&mut view, "dh", start, false, Some(|| Ok(Some(Vec::new())))), "not so soon");
    assert_eq!(view.open(), [warning()]);
    poll(&mut view, "dh", start, true, Some(|| Err("down".to_string())));
    settle(&mut view, "dh");
    assert_eq!(view.open(), [warning()], "a failed read keeps the list");
    let later = start + DataManagerSplitWarningsView::REREAD;
    poll(&mut view, "dh", later, false, Some(|| Ok(Some(Vec::new()))));
    settle(&mut view, "dh");
    assert!(view.open().is_empty(), "the row that covers it ends the warning");
}

/// Another datahub forgets the list, and an older datahub (which serves no read) lists none.
#[test]
fn another_or_an_older_datahub_lists_none() {
    let mut view = DataManagerSplitWarningsView::default();
    poll(&mut view, "one", Instant::now(), false, Some(|| Ok(Some(vec![warning()]))));
    settle(&mut view, "one");
    assert_eq!(view.open().len(), 1);
    let none: Option<fn() -> SplitWarningsAnswer> = None;
    poll(&mut view, "two", Instant::now(), false, none);
    assert!(view.open().is_empty(), "another datahub's list is not this one's");
    poll(&mut view, "two", Instant::now(), true, Some(|| Ok(None)));
    settle(&mut view, "two");
    assert!(view.open().is_empty());
}
