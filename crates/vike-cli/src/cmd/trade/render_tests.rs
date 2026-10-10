use super::*;
use vike_tradehub_client::wire::{WireOrderView, WireSnapshot};

fn snap_with(orders: Vec<WireOrderView>) -> WireSnapshot {
    let mut s = WireSnapshot::empty();
    s.orders = orders;
    s
}

fn order(coid: &str, venue: &str, symbol: &str) -> WireOrderView {
    WireOrderView {
        client_order_id: coid.into(),
        venue: venue.into(),
        account: None,
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        trigger_price: None,
        status: "working".into(),
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

#[test]
fn a_venue_filter_selects_only_that_venues_orders() {
    let s = snap_with(vec![order("a", "binance", "BTCUSDT"), order("b", "okx", "BTCUSDT")]);
    let book = crate::cmd::trade::selector::parse("binance").unwrap();
    let rows = order_rows(&s, Some(&book), None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].coid, "a");
}

// ⚠ Named in lowercase ("flat", not "FLAT"): the literal brief spelling trips rustc's
// `non_snake_case` lint, and this crate's `cargo clippy -- -D warnings` gate denies every
// warning, that one included. The behaviour under test is unchanged.
#[test]
fn json_is_flat_and_every_row_carries_its_own_venue_and_account() {
    let s = snap_with(vec![order("a", "binance", "BTCUSDT")]);
    let rows = order_rows(&s, None, None);
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows)).unwrap();
    let arr = v.as_array().expect("the document is an ARRAY, never a nesting");
    assert_eq!(arr[0]["venue"], "binance");
    assert!(
        arr[0].get("account").is_some(),
        "the account KEY is always present - absent means the wire named none, and a machine \
             reader must not have to infer which book a row belongs to"
    );
}

/// The order read does not attribute a row to an account today (`WireOrderView::account`'s absence
/// cannot tell the default account from an older node), so the column is null on every row — a
/// decision of this read, not a wire limitation. A labelled book is refused before it gets here.
#[test]
fn the_account_is_null_until_order_reads_attribute_an_account() {
    let s = snap_with(vec![order("a", "binance", "BTCUSDT")]);
    let rows = order_rows(&s, None, None);
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows)).unwrap();
    assert!(v[0]["account"].is_null());
}

#[test]
fn an_empty_result_is_an_empty_array_and_not_an_error() {
    let rows = order_rows(&snap_with(vec![]), None, None);
    assert_eq!(rows_json(&rows).trim(), "[]");
}

#[test]
fn the_table_names_every_column_even_when_there_are_no_rows() {
    let table = rows_table(&[]);
    for header in ["COID", "VENUE", "SYMBOL", "SIDE", "QTY", "STATUS"] {
        assert!(table.contains(header), "the header row must survive an empty result");
    }
}

// ---- PositionRow ----

use vike_tradehub_client::wire::WireTradingState;

/// A venue block with every field the type requires (no `Default` derive), the balance/margin
/// numbers zeroed since this module's rows never read them.
fn venue_block(
    venue: &str,
    account: Option<&str>,
    positions: Vec<WirePositionView>,
) -> WireVenueBlock {
    WireVenueBlock {
        venue: venue.into(),
        balance: 0.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
        funding_paid: 0.0,
        equity: 0.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_bp: 0.0,
        trading_state: WireTradingState::Active,
        positions,
        account: account.map(str::to_string),
        route_key: String::new(),
        symbols: Vec::new(),
        mode: None,
    }
}

fn position_view(symbol: &str, size: f64, avg_px: f64, unrealized: f64) -> WirePositionView {
    WirePositionView {
        // Deliberately left blank: `position_row` reads venue off the ENCLOSING block, never off
        // this field, so a fixture that sets it would test nothing extra and could mislead a
        // reader into thinking the row reads it here.
        venue: String::new(),
        symbol: symbol.into(),
        position_side: "BOTH".into(),
        size,
        avg_px,
        unrealized,
        leverage: 0.0,
        liq_price: 0.0,
    }
}

