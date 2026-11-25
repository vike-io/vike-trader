//! **The Account window's History tab: a read only when it shows, and its state across a switch.**
//!
//! Drives the REAL `vike_account_window::account_window_body` headless (no socket, no
//! thread: the read the body returns is counted, never dialled). What it holds:
//!
//! - the Accounts, Orders and Strategies tabs ask for no History read, however many frames they draw;
//!   the History tab asks for exactly one on its first frame and none on the next;
//! - switching away from History and back keeps its venue filter, its seed and the expanded order, and
//!   asks for no second read;
//! - the History tab's tables keep the window's whole width, where every other tab stops at the cap.

use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_account_window::history::AccountWindowHistoryView;
use vike_account_window::history::page::{
    ExecutionHistoryLoad, ExecutionHistoryPage, ExecutionHistoryQuery, ExecutionHistoryServed,
    ExecutionHistoryWindow,
};
use vike_account_window::{
    AccountWindowInputs, AccountWindowTab, AccountWindowView, account_window_body,
};
use vike_data::ExecFillRow;
use vike_execution::CoreSnapshot;
use vike_tradehub_client::node_words::BackendNodeWords;

const NOW: i64 = 1_790_000_000_000;

/// The window over an empty snapshot and a configured datahub, and the reads it asked for.
struct Window {
    view: AccountWindowView,
    asked: Vec<ExecutionHistoryQuery>,
}

fn harness(view: AccountWindowView, width: f32) -> Harness<'static, Window> {
    let snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    Harness::builder().with_size(egui::vec2(width, 700.0)).build_ui_state(
        move |ui, w: &mut Window| {
            // The body draws icons and named weights, whose families only the app's type binds.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let inputs = AccountWindowInputs {
                snapshot: &snapshot,
                words: BackendNodeWords::NONE,
                datahub: Some("127.0.0.1:17879"),
                display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone::default(),
                directory: None,
                now_ms: NOW,
            };
            w.asked.extend(account_window_body(ui, &inputs, &mut w.view));
        },
        Window { view, asked: Vec::new() },
    )
}

/// `n` frames, stepped rather than run: a History read in flight spins, and a spinner repaints.
fn frames(h: &mut Harness<'_, Window>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn fill(i: usize) -> ExecFillRow {
    ExecFillRow {
        ts: NOW - 1_000 * i as i64,
        trade_id: format!("t{i}"),
        order_client_id: format!("c{i}"),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.5,
        px: 65_000.0,
        commission: 0.1,
        mark_price: None,
        liquidity_side: "maker".to_string(),
        commission_asset: "USDT".to_string(),
        venue_order_id: Some(format!("v{i}")),
        account: None,
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    }
}

/// A History view already showing a page of three fills: it asks for nothing on its first frame.
fn showing() -> AccountWindowHistoryView {
    AccountWindowHistoryView::showing(ExecutionHistoryLoad {
        query: ExecutionHistoryQuery {
            venue: None,
            symbol: None,
            window: ExecutionHistoryWindow::Week,
            now_ms: NOW,
            older_than: None,
            generation: 1,
        },
        outcome: Ok(ExecutionHistoryPage {
            fills: (0..3).map(fill).collect(),
            served: ExecutionHistoryServed { fills_range: true, orders: true, recording: true },
            ..ExecutionHistoryPage::default()
        }),
    })
}

/// ⚠ **Only the History tab reads the datahub, and only once**: a window on Accounts, Orders or
/// Strategies asks for nothing, the first History frame asks for exactly one read, and the frames after
/// it ask for none (the read is in flight, and the body waits for it).
#[test]
fn only_the_history_tab_asks_for_a_read_and_only_on_its_first_frame() {
    let mut h = harness(AccountWindowView::default(), 900.0);
    for tab in [AccountWindowTab::Accounts, AccountWindowTab::Orders, AccountWindowTab::Strategies]
    {
        h.state_mut().view.tab = tab;
        frames(&mut h, 6);
        assert!(h.state().asked.is_empty(), "{tab:?} asked for a History read");
    }
    h.state_mut().view.tab = AccountWindowTab::History;
    frames(&mut h, 6);
    assert_eq!(h.state().asked.len(), 1, "the first History frame asks once, and only once");
    assert_eq!(h.state().asked[0].window, ExecutionHistoryWindow::default());
    h.state_mut().view.tab = AccountWindowTab::Accounts;
    frames(&mut h, 3);
    h.state_mut().view.tab = AccountWindowTab::History;
    frames(&mut h, 3);
    assert_eq!(h.state().asked.len(), 1, "coming back to History asks for no second read");
}

/// **A tab switch keeps the History tab's state**: its venue filter, its seed and the order whose
/// lifecycle is expanded are where they were after Accounts, Orders and Strategies have each shown,
/// and no read is asked for on the way.
#[test]
fn switching_away_from_history_and_back_keeps_its_filters_seed_and_expanded_order() {
    let mut history = showing();
    history.venue = Some("binance".to_string());
    history.seed = 2_500.0;
    history.expanded = Some("c1".to_string());
    let view = AccountWindowView { tab: AccountWindowTab::History, history, cancel: None };
    let mut h = harness(view, 900.0);
    frames(&mut h, 3);
    for tab in [
        AccountWindowTab::Accounts,
        AccountWindowTab::Orders,
        AccountWindowTab::Strategies,
        AccountWindowTab::History,
    ] {
        h.state_mut().view.tab = tab;
        frames(&mut h, 3);
    }
    let w = h.state();
    assert!(w.asked.is_empty(), "no read was asked for: {:?}", w.asked);
    assert_eq!(w.view.history.venue.as_deref(), Some("binance"));
    assert_eq!(w.view.history.seed, 2_500.0);
    assert_eq!(w.view.history.expanded.as_deref(), Some("c1"));
}

/// **The History tab keeps the window's whole width** (the Execution-history window's, before it
/// became this tab): in a 2000-point window its fill rows run past the 960-point cap the other tabs
/// stop at.
#[test]
fn the_history_tables_keep_the_whole_width_past_the_cap() {
    let view =
        AccountWindowView { tab: AccountWindowTab::History, history: showing(), cancel: None };
    let mut h = harness(view, 2_000.0);
    frames(&mut h, 4);
    let widest = h
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().label().is_some_and(|l| l.contains("BTCUSDT")))
        .map(|n| n.rect().width())
        .fold(0.0_f32, f32::max);
    let cap = vike_ui_theme::value::account::MAX_WIDTH;
    assert!(widest > cap + 100.0, "a fill row is {widest} wide, not past the {cap} cap");
}
