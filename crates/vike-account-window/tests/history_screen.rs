//! **The Account window's History tab (the former Execution-history window), gated off the accessibility tree.**
//!
//! Drives the REAL `vike_account_window::history::history_body` — the window's whole body —
//! headless, over an [`AccountWindowHistoryView`] already showing a planted load (no socket, no file, no thread).
//! What it holds:
//!
//! - every empty state names ITS OWN cause — no datahub, a read that failed, a datahub too old, a
//!   datahub recording no live executions (with rows and without), nothing in range — six different
//!   sentences, because
//!   "unavailable" and "empty" drawn alike is the defect `DataManagerInventoryContext::load_error` was written for;
//! - the history has NO write control: no cancel, whatever the order's state (the one write path is
//!   the Account window's ✕);
//! - the account note shows only where a venue has several accounts AND a row carries none;
//! - the Summary is one account's: its label and tier, its foreign fills folded and said, the fills
//!   without an account left out and said;
//! - a row keeps its columns in order at a narrow window, where the table runs wider than the pane.

use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_account_window::history::page::{
    ExecutionHistoryFailure, ExecutionHistoryLoad, ExecutionHistoryPage, ExecutionHistoryQuery,
    ExecutionHistoryServed, ExecutionHistoryWindow,
};
use vike_account_window::history::{
    ACCOUNT_NOTE, AccountWindowHistoryInputs, AccountWindowHistoryTab, AccountWindowHistoryView,
    CATALOG_STEP, DATAHUB_TOO_OLD, LAG_CAPTION, LOADING, NO_DATAHUB, NOT_RECORDING_BANNER,
    NOT_RECORDING_EMPTY, READ_FAILED, history_body,
};
use vike_data::{ExecFillRow, ExecOrderRow};
use vike_tradehub_client::wire::{AccountDirectoryAccountWire, AccountDirectoryWire};

const NOW: i64 = 1_790_000_000_000;

fn query() -> ExecutionHistoryQuery {
    ExecutionHistoryQuery {
        venue: None,
        symbol: None,
        window: ExecutionHistoryWindow::Week,
        now_ms: NOW,
        older_than: None,
        generation: 1,
    }
}

fn all_served() -> ExecutionHistoryServed {
    ExecutionHistoryServed { fills_range: true, orders: true, recording: true }
}

