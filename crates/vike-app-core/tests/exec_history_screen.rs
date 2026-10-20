//! **The Tearsheet's execution history, gated off the accessibility tree.**
//!
//! Drives the REAL `vike_app_core::ui::tool_views::history_body` — the window's whole body —
//! headless, over a [`HistoryView`] already showing a planted load (no socket, no file, no thread).
//! What it holds:
//!
//! - every empty state names ITS OWN cause — no datahub, a read that failed, a datahub too old, the
//!   materializer off (with rows and without), nothing in range — six different sentences, because
//!   "unavailable" and "empty" drawn alike is the defect `StoredCtx::load_error` was written for;
//! - the history has NO write control: no cancel, whatever the order's state (the one write path is
//!   the Account window's ✕);
//! - the account note shows only where a venue has several accounts AND a row carries none;
//! - a row keeps its columns in order at a narrow window, where the table runs wider than the pane.

use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_app_core::data::exec_history::{
    ExecHistoryLoad, ExecServed, HistoryFailure, HistoryPage, HistoryQuery, HistoryWindow,
};
use vike_app_core::ui::tool_views::{
    ACCOUNT_NOTE, DATAHUB_TOO_OLD, HistoryInputs, HistoryTab, HistoryView, LAG_CAPTION,
    MATERIALIZER_OFF_BANNER, MATERIALIZER_OFF_EMPTY, NO_DATAHUB, READ_FAILED, history_body,
};
use vike_data::{ExecFillRow, ExecOrderRow};
use vike_tradehub_client::wire::{WireDirectory, WireDirectoryAccount};

const NOW: i64 = 1_790_000_000_000;

fn query() -> HistoryQuery {
    HistoryQuery {
        venue: None,
        symbol: None,
        window: HistoryWindow::Week,
        now_ms: NOW,
        older_than: None,
        generation: 1,
    }
}

fn all_served() -> ExecServed {
    ExecServed { fills_range: true, orders: true, recording: true }
}

fn fill(account: Option<&str>) -> ExecFillRow {
    ExecFillRow {
        ts: NOW - 1_000,
        trade_id: "t1".to_string(),
        client_order_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.5,
        px: 65_000.0,
        commission: 0.1,
        mark_price: None,
        liquidity_side: "maker".to_string(),
        commission_asset: "USDT".to_string(),
        venue_order_id: Some("v1".to_string()),
        account: account.map(str::to_string),
    }
}

fn order(status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts: NOW - 2_000,
        client_order_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.5,
        order_type: "limit".to_string(),
        status: status.to_string(),
        price: Some(65_000.0),
        trigger_price: None,
        venue_order_id: Some("v1".to_string()),
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        account: Some("ALT".to_string()),
    }
}

fn page(served: ExecServed, fills: Vec<ExecFillRow>, orders: Vec<ExecOrderRow>) -> HistoryPage {
    HistoryPage { fills, orders, served, ..HistoryPage::default() }
}

fn loaded(outcome: Result<HistoryPage, HistoryFailure>) -> HistoryView {
    HistoryView::showing(ExecHistoryLoad { query: query(), outcome })
}

/// One headless frame (run to settle) of the body; every accessibility node's text, in tree order.
fn texts(
    mut view: HistoryView,
    datahub: Option<&'static str>,
    directory: Option<WireDirectory>,
    width: f32,
) -> Vec<String> {
    let mut harness = Harness::builder().with_size(egui::vec2(width, 600.0)).build_ui(move |ui| {
        // The body draws icons and named weights, whose families only the app's type binds.
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let inputs = HistoryInputs {
            datahub,
            display_tz: vike_chart::DisplayTz::default(),
            directory: directory.as_ref(),
            terminal_orders: 0,
            now_ms: NOW,
        };
        let asked = history_body(ui, &mut view, &inputs);
        assert!(asked.is_none(), "a view already showing a load asks for nothing: {asked:?}");
    });
    harness.run();
    harness
        .root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().map(|s| s.to_string()).or_else(|| a.value().map(|s| s.to_string()))
        })
        .collect()
}

fn says(texts: &[String], needle: &str) -> bool {
    texts.iter().any(|t| t.contains(needle))
}

