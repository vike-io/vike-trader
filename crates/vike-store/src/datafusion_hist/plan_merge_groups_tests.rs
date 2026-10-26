use super::*;

/// Manifest entries for parts of the given `(rows, bytes)`, with the files written to `dir` so
/// the `target_bytes` check has something real to stat.
fn parts(dir: &Path, spec: &[(usize, u64)]) -> Vec<manifest::FileEntry> {
    std::fs::create_dir_all(dir).unwrap();
    spec.iter()
        .enumerate()
        .map(|(i, &(rows, bytes))| {
            let name = format!("part-{i:05}.parquet");
            std::fs::write(dir.join(&name), vec![0u8; bytes as usize]).unwrap();
            manifest::FileEntry {
                name,
                date: "1970-01-01".into(),
                ts_min: i as i64,
                ts_max: i as i64,
                rows,
                commit_keys: vec![],
            }
        })
        .collect()
}

/// A config that only the knob under test constrains: huge `target_bytes` so the size check
/// never fires, `min_parts` at the normal 2.
fn rows_only(max_merge_rows: usize) -> CompactionConfig {
    CompactionConfig { target_bytes: u64::MAX, min_parts: 2, max_merge_rows }
}

fn all(files: &[manifest::FileEntry]) -> Vec<usize> {
    (0..files.len()).collect()
}

#[test]
fn a_date_within_budget_stays_one_group() {
    let d = tempfile::tempdir().unwrap();
    let files = parts(d.path(), &[(10, 10), (10, 10), (10, 10), (10, 10)]);
    assert_eq!(
        plan_merge_groups(d.path(), &files, &all(&files), &rows_only(1000)),
        vec![vec![0, 1, 2, 3]]
    );
}

/// The property the OOM was the absence of: no group may exceed the budget just because the
/// date does. Bytes are held constant here — ROWS are what bounds the merge.
#[test]
fn an_oversized_date_splits_into_groups_within_the_row_budget() {
    let d = tempfile::tempdir().unwrap();
    let files = parts(d.path(), &[(40, 1); 6]);
    let groups = plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100));
    assert_eq!(groups, vec![vec![0, 1], vec![2, 3], vec![4, 5]]);
    for g in &groups {
        assert!(g.len() * 40 <= 100, "group over budget: {g:?}");
    }
}

/// Byte-identical parts that differ only in ROW COUNT must group differently — the regression
/// guard for the first fix, which bounded by compressed bytes and still peaked at 8.95 GB on
/// the CI box because compressed size says nothing about decoded size.
#[test]
fn grouping_follows_rows_not_file_size() {
    let d = tempfile::tempdir().unwrap();
    let fat = parts(d.path(), &[(60, 10), (60, 10), (60, 10)]);
    let lean = parts(d.path(), &[(10, 10), (10, 10), (10, 10)]);
    let cfg = rows_only(100);
    assert_eq!(plan_merge_groups(d.path(), &fat, &all(&fat), &cfg), vec![vec![0, 1]]);
    assert_eq!(plan_merge_groups(d.path(), &lean, &all(&lean), &cfg), vec![vec![0, 1, 2]]);
}

/// A leftover single part is not a merge — merging one part into one part only renames it.
#[test]
fn a_trailing_single_part_group_is_dropped() {
    let d = tempfile::tempdir().unwrap();
    let files = parts(d.path(), &[(60, 1), (60, 1), (60, 1)]);
    assert_eq!(
        plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100)),
        vec![vec![0, 1]]
    );
}

/// A part whose own row count reaches the budget can never be merged within it. Skipping it is
/// what keeps the at-least-2 rule from pairing two such parts and blowing the bound wide open.
#[test]
fn a_part_at_the_row_budget_is_skipped_rather_than_paired() {
    let d = tempfile::tempdir().unwrap();
    let files = parts(d.path(), &[(500, 1), (500, 1), (10, 1), (10, 1)]);
    assert_eq!(
        plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100)),
        vec![vec![2, 3]],
        "the two oversized parts were paired — that is 1000 rows in one decode"
    );
}

/// `target_bytes` keeps its own meaning: a part already at the sealed-file size is finished,
/// however few rows it holds.
#[test]
fn a_part_at_target_bytes_is_left_alone() {
    let d = tempfile::tempdir().unwrap();
    let files = parts(d.path(), &[(10, 500), (10, 10), (10, 10)]);
    let cfg = CompactionConfig { target_bytes: 100, min_parts: 2, max_merge_rows: 1000 };
    assert_eq!(plan_merge_groups(d.path(), &files, &all(&files), &cfg), vec![vec![1, 2]]);
}

/// A part the planner cannot stat is treated as not-yet-at-target — the merge reports the real
/// error, and a missing file never silently drops its siblings out of the pass.
#[test]
fn an_unstattable_part_still_joins_its_group() {
    let d = tempfile::tempdir().unwrap();
    let mut files = parts(d.path(), &[(10, 10), (10, 10)]);
    files.push(manifest::FileEntry {
        name: "part-99999.parquet".into(),
        date: "1970-01-01".into(),
        ts_min: 9,
        ts_max: 9,
        rows: 10,
        commit_keys: vec![],
    });
    let cfg = CompactionConfig { target_bytes: 100, min_parts: 2, max_merge_rows: 1000 };
    assert_eq!(plan_merge_groups(d.path(), &files, &all(&files), &cfg), vec![vec![0, 1, 2]]);
}

/// `min_parts = 1` is the caller saying a LONE fragment is worth a pass — the one-part "merge"
/// re-encodes it under the current schema, which is how an older-schema part is upgraded (and
/// what `run_maintenance_handles_chain_series` leans on). The trailing-single rule must not
/// quietly take that mode away.
#[test]
fn min_parts_of_one_keeps_a_lone_part_as_its_own_group() {
    let d = tempfile::tempdir().unwrap();
    let files = parts(d.path(), &[(10, 10)]);
    let one = CompactionConfig { min_parts: 1, ..rows_only(100) };
    assert_eq!(plan_merge_groups(d.path(), &files, &all(&files), &one), vec![vec![0]]);
    assert!(plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100)).is_empty());
}
