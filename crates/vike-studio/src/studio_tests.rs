use super::*;
use std::sync::Arc;
use vike_data::{DataFusionHist, HistStore};
use vike_model::Bar;

/// `StudioState::new` with the pairing vike-desktop passes for a LOCAL store: `state_dir` = the
/// store's own root (here the test's temp dir), keeping the old `store.root()` colocations
/// byte-identical. The `Arc<DataFusionHist>` → `StoreHandle` coercion happens at the call.
///
/// ⚠ Over a DEFAULT workspace rather than through `new`'s read, and that is hermeticity, not
/// style. `new` reads `workspace_read_path`, which prefers `<project>/settings/state` found by
/// walking up from the working directory — under `cargo test`, the CHECKOUT's own settings
/// root. Since the strategy source is persisted there, a box whose developer last left the
/// Studio in Plugin mode would start every test here in Plugin mode. Everything else this
/// constructor does (the store walk, the saved list under `state_dir`) is unchanged.
fn state_new(dir: &tempfile::TempDir, store: Arc<DataFusionHist>) -> StudioState {
    StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        ChatApiKeys::default(),
        None,
        QaAutorun::Off,
        StudioWorkspace::default(),
    )
}

/// Bind an ephemeral loopback listener and serve the COMPUTE verbs over `store` on a
/// detached thread; return the address a `Backend::Remote` should dial.
///
/// ⚠ **This exists because `Backend::Local` was deleted**
/// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`). A Studio run
/// leaves the process now, so a test that wants a RESULT needs something to answer - and the
/// DEFAULT address is a real deployed daemon's. Without this the test dialled
/// `127.0.0.1:7880` and, on the CI box where that daemon runs, got its AUTH REFUSAL back: a
/// unit test reaching a live service and reporting the rejection as its own failure.
///
/// The server is `vike_backtest::compute_server::serve_with_studio`, the same one
/// `crates/vike-studio-core/tests/studio_wire_parity.rs` drives, with no node keys - so it is
/// unauthenticated, hermetic, and bound to port 0. This is STRICTLY more coverage than the
/// in-process path it replaces: the run is now marshalled over the real wire.
fn spawn_compute_server(store: Arc<DataFusionHist>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    let handle: Arc<dyn HistStore + Send + Sync> = store;
    std::thread::spawn(move || {
        let _ = vike_backtest::compute_server::serve_with_studio(
            listener,
            handle,
            Some(vike_studio_core::studio_run_table()),
        );
    });
    addr
}

/// `next_tab` cycles through every `RightTab::ALL` entry in display order and wraps from
/// the last tab back to the first — the pure helper behind the Ctrl/Cmd+/ shortcut.
#[test]
fn next_tab_cycles_through_every_tab_and_wraps() {
    let start = RightTab::ALL[0];
    let mut t = start;
    let mut seen = vec![t];
    for _ in 0..RightTab::ALL.len() - 1 {
        t = next_tab(t);
        seen.push(t);
    }
    assert_eq!(seen, RightTab::ALL.to_vec(), "should visit every tab in display order");
    assert_eq!(next_tab(t), start, "the last tab should wrap back to the first");
}

#[test]
fn right_tab_default_is_sweep_and_every_tab_has_a_label() {
    assert_eq!(RightTab::default(), RightTab::Sweep);
    assert_eq!(RightTab::ALL.len(), 7);
    for tab in RightTab::ALL {
        assert!(!tab.label().is_empty());
    }
    // Labels are distinct (no two tabs share a caption).
    let labels: Vec<&str> = RightTab::ALL.iter().map(|t| t.label()).collect();
    let mut uniq = labels.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), labels.len());
}

/// `from_qa_str` (the `VIKE_STUDIO_TAB` capture hook) round-trips every tab by its lowercase
/// name and rejects garbage rather than panicking.
#[test]
fn from_qa_str_parses_every_tab_and_rejects_garbage() {
    let names = ["sweep", "strategy", "data", "indicators", "saved", "research", "chat"];
    for (name, want) in names.iter().zip(RightTab::ALL) {
        assert_eq!(RightTab::from_qa_str(name), Some(want));
    }
    assert_eq!(RightTab::from_qa_str(""), None);
    assert_eq!(RightTab::from_qa_str("Sweep"), None, "exact lowercase only");
    assert_eq!(RightTab::from_qa_str("nonsense"), None);
}

/// A temp-dir-backed store seeded with ~400 oscillating 1m bars for `binance/BTCUSDT`, so the
/// default SMA-crossover editor script has something to cross on. Returns the `TempDir` so the
/// caller binds it and keeps the backing directory alive for the test's lifetime (the fragile
/// `std::mem::forget` + shared-temp-path approach from the original plan is deliberately not
/// used here — see Task 2's `run.rs::tests::seeded_store` for the pattern this mirrors).
fn seeded_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            let c = 100.0 + (i % 7) as f64;
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
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    (dir, Arc::new(store))
}

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
    assert!(matches!(st.rhai_verdict(), Some(Err(_))));

    st.editor.source = crate::editor::EditorPane::default().source;
    st.poll();
    assert_eq!(st.rhai_verdict(), Some(&Ok(())));
}

