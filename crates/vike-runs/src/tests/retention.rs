//! The retention rule: what is counted, what is always kept, the refusals, and the one remover.

use std::path::Path;

use super::*;

/// A finished run `run_id` of `kind`, finished at `finished` (seconds), with `detail`, written through the
/// production writer.
fn plant(runs: &Path, run_id: &str, kind: &str, finished: i64, detail: serde_json::Value) {
    let dir = runs.join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut manifest = a_manifest(run_id);
    manifest.kind = kind.to_string();
    manifest.finished_at = utc_rfc3339(finished);
    manifest.detail = detail;
    write_run(&dir, &manifest, &json!({})).unwrap();
}

/// A single backtest of `file`, finished at `finished`.
fn backtest(runs: &Path, run_id: &str, file: &str, finished: i64) {
    plant(
        runs,
        run_id,
        BACKTEST_RUN_KIND,
        finished,
        json!({ "protocol": "single", "strategy_file": file }),
    );
}

fn rule(root: &Path, keep: usize) -> RunRetention {
    RunRetention {
        keep_per_strategy: keep,
        marks_root: root.join("marks"),
        referencing_documents: vec![root.join("alerts.json")],
    }
}

fn ids(runs: Vec<&ScannedRun>) -> Vec<&str> {
    runs.into_iter().map(|run| run.run_id.as_str()).collect()
}

/// The newest N per strategy FILE are kept, newest by when they FINISHED: a run that started first and finished
/// last is the newest. Only kind `backtest` naming a strategy file is counted: a `forward` run, a study and a
/// walk-forward over the count are never listed.
#[test]
fn the_newest_backtests_of_each_strategy_file_are_kept_and_no_other_kind_is_counted() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    backtest(&runs, "1800000000-a-0", "trend/sma.rs", 1_800_000_900);
    backtest(&runs, "1800000100-a-0", "trend/sma.rs", 1_800_000_200);
    backtest(&runs, "1800000200-a-0", "trend/sma.rs", 1_800_000_300);
    backtest(&runs, "1800000300-a-0", "fade/rsi.rs", 1_800_000_400);
    for (id, kind) in [("1700000000-f-0", "forward"), ("1700000001-s-0", "study")] {
        plant(&runs, id, kind, 1_700_000_100, json!({ "strategy_file": "trend/sma.rs" }));
    }
    plant(
        &runs,
        "1700000002-w-0",
        BACKTEST_RUN_KIND,
        1_700_000_100,
        json!({ "protocol": "permutation_test" }),
    );

    let scan = scan_runs(&runs);
    assert_eq!(ids(rule(root.path(), 2).runs_to_prune(&scan).unwrap()), ["1800000100-a-0"]);
    assert_eq!(
        ids(rule(root.path(), 1).runs_to_prune(&scan).unwrap()),
        ["1800000100-a-0", "1800000200-a-0"],
        "the run that finished last is kept although it started first"
    );
    assert!(rule(root.path(), 3).runs_to_prune(&scan).unwrap().is_empty());
}

