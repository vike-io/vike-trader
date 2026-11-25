//! The Saved list: save / overwrite / load / delete round trips, the re-baseline they perform, and
//! Compare All over a Rhai list.

use super::super::*;
use super::support::{seeded_store, spawn_backtesthub_server, state_new};
use std::sync::Arc;
use vike_store::HistoryStoreDataFusion;

/// A one-order script, distinct from the editor's default.
const BUY_ONE: &str =
    r#"fn on_bar(strategy) { strategy.order("buy", OrderSide::Long).size(1.0); }"#;

/// Saving or loading a strategy re-baselines `saved_source` so the unsaved-changes dot clears.
#[test]
fn save_and_load_rebaseline_saved_source() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = BUY_ONE.to_string();
    assert_ne!(st.editor.source, st.saved_source);

    st.saved.save_name = "dirty-then-clean".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);
    assert_eq!(st.editor.source, st.saved_source, "Save should re-baseline");

    st.editor.source = "stale-edit".to_string();
    assert_ne!(st.editor.source, st.saved_source);
    st.handle_saved_action(StudioSavedAction::Load(0));
    assert_eq!(st.editor.source, st.saved_source, "Load should re-baseline");
}

/// `SaveCurrent` snapshots `editor.source` under `saved.save_name`, appends it to
/// `saved.strategies`, and persists to `state_dir/studio_strategies.json` — a fresh
/// `StudioState::new_with_qa` over the same store must reload it (the round-trip the whole feature
/// exists for).
#[test]
fn save_current_persists_and_reloads_across_a_new_studio_state() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store.clone());
    st.editor.source = "fn on_bar() {}".to_string();
    st.saved.save_name = "my-strategy".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);

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
    st.handle_saved_action(StudioSavedAction::SaveCurrent);

    st.editor.source = BUY_ONE.to_string();
    st.saved.save_name = "v1".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);

    assert_eq!(st.saved.strategies.len(), 1, "same name updates, doesn't duplicate");
    assert_eq!(st.saved.strategies[0].code, BUY_ONE);
}

#[test]
fn load_action_copies_the_saved_code_into_the_editor() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.saved.strategies.push(StudioSavedStrategy::rhai("flat", "fn on_bar() {}"));
    st.editor.source = "stale".to_string();

    st.handle_saved_action(StudioSavedAction::Load(0));

    assert_eq!(st.editor.source, "fn on_bar() {}");
}

#[test]
fn delete_action_removes_and_persists() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store.clone());
    st.saved.strategies.push(StudioSavedStrategy::rhai("a", "1"));
    st.saved.strategies.push(StudioSavedStrategy::rhai("b", "2"));
    st.persist_saved();

    st.handle_saved_action(StudioSavedAction::Delete(0));

    assert_eq!(st.saved.strategies.len(), 1);
    assert_eq!(st.saved.strategies[0].name, "b");
    let reloaded = state_new(&_dir, store);
    assert_eq!(reloaded.saved.strategies.len(), 1);
    assert_eq!(reloaded.saved.strategies[0].name, "b");
}

/// `CompareAll` sends each saved row to the backtesthub on a worker thread (`compare_rx`), so this
/// serves a hub over the seeded store and drives `poll()` until it lands — the
/// `start_run_then_poll_reaches_a_result` pattern. Every saved strategy over the
/// currently-selected slice gets ranked: the SMA-cross script (the editor's own default)
/// should out-trade / out-rank the no-op.
#[test]
fn compare_all_ranks_saved_strategies_over_the_selected_slice() {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    st.saved.strategies.push(StudioSavedStrategy::rhai("no-op", "fn on_bar() {}"));
    st.saved
        .strategies
        .push(StudioSavedStrategy::rhai("sma-cross", StudioEditorPane::default().source));

    st.handle_saved_action(StudioSavedAction::CompareAll);
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
    let trades = |name: &str| {
        let row = rows.iter().find(|r| r.name == name).unwrap();
        row.outcome.as_ref().expect("the row ran").n_trades
    };
    assert_eq!(trades("no-op"), 0, "the no-op strategy never trades");
    assert!(trades("sma-cross") > 0, "the SMA-cross strategy should trade over 400 bars");
}

