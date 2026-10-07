use super::*;

/// A minimal script that compiles and defines the one hook the engine calls.
const OK_SCRIPT: &str = "fn on_bar() {}\n";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Build `<root>/<name>/<name>.rhai` with `src`.
fn strategy_dir(root: &Path, name: &str, src: &str) -> PathBuf {
    let dir = root.join(name);
    write(&dir.join(format!("{name}.rhai")), src);
    dir
}

/// The happy path, INCLUDING presets in BOTH honoured positions — flat beside the entry and
/// filed under `presets/`. Both are the strategy's, so both load, in one name-ordered list.
#[test]
fn a_folder_loads_with_its_source_and_presets_from_both_positions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
    write(&dir.join("fast.toml"), "fast = 5\nslow = 20\n");
    write(&dir.join("presets").join("slow.toml"), "fast = 50\nslow = 200\n");

    let report = load_rhai_strategies(root);

    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    assert_eq!(report.strategies.len(), 1);
    let s = &report.strategies[0];
    assert_eq!(s.name, "sma-cross");
    assert_eq!(s.source(), Some(OK_SCRIPT), "the entry file's text must be verbatim");
    assert_eq!(s.entry(), Some(dir.join("sma-cross.rhai").as_path()));
    assert_eq!(
        s.presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        vec!["fast", "slow"],
        "both positions are honoured, in name order"
    );
    assert_eq!(s.presets[0].params.get("fast").and_then(toml::Value::as_integer), Some(5));
    assert_eq!(s.presets[1].params.get("slow").and_then(toml::Value::as_integer), Some(200));
    assert_eq!(s.spec(), StrategySpec::Rhai(OK_SCRIPT.to_string()));
}

/// THE naming rule: the entry file is the one matching the folder. A folder whose script is
/// named anything else is NOT loaded — and the diagnostic lists what it found, so the user can
/// see the mismatch without opening the folder.
#[test]
fn an_entry_file_that_does_not_match_its_folder_is_named_not_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("sma-cross").join("strat.rhai"), OK_SCRIPT);

    let report = load_rhai_strategies(root);

    assert!(report.strategies.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    match &report.diagnostics[0] {
        LoadDiagnostic::MissingEntry { name, scripts, .. } => {
            assert_eq!(name, "sma-cross");
            assert_eq!(scripts, &vec!["strat.rhai".to_string()]);
        }
        other => panic!("expected MissingEntry, got {other:?}"),
    }
    let msg = report.diagnostics[0].to_string();
    assert!(msg.contains("sma-cross.rhai"), "the message must name the expected path: {msg}");
    assert!(msg.contains("strat.rhai"), "…and what was found instead: {msg}");
    assert_eq!(report.diagnostics[0].severity(), Severity::Error);
}

/// A folder holding the entry file PLUS other scripts is fine — the extras are the script's
/// own helpers. This is the property the folder/entry naming rule exists to buy, so it is
/// pinned: extra scripts must produce NO diagnostic.
#[test]
fn extra_scripts_beside_the_entry_are_not_a_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
    write(&dir.join("helpers.rhai"), "fn helper() {}\n");

    let report = load_rhai_strategies(root);

    assert_eq!(report.strategies.len(), 1);
    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
}

/// A script that does not compile is reported with the SCRIPT AUTHOR'S error, not a generic
/// "failed to load" — the message is the whole value of the diagnostic.
#[test]
fn a_script_that_fails_to_compile_names_the_folder_and_the_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    strategy_dir(root, "broken", "fn on_bar( {\n");

    let report = load_rhai_strategies(root);

    assert!(report.strategies.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    match &report.diagnostics[0] {
        LoadDiagnostic::CompileFailed { name, error, .. } => {
            assert_eq!(name, "broken");
            assert!(!error.is_empty(), "the rhai error must be carried through");
        }
        other => panic!("expected CompileFailed, got {other:?}"),
    }
}

