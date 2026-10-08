//! Native (registry) strategy mode: the spec it resolves, its own dirty chip, restore by name, a
//! run through the Studio's path, and the Saved / sweep-grid / Compare rows it adds.

use super::super::*;
use super::support::{seeded_store, spawn_compute_server, state_new};
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
    st.native_idx = native_strategies().iter().position(|n| *n == "buy_hold").unwrap();
    st.native_params = vec![("size".into(), "2".into())];
    match st.current_spec() {
        StrategySpec::Native { name, params } => {
            assert_eq!(name, "buy_hold");
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

/// The Native half of the same restore: the dropdown comes back by NAME, so a restored Native
/// session runs the strategy it was left on. The LAST roster entry is picked so the answer
/// cannot be the index-0 default by accident, and an unknown name (a registry that dropped the
/// strategy since) falls back to row 0 rather than panicking.
#[test]
fn a_native_selection_is_restored_by_name() {
    let (dir, store) = seeded_store();
    let roster = native_strategies();
    assert!(roster.len() > 1, "the premise: a last entry distinct from row 0");
    let last = *roster.last().unwrap();
    let mut before = state_new(&dir, store.clone());
    before.strategy_source = StrategySource::Native;
    before.native_idx = roster.len() - 1;
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

    let gone = StudioWorkspace {
        strategy_source: StrategySource::Native,
        native_strategy: "no_such_strategy".to_string(),
        ..Default::default()
    };
    assert_eq!(restore(gone).native_idx, 0, "an unknown name keeps row 0");
}

/// `native_name` clamps a stale/oversized index instead of indexing out of bounds.
#[test]
fn native_name_clamps_an_out_of_range_index() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.native_idx = usize::MAX;
    assert_eq!(st.native_name(), *native_strategies().last().unwrap());
}

/// A NATIVE strategy runs end-to-end through the Studio's own Run path (no Rhai anywhere):
/// start_run -> worker -> poll folds in a real `BacktestResult`.
#[test]
fn native_run_reaches_a_result_through_the_studio_run_path() {
    let (_dir, store) = seeded_store();
    let addr = spawn_compute_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::remote::Backend::Remote { addr };
    st.picker.select(0);
    st.strategy_source = StrategySource::Native;
    st.native_idx = native_strategies().iter().position(|n| *n == "buy_hold").unwrap();
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
    st.native_idx = native_strategies().iter().position(|n| *n == "buy_hold").unwrap();
    st.native_params = vec![("size".into(), "3".into())];
    st.saved.save_name = "hold-3".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);

    let mut reloaded = state_new(&_dir, store);
    assert_eq!(reloaded.saved.strategies.len(), 1);
    let entry = &reloaded.saved.strategies[0];
    assert_eq!(entry.source, StrategySource::Native);
    assert_eq!(entry.native, "buy_hold");
    assert_eq!(entry.params, vec![("size".to_string(), "3".to_string())]);
    // ...and a fresh session starts in Rhai mode until the entry is loaded.
    assert_eq!(reloaded.strategy_source, StrategySource::Rhai);
    reloaded.handle_saved_action(SavedAction::Load(0));
    assert_eq!(reloaded.strategy_source, StrategySource::Native);
    assert_eq!(reloaded.native_name(), "buy_hold");
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

/// A mixed Saved list (Rhai + native) compares in ONE pass and each row carries its kind.
#[test]
fn compare_all_ranks_a_mixed_rhai_and_native_list() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.picker.select(0);
    st.saved.strategies.push(SavedStrategy::rhai("no-op", "fn on_bar() {}"));
    st.saved.strategies.push(SavedStrategy::native(
        "hold",
        "buy_hold",
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
    assert!(native_row.error.is_none(), "the native row ran: {:?}", native_row.error);
}