/// Over the count and still kept: a run a mark points at (now, or in its history), a tagged run, a noted run, a run
/// a study's report names, and one the alert rules name. The one run nothing holds is listed.
#[test]
fn a_marked_tagged_noted_or_named_run_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let marks = root.path().join("marks");
    let free = "1800000000-free-0";
    let held = [
        "1800000001-mark-0",
        "1800000002-moved-0",
        "1800000003-tag-0",
        "1800000004-note-0",
        "1800000005-study-0",
        "1800000006-alert-0",
    ];
    for (i, id) in std::iter::once(free).chain(held).enumerate() {
        backtest(&runs, id, "trend/sma.rs", 1_800_000_000 + i as i64);
    }
    backtest(&runs, "1800000099-newest-0", "trend/sma.rs", 1_800_000_099);
    write_mark(&marks, "baseline/sma", "1800000002-moved-0", None, 1_800_000_100).unwrap();
    write_mark(&marks, "baseline/sma", "1800000001-mark-0", None, 1_800_000_101).unwrap();
    add_tags(&runs.join("1800000003-tag-0"), &["keep".to_string()], None, 1).unwrap();
    add_tags(&runs.join("1800000004-note-0"), &[], Some("looked at it"), 1).unwrap();
    let study = runs.join("1800000050-s-0");
    std::fs::create_dir_all(&study).unwrap();
    let mut manifest = a_manifest("1800000050-s-0");
    manifest.kind = "study".to_string();
    write_run(&study, &manifest, &json!({ "params": { "runs": ["1800000005-study-0"] } })).unwrap();
    std::fs::write(
        root.path().join("alerts.json"),
        r#"{"rules":[{"trigger":{"model_drift":{"reference_run":"1800000006-alert-0"}}}]}"#,
    )
    .unwrap();

    let scan = scan_runs(&runs);
    assert_eq!(ids(rule(root.path(), 1).runs_to_prune(&scan).unwrap()), [free]);
}

/// Keeping zero is refused (it would delete the run just saved), and so is a prune while a mark cannot be read:
/// what it points at cannot be known. Nothing is listed either time.
#[test]
fn zero_and_an_unreadable_mark_refuse_the_prune() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    backtest(&runs, "1800000000-a-0", "trend/sma.rs", 1_800_000_000);
    backtest(&runs, "1800000001-a-0", "trend/sma.rs", 1_800_000_001);
    let scan = scan_runs(&runs);
    let zero = rule(root.path(), 0).runs_to_prune(&scan).unwrap_err();
    assert!(zero.contains("the least is 1"), "{zero}");

    std::fs::create_dir_all(root.path().join("marks")).unwrap();
    std::fs::write(root.path().join("marks").join("broken.json"), "{ not a mark").unwrap();
    let broken = rule(root.path(), 1).runs_to_prune(&scan).unwrap_err();
    assert!(broken.contains("no run is pruned while a mark cannot be read"), "{broken}");
}

/// `prune` removes exactly what `runs_to_prune` lists, each run's whole directory, and leaves the rest.
#[test]
fn prune_removes_each_listed_run_directory_whole() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    backtest(&runs, "1800000000-a-0", "trend/sma.rs", 1_800_000_000);
    backtest(&runs, "1800000001-a-0", "trend/sma.rs", 1_800_000_001);
    std::fs::write(runs.join("1800000000-a-0").join("trades.json"), "{}").unwrap();

    assert_eq!(rule(root.path(), 1).prune(&runs).unwrap(), ["1800000000-a-0"]);
    assert!(!runs.join("1800000000-a-0").exists());
    assert!(runs.join("1800000001-a-0").join(MANIFEST_FILE).is_file());
    assert!(rule(root.path(), 1).prune(&runs).unwrap().is_empty(), "a second prune finds nothing");
}

/// The remover refuses any value that is not one run directory under the runs root: a traversal, a separator, an
/// absolute path, the empty id, and a directory with no manifest (a run still being written).
#[test]
fn remove_run_refuses_anything_but_a_finished_run_under_the_root() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    backtest(&runs, "1800000000-a-0", "trend/sma.rs", 1_800_000_000);
    std::fs::create_dir_all(runs.join("1800000001-unfinished-0")).unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let absolute = outside.display().to_string();
    for bad in ["", "..", "../outside", "a/b", absolute.as_str(), "1800000001-unfinished-0"] {
        assert!(remove_run(&runs, bad).is_err(), "{bad:?} was not refused");
    }
    assert!(outside.is_dir() && runs.join("1800000001-unfinished-0").is_dir());
    remove_run(&runs, "1800000000-a-0").unwrap();
    assert!(!runs.join("1800000000-a-0").exists());
}