/// Two folders differing only by case: legal on Linux, impossible on Windows/macOS. One wins
/// DETERMINISTICALLY (name order) and the other is named — a tree that quietly loads a
/// different strategy depending on the machine is the failure this catches.
#[test]
fn two_folders_differing_only_by_case_are_reported_as_a_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    strategy_dir(root, "SmaCross", OK_SCRIPT);
    let second = root.join("smacross");
    // On a case-folding filesystem the two are ONE directory and the second `create_dir_all`
    // is a no-op — in which case there is no duplicate to find and this test has nothing to
    // assert. Skip rather than assert a platform property.
    std::fs::create_dir_all(&second).unwrap();
    if !second.join("smacross.rhai").exists() {
        std::fs::write(second.join("smacross.rhai"), OK_SCRIPT).unwrap();
    }
    if root.join("SmaCross").join("smacross.rhai").exists() {
        return; // case-folding filesystem: the two folders are the same directory
    }

    let report = load_rhai_strategies(root);

    assert_eq!(report.strategies.len(), 1, "exactly one of the two loads");
    assert_eq!(report.strategies[0].name, "SmaCross", "name order decides, not directory order");
    let dup = report
        .diagnostics
        .iter()
        .find(|d| matches!(d, LoadDiagnostic::DuplicateName { .. }))
        .expect("the loser must be reported");
    assert_eq!(dup.severity(), Severity::Error);
    assert!(dup.to_string().contains("rename"), "the message must state the fix");
}

/// The first-time mistake: a script dropped straight into `strategies/rhai/`. Reported, with
/// the path it should have — never silently ignored.
#[test]
fn a_script_loose_in_the_root_is_reported_with_where_it_belongs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mystrat.rhai"), OK_SCRIPT);

    let report = load_rhai_strategies(root);

    assert!(report.strategies.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert!(matches!(report.diagnostics[0], LoadDiagnostic::StrayScript { .. }));
    let msg = report.diagnostics[0].to_string();
    assert!(
        msg.contains(&format!("mystrat{}mystrat.rhai", std::path::MAIN_SEPARATOR)),
        "the message must show the target path: {msg}"
    );
}

/// A broken preset is a WARNING: the strategy still loads, with its other presets. Losing a
/// whole strategy over one bad params file would be the wrong trade.
#[test]
fn a_malformed_preset_is_a_warning_and_the_strategy_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
    write(&dir.join("good.toml"), "fast = 5\n");
    write(&dir.join("broken.toml"), "fast = = 5\n");

    let report = load_rhai_strategies(root);

    assert_eq!(report.strategies.len(), 1, "the strategy must survive its broken preset");
    assert_eq!(report.strategies[0].presets.len(), 1);
    assert_eq!(report.strategies[0].presets[0].name, "good");
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].severity(), Severity::Warning);
    assert!(matches!(report.diagnostics[0], LoadDiagnostic::BadPreset { .. }));
}

/// The same preset name in BOTH positions: the flat one wins (the documented primary
/// position) and the shadowed one is reported rather than silently dropped.
#[test]
fn a_preset_defined_in_both_positions_keeps_the_flat_one_and_reports_the_other() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
    write(&dir.join("fast.toml"), "fast = 5\n");
    write(&dir.join("presets").join("fast.toml"), "fast = 999\n");

    let report = load_rhai_strategies(root);

    let s = &report.strategies[0];
    assert_eq!(s.presets.len(), 1);
    assert_eq!(
        s.presets[0].params.get("fast").and_then(toml::Value::as_integer),
        Some(5),
        "the flat position wins"
    );
    match &report.diagnostics[..] {
        [LoadDiagnostic::ShadowedPreset { preset, .. }] => assert_eq!(preset, "fast"),
        other => panic!("expected one ShadowedPreset, got {other:?}"),
    }
    assert_eq!(report.diagnostics[0].severity(), Severity::Warning);
}