/// **The Rhai check never runs on a Plugin's Rust buffer, and no verdict survives a mode
/// switch in either direction.** The state-machine half of
/// `crates/vike-studio/tests/studio_shell_render.rs`'s
/// `a_plugin_buffer_is_not_judged_as_rhai_and_the_same_buffer_in_rhai_mode_is`, which asserts
/// what the header RENDERS; this one asserts what `poll` HOLDS, including across two polls in
/// a row (the per-frame case).
///
/// The Rhai leg comes FIRST and holds a real `Err` for this exact source, so the Plugin leg's
/// `None` is a verdict being DROPPED rather than one that was never computed — and the last
/// leg flips back with the source unchanged, so only a re-check (not a drifted-source recompile)
/// can put a verdict back.
#[test]
fn plugin_mode_holds_no_rhai_verdict_and_rhai_mode_rechecks_on_return() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.editor.source = "use vike_model::Strategy;\npub fn build() {}\n".to_string();
    st.poll();
    assert!(
        matches!(st.rhai_verdict(), Some(Err(_))),
        "the control: Rust source IS a Rhai error while the buffer is Rhai"
    );

    st.strategy_source = StrategySource::Plugin;
    assert!(!st.buffer_is_rhai());
    st.poll();
    assert_eq!(st.rhai_verdict(), None, "Plugin mode must hold no Rhai verdict");
    st.poll();
    assert!(st.rhai_check.is_none(), "...on every frame, not only the first after the switch");

    st.strategy_source = StrategySource::Rhai;
    st.poll();
    match &st.rhai_check {
        Some((checked, Err(_))) => assert_eq!(checked, &st.editor.source),
        other => panic!("back in Rhai mode the buffer must be re-checked: {other:?}"),
    }

    // Native keeps the check: its buffer is the parked Rhai script (`buffer_is_rhai`'s doc).
    st.strategy_source = StrategySource::Native;
    assert!(st.buffer_is_rhai());
    st.poll();
    assert!(matches!(st.rhai_verdict(), Some(Err(_))));
}

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

/// The parse table, INCLUDING the backwards-compatibility row that matters most: `"1"` must
/// keep meaning one backtest, because every capture script and dev shell in the tree already
/// spells it that way and the read it replaced was `== Ok("1")`.
#[test]
fn the_autorun_knob_parses_its_two_spellings_and_ignores_everything_else() {
    assert_eq!(QaAutorun::from_qa_str(Some("1")), QaAutorun::Run);
    assert_eq!(QaAutorun::from_qa_str(Some("sweep")), QaAutorun::Sweep);
    for garbage in [None, Some(""), Some("0"), Some("true"), Some("Sweep"), Some("run")] {
        assert_eq!(
            QaAutorun::from_qa_str(garbage),
            QaAutorun::Off,
            "{garbage:?} must be inert, never a surprise run"
        );
    }
    assert_eq!(QaAutorun::default(), QaAutorun::Off);
}

#[test]
fn the_sweep_ladder_widens_a_seeded_default_into_three_candidates() {
    assert_eq!(qa_sweep_ladder("10"), "5, 10, 15");
    assert_eq!(qa_sweep_ladder(" 4 "), "2, 4, 6");
    // Rungs come out ASCENDING, so a negative default reads low-to-high like every other axis.
    assert_eq!(qa_sweep_ladder("-8"), "-12, -8, -4");
    // A whole default stays whole — see the rounding comment for why a fractional lookback is
    // a lie rather than a nicety. 5 would otherwise widen to `2.5, 5, 7.5`.
    assert_eq!(qa_sweep_ladder("5"), "3, 5, 8");
    assert_eq!(qa_sweep_ladder("20"), "10, 20, 30");
    // ...and a fractional default is left fractional, because nothing is being misreported.
    assert_eq!(qa_sweep_ladder("2.5"), "1.25, 2.5, 3.75");
}

/// The pass-through arms — see [`qa_sweep_ladder`]'s doc for why each would produce a
/// degenerate axis rather than a wider one.
#[test]
fn the_sweep_ladder_leaves_a_value_it_cannot_widen_alone() {
    for v in ["0", "BTCUSDT", "", "nan", "inf"] {
        assert_eq!(qa_sweep_ladder(v), v, "{v:?} must pass through unchanged");
    }
}

/// Every ladder must still parse back through the CSV rule `start_sweep` applies, or the
/// widening would hand the sweep an empty grid and it would return early having done nothing —
/// the exact empty-frame failure this hook exists to remove. Driven through the real parse
/// rather than a restatement of it.
#[test]
fn a_widened_row_still_parses_as_a_sweep_axis() {
    let axis: Vec<f64> =
        qa_sweep_ladder("10").split(',').filter_map(|s| s.trim().parse().ok()).collect();
    assert_eq!(axis, vec![5.0, 10.0, 15.0]);
    assert!(axis.len() > 1, "a one-candidate axis is the single-combination sweep again");
}

/// The autorun is spent on the first frame WHATEVER it was, INCLUDING an arm that could not
/// start anything — a flag left armed fires a surprise run minutes later, the moment
/// `picker.refresh` auto-selects row 0 as data appears (the review finding the original `bool`
/// hook records, and the reason the disarm is a `replace` ahead of the ladder rather than an
/// assignment inside each arm).
///
/// Driven over an EMPTY store, which is what makes it the could-not-act case: `new_with_qa`
/// populates the picker synchronously at construction, so a SEEDED store already has row 0
/// selected and both arms would dispatch. Same idiom as `an_empty_store_leaves_no_slice`.
#[test]
fn every_autorun_arm_disarms_itself_even_when_it_could_not_act() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap()); // no series -> no selection
    for arm in [QaAutorun::Run, QaAutorun::Sweep] {
        let mut st = state_new(&dir, store.clone());
        assert!(st.picker.selected().is_none(), "the empty-store premise broke");
        st.qa_autorun = arm;
        st.take_qa_autorun();
        assert_eq!(st.qa_autorun, QaAutorun::Off, "{arm:?} left itself armed");
        assert!(!st.running, "{arm:?} dispatched with nothing selected");
        assert!(st.sweep_rx.is_none(), "{arm:?} dispatched with nothing selected");
    }
}

