//! The wire fields of 2026-11-20 as the shell sends them: the funding switch (OFF unless ticked) on every run, a
//! Rhai script's values and costs on a plain Run, and what the panes say on a server that predates either.

use super::super::*;
use super::support::{seeded_store, spawn_backtesthub_server, state_new};
use std::sync::Arc;
use vike_backtesthub_client::proto::{FEATURE_ATTACH_FUNDING, FEATURE_RHAI_PARAMETERS};

/// Run what the panes hold over the seeded slice and wait for its summary.
fn run_to_end(st: &mut StudioState) -> vike_backtesthub_client::summary::RunSummaryWire {
    st.start_run();
    for _ in 0..200 {
        st.poll();
        if !st.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    match &st.last {
        Some(Ok(view)) => view.summary.clone(),
        other => panic!("the run must land: {other:?}"),
    }
}

/// **A plain Run of a Rhai script sends the pane's values and costs** (`RhaiWithParameters`), and the server runs
/// the script at them: the editor's trading script charged a commission ends lower than the bare script. With no
/// value set it is the bare script. The funding switch rides on the same request, and the real server takes both.
#[test]
fn a_plain_rhai_run_sends_its_values_and_costs() {
    let (dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    st.strategy_source = StudioStrategySource::Rhai;
    let script = crate::panes::editor::StudioEditorPane::default().source;
    st.editor.source = script.clone();
    let own = run_to_end(&mut st);
    assert!(own.n_trades > 0, "the editor's script trades over the seeded bars");
    assert_eq!(st.current_spec(), StrategySpec::rhai(script));
    st.grid = vec![StudioParamRow { name: "fast".into(), value: "3".into(), ..Default::default() }];
    match st.current_spec() {
        StrategySpec::RhaiWithParameters { parameters, .. } => {
            assert_eq!(parameters.get("fast").and_then(|v| v.as_integer()), Some(3));
        }
        other => panic!("a Rhai run with a value carries it: {other:?}"),
    }
    st.grid.clear();
    let commission = ("commission".to_string(), "1".to_string());
    let row = StudioParamRow { name: commission.0, value: commission.1, ..Default::default() };
    st.conditions.costs = vec![row];
    st.conditions.funding = true;
    let charged = run_to_end(&mut st);
    assert!(charged.final_equity < own.final_equity, "the commission reached the run");
}

/// **The funding switch is OFF unless ticked**, rides on every run's engine settings with the capital, is persisted
/// with the test conditions, and the Named backend (whose request has no field for it) says so.
#[test]
fn the_funding_switch_is_off_by_default_and_reaches_every_run() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    assert!(!st.conditions.funding, "off unless ticked");
    assert_eq!(st.run_engine(), StudioRunEngine::default());
    st.conditions.funding = true;
    st.conditions.capital = "500".to_string();
    assert_eq!(st.run_engine(), StudioRunEngine { cash: Some(500.0), funding: true });
    assert!(st.workspace_snapshot().test_conditions.funding, "persisted in the workspace");
    st.conditions.capital.clear();
    st.backend = crate::backend::backtesthub::StudioRunBackend::Named { address: "x:1".into() };
    assert_eq!(st.capital_not_sent(), Some(NAMED_BACKEND_CAPITAL));
}

/// **The panes say what the server serves**: unknown (not checked), a Run sends the Rhai values; a server that
/// predates the funding switch and a Rhai script's parameters is answered in words (an unticked run refused, the
/// Value line saying a plain Run with values is refused there); a newer one, neither.
#[test]
fn the_panes_follow_what_the_server_serves() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StudioStrategySource::Rhai;
    assert_eq!(st.server_serves(FEATURE_ATTACH_FUNDING), None, "not checked");
    assert!(!st.rhai_values_unsent(), "a Run decides on the server it dials");
    st.server_check = Some(Ok(Vec::new()));
    assert_eq!(st.server_serves(FEATURE_ATTACH_FUNDING), Some(false));
    assert!(st.rhai_values_unsent());
    assert!(st.value_words().contains("it needs an update"), "{}", st.value_words());
    let served = vec![FEATURE_ATTACH_FUNDING.to_string(), FEATURE_RHAI_PARAMETERS.to_string()];
    st.server_check = Some(Ok(served));
    assert_eq!(st.server_serves(FEATURE_ATTACH_FUNDING), Some(true));
    assert!(!st.rhai_values_unsent());
    assert!(st.value_words().starts_with("Value: what a Run uses"));
    st.server_check = Some(Err("unreachable".into()));
    assert_eq!(st.server_serves(FEATURE_RHAI_PARAMETERS), None, "not connected: not known");
}
