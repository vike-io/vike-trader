//! Strategy-file (Native) mode: the spec it resolves, its own dirty chip, restore by path, Open, a
//! run through the Studio's path, and the Saved / sweep-grid / Compare rows it adds.

use super::super::*;
use super::support::{seeded_store, spawn_backtesthub_server, state_new};
use std::sync::Arc;

// ---- native strategies -------------------------------------------------------------------

/// The default source is Rhai (byte-identical to the pre-native Studio), and flipping to
/// Native makes `current_spec` resolve a registry strategy + its typed params instead.
#[test]
fn current_spec_follows_the_strategy_source_toggle() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    assert_eq!(st.strategy_source, StudioStrategySource::Rhai);
    assert_eq!(st.current_spec(), StrategySpec::rhai(st.editor.source.clone()));

    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("size".into(), "2".into())];
    match st.current_spec() {
        StrategySpec::File { file: name, params, .. } => {
            assert_eq!(name, "trend/buy_hold.rs");
            assert_eq!(params.get("size").and_then(|v| v.as_integer()), Some(2));
        }
        other => panic!("expected a native spec, got {other:?}"),
    }
}

/// **The toolbar's SDK pick** (decision 0296): it reaches the file spec a Run sends and the wire, `None` is
/// "as the file says", and a Rhai buffer has none. The dropdown it is picked in:
/// `crates/vike-studio/src/studio/tests/sdk_source.rs`'s
/// `the_toolbar_sdk_pick_offers_the_servers_sdks_and_a_pick_reaches_the_run`.
#[test]
fn the_sdk_pick_reaches_the_file_spec_and_the_wire() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.sdk_pick = Some(3);
    assert_eq!(st.current_spec(), StrategySpec::rhai(st.editor.source.clone()), "Rhai has no SDK");

    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    assert!(matches!(st.current_spec(), StrategySpec::File { sdk: Some(3), .. }));
    assert_eq!(st.run_blocked_reason(), None);
    let wire = vike_studio_core::to_wire_spec(&st.current_spec()).expect("a file spec converts");
    assert_eq!(wire.sdk(), Some(3), "the pick crosses the wire");

    st.sdk_pick = None;
    assert!(matches!(st.current_spec(), StrategySpec::File { sdk: None, .. }), "as the file says");
}

/// **Native's own dirty check and chip text, not the Rhai-buffer one.** A Native save/load
/// captures `(name, params)`, never `editor.source` — the parked Rhai buffer — so neither the
/// dirty check nor the tooltip may reuse Rhai's. Before this fix the chip never lit on a
/// changed param row (the buffer hadn't moved) and could light spuriously on a stale parked
/// buffer that had nothing to do with the Native selection.
#[test]
fn native_mode_has_its_own_dirty_check_and_chip_text() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StudioStrategySource::File;
    // The premise this fix guards: the parked buffer disagrees with `saved_source`, and that
    // must have NO effect on Native's own chip.
    st.editor.source = "stale rhai text".to_string();
    st.saved_source = "different stale text".to_string();
    assert!(!st.file_is_dirty(), "a freshly restored/constructed Native baseline is clean");

    st.file_params.push(("qty".to_string(), "1".to_string()));
    assert!(st.file_is_dirty(), "an edited param row must light the chip");
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_FILE);

    st.saved.save_name = "n1".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);
    assert!(!st.file_is_dirty(), "a save re-baselines Native's own state");
    assert_eq!(
        st.saved.strategies[0].params, st.file_params,
        "the saved row holds the params that were actually current"
    );

    st.file_params[0].1 = "2".to_string();
    assert!(st.file_is_dirty(), "a further edit must re-dirty it");
    st.handle_saved_action(StudioSavedAction::Load(0));
    assert!(!st.file_is_dirty(), "loading the saved row re-baselines to it, like Rhai's Load");
    assert_eq!(st.file_params[0].1, "1", "the loaded row's own params come back");
}