/// An ABSENT root is the ordinary state of a fresh install: an empty report and NO diagnostic.
/// The opposite ruling would greet every new user with an error about a directory they have
/// never heard of.
#[test]
fn a_missing_root_is_an_empty_report_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("user_data").join("strategies").join("rhai");
    assert!(!root.exists(), "precondition");

    let report = load_rhai_strategies(&root);

    assert!(report.strategies.is_empty());
    assert!(report.diagnostics.is_empty(), "absence is not a failure");

    // …and the same for the both-trees scan, whose `strategies/` may not exist either.
    let both = load_user_strategies(&tmp.path().join("user_data").join("strategies"));
    assert!(both.strategies.is_empty());
    assert!(both.diagnostics.is_empty());
}

/// Dot-entries are tool droppings, not user content.
#[test]
fn dot_entries_are_skipped_without_a_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    strategy_dir(root, "sma-cross", OK_SCRIPT);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(&root.join(".DS_Store"), "junk");

    let report = load_rhai_strategies(root);

    assert_eq!(report.strategies.len(), 1);
    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
}

/// The log records PASSES as well as failures — the directory's own contract — and every line
/// carries the caller's stamp so one appended file stays greppable.
#[test]
fn the_compile_log_names_every_pass_and_every_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
    write(&dir.join("fast.toml"), "fast = 5\n");
    strategy_dir(root, "broken", "fn on_bar( {\n");

    let report = load_rhai_strategies(root);
    let log = render_compile_log(&report, "2026-08-06T10:00:00Z");

    assert!(log.contains("ok    sma-cross"), "a pass must be logged: {log}");
    assert!(log.contains("1 preset (fast)"), "…with its presets: {log}");
    assert!(log.contains("ERROR broken:"), "a failure must be logged: {log}");
    assert!(log.contains("1 loaded, 1 error(s), 0 warning(s)"), "summary: {log}");
    assert!(log.ends_with('\n'), "an appended log must not splice runs onto one line");
    for line in log.lines() {
        assert!(line.starts_with("2026-08-06T10:00:00Z "), "every line is stamped: {line}");
    }
}

// ---- the `rust/` tree: presets for strategies whose code is in the BINARY ------------------

/// Build `<strategies>/rust/<name>/<preset>.toml`.
fn native_preset(strategies: &Path, name: &str, preset: &str, body: &str) -> PathBuf {
    let dir = strategies.join("rust").join(name);
    write(&dir.join(format!("{preset}.toml")), body);
    dir
}

/// ⚠ THE regression this module's third decision exists for: a folder named after a BUILT-IN
/// strategy, holding presets and NO entry file, is legitimate — the code is in the binary. It
/// must load, with its presets, and produce NO diagnostic. Before this it was reported as
/// `MissingEntry` at ERROR severity, advising the user to rename a script that must not exist.
#[test]
fn a_preset_only_folder_for_a_builtin_strategy_loads_with_no_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    native_preset(strategies, "buy_hold", "aggressive", "size = 3\nsymbol = \"BTCUSDT\"\n");

    let report = load_user_strategies(strategies);

    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    assert_eq!(report.strategies.len(), 1);
    let s = &report.strategies[0];
    assert_eq!(s.name, "buy_hold");
    assert!(s.is_native(), "the code is in the binary");
    assert_eq!(s.entry(), None, "there is no entry file to find");
    assert_eq!(s.source(), None);
    assert_eq!(s.spec(), StrategySpec::native_default("buy_hold"));
    assert_eq!(s.presets.len(), 1);
    assert_eq!(s.presets[0].name, "aggressive");
}

/// The shipped `my_experiment/` template: a `rust/` folder that is NOT a registry name and
/// holds no presets. Nothing was lost (its `.rs` is a cargo input, consumed long before this
/// scan), so it must produce no strategy AND no diagnostic — an error on the file `vike-cli
/// init` itself writes would be the worst possible first impression.
#[test]
fn an_unregistered_rust_folder_without_presets_is_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    write(
        &strategies.join("rust").join("my_experiment").join("my_experiment.rs"),
        "// a strategy cargo compiles\n",
    );
    write(&strategies.join("rust").join("README.md"), "# Rust strategies\n");

    let report = load_user_strategies(strategies);

    assert!(report.strategies.is_empty());
    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
}

