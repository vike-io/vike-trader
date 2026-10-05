use super::*;
use vike_analytics::report::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};

/// The factor a 1m slice earns — DERIVED, never written as a number here, so these tests
/// exercise the same function the pane does rather than a second copy of the arithmetic.
fn ppy_1m() -> f64 {
    periods_per_year_for_interval("1m")
}

fn strat(name: &str, code: &str) -> SavedStrategy {
    SavedStrategy::rhai(name, code)
}

/// MIGRATION GATE: an existing `studio_strategies.json` written before native strategies
/// existed has NO `source`/`native`/`params` keys. It must still load, keep its code, and be
/// treated as Rhai — the entire back-compat contract of this file format in one test.
#[test]
fn legacy_file_without_a_source_field_loads_as_rhai() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    // Byte-for-byte the shape `save_strategies` used to emit.
    std::fs::write(
        &path,
        r#"[
  {
    "name": "sma-cross",
    "code": "fn on_bar() {}"
  }
]"#,
    )
    .unwrap();

    let loaded = load_strategies(&path);

    assert_eq!(loaded.len(), 1, "the legacy entry must survive the migration");
    assert_eq!(loaded[0].name, "sma-cross");
    assert_eq!(loaded[0].code, "fn on_bar() {}", "its script must be untouched");
    assert_eq!(loaded[0].source, StrategySource::Rhai, "a missing source key means Rhai");
    assert!(loaded[0].native.is_empty());
    assert!(loaded[0].params.is_empty());
    assert_eq!(
        loaded[0].spec(),
        StrategySpec::rhai("fn on_bar() {}"),
        "and it still RUNS as the Rhai script it always was"
    );
}

/// A native entry round-trips through the file, and its `spec()` carries the typed params —
/// the forward half of the migration.
#[test]
fn native_entry_round_trips_and_specs_with_typed_params() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    let entry = SavedStrategy::native(
        "hold-2",
        "buy_hold",
        vec![("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())],
    );
    save_strategies(&path, std::slice::from_ref(&entry)).unwrap();
    let loaded = load_strategies(&path);
    assert_eq!(loaded, vec![entry]);

    match loaded[0].spec() {
        StrategySpec::Native { name, params } => {
            assert_eq!(name, "buy_hold");
            assert_eq!(params.get("size").and_then(toml::Value::as_integer), Some(2));
            assert_eq!(params.get("symbol").and_then(toml::Value::as_str), Some("BTCUSDT"));
        }
        other => panic!("expected a native spec, got {other:?}"),
    }
}

/// A mixed file (one legacy-shaped Rhai row + one native row) loads both, each with the right
/// source — the realistic post-upgrade file.
#[test]
fn mixed_legacy_and_native_file_loads_both() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    std::fs::write(
        &path,
        r#"[
  { "name": "old", "code": "fn on_bar() {}" },
  { "name": "new", "code": "", "source": "native", "native": "buy_hold",
    "params": [["size", "1"]] }
]"#,
    )
    .unwrap();
    let loaded = load_strategies(&path);
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].source, StrategySource::Rhai);
    assert_eq!(loaded[1].source, StrategySource::Native);
    assert_eq!(loaded[1].native, "buy_hold");
    assert_eq!(loaded[1].params, vec![("size".to_string(), "1".to_string())]);
}

/// The bridge to the file-tree migration: BOTH migratable populations map to their own legacy
/// body, and a native row keeps its param ROWS as text (the migration is what turns them into
/// TOML — see `vike_studio_core::user_strategies`'s `plan_migration`). A native row mapped as
/// Rhai would migrate an empty script under the user's name and lose the params entirely.
#[test]
fn legacy_entry_maps_both_migratable_sources_to_their_own_body() {
    let script = SavedStrategy::rhai("sma-cross", "fn on_bar() {}");
    assert_eq!(
        script.legacy_entry(),
        Some(LegacyEntry {
            name: "sma-cross".to_string(),
            body: LegacyBody::Rhai { code: "fn on_bar() {}".to_string() },
        })
    );

    let rows = vec![("size".to_string(), "2".to_string())];
    let preset = SavedStrategy::native("hold-2", "buy_hold", rows.clone());
    assert_eq!(
        preset.legacy_entry(),
        Some(LegacyEntry {
            name: "hold-2".to_string(),
            body: LegacyBody::Native { native: "buy_hold".to_string(), params: rows },
        })
    );
}

