//! Tests of the Trade window's glue: the fixtures every theme in `trade_tests/` shares.

use super::accounts::{LANE_TWIN_NAMES, account_name, engines_run, status_of, why_not};
use super::catalog::{ROWS_CACHED, split_base_quote};
use super::reasons::{
    ACCOUNT_LIST_UNAVAILABLE, NOT_LISTED_WHY, TPSL_LANE_WHY, TPSL_SYMBOL_WHY, not_running_because,
};
use super::*;
use crate::orders::order_dispatch::{ORDERS_UNATTRIBUTED_WHY, tradable, window_orders};
use std::assert_matches;
use std::sync::Arc;
use vike_catalog::Catalog;
use vike_exec::OrderView;
use vike_marketdata::L2Book;
use vike_model::AccountTier;
use vike_model::Instrument;
use vike_model::OrderStatus;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{Contract, Margining, PerpSpec, SymbolProperties};
use vike_panels::trade::Tradable;
use vike_panels::trade::ticket::TPSL_ACCOUNT_WHY;
use vike_tradehub_client::wire::{WireDirectory, WireDirectoryAccount, WireDirectoryVenue};

// ---- fixtures ------------------------------------------------------------------------------

fn venue(name: &str, title: Option<&str>) -> WireDirectoryVenue {
    WireDirectoryVenue { name: name.into(), title: title.map(Into::into) }
}

fn account(id: i64, venue: &str, label: Option<&str>, tier: &str) -> WireDirectoryAccount {
    WireDirectoryAccount {
        id,
        venue: venue.into(),
        label: label.map(Into::into),
        tier: tier.into(),
        venue_account_id: None,
    }
}

/// One engine's block, as a node that publishes tier and symbols names it. Its route key is
/// derived from the label exactly as the core derives it.
fn block(venue: &str, label: Option<&str>, symbol: &str, tier: Option<AccountTier>) -> VenueBlock {
    let account = label.map(|l| AccountLabel::parse(l).expect("a test label"));
    let route_key = match &account {
        Some(l) => vike_model::accounts::account_keys::route_key_of(venue, l),
        None => venue.to_string(),
    };
    VenueBlock {
        venue: venue.into(),
        account,
        route_key,
        symbol: symbol.into(),
        tier,
        // As a current node publishes it: the venues it opens say so.
        admits_symbols: matches!(venue, "binance" | "bybit" | "aster"),
        ..Default::default()
    }
}

/// A snapshot publishing `blocks`, the first one its primary (`vike_core::snapshot::build` publishes the
/// primary engine first).
fn snap_with(blocks: Vec<VenueBlock>) -> CoreSnapshot {
    let (v, s) = blocks.first().map(|b| (b.venue.clone(), b.symbol.clone())).unwrap_or_default();
    let mut snap = CoreSnapshot::empty(&v, &s);
    snap.portfolio.venues = blocks;
    snap
}

fn order(coid: &str, label: Option<&str>, status: OrderStatus) -> OrderView {
    OrderView {
        client_order_id: coid.into(),
        venue: "binance".into(),
        account: label.map(|l| AccountLabel::parse(l).expect("a test label")),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        data_delay: None,
    }
}

fn inst(venue: &str, symbol: &str, quote: &str, contract: Contract, lot: f64) -> Instrument {
    Instrument::new(venue, symbol, "BTC", quote, contract)
        .with_description(format!("Bitcoin / {quote}"))
        .with_properties(SymbolProperties {
            tick_size: 0.1,
            step_size: lot,
            min_qty: lot,
            ..Default::default()
        })
}

/// A linear perpetual settling in its quote (binance `BTCUSDT.P`, okx `BTC-USDT-SWAP`).
fn linear_perp(settle: &str) -> Contract {
    Contract::Perp(PerpSpec {
        margining: Margining::Linear,
        settle: settle.into(),
        value_ccy: None,
        funding_interval_ms: None,
        settling_ms: None,
    })
}

/// BTC on three venues: binance spot and perp (and a USDC pair), okx spot and swap, bybit spot.
fn btc_catalog() -> Arc<Catalog> {
    Arc::new(Catalog::from_instruments(vec![
        inst("binance", "BTCUSDT", "USDT", Contract::CryptoSpot, 0.001),
        inst("binance", "BTCUSDT.P", "USDT", linear_perp("USDT"), 0.001),
        inst("binance", "BTCUSDC", "USDC", Contract::CryptoSpot, 0.001),
        inst("okx", "BTC-USDT", "USDT", Contract::CryptoSpot, 0.0001),
        inst("okx", "BTC-USDT-SWAP", "USDT", linear_perp("USDT"), 0.01),
        inst("bybit", "BTCUSDT", "USDT", Contract::CryptoSpot, 0.000001),
    ]))
}

/// `c` synced to `cat` for a window at `(venue, symbol)` with the picker on `query`, on a node
/// publishing `snap`.
fn sync_on(
    c: &mut TradeCatalog,
    cat: &Arc<Catalog>,
    _snap: &CoreSnapshot,
    (venue, symbol, query): (&str, &str, &str),
) {
    c.sync(cat, venue, symbol, query);
}

fn synced(
    cat: &Arc<Catalog>,
    snap: &CoreSnapshot,
    venue: &str,
    symbol: &str,
    query: &str,
) -> TradeCatalog {
    let mut c = TradeCatalog::default();
    sync_on(&mut c, cat, snap, (venue, symbol, query));
    c
}

/// One frame's sources at `(venue, account, symbol)`, with no book, no recent picks and no
/// status.
fn sources<'a>(
    snap: &'a CoreSnapshot,
    directory: Option<&'a WireDirectory>,
    catalog: &'a TradeCatalog,
    (venue, account, symbol): (&'a str, Option<&'a str>, &'a str),
) -> WindowSources<'a> {
    WindowSources {
        snap,
        directory,
        catalog,
        venue,
        account,
        symbol,
        book: None,
        tape: &[],
        stale: false,
        source: None,
        recent: &[],
        status: None,
        bracket_wire: false,
        control_link: ControlLink::Open,
        directory_unavailable: false,
        not_running: &[],
        key_changed: &[],
    }
}

/// A node running binance's default account (LIVE, 1500 free) and its `SUB` account (DEMO,
/// 250 free), both on BTCUSDT.
fn two_accounts() -> CoreSnapshot {
    let mut main = block("binance", None, "BTCUSDT", Some(AccountTier::Live));
    main.free_bp = 1500.0;
    let mut sub = block("binance", Some("SUB"), "BTCUSDT", Some(AccountTier::Demo));
    sub.free_bp = 250.0;
    snap_with(vec![main, sub])
}

/// The database behind [`two_accounts`]: SUB listed BEFORE the default account (the reply is
/// id-ordered), an okx account the server does not run, and bybit with no account at all.
fn two_accounts_directory() -> WireDirectory {
    WireDirectory {
        venues: vec![
            venue("binance", Some("Binance")),
            venue("okx", Some("OKX")),
            venue("bybit", None),
        ],
        accounts: vec![
            account(1, "binance", Some("SUB"), "demo"),
            account(2, "binance", None, "live"),
            account(3, "okx", None, "demo"),
        ],
    }
}

#[path = "trade_tests/pieces.rs"]
#[cfg(test)]
mod pieces;

#[path = "trade_tests/window_inputs.rs"]
#[cfg(test)]
mod window_inputs;

#[path = "trade_tests/faults.rs"]
#[cfg(test)]
mod faults;

#[path = "trade_tests/status_strip.rs"]
#[cfg(test)]
mod status_strip;