/// The Native half of the same restore: the selected FILE comes back by its path, so a restored
/// session runs the file it was left on — before any list is fetched, since the list is the
/// server's and the path is the identity.
#[test]
fn a_native_selection_is_restored_by_path() {
    let (dir, store) = seeded_store();
    let last = "pairs/pairs_zscore.rs";
    let mut before = state_new(&dir, store.clone());
    before.strategy_source = StudioStrategySource::File;
    before.picked_file = last.to_string();
    before.file_params = vec![("size".to_string(), "2".to_string())];
    let ws = before.workspace_snapshot();
    assert_eq!(ws.picked_file, last);

    let restore = |ws: StudioWorkspace| {
        StudioState::with_workspace(
            store.clone(),
            dir.path().to_path_buf(),
            StudioChatApiKeys::default(),
            None,
            StudioAutorun::Off,
            ws,
        )
    };
    let after = restore(ws);
    assert_eq!(after.strategy_source, StudioStrategySource::File);
    assert_eq!(after.picked_file, last);
    assert_eq!(after.file_params, vec![("size".to_string(), "2".to_string())]);
    assert!(!after.file_is_dirty(), "the restored selection is the baseline");
}

/// With no file picked, Run is refused in words before anything is sent — an empty path would
/// otherwise reach the server as a strategy file.
#[test]
fn a_native_run_with_no_file_picked_is_refused_before_it_is_sent() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file.clear();
    assert_eq!(st.run_blocked_reason(), Some("Pick a strategy file in the Strategy tab first."));
    st.picked_file = "trend/buy_hold.rs".to_string();
    assert_eq!(st.run_blocked_reason(), None);
}

/// **Open puts a served file in the editor as a LOAD, in the mode its extension says**: a `.rhai`
/// file is a Rhai buffer, a `.rs` file a Rust buffer built as that path and not built yet. The
/// text arrives on the worker `poll` folds; a refusal lands in `open_last` and touches nothing.
#[test]
fn an_opened_file_lands_in_the_editor_in_its_own_language() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let arrive = |st: &mut StudioState, file: &str, answer: Result<String, String>| {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(answer).unwrap();
        st.open_rx = Some((file.to_string(), rx));
        st.poll();
    };
    arrive(&mut st, "trend/momentum.rs", Ok("pub fn build() {}\n".to_string()));
    assert_eq!(st.strategy_source, StudioStrategySource::Rust);
    assert_eq!((st.rust_file.as_str(), st.rust_sha.as_deref()), ("trend/momentum.rs", None));
    assert_eq!(st.editor.source, "pub fn build() {}\n");
    assert_eq!(st.saved_source, st.editor.source, "an Open is a load: no unsaved chip");
    assert!(st.run_blocked_reason().is_some(), "a Rust file runs after its Build");

    arrive(&mut st, "trend/sma_cross.rhai", Ok("fn on_bar() {}\n".to_string()));
    assert_eq!(st.strategy_source, StudioStrategySource::Rhai);
    assert_eq!(st.editor.source, "fn on_bar() {}\n");

    arrive(&mut st, "trend/gone.rs", Err("cannot read it".to_string()));
    assert_eq!(st.editor.source, "fn on_bar() {}\n", "a refused Open touches nothing");
    assert_eq!(st.open_last.as_deref(), Some("trend/gone.rs: cannot read it"));
}

/// **Rhai and Rust keep a buffer each**: after a `.rs` file opens, Rhai mode shows the Rhai script
/// again (judged as Rhai, so it compiles) and Strategy files keeps the buffer on screen; Rust mode
/// brings the Rust text back, edits and all. One shared buffer showed the `.rs` text in Rhai mode
/// under `● error · 'use' is a reserved keyword`.
#[test]
fn rhai_and_rust_keep_a_buffer_each() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let script = st.editor.source.clone();
    let rust = "use vike_model::Strategy;\npub fn build() {}\n";
    st.open_file_text("trend/momentum.rs", rust.to_string());
    st.editor.source.push_str("// an edit\n");
    st.switch_source(StudioStrategySource::Rhai);
    st.poll();
    assert_eq!(st.editor.source, script, "the Rhai script, not the Rust file");
    assert_eq!(st.rhai_verdict(), Some(&Ok(())), "checked as the Rhai it is");
    st.switch_source(StudioStrategySource::File);
    st.poll();
    assert_eq!(st.editor.source, script, "Strategy files keeps the buffer on screen");
    assert!(st.buffer_is_rhai());
    st.switch_source(StudioStrategySource::Rust);
    st.poll();
    assert_eq!(st.editor.source, format!("{rust}// an edit\n"), "the Rust text, edits and all");
    assert_ne!(st.editor.source, st.saved_source, "still unsaved");
    assert_eq!(st.rhai_verdict(), None, "no Rhai verdict over Rust");
}