/// The THIRD population has no shape in the file-tree migration (see `legacy_entry`'s own
/// doc) — `None` is a caller-visible signal that this row's JSON copy must be PRESERVED, not
/// a mis-mapping into Rhai or Native and not a "this row is handled, move on" skip. Pinned by
/// name so a future reader who only sees the test list still gets the obligation, not just
/// the return value.
#[test]
fn legacy_entry_is_none_for_a_plugin_row_because_the_migration_has_no_shape_for_it_yet() {
    let plugin = SavedStrategy::plugin("my_strat", "my_strat", Some("a".repeat(64)));
    assert_eq!(
        plugin.legacy_entry(),
        None,
        "a Plugin row must never produce a LegacyBody — there is no shape for it in the \
             file-tree migration, and a caller seeing None here must leave the row in \
             studio_strategies.json rather than treat it as migrated or droppable"
    );
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    let list = vec![strat("sma-cross", "fn on_bar() {}"), strat("rsi-mean-revert", "let x = 1;")];

    save_strategies(&path, &list).unwrap();
    let loaded = load_strategies(&path);

    assert_eq!(loaded, list);
}

#[test]
fn load_of_a_missing_path_is_an_empty_vec_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does_not_exist.json");
    assert_eq!(load_strategies(&path), Vec::new());
}

#[test]
fn load_of_a_corrupt_file_is_an_empty_vec_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.json");
    std::fs::write(&path, "{ not: valid json ]]]").unwrap();
    assert_eq!(load_strategies(&path), Vec::new());
}

#[test]
fn save_overwrites_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    save_strategies(&path, &[strat("a", "1")]).unwrap();
    save_strategies(&path, &[strat("b", "2")]).unwrap();
    assert_eq!(load_strategies(&path), vec![strat("b", "2")]);
}

fn result(equity_curve: Vec<f64>, n_trades: usize) -> BacktestResult {
    let final_equity = *equity_curve.last().unwrap();
    BacktestResult { equity_curve, final_equity, n_trades, ..Default::default() }
}

#[test]
fn comparison_rows_sorts_by_sharpe_descending_and_carries_fields() {
    // a strongly uptrending curve (high sharpe), a flat/noisy one (low/negative sharpe).
    let good = result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4);
    let bad = result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9);
    let results =
        vec![("bad".to_string(), Ok(bad.clone())), ("good".to_string(), Ok(good.clone()))];

    let rows = comparison_rows(&results, ppy_1m(), |_| StrategySource::Rhai);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "good", "higher sharpe should rank first");
    assert_eq!(rows[1].name, "bad");
    assert!(rows[0].sharpe > rows[1].sharpe);
    assert_eq!(rows[0].final_equity, *good.equity_curve.last().unwrap());
    assert_eq!(rows[0].n_trades, 4);
    assert_eq!(rows[1].n_trades, 9);
    assert!(rows[0].error.is_none());
    assert!(rows[1].error.is_none());
}

#[test]
fn comparison_rows_puts_nan_sharpe_last() {
    // an equity curve with a NaN point (e.g. a div-by-zero upstream) makes every return NaN,
    // so `sharpe` (metrics.rs) itself comes back NaN — a flat curve is NOT this case; `sharpe`
    // explicitly guards zero variance to 0.0, not NaN (see metrics.rs's own doc).
    let broken = result(vec![100.0, f64::NAN, 100.0], 0);
    let trending = result(vec![100.0, 102.0, 104.5, 107.0], 2);
    let results = vec![("broken".to_string(), Ok(broken)), ("trending".to_string(), Ok(trending))];

    let rows = comparison_rows(&results, ppy_1m(), |_| StrategySource::Rhai);

    assert_eq!(rows[0].name, "trending");
    assert_eq!(rows[1].name, "broken");
    assert!(rows[1].sharpe.is_nan());
}

#[test]
fn comparison_rows_on_empty_input_is_empty() {
    assert!(comparison_rows(&[], ppy_1m(), |_| StrategySource::Rhai).is_empty());
}

