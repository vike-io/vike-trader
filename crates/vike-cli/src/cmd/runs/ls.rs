//! `vike-cli backtest ls [selector]` — **the registry listing, the leaderboard and the job list are
//! one question with three filters, so they are one verb** (spec §6.1).
//!
//! # One field vocabulary, three flags
//!
//! `--cols`, `--sort` and `--where` (`crate::cmd::runs::where_expr`) all resolve a name through
//! [`field_of`] and nothing else. A second resolver is how `--sort sharpe` and `--where sharpe>1`
//! come to disagree about what `sharpe` means, which is a bug nobody would ever see reported.
//!
//! | name | source |
//! |---|---|
//! | `run_id` | the DIRECTORY name — `crate::cmd::runs::scan` carries why it is not the declared one |
//! | `kind` `produced_by` `started_at` `finished_at` `git_sha` | `vike_model::runs::RunManifest` |
//! | `config.path` `config.name` | `vike_model::runs::RunConfig` |
//! | `report` | `yes`/`no` — whether the file is on disk. Nothing here reads it for this |
//! | `detail.<dotted.path>` | a scalar in the kind-specific subtree |
//! | anything else | a TOP-LEVEL key of `report.json` |
//!
//! The last row is what makes `--sort sharpe` read the way an operator expects. ⚠ Its cost, stated
//! rather than implied: a misspelled metric is not an unknown FLAG, it is a column of `-`. `--sort`
//! refuses a key NO run carries, which catches the typo; a key SOME runs carry is a heterogeneous
//! store rather than a mistake, and those rows sort last.
//!
//! # `report.json` is opened LAZILY, and never deserialized into a type
//!
//! Only a column, sort key or predicate naming a metric opens it — a bare `ls` over five hundred
//! runs reads five hundred manifests and zero reports. And it is read as `serde_json::Value`,
//! because `vike_analytics::report::BacktestReport` derives `Serialize` ONLY;
//! `crate::cmd::backtest`'s `execute` already does the same and says so. Two shapes a reader must
//! tolerate: `profit_factor` is `null` when non-finite, and `zero_trade` is skipped when absent.
//! Both read as [`FieldValue::Missing`] — never as `0.0`, which would rank a broken run above a
//! good one.
//!
//! # `--json`
//!
//! `{ "root": <the tree that was scanned>, "count": N, "runs": [ {<column>: <value>}, … ] }`, keyed
//! by the SAME names the table's headers use. Emitted on SUCCESS only; a failure is a sentence on
//! stderr plus a rung, and stdout carries nothing — `crate::cmd::runs`'s doc states the rule for
//! this whole family. The scan's `problems` go to stderr in BOTH modes, so one unreadable run
//! directory cannot make the document unparseable.

use std::path::Path;

use crate::cmd::backtest::ReadArgs;
use crate::cmd::runs::scan::{RunScan, ScannedRun, scan_runs};
use crate::cmd::runs::{Ctx, selector::resolve_many};
use crate::exit::{CliError, CmdResult};

/// The default column set: manifest-only, so a bare `ls` opens no report file.
/// `crates/vike-studio/src/research.rs`'s `run_rows` shows the same fields for the same reason.
pub(crate) const DEFAULT_COLS: [&str; 4] = ["run_id", "kind", "started_at", "report"];

/// One resolved field. `Missing` is distinct from absent-name on purpose: a `null` metric and a
/// misspelled one are different problems and only one of them is the operator's fault.
pub(crate) enum FieldValue {
    Num(f64),
    Text(String),
    Missing,
}

impl FieldValue {
    pub(crate) fn render(&self) -> String {
        match self {
            FieldValue::Num(n) => render_num(*n),
            FieldValue::Text(t) => t.clone(),
            FieldValue::Missing => "-".to_string(),
        }
    }
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            FieldValue::Num(n) => serde_json::json!(n),
            FieldValue::Text(t) => serde_json::json!(t),
            FieldValue::Missing => serde_json::Value::Null,
        }
    }
}

/// Six decimal places, trailing zeros trimmed — a leaderboard column, not an accounting one.
fn render_num(n: f64) -> String {
    let s = format!("{n:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".to_string() } else { s.to_string() }
}