/// ...and the twin: over a POPULATED store both arms DO act, so the test above is proving a
/// disarm rather than an inert code path.
#[test]
fn both_autorun_arms_dispatch_when_a_slice_is_selectable() {
    let (_dir, store) = seeded_store();
    let mut run = state_new(&_dir, store.clone());
    run.qa_autorun = QaAutorun::Run;
    run.take_qa_autorun();
    assert!(run.running, "the Run arm did not start a backtest");

    let mut sweep = state_new(&_dir, store);
    sweep.qa_autorun = QaAutorun::Sweep;
    sweep.take_qa_autorun();
    assert!(sweep.sweep_rx.is_some(), "the Sweep arm did not start a sweep");
}

/// The Studio's ONE annualization derivation, over the two states a fixture can reach.
///
/// The seeded store is 1m bars — the interval the defect was worst on (a bare `252.0`
/// understated its Sharpe by `sqrt(1440) ≈ 37.9x`) and the one the CI roundtrip fixture uses.
/// `new_with_qa` populates the picker synchronously, so row 0 is already selected here; the
/// empty store is the app's own starting state, where nothing is.
///
/// The THIRD state — a tick slice — takes the same arm as "nothing picked" by construction
/// (`SeriesRow::interval` is `None` for both), so it is covered by the match rather than by a
/// fixture: seeding a quote/trade series here would exercise the picker's tick listing, not
/// this function's branch.
#[test]
fn the_display_factor_follows_the_picked_slice_and_falls_back_honestly() {
    let (_dir, store) = seeded_store();
    let picked = state_new(&_dir, store);
    assert_eq!(
        picked.picker.selected_row().and_then(|r| r.interval.as_deref()),
        Some("1m"),
        "the seeded-store premise broke — without a 1m pick this proves nothing",
    );
    assert_eq!(picked.display_periods_per_year(), periods_per_year_for_interval("1m"));
    assert_ne!(
        picked.display_periods_per_year(),
        DEFAULT_PERIODS_PER_YEAR,
        "a 1m slice annualized on the daily anchor IS the bug this derivation removes",
    );

    let dir = tempfile::tempdir().unwrap();
    let empty = Arc::new(DataFusionHist::open(dir.path()).unwrap()); // no series -> no pick
    let unpicked = state_new(&dir, empty);
    assert!(unpicked.picker.selected_row().is_none(), "the empty-store premise broke");
    assert_eq!(
        unpicked.display_periods_per_year(),
        DEFAULT_PERIODS_PER_YEAR,
        "with nothing picked the display keeps the anchor it has always had",
    );
}

/// The sweep arm's own contract: it must SEED the grid (a fresh workspace has an empty one and
/// `start_sweep` returns early on that) and widen what it seeded.
#[test]
fn the_sweep_autorun_seeds_and_widens_the_grid_before_dispatching() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.picker.refresh(st.store.as_ref());
    st.picker.select(0);
    assert!(st.grid.is_empty(), "a fresh state must start with the empty grid");
    st.qa_autorun = QaAutorun::Sweep;
    st.take_qa_autorun();
    assert!(!st.grid.is_empty(), "the sweep autorun must seed the grid it is about to sweep");
    for (name, csv) in &st.grid {
        let n = csv.split(',').filter(|s| s.trim().parse::<f64>().is_ok()).count();
        assert!(n > 1, "{name}: a one-candidate axis is the single-combination sweep ({csv:?})");
    }
    assert!(st.sweep_rx.is_some(), "the sweep autorun must actually dispatch a sweep");
}

/// ⚠ THE TEST THAT MAKES THE SWEEP POSE REAL. Everything above could pass while the capture
/// still rendered an empty form, because the whole chain hangs off one property of the SOURCE:
/// `discover_params` reports what `param()` declared, and the shipped `DEFAULT_SCRIPT`
/// declares its lookbacks with `const`. Driven through the REAL discovery rather than by
/// eyeballing the string, and it asserts the contrast in BOTH directions so the day somebody
/// makes the default sweepable this test says the swap is no longer needed.
#[test]
fn the_sweep_capture_script_declares_params_and_the_shipped_default_does_not() {
    let found = vike_script::discover_params(crate::editor::SWEEP_CAPTURE_SCRIPT)
        .expect("the capture script must COMPILE — a sweep over it runs the real engine");
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["fast", "slow"], "the swept axes drifted from the script");
    assert_eq!(found[0].1, 5.0, "the mid rung must reproduce DEFAULT_SCRIPT's own lookback");
    assert_eq!(found[1].1, 20.0);

    let default_params = vike_script::discover_params(&EditorPane::default().source)
        .expect("the shipped default must still compile");
    assert!(
        default_params.is_empty(),
        "DEFAULT_SCRIPT now declares params ({default_params:?}) — the capture script's whole \
             reason to exist was that it did not, so re-read editor.rs's SWEEP_CAPTURE_SCRIPT doc \
             and consider dropping the swap"
    );
}

#[test]
fn start_run_then_poll_reaches_a_result() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.picker.refresh(st.store.as_ref());
    st.picker.select(0);
    st.start_run();
    assert!(st.running);
    // block until the worker delivers, then poll folds it in
    for _ in 0..200 {
        st.poll();
        if !st.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!st.running, "run should complete");
    assert!(matches!(st.last, Some(Ok(_)) | Some(Err(_))));
}

