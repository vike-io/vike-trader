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
    assert_eq!(st.strategy_source, StrategySource::Rhai);
    assert_eq!(st.current_spec(), StrategySpec::rhai(st.editor.source.clone()));

    st.strategy_source = StrategySource::Native;
    st.native_file = "trend/buy_hold.rs".to_string();
    st.native_params = vec![("size".into(), "2".into())];
    match st.current_spec() {
        StrategySpec::File { file: name, params } => {
            assert_eq!(name, "trend/buy_hold.rs");
            assert_eq!(params.get("size").and_then(|v| v.as_integer()), Some(2));
        }
        other => panic!("expected a native spec, got {other:?}"),
    }
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
    st.strategy_source = StrategySource::Native;
    // The premise this fix guards: the parked buffer disagrees with `saved_source`, and that
    // must have NO effect on Native's own chip.
    st.editor.source = "stale rhai text".to_string();
    st.saved_source = "different stale text".to_string();
    assert!(!st.native_is_dirty(), "a freshly restored/constructed Native baseline is clean");

    st.native_params.push(("qty".to_string(), "1".to_string()));
    assert!(st.native_is_dirty(), "an edited param row must light the chip");
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_NATIVE);

    st.saved.save_name = "n1".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);
    assert!(!st.native_is_dirty(), "a save re-baselines Native's own state");
    assert_eq!(
        st.saved.strategies[0].params, st.native_params,
        "the saved row holds the params that were actually current"
    );

    st.native_params[0].1 = "2".to_string();
    assert!(st.native_is_dirty(), "a further edit must re-dirty it");
    st.handle_saved_action(SavedAction::Load(0));
    assert!(!st.native_is_dirty(), "loading the saved row re-baselines to it, like Rhai's Load");
    assert_eq!(st.native_params[0].1, "1", "the loaded row's own params come back");
}

/// The Native half of the same restore: the selected FILE comes back by its path, so a restored
/// session runs the file it was left on — before any list is fetched, since the list is the
/// server's and the path is the identity.
#[test]
fn a_native_selection_is_restored_by_path() {
    let (dir, store) = seeded_store();
    let last = "pairs/pairs_zscore.rs";
    let mut before = state_new(&dir, store.clone());
    before.strategy_source = StrategySource::Native;
    before.native_file = last.to_string();
    before.native_params = vec![("size".to_string(), "2".to_string())];
    let ws = before.workspace_snapshot();
    assert_eq!(ws.native_strategy, last);

    let restore = |ws: StudioWorkspace| {
        StudioState::with_workspace(
            store.clone(),
            dir.path().to_path_buf(),
            ChatApiKeys::default(),
            None,
            QaAutorun::Off,
            ws,
        )
    };
    let after = restore(ws);
    assert_eq!(after.strategy_source, StrategySource::Native);
    assert_eq!(after.native_name(), last);
    assert_eq!(after.native_params, vec![("size".to_string(), "2".to_string())]);
    assert!(!after.native_is_dirty(), "the restored selection is the baseline");
}

/// With no file picked, Run is refused in words before anything is sent — an empty path would
/// otherwise reach the server as a strategy file.
#[test]
fn a_native_run_with_no_file_picked_is_refused_before_it_is_sent() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StrategySource::Native;
    st.native_file.clear();
    assert_eq!(st.run_blocked_reason(), Some("Pick a strategy file in the Strategy tab first."));
    st.native_file = "trend/buy_hold.rs".to_string();
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
    assert_eq!(st.strategy_source, StrategySource::Rust);
    assert_eq!((st.rust_file.as_str(), st.rust_sha.as_deref()), ("trend/momentum.rs", None));
    assert_eq!(st.editor.source, "pub fn build() {}\n");
    assert_eq!(st.saved_source, st.editor.source, "an Open is a load: no unsaved chip");
    assert!(st.run_blocked_reason().is_some(), "a Rust file runs after its Build");

    arrive(&mut st, "trend/sma_cross.rhai", Ok("fn on_bar() {}\n".to_string()));
    assert_eq!(st.strategy_source, StrategySource::Rhai);
    assert_eq!(st.editor.source, "fn on_bar() {}\n");

    arrive(&mut st, "trend/gone.rs", Err("cannot read it".to_string()));
    assert_eq!(st.editor.source, "fn on_bar() {}\n", "a refused Open touches nothing");
    assert_eq!(st.open_last.as_deref(), Some("trend/gone.rs: cannot read it"));
}