/// THE field resolver. Every flag that names a field goes through this and nothing else.
pub(crate) fn field_of(run: &ScannedRun, name: &str) -> Option<FieldValue> {
    let m = &run.manifest;
    let text = |s: &str| Some(FieldValue::Text(s.to_string()));
    let opt =
        |s: Option<&str>| Some(s.map_or(FieldValue::Missing, |v| FieldValue::Text(v.to_string())));
    match name {
        "run_id" => text(&run.run_id),
        "kind" => text(&m.kind),
        "produced_by" => text(&m.produced_by),
        "started_at" => text(&m.started_at),
        "finished_at" => text(&m.finished_at),
        "git_sha" => opt(m.git_sha.as_deref()),
        "config.path" => opt(m.config.path.as_deref()),
        "config.name" => opt(m.config.name.as_deref()),
        "report" => text(if run.report.is_some() { "yes" } else { "no" }),
        other => match other.strip_prefix("detail.") {
            Some(path) => Some(from_json(&m.detail, path)),
            // The fallthrough: a bare name is a TOP-LEVEL key of `report.json`.
            None => Some(report_value(run, other)),
        },
    }
}

/// Read `report.json` ONCE per call and pull one top-level key out of it as a scalar.
///
/// ⚠ No caching, deliberately. A cache would need a `RefCell` or a `&mut` threaded through every
/// renderer, and the measured shape this serves is tens-to-hundreds of runs and one-to-three metric
/// columns — a few hundred small reads. If a store ever grows past that, the fix is to hoist the
/// parse into [`run_ls`] and pass a `&Value` down, not to add interior mutability here.
fn report_value(run: &ScannedRun, key: &str) -> FieldValue {
    let Some(path) = &run.report else { return FieldValue::Missing };
    let Ok(text) = std::fs::read_to_string(path) else { return FieldValue::Missing };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return FieldValue::Missing;
    };
    from_json(&v, key)
}

/// One dotted path into a JSON document, as a SCALAR. An object or an array is `Missing`: a table
/// cell and a comparison both need one value, and rendering `{…}` into a column teaches nobody
/// anything.
fn from_json(root: &serde_json::Value, dotted: &str) -> FieldValue {
    let mut cur = root;
    for seg in dotted.split('.') {
        match cur.get(seg) {
            Some(next) => cur = next,
            None => return FieldValue::Missing,
        }
    }
    match cur {
        serde_json::Value::Number(n) => n.as_f64().map_or(FieldValue::Missing, FieldValue::Num),
        serde_json::Value::String(s) => FieldValue::Text(s.clone()),
        serde_json::Value::Bool(b) => FieldValue::Text(b.to_string()),
        // `null` — a non-finite `profit_factor`, or a `git_sha` the producer could not name.
        _ => FieldValue::Missing,
    }
}

/// `--sort FIELD[:asc|:desc]`. Numbers descend by default (a leaderboard shows the best first);
/// text ascends by default (a name list reads alphabetically). Runs missing the key sort LAST in
/// both directions — a blank is never "best".
///
/// ⚠ **`known` is the UNFILTERED scan, and `runs` is what survived the filters.** The typo check
/// asks whether the STORE carries the field, never whether the surviving rows do — because
/// `all(Missing)` is VACUOUSLY TRUE over an empty slice, and `ls --sort sharpe` on a fresh project,
/// or after a `--where` that matched nothing, would then exit `2` accusing the operator of a typo
/// for a store that is simply empty. That contradicts the rung table this family declares (an empty
/// listing is an ANSWER on rung `0`) and it contradicts this module's own doc, which says the check
/// "refuses a key NO run carries". Pass the same slice twice to check against the survivors
/// deliberately; nothing in this tree does.
pub(crate) fn sort_rows(
    runs: &mut [&ScannedRun],
    known: &[ScannedRun],
    spec: &str,
) -> Result<(), CliError> {
    let (key, explicit) = match spec.split_once(':') {
        Some((k, "asc")) => (k, Some(true)),
        Some((k, "desc")) => (k, Some(false)),
        Some((_, other)) => {
            return Err(CliError::usage(format!(
                "--sort direction '{other}' is not one of: asc | desc"
            )));
        }
        None => (spec, None),
    };
    // The TYPO check, over the whole store. An EMPTY store carries no field and is not a typo, so it
    // is excused before the fold rather than by it.
    if !known.is_empty()
        && known
            .iter()
            .all(|r| matches!(field_of(r, key).unwrap_or(FieldValue::Missing), FieldValue::Missing))
    {
        return Err(CliError::usage(format!(
            "--sort '{key}': no run carries that field. Manifest fields are run_id, kind, \
             produced_by, started_at, finished_at, git_sha, config.path, config.name, report and \
             detail.<path>; any other name is read as a top-level key of report.json."
        )));
    }
    // The DIRECTION default reads the rows actually being ordered: a key that is numeric in the
    // store and absent from the survivors has nothing to order, and a survivor set whose values are
    // all text sorts as text whatever the rest of the store holds.
    let values: Vec<FieldValue> =
        runs.iter().map(|r| field_of(r, key).unwrap_or(FieldValue::Missing)).collect();
    let numeric = values.iter().any(|v| matches!(v, FieldValue::Num(_)));
    let ascending = explicit.unwrap_or(!numeric);
    runs.sort_by(|a, b| {
        let (x, y) = (
            field_of(a, key).unwrap_or(FieldValue::Missing),
            field_of(b, key).unwrap_or(FieldValue::Missing),
        );
        let ord = match (&x, &y) {
            (FieldValue::Missing, FieldValue::Missing) => std::cmp::Ordering::Equal,
            // Missing always LAST, whichever direction: returned before the reverse below.
            (FieldValue::Missing, _) => return std::cmp::Ordering::Greater,
            (_, FieldValue::Missing) => return std::cmp::Ordering::Less,
            (FieldValue::Num(p), FieldValue::Num(q)) => p.total_cmp(q),
            _ => x.render().cmp(&y.render()),
        };
        if ascending { ord } else { ord.reverse() }
    });
    Ok(())
}