/// The compare table's half of the annualization fix, in two claims that must BOTH hold.
///
/// The values MOVE with the factor — a 1m comparison is on a `sqrt(1440) ≈ 37.9x` larger scale
/// than the daily one this function used to hard-code, which is the whole defect — and the
/// ORDER does not, because a positive constant multiplier cannot reorder anything. Without the
/// second half, a future "simplification" back to a fixed constant would look harmless: the
/// table would still rank correctly while every number in it disagreed with the Performance
/// tab beside it.
#[test]
fn the_ranking_is_identical_on_any_factor_while_the_values_move() {
    let good = result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4);
    let bad = result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9);
    let results = vec![("bad".to_string(), Ok(bad)), ("good".to_string(), Ok(good))];

    let daily = comparison_rows(&results, DAILY_PERIODS_PER_YEAR, |_| StrategySource::Rhai);
    let minute = comparison_rows(&results, ppy_1m(), |_| StrategySource::Rhai);

    let daily_names: Vec<&str> = daily.iter().map(|r| r.name.as_str()).collect();
    let minute_names: Vec<&str> = minute.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(daily_names, minute_names, "the factor must not reorder the table");

    let scale = (ppy_1m() / DAILY_PERIODS_PER_YEAR).sqrt();
    for (d, m) in daily.iter().zip(&minute) {
        let name = &d.name;
        let ratio = m.sharpe / d.sharpe;
        assert!((ratio - scale).abs() < 1e-9, "{name}: Sharpe scaled {ratio}, want {scale}");
        // ...and nothing else in the row is annualized, so nothing else may move.
        assert_eq!(d.final_equity, m.final_equity);
        assert_eq!(d.max_dd, m.max_dd);
        assert_eq!(d.n_trades, m.n_trades);
    }
}

/// Hand-verified against `metrics::max_drawdown`'s own contract (running-peak fractional
/// decline, NOT a negative number): peak hits 120 after the second point, then the deepest
/// dip to 90 is `(120-90)/120 == 0.25`; the later partial recovery to 110 is a smaller
/// drawdown (`(120-110)/120 ≈ 0.083`) so 0.25 stays the worst.
#[test]
fn comparison_rows_computes_max_dd_correctly() {
    let r = result(vec![100.0, 120.0, 90.0, 110.0], 3);
    let results = vec![("dd-check".to_string(), Ok(r))];

    let rows = comparison_rows(&results, ppy_1m(), |_| StrategySource::Rhai);

    assert_eq!(rows.len(), 1);
    assert!((rows[0].max_dd - 0.25).abs() < 1e-12, "max_dd = {}", rows[0].max_dd);
}

/// A successful row carries the full equity curve (for the sparkline); a failed row carries
/// none.
#[test]
fn comparison_rows_carries_the_equity_curve_for_successful_rows() {
    let curve = vec![100.0, 101.0, 99.0, 103.0];
    let r = result(curve.clone(), 1);
    let results = vec![("ok".to_string(), Ok(r)), ("broken".to_string(), Err("boom".to_string()))];

    let rows = comparison_rows(&results, ppy_1m(), |_| StrategySource::Rhai);

    let ok_row = rows.iter().find(|r| r.name == "ok").unwrap();
    assert_eq!(ok_row.equity, curve);
    let failed_row = rows.iter().find(|r| r.name == "broken").unwrap();
    assert!(failed_row.equity.is_empty());
}

/// A strategy that failed to compile/run is surfaced as a row with `error: Some(..)` rather
/// than silently dropped, and sorts after every successful row regardless of the successful
/// rows' Sharpe ranking.
#[test]
fn comparison_rows_surfaces_a_failed_strategy_and_sorts_it_last() {
    let good = result(vec![100.0, 105.0, 110.0, 116.0], 2);
    let results = vec![
        ("broken".to_string(), Err("compile error: unexpected token".to_string())),
        ("good".to_string(), Ok(good)),
    ];

    let rows = comparison_rows(&results, ppy_1m(), |_| StrategySource::Rhai);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "good", "the successful row ranks first");
    assert!(rows[0].error.is_none());
    assert_eq!(rows[1].name, "broken");
    assert_eq!(rows[1].error.as_deref(), Some("compile error: unexpected token"));
}