/// A run directory that is a LINK to a finished run elsewhere is refused, and what it points at survives.
#[cfg(unix)]
#[test]
fn remove_run_refuses_a_linked_run_directory() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = root.path().join("elsewhere");
    backtest(&elsewhere, "1800000000-a-0", "trend/sma.rs", 1_800_000_000);
    let runs = root.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("1800000000-a-0"), runs.join("1800000000-a-0"))
        .unwrap();
    let refused = remove_run(&runs, "1800000000-a-0").unwrap_err();
    assert!(refused.contains("it is a link"), "{refused}");
    assert!(elsewhere.join("1800000000-a-0").join(MANIFEST_FILE).is_file());
}

/// The unfinished remover takes only the run directory a writer minted and never finished: one id under the root,
/// no manifest, no link. A finished run, a directory outside the root and a mismatched id are refused, and survive.
#[test]
fn remove_unfinished_run_removes_only_the_unfinished_run_it_was_handed() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let unfinished = crate::create_run_dir(&runs, 1_800_000_001, None).unwrap();
    std::fs::write(unfinished.path.join(REPORT_FILE), "{\"rows\":[").unwrap();
    backtest(&runs, "1800000000-a-0", "trend/sma.rs", 1_800_000_000);
    let finished =
        RunDir { run_id: "1800000000-a-0".to_string(), path: runs.join("1800000000-a-0") };
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let elsewhere = RunDir { run_id: "outside".to_string(), path: outside.clone() };
    let traversal = RunDir { run_id: "..".to_string(), path: runs.join("..") };
    let renamed = RunDir { run_id: "1800000000-a-0".to_string(), path: unfinished.path.clone() };
    for bad in [&finished, &elsewhere, &traversal, &renamed] {
        assert!(remove_unfinished_run(&runs, bad).is_err(), "{:?} was not refused", bad.path);
    }
    let refused = remove_unfinished_run(&runs, &finished).unwrap_err();
    assert!(refused.contains("finished run"), "{refused}");
    assert!(
        outside.is_dir() && finished.path.join(MANIFEST_FILE).is_file() && unfinished.path.is_dir()
    );
    remove_unfinished_run(&runs, &unfinished).unwrap();
    assert!(!unfinished.path.exists(), "the unfinished run is removed whole");
}

/// An unfinished run directory that is a LINK is refused, and what it points at survives.
#[cfg(unix)]
#[test]
fn remove_unfinished_run_refuses_a_linked_run_directory() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("1800000001-b-0")).unwrap();
    let runs = root.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("1800000001-b-0"), runs.join("1800000001-b-0"))
        .unwrap();
    let linked = RunDir { run_id: "1800000001-b-0".to_string(), path: runs.join("1800000001-b-0") };
    let refused = remove_unfinished_run(&runs, &linked).unwrap_err();
    assert!(refused.contains("it is a link"), "{refused}");
    assert!(elsewhere.join("1800000001-b-0").is_dir());
}

/// A bar run's `detail`, as heavy as a saved one: an event-log recipe stamping `days` days.
fn stamped_detail(file: &str, days: i64) -> serde_json::Value {
    let days: Vec<_> =
        (0..days).map(|d| json!({ "day": d, "provisional": false, "stamp": "s" })).collect();
    json!({ "protocol": "single", "strategy_file": file, "equity_log": { "series": [{ "days": days }] } })
}

/// The retention's read and the listing's of the manifest text `text` in `dir`: refused alike, and when both
/// read it, counted alike (the same run, kind, finish and strategy file).
fn assert_read_alike(dir: &Path, text: &str) {
    std::fs::write(dir.join(MANIFEST_FILE), text).unwrap();
    let (full, counted) = (read_manifest(dir), crate::retention::read_counted_manifest(dir));
    assert_eq!(full.is_ok(), counted.is_ok(), "{text}\nfull: {full:?}\ncounted: {counted:?}");
    if let (Ok(full), Ok(counted)) = (full, counted) {
        let seen = |m: &RunManifest| {
            (
                m.run_id.clone(),
                m.kind.clone(),
                m.finished_at.clone(),
                m.strategy_file().map(String::from),
            )
        };
        assert_eq!(seen(&full), seen(&counted), "{text}");
    }
}

