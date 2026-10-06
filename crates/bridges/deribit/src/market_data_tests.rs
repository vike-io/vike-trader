use super::*;
use serde_json::json;

/// A wire-faithful `book.*` SNAPSHOT frame (documented shape: `[action, price, amount]`
/// triplets as JSON NUMBERS, `change_id` the anchor).
fn snapshot_frame(change_id: u64, bids: &[BookLevel], asks: &[BookLevel]) -> String {
    json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "book.BTC-PERPETUAL.100ms", "data": {
                "type": "snapshot", "timestamp": 1_700_000_000_000i64,
                "instrument_name": "BTC-PERPETUAL", "change_id": change_id,
                "bids": bids.iter().map(|&BookLevel { price: p, qty: q }| json!(["new", p, q])).collect::<Vec<_>>(),
                "asks": asks.iter().map(|&BookLevel { price: p, qty: q }| json!(["new", p, q])).collect::<Vec<_>>(),
            }}
        })
        .to_string()
}

/// A wire-faithful `book.*` CHANGE frame (`prev_change_id` chains to the previous frame).
fn change_frame(prev: u64, change_id: u64, bids: &[serde_json::Value]) -> String {
    json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "book.BTC-PERPETUAL.100ms", "data": {
            "type": "change", "timestamp": 1_700_000_000_100i64,
            "instrument_name": "BTC-PERPETUAL",
            "prev_change_id": prev, "change_id": change_id,
            "bids": bids, "asks": [],
        }}
    })
    .to_string()
}

#[test]
fn channel_builders_are_verbatim() {
    assert_eq!(book_channel("BTC-PERPETUAL"), "book.BTC-PERPETUAL.100ms");
    assert_eq!(trades_channel("BTC-PERPETUAL"), "trades.BTC-PERPETUAL.100ms");
    assert_eq!(quote_channel("ETH-PERPETUAL"), "quote.ETH-PERPETUAL");
    assert_eq!(chart_channel("BTC-PERPETUAL", "1"), "chart.trades.BTC-PERPETUAL.1");
}

#[test]
fn subscribe_frame_is_a_public_subscribe_rpc() {
    let sub = public_subscribe_frame(&["book.BTC-PERPETUAL.100ms".to_string()]);
    let v: Value = serde_json::from_str(&sub).unwrap();
    assert_eq!(v["method"], "public/subscribe");
    assert_eq!(v["params"]["channels"][0], "book.BTC-PERPETUAL.100ms");
}

// ── book: the DeltaSync chain ───────────────────────────────────────────────────────────────

#[test]
fn snapshot_seeds_the_book_at_change_id() {
    let mut book = L2Book::new(0.5);
    let ev = route_frame(
        &snapshot_frame(
            297_000,
            &[BookLevel::new(60_000.0, 5.0), BookLevel::new(59_999.5, 2.0)],
            &[BookLevel::new(60_000.5, 4.0)],
        ),
        "BTC-PERPETUAL",
        &mut book,
    );
    assert_eq!(ev, MdEvent::BookUpdated);
    assert_eq!(book.last_seq, 297_000);
    assert_eq!(book.best_bid(), Some(BookLevel::new(60_000.0, 5.0)));
    assert_eq!(book.best_ask(), Some(BookLevel::new(60_000.5, 4.0)));
}

#[test]
fn a_chained_change_folds_and_advances_the_anchor() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(100, &[BookLevel::new(60_000.0, 5.0)], &[BookLevel::new(60_000.5, 4.0)]),
        "s",
        &mut book,
    );
    // change_id values JUMP (venue-global) — only prev_change_id must chain.
    let ev =
        route_frame(&change_frame(100, 137, &[json!(["change", 60_000.0, 8.0])]), "s", &mut book);
    assert_eq!(ev, MdEvent::BookUpdated);
    assert_eq!(book.last_seq, 137);
    assert_eq!(book.best_bid(), Some(BookLevel::new(60_000.0, 8.0)));
}

