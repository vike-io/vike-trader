use super::*;

fn fe(name: &str, date: &str) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        date: date.to_string(),
        ts_min: 0,
        ts_max: 0,
        rows: 5,
        commit_keys: Vec::new(),
    }
}

/// THE regression: the manifest state a compaction leaves behind when appends landed during its
/// unlocked merge — its `part-c…` output plus live parts whose indices are ABOVE the file count.
/// A `count + 1` name lands on `part-00006`, which is LIVE: the append truncates that file (rows
/// LOST) and adds a duplicate manifest entry (rows read TWICE). Equal loss and duplication is
/// why `concurrent_append_and_compact_no_lost_update` kept its 40-row count while failing
/// `ts unique + ascending`.
#[test]
fn next_part_name_never_collides_with_a_live_part() {
    let files = vec![
        fe("part-00005.parquet", "1970-01-01"),
        fe("part-00006.parquet", "1970-01-01"),
        fe("part-00007.parquet", "1970-01-01"),
        fe("part-00008.parquet", "1970-01-01"),
        fe("part-c00000009.parquet", "1970-01-01"),
    ];
    let name = next_part_name(&files, "1970-01-01");
    assert!(
        !files.iter().any(|f| f.name == name && f.date == "1970-01-01"),
        "chose a LIVE part name: {name}"
    );
    assert_eq!(name, "part-00009.parquet", "one above the highest live index");
}

/// Names are per-`date=` dir, so a busy neighbouring date must not push this date's index up.
#[test]
fn next_part_name_is_scoped_to_its_date() {
    let files = vec![
        fe("part-00001.parquet", "1970-01-01"),
        fe("part-00002.parquet", "1970-01-01"),
        fe("part-00003.parquet", "1970-01-02"),
    ];
    assert_eq!(next_part_name(&files, "1970-01-02"), "part-00004.parquet");
    assert_eq!(
        next_part_name(&files, "1970-01-03"),
        "part-00001.parquet",
        "unseen date starts at 1"
    );
}

/// A compaction output must never raise the index (it is a separate name space), and a series
/// holding ONLY compacted parts restarts appends at 1 — no live `part-NNNNN` to collide with.
#[test]
fn compaction_outputs_do_not_raise_the_index() {
    let files = vec![fe("part-c00000042.parquet", "1970-01-01")];
    assert_eq!(next_part_name(&files, "1970-01-01"), "part-00001.parquet");
}