/// Spec §7 regression: if the worker's sender is dropped without ever sending (the
/// worker-thread-panic case, simulated here directly), `poll()` must observe the disconnect
/// and clear `running` rather than treat `Disconnected` the same as `Empty` forever — the
/// bug that would leave the spinner stuck and Run disabled for the process lifetime.
#[test]
fn poll_clears_running_when_worker_disconnects_without_sending() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let (tx, rx) = std::sync::mpsc::channel::<RunOutcome>();
    drop(tx);
    st.run_rx = Some(rx);
    st.running = true;

    st.poll();

    assert!(!st.running, "poll() must clear running on a disconnected channel");
    assert!(matches!(st.last, Some(Err(_))), "disconnect should surface as a run failure");
}

/// The ⟳ Refresh latch: a second click while a walk is in flight must not spawn a second walk
/// (`spawn_catalog_refresh`'s early return).
///
/// Proved by SUBSTITUTION rather than by counting threads: a planted receiver whose sender the
/// test still holds stands in for the in-flight walk, and a distinctive planted answer is then
/// pushed through it. If the second click had replaced `catalog_rx`, that send would go
/// nowhere and `poll` would fold the real store's lists instead.
#[test]
fn a_second_refresh_while_a_walk_is_in_flight_does_not_spawn_another() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let ctx = egui::Context::default();

    let (tx, rx) = std::sync::mpsc::channel::<crate::catalog::CatalogLoad>();
    st.catalog_rx = Some(rx);
    st.spawn_catalog_refresh(&ctx);

    tx.send(crate::catalog::CatalogLoad {
        series: Err("planted answer".to_string()),
        inventory: Err("planted answer".to_string()),
    })
    .expect("the planted receiver must still be the one the shell holds");
    st.poll();

    assert_eq!(
        st.picker.error(),
        Some("planted answer"),
        "the in-flight receiver must survive a second click"
    );
    assert!(st.catalog_rx.is_none(), "...and the delivered walk clears the latch");
}

/// The ⟳ Refresh walk, end to end on a real worker thread: spawn, wait, fold — and the picker
/// ends up holding exactly what the store holds.
///
/// The wait is a bounded sleep-poll over `poll()`, following this file's other worker tests
/// (`start_run_then_poll_reaches_a_result` and its siblings) rather than `catalog.rs`'s
/// `recv()`: the shell owns the receiver, and the only way to consume it is the per-frame
/// `poll` the real UI calls — a `recv` here would test a channel, not the shell.
#[test]
fn a_spawned_catalog_refresh_populates_the_picker_through_poll() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    // Start from a picker that is empty for a reason the fold must overwrite, so a passing
    // assertion cannot be the constructor's own walk still standing.
    st.picker.apply(Err("cleared before the refresh".to_string()));
    assert!(st.picker.available().is_empty());

    let ctx = egui::Context::default();
    st.spawn_catalog_refresh(&ctx);
    assert!(st.catalog_scanning(), "the button is disabled while the walk is owed an answer");
    for _ in 0..200 {
        st.poll();
        if !st.catalog_scanning() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(!st.catalog_scanning(), "the walk must land within the wait");
    assert!(st.picker.error().is_none(), "a seeded store must not read as a scan failure");
    assert_eq!(st.picker.available().len(), 1, "the one seeded bar series");
}

/// A catalog worker that dies without sending is a terminal failure in BOTH panes — never a
/// silently-kept previous list, which would claim the refresh found the store unchanged.
#[test]
fn poll_surfaces_a_dead_catalog_worker_in_both_panes() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let (tx, rx) = std::sync::mpsc::channel::<crate::catalog::CatalogLoad>();
    drop(tx);
    st.catalog_rx = Some(rx);

    st.poll();

    assert_eq!(st.picker.error(), Some(crate::catalog::CATALOG_WALK_LOST));
    assert_eq!(st.data_browser.error(), Some(crate::catalog::CATALOG_WALK_LOST));
    assert!(!st.catalog_scanning(), "a disconnect clears the latch — Refresh works again");
}

/// Mirrors `poll_clears_running_when_worker_disconnects_without_sending` for the `chat_rx`
/// arm: `ChatOutcome`'s doc claims a worker panic surfaces as a human-readable failure, same
/// as the run/sweep/walk-forward arms do. Before this fix the disconnect arm cleared
/// `chat.running`/`chat_rx` but never pushed anything to the transcript, silently swallowing
/// the failure -- making the doc false.
#[test]
fn poll_surfaces_chat_worker_disconnect_in_the_transcript() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let (tx, rx) = std::sync::mpsc::channel::<ChatOutcome>();
    drop(tx);
    st.chat_rx = Some(rx);
    st.chat.running = true;

    st.poll();

    assert!(!st.chat.running, "poll() must clear chat.running on a disconnected channel");
    let (role, text) = st.chat.history.last().expect("a transcript line must be pushed");
    assert_eq!(role, "assistant");
    assert!(
        text.to_lowercase().contains("terminated") || text.to_lowercase().contains("failed"),
        "transcript line should read as a human-readable worker failure, got: {text}"
    );
}

#[test]
fn seed_grid_from_discovered_params() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = "let fast = param(\"fast\", 5.0);\nfn on_bar() {}".to_string();
    st.seed_sweep_grid(); // discover_params -> grid rows
    assert_eq!(st.grid.len(), 1);
    assert_eq!(st.grid[0].0, "fast");
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

