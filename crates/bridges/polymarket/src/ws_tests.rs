use super::*;

#[test]
fn subscribe_frame() {
    let s = subscribe_message(&["111".to_string(), "222".to_string()]);
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "market");
    assert_eq!(v["assets_ids"][0], "111");
    assert_eq!(v["assets_ids"][1], "222");
}

/// The exact `book` frame captured live 2026-07-08 (brief). It embeds `tick_size` and
/// `last_trade_price` alongside the ladders — both must be ignored harmlessly by `parse_book`.
#[test]
fn decode_book_real_captured_frame() {
    let book = serde_json::json!({
        "market":"0x66bbf6d55e0296278858b3147689f3df9259374f158f9f028b608baa322a639c",
        "asset_id":"7589880081658059374445882095611024032084645048848237404312287942463647821549",
        "timestamp":"1783516513010",
        "hash":"43b683fda60e94ae739c04a2c31ab18ef450a006",
        "bids":[{"price":"0.01","size":"22260.18"},{"price":"0.02","size":"36800"},{"price":"0.03","size":"32172.32"}],
        "asks":[{"price":"0.99","size":"655294.85"},{"price":"0.98","size":"90000"},{"price":"0.97","size":"34609.76"}],
        "tick_size":"0.01","event_type":"book","last_trade_price":"0.600"
    });
    let ups = decode_market(&book);
    assert_eq!(ups.len(), 1);
    match &ups[0] {
        MarketUpdate::Book { book, ts } => {
            assert_eq!(
                book.asset_id,
                "7589880081658059374445882095611024032084645048848237404312287942463647821549"
            );
            assert_eq!(book.best_bid(), Some(0.03));
            assert_eq!(book.best_ask(), Some(0.97));
            assert_eq!(*ts, Some(1_783_516_513_010));
        }
        other => panic!("expected Book, got {other:?}"),
    }
}

/// The exact `price_change` frame captured live 2026-07-08 (brief): plural `price_changes`,
/// TWO distinct assets. This is the regression test for the bug — the old decoder read the
/// singular `changes` key and silently decoded this frame to nothing.
#[test]
fn decode_price_change_real_multi_asset_frame() {
    let pc = serde_json::json!({
        "market":"0x418fcd9c72501eea029eee747b522e4da91478dbf52829893aa7a55b36d84984",
        "price_changes":[
            {"asset_id":"24395104702642353411948925889097630568855366590528757185881735998149149192479","price":"0.1","size":"380","side":"BUY","hash":"22e8a421790bf8a9ba05c7ed246902f87377a0de","best_bid":"0.31","best_ask":"0.34"},
            {"asset_id":"20904118177412102896316191568338021059060941327365407257046608077345075967207","price":"0.9","size":"380","side":"SELL","hash":"3ba21fbafa1a37ddde5c617de63f2e5f677d6e63","best_bid":"0.66","best_ask":"0.69"}
        ],
        "timestamp":"1783516514216","event_type":"price_change"
    });
    let ups = decode_market(&pc);
    assert_eq!(ups.len(), 2, "one MarketUpdate::PriceChange per distinct asset_id");
    match &ups[0] {
        MarketUpdate::PriceChange { asset_id, changes, ts } => {
            assert_eq!(
                asset_id,
                "24395104702642353411948925889097630568855366590528757185881735998149149192479"
            );
            assert_eq!(changes, &[LevelChange { price: 0.1, size: 380.0, is_bid: true }]);
            assert_eq!(*ts, Some(1_783_516_514_216));
        }
        other => panic!("expected PriceChange, got {other:?}"),
    }
    match &ups[1] {
        MarketUpdate::PriceChange { asset_id, changes, ts } => {
            assert_eq!(
                asset_id,
                "20904118177412102896316191568338021059060941327365407257046608077345075967207"
            );
            assert_eq!(changes, &[LevelChange { price: 0.9, size: 380.0, is_bid: false }]);
            assert_eq!(*ts, Some(1_783_516_514_216));
        }
        other => panic!("expected PriceChange, got {other:?}"),
    }
}

