//! `poll` folding each worker's answer into the shell - a run, a catalog walk, a chat reply - and
//! the disconnect arms that keep a dead worker from leaving a spinner stuck; plus `cancel`.

use super::super::*;
use super::support::{seeded_store, spawn_backtesthub_server, state_new};
use std::assert_matches;
use std::sync::Arc;

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
    assert_matches!(st.last, Some(Ok(_)) | Some(Err(_)));
}

/// The editor's default template TRADES on a spot pair. The seeded slice is binance `BTCUSDT`, a
/// spot pair by its name, and a Studio run states no margin mode, so the spot law judges it
/// (decision 0276): the reversing cross the template used to be held its first buy, had every
/// short refused and closed nothing. The long-only template sells only what it bought.
#[test]
fn the_default_template_trades_on_a_spot_pair() {
    let res = run_over_the_seeded_spot_slice(None);
    assert!(res.n_trades > 0, "the default template closed no trade on a spot pair");
    assert!(res.refused.is_empty(), "and had no order refused: {:?}", res.refused);
}

/// The template as it was before it went long-only: an entry the other way is one sell of twice
/// the long, which the spot law refuses whole.
const REVERSING_CROSS: &str = r#"fn on_bar(strategy) {
    let f = sma(5);
    let s = sma(20);
    if s.is_nan() { return; }
    if f > s {
        strategy.entry("Long", OrderSide::Long).size(1.0);
    } else {
        strategy.entry("Short", OrderSide::Short).size(1.0);
    }
}
"#;

/// A refused order is SEEN in the Studio (the owner's ruling of 2026-11-12, part C): a reversing
/// cross over the spot slice has its shorts refused by the spot law on the server, the refusals
/// cross the wire, and the results pane's line names them in the gate's words. It is also why the
/// default template went long-only: the reversing one closes nothing here.
#[test]
fn a_run_whose_orders_are_refused_says_so_beside_the_result() {
    let res = run_over_the_seeded_spot_slice(Some(REVERSING_CROSS));
    assert_eq!(res.n_trades, 0, "the reversing cross never sells its long on a spot pair");
    let line = crate::panes::results::refused_line(&res).expect("the shorts were refused");
    assert!(line.contains("refused") && line.contains("a spot account cannot sell more"), "{line}");
}

/// Run `source` (else the editor's default) over the seeded binance `BTCUSDT` slice through a
/// loopback backtesthub, the Studio's own Run path, and return the run's summary.
fn run_over_the_seeded_spot_slice(
    source: Option<&str>,
) -> vike_backtesthub_client::summary::RunSummaryWire {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(Arc::clone(&store));
    let mut st = state_new(&_dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    if let Some(src) = source {
        st.editor.source = src.to_string();
    }
    st.start_run();
    for _ in 0..200 {
        st.poll();
        if !st.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    match st.last.take() {
        Some(Ok(r)) => r.summary,
        other => panic!("the run must land: {other:?}"),
    }
}

/// Spec §7 regression: if the worker's sender is dropped without ever sending (the
/// worker-thread-panic case, simulated here directly), `poll()` must observe the disconnect
/// and clear `running` rather than treat `Disconnected` the same as `Empty` forever — the
/// bug that would leave the spinner stuck and Run disabled for the process lifetime.
#[test]
fn poll_clears_running_when_worker_disconnects_without_sending() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let (tx, rx) = std::sync::mpsc::channel::<crate::backend::backtesthub::StudioRunViewOutcome>();
    drop(tx);
    st.run_rx = Some(rx);
    st.running = true;

    st.poll();

    assert!(!st.running, "poll() must clear running on a disconnected channel");
    assert_matches!(st.last, Some(Err(_)), "disconnect should surface as a run failure");
}

/// The ⟳ Refresh latch: a second click while a walk is in flight must not spawn a second walk
/// (`start_catalog_load`'s early return).
///
/// Proved by SUBSTITUTION rather than by counting threads: a planted receiver whose sender the
/// test still holds stands in for the in-flight walk, and a distinctive planted answer is then
/// pushed through it. If the second click had replaced `catalog_rx`, that send would go
/// nowhere and `poll` would fold the real store's lists instead.
#[test]
fn a_second_refresh_while_a_walk_is_in_flight_does_not_spawn_another() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let context = egui::Context::default();

    let (tx, rx) = std::sync::mpsc::channel::<crate::backend::catalog::StudioCatalogLoad>();
    st.catalog_rx = Some(rx);
    st.start_catalog_load(&context);

    tx.send(crate::backend::catalog::StudioCatalogLoad {
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

    let context = egui::Context::default();
    st.start_catalog_load(&context);
    assert!(st.catalog_loading(), "the button is disabled while the walk is owed an answer");
    for _ in 0..200 {
        st.poll();
        if !st.catalog_loading() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(!st.catalog_loading(), "the walk must land within the wait");
    assert!(st.picker.error().is_none(), "a seeded store must not read as a scan failure");
    assert_eq!(st.picker.available().len(), 1, "the one seeded bar series");
}

/// A catalog worker that dies without sending is a terminal failure in BOTH panes — never a
/// silently-kept previous list, which would claim the refresh found the store unchanged.
#[test]
fn poll_surfaces_a_dead_catalog_worker_in_both_panes() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let (tx, rx) = std::sync::mpsc::channel::<crate::backend::catalog::StudioCatalogLoad>();
    drop(tx);
    st.catalog_rx = Some(rx);

    st.poll();

    assert_eq!(st.picker.error(), Some(crate::backend::catalog::CATALOG_LOAD_LOST));
    assert_eq!(st.data_browser.error(), Some(crate::backend::catalog::CATALOG_LOAD_LOST));
    assert!(!st.catalog_loading(), "a disconnect clears the latch — Refresh works again");
}

/// Mirrors `poll_clears_running_when_worker_disconnects_without_sending` for the `chat_rx`
/// arm: `StudioChatOutcome`'s doc claims a worker panic surfaces as a human-readable failure, same
/// as the run/sweep/walk-forward arms do. Before this fix the disconnect arm cleared
/// `chat.running`/`chat_rx` but never pushed anything to the transcript, silently swallowing
/// the failure -- making the doc false.
#[test]
fn poll_surfaces_chat_worker_disconnect_in_the_transcript() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    let (tx, rx) = std::sync::mpsc::channel::<StudioChatOutcome>();
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
    st.grid = vec![StudioParamRow::scanned("fast", "3,5")];
    st.start_sweep();
    assert!(st.any_running(), "a running sweep should register as any_running");
    st.cancel();
    assert!(!st.any_running());
}