fn snap_with_venues(venues: Vec<WireVenueBlock>) -> WireSnapshot {
    let mut s = WireSnapshot::empty();
    s.venues = venues;
    s
}

#[test]
fn a_venue_filter_selects_only_that_venues_positions() {
    let s = snap_with_venues(vec![
        venue_block("binance", None, vec![position_view("BTCUSDT", 1.0, 100.0, 5.0)]),
        venue_block("okx", None, vec![position_view("ETHUSDT", 2.0, 200.0, -3.0)]),
    ]);
    let book = crate::cmd::trade::selector::parse("binance").unwrap();
    let rows = position_rows(&s, Some(&book), None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].venue, "binance");
    assert_eq!(rows[0].symbol, "BTCUSDT");
}

/// **The property `order_rows` cannot have**: a labelled book genuinely narrows a position read,
/// because [`WireVenueBlock`] carries the account this compares against with no ambiguity (an
/// order row's optional [`WireOrderView`] `account` is ambiguous when absent). Two blocks of the
/// SAME venue, one unlabelled and one `ALT` — each selector must reach only its own block's row,
/// never the other's.
#[test]
fn a_labelled_book_narrows_to_that_accounts_positions_rather_than_refusing() {
    let s = snap_with_venues(vec![
        venue_block("binance", None, vec![position_view("BTCUSDT", 1.0, 100.0, 5.0)]),
        venue_block("binance", Some("ALT"), vec![position_view("ETHUSDT", 2.0, 200.0, -3.0)]),
    ]);
    let default_book = crate::cmd::trade::selector::parse("binance").unwrap();
    let default_rows = position_rows(&s, Some(&default_book), None);
    assert_eq!(default_rows.len(), 1, "{default_rows:?}");
    assert_eq!(default_rows[0].symbol, "BTCUSDT");
    assert!(default_rows[0].account.is_none());

    let alt_book = crate::cmd::trade::selector::parse("binance/ALT").unwrap();
    let alt_rows = position_rows(&s, Some(&alt_book), None);
    assert_eq!(alt_rows.len(), 1, "{alt_rows:?}");
    assert_eq!(alt_rows[0].symbol, "ETHUSDT");
    assert_eq!(alt_rows[0].account.as_deref(), Some("ALT"));
}

#[test]
fn positions_json_is_flat_and_every_row_carries_its_own_venue_and_account() {
    let s = snap_with_venues(vec![venue_block(
        "binance",
        None,
        vec![position_view("BTCUSDT", 1.0, 100.0, 5.0)],
    )]);
    let rows = position_rows(&s, None, None);
    let v: serde_json::Value = serde_json::from_str(&positions_json(&rows)).unwrap();
    let arr = v.as_array().expect("the document is an ARRAY, never a nesting");
    assert_eq!(arr[0]["venue"], "binance");
    assert!(
        arr[0].get("account").is_some(),
        "the account KEY is always present, null or not: {arr:?}"
    );
    assert!(arr[0]["account"].is_null(), "the unlabelled account is wire-null: {arr:?}");
}

/// The evidence for this task's account-attribution decision: `account` is NOT unconditionally
/// null the way `OrderRow::account` is — a venue block that names a label produces a populated
/// column, because [`WireVenueBlock::account`] genuinely carries one.
#[test]
fn the_account_is_populated_when_the_venue_block_names_one() {
    let s = snap_with_venues(vec![venue_block(
        "binance",
        Some("ALT"),
        vec![position_view("ETHUSDT", 2.0, 200.0, -3.0)],
    )]);
    let rows = position_rows(&s, None, None);
    let v: serde_json::Value = serde_json::from_str(&positions_json(&rows)).unwrap();
    assert_eq!(v[0]["account"], "ALT");
}

#[test]
fn positions_an_empty_result_is_an_empty_array_and_not_an_error() {
    let rows = position_rows(&snap_with_venues(vec![]), None, None);
    assert_eq!(positions_json(&rows).trim(), "[]");
}

