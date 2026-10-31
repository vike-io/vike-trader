use super::*;
use vike_tradehub_client::wire::{WireOrderView, WireSnapshot};

fn snap_with(orders: Vec<WireOrderView>) -> WireSnapshot {
    let mut s = WireSnapshot::empty();
    s.orders = orders;
    s
}

fn order(coid: &str, venue: &str, symbol: &str) -> WireOrderView {
    WireOrderView {
        data_delay: None,
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
        reason: None,
        age_ms: None,
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
        tier: None,
        contract_values: Vec::new(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: String::new(),
        ledger_ccy: String::new(),
        ccy_assumed: false,
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

/// `position ls` is the other table the engine's float tails reached (the live node printed a Binance spot
/// position as `0.000010000000000000026`): a size and an average price are `n * step` products one ULP off
/// their decimal, and a Σ of rows adds its own. The table shortens them like the order tables do, while
/// `--json` keeps the engine's exact value (a script parses the same f64 back).
#[test]
fn positions_table_spells_a_grid_multiple_without_its_float_tail_and_json_keeps_the_value() {
    let noisy_px = 8_092_593.0_f64 * 0.01;
    let noisy_size = 7.0_f64 * 0.01 * 0.01;
    let noisy_pnl = 0.1_f64 + 0.2;
    assert_eq!(noisy_px.to_string(), "80925.93000000001", "the fixture is a measured value");
    assert_ne!(noisy_size.to_string(), "0.0007", "…and the size is off by an ULP too");
    assert_eq!(noisy_pnl.to_string(), "0.30000000000000004");
    let rows = vec![PositionRow {
        venue: "okx".into(),
        account: None,
        symbol: "BTC-USDT-SWAP".into(),
        size: noisy_size,
        avg_px: noisy_px,
        unrealized: noisy_pnl,
        ccy: String::new(),
    }];

    let table = positions_table(&rows);
    let row = table.lines().nth(1).expect("one position row");
    for cell in ["0.0007", "80925.93", "0.3"] {
        assert!(row.split_whitespace().any(|c| c == cell), "{cell} in the row:\n{table}");
    }
    let total = table.lines().last().expect("the Σ line");
    assert!(total.ends_with("balance): 0.3"), "the Σ is shortened too: {total}");
    assert!(!table.contains("0000000"), "no float tail anywhere:\n{table}");

    let v: serde_json::Value = serde_json::from_str(&positions_json(&rows)).unwrap();
    assert_eq!(v[0]["size"].as_f64(), Some(noisy_size), "--json is the engine's value");
    assert_eq!(v[0]["avg_px"].as_f64(), Some(noisy_px));
    assert_eq!(v[0]["unrealized"].as_f64(), Some(noisy_pnl));
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
            ccy: String::new(),
        },
        PositionRow {
            venue: "okx".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            size: -3.10,
            avg_px: 2_988.0,
            unrealized: -44.10,
            ccy: String::new(),
        },
    ];
    let table = positions_table(&rows);
    assert!(
        table.contains('\u{03a3}'),
        "an aggregate must be visually marked as an aggregate - a summed number that looks like \
             one book's balance is a new way to misread a position: {table}"
    );
    assert_eq!(table.matches('\u{03a3}').count(), 1, "one currency: the one line it always had");
    assert!(!table.contains("CCY"), "one currency: no currency column: {table}");
}

/// ⚠ **A Σ line adds ONE currency** (decision 0183, item 13): a position of a EUR account and a dollar one
/// are two sums, each named by its code, and each row says which it is in. The JSON row names the currency
/// only off the dollar.
#[test]
fn the_total_unrealized_is_one_line_per_currency_never_a_sum_across_them() {
    let row = |venue: &str, symbol: &str, unrealized: f64, ccy: &str| PositionRow {
        venue: venue.into(),
        account: None,
        symbol: symbol.into(),
        size: 1.0,
        avg_px: 1.0,
        unrealized,
        ccy: ccy.into(),
    };
    let rows = vec![
        row("binance", "BTCUSDT", 100.0, ""),
        row("fxcm", "EURUSD", -30.0, "EUR"),
        row("okx", "ETHUSDT", 50.0, ""),
        row("fxcm", "GBPUSD", -20.0, "EUR"),
    ];
    let table = positions_table(&rows);
    assert!(table.contains("CCY"), "{table}");
    assert!(table.contains("across 2 rows in USD (NOT one account's balance): 150"), "{table}");
    assert!(table.contains("across 2 rows in EUR (NOT one account's balance): -50"), "{table}");
    assert!(!table.contains("balance): 100"), "never the raw sum of all four rows: {table}");

    let json: serde_json::Value = serde_json::from_str(&positions_json(&rows)).unwrap();
    assert!(json[0].get("ledger_ccy").is_none(), "the dollar row is the row it always was: {json}");
    assert_eq!(json[1]["ledger_ccy"], "EUR");
}

/// The block's `ledger_ccy` reaches the row (the node's own key, EMPTY for the dollar group).
#[test]
fn a_position_row_carries_its_blocks_currency_group() {
    let mut eur = venue_block("fxcm", None, vec![position_view("EURUSD", 1.0, 1.1, 5.0)]);
    eur.ledger_ccy = "EUR".into();
    let usd = venue_block("oanda", None, vec![position_view("EURUSD", 1.0, 1.1, 5.0)]);
    let rows = position_rows(&snap_with_venues(vec![eur, usd]), None, None);
    assert_eq!(rows.iter().map(|r| r.ccy.as_str()).collect::<Vec<_>>(), ["EUR", ""]);
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
        ccy: String::new(),
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
        latched: false,
    }
}

