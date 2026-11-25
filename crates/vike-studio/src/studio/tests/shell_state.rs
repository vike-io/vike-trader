//! A fresh `StudioState`, the Rhai verdict `poll` holds, the "why is this disabled" sentences Run,
//! Sweep, Walk-forward and Send give, and the backend switch.

use super::super::*;
use super::support::{seeded_store, state_new};
use std::assert_matches;
use std::sync::Arc;
use vike_store::HistoryStoreDataFusion;

#[test]
fn editor_and_tools_panes_start_expanded() {
    let (_dir, store) = seeded_store();
    let st = state_new(&_dir, store);
    assert!(!st.editor_collapsed);
    assert!(!st.tools_collapsed);
}

/// `new()` seeds the compile cache with the default (known-good) script, and the "unsaved"
/// baseline starts equal to the source — no red/amber dot on first paint.
#[test]
fn new_state_starts_with_a_clean_compile_and_no_unsaved_marker() {
    let (_dir, store) = seeded_store();
    let st = state_new(&_dir, store);
    assert_eq!(st.rhai_verdict(), Some(&Ok(())));
    assert_eq!(st.editor.source, st.saved_source);
}

/// `poll()` recompiles only when the source drifted, and reports the compile error for broken
/// source.
#[test]
fn poll_updates_the_rhai_verdict_when_source_changes() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = "fn on_bar( {".to_string();
    st.poll();
    assert_matches!(st.rhai_verdict(), Some(Err(_)));

    st.editor.source = crate::panes::editor::StudioEditorPane::default().source;
    st.poll();
    assert_eq!(st.rhai_verdict(), Some(&Ok(())));
}

/// **The Rhai check never runs on a Strategy file's Rust buffer, and no verdict survives a mode
/// switch in either direction.** The state-machine half of
/// `crates/vike-studio/tests/headless/studio_shell_render/editor_verdict.rs`'s
/// `a_strategy_file_buffer_is_not_judged_as_rhai_and_the_same_buffer_in_rhai_mode_is`, which asserts
/// what the header RENDERS; this one asserts what `poll` HOLDS, including across two polls in
/// a row (the per-frame case).
///
/// The Rhai leg comes FIRST and holds a real `Err` for this exact source, so the Strategy file leg's
/// `None` is a verdict being DROPPED rather than one that was never computed — and the last
/// leg flips back with the source unchanged, so only a re-check (not a drifted-source recompile)
/// can put a verdict back.
#[test]
fn strategy_file_mode_holds_no_rhai_verdict_and_rhai_mode_rechecks_on_return() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.editor.source = "use vike_model::Strategy;\npub fn build() {}\n".to_string();
    st.poll();
    assert_matches!(
        st.rhai_verdict(),
        Some(Err(_)),
        "the control: Rust source IS a Rhai error while the buffer is Rhai"
    );

    st.strategy_source = StudioStrategySource::Rust;
    assert!(!st.buffer_is_rhai());
    st.poll();
    assert_eq!(st.rhai_verdict(), None, "Strategy file mode must hold no Rhai verdict");
    st.poll();
    assert!(st.rhai_check.is_none(), "...on every frame, not only the first after the switch");

    st.strategy_source = StudioStrategySource::Rhai;
    st.poll();
    match &st.rhai_check {
        Some((checked, Err(_))) => assert_eq!(checked, &st.editor.source),
        other => panic!("back in Rhai mode the buffer must be re-checked: {other:?}"),
    }

    // Native keeps the check: its buffer is the parked Rhai script (`buffer_is_rhai`'s doc).
    st.strategy_source = StudioStrategySource::File;
    assert!(st.buffer_is_rhai());
    st.poll();
    assert_matches!(st.rhai_verdict(), Some(Err(_)));
}

