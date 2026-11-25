use super::*;

const GIB: u64 = 1 << 30;

/// A fake reader of one filesystem of `total` bytes with `available` free, wherever it is asked.
fn disk(available: u64, total: u64) -> impl Fn(&Path) -> Result<FilesystemSpace, String> {
    move |_| Ok(FilesystemSpace { available_bytes: available, total_bytes: total })
}

/// The floor is the larger of the reserve and 5% of the size: a 100 GiB disk keeps the 10 GiB default, a
/// 1 TiB disk keeps 5% of itself; a write is refused below it, in words that name the row, and allowed at it.
#[test]
fn the_floor_is_the_reserve_or_five_percent_and_a_write_below_it_is_refused() {
    let here = std::env::temp_dir();
    let floor = DiskFloor::of(&vike_config::Config::default());
    assert_eq!(floor, DiskFloor::of_mib(10_240), "the row's default");
    assert_eq!(floor.floor_bytes(100 * GIB), 10 * GIB);
    assert_eq!(floor.floor_bytes(1_024 * GIB), 1_024 * GIB / 100 * 5);

    let refused = floor.refusal_with(&here, disk(10 * GIB - 1, 100 * GIB)).expect("below");
    assert!(refused.contains("config.optional_writes_min_free_mib"), "{refused}");
    assert!(refused.contains("10239 MiB free") && refused.contains("10240 MiB"), "{refused}");
    assert_eq!(floor.refusal_with(&here, disk(10 * GIB, 100 * GIB)), None, "at the floor");
    assert!(floor.refusal_with(&here, disk(40 * GIB, 1_024 * GIB)).is_some(), "under 5%");

    let set = vike_config::Config { optional_writes_min_free_mib: Some(1), ..Default::default() };
    assert_eq!(DiskFloor::of(&set).refusal_with(&here, disk(2 << 20, 20 << 20)), None);
}

/// A filesystem that cannot be measured is not a full one, and a directory not created yet is measured on
/// the nearest one that exists.
#[test]
fn an_unmeasured_disk_writes_and_a_missing_directory_is_measured_where_it_will_be() {
    let floor = DiskFloor::of_mib(1);
    let unmeasured = |_: &Path| Err::<FilesystemSpace, _>("no statvfs".to_string());
    assert_eq!(floor.refusal_with(&std::env::temp_dir(), unmeasured), None);

    let root = std::env::temp_dir();
    let missing = root.join("vike-disk-floor-not-created").join("runs");
    let asked = std::sync::Mutex::new(None);
    let refused = floor.refusal_with(&missing, |p: &Path| {
        *asked.lock().unwrap() = Some(p.to_path_buf());
        Ok(FilesystemSpace { available_bytes: 0, total_bytes: 1 << 30 })
    });
    assert!(refused.is_some());
    assert_eq!(asked.into_inner().unwrap().as_deref(), Some(root.as_path()));
}

/// The real reader measures an existing directory (non-zero on any box that can run the test) and refuses
/// an absent one.
#[cfg(unix)]
#[test]
fn the_real_reader_measures_a_directory_and_refuses_an_absent_one() {
    let space = filesystem_space(&std::env::temp_dir()).expect("statvfs of the temp dir");
    assert!(space.total_bytes > 0 && space.available_bytes <= space.total_bytes, "{space:?}");
    assert!(filesystem_space(Path::new("/vike-disk-floor/absent")).is_err());
}
