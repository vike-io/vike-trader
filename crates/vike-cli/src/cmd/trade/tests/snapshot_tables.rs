//! The REPL's orders table: the order client id and symbol columns, truncation, and the pre-fold stamp.

use super::*;

// -- the orders table ----------------------------------------------------------------------

/// A Polymarket-shaped order: a long decimal token id for a symbol, and an order client id at the upper end
/// of what `submit --order-client-id` accepts.
fn order(order_client_id: &str, symbol: &str) -> vike_tradehub_client::wire::OrderViewWire {
    vike_tradehub_client::wire::OrderViewWire {
        data_delay: None,
        order_client_id: order_client_id.to_string(),
        venue: "polymarket".to_string(),
        account: None,
        symbol: symbol.to_string(),
        side: 1,
        qty: 10.0,
        order_type: "Limit".to_string(),
        price: Some(0.51),
        trigger_price: None,
        status: "Accepted".to_string(),
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
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

fn snap_with(orders: Vec<vike_tradehub_client::wire::OrderViewWire>) -> CoreSnapshotWire {
    CoreSnapshotWire { orders, ..CoreSnapshotWire::empty() }
}

/// The order client id is the HANDLE — it is what the operator types back at `cancel <order client id>` — so it must
/// survive the table whole. The column was a fixed 20 while `submit --order-client-id` accepts 32, so a
/// pinned id came back `aaaaaaaaaaaaaaaaaaa…` and could not be retyped at all.
#[test]
fn the_order_client_id_column_is_never_truncated() {
    let order_client_id = "A".repeat(32);
    let table = orders_table(&snap_with(vec![order(&order_client_id, "BTCUSDT")]), None);
    assert!(table.contains(&order_client_id), "the order client id must survive whole:\n{table}");
    assert!(!table.contains('…'), "nothing on this row is long enough to shorten:\n{table}");
}

/// **A v2 id's words sit beside it** (decision 0270's decode surfaces); a v1 id beside it reads `-`, and a
/// table of v1 ids alone has no words column at all, so it reads as it always did.
#[test]
fn a_v2_id_reads_in_words_beside_it() {
    let v1 = orders_table(&snap_with(vec![order("c-1", "BTCUSDT")]), None);
    assert!(!v1.contains("words"), "{v1}");
    let id = "C00000Bq7w2e9r4t1y6OL";
    let table = orders_table(&snap_with(vec![order(id, "BTCUSDT"), order("c-1", "BTCUSDT")]), None);
    assert!(table.lines().nth(1).expect("the header").contains(" words "), "{table}");
    let row = |order_client_id: &str| {
        table
            .lines()
            .find(|l| l.trim_start().starts_with(order_client_id))
            .expect(order_client_id)
            .to_string()
    };
    let said = "CLI (by hand) · no strategy · account #11 · opens or adds long";
    assert!(row(id).contains(said), "{table}");
    assert_eq!(row("c-1").split_whitespace().nth(1), Some("-"), "{table}");
}

/// The symbol IS capped (a Polymarket token id is a ~77-digit decimal and would push every later
/// column off the terminal) but truncated in the MIDDLE: the tokens of one up/down family share
/// a long prefix and differ at the TAIL, so a head-only cell named every order in the family
/// identically — the reported `DUMMYTOKEN000…`.
#[test]
fn a_long_symbol_keeps_both_ends() {
    let a = format!("{}0001", "7".repeat(72));
    let b = format!("{}9999", "7".repeat(72));
    let table = orders_table(&snap_with(vec![order("c-1", &a), order("c-2", &b)]), None);

    assert!(table.contains("0001") && table.contains("9999"), "tails must survive:\n{table}");
    assert!(table.contains("7777"), "…and so must the head:\n{table}");
    // The two rows must be TELLABLE APART, which is the whole defect.
    let lines: Vec<&str> = table.lines().filter(|l| l.contains('…')).collect();
    assert_eq!(lines.len(), 2, "both rows truncate:\n{table}");
    assert_ne!(lines[0], lines[1], "two token ids rendered identically:\n{table}");
}

/// A resting trailing order's terms follow its row (the trail, a trailing limit's offset, the activation),
/// and a row of any other kind ends where it always did.
#[test]
fn a_trailing_order_row_shows_its_terms() {
    let trailing = vike_tradehub_client::wire::OrderViewWire {
        order_type: "trailing".into(),
        price: None,
        trigger_price: Some(0.6),
        trail: Some(0.05),
        ..order("c-t", "BTCUSDT")
    };
    let table = orders_table(&snap_with(vec![trailing, order("c-l", "BTCUSDT")]), None);
    let line = |order_client_id: &str| {
        table.lines().find(|l| l.contains(order_client_id)).expect(order_client_id).to_string()
    };
    assert!(line("c-t").ends_with("  trail=0.05 activation=0.6"), "{table}");
    assert!(!line("c-l").contains("trail="), "{table}");
}

/// An ordinary instrument is untouched — the cap is generous enough that widening it cost the
/// common case nothing.
#[test]
fn ordinary_symbols_are_not_truncated() {
    for symbol in ["BTCUSDT", "EUR/USD", "BTC-30AUG26-120000-C"] {
        let table = orders_table(&snap_with(vec![order("c-1", symbol)]), None);
        assert!(table.contains(symbol), "{symbol} was shortened:\n{table}");
    }
}

/// **The table spells the price the venue holds, not the float the engine multiplied.** `round_to_step`
/// ends in `n * tick`, so a limit at `80925.93` on a 0.01 grid is held as `80925.93000000001` (measured on
/// the OKX demo, 2026-10-28: the venue's own `px` row said `80925.93`, and so did the wire string,
/// `format_to_step`'s grid multiple). `f64::to_string` printed all seventeen digits in the order table; the
/// preview line above it already used `vike_utility::number_text::format_number`. Quantity and filled-quantity are the same
/// quantizer's output (`7` contracts of `0.01` is `0.07` and `0.0700...01`).
#[test]
fn the_orders_table_does_not_print_the_float_noise_of_a_grid_multiple() {
    let noisy_price = 8_092_593.0_f64 * 0.01;
    let noisy_quantity = 7.0_f64 * 0.01 * 0.01;
    assert_eq!(noisy_price.to_string(), "80925.93000000001", "the fixture is the measured value");
    assert_ne!(noisy_quantity.to_string(), "0.0007", "…and the qty is off by an ULP too");
    let mut o = order("b99okxmkt", "BTC-USDT-SWAP");
    o.venue = "okx".to_string();
    o.price = Some(noisy_price);
    o.qty = noisy_quantity;
    o.filled_qty = noisy_quantity;
    let table = orders_table(&snap_with(vec![o]), None);
    assert!(table.contains(" 80925.93 "), "the price is spelled as the venue holds it:\n{table}");
    assert!(!table.contains("80925.93000000001"), "no float tail:\n{table}");
    assert_eq!(table.matches("0.0007").count(), 2, "qty and filled, shortened:\n{table}");
    assert!(!table.contains("0.00070000000"), "no float tail on a quantity:\n{table}");
}

#[test]
fn trunc_mid_keeps_both_ends_and_degrades_cleanly() {
    assert_eq!(trunc_mid("abcdef", 6), "abcdef", "a fitting string is untouched");
    assert_eq!(trunc_mid("abcdefghij", 5), "ab…ij");
    assert_eq!(trunc_mid("abcdefghij", 4), "ab…j", "an odd budget favours the head");
    assert_eq!(trunc_mid("abcdefghij", 2), "a…");
    assert_eq!(trunc_mid("abcdefghij", 1), "…");
    assert_eq!(trunc_mid("abcdefghij", 0), "");
    // Char-counted, not byte-counted — a multi-byte symbol must not panic on a slice boundary.
    assert_eq!(trunc_mid("ααααββββ", 5), "αα…ββ");
}

#[test]
fn width_of_fits_the_header_and_honours_the_cap() {
    assert_eq!(width_of("kind", ["ab"].into_iter(), None), 4, "never narrower than the header");
    assert_eq!(width_of("kind", ["abcdefgh"].into_iter(), None), 8, "uncapped grows to fit");
    assert_eq!(width_of("kind", ["abcdefgh"].into_iter(), Some(5)), 5, "capped stops");
    assert_eq!(width_of("symbol", std::iter::empty(), Some(24)), 6, "no rows ⇒ the header");
}

/// The empty view still renders (and still carries its `[seq N]` stamp). A BUILT frame with no
/// orders, deliberately: `seq: 0` is the pre-fold case, pinned by the test below.
#[test]
fn an_empty_orders_table_says_none() {
    let table = orders_table(&CoreSnapshotWire { sequence: 5, ..CoreSnapshotWire::empty() }, None);
    assert!(table.contains("(none)"), "{table}");
    assert!(table.contains("[seq 5]"), "{table}");
}

/// **A frame with nothing built must not read as the node's state.** `[seq 0] orders` over
/// `(none)` is what an operator reads as "there are no orders", and on a frame the node published
/// before its first fold — or on this client's own placeholder, before any node frame arrived —
/// that is not a reading at all. Every REPL table header carries this stamp (`orders`,
/// `positions`, `equity`, `recent`, the mode line), so the mark rides on it; a built frame keeps
/// the bare `[seq N]` it always had.
#[test]
fn a_pre_fold_frame_is_marked_in_every_table_header() {
    let table = orders_table(&snap_with(Vec::new()), None);
    assert!(
        table.contains("PRE-FOLD"),
        "a seq-0 frame must be marked, not shown as state:\n{table}"
    );
    assert_eq!(stamp(&CoreSnapshotWire::empty()), "[seq 0 PRE-FOLD]");
    assert_eq!(stamp(&CoreSnapshotWire { sequence: 3, ..CoreSnapshotWire::empty() }), "[seq 3]");
}

/// The line printed above a pre-fold render says WHICH placeholder it is — the node's own (it is
/// up and has folded nothing) or this client's own (nothing from the node arrived in time) — and a
/// built frame gets none. The same split `node_snapshot`'s `pre_fold_note` makes, read off the
/// same [`is_node_frame`].
#[test]
fn the_pre_fold_line_names_whose_placeholder_it_is_and_a_built_frame_gets_none() {
    let identity = vike_tradehub_client::wire::HubIdentityWire {
        name: "idle".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
        priced_venues: None,
    };
    let node =
        pre_fold_line(&CoreSnapshotWire { identity: Some(identity), ..CoreSnapshotWire::empty() })
            .expect("the node's pre-fold placeholder is marked");
    assert!(
        node.contains("the node is up") && node.contains("stamped with its identity"),
        "{node}"
    );
    let client =
        pre_fold_line(&CoreSnapshotWire::empty()).expect("the client's own placeholder is marked");
    assert!(client.contains("client's OWN") && !client.contains("the node is up"), "{client}");
    for line in [node, client] {
        assert!(line.starts_with("[pre-fold]") && line.contains("does NOT mean"), "{line}");
    }
    assert_eq!(pre_fold_line(&CoreSnapshotWire { sequence: 1, ..CoreSnapshotWire::empty() }), None);
}

/// [`observe_read`]'s two arms that need no node: no observe key refuses on `FAILURE` before any
/// socket (a control key alone does not count), and a socket that never opens is [`Exit::Connect`].
/// The real-node arms (a frame, a refused key) are `crates/vike-cli/src/cmd/trade/tests/idle_node.rs`.
#[test]
fn the_ls_verbs_observe_read_refuses_without_an_observe_key_and_reports_a_dead_socket_as_connect() {
    let keys = |pairs: &[(&str, &str)]| {
        let map: std::collections::HashMap<String, String> =
            pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        hubkeys::resolve(&map, None)
    };
    let code = |r: Result<std::sync::Arc<CoreSnapshotWire>, ExitCode>| {
        format!("{:?}", r.expect_err("no read without a key and a node"))
    };

    let control_only = keys(&[(hubkeys::CONTROL_KEY_ENV, "ctl")]);
    assert_eq!(
        code(observe_read("trade order ls", "127.0.0.1:1", &control_only)),
        format!("{:?}", ExitCode::FAILURE)
    );
    let dead = keys(&[(hubkeys::OBSERVE_KEY_ENV, "obs")]);
    assert_eq!(
        code(observe_read("trade order ls", "127.0.0.1:1", &dead)),
        format!("{:?}", ExitCode::from(Exit::Connect))
    );
}

// -- the equity view: one currency per total ---------------------------------------------------

/// A wire venue block of `venue` in currency group `currency` (`""` = dollars), the way a node writes it.
fn block(
    venue: &str,
    currency: &str,
    equity: f64,
    realized: f64,
) -> vike_tradehub_client::wire::VenueBlockWire {
    serde_json::from_value(serde_json::json!({
        "venue": venue, "balance": 0.0, "realized_pnl": realized, "fees_paid": 0.0,
        "funding_paid": 0.0, "equity": equity, "unrealized": 0.0, "missing_prices": 0,
        "margin_used": 0.0, "free_buying_power": 0.0, "trading_state": "Active", "positions": [],
        "ledger_currency": currency,
    }))
    .expect("a venue block")
}

/// **A `balance` that is no venue balance says so**: deribit's read `-0.155` beside a wallet of 999.99 ETH, its ledger the fees
/// paid since it started. A block holding the venue's figure says nothing, as a node that predates the word does.
#[test]
fn a_balance_that_is_the_engines_own_book_says_so() {
    let mut own_book = block("deribit", "", 999.8, -0.01);
    own_book.balance = -0.155;
    own_book.balance_is_own_book = true;
    let snap = CoreSnapshotWire {
        sequence: 5,
        venues: vec![own_book, block("bybit", "", 100.0, 0.0)],
        ..CoreSnapshotWire::empty()
    };
    let text = equity_text(&snap);
    assert!(text.contains("deribit: this engine keeps its own book"), "{text}");
    assert!(text.contains("not the venue's wallet"), "{text}");
    assert!(!text.contains("bybit: this engine keeps its own book"), "{text}");
}

/// ⚠ **A frame in two currencies shows a total per currency, never one across them** (decision 0183, item
/// 13): `equity_total` is labelled as the primary's currency alone and each group has its own line.
#[test]
fn the_equity_view_of_two_currencies_lists_each_never_adds() {
    let snap = CoreSnapshotWire {
        sequence: 5,
        equity_total: 1_100.5,
        venues: vec![block("binance", "", 1_100.5, 12.0), block("oanda", "EUR", 7_000.25, -300.0)],
        ..CoreSnapshotWire::empty()
    };
    let text = equity_text(&snap);
    assert!(
        text.contains("equity_total:      1100.5 (USD only, the primary's currency)"),
        "{text}"
    );
    assert!(text.contains("USD  equity 1100.5 realized 12"), "{text}");
    assert!(text.contains("EUR  equity 7000.25 realized -300"), "{text}");
    assert!(!text.contains("8100"), "no figure adds the two currencies:\n{text}");
}

/// **A group's line is the law every report folds by** (`vike_model::ledger_sums`): `compensated_sum` keeps the 1 a left fold
/// loses between 1e16 and -1e16, and `margin_used` is that left fold.
#[test]
fn a_group_line_folds_by_the_one_ledger_law() {
    let with_margin = |equity: f64| {
        let mut b = block("oanda", "EUR", equity, equity);
        b.margin_used = equity;
        b
    };
    let snap = CoreSnapshotWire {
        sequence: 5,
        venues: vec![
            block("binance", "", 1_100.5, 12.0),
            with_margin(1e16),
            with_margin(1.0),
            with_margin(-1e16),
        ],
        ..CoreSnapshotWire::empty()
    };
    let text = equity_text(&snap);
    assert!(text.contains("EUR  equity 1 realized 1 unreal 0 margin_used 0"), "{text}");
}

/// ⚠ **The REPL labels `equity_total` with the currency the NODE states, not the first block's**: a node
/// `[binance USDT (primary), oanda EUR, bybit USDT]` whose binance restarted publishes `[oanda, bybit,
/// binance']`, so the first block is EUR while `equity_total` is the dollar group's.
#[test]
fn the_equity_view_names_the_primary_the_node_states_after_a_primary_restart() {
    let frame = |primary: &str| CoreSnapshotWire {
        sequence: 5,
        equity_total: 160.0,
        primary_currency: primary.to_string(),
        venues: vec![
            block("oanda", "EUR", 7_000.25, -300.0),
            block("bybit", "", 50.0, 1.0),
            block("binance", "", 110.0, 2.0),
        ],
        ..CoreSnapshotWire::empty()
    };
    let text = equity_text(&frame(""));
    assert!(text.contains("equity_total:      160 (USD only, the primary's currency)"), "{text}");
    let usd = text.find("USD  equity").expect("the dollar group line");
    let eur = text.find("EUR  equity").expect("the EUR group line");
    assert!(usd < eur, "the stated primary leads:\n{text}");

    let text = equity_text(&frame("EUR"));
    assert!(text.contains("(EUR only, the primary's currency)"), "{text}");
    assert!(text.find("EUR  equity").expect("EUR") < text.find("USD  equity").expect("USD"));
}

/// ⚠ **An account whose venue balance the node holds back is named in the equity view**, in the shared words
/// (`<route key>: balance held: <reason> - ...`): its equity above is the engine's own book (seed + PnL), not the
/// venue's wallet, and nothing else on the screen says so. A frame that reports none prints no such line.
#[test]
fn the_equity_view_names_an_account_whose_balance_is_held_back() {
    let mut snap = CoreSnapshotWire {
        sequence: 5,
        equity_total: 1_000.0,
        venues: vec![block("oanda", "", 1_000.0, 0.0)],
        ..CoreSnapshotWire::empty()
    };
    assert!(!equity_text(&snap).contains("balance held"), "nothing reported, nothing said");
    snap.balance_withheld = vec![
        vike_tradehub_client::wire::BalanceWithheldWire {
            route_key: "oanda".into(),
            reason: "assumed USD".into(),
        },
        vike_tradehub_client::wire::BalanceWithheldWire {
            route_key: "ig#ALT".into(),
            reason: "stated GBP not anchored".into(),
        },
    ];
    let text = equity_text(&snap);
    assert!(text.contains("\n  oanda: balance held: assumed USD - "), "{text}");
    assert!(text.contains("\n  ig#ALT: balance held: stated GBP not anchored - "), "{text}");
    assert!(text.contains("seed + PnL"), "the consequence is said: {text}");
}

/// ⚠ **An engine whose ledger currency is only the owner's ASSUMED USD is named in the equity view** (decision 0189,
/// A52): the flag was visible nowhere but the logs. A frame whose blocks assume nothing (every older node's) prints
/// no such line.
#[test]
fn the_equity_view_names_an_engine_whose_ledger_currency_is_only_assumed() {
    let mut snap = CoreSnapshotWire {
        sequence: 5,
        equity_total: 1_000.0,
        venues: vec![block("oanda", "", 1_000.0, 0.0)],
        ..CoreSnapshotWire::empty()
    };
    assert!(!equity_text(&snap).contains("ASSUMED"), "nothing reported, nothing said");
    snap.venues[0].currency_assumed = true;
    snap.venues[0].route_key = "oanda#ALT".into();
    let text = equity_text(&snap);
    assert!(
        text.contains(
            "\n  oanda#ALT: ledger currency USD was ASSUMED when this engine started (the venue stated none)"
        ),
        "{text}"
    );
    snap.venues[0].route_key.clear();
    assert!(
        equity_text(&snap).contains("\n  oanda: ledger currency USD was ASSUMED"),
        "an older node names no route key"
    );
}

/// A node of one currency, or a frame from an older node (no `ledger_currency` key: every block is the
/// dollar group), prints the equity view it always did: the plain `equity_total` line, no currency column.
#[test]
fn the_equity_view_of_one_currency_is_unchanged() {
    let snap = CoreSnapshotWire {
        sequence: 5,
        equity_total: 1_110.5,
        venues: vec![block("binance", "", 1_100.5, 12.0), block("bybit", "", 10.0, 1.0)],
        ..CoreSnapshotWire::empty()
    };
    let text = equity_text(&snap);
    assert!(text.contains("\n  equity_total:      1110.5\n"), "{text}");
    assert!(!text.contains("USD") && !text.contains("ccy"), "{text}");
}

// -- `--json` is the edge, whichever codec carried the frame ---------------------------------

/// **`--json` prints the same bytes whichever form carried the frame** (`vike_hub_protocol::frame::BodyCodec`): a fully
/// populated snapshot and a strategy status, decoded off a JSON body, a msgpack one and its LZ4 form, render
/// byte-identical order, position and mount documents, so the CLI's edge stays JSON while its link to the node does not.
#[test]
fn the_json_documents_are_byte_identical_whichever_codec_carried_the_frame() {
    use crate::cmd::trade::render::{
        emulated_rows, mount_rows, mounts_json, order_rows, position_rows, positions_json,
        rows_json,
    };
    use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_frame};
    use vike_tradehub_client::test_support::observe_frame::full_wire_snapshot;
    use vike_tradehub_client::test_support::strategy_node::canned_status;
    use vike_tradehub_client::wire::StrategyStatusWire;

    let documents = |codec: BodyCodec| {
        // A request's form, so the LZ4 one is LZ4 whatever it saves.
        let snapshot = encode_frame(codec, &full_wire_snapshot()).expect("encode");
        let snap: CoreSnapshotWire = decode_body(&snapshot[4..]).expect("decode");
        let status = encode_frame(codec, &canned_status()).expect("encode");
        let status: StrategyStatusWire = decode_body(&status[4..]).expect("decode");
        [
            rows_json(&order_rows(&snap, None, None), &emulated_rows(&snap, None, None)),
            positions_json(&position_rows(&snap, None, None)),
            mounts_json(&mount_rows(&status), true),
        ]
    };
    let json = documents(BodyCodec::Json);
    assert!(json.iter().all(|document| document.len() > 2), "every document has rows: {json:?}");
    assert_eq!(documents(BodyCodec::Msgpack), json);
    assert_eq!(documents(BodyCodec::MsgpackLz4), json);
}