/// Run says why it is disabled — one blocker at a time, the first a person can act on — and its
/// enable condition is unchanged: `None` exactly when the old `add_enabled` bool was true.
#[test]
fn run_says_why_it_is_disabled() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    assert_eq!(st.run_disabled_reason(), None, "a picked slice over a Rhai buffer runs");
    st.running = true;
    assert_eq!(st.run_disabled_reason(), Some(RUN_BUSY));
    st.running = false;
    st.strategy_source = StudioStrategySource::Rust;
    assert!(st.run_disabled_reason().is_some(), "an unbuilt strategy file cannot run");
    assert_eq!(st.run_disabled_reason(), st.run_blocked_reason());
}

/// The toolbar's "Server:" is the desktop's server name when its address names the backtest server's
/// box: the same host, whatever the port; on a loopback host (an ssh tunnel, which may lead to any box)
/// the same port too. Else the address itself.
#[test]
fn the_server_is_named_by_the_desktops_server_when_the_hosts_agree() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.backend = StudioRunBackend::Source { address: "127.0.0.1:8810".to_string() };
    assert_eq!(st.server_name(), "127.0.0.1:8810", "no desktop server: the address");
    st.desktop_server = Some(("the CI box".to_string(), "127.0.0.1:9097".to_string()));
    assert_eq!(st.server_name(), "127.0.0.1:8810", "two loopback tunnels may be two boxes");
    st.desktop_server = Some(("the CI box".to_string(), "127.0.0.1:8810".to_string()));
    assert_eq!(st.server_name(), "the CI box", "one tunnel is one box");
    st.desktop_server = Some(("the latency box".to_string(), "<host>:9097".to_string()));
    assert_eq!(st.server_name(), "127.0.0.1:8810", "another host is another box");
    st.backend = StudioRunBackend::Source { address: "<host>:7880".to_string() };
    assert_eq!(st.server_name(), "the latency box", "a box's own host names it, whatever the port");
    assert_eq!(st.server_check, None, "nothing was asked: the Studio dials nothing by itself");
}

/// The side panels give way so the results keep [`studio::RESULTS_MIN_WIDTH`]: both keep their widths
/// while they fit, then shrink by one factor down to [`studio::SIDE_PANEL_MIN_WIDTH`], then the editor
/// folds to its strip and the settings panel takes what is left.
#[test]
fn the_side_panels_give_way_to_the_results() {
    use super::super::shell::fit_side_panels;
    use vike_ui_theme::value::studio;
    let (editor, tools) = (Some(studio::EDITOR_DEFAULT_WIDTH), Some(studio::TOOLS_PANEL_WIDTH));
    assert_eq!(fit_side_panels(1440.0, editor, tools), (editor, tools), "both fit");
    let (Some(e), Some(t)) = fit_side_panels(1250.0, editor, tools) else {
        panic!("both still show at 1250");
    };
    assert!(e < studio::EDITOR_DEFAULT_WIDTH && t < studio::TOOLS_PANEL_WIDTH, "{e} {t}");
    assert!(t >= studio::SIDE_PANEL_MIN_WIDTH && e >= studio::SIDE_PANEL_MIN_WIDTH, "{e} {t}");
    let results = 1250.0 - studio::RAIL_WIDTH - e - t;
    assert!((results - studio::RESULTS_MIN_WIDTH).abs() < 0.01, "the results keep {results}");
    // The owner's Studio, about 1420 pixels at 1.5 pixels per point: the editor folds.
    assert_eq!(fit_side_panels(943.0, editor, tools), (None, tools));
    assert_eq!(fit_side_panels(943.0, None, tools), (None, tools), "hidden by the user: the same");
    let rest = 1000.0 - studio::RAIL_WIDTH - studio::RESULTS_MIN_WIDTH - studio::EDITOR_MIN_WIDTH;
    assert_eq!(
        fit_side_panels(1000.0, editor, None),
        (Some(rest), None),
        "the editor alone shrinks"
    );
    // Alone it never folds (the strip's arrow hid the settings panel to show it): at its floor.
    let floor = Some(studio::SIDE_PANEL_MIN_WIDTH);
    assert_eq!(fit_side_panels(560.0, editor, None), (floor, None), "the Studio's opening size");
}