/// **Open over unsaved changes asks first**, as a template load does: the text waits, the buffer is
/// untouched, and "Open it over my changes" (`write_open`) opens it. A clean buffer opens at once.
#[test]
fn open_over_unsaved_changes_asks_first() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = "fn on_bar() { /* mine */ }\n".to_string();
    st.open_file_text("trend/sma_cross.rhai", "fn on_bar() {}\n".to_string());
    assert_eq!(st.editor.source, "fn on_bar() { /* mine */ }\n", "nothing replaced yet");
    let (file, text) = st.pending_open.clone().expect("the open waits on the question");
    assert_eq!(file, "trend/sma_cross.rhai");
    st.write_open(&file, text);
    assert_eq!(st.editor.source, "fn on_bar() {}\n");
    assert_eq!(
        (st.pending_open.clone(), st.editor.source.clone()),
        (None, st.saved_source.clone())
    );
    st.open_file_text("trend/other.rhai", "fn on_bar() { 1; }\n".to_string());
    assert_eq!(st.editor.source, "fn on_bar() { 1; }\n", "a clean buffer opens at once");
}

/// **Open waits while the editor holds Rust edits nothing else holds** (a Rust save keeps a path
/// and a sha, never the text): it is refused until the edits are built or undone.
#[test]
fn open_is_refused_over_rust_edits_no_file_and_no_build_holds() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StudioStrategySource::Rust;
    st.editor.source = "pub fn build() {}".to_string();
    st.saved_source = st.editor.source.clone();
    assert_eq!(st.open_blocked_reason(), None, "the buffer is what was loaded");
    st.editor.source.push_str(" // edit");
    assert!(st.open_blocked_reason().is_some(), "an unkept Rust edit blocks Open");
    st.start_open_strategy_file("trend/momentum.rs".to_string());
    assert!(st.open_rx.is_none(), "the dispatcher refuses too, not only the button");
    st.rust_built_source = Some(st.editor.source.clone());
    assert_eq!(st.open_blocked_reason(), None, "a Build holds it");
    st.strategy_source = StudioStrategySource::Rhai;
    st.rust_built_source = None;
    assert_eq!(st.open_blocked_reason(), None, "a Rhai buffer is overwritten like a template load");
}

/// A NATIVE strategy runs end-to-end through the Studio's own Run path (no Rhai anywhere):
/// start_run -> worker -> poll folds in a real `TradingRecord`.
#[test]
fn native_run_reaches_a_result_through_the_studio_run_path() {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("symbol".into(), "BTCUSDT".into()), ("size".into(), "2".into())];
    st.start_run();
    assert!(st.running);
    for _ in 0..200 {
        st.poll();
        if !st.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!st.running, "the native run should complete");
    let res = st.last.expect("an outcome").expect("buy_hold should run");
    assert!(!res.summary.curve.is_empty());
}

/// Saving while in Native mode persists the registry name + param rows (not the editor
/// buffer), and Load restores the mode — the round trip the Strategy tab exists for.
#[test]
fn save_and_load_round_trip_a_native_strategy() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store.clone());
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("size".into(), "3".into())];
    st.saved.save_name = "hold-3".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);

    let mut reloaded = state_new(&_dir, store);
    assert_eq!(reloaded.saved.strategies.len(), 1);
    let entry = &reloaded.saved.strategies[0];
    assert_eq!(entry.source, StudioStrategySource::File);
    assert_eq!(entry.file, "trend/buy_hold.rs");
    assert_eq!(entry.params, vec![("size".to_string(), "3".to_string())]);
    // ...and a fresh session starts in Rhai mode until the entry is loaded.
    assert_eq!(reloaded.strategy_source, StudioStrategySource::Rhai);
    reloaded.handle_saved_action(StudioSavedAction::Load(0));
    assert_eq!(reloaded.strategy_source, StudioStrategySource::File);
    assert_eq!(reloaded.picked_file, "trend/buy_hold.rs");
    assert_eq!(reloaded.file_params, vec![("size".to_string(), "3".to_string())]);
    assert_eq!(reloaded.right_tab, StudioTab::Strategy, "Load jumps to the Strategy tab");
}