/// The market channel may deliver a JSON ARRAY of event objects in one frame; `decode_market`
/// must flatten it — each element through `decode_one` (book → 1, a 2-asset price_change → 2).
#[test]
fn decode_market_flattens_an_array_of_frames() {
    let arr = serde_json::json!([
        {
            "asset_id":"tokA","timestamp":"1000","event_type":"book",
            "bids":[{"price":"0.4","size":"10"}],"asks":[{"price":"0.6","size":"20"}],"tick_size":"0.01"
        },
        {
            "market":"0xm","timestamp":"2000","event_type":"price_change",
            "price_changes":[
                {"asset_id":"tokA","price":"0.41","size":"5","side":"BUY"},
                {"asset_id":"tokB","price":"0.7","size":"3","side":"SELL"}
            ]
        }
    ]);
    let ups = decode_market(&arr);
    // 1 book + 2 per-asset price_changes = 3, in element order.
    assert_eq!(ups.len(), 3, "array flattens: book(1) + price_change(2 assets)");
    assert!(matches!(&ups[0], MarketUpdate::Book { .. }));
    assert!(matches!(&ups[1], MarketUpdate::PriceChange { asset_id, .. } if asset_id == "tokA"));
    assert!(matches!(&ups[2], MarketUpdate::PriceChange { asset_id, .. } if asset_id == "tokB"));
}

/// A `price_changes` array can carry more than one entry for the SAME asset (e.g. a bid and an
/// ask level moving in the same frame) — those must land in one `PriceChange`'s `changes`
/// Vec, not be split across duplicate updates for that asset.
#[test]
fn decode_price_change_groups_repeated_asset_entries_together() {
    let pc = serde_json::json!({
        "price_changes":[
            {"asset_id":"A","price":"0.10","size":"5","side":"BUY"},
            {"asset_id":"B","price":"0.20","size":"7","side":"SELL"},
            {"asset_id":"A","price":"0.11","size":"0","side":"BUY"}
        ],
        "timestamp":"1700000000123","event_type":"price_change"
    });
    let ups = decode_market(&pc);
    assert_eq!(ups.len(), 2, "grouped by asset_id, not one update per entry");
    match &ups[0] {
        MarketUpdate::PriceChange { asset_id, changes, .. } => {
            assert_eq!(asset_id, "A");
            assert_eq!(
                changes,
                &[
                    LevelChange { price: 0.10, size: 5.0, is_bid: true },
                    LevelChange { price: 0.11, size: 0.0, is_bid: true },
                ]
            );
        }
        other => panic!("expected PriceChange, got {other:?}"),
    }
    match &ups[1] {
        MarketUpdate::PriceChange { asset_id, changes, .. } => {
            assert_eq!(asset_id, "B");
            assert_eq!(changes, &[LevelChange { price: 0.20, size: 7.0, is_bid: false }]);
        }
        other => panic!("expected PriceChange, got {other:?}"),
    }
}

/// The exact `last_trade_price` frame captured live 2026-07-08 (brief) — confirms the extra
/// real-world fields (`fee_rate_bps`, `transaction_hash`) are harmlessly ignored.
#[test]
fn last_trade_with_size_and_side() {
    let buy = serde_json::json!({
        "market":"0x80b3af88cb991980e8da1ce86b9794a0957f96ec98c29319dd7ba65e9744d82b",
        "asset_id":"93005850938352995663334573245996733794924636935158112548608169054144721737755",
        "price":"0.43","size":"5999","fee_rate_bps":"0","side":"BUY",
        "timestamp":"1783516542743","event_type":"last_trade_price",
        "transaction_hash":"0xe999d25b55a4362c2b4597d14147937e24274123470c38fb291aaccaa8ea2875"
    });
    match &decode_market(&buy)[0] {
        MarketUpdate::LastTrade { asset_id, price, size, taker_is_buy, ts } => {
            assert_eq!(
                asset_id,
                "93005850938352995663334573245996733794924636935158112548608169054144721737755"
            );
            assert_eq!(*price, 0.43);
            assert_eq!(*size, 5999.0);
            assert_eq!(*taker_is_buy, Some(true));
            assert_eq!(*ts, Some(1_783_516_542_743));
        }
        other => panic!("expected LastTrade, got {other:?}"),
    }

    let sell = serde_json::json!({
        "event_type": "last_trade_price", "asset_id": "111",
        "price": "0.53", "size": "10", "side": "SELL"
    });
    match &decode_market(&sell)[0] {
        MarketUpdate::LastTrade { taker_is_buy, .. } => assert_eq!(*taker_is_buy, Some(false)),
        other => panic!("expected LastTrade, got {other:?}"),
    }
}