/// **The prune's read refuses exactly what the listing's read refuses** (`crate::retention`'s `CountedManifest`): a
/// whole manifest with every field set, then each top-level field removed and set to every JSON type, then `detail`
/// as values only a full read would catch (a number out of range, a lone surrogate) and as the keys it counts by
/// (repeated, escaped, nested, not a string). A field added to `RunManifest` and not to the counted read fails here.
#[test]
fn the_counted_read_refuses_exactly_what_the_manifest_read_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = a_manifest("1800000000-a-0");
    manifest.git_sha = Some("abc1234".to_string());
    manifest.fingerprint = Some("ff".to_string());
    manifest.detail = stamped_detail("trend/sma.rs", 30);
    manifest.kept_rows = vec![vike_model::Instrument::new(
        "binance",
        "BTCUSDT",
        "BTC",
        "USDT",
        vike_model::Contract::CryptoSpot,
    )];
    let whole = serde_json::to_value(&manifest).unwrap();
    assert_read_alike(dir.path(), &whole.to_string());
    let types = [
        json!(null),
        json!(true),
        json!(7),
        json!(-7),
        json!(1.5),
        json!("x"),
        json!([]),
        json!({}),
        json!([1]),
        json!({ "a": 1 }),
    ];
    for key in whole.as_object().unwrap().keys() {
        let mut without = whole.clone();
        without.as_object_mut().unwrap().remove(key);
        assert_read_alike(dir.path(), &without.to_string());
        for value in &types {
            let mut changed = whole.clone();
            changed[key] = value.clone();
            assert_read_alike(dir.path(), &changed.to_string());
        }
    }
    let mut slot = whole.clone();
    slot["detail"] = json!("DETAIL_SLOT");
    let slot = slot.to_string();
    for detail in [
        r#"1e400"#,
        r#"{"x":[1,{"y":1e400}]}"#,
        r#"{"x":"\ud800"}"#,
        r#"{"strategy_file":"a.rs","strategy_file":"b.rs"}"#,
        r#"{"strategy_file":"a.rs","strategy_file":7}"#,
        r#"{"strategy_file":"escaped.rs"}"#,
        r#"{"x":{"strategy_file":"nested.rs"}}"#,
        r#"[{"strategy_file":"in-an-array.rs"}]"#,
        r#"{"strategy_file":{"a":1}}"#,
        r#""strategy_file""#,
        r#"{"strategy_file":"a.rs","x":[1,2,{"z":null}],"y":-0.0}"#,
    ] {
        assert_read_alike(dir.path(), &slot.replace("\"DETAIL_SLOT\"", detail));
    }
    assert_read_alike(dir.path(), &whole.to_string()[..40]);
}