/// …but the moment that same folder holds PRESETS, they are addressed by a name that resolves
/// to nothing, and THAT is a loss worth naming — with both fixes in the message.
#[test]
fn an_unregistered_rust_folder_with_presets_is_reported_with_both_fixes() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    native_preset(strategies, "my_experiment", "tuned", "size = 2\n");

    let report = load_user_strategies(strategies);

    assert!(report.strategies.is_empty());
    match &report.diagnostics[..] {
        [LoadDiagnostic::UnregisteredNative { name, presets, .. }] => {
            assert_eq!(name, "my_experiment");
            assert_eq!(presets, &vec!["tuned".to_string()]);
        }
        other => panic!("expected one UnregisteredNative, got {other:?}"),
    }
    let msg = report.diagnostics[0].to_string();
    assert!(msg.contains("tuned"), "names the presets that reach nothing: {msg}");
    assert!(msg.contains("strategy_by_name"), "names where to register it: {msg}");
    assert_eq!(report.diagnostics[0].severity(), Severity::Error);
}

/// A registry-named preset folder filed under `rhai/` gets the RIGHT advice: move it, not
/// "rename the script". The old `MissingEntry` wording told the user to rename a script that
/// should not exist in the first place.
#[test]
fn builtin_presets_misfiled_under_rhai_name_the_folder_they_belong_in() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    let dir = strategies.join("rhai").join("buy_hold");
    write(&dir.join("aggressive.toml"), "size = 3\n");

    let report = load_user_strategies(strategies);

    assert!(report.strategies.is_empty());
    match &report.diagnostics[..] {
        [LoadDiagnostic::NativePresetsMisfiled { name, expected_dir, .. }] => {
            assert_eq!(name, "buy_hold");
            assert_eq!(*expected_dir, strategies.join("rust").join("buy_hold"));
        }
        other => panic!("expected one NativePresetsMisfiled, got {other:?}"),
    }
    let msg = report.diagnostics[0].to_string();
    assert!(msg.contains("BUILT-IN"), "must say why there is no script: {msg}");
    assert!(!msg.contains("Rename the script"), "the wrong advice must be gone: {msg}");
}

/// A folder named after a built-in but holding NOTHING is more likely a Rhai strategy someone
/// just started than a misfiled preset folder, so it keeps the ordinary `MissingEntry` row.
#[test]
fn an_empty_registry_named_rhai_folder_is_still_a_missing_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    std::fs::create_dir_all(strategies.join("rhai").join("grid")).unwrap();

    let report = load_user_strategies(strategies);

    assert!(matches!(report.diagnostics[..], [LoadDiagnostic::MissingEntry { .. }]));
}

/// The folder's CASE does not decide which strategy it presets — the registry's spelling is
/// what `strategy_by_name` matches, so that is the name the loaded strategy carries.
#[test]
fn a_case_folded_folder_still_names_the_registry_strategy() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    native_preset(strategies, "Buy_Hold", "aggressive", "size = 3\n");

    let report = load_user_strategies(strategies);

    assert_eq!(report.strategies.len(), 1);
    assert_eq!(report.strategies[0].name, "buy_hold", "the REGISTRY's spelling");
    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
}

/// One namespace across both trees: a strategy is addressed by NAME, so `rhai/grid/` and
/// `rust/grid/` are a genuine collision. The `rhai` one is scanned first and wins; the other is
/// NAMED rather than silently shadowed.
#[test]
fn a_name_claimed_in_both_trees_is_a_duplicate_not_a_silent_shadow() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    strategy_dir(&strategies.join("rhai"), "grid", OK_SCRIPT);
    native_preset(strategies, "grid", "tight", "step = 1.0\n");

    let report = load_user_strategies(strategies);

    assert_eq!(report.strategies.len(), 1);
    assert!(!report.strategies[0].is_native(), "the rhai tree is scanned first and wins");
    match &report.diagnostics[..] {
        [LoadDiagnostic::DuplicateName { name, .. }] => assert_eq!(name, "grid"),
        other => panic!("expected one DuplicateName, got {other:?}"),
    }
}

