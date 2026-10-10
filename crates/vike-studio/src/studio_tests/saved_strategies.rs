//! The Saved list: save / overwrite / load / delete round trips, the re-baseline they perform, and
//! Compare All over a Rhai list.

use super::super::*;
use super::support::{seeded_store, state_new};
use std::sync::Arc;
use vike_data::DataFusionHist;

/// Saving or loading a strategy re-baselines `saved_source` so the unsaved-changes dot clears.
#[test]
fn save_and_load_rebaseline_saved_source() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = "fn on_bar() { market(1, 1.0); }".to_string();
    assert_ne!(st.editor.source, st.saved_source);

    st.saved.save_name = "dirty-then-clean".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);
    assert_eq!(st.editor.source, st.saved_source, "Save should re-baseline");

    st.editor.source = "stale-edit".to_string();
    assert_ne!(st.editor.source, st.saved_source);
    st.handle_saved_action(SavedAction::Load(0));
    assert_eq!(st.editor.source, st.saved_source, "Load should re-baseline");
}

/// `SaveCurrent` snapshots `editor.source` under `saved.save_name`, appends it to
/// `saved.strategies`, and persists to `state_dir/studio_strategies.json` — a fresh
/// `StudioState::new` over the same store must reload it (the round-trip the whole feature
/// exists for).
#[test]
fn save_current_persists_and_reloads_across_a_new_studio_state() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store.clone());
    st.editor.source = "fn on_bar() {}".to_string();
    st.saved.save_name = "my-strategy".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);

    assert_eq!(st.saved.strategies.len(), 1);
    assert_eq!(st.saved.strategies[0].name, "my-strategy");
    assert!(st.saved.save_name.is_empty(), "the name box should clear after saving");

    // a fresh StudioState over the SAME store root reloads the file SaveCurrent wrote.
    let reloaded = state_new(&_dir, store);
    assert_eq!(reloaded.saved.strategies.len(), 1);
    assert_eq!(reloaded.saved.strategies[0].name, "my-strategy");
    assert_eq!(reloaded.saved.strategies[0].code, "fn on_bar() {}");
}

/// Saving again under the same name overwrites the code in place rather than appending a
/// duplicate row.
#[test]
fn save_current_with_an_existing_name_overwrites_in_place() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = "fn on_bar() {}".to_string();
    st.saved.save_name = "v1".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);

    st.editor.source = "fn on_bar() { market(1, 1.0); }".to_string();
    st.saved.save_name = "v1".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);

    assert_eq!(st.saved.strategies.len(), 1, "same name updates, doesn't duplicate");
    assert_eq!(st.saved.strategies[0].code, "fn on_bar() { market(1, 1.0); }");
}

#[test]
fn load_action_copies_the_saved_code_into_the_editor() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.saved.strategies.push(SavedStrategy::rhai("flat", "fn on_bar() {}"));
    st.editor.source = "stale".to_string();

    st.handle_saved_action(SavedAction::Load(0));

    assert_eq!(st.editor.source, "fn on_bar() {}");
}

#[test]
fn delete_action_removes_and_persists() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store.clone());
    st.saved.strategies.push(SavedStrategy::rhai("a", "1"));
    st.saved.strategies.push(SavedStrategy::rhai("b", "2"));
    st.persist_saved();

    st.handle_saved_action(SavedAction::Delete(0));

    assert_eq!(st.saved.strategies.len(), 1);
    assert_eq!(st.saved.strategies[0].name, "b");
    let reloaded = state_new(&_dir, store);
    assert_eq!(reloaded.saved.strategies.len(), 1);
    assert_eq!(reloaded.saved.strategies[0].name, "b");
}

/// `CompareAll` now runs on a worker thread (`compare_rx`), so this drives `poll()` until it
/// lands — the `start_run_then_poll_reaches_a_result` pattern. Every saved strategy over the
/// currently-selected slice gets ranked: the SMA-cross script (the editor's own default)
/// should out-trade / out-rank the no-op.
#[test]
fn compare_all_ranks_saved_strategies_over_the_selected_slice() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.picker.select(0);
    st.saved.strategies.push(SavedStrategy::rhai("no-op", "fn on_bar() {}"));
    st.saved.strategies.push(SavedStrategy::rhai("sma-cross", EditorPane::default().source));

    st.handle_saved_action(SavedAction::CompareAll);
    assert!(st.compare_rx.is_some(), "compare should be running on a worker thread");
    for _ in 0..200 {
        st.poll();
        if st.compare_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(st.compare_rx.is_none(), "compare should complete");

    let rows = st.saved.compare_rows.expect("compare should populate rows");
    assert_eq!(rows.len(), 2);
    assert!(st.saved.compare_error.is_none());
    let noop = rows.iter().find(|r| r.name == "no-op").unwrap();
    assert_eq!(noop.n_trades, 0, "the no-op strategy never trades");
    let cross = rows.iter().find(|r| r.name == "sma-cross").unwrap();
    assert!(cross.n_trades > 0, "the SMA-cross strategy should trade over 400 bars");
}

/// A second `CompareAll` while one is already running is a no-op (mirrors `start_sweep`'s
/// "already running" guard) — it must not spawn a second worker or clobber `compare_rx`.
#[test]
fn compare_all_is_a_no_op_while_already_running() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.picker.select(0);
    st.saved.strategies.push(SavedStrategy::rhai("no-op", "fn on_bar() {}"));

    st.handle_saved_action(SavedAction::CompareAll);
    assert!(st.compare_rx.is_some());

    st.handle_saved_action(SavedAction::CompareAll);
    assert!(st.compare_rx.is_some(), "still running, unchanged");

    for _ in 0..200 {
        st.poll();
        if st.compare_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(st.compare_rx.is_none());
    assert!(st.saved.compare_rows.is_some());
}

#[test]
fn compare_all_with_no_slice_selected_sets_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap()); // no series -> picker has nothing selected
    let mut st = state_new(&dir, store);
    st.saved.strategies.push(SavedStrategy::rhai("a", "1"));

    st.handle_saved_action(SavedAction::CompareAll);

    assert!(st.saved.compare_rows.is_none());
    assert!(st.saved.compare_error.is_some());
}