/// **The prune counts the runs the listing reads and removes exactly what the dry run lists**: over planted runs
/// (three strategy files, two of them over a retention of 2, with day-stamped details; a study; a forward run; a
/// backtest naming no file, one naming a number and one naming two files by a repeated key; a manifest missing a
/// field; a run being written), the counted scan lists the same runs counted the same, both lists of runs to prune
/// are one list, and `prune` removes it and nothing else.
#[test]
fn the_prune_counts_the_runs_the_listing_reads_and_removes_the_dry_runs_list() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let mut at = 1_800_000_000;
    let mut plant_backtest = |id: &str, detail: serde_json::Value| {
        at += 10;
        plant(&runs, id, BACKTEST_RUN_KIND, at, detail);
    };
    for i in 0..5 {
        plant_backtest(&format!("18000000{i:02}-sma-0"), stamped_detail("trend/sma.rs", 180));
    }
    for i in 0..3 {
        plant_backtest(&format!("18000001{i:02}-rsi-0"), stamped_detail("fade/rsi.rs", 1_000));
    }
    plant_backtest("1800000200-one-0", stamped_detail("solo/one.rs", 5));
    plant_backtest("1800000300-none-0", json!({ "protocol": "single" }));
    plant_backtest("1800000301-number-0", json!({ "strategy_file": 7 }));
    for (id, kind) in [("1800000400-study-0", "study"), ("1800000401-forward-0", "forward")] {
        plant(&runs, id, kind, 1_800_000_000, json!({ "strategy_file": "trend/sma.rs" }));
    }
    let twice = runs.join("1800000500-twice-0");
    std::fs::create_dir_all(&twice).unwrap();
    let mut manifest = a_manifest("1800000500-twice-0");
    manifest.finished_at = utc_rfc3339(1_700_000_000);
    manifest.detail = json!("DETAIL_SLOT");
    let text = serde_json::to_string(&manifest).unwrap().replace(
        "\"DETAIL_SLOT\"",
        r#"{"strategy_file":"solo/one.rs","strategy_file":"trend/sma.rs"}"#,
    );
    std::fs::write(twice.join(MANIFEST_FILE), text).unwrap();
    let broken = runs.join("1800000600-broken-0");
    std::fs::create_dir_all(&broken).unwrap();
    let mut no_kind = serde_json::to_value(a_manifest("1800000600-broken-0")).unwrap();
    no_kind.as_object_mut().unwrap().remove("kind");
    std::fs::write(broken.join(MANIFEST_FILE), no_kind.to_string()).unwrap();
    std::fs::create_dir_all(runs.join("1800000700-unfinished-0")).unwrap();

    let full = scan_runs(&runs);
    let counted = crate::scan::scan_runs_reading(&runs, crate::retention::read_counted_manifest);
    let rows = |scan: &RunScan| {
        let runs: Vec<_> = scan
            .runs
            .iter()
            .map(|r| {
                let m = &r.manifest;
                (
                    r.run_id.clone(),
                    r.dir.clone(),
                    m.kind.clone(),
                    m.finished_at.clone(),
                    m.strategy_file().map(String::from),
                )
            })
            .collect();
        let diagnostics: Vec<_> = scan.diagnostics.iter().map(std::mem::discriminant).collect();
        (runs, diagnostics)
    };
    assert_eq!(rows(&full), rows(&counted));
    assert_eq!(full.runs.len(), 14, "{:?}", full.diagnostics);
    assert_eq!(full.diagnostics.len(), 2, "the broken manifest and the unfinished run");

    let keep = rule(root.path(), 2);
    let listed = ids(keep.runs_to_prune(&full).unwrap());
    assert_eq!(
        listed,
        [
            "1800000100-rsi-0",
            "1800000500-twice-0",
            "1800000000-sma-0",
            "1800000001-sma-0",
            "1800000002-sma-0"
        ],
        "the oldest over 2 of each file, the repeated key counted by its last spelling"
    );
    assert_eq!(ids(keep.runs_to_prune(&counted).unwrap()), listed);
    assert_eq!(keep.prune(&runs).unwrap(), listed);
    let left: Vec<String> = scan_runs(&runs).runs.into_iter().map(|r| r.run_id).collect();
    assert_eq!(left.len(), 9);
    assert!(listed.iter().all(|id| !left.iter().any(|l| l == id)), "{left:?}");
}

