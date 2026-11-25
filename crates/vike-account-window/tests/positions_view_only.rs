//! **The Account window's Positions tab is VIEW ONLY** (the owner's ruling, top layers wave 3:
//! "только просмотр"): it shows every account's open positions and offers nothing that sends.
//!
//! Held three ways:
//! - **on screen**, through the REAL `account_window_body` on the Positions tab over positions on three
//!   accounts: the only buttons are the other tabs, clicking every other node writes no cancel, asks for
//!   no read and leaves the tab where it is, and no text offers to close, reverse or cancel;
//! - **in the source**: `src/positions.rs` names no send path (the command lift, the control link, the
//!   wire's command, an order request or a cancel);
//! - **in the manifest**: this crate has no `vike-order-entry` edge, so the dispatcher's
//!   `OrderEntryDispatch*` is not even nameable here.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_account_window::{
    AccountWindowInputs, AccountWindowTab, AccountWindowView, account_window_body,
};
use vike_execution::{BalanceMode, CoreSnapshot, PositionView, VenueBlock};
use vike_model::accounts::account_keys::{parse_wire_account, route_key_of};
use vike_model::events::PositionSide;
use vike_model::{AccountTier, TradingState};
use vike_tradehub_client::node_words::BackendNodeWords;

fn position(venue: &str, symbol: &str, size: f64) -> PositionView {
    PositionView {
        venue: venue.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        size,
        average_price: 100.0,
        unrealized: -2.5,
        mark_source: None,
        leverage: 0.0,
        liquidation_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

fn block(venue: &str, label: Option<&str>, positions: Vec<PositionView>) -> VenueBlock {
    let account = label.map(|l| parse_wire_account(l).expect("a test label parses"));
    VenueBlock {
        account_code: None,
        order_client_id_shape: None,
        venue: venue.to_string(),
        route_key: account.as_ref().map_or_else(|| venue.to_string(), |l| route_key_of(venue, l)),
        account,
        symbol: String::new(),
        extra_symbols: Default::default(),
        tier: Some(AccountTier::Demo),
        balance: 0.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
        funding_paid: 0.0,
        balance_mode: BalanceMode::Delta,
        contract_values: Default::default(),
        contract_value_default: vike_model::ContractValue::default(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: Default::default(),
        ledger_currency: Default::default(),
        currency_assumed: false,
        colliding_fills_refused: 0,
        fees_unconverted: 0,
        fees_at_stale_rate: 0,
        equity: 1_000.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_buying_power: 0.0,
        margin_ratio: 0.0,
        fee_schedule: None,
        trading_state: TradingState::Active,
        positions,
    }
}

/// Positions on three accounts, two of them of one venue.
fn position_snapshot() -> CoreSnapshot {
    let mut snapshot = CoreSnapshot::empty("bybit", "BTCUSDT");
    snapshot.portfolio.venues = vec![
        block("bybit", None, vec![position("bybit", "BTCUSDT", 0.5)]),
        block("bybit", Some("LIVE"), vec![position("bybit", "ETHUSDT", -2.0)]),
        block("okx", None, vec![position("okx", "SOLUSDT", 7.0)]),
    ];
    snapshot
}

/// What the window wrote and asked for, frame by frame.
struct Window {
    view: AccountWindowView,
    asked: usize,
}

fn every_label(h: &Harness<'_, Window>) -> Vec<(Role, String)> {
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            let text = a.label().or_else(|| a.value())?.to_string();
            Some((a.role(), text))
        })
        .collect()
}

/// ⚠ **Nothing on the Positions tab sends**: its rows are on screen, the only buttons are the four other
/// tabs, a click on every other node writes no cancel, asks for no read and keeps the tab, and no text
/// offers to close, reverse or cancel a position.
#[test]
fn the_positions_tab_draws_every_accounts_positions_and_nothing_that_sends() {
    let snapshot = position_snapshot();
    let view =
        AccountWindowView { tab: AccountWindowTab::Positions, ..AccountWindowView::default() };
    let mut h = Harness::builder().with_size(egui::vec2(1_200.0, 700.0)).build_ui_state(
        move |ui, w: &mut Window| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let inputs = AccountWindowInputs {
                snapshot: &snapshot,
                words: BackendNodeWords::NONE,
                datahub: Some("127.0.0.1:17879"),
                display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone::default(),
                directory: None,
                now_ms: 1_790_000_000_000,
            };
            w.asked += usize::from(account_window_body(ui, &inputs, &mut w.view).is_some());
        },
        Window { view, asked: 0 },
    );
    h.run();
    let seen = every_label(&h);
    for symbol in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        assert!(seen.iter().any(|(_, t)| t == symbol), "{symbol}'s row is drawn: {seen:#?}");
    }
    assert!(seen.iter().any(|(_, t)| t == "Positions (3)"), "the tab counts its rows: {seen:#?}");
    let tabs = ["Accounts (3)", "Orders (0)", "Strategies (0)", "History"];
    for (role, text) in &seen {
        if *role == Role::Button {
            assert!(tabs.contains(&text.as_str()), "a button that is not a tab: {text:?}");
        }
        let lower = text.to_lowercase();
        for word in ["close", "reverse", "cancel", "flatten"] {
            assert!(!lower.contains(word), "{text:?} offers to {word}");
        }
        assert!(
            !text.contains(vike_ui_theme::icons::CANCEL.rich().text()),
            "a ✕ is drawn: {text:?}"
        );
    }
    let others: Vec<String> = seen
        .iter()
        .filter(|(role, text)| !(*role == Role::Button && tabs.contains(&text.as_str())))
        .map(|(_, text)| text.clone())
        .collect();
    for text in &others {
        let nodes: Vec<_> = h
            .root()
            .children_recursive()
            .filter(|n| {
                let a = n.accesskit_node();
                a.label().or_else(|| a.value()).as_deref() == Some(text.as_str())
            })
            .collect();
        nodes.iter().for_each(|n| n.click());
        h.run();
    }
    let w = h.state();
    assert_eq!(w.view.cancel, None, "no cancel was written");
    assert_eq!(w.asked, 0, "no read was asked for");
    assert_eq!(w.view.tab, AccountWindowTab::Positions, "no click left the tab");
}

/// **The Positions tab's source names no send path, and the crate links no order entry.** Both files
/// ship in the public mirror, so reading them at compile time is allowed.
#[test]
fn the_positions_source_names_no_send_path_and_the_crate_links_no_order_entry() {
    let source = include_str!("../src/positions.rs");
    for word in
        ["command_lift", "tradehub_control", "CommandWire", "OrderRequest", "cancel", "Cancel"]
    {
        assert!(!source.contains(word), "src/positions.rs names {word:?}");
    }
    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains("vike-order-entry"), "the crate gained a vike-order-entry edge");
}