#[test]
fn a_delete_action_removes_the_level() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(
            100,
            &[BookLevel::new(60_000.0, 5.0), BookLevel::new(59_999.5, 2.0)],
            &[BookLevel::new(60_000.5, 4.0)],
        ),
        "s",
        &mut book,
    );
    // The documented delete shape: amount 0 on the wire; the decoder forces 0.0 regardless.
    let ev =
        route_frame(&change_frame(100, 101, &[json!(["delete", 60_000.0, 0.0])]), "s", &mut book);
    assert_eq!(ev, MdEvent::BookUpdated);
    assert_eq!(
        book.best_bid(),
        Some(BookLevel::new(59_999.5, 2.0)),
        "the deleted top level is gone"
    );
}

#[test]
fn a_replayed_change_is_ignored_and_the_book_untouched() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(100, &[BookLevel::new(60_000.0, 5.0)], &[BookLevel::new(60_000.5, 4.0)]),
        "s",
        &mut book,
    );
    route_frame(&change_frame(100, 137, &[json!(["change", 60_000.0, 8.0])]), "s", &mut book);
    // The SAME frame again: change_id already reflected → Ignored (NOT Resync, even though its
    // prev_change_id no longer matches the advanced anchor — the check-order rule).
    let ev =
        route_frame(&change_frame(100, 137, &[json!(["change", 60_000.0, 999.0])]), "s", &mut book);
    assert_eq!(ev, MdEvent::Ignored);
    assert_eq!(book.last_seq, 137, "anchor unmoved");
    assert_eq!(book.best_bid(), Some(BookLevel::new(60_000.0, 8.0)), "book unmoved");
}

#[test]
fn a_broken_chain_answers_resync_and_does_not_fold() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(100, &[BookLevel::new(60_000.0, 5.0)], &[BookLevel::new(60_000.5, 4.0)]),
        "s",
        &mut book,
    );
    // prev_change_id 104 != anchor 100 — the venue dropped a message between them.
    let ev =
        route_frame(&change_frame(104, 105, &[json!(["change", 60_000.0, 999.0])]), "s", &mut book);
    assert_eq!(ev, MdEvent::Resync);
    assert_eq!(book.last_seq, 100, "anchor unmoved across the gap");
    assert_eq!(
        book.best_bid(),
        Some(BookLevel::new(60_000.0, 5.0)),
        "the gapped delta was NOT folded"
    );
}

#[test]
fn a_post_gap_snapshot_reseeds_cleanly() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(100, &[BookLevel::new(60_000.0, 5.0)], &[BookLevel::new(60_000.5, 4.0)]),
        "s",
        &mut book,
    );
    route_frame(&change_frame(104, 105, &[]), "s", &mut book); // gap
    let ev = route_frame(
        &snapshot_frame(300, &[BookLevel::new(60_000.5, 3.0)], &[BookLevel::new(60_001.0, 4.0)]),
        "s",
        &mut book,
    );
    assert_eq!(ev, MdEvent::BookUpdated);
    assert_eq!(book.last_seq, 300);
    assert_eq!(book.best_bid(), Some(BookLevel::new(60_000.5, 3.0)));
}

#[test]
fn parse_book_snapshot_answers_only_snapshots() {
    let frame =
        snapshot_frame(297_000, &[BookLevel::new(60_000.0, 5.0)], &[BookLevel::new(60_000.5, 4.0)]);
    let (seq, bids, asks) = parse_book_snapshot(&serde_json::from_str(&frame).unwrap()).unwrap();
    assert_eq!(seq, 297_000);
    assert_eq!(bids, vec![BookLevel::new(60_000.0, 5.0)]);
    assert_eq!(asks, vec![BookLevel::new(60_000.5, 4.0)]);
    let change = change_frame(297_000, 297_001, &[]);
    assert!(parse_book_snapshot(&serde_json::from_str(&change).unwrap()).is_none());
}

