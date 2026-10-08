use super::*;
use crate::user_strategies::load_user_strategies;
use std::path::{Path, PathBuf};
use vike_model::Bar;
use vike_sim::{EngineParams, StrategyEngine};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A `strategies/` tree with one built-in preset folder and one Rhai strategy, which is the
/// shape every test below resolves against.
fn tree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path().join("strategies");
    write(
        &strategies.join("rust").join("buy_hold").join("aggressive.toml"),
        "size = 3\nsymbol = \"BTCUSDT\"\n",
    );
    write(
        &strategies.join("rhai").join("sma_cross").join("sma_cross.rhai"),
        // BOTH knobs the preset below sets are DECLARED here. A preset key the script never
        // reads is an UNREAD key, which `run_with` now reports — so a fixture that set `qty`
        // without declaring it would make every test on this tree lossy and mask the signal
        // the check exists to send.
        "let fast = param(\"fast\", 10.0);\nlet qty = param(\"qty\", 1.0);\nfn on_bar() {}\n",
    );
    write(&strategies.join("rhai").join("sma_cross").join("fast.toml"), "fast = 5\nqty = 2.5\n");
    (tmp, strategies)
}

/// A synthetic rising bar series — enough for `buy_hold` to buy once and for the run's final
/// equity to depend on HOW MUCH it bought.
fn bars(sym: &str) -> Vec<(String, Vec<Bar>)> {
    let series: Vec<Bar> = (0..20)
        .map(|i| {
            let c = 100.0 + i as f64;
            Bar {
                ts: 60_000 * (i as i64 + 1),
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some(sym.to_string()),
            }
        })
        .collect();
    vec![(sym.to_string(), series)]
}

/// THE resolution, for a strategy whose code is in the BINARY: the preset's table becomes the
/// params `strategy_by_name` constructs it with, whole and with its TOML types intact.
#[test]
fn a_preset_for_a_builtin_strategy_becomes_its_construction_params() {
    let (_tmp, strategies) = tree();
    let report = load_user_strategies(&strategies);

    let run = resolve_preset(&report, "buy_hold", "aggressive").expect("resolves");

    match &run.spec {
        StrategySpec::Native { name, params } => {
            assert_eq!(name, "buy_hold");
            assert_eq!(params.get("size").and_then(toml::Value::as_integer), Some(3));
            assert_eq!(
                params.get("symbol").and_then(toml::Value::as_str),
                Some("BTCUSDT"),
                "a STRING param survives on the native path"
            );
        }
        other => panic!("expected Native, got {other:?}"),
    }
    assert!(run.overrides.is_empty(), "native params ride in the spec, not as overrides");
    assert!(run.is_lossless(), "nothing can be dropped on the native path");
    run.build().expect("the resolved preset builds a real strategy");
}

/// …and it is not decorative: the SAME strategy built from the preset behaves differently from
/// the one built without it, because `size = 3` genuinely reached `BuyHold::from_params`.
#[test]
fn the_preset_params_actually_reach_the_strategy() {
    let (_tmp, strategies) = tree();
    let report = load_user_strategies(&strategies);
    let s = report.strategy("buy_hold").expect("loaded");

    let with_preset = s.run_with(s.preset("aggressive").unwrap()).build().unwrap();
    let without = crate::build_strategy(&s.spec()).unwrap();

    let equity = |strat| {
        StrategyEngine::new(bars("BTCUSDT"), strat, EngineParams::default()).run().final_equity
    };
    let preset_equity = equity(with_preset);
    let default_equity = equity(without);
    assert!(
        preset_equity != default_equity,
        "size = 3 must not produce the size = 1 default's equity ({preset_equity} vs \
             {default_equity})"
    );
}

/// A Rhai strategy's preset reaches the script's `param(name, default)` plane as numeric
/// overrides — the same lane a `[sweep]` grid point uses.
#[test]
fn a_preset_for_a_rhai_strategy_becomes_numeric_param_overrides() {
    let (_tmp, strategies) = tree();
    let report = load_user_strategies(&strategies);

    let run = resolve_preset(&report, "sma_cross", "fast").expect("resolves");

    assert!(matches!(run.spec, StrategySpec::Rhai(_)), "the script is the spec");
    assert_eq!(
        run.overrides,
        vec![("fast".to_string(), 5.0), ("qty".to_string(), 2.5)],
        "integers and floats both arrive as f64, in the table's own order"
    );
    assert!(run.is_lossless());
    run.build().expect("the resolved preset compiles the script with its params");
}

/// ⚠ The Rhai type gap is NAMED. A string in a preset for a Rhai script cannot reach a
/// `param()`, and the caller is told which key — `rhai_overrides` would have dropped it in
/// silence.
#[test]
fn a_non_numeric_key_in_a_rhai_preset_is_reported_not_swallowed() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path().join("strategies");
    write(&strategies.join("rhai").join("s").join("s.rhai"), "fn on_bar() {}\n");
    write(
        &strategies.join("rhai").join("s").join("p.toml"),
        "qty = 2.0\nsymbol = \"BTCUSDT\"\nflag = true\n",
    );
    let report = load_user_strategies(&strategies);

    let run = resolve_preset(&report, "s", "p").expect("resolves");

    assert_eq!(run.overrides, vec![("qty".to_string(), 2.0)]);
    assert_eq!(run.dropped, vec!["flag".to_string(), "symbol".to_string()]);
    assert!(!run.is_lossless(), "a caller must be able to warn about this");
}