/// A strategy file's rows are its header's number parameters, read from the file's text when it
/// arrives (a text param is no row), each row's value being the file's params row of that name.
#[test]
fn a_strategy_files_rows_are_its_headers_number_params() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.sync_grid_key();
    st.seed_sweep_grid();
    assert!(st.seed_file_rx.is_some(), "the file's text is read to find its parameters");
    let (tx, rx) = std::sync::mpsc::channel();
    st.seed_file_rx = Some(rx);
    let text = "//! ```toml\n//! [strategy]\n//! [params.size]\n//! type = \"number\"\n//! default = 1.0\n\
                //! [params.symbol]\n//! type = \"text\"\n//! ```\npub fn x() {}\n";
    tx.send(Ok(text.to_string())).unwrap();
    st.poll();
    let names: Vec<&str> = st.grid.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["size"]);
    assert_eq!(st.grid[0].default, "1");
}

/// The test conditions' costs reach a strategy file's params (a value set there wins over a params
/// row), and the starting capital is read only as a number above zero.
#[test]
fn the_test_conditions_reach_a_strategy_files_params() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("commission".into(), "0.5".into())];
    st.conditions.costs = vec![StudioParamRow {
        name: "commission".into(),
        value: "0.1".into(),
        ..Default::default()
    }];
    match st.current_spec() {
        StrategySpec::File { params, .. } => {
            assert_eq!(params.get("commission").and_then(|v| v.as_f64()), Some(0.1));
        }
        other => panic!("expected a strategy file spec, got {other:?}"),
    }
    st.conditions.capital = "25000".into();
    assert_eq!(st.conditions.capital(), Some(25_000.0));
    st.conditions.capital = "-1".into();
    assert_eq!(st.conditions.capital(), None);
}

/// The costs are ONE reading of the panes: the saved row a Save stores (and a Compare all later runs),
/// the Named backend's spec and a Rust file's spec all carry them, as a Run's does. They were dropped by
/// every path but the Run's.
#[test]
fn the_costs_reach_a_saved_row_a_named_run_and_a_rust_file() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let commission = |spec: StrategySpec| match spec {
        StrategySpec::File { params, .. } => params.get("commission").and_then(|v| v.as_f64()),
        other => panic!("expected a file spec, got {other:?}"),
    };
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("size".into(), "2".into())];
    st.conditions.costs = vec![StudioParamRow {
        name: "commission".into(),
        value: "0.1".into(),
        ..Default::default()
    }];
    st.saved.save_name = "with costs".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);
    let saved = st.saved.strategies.iter().find(|s| s.name == "with costs").expect("saved");
    assert_eq!(commission(saved.spec()), Some(0.1), "the saved row, which Compare all runs");
    assert_eq!(st.file_saved.as_ref().map(|(_, p)| p.len()), Some(1), "the baseline is the rows");
    st.backend = crate::backend::backtesthub::StudioRunBackend::Named { address: "x:1".into() };
    st.named_strategy = Some("buy_hold".to_string());
    assert_eq!(commission(st.named_spec()), Some(0.1), "the Named backend's spec");
    st.strategy_source = StudioStrategySource::Rust;
    st.rust_file = "my/strat.rs".to_string();
    assert_eq!(commission(st.current_spec()), Some(0.1), "a Rust file's spec");
}

/// A Rhai script's run carries no params, so its scan holds the values set on unticked rows (and the
/// costs) as one-value axes; a strategy file's ride in its params and are not repeated.
#[test]
fn a_rhai_scan_holds_its_unticked_values_and_a_file_scan_does_not() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.grid = vec![
        StudioParamRow::scanned("fast", "5..15 step 5"),
        StudioParamRow { name: "slow".into(), value: "25".into(), ..Default::default() },
    ];
    st.conditions.costs = vec![StudioParamRow {
        name: "commission".into(),
        value: "0.1".into(),
        ..Default::default()
    }];
    assert_eq!(st.ticked_rows(), vec![("fast".to_string(), "5..15 step 5".to_string())]);
    assert_eq!(
        st.held_rows(),
        vec![("slow".to_string(), "25".to_string()), ("commission".to_string(), "0.1".to_string())]
    );
    st.strategy_source = StudioStrategySource::File;
    assert!(st.held_rows().is_empty());
}