/// `cancel()` drops the in-flight receiver(s) and resets `running` so a fresh Run/Sweep/
/// Compare can start immediately — the MVP "abandon" contract (the spawned worker thread
/// keeps running to completion, but its result is discarded because nothing polls the
/// receiver anymore, matching every other disconnect path in this file).
#[test]
fn cancel_abandons_every_in_flight_worker_and_resets_state() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.picker.select(0);
    st.start_run();
    assert!(st.running);
    assert!(st.any_running());

    st.cancel();

    assert!(!st.running, "cancel should clear the running flag");
    assert!(st.run_rx.is_none(), "cancel should drop the receiver");
    assert!(!st.any_running());
    assert!(st.last.is_none(), "cancel doesn't fabricate a result");

    // the editor/picker/store are all still usable — a fresh Run can start right away.
    st.start_run();
    assert!(st.running, "a new run should be startable immediately after cancel");
}

#[test]
fn any_running_is_false_when_idle_and_true_while_a_sweep_is_in_flight() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    assert!(!st.any_running());
    st.picker.select(0);
    st.grid = vec![("fast".to_string(), "3,5".to_string())];
    st.start_sweep();
    assert!(st.any_running(), "a running sweep should register as any_running");
    st.cancel();
    assert!(!st.any_running());
}

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

// ---- plugin strategies --------------------------------------------------------------------

/// Flipping to Plugin makes `current_spec` resolve a [`StrategySpec::Plugin`] naming the
/// declared plugin and its last-built sha — the editor buffer itself never joins the spec
/// (unlike Rhai): it is BUILT by the builder service (`start_plugin_build`) and only the
/// resulting sha crosses into a run.
#[test]
fn plugin_mode_makes_current_spec_resolve_a_plugin() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source =
        "pub fn build<B>(p: &toml::Value) -> Box<dyn Strategy<B>> { todo!() }".to_string();
    st.plugin_name = "my_strat".to_string();
    st.plugin_sha = Some("a".repeat(64));
    match st.current_spec() {
        StrategySpec::Plugin { name, sha, .. } => {
            assert_eq!(name, "my_strat");
            assert_eq!(sha.len(), 64);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// The ordering rule from the spec, as a test: Run must not be reachable while no sha is held,
/// because the server would be asked for an artifact that does not exist yet.
#[test]
fn run_is_refused_until_a_build_has_returned_a_sha() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_sha = None;
    assert!(st.run_blocked_reason().is_some(), "Run must be blocked with no sha");

    st.plugin_sha = Some("a".repeat(64));
    assert!(st.run_blocked_reason().is_none(), "a returned sha must clear the refusal");
}

/// **THE SEQUENCING CONTRACT, folded through the real `poll`.** A build's `Ok(sha)` arrives on
/// the same worker channel every other dispatch uses, and delivering it does exactly two
/// things: it sets the sha, and it records the source that sha was built FROM. Nothing here
/// dispatches a run — Run is what the operator presses next, and this delivery is what stops
/// `run_blocked_reason` refusing it.
#[test]
fn a_delivered_build_sha_unblocks_run_and_nothing_else_is_dispatched() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_name = "my_strat".to_string();
    st.editor.source = "// v1".to_string();
    assert!(st.run_blocked_reason().is_some(), "no sha yet");

    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    assert!(st.any_running(), "a build in flight is an in-flight worker like any other");
    st.poll();

    assert_eq!(st.plugin_sha.as_deref(), Some("c".repeat(64).as_str()));
    assert_eq!(st.plugin_built_source.as_deref(), Some("// v1"));
    assert!(st.run_blocked_reason().is_none(), "the sha must unblock Run");
    assert!(st.build_rx.is_none(), "the receiver is consumed");
    // ⚠ The half that would be easy to leave out: a build must NOT start a run.
    assert!(!st.running, "a build answers with a sha and dispatches nothing");
    assert!(st.run_rx.is_none(), "a build must not have queued a run");
    assert!(st.last.is_none(), "and must not have fabricated a result");
}

/// **The STALENESS guard.** Editing after a successful build re-blocks Run: the held sha names
/// the PREVIOUS artifact, so running would report on code the author already replaced — with
/// plausible numbers and no error, which is the worst shape a defect can take here.
#[test]
fn an_edit_after_a_build_blocks_run_until_it_is_rebuilt() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert!(st.run_blocked_reason().is_none());

    st.editor.source = "// v2".to_string();
    let reason = st.run_blocked_reason().expect("an edit must re-block Run");
    assert!(reason.contains("changed"), "{reason}");

    // Building again clears it, with the NEW source recorded.
    st.build_rx = answered_build("// v2", Ok("d".repeat(64)));
    st.poll();
    assert!(st.run_blocked_reason().is_none(), "a fresh build clears the staleness refusal");
    assert_eq!(st.plugin_sha.as_deref(), Some("d".repeat(64).as_str()));
}

/// A FAILED build must not leave the previous sha standing. The buffer the author is looking
/// at is the one that failed to compile, so an unchanged "built abc123" chip beside it would
/// invite a Run over the LAST artifact and report it as this code.
#[test]
fn a_failed_build_clears_the_sha_rather_than_leaving_a_stale_one() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert!(st.plugin_sha.is_some());

    st.editor.source = "// v2 (does not compile)".to_string();
    st.build_rx = answered_build(
        "// v2 (does not compile)",
        Err("error[E0425]: cannot find value `x`".to_string()),
    );
    st.poll();

    assert!(st.plugin_sha.is_none(), "a failed build must not leave the old sha standing");
    assert!(st.run_blocked_reason().is_some(), "and Run must be blocked again");
    match &st.build_last {
        // rustc's diagnostics reach the author VERBATIM — the design's `Err(<rustc
        // diagnostics as text>)`.
        Some(Err(e)) => assert!(e.contains("E0425"), "{e}"),
        other => panic!("expected the diagnostics, got {other:?}"),
    }
}