/// The owner's desktop (2026-11-21): ONE ssh tunnel to the CI box, recorded by `vike-cli backend connect`,
/// forwards the tradehub's, the datahub's and the backtest server's ports to 127.0.0.1. The tradehub
/// and the backtest server answer at two mouths of it, so they are one box and the toolbar names it; a
/// loopback port the tunnel does not forward is another tunnel, maybe to another box. (The ports here
/// are ones no hub binds: these addresses are only parsed, never dialled.)
#[test]
fn two_mouths_of_the_one_recorded_tunnel_are_one_box() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.backend = StudioRunBackend::Source { address: "127.0.0.1:18880".to_string() };
    st.desktop_server = Some(("the CI box".to_string(), "127.0.0.1:18879".to_string()));
    assert_eq!(
        st.server_name(),
        "127.0.0.1:18880",
        "no tunnel recorded: two ports, maybe two boxes"
    );
    st.desktop_tunnel_ports = vec![18879, 18878, 18880];
    assert_eq!(st.server_name(), "the CI box", "both mouths of the one tunnel");
    st.backend = StudioRunBackend::Source { address: "localhost:18880".to_string() };
    assert_eq!(st.server_name(), "the CI box", "localhost is the same mouth");
    // A second tunnel, raised by hand to another box: its mouth is not one the record forwards.
    st.backend = StudioRunBackend::Source { address: "127.0.0.1:17880".to_string() };
    assert_eq!(st.server_name(), "127.0.0.1:17880", "another tunnel is not the desktop's box");
    st.backend = StudioRunBackend::Source { address: "127.0.0.1:18880".to_string() };
    st.desktop_server = Some(("the latency box".to_string(), "127.0.0.1:9097".to_string()));
    assert_eq!(st.server_name(), "127.0.0.1:18880", "the desktop on another tunnel: not its name");
    st.desktop_server = Some(("the latency box".to_string(), "<host>:18879".to_string()));
    assert_eq!(st.server_name(), "127.0.0.1:18880", "a host and a tunnel mouth are not one box");
}

/// Run says why while From/To are no window, and runs nothing over the whole series instead.
#[test]
fn run_says_why_a_window_that_does_not_parse_runs_nothing() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    st.picker.from = "2026-13-01".to_string();
    assert!(st.run_disabled_reason().is_some_and(|why| why.starts_with("From is not a date")));
    assert!(st.picker.selected().is_none());
}

#[test]
fn run_asks_for_a_slice_over_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let st = state_new(&dir, Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap()));
    assert_eq!(st.run_disabled_reason(), Some(PICK_A_SLICE));
}

#[test]
fn sweep_and_walk_forward_say_why_they_are_disabled() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    assert_eq!(st.sweep_disabled_reason(&[6]), None);
    assert_eq!(st.sweep_disabled_reason(&[]), Some(SEED_A_GRID));
    assert_eq!(st.walkforward_disabled_reason(), None);
    let (_sweep_tx, sweep_rx) = std::sync::mpsc::channel();
    st.sweep_rx = Some(sweep_rx);
    assert_eq!(st.sweep_disabled_reason(&[6]), Some(SWEEP_BUSY));
    let (_walkforward_tx, walkforward_rx) = std::sync::mpsc::channel();
    st.walkforward_rx = Some(walkforward_rx);
    assert_eq!(st.walkforward_disabled_reason(), Some(WALKFORWARD_BUSY));
}