#[test]
fn positions_table_names_every_column_even_when_there_are_no_rows() {
    let table = positions_table(&[]);
    for header in ["VENUE", "ACCOUNT", "SYMBOL", "SIZE", "AVG_PX", "UNREALIZED"] {
        assert!(table.contains(header), "the header row must survive an empty result: {table}");
    }
    assert!(!table.contains('\u{03a3}'), "an empty result has nothing to aggregate: {table}");
}

// The two tests below are this task's brief, verbatim (Step 1) — they exercise `positions_table`
// / `positions_json` directly over hand-built rows, so they need no wire fixture at all.

#[test]
fn a_total_row_is_marked_as_a_total_and_never_styled_as_one_books_number() {
    let rows = vec![
        PositionRow {
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            size: 0.42,
            avg_px: 61_204.0,
            unrealized: 182.40,
        },
        PositionRow {
            venue: "okx".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            size: -3.10,
            avg_px: 2_988.0,
            unrealized: -44.10,
        },
    ];
    let table = positions_table(&rows);
    assert!(
        table.contains('\u{03a3}'),
        "an aggregate must be visually marked as an aggregate - a summed number that looks like \
             one book's balance is a new way to misread a position: {table}"
    );
}

#[test]
fn the_json_form_carries_no_total_row() {
    let rows = vec![PositionRow {
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        size: 0.42,
        avg_px: 61_204.0,
        unrealized: 182.40,
    }];
    let v: serde_json::Value = serde_json::from_str(&positions_json(&rows)).unwrap();
    assert_eq!(
        v.as_array().unwrap().len(),
        1,
        "a total is a RENDERING, never a row a machine folds"
    );
}

// ---- MountRow ----

fn mount(
    strategy: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    live: bool,
    asset_class: Option<&str>,
) -> WireMountRow {
    WireMountRow {
        strategy: strategy.into(),
        params: format!("venue={venue} symbol={symbol}"),
        live,
        venue: venue.into(),
        symbol: symbol.into(),
        interval: interval.into(),
        mount_id: String::new(),
        typed_params: None,
        asset_class: asset_class.map(str::to_string),
    }
}

fn status_with(mounts: Vec<WireMountRow>) -> WireStrategyStatus {
    WireStrategyStatus {
        identity: vike_tradehub_client::wire::WireNodeIdentity {
            name: "hub-a".into(),
            strategy: String::new(),
            params: String::new(),
            live: false,
            build: String::new(),
            advertise_addr: String::new(),
        },
        effective_params: String::new(),
        mounts,
    }
}

#[test]
fn mount_rows_projects_every_field() {
    let status = status_with(vec![mount(
        "spread_maker",
        "polymarket",
        "TOK-A",
        "1m",
        true,
        Some("PredictionMarket"),
    )]);
    let rows = mount_rows(&status);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].strategy, "spread_maker");
    assert_eq!(rows[0].venue, "polymarket");
    assert_eq!(rows[0].symbol, "TOK-A");
    assert_eq!(rows[0].interval, "1m");
    assert!(rows[0].live);
    assert_eq!(rows[0].asset_class.as_deref(), Some("PredictionMarket"));
}

#[test]
fn mounts_table_names_every_column_even_when_there_are_no_rows() {
    let table = mounts_table(&[], true);
    for header in ["STRATEGY", "VENUE", "SYMBOL", "INTERVAL", "MODE", "PRODUCT", "PARAMS"] {
        assert!(table.contains(header), "the header row must survive an empty result: {table}");
    }
}

/// The em-dash placeholder for an unnamed product, and the column stays aligned across an ASCII
/// cell and a multi-byte one — the same property `crate::cmd::trade::status::registry_lines`
/// pins for the identical cell. Only reachable under `knows_class = true`: an em dash is a claim
/// ("this mount names no product") this function may make only once it knows the node COULD have
/// said otherwise.
#[test]
fn mounts_table_shows_an_em_dash_for_an_unnamed_product_when_the_node_knows_the_capability() {
    let status = status_with(vec![
        mount("spread_maker", "binance", "BTCUSDT", "1m", false, None),
        mount("np", "okx", "ETHUSDT", "1m", true, Some("CryptoPerp")),
    ]);
    let rows = mount_rows(&status);
    let table = mounts_table(&rows, true);
    assert!(table.contains('—'), "an unnamed product renders as an em dash, not a blank: {table}");
    assert!(table.contains("CryptoPerp"), "{table}");
    assert!(table.contains("LIVE") && table.contains("paper"), "{table}");
}