/// A sha RELOADED from a saved row is not blocked by staleness, because this shell never held
/// the source that artifact was built from — a saved row carries a name and a sha and no file.
/// Claiming a mismatch against an unrelated buffer would break the reload path to guard
/// against a drift nothing here can measure.
#[test]
fn a_reloaded_saved_sha_is_not_refused_as_stale() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();

    // What the saved-row arm does: name + sha in, provenance cleared.
    st.plugin_name = "reloaded".to_string();
    st.plugin_sha = Some("e".repeat(64));
    st.plugin_built_source = None;
    st.editor.source = "// something else entirely".to_string();
    assert!(
        st.run_blocked_reason().is_none(),
        "a reloaded row must run — its artifact's source was never in this buffer"
    );
}

/// A build in flight is not started twice: a second press would race the first, and whichever
/// finished last would decide the sha.
#[test]
fn a_second_build_press_while_one_is_in_flight_is_a_no_op() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_name = "my_strat".to_string();
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.start_plugin_build();
    st.poll();
    assert_eq!(
        st.plugin_sha.as_deref(),
        Some("c".repeat(64).as_str()),
        "the in-flight build's answer must survive a second press"
    );
}

/// A channel already carrying `v` — a delivered worker answer, with no thread and no timing.
fn ready<T>(v: T) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(v).expect("plant the answer");
    rx
}

/// A build that has already ANSWERED, planted as the dispatch would leave it: `sent` is the
/// source `dispatch_plugin_build` captured, `answer` the builder's reply. The tests that only
/// need a delivered answer plant it this way; the one that is ABOUT what gets captured
/// (`an_edit_made_while_the_build_runs_is_not_recorded_as_built`) drives the real dispatch.
fn answered_build(sent: &str, answer: Result<String, String>) -> Option<PendingBuild> {
    Some((sent.to_string(), ready(answer)))
}

/// **The staleness guard across the BUILD's own window — the defect the guard was blind to.**
/// The author presses Build on v1, keeps typing while cargo runs (a build takes minutes), and
/// the sha for v1 lands while the buffer holds v2. That sha names v1's artifact, so Run must stay
/// blocked with the staleness reason: running it would report v1's behaviour as v2's.
///
/// Driven through the REAL dispatch (`dispatch_plugin_build`, the body of
/// `start_plugin_build`) with a receiver whose sender this test holds, so the recorded source
/// is whatever the dispatch captured — not a value the test planted beside the answer. The
/// sent source is asserted too, so the test also pins WHAT was handed to the builder.
#[test]
fn an_edit_made_while_the_build_runs_is_not_recorded_as_built() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_name = "my_strat".to_string();
    st.editor.source = "// v1".to_string();

    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let mut handed_to_builder: Option<String> = None;
    st.dispatch_plugin_build(|_addr, _keys, _name, source| {
        handed_to_builder = Some(source);
        rx
    });
    assert_eq!(handed_to_builder.as_deref(), Some("// v1"), "the builder is sent the buffer");

    // The author keeps typing while the build runs; frames keep polling a pending build.
    st.editor.source = "// v2".to_string();
    st.poll();
    assert!(st.plugin_sha.is_none(), "nothing has answered yet");

    tx.send(Ok("c".repeat(64))).expect("the dispatch holds the receiver");
    st.poll();
    assert_eq!(st.plugin_sha.as_deref(), Some("c".repeat(64).as_str()), "the sha is accepted");
    assert_eq!(
        st.plugin_built_source.as_deref(),
        Some("// v1"),
        "the sha is the build of what was SENT, not of the buffer when it landed"
    );
    let reason =
        st.run_blocked_reason().expect("an edit made during the build must leave Run blocked");
    assert!(reason.contains("changed since the last Build"), "the STALENESS reason: {reason}");

    // The comparison is against the sent bytes exactly: undoing the edit unblocks Run.
    st.editor.source = "// v1".to_string();
    assert!(st.run_blocked_reason().is_none(), "the buffer is v1 again, which is what was built");
}

/// **No Rhai writer overwrites a Plugin's Rust buffer** — the write half of the four
/// template/copilot writers, which `rhai_writer_blocked_reason` argues are REFUSED in Plugin
/// mode rather than made to switch the source. What the buttons render is
/// `crates/vike-studio/tests/studio_shell_render.rs`'s
/// `every_rhai_writer_is_refused_over_a_plugin_buffer_and_armed_over_a_rhai_one`; this pins the
/// check INSIDE the write, which protects any caller that reaches it without a button.
///
/// The second half is the control: in Rhai and Native mode both writers DO write (and only the
/// template load re-baselines), so the refusal is keyed on the mode, not a writer that stopped
/// working.
#[test]
fn plugin_mode_refuses_every_rhai_writer_and_the_rhai_modes_do_not() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    let rust = "use vike_model::Strategy;\npub fn build() {}\n";
    let template = TEMPLATES[0].1;
    let copilot = vike_ai::AgentResult {
        code: "fn on_bar() {}".to_string(),
        accepted: true,
        ..Default::default()
    };

    st.strategy_source = StrategySource::Plugin;
    st.editor.source = rust.to_string();
    st.saved_source = rust.to_string();
    assert_eq!(st.rhai_writer_blocked_reason(), Some(RHAI_WRITER_BLOCKED_IN_PLUGIN));
    assert!(!st.load_template(template), "a template load must be refused over Rust source");
    assert!(!st.apply_copilot_result(&copilot), "...and so must the copilot's Apply");
    assert_eq!(st.editor.source, rust, "the Rust buffer must survive both");
    assert_eq!(st.saved_source, rust, "...and so must its unsaved baseline");
    assert_eq!(st.strategy_source, StrategySource::Plugin, "refused, not switched");

    for source in [StrategySource::Rhai, StrategySource::Native] {
        st.strategy_source = source;
        st.editor.source = "stale".to_string();
        assert_eq!(st.rhai_writer_blocked_reason(), None, "{source:?} buffers are Rhai");
        assert!(st.load_template(template), "{source:?}: a template must load");
        assert_eq!(st.editor.source, template);
        assert_eq!(st.saved_source, template, "{source:?}: a template load re-baselines");
        assert!(st.apply_copilot_result(&copilot), "{source:?}: the copilot must apply");
        assert_eq!(st.editor.source, copilot.code);
        assert_ne!(st.editor.source, st.saved_source, "{source:?}: Apply is an edit, not a load");
    }
}