// ── trades ──────────────────────────────────────────────────────────────────────────────────

/// Documented `trades.*` shape: an ARRAY of rows; `direction` is the TAKER side; `amount` the
/// venue contract unit (USD on the inverse perp).
const TRADES_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"trades.BTC-PERPETUAL.100ms","data":[{"trade_seq":30289432,"trade_id":"48079254","timestamp":1590484512188,"tick_direction":2,"price":8950.0,"mark_price":8948.9,"instrument_name":"BTC-PERPETUAL","index_price":8955.88,"direction":"sell","amount":10.0},{"trade_seq":30289433,"trade_id":"48079255","timestamp":1590484512188,"tick_direction":2,"price":8949.5,"mark_price":8948.9,"instrument_name":"BTC-PERPETUAL","index_price":8955.88,"direction":"buy","amount":20.0}]}}"#;

#[test]
fn trades_frame_decodes_every_row_with_the_taker_side_mapping() {
    let mut book = L2Book::new(0.5);
    let MdEvent::Trades(rows) = route_frame(TRADES_FRAME, "BTC-PERPETUAL", &mut book) else {
        panic!("expected Trades");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].price, 8950.0);
    assert_eq!(rows[0].size, 10.0, "amount verbatim — venue contract units");
    assert!(rows[0].is_buyer_maker, "taker sold → buyer was the maker");
    assert!(!rows[1].is_buyer_maker, "taker bought");
    assert_eq!(rows[0].ts, 1_590_484_512_188);
    assert_eq!(rows[0].symbol, "BTC-PERPETUAL");
    assert_eq!(rows[0].local_ts, 0, "receive stamp is the pump's job");
}

#[test]
fn a_malformed_trade_row_is_skipped_not_fatal() {
    let frame = json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "trades.BTC-PERPETUAL.100ms", "data": [
            {"instrument_name": "BTC-PERPETUAL", "price": 8950.0, "amount": 10.0,
             "direction": "sell", "timestamp": 1i64},
            {"instrument_name": "BTC-PERPETUAL", "price": 0.0, "amount": 10.0,
             "direction": "sell", "timestamp": 2i64},
            {"instrument_name": "BTC-PERPETUAL", "price": 8950.0, "amount": 10.0,
             "direction": "??", "timestamp": 3i64}
        ]}
    })
    .to_string();
    let mut book = L2Book::new(0.5);
    let MdEvent::Trades(rows) = route_frame(&frame, "s", &mut book) else {
        panic!("expected Trades");
    };
    assert_eq!(rows.len(), 1, "zero-price and unknown-direction rows dropped in place");
}

// ── quotes ──────────────────────────────────────────────────────────────────────────────────

/// Documented `quote.*` shape: a SINGLE object, both sides + amounts, `timestamp` epoch-ms.
const QUOTE_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"quote.BTC-PERPETUAL","data":{"timestamp":1550658624149,"instrument_name":"BTC-PERPETUAL","best_bid_price":3914.97,"best_bid_amount":40.0,"best_ask_price":3915.5,"best_ask_amount":50.0}}}"#;

#[test]
fn quote_frame_decodes_to_a_two_sided_quote() {
    let mut book = L2Book::new(0.5);
    let MdEvent::Quote(q) = route_frame(QUOTE_FRAME, "BTC-PERPETUAL", &mut book) else {
        panic!("expected Quote");
    };
    assert_eq!((q.bid, q.bid_size, q.ask, q.ask_size), (3914.97, 40.0, 3915.5, 50.0));
    assert_eq!(q.ts, 1_550_658_624_149);
    assert_eq!(q.symbol, "BTC-PERPETUAL");
    assert_eq!(q.local_ts, 0);
}