/// **Open waits while the editor holds Rust edits nothing else holds** (a Rust save keeps a path
/// and a sha, never the text): it is refused until the edits are built or undone.
#[test]
fn open_is_refused_over_rust_edits_no_file_and_no_build_holds() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StrategySource::Rust;
    st.editor.source = "pub fn build() {}".to_string();
    st.saved_source = st.editor.source.clone();
    assert_eq!(st.open_blocked_reason(), None, "the buffer is what was loaded");
    st.editor.source.push_str(" // edit");
    assert!(st.open_blocked_reason().is_some(), "an unkept Rust edit blocks Open");
    st.start_open_strategy_file("trend/momentum.rs".to_string());
    assert!(st.open_rx.is_none(), "the dispatcher refuses too, not only the button");
    st.strategy_file_built_source = Some(st.editor.source.clone());
    assert_eq!(st.open_blocked_reason(), None, "a Build holds it");
    st.strategy_source = StrategySource::Rhai;
    st.strategy_file_built_source = None;
    assert_eq!(st.open_blocked_reason(), None, "a Rhai buffer is overwritten like a template load");
}

/// A NATIVE strategy runs end-to-end through the Studio's own Run path (no Rhai anywhere):
/// start_run -> worker -> poll folds in a real `BacktestResult`.
#[test]
fn native_run_reaches_a_result_through_the_studio_run_path() {
    let (_dir, store) = seeded_store();
    let addr = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::remote::Backend::Remote { addr };
    st.picker.select(0);
    st.strategy_source = StrategySource::Native;
    st.native_file = "trend/buy_hold.rs".to_string();
    st.native_params = vec![("symbol".into(), "BTCUSDT".into()), ("size".into(), "2".into())];
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
    assert!(!res.equity_curve.is_empty());
}

/// Saving while in Native mode persists the registry name + param rows (not the editor
/// buffer), and Load restores the mode — the round trip the Strategy tab exists for.
#[test]
fn save_and_load_round_trip_a_native_strategy() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store.clone());
    st.strategy_source = StrategySource::Native;
    st.native_file = "trend/buy_hold.rs".to_string();
    st.native_params = vec![("size".into(), "3".into())];
    st.saved.save_name = "hold-3".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);

    let mut reloaded = state_new(&_dir, store);
    assert_eq!(reloaded.saved.strategies.len(), 1);
    let entry = &reloaded.saved.strategies[0];
    assert_eq!(entry.source, StrategySource::Native);
    assert_eq!(entry.native, "trend/buy_hold.rs");
    assert_eq!(entry.params, vec![("size".to_string(), "3".to_string())]);
    // ...and a fresh session starts in Rhai mode until the entry is loaded.
    assert_eq!(reloaded.strategy_source, StrategySource::Rhai);
    reloaded.handle_saved_action(SavedAction::Load(0));
    assert_eq!(reloaded.strategy_source, StrategySource::Native);
    assert_eq!(reloaded.native_name(), "trend/buy_hold.rs");
    assert_eq!(reloaded.native_params, vec![("size".to_string(), "3".to_string())]);
    assert_eq!(reloaded.right_tab, RightTab::Strategy, "Load jumps to the Strategy tab");
}

/// The sweep grid seeds from the NATIVE param rows when native is active (there is no
/// `discover_params` twin for the registry), numeric rows only.
#[test]
fn seed_grid_from_native_param_rows_skips_non_numeric() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StrategySource::Native;
    st.native_params = vec![
        ("size".into(), "2".into()),
        ("symbol".into(), "BTCUSDT".into()),
        ("".into(), "9".into()),
    ];
    st.seed_sweep_grid();
    assert_eq!(st.grid, vec![("size".to_string(), "2".to_string())]);
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
    st.strategy_source = StrategySource::Native;
    st.native_file = "trend/buy_hold.rs".to_string();
    st.native_params = vec![("size".into(), "2".into()), ("rate".into(), "0.5".into())];
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
    let addr = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::remote::Backend::Remote { addr };
    st.picker.select(0);
    st.saved.strategies.push(SavedStrategy::rhai("no-op", "fn on_bar() {}"));
    st.saved.strategies.push(SavedStrategy::native(
        "hold",
        "trend/buy_hold.rs",
        vec![("symbol".into(), "BTCUSDT".into())],
    ));

    st.handle_saved_action(SavedAction::CompareAll);
    for _ in 0..200 {
        st.poll();
        if st.compare_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let rows = st.saved.compare_rows.expect("compare should populate rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().find(|r| r.name == "no-op").unwrap().source, StrategySource::Rhai);
    let native_row = rows.iter().find(|r| r.name == "hold").unwrap();
    assert_eq!(native_row.source, StrategySource::Native);
    assert!(native_row.outcome.is_ok(), "the native row ran: {:?}", native_row.outcome);
}