/// **Six states, six sentences, and none borrows another's.** Each state's own sentence is on
/// screen; every OTHER state's sentence is not.
#[test]
fn each_empty_state_names_its_own_cause() {
    const HUB: Option<&str> = Some("127.0.0.1:7879");
    let off = ExecServed { recording: false, ..all_served() };
    let states: Vec<(&str, Vec<String>, String)> = vec![
        ("no datahub", texts(HistoryView::default(), None, None, 900.0), NO_DATAHUB.to_string()),
        (
            "read failed",
            texts(
                loaded(Err(HistoryFailure::Unreadable("connection refused".into()))),
                HUB,
                None,
                900.0,
            ),
            format!("{READ_FAILED}connection refused"),
        ),
        (
            "datahub too old",
            texts(
                loaded(Ok(page(ExecServed::default(), vec![fill(None)], vec![]))),
                HUB,
                None,
                900.0,
            ),
            DATAHUB_TOO_OLD.to_string(),
        ),
        (
            "recording off, no rows",
            texts(loaded(Ok(page(off, vec![], vec![]))), HUB, None, 900.0),
            MATERIALIZER_OFF_EMPTY.to_string(),
        ),
        (
            "recording off, rows",
            texts(loaded(Ok(page(off, vec![fill(None)], vec![]))), HUB, None, 900.0),
            MATERIALIZER_OFF_BANNER.to_string(),
        ),
        (
            "nothing in range",
            texts(loaded(Ok(page(all_served(), vec![], vec![]))), HUB, None, 900.0),
            "No executions between".to_string(),
        ),
    ];
    for (name, seen, own) in &states {
        assert!(says(seen, own), "{name}: its own sentence {own:?} is on screen: {seen:#?}");
        for (other, _, theirs) in &states {
            if other != name {
                assert!(!says(seen, theirs), "{name} must not say {other}'s {theirs:?}: {seen:#?}");
            }
        }
    }
    // The standing lag caption is under every state that has a datahub to read.
    for (name, seen, _) in &states[1..] {
        assert!(says(seen, LAG_CAPTION), "{name}: the lag caption: {seen:#?}");
    }
}

/// **No write control in the history**: an order whose last snapshot is still working says so in
/// the Account window's word and offers no cancel — that path is the Account window's alone.
#[test]
fn the_history_has_no_write_control() {
    let mut view = loaded(Ok(page(all_served(), vec![fill(Some("ALT"))], vec![order("ACCEPTED")])));
    view.tab = HistoryTab::Orders;
    let seen = texts(view, Some("hub"), None, 1_200.0);
    assert!(says(&seen, "ACCEPTED (live state: Account window)"), "{seen:#?}");
    for t in &seen {
        let lower = t.to_lowercase();
        assert!(!lower.contains("cancel"), "a write control is drawn in the history: {t:?}");
        assert!(!t.contains(vike_ui_theme::icons::CANCEL.rich().text()), "a ✕ is drawn: {t:?}");
    }
}

fn directory(accounts_on_binance: usize) -> WireDirectory {
    WireDirectory {
        venues: Vec::new(),
        accounts: (0..accounts_on_binance)
            .map(|i| WireDirectoryAccount {
                id: i as i64,
                venue: "binance".to_string(),
                label: (i > 0).then(|| format!("A{i}")),
                tier: "demo".to_string(),
                venue_account_id: None,
            })
            .collect(),
    }
}

/// The account note is said only where it is TRUE: a venue with several accounts, and a row of it
/// that carries none.
#[test]
fn the_account_note_shows_only_for_a_multi_account_venue() {
    let unlabelled = || loaded(Ok(page(all_served(), vec![fill(None)], vec![])));
    assert!(says(&texts(unlabelled(), Some("hub"), Some(directory(2)), 1_200.0), ACCOUNT_NOTE));
    assert!(!says(&texts(unlabelled(), Some("hub"), Some(directory(1)), 1_200.0), ACCOUNT_NOTE));
    let labelled = loaded(Ok(page(all_served(), vec![fill(Some("A1"))], vec![])));
    assert!(!says(&texts(labelled, Some("hub"), Some(directory(2)), 1_200.0), ACCOUNT_NOTE));
}

/// At a window far narrower than the table's minimums the row still names its cells in column
/// order — time, venue, symbol, account, side, qty, price, fee, liquidity, order.
#[test]
fn columns_keep_their_order_at_the_minimum_window_width() {
    let view = loaded(Ok(page(all_served(), vec![fill(Some("ALT"))], vec![])));
    let seen = texts(view, Some("hub"), None, 320.0);
    let row = seen
        .iter()
        .find(|t| t.contains("BTCUSDT") && t.contains("ALT"))
        .unwrap_or_else(|| panic!("the fill row is in the tree: {seen:#?}"));
    let order = ["binance", "BTCUSDT", "ALT", "BUY", "0.5", "65000", "0.1 USDT", "M", "c1"];
    let mut at = 0usize;
    for cell in order {
        let found =
            row[at..].find(cell).unwrap_or_else(|| panic!("{cell:?} after {at} in {row:?}"));
        at += found + cell.len();
    }
}