/// A folder that loaded NOTHING must not claim its name: the `rust/` half here is a real
/// strategy and has to load even though a broken `rhai/` folder of the same name was scanned
/// first.
#[test]
fn a_failed_folder_does_not_claim_the_name_for_a_good_one() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    // a `rhai/buy_hold/` with a script that does not compile
    strategy_dir(&strategies.join("rhai"), "buy_hold", "fn on_bar( {\n");
    native_preset(strategies, "buy_hold", "aggressive", "size = 3\n");

    let report = load_user_strategies(strategies);

    assert_eq!(report.strategies.len(), 1);
    assert!(report.strategies[0].is_native());
    assert!(
        report.diagnostics.iter().all(|d| !matches!(d, LoadDiagnostic::DuplicateName { .. })),
        "a failure is not a claim: {:?}",
        report.diagnostics
    );
}

/// The compile log names a built-in strategy too, and says where its code is.
#[test]
fn the_compile_log_marks_a_builtin_strategy_as_built_in() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    native_preset(strategies, "buy_hold", "aggressive", "size = 3\n");

    let log = render_compile_log(&load_user_strategies(strategies), "2026-08-06T10:00:00Z");

    assert!(log.contains("ok    buy_hold [built-in, 1 preset (aggressive)]"), "{log}");
}

// ---- the preset SHAPE rule -----------------------------------------------------------------

/// ⚠ A preset wrapping its knobs in `[params]` is REFUSED, loudly, naming the fix — it would
/// otherwise merge into `[strategy.params]` as one key nothing reads while every knob silently
/// kept its default.
#[test]
fn a_params_wrapped_preset_is_refused_with_the_fix_in_the_message() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
    write(&dir.join("fast.toml"), "[params]\nfast = 5.0\nslow = 15.0\n");

    let report = load_rhai_strategies(root);

    assert_eq!(report.strategies.len(), 1, "the strategy survives its unusable preset");
    assert!(report.strategies[0].presets.is_empty());
    match &report.diagnostics[..] {
        [LoadDiagnostic::BadPreset { error, .. }] => {
            assert!(error.contains("[params]"), "names the offending header: {error}");
            assert!(error.contains("top level"), "names the fix: {error}");
        }
        other => panic!("expected one BadPreset, got {other:?}"),
    }
    assert_eq!(report.diagnostics[0].severity(), Severity::Warning);
}

/// …but a genuinely NESTED knob is untouched: `funding_carry` reads a `[venues]` table
/// straight out of `[strategy.params]`, so only the exact lone-`params`-table shape is refused.
#[test]
fn a_nested_knob_table_is_a_perfectly_good_preset() {
    let tmp = tempfile::tempdir().unwrap();
    let strategies = tmp.path();
    native_preset(
        strategies,
        "funding_carry",
        "cross",
        "cooldown_ms = 500\n[venues]\nBTCUSDT = \"binance\"\n",
    );

    let report = load_user_strategies(strategies);

    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    let p = &report.strategies[0].presets[0];
    assert!(p.params.get("venues").is_some_and(toml::Value::is_table));
}

/// `src` is the strategy's SOURCE, not a knob — a preset carrying one could smuggle a whole
/// script past `--script`, so it is refused rather than resolved by an invisible precedence.
#[test]
fn a_preset_that_defines_src_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
    write(&dir.join("sneaky.toml"), "src = \"fn on_bar() { buy(99.0); }\"\n");

    let report = load_rhai_strategies(root);

    assert!(report.strategies[0].presets.is_empty());
    match &report.diagnostics[..] {
        [LoadDiagnostic::BadPreset { error, .. }] => {
            assert!(error.contains("src"), "names the offending key: {error}")
        }
        other => panic!("expected one BadPreset, got {other:?}"),
    }
}