/// **A fifth Rhai writer: loading a SAVED Rhai row over a Plugin buffer.** Found while fixing
/// the four template/copilot writers — `handle_saved_action`'s `Load` arm overwrites
/// `editor.source` for a Rhai row exactly like the template loader does, and was reachable
/// while Plugin held the only copy of a Rust strategy. Refused the same way: the whole action
/// no-ops rather than switching the mode or touching the buffer. A Native or Plugin row never
/// writes `editor.source`, so loading one is unaffected by this guard.
#[test]
fn loading_a_saved_rhai_row_is_refused_over_a_plugin_buffer_and_armed_over_a_rhai_one() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    let rust = "use vike_model::Strategy;\npub fn build() {}\n";
    st.saved
        .strategies
        .push(SavedStrategy::rhai("saved rhai".to_string(), "let x = 1;".to_string()));

    st.strategy_source = StrategySource::Plugin;
    st.editor.source = rust.to_string();
    st.saved_source = rust.to_string();
    st.handle_saved_action(SavedAction::Load(0));
    assert_eq!(st.editor.source, rust, "the Rust buffer must survive a saved-Rhai Load");
    assert_eq!(st.saved_source, rust, "...and so must its unsaved baseline");
    assert_eq!(st.strategy_source, StrategySource::Plugin, "refused, not switched");

    // The control: the same Load, over a Rhai (or Native) buffer with no Rust to lose, works.
    for source in [StrategySource::Rhai, StrategySource::Native] {
        st.strategy_source = source;
        st.editor.source = "stale".to_string();
        st.handle_saved_action(SavedAction::Load(0));
        assert_eq!(
            st.strategy_source,
            StrategySource::Rhai,
            "{source:?}: the row's own kind loads"
        );
        assert_eq!(st.editor.source, "let x = 1;", "{source:?}: a Rhai row's script loads");
    }
}

/// **A Plugin save re-baselines to what its sha was BUILT from, and the unsaved chip says what
/// Ctrl+S actually saves.** A Plugin row stores a name and a sha, never the buffer, and this
/// save used to leave the baseline alone — so the amber `● unsaved` chip could never clear in
/// Plugin mode, under a tooltip promising that Ctrl+S would save it.
///
/// Walked through every state the chip can be in: unbuilt (the tip says a save cannot capture
/// this), built (the tip says a save names this code), saved (the chip clears), edited since
/// (the chip comes back and a second save does NOT clear it — the row still names v1's
/// artifact, so claiming v2 was saved would be the lie), and a reloaded sha (no recorded source,
/// nothing to re-baseline to).
#[test]
fn a_plugin_save_rebaselines_to_its_built_source_and_the_chip_says_what_ctrl_s_saves() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_name = "my_strat".to_string();
    st.editor.source = "// v1".to_string();
    assert_ne!(st.editor.source, st.saved_source, "the premise: the unsaved chip is lit");
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_PLUGIN_UNBUILT, "no Build: nothing to save");

    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_PLUGIN_BUILT, "built: a save names this code");

    st.saved.save_name = "p1".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);
    assert_eq!(st.saved.strategies[0].plugin_sha.as_deref(), Some("c".repeat(64).as_str()));
    assert_eq!(st.saved_source, "// v1", "the baseline moves to the source the sha was built from");
    assert_eq!(st.editor.source, st.saved_source, "...so the chip clears");

    st.editor.source = "// v2".to_string();
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_PLUGIN_UNBUILT, "an edit since the Build");
    st.saved.save_name = "p1".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);
    assert_eq!(st.saved_source, "// v1", "a save cannot capture an edit its sha does not hold");
    assert_ne!(st.editor.source, st.saved_source, "...so the chip stays lit, truthfully");

    // A sha reloaded from a saved row: this session never held its source.
    st.plugin_built_source = None;
    st.saved_source = "baseline".to_string();
    st.saved.save_name = "p1".to_string();
    st.handle_saved_action(SavedAction::SaveCurrent);
    assert_eq!(st.saved_source, "baseline", "no recorded source, nothing to re-baseline to");
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_PLUGIN_UNBUILT);

    // Rhai mode keeps its sentence — there Ctrl+S does save the buffer.
    st.strategy_source = StrategySource::Rhai;
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP);
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