/// GOLDEN, the axis rule (`vike_config::params_text::parse_axis`) as the Studio applies it: a
/// value takes the BASE knob's number kind where the base holds the knob (decision 0083), and its
/// own text kind where it does not (the CLI's rule, ruled the one rule: integral text is an `Int`).
/// A row that reads to no value is no axis.
#[test]
fn a_sweep_axis_is_typed_by_its_base_knob_and_by_its_text_without_one() {
    use super::super::dispatch::sweep_axes;
    use vike_model::params::ParamValue::{Float, Int};

    let base = params_from_rows(&[
        ("rungs".to_string(), "4".to_string()),
        ("size".to_string(), "1.5".to_string()),
    ]);
    let rows = vec![
        ("rungs".to_string(), "6, -2, 4.5, 1e19, -0.0".to_string()),
        ("size".to_string(), "2, 2.5".to_string()),
        ("absent".to_string(), "3, 3.5".to_string()),
        ("blank".to_string(), " , ".to_string()),
    ];
    let axes = sweep_axes(&rows, Some(&base));
    assert_eq!(
        axes,
        vec![
            ("rungs".to_string(), vec![Int(6), Int(-2), Float(4.5), Float(1e19), Int(0)]),
            ("size".to_string(), vec![Float(2.0), Float(2.5)]),
            ("absent".to_string(), vec![Int(3), Float(3.5)]),
        ]
    );
    let nan = sweep_axes(&[("rungs".to_string(), "nan".to_string())], Some(&base));
    assert_eq!(nan[0].1[0].type_str(), "float", "a NaN stays a Float");
    // Rhai: no table, so every axis keeps its text's kind — `5, 10` sweeps integers, which a
    // script's `param()` reads through `as_f64` exactly as it read the floats.
    let rhai = sweep_axes(&[("fast".to_string(), "5, 10".to_string())], None);
    assert_eq!(rhai, vec![("fast".to_string(), vec![Int(5), Int(10)])]);
}

/// The NATIVE sweep end to end on the Studio side: a knob typed `4` in the editor is swept as an
/// `Int` axis — the kind is decided from the very spec `start_sweep` sends.
#[test]
fn a_native_integer_row_becomes_an_int_axis() {
    use super::super::dispatch::sweep_axes;
    use vike_model::params::ParamValue::{Float, Int};

    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("size".into(), "2".into()), ("rate".into(), "0.5".into())];
    let spec = st.current_spec();
    let axes = sweep_axes(
        &[("size".to_string(), "1, 3".to_string()), ("rate".to_string(), "1".to_string())],
        spec.base_params(),
    );
    assert_eq!(
        axes,
        vec![("size".to_string(), vec![Int(1), Int(3)]), ("rate".to_string(), vec![Float(1.0)]),]
    );
}

/// A mixed Saved list (Rhai + native) compares in ONE pass and each row carries its kind.
#[test]
fn compare_all_ranks_a_mixed_rhai_and_native_list() {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    st.saved.strategies.push(StudioSavedStrategy::rhai("no-op", "fn on_bar() {}"));
    st.saved.strategies.push(StudioSavedStrategy::file(
        "hold",
        "trend/buy_hold.rs",
        vec![("symbol".into(), "BTCUSDT".into())],
    ));

    st.handle_saved_action(StudioSavedAction::CompareAll);
    for _ in 0..200 {
        st.poll();
        if st.compare_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let rows = st.saved.compare_rows.expect("compare should populate rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().find(|r| r.name == "no-op").unwrap().source, StudioStrategySource::Rhai);
    let native_row = rows.iter().find(|r| r.name == "hold").unwrap();
    assert_eq!(native_row.source, StudioStrategySource::File);
    assert!(native_row.outcome.is_ok(), "the native row ran: {:?}", native_row.outcome);
}