/// The pure row mapper — one row per run, one cell per named column.
pub(crate) fn rows_of(runs: &[&ScannedRun], cols: &[String]) -> Vec<Vec<String>> {
    runs.iter()
        .map(|r| {
            cols.iter().map(|c| field_of(r, c).unwrap_or(FieldValue::Missing).render()).collect()
        })
        .collect()
}

/// The `--json` document, keyed by the SAME names the table's headers use.
pub(crate) fn ls_json(runs: &[&ScannedRun], cols: &[String], root: &Path) -> String {
    let rows: Vec<serde_json::Value> = runs
        .iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for c in cols {
                obj.insert(c.clone(), field_of(r, c).unwrap_or(FieldValue::Missing).to_json());
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    let doc = serde_json::json!({
        "root": root.display().to_string(),
        "count": rows.len(),
        "runs": rows,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// A plain column-width table: header, then one line per run. No box drawing — this is piped into
/// `grep` far more often than it is read whole.
fn table(runs: &[&ScannedRun], cols: &[String]) -> String {
    let rows = rows_of(runs, cols);
    let mut widths: Vec<usize> = cols.iter().map(|c| c.chars().count()).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths[i].saturating_sub(c.chars().count());
                if i + 1 == cells.len() { c.clone() } else { format!("{c}{}", " ".repeat(pad)) }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let mut out = line(cols);
    for row in &rows {
        out.push('\n');
        out.push_str(&line(row));
    }
    out
}

/// The empty-state note. An empty list is an ANSWER — rung `0` — and it names the TREE, which is
/// what an operator needs the moment `VIKE_USER_DATA_DIR` is in play. The precedent is
/// `crate::cmd::data`'s own empty note.
fn empty_note(scan: &RunScan, selector: Option<&str>) -> String {
    match selector {
        Some(s) => format!(
            "no runs matching '{s}' under {} ({} run(s) there).",
            scan.root.display(),
            scan.runs.len()
        ),
        None => format!(
            "no runs under {} — nothing has been run in this project yet.",
            scan.root.display()
        ),
    }
}

pub(crate) fn run_ls(ctx: &Ctx<'_>, a: &ReadArgs) -> CmdResult<()> {
    let root = ctx.runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory to \
             list. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = scan_runs(root);
    for problem in &scan.problems {
        eprintln!("vike-cli backtest ls: {problem}");
    }
    let mut runs = resolve_many(&scan, ctx.marks_root, a.selector.as_deref())?;
    if let Some(expr) = &a.where_expr {
        runs = crate::cmd::runs::where_expr::filter(runs, expr)?;
    }
    if let Some(spec) = &a.sort {
        sort_rows(&mut runs, &scan.runs, spec)?;
    }
    if let Some(n) = &a.limit {
        let n: usize = n
            .parse()
            .map_err(|_| CliError::usage(format!("--limit '{n}' is not a whole number of rows")))?;
        runs.truncate(n);
    }
    let cols: Vec<String> = match &a.cols {
        Some(list) => {
            list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        }
        None => DEFAULT_COLS.iter().map(|s| (*s).to_string()).collect(),
    };
    if cols.is_empty() {
        return Err(CliError::usage("--cols was given no column names".to_string()));
    }

    let text = if a.json {
        ls_json(&runs, &cols, &scan.root)
    } else if runs.is_empty() {
        empty_note(&scan, a.selector.as_deref())
    } else {
        table(&runs, &cols)
    };
    match &a.out {
        Some(file) => std::fs::write(file, format!("{text}\n"))
            .map_err(|e| CliError::failed(format!("cannot write {file}: {e}")))?,
        None => println!("{text}"),
    }
    Ok(())
}

/// Give a fixture run a real `report.json` on disk, so the LAZY read has something to open.
/// Outside `mod tests` for the reason `crate::cmd::runs::selector`'s `test_run_at` states: a
/// `pub(crate) mod tests {` is invisible to the gates' inline-test-module locator.
#[cfg(test)]
pub(crate) fn test_with_report(mut r: ScannedRun, json: &str, dir: &std::path::Path) -> ScannedRun {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("report.json"), json).unwrap();
    r.dir = dir.to_path_buf();
    r.report = Some(dir.join("report.json"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::runs::selector::test_run_at as run_at;

    use super::test_with_report as with_report;

    #[test]
    fn the_default_columns_are_manifest_only_and_open_no_report() {
        assert_eq!(DEFAULT_COLS, ["run_id", "kind", "started_at", "report"]);
    }

    #[test]
    fn a_manifest_field_resolves_without_touching_the_report() {
        let r = run_at("1789213143-8821-00", "backtest");
        assert!(matches!(field_of(&r, "kind"), Some(FieldValue::Text(k)) if k == "backtest"));
        assert!(
            matches!(field_of(&r, "run_id"), Some(FieldValue::Text(k)) if k == "1789213143-8821-00")
        );
        assert!(matches!(field_of(&r, "report"), Some(FieldValue::Text(v)) if v == "no"));
        // A `detail.` path reaches into the kind-specific subtree without a per-kind parser.
        assert!(
            matches!(field_of(&r, "detail.strategy"), Some(FieldValue::Text(s)) if s == "sma_cross")
        );
        assert!(matches!(field_of(&r, "detail.nope"), Some(FieldValue::Missing) | None));
    }

    #[test]
    fn a_bare_name_falls_through_to_a_top_level_report_key() {
        let tmp = tempfile::tempdir().unwrap();
        let r = with_report(
            run_at("1789213143-8821-00", "backtest"),
            r#"{"sharpe":1.25,"n_trades":7,"profit_factor":null}"#,
            &tmp.path().join("1789213143-8821-00"),
        );
        assert!(
            matches!(field_of(&r, "sharpe"), Some(FieldValue::Num(v)) if (v - 1.25).abs() < 1e-12)
        );
        assert!(
            matches!(field_of(&r, "n_trades"), Some(FieldValue::Num(v)) if (v - 7.0).abs() < 1e-12)
        );
        // ⚠ `profit_factor` is `null` when non-finite — a reader must tolerate it as MISSING rather
        // than parsing it as zero, which would rank a broken run above a good one.
        assert!(matches!(field_of(&r, "profit_factor"), Some(FieldValue::Missing)));
        // ...and `zero_trade` is `skip_serializing_if`, so it is usually absent outright.
        assert!(matches!(field_of(&r, "zero_trade"), Some(FieldValue::Missing) | None));
    }

    #[test]
    fn rows_render_a_dash_for_a_field_a_run_does_not_have() {
        let r = run_at("1789213143-8821-00", "backtest");
        let cols = vec!["run_id".to_string(), "sharpe".to_string()];
        let rows = rows_of(&[&r], &cols);
        assert_eq!(rows[0][0], "1789213143-8821-00");
        assert_eq!(rows[0][1], "-", "a run with no report renders a dash, never an empty cell");
    }

    /// Numbers sort NUMERICALLY and text sorts lexically, and the default direction is DESCENDING
    /// for a number — a leaderboard's whole point is the best row first.
    #[test]
    fn sorting_is_numeric_for_numbers_and_descending_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let store = vec![
            with_report(run_at("a-1-0", "backtest"), r#"{"sharpe":0.9}"#, &tmp.path().join("a")),
            with_report(run_at("b-1-0", "backtest"), r#"{"sharpe":10.0}"#, &tmp.path().join("b")),
        ];
        let mut v: Vec<&ScannedRun> = store.iter().collect();
        sort_rows(&mut v, &store, "sharpe").unwrap();
        assert_eq!(v[0].run_id, "b-1-0", "10.0 outranks 0.9 — not a string sort");
    }

    /// An explicit ascending spelling, so `--sort max_dd:asc` is expressible.
    #[test]
    fn a_sort_key_may_name_its_direction() {
        let tmp = tempfile::tempdir().unwrap();
        let store = vec![
            with_report(run_at("a-1-0", "backtest"), r#"{"sharpe":0.9}"#, &tmp.path().join("a")),
            with_report(run_at("b-1-0", "backtest"), r#"{"sharpe":10.0}"#, &tmp.path().join("b")),
        ];
        let mut v: Vec<&ScannedRun> = store.iter().collect();
        sort_rows(&mut v, &store, "sharpe:asc").unwrap();
        assert_eq!(v[0].run_id, "a-1-0");
    }

    /// A sort key NO run carries is a typo, and a typo must be an error rather than a no-op ordering
    /// the operator will read as meaningful.
    #[test]
    fn a_sort_key_no_run_carries_is_a_usage_refusal() {
        let store = vec![run_at("a-1-0", "backtest")];
        let mut v: Vec<&ScannedRun> = store.iter().collect();
        let e = sort_rows(&mut v, &store, "sharp").expect_err("typo");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(e.msg.contains("sharp"), "{}", e.msg);
    }

    /// ⚠ **An EMPTY store is not a typo.** `all(Missing)` is VACUOUSLY TRUE over an empty slice, so
    /// the check that catches a misspelled metric used to fire on a fresh project too:
    /// `ls --sort sharpe` exited `2` saying "no run carries that field" when the answer was simply
    /// that there are no runs. That contradicts the rung this family declares for an empty listing.
    #[test]
    fn an_empty_store_sorts_without_accusing_the_operator_of_a_typo() {
        let store: Vec<ScannedRun> = Vec::new();
        let mut v: Vec<&ScannedRun> = Vec::new();
        sort_rows(&mut v, &store, "sharpe").expect("an empty store is an ANSWER, not a typo");
        sort_rows(&mut v, &store, "anything-at-all").expect("…and so is any key over it");
    }

    /// ⚠ ...and the same hole one step along: a `--where` that legitimately matched NOTHING leaves
    /// an empty survivor set over a NON-empty store. The key is validated against the STORE, so a
    /// real key still sorts and a typo is still caught, whatever the filter left behind.
    #[test]
    fn a_filter_that_matched_nothing_still_sorts_on_a_key_the_store_carries() {
        let tmp = tempfile::tempdir().unwrap();
        let store = vec![with_report(
            run_at("a-1-0", "backtest"),
            r#"{"sharpe":0.9}"#,
            &tmp.path().join("a"),
        )];
        let mut survivors: Vec<&ScannedRun> = Vec::new();

        sort_rows(&mut survivors, &store, "sharpe")
            .expect("the STORE carries `sharpe`; the filter simply kept no row");
        // ...and the typo is still refused over the same empty survivor set, which is the half a
        // check against the survivors could never do.
        let e = sort_rows(&mut survivors, &store, "sharp").expect_err("still a typo");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
    }

    /// The `--json` document is an ARRAY of objects keyed by the SAME field vocabulary the table
    /// uses, so a `--cols` change and a `jq` filter cannot disagree about a name.
    #[test]
    fn the_json_document_is_keyed_by_the_same_field_names_as_the_table() {
        let r = run_at("1789213143-8821-00", "backtest");
        let cols = vec!["run_id".to_string(), "kind".to_string()];
        let doc: serde_json::Value =
            serde_json::from_str(&ls_json(&[&r], &cols, Path::new("/runs"))).unwrap();
        assert_eq!(doc["root"], serde_json::json!("/runs"));
        assert_eq!(doc["runs"][0]["run_id"], serde_json::json!("1789213143-8821-00"));
        assert_eq!(doc["runs"][0]["kind"], serde_json::json!("backtest"));
        assert_eq!(doc["count"], serde_json::json!(1));
    }
}