fn status_with(mounts: Vec<WireMountRow>) -> WireStrategyStatus {
    WireStrategyStatus {
        identity: vike_tradehub_client::wire::WireHubIdentity {
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

/// **A LATCHED mount says so** (decision 0239): its row ends with the note that names the verb that clears it and the
/// id to give it, in the table, in `trade status`'s registry lines and (as `"latched": true`) in the JSON; an
/// unlatched row is byte-identical to before the flag existed and writes no `latched` key, because a node that
/// predates the flag writes none and silence must never read as "not latched".
#[test]
fn a_latched_mount_row_names_the_verb_that_clears_it_and_a_free_one_changes_nothing() {
    let mut latched = mount("grid", "binance", "BTCUSDT", "1m", true, Some("CryptoPerp"));
    latched.latched = true;
    latched.mount_id = "binance__BTCUSDT__1m".into();
    let free = mount("grid", "binance", "ETHUSDT", "1m", true, Some("CryptoPerp"));
    let status = status_with(vec![latched, free]);
    let rows = mount_rows(&status);

    for knows_class in [true, false] {
        let table = mounts_table(&rows, knows_class);
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[1].contains("BUDGET LATCHED"), "{table}");
        assert!(lines[1].contains("trade mount-unlatch binance__BTCUSDT__1m"), "{table}");
        assert!(!lines[2].contains("LATCHED"), "the free row is untouched: {table}");
        let registry = crate::cmd::trade::status::registry_lines(&status, knows_class).join("\n");
        assert!(registry.contains("BUDGET LATCHED"), "{registry}");
        assert_eq!(registry.matches("LATCHED").count(), 1, "{registry}");
    }

    let v: serde_json::Value = serde_json::from_str(&mounts_json(&rows, true)).unwrap();
    assert_eq!(v[0]["latched"], true, "{v}");
    assert_eq!(v[0]["mount_id"], "binance__BTCUSDT__1m", "{v}");
    assert!(v[1].get("latched").is_none(), "no key for a mount that is not latched: {v}");
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

/// `order ls` names WHY a refused order was refused: the JSON row always carries a `reason` key
/// (null when the node sent none) and the table ends with a REASON column.
#[test]
fn a_refused_order_shows_its_reason_in_json_and_in_the_table() {
    let mut refused = order("a", "ctrader", "EURUSD");
    refused.status = "REJECTED".into();
    refused.reason = Some("INVALID_REQUEST: more digits than symbol allows".into());
    let s = snap_with(vec![refused, order("b", "ctrader", "GBPUSD")]);
    let rows = order_rows(&s, None, None);

    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows)).unwrap();
    assert_eq!(v[0]["reason"], "INVALID_REQUEST: more digits than symbol allows");
    assert!(v[1].get("reason").is_some_and(serde_json::Value::is_null), "the key is always there");

    let table = rows_table(&rows);
    let mut lines = table.lines();
    assert!(lines.next().unwrap().ends_with(" REASON"), "{table}");
    assert!(lines.next().unwrap().ends_with(" INVALID_REQUEST: more digits than symbol allows"));
    assert!(lines.next().unwrap().ends_with(" -"), "an order with no reason reads `-`");
}

/// `order ls` is the table the OKX demo showed `80925.93000000001` in (the venue held `80925.93`): the
/// engine's price is `round_to_step`'s `n * tick`, one ULP off its grid point. The table shortens it
/// (as the preview does), while `--json` keeps the engine's exact value, a number a script parses back.
#[test]
fn order_ls_spells_a_grid_multiple_without_its_float_tail_and_json_keeps_the_value() {
    let noisy_price = 8_092_593.0_f64 * 0.01;
    let noisy_qty = 7.0_f64 * 0.01 * 0.01;
    assert_eq!(noisy_price.to_string(), "80925.93000000001");
    let mut o = order("b99okxmkt", "okx", "BTC-USDT-SWAP");
    o.price = Some(noisy_price);
    o.qty = noisy_qty;
    o.filled_qty = noisy_qty;
    let rows = order_rows(&snap_with(vec![o]), None, None);

    let table = rows_table(&rows);
    let row = table.lines().nth(1).expect("one order row");
    assert!(row.split_whitespace().any(|c| c == "80925.93"), "{table}");
    assert_eq!(row.split_whitespace().filter(|c| *c == "0.0007").count(), 2, "{table}");
    assert!(!table.contains("80925.93000000001"), "{table}");

    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows)).unwrap();
    assert_eq!(
        v[0]["price"].as_f64(),
        Some(noisy_price),
        "--json is the engine's value, untouched"
    );
}

/// A venue's free text never breaks the one-line-per-order table.
#[test]
fn a_multiline_reason_stays_on_one_table_line() {
    let mut refused = order("a", "ctrader", "EURUSD");
    refused.reason = Some("first line\r\nsecond line".into());
    let table = rows_table(&order_rows(&snap_with(vec![refused]), None, None));
    assert_eq!(table.lines().count(), 2, "header + one row: {table}");
    assert!(table.ends_with("first line  second line"), "{table}");
}