/// A scan past the points one scan may run (the server's cap too) is refused at the button and inside
/// the dispatcher: two rows of `1..5000` are 25 million points.
#[test]
fn a_scan_past_the_points_cap_is_refused_before_it_is_sent() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    let cap = vike_backtesthub_client::summary::PARAMSCAN_POINTS_MAX;
    assert_eq!(st.sweep_disabled_reason(&[cap]), None, "the cap itself may run");
    assert_eq!(st.sweep_disabled_reason(&[cap, 2]), Some(SCAN_TOO_LARGE));
    st.strategy_source = StudioStrategySource::Rhai;
    let scanned = |name: &str| StudioParamRow::scanned(name, "1..5000");
    st.grid = vec![scanned("fast"), scanned("slow")];
    st.start_sweep();
    assert!(st.sweep_rx.is_none(), "nothing was sent");
    assert!(matches!(&st.sweep_last, Some(Err(e)) if e.to_string().contains(SCAN_TOO_LARGE)));
    st.walkforward_search = true;
    let why = st.walkforward_disabled_reason();
    assert_eq!(why, Some(SCAN_TOO_LARGE), "a searching walk sends the grid");
    st.walkforward_search = false;
    assert_eq!(st.walkforward_disabled_reason(), None, "a fixed walk sends none");
}

/// A Value is one number: a list or a range typed there would join a Rhai scan as an axis its size line
/// does not count, so the scan and the walk-forward refuse it and name the row.
#[test]
fn a_value_that_reads_to_more_than_one_value_is_refused() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    st.strategy_source = StudioStrategySource::Rhai;
    st.grid = vec![
        StudioParamRow::scanned("fast", "5..15 step 5"),
        StudioParamRow { name: "slow".into(), value: "20, 30".into(), ..Default::default() },
    ];
    assert_eq!(st.multi_value_held().as_deref(), Some("slow"));
    assert_eq!(st.sweep_disabled_reason(&[3]), Some(HELD_VALUE_IS_ONE));
    assert_eq!(st.walkforward_disabled_reason(), Some(HELD_VALUE_IS_ONE));
    st.grid[1].value = "1..3".into();
    assert_eq!(st.multi_value_held().as_deref(), Some("slow"), "a range is more than one value");
    st.grid[1].value = "25".into();
    assert_eq!(st.multi_value_held(), None);
    assert_eq!(st.sweep_disabled_reason(&[3]), None);
}

/// A study's window is the picked series' From/To, refused in the picker's words when it does not
/// parse — never the whole store (Run refuses the same way).
#[test]
fn a_study_refuses_a_window_that_does_not_parse() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    assert_eq!(st.study_window(), Ok(TimestampRange::all()), "no series picked: the whole store");
    st.picker.select(0);
    st.picker.from = "2026-13-01".to_string();
    let why = st.study_window().expect_err("a From that is no date");
    assert!(why.starts_with("From is not a date"), "{why}");
    assert_eq!(st.study_window_label(), why, "the Research pane says it too");
    st.backend = StudioRunBackend::Source { address: "127.0.0.1:1".to_string() };
    st.start_study();
    assert!(st.study_rx.is_none(), "nothing was sent");
    assert!(
        matches!(&st.study_last, Some(Err(e)) if e == why),
        "the refusal is the study's answer"
    );
}

/// The starting capital reaches no run on the Named backend (its request has no field for it), and the
/// pane says so rather than letting the field look honoured.
#[test]
fn the_named_backend_says_the_capital_does_not_reach_it() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.conditions.capital = "25000".to_string();
    assert_eq!(st.capital_not_sent(), None);
    st.backend = StudioRunBackend::Named { address: "127.0.0.1:1".to_string() };
    assert_eq!(st.capital_not_sent(), Some(NAMED_BACKEND_CAPITAL));
    st.conditions.capital.clear();
    assert_eq!(st.capital_not_sent(), None, "nothing set, nothing to say");
}

