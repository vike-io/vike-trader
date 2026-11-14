use super::*;
use vike_tradehub_client::wire::{CoreSnapshotWire, OrderViewWire};

fn snap_with(orders: Vec<OrderViewWire>) -> CoreSnapshotWire {
    let mut s = CoreSnapshotWire::empty();
    s.orders = orders;
    s
}

fn order(order_client_id: &str, venue: &str, symbol: &str) -> OrderViewWire {
    OrderViewWire {
        data_delay: None,
        order_client_id: order_client_id.into(),
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
        average_fill_price: 0.0,
        reason: None,
        age_ms: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

#[test]
fn a_venue_filter_selects_only_that_venues_orders() {
    let s = snap_with(vec![order("a", "binance", "BTCUSDT"), order("b", "okx", "BTCUSDT")]);
    let book = crate::cmd::trade::selector::parse("binance").unwrap();
    let rows = order_rows(&s, Some(&book), None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].order_client_id, "a");
}

// ⚠ Named in lowercase ("flat", not "FLAT"): the literal brief spelling trips rustc's
// `non_snake_case` lint, and this crate's `cargo clippy -- -D warnings` gate denies every
// warning, that one included. The behaviour under test is unchanged.
#[test]
fn json_is_flat_and_every_row_carries_its_own_venue_and_account() {
    let s = snap_with(vec![order("a", "binance", "BTCUSDT")]);
    let rows = order_rows(&s, None, None);
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    let arr = v.as_array().expect("the document is an ARRAY, never a nesting");
    assert_eq!(arr[0]["venue"], "binance");
    assert!(
        arr[0].get("account").is_some(),
        "the account KEY is always present - absent means the wire named none, and a machine \
             reader must not have to infer which book a row belongs to"
    );
}

/// An order with no `account` beside a block of its venue, on a node that says `tier` or not.
fn venue_with_tier(account: Option<&str>, tier: Option<&str>) -> VenueBlockWire {
    VenueBlockWire { tier: tier.map(str::to_string), ..venue_block("bybit", account, vec![]) }
}

/// **Each row names its account**: a labelled order says which; an unnamed one on a node that names
/// them (any block carries a `tier`) is the default account, in both output shapes.
#[test]
fn every_order_row_names_its_account() {
    let mut s = snap_with(vec![
        OrderViewWire { account: Some("DEMO".into()), ..order("a", "bybit", "BTCUSDT") },
        OrderViewWire { account: Some("LIVE".into()), ..order("b", "bybit", "BTCUSDT") },
        order("c", "bybit", "BTCUSDT"),
    ]);
    s.venues = vec![venue_with_tier(Some("DEMO"), Some("demo"))];
    let rows = order_rows(&s, None, None);
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert_eq!(v[0]["account"], "DEMO");
    assert_eq!(v[1]["account"], "LIVE");
    assert!(v[2]["account"].is_null(), "the unlabelled account is wire-null: {v}");
    assert!(rows.iter().all(|r| r.account_known), "{rows:?}");

    let table = rows_table(&rows);
    let cell = |order_client_id: &str| {
        let line = table.lines().find(|l| l.starts_with(order_client_id)).expect(order_client_id);
        line.split_whitespace().nth(2).expect("the ACCOUNT cell").to_string()
    };
    assert_eq!((cell("a"), cell("b"), cell("c")), ("DEMO".into(), "LIVE".into(), "-".into()));
}

/// **An older node's silence is never the default account** (`OrderViewWire::account`'s doc): with no
/// `tier` on any block, an unnamed order is the account of the one block its venue has, and with
/// several, or none, nobody can say, which the row says.
#[test]
fn an_unnamed_order_on_an_older_node_is_its_one_blocks_or_unknown() {
    let mut s = snap_with(vec![order("a", "bybit", "BTCUSDT")]);
    s.venues = vec![venue_with_tier(Some("ALT"), None)];
    let row = order_rows(&s, None, None).remove(0);
    assert_eq!((row.account.as_deref(), row.account_known), (Some("ALT"), true));

    s.venues.push(venue_with_tier(None, None));
    let rows = order_rows(&s, None, None);
    assert_eq!((rows[0].account.as_deref(), rows[0].account_known), (None, false));
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert!(v[0]["account"].is_null());
    assert_eq!(v[0]["account_known"], false);
    assert!(rows_table(&rows).lines().nth(1).expect("the row").contains(" ? "), "{rows:?}");

    s.venues.clear();
    assert!(!order_rows(&s, None, None)[0].account_known, "no block says nothing either");
}

/// **A resting trailing order shows its terms**: the trail, a trailing limit's offset and the activation, in
/// the table's last cell and as three JSON keys on its row only; every other row keeps its bytes (a stop's
/// trigger is not an activation, and a limit row grows no key).
#[test]
fn a_trailing_order_shows_its_trail_offset_and_activation_and_no_other_row_changes() {
    let trailing = OrderViewWire {
        order_type: "trailing_limit".into(),
        price: None,
        trigger_price: Some(70_000.0),
        trail: Some(50.0),
        limit_offset: Some(5.0),
        ..order("t", "okx", "BTC-USDT-SWAP")
    };
    let stop = OrderViewWire {
        order_type: "stop".into(),
        price: None,
        trigger_price: Some(60_000.0),
        ..order("s", "okx", "BTC-USDT-SWAP")
    };
    let limit = order("l", "okx", "BTC-USDT-SWAP");
    let plain_before =
        rows_json(&order_rows(&snap_with(vec![limit.clone(), stop.clone()]), None, None), &[]);
    let rows = order_rows(&snap_with(vec![trailing, limit, stop]), None, None);

    let table = rows_table(&rows);
    let line = |order_client_id: &str| {
        table.lines().find(|l| l.starts_with(order_client_id)).expect(order_client_id).to_string()
    };
    assert!(line("t").ends_with(" trail=50 offset=5 activation=70000"), "{table}");
    assert!(line("l").ends_with(" -") && line("s").ends_with(" -"), "{table}");

    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert_eq!(v[0]["trail"], 50.0);
    assert_eq!(v[0]["limit_offset"], 5.0);
    assert_eq!(v[0]["activation"], 70_000.0);
    let after: serde_json::Value = serde_json::Value::Array(v.as_array().unwrap()[1..].to_vec());
    let before: serde_json::Value = serde_json::from_str(&plain_before).unwrap();
    assert_eq!(after, before, "the limit and the stop rows keep their bytes");
}

#[test]
fn an_empty_result_is_an_empty_array_and_not_an_error() {
    let rows = order_rows(&snap_with(vec![]), None, None);
    assert_eq!(rows_json(&rows, &[]).trim(), "[]");
}

#[test]
fn the_table_names_every_column_even_when_there_are_no_rows() {
    let table = rows_table(&[]);
    for header in ["ORDER_CLIENT_ID", "VENUE", "SYMBOL", "SIDE", "QTY", "STATUS"] {
        assert!(table.contains(header), "the header row must survive an empty result");
    }
}

/// **`trade order ls` puts a v2 id's words beside it** (decision 0270's decode surfaces), `-` beside a v1 id in the
/// same table, every later column still aligned; a table of v1 ids alone has no WORDS column and keeps its bytes.
#[test]
fn trade_order_ls_shows_a_v2_ids_words_beside_it() {
    let v1 = order("a", "bybit", "BTCUSDT");
    assert!(!rows_table(&order_rows(&snap_with(vec![v1.clone()]), None, None)).contains("WORDS"));
    let id = "C00000Bq7w2e9r4t1y6OL";
    let rows = order_rows(&snap_with(vec![order(id, "bybit", "BTCUSDT"), v1]), None, None);
    let table = rows_table(&rows);
    assert!(table.lines().next().expect("the header").contains(" WORDS "), "{table}");
    let line = |order_client_id: &str| {
        table.lines().find(|l| l.starts_with(order_client_id)).expect(order_client_id).to_string()
    };
    let said = "CLI (by hand) · no strategy · account #11 · opens or adds long";
    assert!(line(id).contains(said), "{table}");
    assert_eq!(line("a ").split_whitespace().nth(1), Some("-"), "{table}");
    let venue_at = |l: String| l.split(" bybit ").next().expect("a cell").chars().count();
    assert_eq!(venue_at(line(id)), venue_at(line("a ")), "the columns after it align:\n{table}");

    // `--json`: an additive `words` key on the v2 row only.
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert_eq!(v[0]["words"], said, "{v}");
    assert!(v[1].get("words").is_none(), "a v1 row keeps its bytes: {v}");
}

// ---- PositionRow ----

use vike_model::TradingState;

/// A venue block with every field the type requires (no `Default` derive), the balance/margin
/// numbers zeroed since this module's rows never read them.
fn venue_block(
    venue: &str,
    account: Option<&str>,
    positions: Vec<PositionViewWire>,
) -> VenueBlockWire {
    VenueBlockWire {
        account_code: None,
        order_client_id_shape: None,
        venue: venue.into(),
        balance: 0.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
        funding_paid: 0.0,
        equity: 0.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_buying_power: 0.0,
        trading_state: TradingState::Active,
        positions,
        account: account.map(str::to_string),
        route_key: String::new(),
        symbols: Vec::new(),
        tier: None,
        contract_values: Vec::new(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: String::new(),
        ledger_currency: String::new(),
        currency_assumed: false,
        balance_is_own_book: false,
    }
}

fn position_view(symbol: &str, size: f64, avg_px: f64, unrealized: f64) -> PositionViewWire {
    PositionViewWire {
        // Deliberately left blank: `position_row` reads venue off the ENCLOSING block, never off
        // this field, so a fixture that sets it would test nothing extra and could mislead a
        // reader into thinking the row reads it here.
        venue: String::new(),
        symbol: symbol.into(),
        position_side: "BOTH".into(),
        size,
        average_price: avg_px,
        unrealized,
        leverage: 0.0,
        liquidation_price: 0.0,
    }
}

fn snap_with_venues(venues: Vec<VenueBlockWire>) -> CoreSnapshotWire {
    let mut s = CoreSnapshotWire::empty();
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

/// The venue filter reaches every account of the venue, each row naming its own off its
/// [`VenueBlockWire`], so an open position on one account cannot hide behind a flat one on another.
/// Narrowing to the account a book names is `position ls`'s `run_ls`, pinned over the shipped binary
/// by `crates/vike-cli/tests/shipped_cli/trade_plane_cli.rs`'s
/// `position_ls_of_a_bare_venue_reads_every_account_and_a_book_narrows`.
#[test]
fn the_venue_filter_reaches_every_account_and_each_row_names_its_own() {
    let s = snap_with_venues(vec![
        venue_block("binance", None, vec![position_view("BTCUSDT", 1.0, 100.0, 5.0)]),
        venue_block("binance", Some("ALT"), vec![position_view("ETHUSDT", 2.0, 200.0, -3.0)]),
        venue_block("okx", None, vec![position_view("SOLUSDT", 3.0, 30.0, 1.0)]),
    ]);
    let book = crate::cmd::trade::selector::parse("BINANCE").unwrap();
    let rows = position_rows(&s, Some(&book), None);
    let read: Vec<_> = rows.iter().map(|r| (r.symbol.as_str(), r.account.as_deref())).collect();
    assert_eq!(read, [("BTCUSDT", None), ("ETHUSDT", Some("ALT"))]);
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
/// column, because [`VenueBlockWire::account`] genuinely carries one.
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

/// `position ls` lists OPEN positions: an engine keeps a size-0 entry for every symbol it ever traded, an expired
/// contract's included, and a flat row is no position. ⚠ KILL PROOF: drop the size filter in `position_rows`.
#[test]
fn a_flat_entry_is_not_listed_as_a_position() {
    let s = snap_with_venues(vec![venue_block(
        "bybit",
        Some("DEMO"),
        vec![
            position_view("ETHUSDT-06NOV26", 0.0, 0.0, 0.0),
            position_view("BTCUSDT", 0.5, 100.0, 1.0),
        ],
    )]);
    let rows = position_rows(&s, None, None);
    assert_eq!(rows.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), ["BTCUSDT"]);
    assert!(position_rows(&s, None, Some("ETHUSDT-06NOV26")).is_empty());
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
        average_price: noisy_px,
        unrealized: noisy_pnl,
        currency: String::new(),
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
    assert_eq!(v[0]["average_price"].as_f64(), Some(noisy_px));
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
            average_price: 61_204.0,
            unrealized: 182.40,
            currency: String::new(),
        },
        PositionRow {
            venue: "okx".into(),
            account: None,
            symbol: "ETHUSDT".into(),
            size: -3.10,
            average_price: 2_988.0,
            unrealized: -44.10,
            currency: String::new(),
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
    let row = |venue: &str, symbol: &str, unrealized: f64, currency_value: &str| PositionRow {
        venue: venue.into(),
        account: None,
        symbol: symbol.into(),
        size: 1.0,
        average_price: 1.0,
        unrealized,
        currency: currency_value.into(),
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
    assert!(
        json[0].get("ledger_currency").is_none(),
        "the dollar row is the row it always was: {json}"
    );
    assert_eq!(json[1]["ledger_currency"], "EUR");
}

/// The block's `ledger_currency` reaches the row (the node's own key, EMPTY for the dollar group).
#[test]
fn a_position_row_carries_its_blocks_currency_group() {
    let mut eur = venue_block("fxcm", None, vec![position_view("EURUSD", 1.0, 1.1, 5.0)]);
    eur.ledger_currency = "EUR".into();
    let usd = venue_block("oanda", None, vec![position_view("EURUSD", 1.0, 1.1, 5.0)]);
    let rows = position_rows(&snap_with_venues(vec![eur, usd]), None, None);
    assert_eq!(rows.iter().map(|r| r.currency.as_str()).collect::<Vec<_>>(), ["EUR", ""]);
}

#[test]
fn the_json_form_carries_no_total_row() {
    let rows = vec![PositionRow {
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        size: 0.42,
        average_price: 61_204.0,
        unrealized: 182.40,
        currency: String::new(),
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
) -> MountViewWire {
    MountViewWire {
        order_client_id_code: None,
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
        route_key: String::new(),
        tier: None,
        not_running: false,
    }
}

fn status_with(mounts: Vec<MountViewWire>) -> StrategyStatusWire {
    StrategyStatusWire {
        identity: vike_tradehub_client::wire::HubIdentityWire {
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
}

/// **The Critical fix this round covers.** Against a node that does NOT advertise
/// `FEATURE_MOUNT_CLASS`, the PRODUCT column must be ABSENT — not filled with em dashes — and
/// every other column must still render correctly. An em dash here would tell an operator on an
/// old node "this mount names no product," which is indistinguishable from "your daemon predates
/// this feature" only to the code, never to the reader looking at a screen full of dashes; the
/// consequence `MountViewWire::asset_class`'s own doc names is an operator sent to upgrade a node
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
    assert!(table.contains("spread_maker"), "{table}");
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
        assert!(lines[1].contains("trade strategy reset-budget binance__BTCUSDT__1m"), "{table}");
        assert!(!lines[2].contains("LATCHED"), "the free row is untouched: {table}");
        let registry =
            crate::cmd::trade::status::registry_lines(&status, knows_class, None).join("\n");
        assert!(registry.contains("BUDGET LATCHED"), "{registry}");
        assert_eq!(registry.matches("LATCHED").count(), 1, "{registry}");
    }

    let v: serde_json::Value = serde_json::from_str(&mounts_json(&rows, true)).unwrap();
    assert_eq!(v[0]["latched"], true, "{v}");
    assert_eq!(v[0]["mount_id"], "binance__BTCUSDT__1m", "{v}");
    assert!(v[1].get("latched").is_none(), "no key for a mount that is not latched: {v}");
}

/// A row that states no tier reads `?` (decision 0156's `MODE ?`): `live` is true for a demo account and false for a
/// stopped one, so neither `LIVE` nor `paper` follows from it.
#[test]
fn a_mount_row_that_states_no_tier_claims_none() {
    for live in [false, true] {
        assert_eq!(mount_mode(&mount("spread_maker", "bybit", "BTCUSDT", "1m", live, None)), "?");
    }
}

/// **A mount row names its account and the tier that account runs at NOW**, in both tables and the JSON, so a demo and
/// a live mount of one venue no longer look alike: `demo` is no longer `LIVE`, a stale `live` loses to the tier in
/// both directions of a tier change, and an account the node does not run says so whatever the row's other fields hold.
/// A node from before the fields writes neither, so its rows claim no tier.
#[test]
fn a_mount_row_names_its_account_and_the_tier_it_runs_at_now() {
    let row = |key: &str, tier: Option<&str>, live: bool, not_running: bool| MountViewWire {
        route_key: key.into(),
        tier: tier.map(str::to_string),
        not_running,
        ..mount("spread_maker", "bybit", "BTCUSDT", "1m", live, None)
    };
    let status = status_with(vec![
        row("bybit#DEMO", Some("demo"), true, false),
        row("bybit#LIVE", Some("live"), false, false),
        row("bybit#PAPER", Some("paper"), true, false),
        row("bybit#OFF", Some("demo"), false, true),
        row("", None, true, false),
        row("", None, false, false),
    ]);
    let words = [
        "demo (bybit#DEMO)",
        "LIVE (bybit#LIVE)",
        "paper (bybit#PAPER)",
        "NOT RUNNING (bybit#OFF)",
        "?",
        "?",
    ];

    let table = mounts_table(&mount_rows(&status), true);
    let registry = crate::cmd::trade::status::registry_lines(&status, true, None);
    for (i, word) in words.into_iter().enumerate() {
        let table_row = table.lines().nth(i + 1).expect("one row per mount");
        assert!(table_row.contains(&format!(" {word} ")), "{word}: {table}");
        let registry_row = &registry[i + 4];
        assert!(registry_row.contains(&format!("  {word} ")), "{word}: {registry:?}");
    }

    let v: serde_json::Value =
        serde_json::from_str(&mounts_json(&mount_rows(&status), true)).unwrap();
    assert_eq!(v[0]["route_key"], "bybit#DEMO", "{v}");
    assert_eq!(v[0]["tier"], "demo", "{v}");
    assert_eq!(v[0]["live"], true, "`live` keeps its name and meaning: {v}");
    assert!(v[0].get("not_running").is_none(), "only `true` is a claim: {v}");
    assert_eq!(v[3]["not_running"], true, "{v}");
    for key in ["route_key", "tier", "not_running"] {
        assert!(v[4].get(key).is_none(), "an older node's row gains no `{key}`: {v}");
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

/// `order ls` names WHY a refused order was refused: the JSON row always carries a `reason` key
/// (null when the node sent none) and the table ends with a REASON column.
#[test]
fn a_refused_order_shows_its_reason_in_json_and_in_the_table() {
    let mut refused = order("a", "ctrader", "EURUSD");
    refused.status = "REJECTED".into();
    refused.reason = Some("INVALID_REQUEST: more digits than symbol allows".into());
    let s = snap_with(vec![refused, order("b", "ctrader", "GBPUSD")]);
    let rows = order_rows(&s, None, None);

    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
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
    let noisy_quantity = 7.0_f64 * 0.01 * 0.01;
    assert_eq!(noisy_price.to_string(), "80925.93000000001");
    let mut o = order("b99okxmkt", "okx", "BTC-USDT-SWAP");
    o.price = Some(noisy_price);
    o.qty = noisy_quantity;
    o.filled_qty = noisy_quantity;
    let rows = order_rows(&snap_with(vec![o]), None, None);

    let table = rows_table(&rows);
    let row = table.lines().nth(1).expect("one order row");
    assert!(row.split_whitespace().any(|c| c == "80925.93"), "{table}");
    assert_eq!(row.split_whitespace().filter(|c| *c == "0.0007").count(), 2, "{table}");
    assert!(!table.contains("80925.93000000001"), "{table}");

    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
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

/// An order the node HOLDS until its release is listed after the venue's orders, in `trade order ls` (human and
/// `--json`) and in the REPL's `orders`, as `HELD` with what it waits for; a held bracket exit (no release) stays
/// out, as it always has, and a plain row's JSON gains no key.
#[test]
fn a_held_order_is_listed_as_held_with_its_release() {
    use vike_tradehub_client::wire::{OrderHeldViewWire, OrderReleasePriceWire, OrderReleaseWire};
    let held = |order_client_id: &str, release| OrderHeldViewWire {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        order_type: "limit".into(),
        price: Some(90.0),
        trigger_price: None,
        parent_order_id: None,
        account: Some("ALT".into()),
        release,
        waiting: None,
        release_venue: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    };
    let at = 4_097_217_600_000; // 2099-11-01T12:00:00Z
    let mut s = snap_with(vec![order("a", "binance", "BTCUSDT")]);
    s.held_exits = vec![
        held(
            "h1",
            Some(OrderReleaseWire {
                at,
                price: Some(OrderReleasePriceWire::Below(85.0)),
                symbol: None,
                venue: None,
            }),
        ),
        held("exit", None),
    ];
    let rows = order_rows(&s, None, None);
    assert_eq!(rows.iter().map(|r| r.order_client_id.as_str()).collect::<Vec<_>>(), ["a", "h1"]);
    assert_eq!((rows[1].status.as_str(), rows[1].account.as_deref()), (HELD, Some("ALT")));
    let table = rows_table(&rows);
    assert!(
        table.ends_with("held until 2099-11-01T12:00:00Z and the price at or below 85"),
        "{table}"
    );
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert!(v[0].get("release").is_none(), "a plain row keeps its keys: {}", v[0]);
    assert_eq!(v[1]["release"], serde_json::json!({"at": at, "price": {"below": 85.0}}));
    let repl = crate::cmd::trade::orders_table(&s, None);
    assert!(repl.contains(" HELD "), "{repl}");
    assert!(
        repl.ends_with("held until 2099-11-01T12:00:00Z and the price at or below 85"),
        "{repl}"
    );
    assert!(!repl.contains("exit"), "a bracket exit stays out: {repl}");
    // A condition on ANOTHER symbol says whose price it waits for.
    let other = OrderReleaseWire {
        at,
        price: Some(OrderReleasePriceWire::Above(3500.0)),
        symbol: Some("ETHUSDT".into()),
        venue: None,
    };
    assert_eq!(
        held_words(&other),
        "held until 2099-11-01T12:00:00Z and the price of ETHUSDT at or above 3500"
    );
    assert!(v[1].get("waiting").is_none(), "nothing to say yet, no key: {}", v[1]);
    // Its time came and it has no fresh price: why it still waits follows what it waits for, in the
    // table and the REPL, and in its OWN json key (`reason` is a refusal's and stays empty).
    let why = "no price of BTCUSDT on binance on this node yet";
    s.held_exits[0].waiting = Some(why.into());
    let rows = order_rows(&s, None, None);
    let table = rows_table(&rows);
    let said =
        format!("held until 2099-11-01T12:00:00Z and the price at or below 85; waiting: {why}");
    assert!(table.ends_with(&said), "{table}");
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert_eq!((v[1]["waiting"].as_str(), v[1]["reason"].is_null()), (Some(why), true), "{}", v[1]);
    assert!(v[0].get("waiting").is_none(), "a plain row keeps its keys: {}", v[0]);
    let repl = crate::cmd::trade::orders_table(&s, None);
    assert!(repl.ends_with(&said), "{repl}");
}

/// **A held order whose condition reads ANOTHER venue's price names that venue** (decision 0267's 2.13): the
/// HELD row's `release_venue` reading is folded back into its release, so `trade order ls` and the REPL's
/// `orders` print `releases at <t> once BTCUSDT on binance is at or above 100000`, then why it still waits; the
/// `--json` row carries the venue inside its `release`, and a row whose condition reads its own venue keeps its
/// words and its keys.
#[test]
fn a_held_order_on_another_venues_price_names_the_venue_and_why_it_waits() {
    use vike_tradehub_client::wire::{OrderHeldViewWire, OrderReleasePriceWire, OrderReleaseWire};
    let at = 4_097_217_600_000; // 2099-11-01T12:00:00Z
    let held = OrderHeldViewWire {
        order_client_id: "h1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 2.0,
        order_type: "limit".into(),
        price: Some(190.0),
        trigger_price: None,
        parent_order_id: None,
        account: None,
        release: Some(OrderReleaseWire {
            at,
            price: Some(OrderReleasePriceWire::Above(100_000.0)),
            symbol: Some("BTCUSDT".into()),
            venue: None,
        }),
        waiting: Some("no price of BTCUSDT on binance on this node yet".into()),
        release_venue: Some("binance".into()),
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    };
    let mut s = snap_with(vec![]);
    s.held_exits = vec![held.clone()];
    let words = "releases at 2099-11-01T12:00:00Z once BTCUSDT on binance is at or above 100000; \
                 waiting: no price of BTCUSDT on binance on this node yet";
    let rows = order_rows(&s, None, None);
    assert!(rows_table(&rows).ends_with(words), "{}", rows_table(&rows));
    let repl = crate::cmd::trade::orders_table(&s, None);
    assert!(repl.ends_with(words), "the REPL reads the same row: {repl}");
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert_eq!(v[0]["release"]["venue"], "binance", "{v}");
    assert_eq!(v[0]["waiting"], "no price of BTCUSDT on binance on this node yet");
    assert!(v[0]["reason"].is_null(), "{v}");

    // Its own venue's price: no reading, the words and the keys it always had.
    s.held_exits = vec![OrderHeldViewWire { release_venue: None, waiting: None, ..held }];
    let rows = order_rows(&s, None, None);
    assert!(
        rows_table(&rows).ends_with(
            "held until 2099-11-01T12:00:00Z and the price of BTCUSDT at or above 100000"
        ),
        "{}",
        rows_table(&rows)
    );
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert!(v[0]["release"].get("venue").is_none(), "{v}");
}

/// **A resting order shows the terms of this wave** (`OrderViewWire`'s readings): a percent trail, a limit
/// order's attributes, a stop's adjustment not yet made and the time release its venue holds, in the last cell
/// of `trade order ls` and the REPL's `orders`, and as their wire shapes on the `--json` row; a plain row keeps
/// its bytes.
#[test]
fn a_resting_order_shows_its_attributes_percent_trail_adjustment_and_release() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    use vike_tradehub_client::wire::OrderReleaseWire;
    let at = 4_097_217_600_000; // 2099-11-01T12:00:00Z
    let attributed = OrderViewWire {
        attributes: Some(OrderAttributes {
            all_or_none: true,
            sweep_to_fill: false,
            discretionary_amount: Some(0.5),
        }),
        release: Some(OrderReleaseWire { at, price: None, symbol: None, venue: None }),
        ..order("l", "ibkr", "AAPL")
    };
    let trailing = OrderViewWire {
        order_type: "trailing_take_profit".into(),
        price: None,
        trail_percent: Some(1.5),
        limit_offset: Some(0.2),
        ..order("t", "ibkr", "AAPL")
    };
    let adjust = |to| OrderViewWire {
        order_type: "stop".into(),
        side: -1,
        price: None,
        trigger_price: Some(180.0),
        adjust: Some(OrderStopAdjust { at: 200.0, to }),
        ..order("s", "ibkr", "AAPL")
    };
    let rows = order_rows(
        &snap_with(vec![
            attributed.clone(),
            trailing.clone(),
            adjust(OrderStopAdjustTo::Stop { stop: 195.0 }),
            order("p", "ibkr", "AAPL"),
        ]),
        None,
        None,
    );
    let table = rows_table(&rows);
    let line = |order_client_id: &str| {
        table.lines().find(|l| l.starts_with(order_client_id)).expect(order_client_id).to_string()
    };
    assert!(
        line("l").ends_with(" all-or-none discretion=0.5 releases at 2099-11-01T12:00:00Z"),
        "{table}"
    );
    assert!(line("t").ends_with(" trail=1.5% offset=0.2"), "{table}");
    assert!(line("s").ends_with(" adjust at 200 to stop 195"), "{table}");
    assert!(line("p").ends_with(" -"), "a plain row keeps its cell: {table}");
    for (to, words) in [
        (OrderStopAdjustTo::StopLimit { stop: 195.0, limit: 194.0 }, "to stop-limit 195 limit 194"),
        (
            OrderStopAdjustTo::Trailing { stop: 195.0, trail: Some(2.0), trail_percent: None },
            "to trailing from 195 by 2",
        ),
        (
            OrderStopAdjustTo::Trailing { stop: 195.0, trail: None, trail_percent: Some(1.0) },
            "to trailing from 195 by 1%",
        ),
    ] {
        let one = rows_table(&order_rows(&snap_with(vec![adjust(to)]), None, None));
        assert!(one.ends_with(&format!(" adjust at 200 {words}")), "{one}");
    }

    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert_eq!(
        v[0]["attributes"],
        serde_json::json!({"all_or_none": true, "discretionary_amount": 0.5})
    );
    assert_eq!(v[0]["release"], serde_json::json!({"at": at}));
    assert_eq!((v[1]["trail_percent"].as_f64(), v[1]["trail"].is_null()), (Some(1.5), true));
    assert_eq!(v[2]["adjust"], serde_json::json!({"at": 200.0, "to": {"stop": {"stop": 195.0}}}));
    let plain = v[3].as_object().unwrap();
    for key in ["attributes", "trail_percent", "adjust", "release", "trail"] {
        assert!(!plain.contains_key(key), "a plain row grows no `{key}`: {}", v[3]);
    }

    let repl = crate::cmd::trade::orders_table(&snap_with(vec![attributed, trailing]), None);
    assert!(
        repl.contains("  all-or-none discretion=0.5 releases at 2099-11-01T12:00:00Z"),
        "{repl}"
    );
    assert!(repl.contains("  trail=1.5% offset=0.2"), "{repl}");
}

/// **A HELD order shows its terms** (the held row's readings: attributes, a percent trail, an adjustment, a display of
/// zero) after what it waits for and before why it still waits, in `trade order ls` and the REPL's `orders`, and as
/// their wire shapes on the `--json` row; a held row without them keeps its words (the held tests above).
#[test]
fn a_held_order_shows_its_terms_after_what_it_waits_for() {
    use vike_tradehub_client::wire::{OrderHeldViewWire, OrderReleasePriceWire, OrderReleaseWire};
    let at = 4_097_217_600_000; // 2099-11-01T12:00:00Z
    let limit = OrderHeldViewWire {
        order_client_id: "h1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 2.0,
        order_type: "limit".into(),
        price: Some(190.0),
        trigger_price: None,
        parent_order_id: None,
        account: None,
        release: Some(OrderReleaseWire {
            at,
            price: Some(OrderReleasePriceWire::Below(185.0)),
            symbol: None,
            venue: None,
        }),
        waiting: Some("no fresh price yet".into()),
        release_venue: None,
        attributes: Some(OrderAttributes { all_or_none: true, ..Default::default() }),
        trail_percent: None,
        adjust: None,
        display_quantity: Some(0.0),
    };
    let trailing = OrderHeldViewWire {
        order_client_id: "h2".into(),
        order_type: "trailing".into(),
        price: None,
        trigger_price: Some(180.0),
        waiting: None,
        attributes: None,
        trail_percent: Some(1.5),
        display_quantity: None,
        ..limit.clone()
    };
    let stop = OrderHeldViewWire {
        order_client_id: "h3".into(),
        order_type: "stop".into(),
        trail_percent: None,
        adjust: Some(OrderStopAdjust { at: 200.0, to: OrderStopAdjustTo::Stop { stop: 195.0 } }),
        ..trailing.clone()
    };
    let mut s = snap_with(vec![]);
    s.held_exits = vec![limit, trailing, stop];
    let rows = order_rows(&s, None, None);
    let table = rows_table(&rows);
    let repl = crate::cmd::trade::orders_table(&s, None);
    let until = "held until 2099-11-01T12:00:00Z and the price at or below 185";
    for (order_client_id, words) in [
        ("h1", format!("{until} all-or-none hidden; waiting: no fresh price yet")),
        ("h2", format!("{until} trail=1.5%")),
        ("h3", format!("{until} adjust at 200 to stop 195")),
    ] {
        let line = table.lines().find(|l| l.starts_with(order_client_id)).expect(order_client_id);
        assert!(line.ends_with(&words), "{table}");
        let line = repl
            .lines()
            .find(|l| l.trim_start().starts_with(order_client_id))
            .expect(order_client_id);
        assert!(line.ends_with(&words), "the REPL reads the same row: {repl}");
    }
    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &[])).unwrap();
    assert_eq!(v[0]["attributes"], serde_json::json!({"all_or_none": true}));
    assert_eq!(v[0]["hidden"], true, "{v}");
    assert_eq!(v[1]["trail_percent"], 1.5);
    assert_eq!(v[2]["adjust"], serde_json::json!({"at": 200.0, "to": {"stop": {"stop": 195.0}}}));
    assert!(v[1].get("hidden").is_none() && v[2].get("attributes").is_none(), "{v}");
}

/// **A resting order with a display of ZERO shows as HIDDEN** (`OrderViewWire::display_quantity`): `hidden` in the last
/// cell of `trade order ls` and the REPL's `orders`, `"hidden": true` on its `--json` row. An iceberg with a slice
/// above zero says nothing new, so its row keeps a plain row's bytes in all three, and a plain row grows no key.
#[test]
fn a_resting_display_of_zero_shows_hidden_and_an_iceberg_keeps_its_bytes() {
    let shown = |o: &OrderViewWire| {
        let snap = snap_with(vec![o.clone()]);
        let rows = order_rows(&snap, None, None);
        (rows_table(&rows), rows_json(&rows, &[]), crate::cmd::trade::orders_table(&snap, None))
    };
    let hidden = OrderViewWire { display_quantity: Some(0.0), ..order("h", "ibkr", "AAPL") };
    let (table, json, repl) = shown(&hidden);
    assert!(table.ends_with(" hidden"), "{table}");
    assert!(repl.ends_with("  hidden"), "{repl}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v[0]["hidden"], true, "{v}");

    let iceberg = OrderViewWire { display_quantity: Some(2.0), ..order("i", "ibkr", "AAPL") };
    assert_eq!(shown(&iceberg), shown(&order("i", "ibkr", "AAPL")), "an iceberg's bytes");
    let (table, json, _) = shown(&order("p", "ibkr", "AAPL"));
    assert!(table.ends_with(" -"), "a plain row keeps its cell: {table}");
    assert!(!json.contains("hidden"), "a plain row grows no key: {json}");
}

/// A working emulated order (a chase with a live child) and a trigger kind (a core-held stop), as the node lists
/// them (`CoreSnapshotWire::emulated_orders`).
fn emulated_snap() -> CoreSnapshotWire {
    use vike_tradehub_client::wire::{OrderEmulationViewWire, OrderPegWire};
    let mut s =
        snap_with(vec![order("c-77-3", "bybit", "BTCUSDT"), order("x", "bybit", "ETHUSDT")]);
    let chase = OrderEmulationViewWire {
        id: "c-77".into(),
        venue: "bybit".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        kind: "chase".into(),
        reduce_only: false,
        trigger: None,
        price: None,
        trail: None,
        peg: Some(OrderPegWire { anchor: "mid".into(), offset: 0.5 }),
        display_quantity: None,
        filled: 0.4,
        average_price: 60_010.0,
        child: Some("c-77-3".into()),
        waiting: None,
        rearms: 0,
        held: false,
    };
    let stop = OrderEmulationViewWire {
        id: "s0a1".into(),
        venue: "ig".into(),
        account: Some("ALT".into()),
        symbol: "CS.D.EURUSD.CFD.IP".into(),
        side: -1,
        qty: 2.0,
        kind: "stop".into(),
        reduce_only: true,
        trigger: Some(1.05),
        peg: None,
        filled: 0.0,
        average_price: 0.0,
        child: None,
        waiting: Some("restored: held until its account's first reconcile pass".into()),
        rearms: 2,
        ..chase.clone()
    };
    s.emulated_orders = vec![chase, stop];
    s
}

/// **ONE EMULATED block lists every emulated order once** (decision 0275): its kind, id, account, terms, filled at
/// its average, child and waiting words, below the orders in `trade order ls` and the REPL's `orders`; the child
/// row of `orders` says whose child it is, and `--json` carries each emulated order as a row with its `emulated`
/// kind word and the child with `child_of`. A node that emulates nothing prints no block.
#[test]
fn the_emulated_block_lists_each_emulated_order_once_and_marks_the_child() {
    let s = emulated_snap();
    let rows = order_rows(&s, None, None);
    let emulated = emulated_rows(&s, None, None);
    let table = emulated_table(&emulated);
    let lines: Vec<&str> = table.lines().collect();
    assert!(lines[0].starts_with("EMULATED") && lines[1].starts_with("ID "), "{table}");
    assert_eq!(lines.len(), 4, "a title, a header, one row each:\n{table}");
    for part in [
        "c-77",
        "chase",
        "bybit",
        "BTCUSDT",
        "buy",
        "peg=mid+0.5",
        "0.4 @ 60010",
        "c-77-3",
        "working",
    ] {
        assert!(lines[2].contains(part), "{part:?} missing from {}", lines[2]);
    }
    for part in [
        "s0a1",
        "stop",
        "ALT",
        "sell",
        "trigger=1.05 reduce-only",
        "waiting: restored",
        "re-armed 2x",
    ] {
        assert!(lines[3].contains(part), "{part:?} missing from {}", lines[3]);
    }

    let orders = rows_table(&rows);
    let child = orders.lines().find(|l| l.starts_with("c-77-3")).expect("the child row");
    assert!(child.ends_with(" child of c-77"), "{orders}");
    assert!(!orders.lines().any(|l| l.starts_with("x ") && l.contains("child of")), "{orders}");
    let repl = crate::cmd::trade::orders_table(&s, None);
    assert!(repl.contains("  child of c-77") && repl.contains("\n  EMULATED"), "{repl}");
    assert!(!crate::cmd::trade::orders_table(&s, Some("ETHUSDT")).contains("child"));

    let v: serde_json::Value = serde_json::from_str(&rows_json(&rows, &emulated)).unwrap();
    let arr = v.as_array().expect("one flat array");
    assert_eq!(arr.len(), 4, "two orders, then two emulated rows: {v}");
    assert_eq!(arr[0]["child_of"], "c-77", "{v}");
    assert!(arr[1].get("child_of").is_none() && arr[1].get("emulated").is_none(), "{v}");
    assert_eq!((arr[2]["emulated"].as_str(), arr[2]["id"].as_str()), (Some("chase"), Some("c-77")));
    assert_eq!(arr[2]["peg"], serde_json::json!({"anchor": "mid", "offset": 0.5}), "{v}");
    assert_eq!(
        (arr[3]["emulated"].as_str(), arr[3]["account"].as_str()),
        (Some("stop"), Some("ALT"))
    );
    assert_eq!(arr[3]["trigger"].as_f64(), Some(1.05), "{v}");
    assert_eq!((arr[3]["rearms"].as_u64(), arr[2].get("rearms")), (Some(2), None), "{v}");

    // A venue filter narrows the block as it narrows the orders; with nothing emulated the REPL table is as before.
    let bybit = crate::cmd::trade::selector::parse("bybit").unwrap();
    let narrowed = emulated_rows(&s, Some(&bybit), None);
    assert_eq!(narrowed.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["c-77"]);
    let mut plain = emulated_snap();
    plain.emulated_orders.clear();
    assert!(!crate::cmd::trade::orders_table(&plain, None).contains("EMULATED"));
}