/// A bar run's `detail` as the backtesthub saves it since [`DAY_STAMPS_FILE`] (decision 0078's amendment of
/// 2026-11-19), over `days` days: the recipe under `equity_log_recipe` with no day in it, naming the file of its
/// stamps; `inline` is the layout before it, every day stamped under `equity_log`. `0` days is a run with no event
/// log. The recipe's fields are a saved one's, so a manifest is about as long as a saved one.
fn saved_detail(file: &str, days: usize, inline: bool) -> serde_json::Value {
    let mut detail =
        json!({ "protocol": "single", "strategy_file": file, "strategy_sha256": "ab".repeat(32) });
    if days == 0 {
        return detail;
    }
    let stamps: Vec<_> = (0..days)
        .map(|d| json!({ "day": 19_700 + d, "provisional": false, "stamp": u64::MAX - d as u64 }))
        .collect();
    let series = json!({
        "venue": "binance", "symbol": "BTCUSDT", "source": null, "interval": "1h",
        "from": 1_700_000_000_000_i64, "to": null, "splits": [],
        "first_bar": 1_700_000_000_000_i64, "last_bar": 1_700_000_000_000_i64 + days as i64 * 86_400_000,
        "closes_fnv": "0123456789abcdef", "days": if inline { json!(stamps) } else { json!(null) },
    });
    let mut recipe = json!({
        "format": 1, "build": "abc1234", "steps": days * 24, "curve_fnv": "0123456789abcdef",
        "cash": 10_000.0, "currency": "USDT", "decide": "sequential", "symbols": ["BTCUSDT"],
        "values": [{ "Linear": { "multiplier": 1.0 } }], "series": [series],
    });
    if inline {
        detail["equity_log"] = recipe;
    } else {
        recipe["day_stamps"] = json!({ "fnv": "0123456789abcdef", "count": days });
        detail["equity_log_recipe"] = recipe;
    }
    detail
}

/// Plant the backtest `run_id` of `file` with [`saved_detail`]'s, finished at `finished`, and its day stamps file
/// beside it when they are apart (about the bytes a saved one holds: its content is no scan's business).
fn plant_saved(runs: &Path, run_id: &str, file: &str, finished: i64, days: usize, inline: bool) {
    plant(runs, run_id, BACKTEST_RUN_KIND, finished, saved_detail(file, days, inline));
    if days > 0 && !inline {
        std::fs::write(runs.join(run_id).join(DAY_STAMPS_FILE), vec![0x81; 31 * days]).unwrap();
    }
}

/// What a listing renders and the retention counts by, per run, and how many diagnostics the scan had.
type Listed = (Vec<(String, String, String, Option<String>)>, usize);

fn listed(scan: &RunScan) -> Listed {
    let rows = scan.runs.iter().map(|r| {
        let m = &r.manifest;
        let file = m.strategy_file().map(String::from);
        (r.run_id.clone(), m.kind.clone(), m.finished_at.clone(), file)
    });
    (rows.collect(), scan.diagnostics.len())
}

/// **Prune and listing decide alike over runs saved in either layout** (decision 0078's amendment of 2026-11-19): one
/// set of runs, planted once with every day stamp inline and once with every other run's stamps in
/// [`DAY_STAMPS_FILE`], lists the same rows and prunes the same runs, through the whole read and the common one
/// ([`scan_runs_common`]); the prune removes those runs, day stamps file and all, and nothing else.
#[test]
fn prune_and_listing_decide_alike_over_runs_saved_with_inline_or_apart_day_stamps() {
    let root = tempfile::tempdir().unwrap();
    let plant_set = |runs: &Path, apart_every_other: bool| {
        for i in 0..12_usize {
            let file = ["trend/sma.rs", "fade/rsi.rs", "solo/one.rs"][i % 3];
            let inline = !(apart_every_other && i % 2 == 0);
            let id = format!("18000000{i:02}-a-0");
            plant_saved(runs, &id, file, 1_800_000_000 + 10 * i as i64, 30 * i, inline);
        }
        let study = json!({ "strategy_file": "a.rs" });
        plant(runs, "1800000400-study-0", "study", 1_800_000_000, study);
    };
    let (inline, mixed) = (root.path().join("inline"), root.path().join("mixed"));
    plant_set(&inline, false);
    plant_set(&mixed, true);
    assert!(mixed.join("1800000002-a-0").join(DAY_STAMPS_FILE).is_file(), "the mix holds both");
    let keep = rule(root.path(), 2);
    let decided = |runs: &Path| -> (Listed, Vec<String>) {
        let (full, common) = (scan_runs(runs), scan_runs_common(runs));
        assert_eq!(listed(&full), listed(&common));
        let over = keep.runs_to_prune(&full).unwrap();
        let pruned: Vec<String> = ids(over).into_iter().map(String::from).collect();
        assert_eq!(ids(keep.runs_to_prune(&common).unwrap()), pruned);
        (listed(&full), pruned)
    };
    let before = decided(&inline);
    assert_eq!(decided(&mixed), before, "one decision for both layouts");
    assert_eq!((before.0.0.len(), before.0.1), (13, 0));
    assert_eq!(before.1.len(), 6, "two of each file's four are over 2: {:?}", before.1);
    assert_eq!(keep.prune(&mixed).unwrap(), before.1);
    assert_eq!(keep.prune(&inline).unwrap(), before.1);
    assert!(before.1.iter().all(|id| !mixed.join(id).exists()), "removed whole");
    assert_eq!(scan_runs(&mixed).runs.len(), 7);
}