/// **The Critical fix this round covers.** Against a node that does NOT advertise
/// `FEATURE_MOUNT_CLASS`, the PRODUCT column must be ABSENT — not filled with em dashes — and
/// every other column must still render correctly. An em dash here would tell an operator on an
/// old node "this mount names no product," which is indistinguishable from "your daemon predates
/// this feature" only to the code, never to the reader looking at a screen full of dashes; the
/// consequence `WireMountRow::asset_class`'s own doc names is an operator sent to upgrade a node
/// that may already be current.
#[test]
fn mounts_table_omits_the_product_column_when_the_node_does_not_know_the_capability() {
    let status = status_with(vec![mount("spread_maker", "binance", "BTCUSDT", "1m", true, None)]);
    let rows = mount_rows(&status);
    let table = mounts_table(&rows, false);
    assert!(!table.contains("PRODUCT"), "the column must be ABSENT, not dashed: {table}");
    assert!(
        !table.contains('—'),
        "no placeholder may stand in for a column this function cannot answer: {table}"
    );
    for header in ["STRATEGY", "VENUE", "SYMBOL", "INTERVAL", "MODE", "PARAMS"] {
        assert!(table.contains(header), "every other column still renders: {table}");
    }
    assert!(table.contains("spread_maker") && table.contains("LIVE"), "{table}");
}

/// The header-survives-an-empty-result rule holds under EITHER `knows_class` value — there is no
/// row to read the capability off, so the caller's own explicit parameter is what makes this
/// answerable at all (see `mounts_table`'s own doc).
#[test]
fn mounts_table_omits_the_product_column_over_an_empty_result_too() {
    let table = mounts_table(&[], false);
    assert!(!table.contains("PRODUCT"), "{table}");
    for header in ["STRATEGY", "VENUE", "SYMBOL", "INTERVAL", "MODE", "PARAMS"] {
        assert!(table.contains(header), "the header row must survive an empty result: {table}");
    }
}

#[test]
fn mounts_json_is_flat_with_no_nesting_and_no_total_row() {
    let status = status_with(vec![mount("spread_maker", "polymarket", "TOK-A", "1m", false, None)]);
    let rows = mount_rows(&status);
    let v: serde_json::Value = serde_json::from_str(&mounts_json(&rows, true)).unwrap();
    let arr = v.as_array().expect("the document is an ARRAY, never a nesting");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["strategy"], "spread_maker");
    assert_eq!(arr[0]["venue"], "polymarket");
    assert!(arr[0]["asset_class"].is_null());
}

/// **Fix round 2, Finding 4.** `mounts_json`'s `asset_class_known` key must carry the SAME
/// `knows_class` fact `mounts_table`'s PRODUCT-column gate does, per row — a machine consumer
/// reading `"asset_class": null` with no marker cannot tell "this node cannot say" from "this
/// mount genuinely names none," which is the identical three-way ambiguity the table's em dash
/// used to hide, and worse for an unattended agent than for a human glancing at a dash.
#[test]
fn mounts_json_carries_asset_class_known_in_both_states() {
    let status = status_with(vec![mount("spread_maker", "polymarket", "TOK-A", "1m", false, None)]);
    let rows = mount_rows(&status);

    let known: serde_json::Value = serde_json::from_str(&mounts_json(&rows, true)).unwrap();
    assert_eq!(known[0]["asset_class_known"], true, "{known}");

    let unknown: serde_json::Value = serde_json::from_str(&mounts_json(&rows, false)).unwrap();
    assert_eq!(unknown[0]["asset_class_known"], false, "{unknown}");
}

#[test]
fn mounts_an_empty_result_is_an_empty_array_and_not_an_error() {
    let rows = mount_rows(&status_with(vec![]));
    assert_eq!(mounts_json(&rows, true).trim(), "[]");
}