/// THE D4 PROOF: a compare row is the very run a Run of that row is. The SMA-cross row's report
/// equals the report of the same script Run through the same hub over the same slice — both
/// priced by the hub (the lane fee), neither by an in-process simulator at zero cost.
#[test]
fn a_compare_row_is_the_run_of_that_row() {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    let script = StudioEditorPane::default().source;
    st.saved.strategies.push(StudioSavedStrategy::rhai("sma-cross", script.clone()));

    st.handle_saved_action(StudioSavedAction::CompareAll);
    st.strategy_source = StudioStrategySource::Rhai;
    st.editor.source = script;
    st.start_run();
    for _ in 0..200 {
        st.poll();
        if st.compare_rx.is_none() && !st.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let rows = st.saved.compare_rows.as_ref().expect("compare should populate rows");
    let compared = rows[0].outcome.as_ref().expect("the row ran");
    let ran = match &st.last {
        Some(Ok(r)) => &r.summary,
        other => panic!("the Run must land: {other:?}"),
    };
    assert_eq!(compared.final_equity.to_bits(), ran.final_equity.to_bits());
    assert_eq!(compared.n_trades, ran.n_trades);
    assert_eq!(
        rows[0].cells[0][0],
        ran.rows.iter().find(|(id, _)| id == "sharpe").map(|(_, v)| v.clone()).unwrap(),
        "the compare table's Sharpe is the Performance tab's"
    );
}

/// **Loading a Rhai row brings back its values and costs**, so a Run after the Load runs what the row saved, as
/// Compare all runs it: its spec is the saved row's again after the panes moved on.
#[test]
fn loading_a_rhai_row_brings_back_its_values_and_costs() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.strategy_source = StudioStrategySource::Rhai;
    st.poll();
    st.grid = vec![StudioParamRow { name: "fast".into(), value: "7".into(), ..Default::default() }];
    let commission = StudioParamRow { name: "commission".into(), ..Default::default() };
    st.conditions.costs = vec![StudioParamRow { value: "0.1".into(), ..commission }];
    st.saved.save_name = "with values".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);
    let saved = st.saved.strategies[0].spec();
    assert!(matches!(saved, StrategySpec::RhaiWithParameters { .. }), "{saved:?}");
    st.grid[0].value.clear();
    st.conditions.costs[0].value = "0.5".into();
    assert_ne!(st.current_spec(), saved, "the panes moved on");
    st.handle_saved_action(StudioSavedAction::Load(0));
    assert_eq!(st.grid[0].value, "7");
    assert_eq!(st.conditions.costs[0].value, "0.1");
    assert_eq!(st.current_spec(), saved, "a Run after the Load runs the saved row");
}

/// A state over `state_dir` (not the temp dir itself), as the desktop builds one.
fn state_in(state_dir: std::path::PathBuf, store: Arc<HistoryStoreDataFusion>) -> StudioState {
    StudioState::with_workspace(
        store,
        state_dir,
        StudioChatApiKeys::default(),
        None,
        StudioAutorun::Off,
        StudioWorkspace::default(),
    )
}

/// A Save into a state directory that does not exist yet creates it and lands on disk, so a restart
/// keeps it (the owner's thin client had no `market_data/hist`, and the write was swallowed).
#[test]
fn a_save_into_a_missing_folder_lands_on_disk() {
    let (dir, store) = seeded_store();
    let state_dir = dir.path().join("settings").join("state");
    let mut st = state_in(state_dir.clone(), Arc::clone(&store));
    st.editor.source = BUY_ONE.to_string();
    st.save_editor();
    assert_eq!(st.saved.write_error, None);
    assert_eq!(st.editor.source, st.saved_source, "written, so no longer unsaved");
    let reloaded = state_in(state_dir, store);
    assert_eq!(reloaded.saved.strategies.len(), 1, "the save survives a restart");
    assert_eq!(reloaded.saved.strategies[0].code, BUY_ONE);
}

/// A Save whose write fails keeps "unsaved" on, leaves the list as it was, and says why; a Delete that
/// was not written is undone.
#[test]
fn a_failed_save_stays_unsaved_and_says_why() {
    let (dir, store) = seeded_store();
    let blocker = dir.path().join("a-file");
    std::fs::write(&blocker, "not a folder").unwrap();
    let mut st = state_in(blocker.join("state"), store);
    st.editor.source = BUY_ONE.to_string();
    st.save_editor();
    assert_ne!(st.editor.source, st.saved_source, "nothing was written: still unsaved");
    assert!(st.saved.strategies.is_empty(), "the list shows what is on disk");
    let why = st.saved.write_error.clone().expect("the failure is shown");
    assert!(why.starts_with("Not saved to "), "{why}");
    st.saved.strategies.push(StudioSavedStrategy::rhai("kept", "1"));
    st.handle_saved_action(StudioSavedAction::Delete(0));
    assert_eq!(st.saved.strategies.len(), 1, "a delete that was not written is undone");
}

/// Compare all starts every row at the test conditions' starting capital, as a Run does: a strategy that
/// never trades ends at exactly that capital.
#[test]
fn compare_all_starts_every_row_at_the_starting_capital() {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    st.conditions.capital = "31337".to_string();
    st.saved.strategies.push(StudioSavedStrategy::rhai("no-op", "fn on_bar() {}"));
    st.handle_saved_action(StudioSavedAction::CompareAll);
    for _ in 0..200 {
        st.poll();
        if st.compare_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let rows = st.saved.compare_rows.expect("compare should populate rows");
    let row = rows[0].outcome.as_ref().expect("the row ran");
    assert_eq!(row.final_equity, 31_337.0);
}

/// A second `CompareAll` while one is already running is a no-op (mirrors `start_sweep`'s
/// "already running" guard) — it must not spawn a second worker or clobber `compare_rx`.
#[test]
fn compare_all_is_a_no_op_while_already_running() {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    st.saved.strategies.push(StudioSavedStrategy::rhai("no-op", "fn on_bar() {}"));

    st.handle_saved_action(StudioSavedAction::CompareAll);
    assert!(st.compare_rx.is_some());

    st.handle_saved_action(StudioSavedAction::CompareAll);
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
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap()); // no series -> picker has nothing selected
    let mut st = state_new(&dir, store);
    st.saved.strategies.push(StudioSavedStrategy::rhai("a", "1"));

    st.handle_saved_action(StudioSavedAction::CompareAll);

    assert!(st.saved.compare_rows.is_none());
    assert!(st.saved.compare_error.is_some());
}