fn fill(account: Option<&str>) -> ExecFillRow {
    ExecFillRow {
        ts: NOW - 1_000,
        trade_id: "t1".to_string(),
        order_client_id: "c1".to_string(),
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

fn order(status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts: NOW - 2_000,
        order_client_id: "c1".to_string(),
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
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    }
}

fn page(
    served: ExecutionHistoryServed,
    fills: Vec<ExecFillRow>,
    orders: Vec<ExecOrderRow>,
) -> ExecutionHistoryPage {
    ExecutionHistoryPage { fills, orders, served, ..ExecutionHistoryPage::default() }
}

fn loaded(
    outcome: Result<ExecutionHistoryPage, ExecutionHistoryFailure>,
) -> AccountWindowHistoryView {
    AccountWindowHistoryView::showing(ExecutionHistoryLoad { query: query(), outcome })
}

/// One headless frame (run to settle) of the body; every accessibility node's text, in tree order.
fn texts(
    mut view: AccountWindowHistoryView,
    datahub: Option<&'static str>,
    directory: Option<AccountDirectoryWire>,
    width: f32,
) -> Vec<String> {
    let mut harness = Harness::builder().with_size(egui::vec2(width, 600.0)).build_ui(move |ui| {
        // The body draws icons and named weights, whose families only the app's type binds.
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let inputs = AccountWindowHistoryInputs {
            datahub,
            display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone::default(),
            directory: directory.as_ref(),
            terminal_orders: 0,
            off_dollar_ledgers: &[],
            off_default_contracts: &[],
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

/// **A load says where it is from its first frame**: before its catalog has said how many series
/// there are to read, the body and the bar beside the spinner name the catalog step (the owner's
/// screenshot of 2026-11-17 showed a bare spinner for the 3.0 s the catalog took of a 3.8 s load).
#[test]
fn a_load_names_its_step_before_the_catalog_is_in() {
    let mut view = AccountWindowHistoryView::default();
    let mut harness = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_ui(move |ui| {
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        let inputs = AccountWindowHistoryInputs {
            datahub: Some("127.0.0.1:17879"),
            display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone::default(),
            directory: None,
            terminal_orders: 0,
            off_dollar_ledgers: &[],
            off_default_contracts: &[],
            now_ms: NOW,
        };
        // The load it asks for never lands here: the window stays on its first step.
        let _ = history_body(ui, &mut view, &inputs);
    });
    // Stepped, not run: the spinner repaints for as long as the load is in flight.
    for _ in 0..4 {
        harness.step();
    }
    let seen: Vec<String> = harness
        .root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().or_else(|| a.value()).map(|s| s.to_string())
        })
        .collect();
    assert!(
        says(&seen, &format!("{LOADING} {CATALOG_STEP}")),
        "the body names the step: {seen:#?}"
    );
    assert!(seen.iter().any(|t| t == CATALOG_STEP), "and so does the bar: {seen:#?}");
}

/// **Six states, six sentences, and none borrows another's.** Each state's own sentence is on
/// screen; every OTHER state's sentence is not.
#[test]
fn each_empty_state_names_its_own_cause() {
    const HUB: Option<&str> = Some("127.0.0.1:17879");
    let off = ExecutionHistoryServed { recording: false, ..all_served() };
    let states: Vec<(&str, Vec<String>, String)> = vec![
        (
            "no datahub",
            texts(AccountWindowHistoryView::default(), None, None, 900.0),
            NO_DATAHUB.to_string(),
        ),
        (
            "read failed",
            texts(
                loaded(Err(ExecutionHistoryFailure::Unreadable("connection refused".into()))),
                HUB,
                None,
                900.0,
            ),
            format!("{READ_FAILED}connection refused"),
        ),
        (
            "datahub too old",
            texts(
                loaded(Ok(page(ExecutionHistoryServed::default(), vec![fill(None)], vec![]))),
                HUB,
                None,
                900.0,
            ),
            DATAHUB_TOO_OLD.to_string(),
        ),
        (
            "recording off, no rows",
            texts(loaded(Ok(page(off, vec![], vec![]))), HUB, None, 900.0),
            NOT_RECORDING_EMPTY.to_string(),
        ),
        (
            "recording off, rows",
            texts(loaded(Ok(page(off, vec![fill(None)], vec![]))), HUB, None, 900.0),
            NOT_RECORDING_BANNER.to_string(),
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

/// **The History names the execution stream, never the retired journal materializer**: no sentence
/// sends an operator to the `flags.journal_materializer` row, which nothing reads any more.
#[test]
fn the_history_says_the_execution_streams_words() {
    for (name, words) in
        [("empty", NOT_RECORDING_EMPTY), ("banner", NOT_RECORDING_BANNER), ("caption", LAG_CAPTION)]
    {
        assert!(!words.contains("journal_materializer"), "{name}: {words}");
        assert!(!words.to_lowercase().contains("journal"), "{name}: {words}");
    }
    assert!(NOT_RECORDING_EMPTY.starts_with("This datahub records no live executions"));
    assert!(LAG_CAPTION.contains("streams"), "{LAG_CAPTION}");
}

/// **No write control in the history**: an order whose last snapshot is still working says so in
/// the Account window's word and offers no cancel — that path is the Account window's alone.
#[test]
fn the_history_has_no_write_control() {
    let mut view = loaded(Ok(page(all_served(), vec![fill(Some("ALT"))], vec![order("ACCEPTED")])));
    view.tab = AccountWindowHistoryTab::Orders;
    let seen = texts(view, Some("hub"), None, 1_200.0);
    assert!(says(&seen, "ACCEPTED (live state: Account window)"), "{seen:#?}");
    for t in &seen {
        let lower = t.to_lowercase();
        assert!(!lower.contains("cancel"), "a write control is drawn in the history: {t:?}");
        assert!(!t.contains(vike_ui_theme::icons::CANCEL.rich().text()), "a ✕ is drawn: {t:?}");
    }
}

fn directory(accounts_on_binance: usize) -> AccountDirectoryWire {
    AccountDirectoryWire {
        venues: Vec::new(),
        accounts: (0..accounts_on_binance)
            .map(|i| AccountDirectoryAccountWire {
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

/// **The Summary is ONE account's** (the owner, 2026-11-18): the label names the account and the tier
/// the node's directory gives it, its foreign fill is folded and said, and the fill recorded before the
/// execution stream (no account) is left out and said.
#[test]
fn the_summary_is_one_accounts_and_says_what_it_folded_and_left_out() {
    let ours = ExecFillRow { side: -1, ownership: Some("ours".to_string()), ..fill(Some("A1")) };
    let foreign = ExecFillRow {
        ts: NOW - 2_000,
        trade_id: "t2".to_string(),
        ownership: Some("foreign".to_string()),
        ..fill(Some("A1"))
    };
    // Its own execution: a pre-stream row of `t1` would be the stream's `ours` restated, and not left out.
    let before = ExecFillRow { trade_id: "t0".to_string(), ..fill(None) };
    let mut view = loaded(Ok(page(all_served(), vec![ours, foreign, before], vec![])));
    view.tab = AccountWindowHistoryTab::Summary;
    let seen = texts(view, Some("hub"), Some(directory(2)), 1_200.0);
    assert!(says(&seen, "binance/A1 (demo) "), "the label, with the directory's tier: {seen:#?}");
    assert!(says(&seen, "1 of these fills are foreign"), "{seen:#?}");
    assert!(
        says(&seen, "1 fills of binance were recorded before the execution stream"),
        "{seen:#?}"
    );
    assert!(says(&seen, "over this account's 2 fills loaded above"), "{seen:#?}");
}

/// **A fee correction is folded into its fill, never drawn as a row of its own**
/// (`vike_data::ExecFillRow::fill_events`, the Summary's own read). The stored correction row moves
/// no quantity and states no side, so drawn raw it read `SELL`, Qty 0, with the correction's total
/// as its fee, beside the fill still showing the fee it was written with.
#[test]
fn a_fee_correction_is_folded_into_its_fill_not_drawn_as_a_row() {
    let filled = ExecFillRow { commission: 0.25, ..fill(Some("ALT")) };
    let correction = ExecFillRow {
        trade_id: String::new(),
        side: 0,
        qty: 0.0,
        commission: 0.5,
        liquidity_side: String::new(),
        corrects_trade_id: Some("t1".to_string()),
        correction_seq: Some(7),
        ..filled.clone()
    };
    let view = loaded(Ok(page(all_served(), vec![filled, correction], vec![])));
    let seen = texts(view, Some("hub"), None, 1_200.0);
    // A row is named by its cells joined; a cell alone holds one of these words, never both.
    let rows: Vec<&String> =
        seen.iter().filter(|t| t.contains("binance") && t.contains("BTCUSDT")).collect();
    assert_eq!(rows.len(), 1, "one fill, one row: {seen:#?}");
    assert!(rows[0].contains("0.75 USDT"), "the fill's fee with its correction: {rows:?}");
    assert!(!rows[0].contains("SELL"), "{rows:?}");
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

/// Every node of `h`'s tree whose label is exactly `want`.
fn labelled<'t>(h: &'t Harness<'_, ()>, want: &str) -> Vec<egui_kittest::Node<'t>> {
    h.root()
        .children_recursive()
        .filter(|n| {
            let a = n.accesskit_node();
            a.label().is_some_and(|l| l == want) || a.value().is_some_and(|v| v == want)
        })
        .collect()
}

/// **Picking a venue does not move the panel** (the owner's report of 2026-11-16: the heading and
/// the left edge shifted when he picked bybit). Through the real dropdown — open "Venue", click
/// "bybit" — the view asks a load, the datahub's answer (nothing that week) lands, and the heading
/// sits exactly where it sat with the dropdown closed, at a narrow and at a wide width.
#[test]
fn picking_a_venue_keeps_the_heading_where_it_was() {
    for width in [420.0, 1_200.0] {
        let mut all = page(all_served(), vec![fill(Some("ALT"))], vec![order("FILLED")]);
        all.venues = [("binance", "BTCUSDT"), ("bybit", "BTCUSDT")]
            .into_iter()
            .map(|(v, s)| (v.to_string(), [s.to_string()].into_iter().collect()))
            .collect();
        let venues = all.venues.clone();
        let mut view = loaded(Ok(all));
        view.tab = AccountWindowHistoryTab::Summary;
        let mut harness =
            Harness::builder().with_size(egui::vec2(width, 700.0)).build_ui(move |ui| {
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                let inputs = AccountWindowHistoryInputs {
                    datahub: Some("hub"),
                    display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone::default(),
                    directory: None,
                    terminal_orders: 0,
                    off_dollar_ledgers: &[],
                    off_default_contracts: &[],
                    now_ms: NOW,
                };
                if let Some(query) = history_body(ui, &mut view, &inputs) {
                    // The datahub's answer for the picked venue: nothing that week.
                    let outcome = Ok(ExecutionHistoryPage {
                        venues: venues.clone(),
                        ..page(all_served(), vec![], vec![])
                    });
                    view.slot().put(ExecutionHistoryLoad { query, outcome });
                }
            });
        harness.run();
        let heading = |h: &Harness<'_, ()>| {
            labelled(h, "Execution history").first().expect("the heading is drawn").rect()
        };
        let before = heading(&harness);
        labelled(&harness, "Venue").first().expect("the venue dropdown is drawn").click();
        harness.run();
        // The option and, beside it, its text node: the click lands on whichever takes it.
        let options = labelled(&harness, "bybit");
        assert!(!options.is_empty(), "bybit is in the open dropdown");
        options.iter().for_each(|n| n.click());
        harness.run();
        let shown: Vec<String> = harness
            .root()
            .children_recursive()
            .filter_map(|n| {
                let a = n.accesskit_node();
                a.label().or_else(|| a.value()).map(|s| s.to_string())
            })
            .collect();
        assert!(says(&shown, "No executions between"), "bybit's empty week is shown: {shown:#?}");
        assert_eq!(heading(&harness), before, "width {width}: the heading did not move");
    }
}