/// **Time the listing's and the prune's scan of 300 saved runs, before and after** (by hand on a lane:
/// `cargo test -p vike-runs --release --lib -- --ignored --nocapture scan_of_300_saved_runs`). the CI box's mix on
/// 2026-11-19 was 263 runs and 5.5 MB of manifests; this plants 300: 25 three-year 1h bar runs (1,095 days each), 50
/// six-month ones (184 days) and 225 with no event log, over 30 strategy files of 10 runs (the prune removes nothing,
/// as on the CI box then). BEFORE is every day stamp inline, read whole ([`scan_runs`], what both read until now); AFTER
/// is [`scan_runs_common`] over the day stamps apart (a run saved now), and over inline ones (an older run, until the
/// retention removes it). Prints the median of 15 warm passes; asserts only that every scan lists the same rows.
#[test]
#[ignore = "a timing, by hand on a lane"]
fn scan_of_300_saved_runs_before_and_after() {
    use std::time::{Duration, Instant};
    let root = tempfile::tempdir().unwrap();
    let plant_all = |runs: &Path, inline: bool| {
        for i in 0..300_usize {
            let days = match i % 12 {
                0 => 1_095,
                1 | 2 => 184,
                _ => 0,
            };
            let (id, file) =
                (format!("{}-a-0", 1_800_000_000 + i), format!("lab/s{:02}.rs", i % 30));
            plant_saved(runs, &id, &file, 1_800_000_000 + i as i64, days, inline);
        }
    };
    let (inline, apart) = (root.path().join("inline"), root.path().join("apart"));
    plant_all(&inline, true);
    plant_all(&apart, false);
    let bytes = |runs: &Path| -> u64 {
        let manifests =
            std::fs::read_dir(runs).unwrap().map(|run| run.unwrap().path().join(MANIFEST_FILE));
        manifests.map(|path| std::fs::metadata(path).unwrap().len()).sum()
    };
    let keep = rule(root.path(), 10);
    let median = |pass: &dyn Fn()| -> Duration {
        pass();
        let mut times: Vec<Duration> = (0..15)
            .map(|_| {
                let at = Instant::now();
                pass();
                at.elapsed()
            })
            .collect();
        times.sort();
        times[times.len() / 2]
    };
    let rows = listed(&scan_runs(&inline));
    assert_eq!((rows.0.len(), rows.1), (300, 0));
    assert_eq!(listed(&scan_runs_common(&inline)), rows);
    assert_eq!(listed(&scan_runs(&apart)), rows);
    assert_eq!(listed(&scan_runs_common(&apart)), rows);
    println!("TIMING: manifests inline {} bytes, apart {} bytes", bytes(&inline), bytes(&apart));
    for (what, runs) in [("inline", &inline), ("apart", &apart)] {
        let whole = median(&|| assert_eq!(scan_runs(runs).runs.len(), 300));
        let common = median(&|| assert_eq!(scan_runs_common(runs).runs.len(), 300));
        let whole_prune =
            median(&|| assert!(keep.runs_to_prune(&scan_runs(runs)).unwrap().is_empty()));
        let prune = median(&|| assert!(keep.prune(runs).unwrap().is_empty()));
        println!(
            "TIMING: {what}: listing whole {whole:?}, listing common {common:?}; prune over the whole \
             read {whole_prune:?}, prune {prune:?}"
        );
    }
}