/// A missing preset names every preset that DOES exist — the difference between a usable
/// failure and `no such preset`.
#[test]
fn a_missing_preset_names_what_was_found() {
    let (_tmp, strategies) = tree();
    let report = load_user_strategies(&strategies);

    let err = resolve_preset(&report, "buy_hold", "conservative").unwrap_err();

    match &err {
        PresetError::UnknownPreset { strategy, preset, found } => {
            assert_eq!(strategy, "buy_hold");
            assert_eq!(preset, "conservative");
            assert_eq!(found, &vec!["aggressive".to_string()]);
        }
        other => panic!("expected UnknownPreset, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("conservative"), "names what was asked for: {msg}");
    assert!(msg.contains("aggressive"), "names what was found: {msg}");
}

/// A strategy with NO presets says so, rather than listing an empty set.
#[test]
fn a_strategy_with_no_presets_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path().join("strategies");
    write(&strategies.join("rhai").join("bare").join("bare.rhai"), "fn on_bar() {}\n");
    let report = load_user_strategies(&strategies);

    let msg = resolve_preset(&report, "bare", "fast").unwrap_err().to_string();

    assert!(msg.contains("no presets at all"), "{msg}");
}

/// An unknown STRATEGY names the ones that loaded — which is also how a user finds out their
/// folder did not load at all.
#[test]
fn an_unknown_strategy_names_the_ones_that_loaded() {
    let (_tmp, strategies) = tree();
    let report = load_user_strategies(&strategies);

    let err = resolve_preset(&report, "nope", "fast").unwrap_err();

    match &err {
        PresetError::UnknownStrategy { known, .. } => {
            assert!(known.contains(&"buy_hold".to_string()));
            assert!(known.contains(&"sma_cross".to_string()));
        }
        other => panic!("expected UnknownStrategy, got {other:?}"),
    }
    assert!(err.to_string().contains("sma_cross"), "{err}");
}

/// The flat position beats `presets/` for the RESOLVED value too, not just in the scan: a
/// caller asking for `fast` gets the flat file's params, and never a merge of the two.
#[test]
fn the_flat_position_wins_over_the_presets_subfolder() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path().join("strategies");
    let dir = strategies.join("rust").join("buy_hold");
    write(&dir.join("tuned.toml"), "size = 3\n");
    write(&dir.join("presets").join("tuned.toml"), "size = 999\n");
    let report = load_user_strategies(&strategies);

    let run = resolve_preset(&report, "buy_hold", "tuned").expect("resolves");

    match &run.spec {
        StrategySpec::Native { params, .. } => {
            assert_eq!(
                params.get("size").and_then(toml::Value::as_integer),
                Some(3),
                "the flat file wins"
            );
            assert!(
                params.as_table().is_some_and(|t| t.len() == 1),
                "…and the shadowed file is not merged in"
            );
        }
        other => panic!("expected Native, got {other:?}"),
    }
    // The loser is still reported by the scan — resolving must not hide it.
    assert_eq!(report.warning_count(), 1, "{:?}", report.diagnostics);
}

/// …and a preset filed ONLY under `presets/` resolves exactly like a flat one.
#[test]
fn a_preset_filed_under_the_subfolder_resolves_too() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path().join("strategies");
    write(
        &strategies.join("rust").join("buy_hold").join("presets").join("filed.toml"),
        "size = 7\n",
    );
    let report = load_user_strategies(&strategies);

    let run = resolve_preset(&report, "buy_hold", "filed").expect("resolves");

    match &run.spec {
        StrategySpec::Native { params, .. } => {
            assert_eq!(params.get("size").and_then(toml::Value::as_integer), Some(7))
        }
        other => panic!("expected Native, got {other:?}"),
    }
}

/// The strategy NAME is matched case-insensitively, like every other name in this tree.
#[test]
fn strategy_and_preset_names_are_matched_case_insensitively() {
    let (_tmp, strategies) = tree();
    let report = load_user_strategies(&strategies);

    assert!(resolve_preset(&report, "BUY_HOLD", "Aggressive").is_ok());
}

/// A typo'd NUMERIC key is reported, not swallowed.
///
/// This is the case `split_numeric` structurally cannot see — `fastt = 5.0` is valid TOML and
/// valid f64, so it reaches `overrides` looking exactly like a real knob. Without the
/// declared-set check the run would use `fast`'s DEFAULT and report success, which is the
/// silent-wrong-configuration outcome the preset mechanism exists to prevent.
#[test]
fn a_typod_numeric_key_is_named_rather_than_silently_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path().join("strategies");
    let sd = strategies.join("rhai").join("t");
    write(
        &sd.join("t.rhai"),
        "let fast = param(\"fast\", 10.0);
fn on_bar() {}
",
    );
    write(
        &sd.join("oops.toml"),
        "fastt = 5.0
",
    );

    let report = load_user_strategies(&strategies);
    let run = resolve_preset(&report, "t", "oops").expect("preset resolves");

    assert!(
        run.dropped.iter().any(|k| k == "fastt"),
        "a key the script never reads must be NAMED; got dropped = {:?}",
        run.dropped
    );
    assert!(!run.is_lossless(), "a run with an unread key is not lossless");
}

/// ...and a correctly-spelled key is NOT reported, so the check above cannot pass vacuously.
#[test]
fn a_key_the_script_declares_is_not_reported_as_unread() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path().join("strategies");
    let sd = strategies.join("rhai").join("t");
    write(
        &sd.join("t.rhai"),
        "let fast = param(\"fast\", 10.0);
fn on_bar() {}
",
    );
    write(
        &sd.join("ok.toml"),
        "fast = 5.0
",
    );

    let report = load_user_strategies(&strategies);
    let run = resolve_preset(&report, "t", "ok").expect("preset resolves");

    assert_eq!(run.dropped, Vec::<String>::new());
    assert!(run.is_lossless());
    assert_eq!(run.overrides, vec![("fast".to_string(), 5.0)]);
}