#[test]
fn a_one_sided_quote_is_ignored_never_fabricated() {
    let frame = json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "quote.BTC-PERPETUAL", "data": {
            "timestamp": 1i64, "instrument_name": "BTC-PERPETUAL",
            "best_bid_price": 3914.97, "best_bid_amount": 40.0
        }}
    })
    .to_string();
    let mut book = L2Book::new(0.5);
    assert_eq!(route_frame(&frame, "s", &mut book), MdEvent::Ignored);
}

// ── chart bars ──────────────────────────────────────────────────────────────────────────────

/// Documented `chart.trades.*` shape: `tick` = bar-open ms, `volume` BASE units, `cost` quote
/// notional (unread — the REST reader's column choice, pinned here too).
const CHART_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"chart.trades.BTC-PERPETUAL.1","data":{"volume":0.05219351,"tick":1573645080000,"open":8869.79,"low":8788.25,"high":8870.31,"cost":463.0,"close":8791.25}}}"#;

#[test]
fn chart_frame_decodes_volume_base_not_cost() {
    let bar = parse_chart_bar(&serde_json::from_str(CHART_FRAME).unwrap()).unwrap();
    assert_eq!(bar.ts, 1_573_645_080_000);
    assert_eq!((bar.open, bar.high, bar.low, bar.close), (8869.79, 8870.31, 8788.25, 8791.25));
    assert_eq!(bar.volume, 0.05219351, "volume = BASE units; cost (463.0) must NOT be read");
}

#[test]
fn chart_parse_rejects_other_channels_and_replies() {
    assert!(parse_chart_bar(&serde_json::from_str::<Value>(QUOTE_FRAME).unwrap()).is_none());
    let reply = json!({"jsonrpc": "2.0", "id": 1, "result": ["chart.trades.BTC-PERPETUAL.1"]});
    assert!(parse_chart_bar(&reply).is_none());
}

// ── the JSON-RPC reply lane ─────────────────────────────────────────────────────────────────

#[test]
fn rpc_replies_classify_ack_error_empty_and_keepalive() {
    let ack = json!({"jsonrpc": "2.0", "id": 1, "result": ["book.BTC-PERPETUAL.100ms"]});
    assert_eq!(classify_rpc_reply(&ack), RpcReply::Ack);
    let empty = json!({"jsonrpc": "2.0", "id": 1, "result": []});
    assert!(
        matches!(classify_rpc_reply(&empty), RpcReply::Error(_)),
        "an empty subscribe result is a dead subscription, not a success"
    );
    let err = json!({"jsonrpc": "2.0", "id": 1,
            "error": {"code": -32602, "message": "Invalid params"}});
    let RpcReply::Error(msg) = classify_rpc_reply(&err) else { panic!("expected Error") };
    assert!(msg.contains("-32602") && msg.contains("Invalid params"));
    // the public/test keepalive reply: a result OBJECT — activity, not an ack.
    let ping = json!({"jsonrpc": "2.0", "id": 9929, "result": {"version": "1.2.26"}});
    assert_eq!(classify_rpc_reply(&ping), RpcReply::Other);
    // a subscription notification is not a reply.
    let notif: Value = serde_json::from_str(QUOTE_FRAME).unwrap();
    assert_eq!(classify_rpc_reply(&notif), RpcReply::NotReply);
}

#[test]
fn junk_and_unknown_channels_are_ignored() {
    let mut book = L2Book::new(0.5);
    assert_eq!(route_frame("not json", "s", &mut book), MdEvent::Ignored);
    assert_eq!(
        route_frame(
            r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"deribit_price_index.btc_usd","data":{"price":60000.0}}}"#,
            "s",
            &mut book
        ),
        MdEvent::Ignored
    );
    // a reply frame through the book router: Ignored (the reply lane classifies it).
    assert_eq!(
        route_frame(r#"{"jsonrpc":"2.0","id":1,"result":["x"]}"#, "s", &mut book),
        MdEvent::Ignored
    );
}