/// **A restart restores the strategy source WITH the buffer** — the defect: the buffer was
/// persisted and its language was not, so a Plugin author's Rust source came back in Rhai mode.
///
/// Round-tripped through the real snapshot a workspace write records (`workspace_snapshot`)
/// and the real restore (`with_workspace`), minus only the disk in between, which
/// `crate::workspace`'s own tests cover — including the old-format file. What comes back:
/// Plugin mode, the buffer, the plugin's name, NO Rhai verdict even before the first `poll`,
/// and no sha — a restored session is unbuilt until Build answers (the constructor says why).
#[test]
fn a_plugin_session_comes_back_in_plugin_mode_after_a_restart() {
    let (dir, store) = seeded_store();
    let rust = "use vike_model::Strategy;\npub fn build() {}\n";
    let mut before = state_new(&dir, store.clone());
    before.strategy_source = StrategySource::Plugin;
    before.plugin_name = "my_strat".to_string();
    before.editor.source = rust.to_string();
    before.build_rx = answered_build(rust, Ok("c".repeat(64)));
    before.poll();
    assert!(before.plugin_sha.is_some(), "the premise: the previous session had built it");

    let ws = before.workspace_snapshot();
    assert_eq!(ws.strategy_source, StrategySource::Plugin, "the write must record the source");

    let after = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        ChatApiKeys::default(),
        None,
        QaAutorun::Off,
        ws,
    );
    assert_eq!(after.strategy_source, StrategySource::Plugin, "Rust must come back as Plugin");
    assert_eq!(after.editor.source, rust);
    assert_eq!(after.plugin_name, "my_strat");
    assert_eq!(after.rhai_verdict(), None, "not judged as Rhai, not even before the first poll");
    assert!(after.plugin_sha.is_none(), "a sha is never restored");
    assert!(after.run_blocked_reason().is_some(), "Run waits for a Build of the restored buffer");
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

/// A plugin is not on the Named backend's roster any more than a script is — `named_spec`
/// falls through to `current_spec` rather than inventing a substitution.
///
/// ⚠ **`named_strategy` is set DELIBERATELY, and this test did not set it.** The arm being
/// covered is `(StrategySource::Plugin, _)` — a WILDCARD in the second position — and with
/// `named_strategy` left `None` every assertion below is equally satisfied by a rule reading
/// "anything with no name picked falls through", which is what the neighbouring
/// `(StrategySource::Native, None)` arm already says. Only a plugin that ALSO has a name
/// selected distinguishes the two: a substitution rule keyed on the name alone would swap in
/// `buy_hold` here, and this assertion would fail. Picking a real roster entry rather than an
/// invented string matters for the same reason — a name nothing could resolve would leave a
/// substituting implementation no substitution to make.
#[test]
fn plugin_mode_falls_through_named_spec_to_current_spec() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.named_strategy = Some("buy_hold".to_string());
    st.plugin_name = "my_strat".to_string();
    st.plugin_sha = Some("a".repeat(64));
    assert_eq!(st.named_spec(), st.current_spec());
    // ...and what it resolved to is still the PLUGIN, not the named native strategy the line
    // above would have diverted it to.
    assert!(
        matches!(st.named_spec(), StrategySpec::Plugin { .. }),
        "a picked native name must not divert a Plugin: {:?}",
        st.named_spec()
    );
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
    st.backend = crate::remote::Backend::Remote { addr };
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
    st.strategy_source = StrategySource::Plugin;
    assert!(st.run_disabled_reason().is_some(), "an unbuilt plugin cannot run");
    assert_eq!(st.run_disabled_reason(), st.run_blocked_reason());
}

#[test]
fn run_asks_for_a_slice_over_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let st = state_new(&dir, Arc::new(DataFusionHist::open(dir.path()).unwrap()));
    assert_eq!(st.run_disabled_reason(), Some(PICK_A_SLICE));
}

#[test]
fn sweep_and_walk_forward_say_why_they_are_disabled() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    assert_eq!(st.sweep_disabled_reason(true), None);
    assert_eq!(st.sweep_disabled_reason(false), Some(SEED_A_GRID));
    assert_eq!(st.walk_forward_disabled_reason(), None);
    let (_sweep_tx, sweep_rx) = std::sync::mpsc::channel();
    st.sweep_rx = Some(sweep_rx);
    assert_eq!(st.sweep_disabled_reason(true), Some(SWEEP_BUSY));
    let (_wf_tx, wf_rx) = std::sync::mpsc::channel();
    st.wf_rx = Some(wf_rx);
    assert_eq!(st.walk_forward_disabled_reason(), Some(WALK_FORWARD_BUSY));
}

#[test]
fn send_says_why_it_is_disabled() {
    let (dir, store) = seeded_store();
    let keyless = state_new(&dir, store.clone());
    assert_eq!(keyless.send_disabled_reason(), Some(NO_PROVIDER_KEY));
    let keys = ChatApiKeys { anthropic: Some("not-a-real-key".to_string()), cerebras: None };
    let mut st = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        keys,
        None,
        QaAutorun::Off,
        StudioWorkspace::default(),
    );
    st.picker.select(0);
    assert_eq!(st.send_disabled_reason(), Some(NOTHING_TO_SEND));
    st.chat.input = "RSI mean reversion".to_string();
    assert_eq!(st.send_disabled_reason(), None);
}

/// Switching backends keeps the address the operator typed — both dial the same compute daemon —
/// and a switch onto Named drops the roster, which belongs to the daemon that answered it. The
/// segmented control's one action; the two buttons it replaces each spelled half of this.
#[test]
fn switching_backends_keeps_the_address_and_drops_a_stale_roster() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.backend = Backend::Remote { addr: "127.0.0.2:7999".to_string() };
    st.switch_backend(BackendKind::Named);
    assert_eq!(st.backend, Backend::Named { addr: "127.0.0.2:7999".to_string() });
    st.named_roster = Some(Err("stale".to_string()));
    st.switch_backend(BackendKind::Remote);
    assert_eq!(st.backend, Backend::Remote { addr: "127.0.0.2:7999".to_string() });
    st.switch_backend(BackendKind::Named);
    assert!(st.named_roster.is_none(), "a roster belongs to the daemon that answered it");
}