#[test]
fn last_trade_missing_size_defaults_zero() {
    let no_size_or_side = serde_json::json!({
        "event_type": "last_trade_price", "asset_id": "111", "price": "0.53"
    });
    match &decode_market(&no_size_or_side)[0] {
        MarketUpdate::LastTrade { price, size, taker_is_buy, .. } => {
            assert_eq!(*price, 0.53);
            assert_eq!(*size, 0.0);
            assert_eq!(*taker_is_buy, None);
        }
        other => panic!("expected LastTrade, got {other:?}"),
    }
}

/// Final-review fix: a frame carrying a `timestamp` field (string OR number JSON form) must
/// have that value land in the decoded `MarketUpdate::ts` — for every event type.
#[test]
fn decode_carries_wire_timestamp_when_present() {
    let book = serde_json::json!({
        "event_type": "book", "asset_id": "111", "timestamp": "1700000000000",
        "bids": [{"price":"0.51","size":"100"}], "asks": [{"price":"0.55","size":"40"}]
    });
    match &decode_market(&book)[0] {
        MarketUpdate::Book { ts, .. } => assert_eq!(*ts, Some(1_700_000_000_000)),
        other => panic!("expected Book, got {other:?}"),
    }

    let pc = serde_json::json!({
        "event_type": "price_change", "timestamp": 1_700_000_000_001i64,
        "price_changes": [{"asset_id":"111","price":"0.52","size":"80","side":"BUY"}]
    });
    match &decode_market(&pc)[0] {
        MarketUpdate::PriceChange { ts, .. } => assert_eq!(*ts, Some(1_700_000_000_001)),
        other => panic!("expected PriceChange, got {other:?}"),
    }

    let lt = serde_json::json!({
        "event_type": "last_trade_price", "asset_id": "111",
        "price": "0.53", "timestamp": "1700000000002"
    });
    match &decode_market(&lt)[0] {
        MarketUpdate::LastTrade { ts, .. } => assert_eq!(*ts, Some(1_700_000_000_002)),
        other => panic!("expected LastTrade, got {other:?}"),
    }
}

/// A frame with no `timestamp` field decodes to `ts: None` — the caller then falls back to
/// receive time (`market_feed`'s module doc).
#[test]
fn decode_ts_is_none_when_frame_omits_timestamp() {
    let book = serde_json::json!({
        "event_type": "book", "asset_id": "111", "bids": [], "asks": []
    });
    match &decode_market(&book)[0] {
        MarketUpdate::Book { ts, .. } => assert_eq!(*ts, None),
        other => panic!("expected Book, got {other:?}"),
    }

    let lt = serde_json::json!({
        "event_type": "last_trade_price", "asset_id": "111", "price": "0.53"
    });
    match &decode_market(&lt)[0] {
        MarketUpdate::LastTrade { ts, .. } => assert_eq!(*ts, None),
        other => panic!("expected LastTrade, got {other:?}"),
    }

    let pc = serde_json::json!({
        "event_type": "price_change",
        "price_changes": [{"asset_id":"111","price":"0.52","size":"80","side":"BUY"}]
    });
    match &decode_market(&pc)[0] {
        MarketUpdate::PriceChange { ts, .. } => assert_eq!(*ts, None),
        other => panic!("expected PriceChange, got {other:?}"),
    }
}

#[test]
fn maintain_book_across_snapshot_then_delta() {
    let mut books = HashMap::new();
    for up in decode_market(&serde_json::json!({
        "event_type": "book", "asset_id": "111",
        "bids": [{"price":"0.51","size":"100"}], "asks": [{"price":"0.55","size":"40"}]
    })) {
        apply_update(&mut books, &up);
    }
    // a new better bid at 0.52, and the 0.55 ask removed (size 0) — real `price_changes`
    // plural shape, one entry per level for the same asset_id (grouped into one PriceChange).
    for up in decode_market(&serde_json::json!({
        "event_type": "price_change",
        "price_changes": [
            {"asset_id":"111","price":"0.52","size":"80","side":"BUY"},
            {"asset_id":"111","price":"0.55","size":"0","side":"SELL"}
        ]
    })) {
        apply_update(&mut books, &up);
    }
    let b = &books["111"];
    assert_eq!(b.best_bid(), Some(0.52)); // improved
    assert_eq!(b.best_ask(), None); // the only ask was removed
}