/// The keyless copy names the verb that writes the key and no longer a `.env` file: the credential
/// FILE plane was removed on 2026-10-07, so a user who followed the old sentence edited a file
/// nothing reads. Both provider names stay in it, and the value is told to travel on stdin.
#[test]
fn the_keyless_copy_points_at_secrets_set_not_an_env_file() {
    let copy = NO_PROVIDER_KEY;
    assert!(copy.contains("vike-cli secrets set ANTHROPIC_API_KEY"), "{copy}");
    assert!(copy.contains("(or CEREBRAS_API_KEY;"), "{copy}");
    assert!(copy.contains("stdin"), "{copy}");
    assert!(!copy.contains(".env"), "{copy}");
}

#[test]
fn send_says_why_it_is_disabled() {
    let (dir, store) = seeded_store();
    let keyless = state_new(&dir, store.clone());
    assert_eq!(keyless.send_disabled_reason(), Some(NO_PROVIDER_KEY));
    let keys = StudioChatApiKeys { anthropic: Some("not-a-real-key".to_string()), cerebras: None };
    let mut st = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        keys,
        None,
        StudioAutorun::Off,
        StudioWorkspace::default(),
    );
    st.picker.select(0);
    assert_eq!(st.send_disabled_reason(), Some(NOTHING_TO_SEND));
    st.chat.input = "RSI mean reversion".to_string();
    assert_eq!(st.send_disabled_reason(), None);
}

/// Switching backends keeps the address the operator typed — both dial the same backtesthub —
/// and a switch onto Named drops the roster, which belongs to the daemon that answered it. The
/// segmented control's one action; the two buttons it replaces each spelled half of this.
#[test]
fn switching_backends_keeps_the_address_and_drops_a_stale_roster() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.backend = StudioRunBackend::Source { address: "127.0.0.2:7999".to_string() };
    st.switch_backend(StudioRunBackendKind::Named);
    assert_eq!(st.backend, StudioRunBackend::Named { address: "127.0.0.2:7999".to_string() });
    st.named_roster = Some(Err("stale".to_string()));
    st.switch_backend(StudioRunBackendKind::Source);
    assert_eq!(st.backend, StudioRunBackend::Source { address: "127.0.0.2:7999".to_string() });
    st.switch_backend(StudioRunBackendKind::Named);
    assert!(st.named_roster.is_none(), "a roster belongs to the daemon that answered it");
}

/// **The params panel's two cost rows** (the owner's ruling of 2026-11-18): commission and slippage are
/// strategy params, so the strategy-file panel always shows `commission` and `slippage`, each one row of the
/// params a Run sends and a save keeps; a blank value is no row (the strategy's own applies). The Run
/// itself carries no cost field: the backend calls take none.
#[test]
fn the_params_panel_shows_the_two_cost_params_and_a_run_sends_them_as_params() {
    assert_eq!(STUDIO_COST_ROWS.map(|(key, _)| key), ["commission", "slippage"]);
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("size".into(), "2".into())];
    assert_eq!(cost_row_value(&st.file_params, "commission"), "", "blank: the strategy's own");
    set_cost_row(&mut st.file_params, "commission", "0.1");
    set_cost_row(&mut st.file_params, "slippage", "2");
    set_cost_row(&mut st.file_params, "slippage", "1");
    assert_eq!(cost_row_value(&st.file_params, "slippage"), "1", "edited in place");
    match st.current_spec() {
        StrategySpec::File { params, .. } => {
            assert_eq!(params.get("commission").and_then(|v| v.as_f64()), Some(0.1));
            assert_eq!(params.get("slippage").and_then(|v| v.as_f64()), Some(1.0));
            assert_eq!(params.get("size").and_then(|v| v.as_integer()), Some(2));
        }
        other => panic!("expected a strategy file spec, got {other:?}"),
    }
    set_cost_row(&mut st.file_params, "commission", " ");
    assert_eq!(st.file_params.len(), 2, "a blank row is removed: {:?}", st.file_params);
    assert_eq!(st.workspace_snapshot().file_params, st.file_params, "saved with the params");
}
