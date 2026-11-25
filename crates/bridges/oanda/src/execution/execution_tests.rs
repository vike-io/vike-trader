use super::*;
use std::assert_matches;
use vike_bridge_core::execution_drive::fill_symbols;
use vike_execution::ExecutionClient;

/// An order on the USER name `EURUSD`; the tests' kept list is held first, so its wire name is `EUR_USD`.
fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    crate::test_support::kept_list_for_tests();
    OrderRequest {
        data_delay: None,
        display_quantity: None,
        release: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        account: None,
        combo_legs: Vec::new(),
        order_client_id: "order-client-id-1".into(),
        venue: VENUE.into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: vike_model::OrderType::from_stored(order_type),
        price: Some(1.09),
        trigger_price: Some(1.08),
        reduce_only: false,
        time_in_force: TimeInForce::Gtc,
        gtd_expiry: None,
        timestamp: 111,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

#[test]
fn market_body_is_signed_fok() {
    let b = build_order_body(&req("market", -1, 1000.0));
    let o = &b["order"];
    assert_eq!(o["type"], "MARKET");
    assert_eq!(o["timeInForce"], "FOK");
    assert_eq!(o["instrument"], "EUR_USD");
    assert_eq!(o["units"], "-1000"); // sell → negative units
    assert_eq!(o["clientExtensions"]["id"], "order-client-id-1");
}

#[test]
fn limit_body_carries_price() {
    let b = build_order_body(&req("limit", 1, 500.0));
    assert_eq!(b["order"]["type"], "LIMIT");
    assert_eq!(b["order"]["units"], "500");
    assert_eq!(b["order"]["price"], "1.09000");
    assert_eq!(b["order"]["timeInForce"], "GTC"); // default TIF
}

#[test]
fn time_in_force_flows_through() {
    let mut r = req("limit", 1, 500.0);
    r.time_in_force = TimeInForce::Ioc;
    assert_eq!(build_order_body(&r)["order"]["timeInForce"], "IOC");
    // market coerces a non-immediate TIF to FOK
    let mut m = req("market", 1, 500.0);
    m.time_in_force = TimeInForce::Gtc;
    assert_eq!(build_order_body(&m)["order"]["timeInForce"], "FOK");
}

/// **The whole body of each kind this bridge builds**, so the guard that refuses every other kind cannot
/// move one of them.
#[test]
fn the_body_of_every_built_kind_is_pinned_whole() {
    let mut stop = req("stop", 1, 100.0);
    stop.trigger_price = Some(1.075);
    let mut mit = req("take_profit", -1, 100.0);
    mit.price = None;
    mit.trigger_price = Some(1.12);
    for (r, want) in [
        (
            req("market", -1, 1000.0),
            json!({"order": {"instrument": "EUR_USD", "units": "-1000",
                "clientExtensions": {"id": "order-client-id-1"}, "timeInForce": "FOK", "type": "MARKET"}}),
        ),
        (
            req("limit", 1, 500.0),
            json!({"order": {"instrument": "EUR_USD", "units": "500",
                "clientExtensions": {"id": "order-client-id-1"}, "timeInForce": "GTC", "type": "LIMIT",
                "price": "1.09000"}}),
        ),
        (
            stop,
            json!({"order": {"instrument": "EUR_USD", "units": "100",
                "clientExtensions": {"id": "order-client-id-1"}, "timeInForce": "GTC", "type": "STOP",
                "price": "1.07500"}}),
        ),
        (
            // A priceless take-profit is OANDA's MARKET_IF_TOUCHED: the trigger is its `price`, and
            // it rests GTC like a stop (no `priceBound`, no `triggerCondition`: the venue's defaults).
            mit,
            json!({"order": {"instrument": "EUR_USD", "units": "-100",
                "clientExtensions": {"id": "order-client-id-1"}, "timeInForce": "GTC",
                "type": "MARKET_IF_TOUCHED", "price": "1.12000"}}),
        ),
    ] {
        assert_eq!(build_order_body(&r), want, "{}", r.order_type);
        assert_eq!(unbuilt_kind(&r), None, "{}", r.order_type);
    }
}

/// **A reduce-only order of every built kind says so on the wire, and only a reduce-only one does**: its body is
/// the plain order's (pinned whole above) plus `"positionFill": "REDUCE_ONLY"`, on any account (the venue's
/// `DEFAULT` would let it open a position once its own is gone).
#[test]
fn a_reduce_only_order_of_every_built_kind_carries_position_fill_and_nothing_else_does() {
    let mut stop = req("stop", -1, 100.0);
    stop.trigger_price = Some(1.075);
    let mut mit = req("take_profit", -1, 100.0);
    mit.price = None;
    mit.trigger_price = Some(1.12);
    for plain in [req("market", -1, 1000.0), req("limit", -1, 500.0), stop, mit] {
        let kind = plain.order_type.clone();
        let body = build_order_body(&plain);
        assert!(
            body["order"].get("positionFill").is_none(),
            "{kind}: a plain order carries no word"
        );
        let mut reduce = build_order_body(&OrderRequest { reduce_only: true, ..plain });
        assert_eq!(reduce["order"]["positionFill"], "REDUCE_ONLY", "{kind}");
        reduce["order"].as_object_mut().expect("an order object").remove("positionFill");
        assert_eq!(reduce, body, "{kind}: the word is the only difference");
    }
}

/// A kind this bridge builds no order for is refused BY NAME before any read (the live loop's
/// `unbuilt_kind`), and its body is no MARKET order: `stop_limit`, `market_to_limit` and an
/// unrecognised word used to go out as one.
#[test]
fn a_kind_this_bridge_does_not_build_is_named_and_its_body_is_no_market_order() {
    for kind in ["stop_limit", "market_to_limit", "market_if_touched"] {
        let r = req(kind, 1, 100.0);
        let why = unbuilt_kind(&r).expect("refused");
        assert!(why.contains(&format!("oanda builds no `{kind}` order")), "{why}");
        assert!(build_order_body(&r)["order"].get("type").is_none(), "{kind}");
    }
}

/// A take-profit WITH a limit price (limit-if-touched) is refused by name: OANDA's `priceBound` is not
/// a resting limit, so it is never sent as the market-if-touched it would become. Its body carries no
/// `type` either.
#[test]
fn a_limit_if_touched_is_refused_by_name_and_never_sent_as_a_market_if_touched() {
    let r = req("take_profit", 1, 100.0); // `req` states a price (1.09) and a trigger (1.08)
    let why = unbuilt_kind(&r).expect("refused");
    assert!(why.contains("oanda builds no limit-if-touched order"), "{why}");
    assert!(build_order_body(&r)["order"].get("type").is_none());
}

/// An AUCTION lifetime is refused by name on every kind: v20 has no auction order, and this bridge
/// would otherwise send a market-on-open as an immediate FOK market and a limit-on-close as a GTC limit.
#[test]
fn an_auction_lifetime_is_refused_by_name_on_a_market_and_a_limit() {
    for (kind, tif, word) in
        [("market", TimeInForce::AtOpen, "at-open"), ("limit", TimeInForce::AtClose, "at-close")]
    {
        let mut r = req(kind, 1, 100.0);
        r.time_in_force = tif;
        let why = unbuilt_kind(&r).expect("refused");
        assert!(why.contains("oanda has no auction order") && why.contains(word), "{why}");
    }
}

/// Equivalence gate for the `venue_time_in_force` routing: the working-order arm's five recorded wire
/// strings, asserted BOTH through `oanda_time_in_force` and against this venue's row of the cross-venue
/// table (byte-for-byte), plus the local MARKET arm's FOK/IOC-only coercion (a genuine venue
/// constraint kept OUT of the resting-path table — see `vike_venues::venue_time_in_force`'s module doc).
#[test]
fn time_in_force_truth_table_matches_the_venue_time_in_force_row() {
    use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
    // Working-order arm — recorded strings; must equal the "oanda" table row.
    for (tif, want) in [(Gtc, "GTC"), (Ioc, "IOC"), (Fok, "FOK"), (Gtd, "GTD"), (Day, "GFD")] {
        assert_eq!(oanda_time_in_force(tif, false), want, "working {tif:?}");
        assert_eq!(
            vike_venues::venue_time_in_force::venue_time_in_force(VENUE, tif).wire(),
            Some(want),
            "table row {tif:?}"
        );
    }
    // MARKET arm — local by design (OANDA MARKET accepts only FOK/IOC): recorded strings.
    for (tif, want) in [(Gtc, "FOK"), (Ioc, "IOC"), (Fok, "FOK"), (Gtd, "FOK"), (Day, "FOK")] {
        assert_eq!(oanda_time_in_force(tif, true), want, "market {tif:?}");
    }
}

#[test]
fn market_fill_maps_to_accepted_plus_filled() {
    let response: serde_json::Value = serde_json::from_str(
            r#"{
                "orderCreateTransaction": {"id": "6372", "type": "MARKET_ORDER"},
                "orderFillTransaction": {"id": "6373", "time": "1478012400.000000000",
                    "instrument": "EUR_USD", "units": "1000", "price": "1.09000", "commission": "0.04"},
                "lastTransactionID": "6373"
            }"#,
        )
        .unwrap();
    // Dual-publish: Accepted, then the bare Fill (Account folds position/PnL), then the
    // OrderFilled wrap (FSM), both fills carrying the same trade_id.
    let evs = map_order_response("order-client-id-1", 111, &response, &Default::default());
    assert_eq!(evs.len(), 3);
    assert_matches!(
        &evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("6372")
    );
    match &evs[1] {
        Event::Fill(fill) => {
            assert_eq!(fill.trade_id, "6373");
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0);
            assert_eq!(fill.last_price, 1.09);
            assert_eq!(fill.commission, 0.04);
            assert_eq!(fill.timestamp, 1_478_012_400_000);
        }
        other => panic!("expected bare Fill second, got {other:?}"),
    }
    match &evs[2] {
        Event::OrderFilled(of) => {
            assert_eq!(of.fill.trade_id, "6373"); // same fill on the wrap
            assert_eq!(of.fill.side, 1);
        }
        other => panic!("expected OrderFilled wrap third, got {other:?}"),
    }
}

#[test]
fn last_transaction_id_watermark_advances_monotonically() {
    let last_seen = Arc::new(AtomicU64::new(0));
    // A POST response carrying lastTransactionID=6373 advances the watermark.
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "6373"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);
    // A LOWER (stale/out-of-order) id must NOT move it backwards.
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "10"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);
    // A higher one advances it.
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "9000"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 9000);
    // A response without the field is a no-op.
    note_last_transaction_id(&last_seen, &serde_json::json!({"orderCreateTransaction": {}}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 9000);
}

/// The transactions stream advances the same watermark by each transaction's `id`, through the one
/// `note_max_id`: monotonic, and a missing or non-numeric id moves nothing.
#[test]
fn a_streamed_transaction_id_advances_the_watermark_monotonically() {
    let last_seen = Arc::new(AtomicU64::new(0));
    note_max_id(&last_seen, &serde_json::json!({"id": "6373"}), "id");
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);
    note_max_id(&last_seen, &serde_json::json!({"id": "10"}), "id");
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);
    note_max_id(&last_seen, &serde_json::json!({"id": "x", "lastTransactionID": "9000"}), "id");
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373, "only `id` is read, and `x` is no id");
}

#[test]
fn reject_maps_to_rejected() {
    let response: serde_json::Value = serde_json::from_str(
        r#"{"orderRejectTransaction": {"id": "6372", "rejectReason": "INSUFFICIENT_MARGIN"}}"#,
    )
    .unwrap();
    let evs = map_order_response("order-client-id-1", 111, &response, &Default::default());
    assert_eq!(evs.len(), 1);
    assert_matches!(&evs[0], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_MARGIN");
}

// --- order REPLACE (the native amend) ----------------------------------------------------------

#[test]
fn replace_body_reprices_a_limit_and_keeps_every_other_term() {
    let mut r = req("limit", -1, 500.0);
    r.time_in_force = TimeInForce::Gtd;
    r.gtd_expiry = Some(1_700_000_000_000);
    let b = build_replace_body(&r, None, Some(1.0950)).expect("a resting limit can be replaced");
    let o = &b["order"];
    assert_eq!(o["type"], "LIMIT");
    assert_eq!(o["price"], "1.09500", "the new price");
    assert_eq!(o["units"], "-500", "qty kept, sell stays negative");
    assert_eq!(o["instrument"], "EUR_USD");
    assert_eq!(o["timeInForce"], "GTD", "TIF kept");
    assert_eq!(o["gtdTime"], "1700000000", "GTD expiry kept");
    // THE IDENTITY: the replacement carries the SAME client id, so `@order_client_id` and every later fill's
    // `clientOrderID` still name this order.
    assert_eq!(o["clientExtensions"]["id"], "order-client-id-1");
}

#[test]
fn replace_body_resizes_and_may_do_both_at_once() {
    let b = build_replace_body(&req("limit", 1, 500.0), Some(750.0), None).unwrap();
    assert_eq!(b["order"]["units"], "750");
    assert_eq!(b["order"]["price"], "1.09000", "price kept when only qty changes");
    let b = build_replace_body(&req("limit", 1, 500.0), Some(750.4), Some(1.1)).unwrap();
    assert_eq!(b["order"]["units"], "750", "rounded to whole units like a submit");
    assert_eq!(b["order"]["price"], "1.10000");
}

#[test]
fn replace_body_moves_a_stops_trigger_not_a_limit_price() {
    // `req` gives price 1.09 and trigger 1.08: a STOP rests on its trigger.
    let b = build_replace_body(&req("stop", 1, 100.0), None, Some(1.07)).unwrap();
    assert_eq!(b["order"]["type"], "STOP");
    assert_eq!(b["order"]["price"], "1.07000");
    let kept = build_replace_body(&req("stop", 1, 100.0), Some(200.0), None).unwrap();
    assert_eq!(kept["order"]["price"], "1.08000", "an unchanged stop keeps its TRIGGER");
}

#[test]
fn replace_body_refuses_what_cannot_rest_or_cannot_be_sized() {
    assert!(build_replace_body(&req("market", 1, 100.0), None, Some(1.0)).is_err());
    assert!(build_replace_body(&req("take_profit", 1, 100.0), None, Some(1.0)).is_err());
    assert!(build_replace_body(&req("limit", 1, 100.0), Some(0.0), None).is_err());
    assert!(build_replace_body(&req("limit", 1, 100.0), Some(0.4), None).is_err());
    assert!(build_replace_body(&req("limit", 1, 100.0), Some(f64::NAN), None).is_err());
    assert!(build_replace_body(&req("limit", 1, 100.0), None, Some(0.0)).is_err());
    assert!(build_replace_body(&req("limit", 1, 100.0), None, Some(f64::INFINITY)).is_err());
    let mut no_price = req("limit", 1, 100.0);
    no_price.price = None;
    assert!(build_replace_body(&no_price, None, None).is_err(), "no price to carry over");
}

const REPLACE_REPLY: &str = r#"{
    "orderCancelTransaction": {"id": "6401", "type": "ORDER_CANCEL", "orderID": "6372",
        "clientOrderID": "order-client-id-1", "reason": "CLIENT_REQUEST_REPLACED", "replacedByOrderID": "6402"},
    "orderCreateTransaction": {"id": "6402", "type": "LIMIT_ORDER", "time": "1478012400.5",
        "instrument": "EUR_USD", "units": "-500", "price": "1.09500", "replacesOrderID": "6372",
        "clientExtensions": {"id": "order-client-id-1"}},
    "relatedTransactionIDs": ["6401", "6402"],
    "lastTransactionID": "6402"
}"#;

#[test]
fn replace_reply_is_one_order_modified_with_the_new_venue_id() {
    let response: serde_json::Value = serde_json::from_str(REPLACE_REPLY).unwrap();
    let evs = map_replace_response("order-client-id-1", 111, &response, &Default::default());
    assert_eq!(evs.len(), 1, "no cancel, no accept: ONE logical order");
    match &evs[0] {
        Event::OrderModified(m) => {
            assert_eq!(
                m.order_client_id, "order-client-id-1",
                "the platform identity does not change"
            );
            assert_eq!(m.venue_order_id.as_deref(), Some("6402"), "the venue's NEW order id");
            assert_eq!(m.new_qty, Some(500.0), "units echoed by the venue, unsigned");
            assert_eq!(m.new_price, Some(1.095));
            assert_eq!(
                m.timestamp, 1_478_012_400_500,
                "the replace's own time, not the order's 111"
            );
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

#[test]
fn replace_reply_with_an_immediate_fill_dual_publishes_after_the_modify() {
    let response = serde_json::json!({
        "orderCancelTransaction": {"id": "1", "reason": "CLIENT_REQUEST_REPLACED"},
        "orderCreateTransaction": {"id": "2", "units": "500", "price": "1.1"},
        "orderFillTransaction": {"id": "3", "time": "1478012400.000000000",
            "instrument": "EUR_USD", "units": "500", "price": "1.1", "commission": "0.01"}
    });
    let evs = map_replace_response("order-client-id-1", 111, &response, &Default::default());
    assert_eq!(evs.len(), 3);
    assert_matches!(&evs[0], Event::OrderModified(_));
    assert_matches!(&evs[1], Event::Fill(f) if f.order_client_id == "order-client-id-1" && f.trade_id == "3");
    assert_matches!(&evs[2], Event::OrderFilled(w) if w.fill.trade_id == "3");
}

#[test]
fn a_replacement_cancelled_at_once_ends_the_order() {
    let response = serde_json::json!({
        "orderCancelTransaction": {"id": "1", "reason": "CLIENT_REQUEST_REPLACED"},
        "orderCreateTransaction": {"id": "2", "units": "500", "price": "1.1"},
        "replacingOrderCancelTransaction": {"id": "3", "reason": "TIME_IN_FORCE_EXPIRED"}
    });
    let evs = map_replace_response("order-client-id-1", 111, &response, &Default::default());
    assert_eq!(evs.len(), 2);
    assert_matches!(&evs[0], Event::OrderModified(_));
    assert_matches!(
        &evs[1], Event::OrderCanceled(c) if c.order_client_id == "order-client-id-1"
            && c.reason == "TIME_IN_FORCE_EXPIRED"
    );
}

#[test]
fn a_replace_reply_that_created_nothing_is_a_rejection_never_assumed_success() {
    // OANDA's refusal is its own; a reply that created nothing says neither that the replace landed nor that it did not.
    let unknown = Some(OrderRefusalId::OutcomeUnknown.word());
    let refused = Some(OrderRefusalId::VenueRefused.word());
    for (response, id) in [
        (serde_json::json!({}), unknown),
        (serde_json::json!({"orderCancelTransaction": {"id": "1"}}), unknown),
        (
            serde_json::json!({"orderRejectTransaction": {"rejectReason": "PRICE_PRECISION_EXCEEDED"}}),
            refused,
        ),
    ] {
        let evs = map_replace_response("order-client-id-1", 111, &response, &Default::default());
        assert_eq!(evs.len(), 1, "{response}");
        // no transaction time in these replies: stamped when read, never at the order's 111
        assert_matches!(
            &evs[0],
            Event::OrderModifyRejected(r) if r.order_client_id == "order-client-id-1" && r.timestamp > 111
                && r.refusal_id.as_deref() == id,
            "{response}"
        );
    }
    let rej = map_replace_response(
        "order-client-id-1",
        1,
        &serde_json::json!({"orderRejectTransaction": {"rejectReason": "PRICE_PRECISION_EXCEEDED"}}),
        &Default::default(),
    );
    assert_matches!(
        &rej[0], Event::OrderModifyRejected(r) if r.reason == "PRICE_PRECISION_EXCEEDED"
    );
}

// --- the REAL run loop against a loopback OANDA double ---------------------------------------

pub(crate) mod loopback {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::mpsc::channel;

    pub(crate) type Served = Vec<(String, String)>;

    /// A loopback HTTP server answering each request with the next `(status, body)` of `script`.
    /// Its join value is every request it served as `(request line, body)`.
    pub(crate) fn spawn_double(
        script: Vec<(u16, &'static str)>,
    ) -> (SocketAddr, thread::JoinHandle<Served>) {
        spawn_double_after(std::time::Duration::ZERO, script)
    }

    /// [`spawn_double`], waiting `slow` before each reply.
    pub(crate) fn spawn_double_after(
        slow: std::time::Duration,
        script: Vec<(u16, &'static str)>,
    ) -> (SocketAddr, thread::JoinHandle<Served>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let handle = thread::spawn(move || {
            let mut served = Vec::new();
            for (status, body) in script {
                let Ok((mut sock, _)) = listener.accept() else { break };
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 1024];
                let (head_end, content_length) = loop {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break (buffer.len(), 0);
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buffer[..p]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (p + 4, len);
                    }
                };
                while buffer.len() < head_end + content_length {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&buffer).to_string();
                let request_line = text.lines().next().unwrap_or_default().to_string();
                served.push((request_line, text[head_end.min(text.len())..].to_string()));
                thread::sleep(slow);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes());
                let _ = sock.flush();
            }
            served
        });
        (addr, handle)
    }

    /// Drive the REAL `run` loop with `commands` against `addr`; return every event it emitted and the
    /// final A3 watermark.
    fn drive(addr: SocketAddr, commands: Vec<ExecutionClientActorCommand>) -> (Vec<Event>, u64) {
        drive_with(addr, commands, submitted_symbols())
    }

    /// [`drive`] with the submitted-symbol table the loop reads, so a test can record one.
    fn drive_with(
        addr: SocketAddr,
        commands: Vec<ExecutionClientActorCommand>,
        symbols: Arc<OrderClientIdSymbols>,
    ) -> (Vec<Event>, u64) {
        drive_on(addr, commands, symbols, AccountHedging::stated(false))
    }

    /// [`drive_with`] on an account whose hedging is `hedging`: `AccountHedging::default()` is "not asked yet",
    /// so the loop's own first `GET /summary` is the first request the double serves.
    fn drive_on(
        addr: SocketAddr,
        commands: Vec<ExecutionClientActorCommand>,
        symbols: Arc<OrderClientIdSymbols>,
        hedging: AccountHedging,
    ) -> (Vec<Event>, u64) {
        crate::test_support::kept_list_for_tests();
        let config = OandaConfig {
            api_token: vike_model::secret::Secret::new("tok".to_string()),
            account_id: "acct-1".to_string(),
            rest_base: format!("http://{addr}"),
            stream_base: format!("http://{addr}"),
        };
        let (events, mut ingest) = vike_execution::event_channel(64);
        let (tx, rx) = channel();
        let last_seen = Arc::new(AtomicU64::new(0));
        let seen = last_seen.clone();
        let lane = thread::spawn(move || {
            let book = GridCache::preloaded([("EUR_USD", InstrumentGrid::MAJORS)]);
            let fence = Arc::new(SubmitFence::new(events.clone()));
            run_on(config, events, &rx, seen, symbols, fence, book, hedging)
        });
        for c in commands {
            tx.send(c).expect("the loop is alive");
        }
        tx.send(ExecutionClientActorCommand::Shutdown).expect("the loop is alive");
        lane.join().expect("the loop returns on Shutdown");
        let mut out = Vec::new();
        while let Some(i) = ingest.blocking_recv() {
            match i {
                vike_execution::Ingest::Event(e) => out.push(e),
                other => panic!("expected Ingest::Event, got {other:?}"),
            }
        }
        (out, last_seen.load(Ordering::Relaxed))
    }

    fn modify(price: Option<f64>, qty: Option<f64>) -> ExecutionClientActorCommand {
        ExecutionClientActorCommand::Modify {
            order: Box::new(req("limit", 1, 500.0)),
            new_qty: qty,
            new_price: price,
            new_trigger_price: None,
            queued_at: std::time::Instant::now(),
        }
    }

    /// The reply of a replace was lost and the replace landed: the stream drops both its halves, so the old code said
    /// `OrderModifyRejected` ("the order keeps its terms") over an order the venue had repriced, for ever.
    #[test]
    fn a_replace_whose_reply_was_lost_and_that_landed_is_order_modified() {
        let (addr, server) = spawn_double(vec![
            (504, "<html>"),
            (200, r#"{"order":{"id":"7777","state":"PENDING","units":"500","price":"1.09500"}}"#),
        ]);
        let (evs, _) = drive(addr, vec![modify(Some(1.095), None)]);
        assert_matches!(
            evs.as_slice(),
            [Event::OrderModified(m)] if m.order_client_id == "order-client-id-1"
                && m.venue_order_id.as_deref() == Some("7777")
                && m.new_qty == Some(500.0)
                && m.new_price == Some(1.095)
        );
        let served = server.join().unwrap();
        assert_eq!(served[0].0, "PUT /v3/accounts/acct-1/orders/@order-client-id-1 HTTP/1.1");
        assert_eq!(served[1].0, "GET /v3/accounts/acct-1/orders/@order-client-id-1 HTTP/1.1");
    }

    /// The id an OUTCOME UNKNOWN advisory carries.
    fn outcome_unknown() -> Option<&'static str> {
        Some(vike_model::OrderRefusalId::OutcomeUnknown.word())
    }

    /// Read back at its old terms the order keeps them, and the advisory says how that is known; it is no refusal: the
    /// reply was lost, so its outcome stays unknown (decision 0303).
    #[test]
    fn a_replace_whose_reply_was_lost_and_that_did_not_land_keeps_the_old_terms() {
        let (addr, server) = spawn_double(vec![
            (504, "<html>"),
            (200, r#"{"order":{"id":"7777","state":"PENDING","units":"500","price":"1.09000"}}"#),
        ]);
        let (evs, _) = drive(addr, vec![modify(Some(1.095), None)]);
        assert_matches!(
            evs.as_slice(),
            [Event::OrderModifyRejected(r)] if r.reason.contains("holds the old terms")
                && r.refusal_id.as_deref() == outcome_unknown()
        );
        server.join().unwrap();
    }

    /// A read that shows nothing, or an order no longer pending, is no answer: the advisory says the outcome is unknown.
    #[test]
    fn a_replace_whose_reply_was_lost_and_cannot_be_read_back_says_unknown() {
        for read in [
            (500, "boom"),
            (200, r#"{"order":{"id":"7777","state":"FILLED","units":"500","price":"1.09500"}}"#),
            (200, r#"{"order":{"id":"7777","state":"PENDING"}}"#),
        ] {
            let (addr, server) = spawn_double(vec![(504, "<html>"), read]);
            let (evs, _) = drive(addr, vec![modify(Some(1.095), None)]);
            assert_matches!(
                evs.as_slice(),
                [Event::OrderModifyRejected(r)] if r.reason.contains("outcome is UNKNOWN")
                    && r.refusal_id.as_deref() == outcome_unknown(),
                "{read:?}"
            );
            server.join().unwrap();
        }
    }

    fn cancel() -> ExecutionClientActorCommand {
        ExecutionClientActorCommand::Cancel {
            order_client_id: "order-client-id-1".to_string(),
            intent: vike_model::OrderCancelIntent::Unspecified,
        }
    }

    /// **A cancel OANDA may have acted on is OUTCOME UNKNOWN, never a refused cancel, and the order is read back by its
    /// client id at once** (decision 0303): a bare 500 to the cancel PUT (the agent's lost reply reads the same) used to
    /// be a refused cancel. A CANCELLED read ends the order; a PENDING one adds nothing (the stream or reconcile ends it).
    /// A refusal keeps OANDA's words and reads nothing.
    #[test]
    fn a_cancel_of_unknown_outcome_is_said_so_and_read_back_by_its_client_id() {
        for (state, ends) in [("CANCELLED", true), ("PENDING", false)] {
            let read = if ends {
                r#"{"order":{"id":"7777","state":"CANCELLED"}}"#
            } else {
                r#"{"order":{"id":"7777","state":"PENDING"}}"#
            };
            let (addr, server) =
                spawn_double(vec![(500, r#"{"errorMessage":"boom"}"#), (200, read)]);
            let (evs, _) = drive(addr, vec![cancel()]);
            assert_matches!(
                evs.first(),
                Some(Event::OrderCancelRejected(r)) if r.refusal_id.as_deref() == outcome_unknown()
                    && r.reason.starts_with("cancel outcome unknown"),
                "{state}: {evs:?}"
            );
            match &evs[1..] {
                [Event::OrderCanceled(c)] if ends => {
                    assert_eq!(c.order_client_id, "order-client-id-1")
                }
                [] if !ends => {}
                other => panic!("{state}: the read's cancel alone: {other:?}"),
            }
            let served = server.join().unwrap();
            assert_eq!(
                served[0].0,
                "PUT /v3/accounts/acct-1/orders/@order-client-id-1/cancel HTTP/1.1"
            );
            assert_eq!(served[1].0, "GET /v3/accounts/acct-1/orders/@order-client-id-1 HTTP/1.1");
        }

        let (addr, server) =
            spawn_double(vec![(400, r#"{"errorMessage":"The Order cannot be cancelled"}"#)]);
        let (evs, _) = drive(addr, vec![cancel()]);
        assert_matches!(
            evs.as_slice(),
            [Event::OrderCancelRejected(r)]
                if r.refusal_id.as_deref() == Some(vike_model::OrderRefusalId::VenueRefused.word())
                    && r.reason == "OANDA error 400: The Order cannot be cancelled"
        );
        assert_eq!(server.join().unwrap().len(), 1, "a refusal reads nothing");
    }

    /// A 2xx naming no `orderCancelTransaction` says neither that the cancel took nor that OANDA refused it: OUTCOME
    /// UNKNOWN (decision 0303), never a refused cancel.
    #[test]
    fn a_cancel_reply_with_no_cancel_transaction_is_of_unknown_outcome() {
        let (addr, server) = spawn_double(vec![(200, r#"{"lastTransactionID":"1"}"#)]);
        let (evs, _) = drive(addr, vec![cancel()]);
        assert_eq!(server.join().expect("double joins").len(), 1);
        assert_matches!(
            evs.as_slice(),
            [Event::OrderCancelRejected(r)] if r.refusal_id.as_deref() == outcome_unknown()
        );
    }

    #[test]
    fn a_modify_puts_the_replacing_order_at_the_order_client_id_and_emits_order_modified() {
        let (addr, server) = spawn_double(vec![(201, REPLACE_REPLY)]);
        let (evs, last_seen) = drive(addr, vec![modify(Some(1.095), None)]);

        let served = server.join().expect("double joins");
        assert_eq!(served.len(), 1);
        assert_eq!(served[0].0, "PUT /v3/accounts/acct-1/orders/@order-client-id-1 HTTP/1.1");
        let body: serde_json::Value = serde_json::from_str(&served[0].1).expect("a JSON body");
        assert_eq!(body["order"]["type"], "LIMIT");
        assert_eq!(body["order"]["price"], "1.09500");
        assert_eq!(body["order"]["units"], "500");
        assert_eq!(body["order"]["clientExtensions"]["id"], "order-client-id-1");

        assert_eq!(evs.len(), 1);
        assert_matches!(
            &evs[0], Event::OrderModified(m) if m.order_client_id == "order-client-id-1"
                && m.venue_order_id.as_deref() == Some("6402")
        );
        assert_eq!(last_seen, 6402, "the A3 watermark advances from the replace reply too");
    }

    /// The REAL `run` loop labels a market order's inline fill with the symbol the order was submitted
    /// under (`EURUSD`, the venue answers `EUR_USD`) and, once the order is filled, lets its entry go.
    #[test]
    fn the_run_loop_labels_an_inline_fill_and_then_forgets_the_order() {
        let (addr, server) = spawn_double(vec![
            (200, r#"{"lastTransactionID":"6400"}"#),
            (
                201,
                r#"{"orderCreateTransaction":{"id":"6401","type":"MARKET_ORDER"},
                    "orderFillTransaction":{"id":"6402","time":"1.0","instrument":"EUR_USD",
                        "units":"100","price":"1.09","commission":"0.01"},
                    "lastTransactionID":"6402"}"#,
            ),
        ]);
        let symbols = submitted_symbols();
        symbols.record("order-client-id-1", "EURUSD");
        let (evs, _) = drive_with(
            addr,
            vec![ExecutionClientActorCommand::Submit(
                Box::new(req("market", 1, 100.0)),
                std::time::Instant::now(),
            )],
            symbols.clone(),
        );
        server.join().expect("double joins");
        assert_eq!(fill_symbols(&evs), ["EURUSD"], "{evs:?}");
        assert_eq!(
            symbols.label("order-client-id-1", "EUR_USD"),
            "EUR_USD",
            "a filled order's entry left"
        );
    }

    /// **An order on the USER name goes out on its kept row's wire name, and its fill comes back under the user
    /// name** (decision 0290), through the REAL loop: `EURUSD` is POSTed as `instrument: EUR_USD`, and the reply's
    /// `EUR_USD` fill books as `EURUSD` even with nothing recorded in the submitted-symbol table.
    #[test]
    fn an_order_on_the_user_name_posts_the_wire_name_and_its_fill_comes_back_under_the_user_name() {
        let (addr, server) = spawn_double(vec![
            (200, r#"{"lastTransactionID":"6400"}"#),
            (
                201,
                r#"{"orderCreateTransaction":{"id":"6401","type":"MARKET_ORDER"},
                    "orderFillTransaction":{"id":"6402","time":"1.0","instrument":"EUR_USD",
                        "units":"100","price":"1.09","commission":"0.01"},
                    "lastTransactionID":"6402"}"#,
            ),
        ]);
        let (evs, _) = drive(
            addr,
            vec![ExecutionClientActorCommand::Submit(
                Box::new(req("market", 1, 100.0)),
                std::time::Instant::now(),
            )],
        );
        let served = server.join().expect("double joins");
        assert_eq!(served[1].0, "POST /v3/accounts/acct-1/orders HTTP/1.1");
        let body: serde_json::Value = serde_json::from_str(&served[1].1).expect("a JSON body");
        assert_eq!(body["order"]["instrument"], "EUR_USD");
        assert_eq!(fill_symbols(&evs), ["EURUSD"], "{evs:?}");
    }

    /// **A name with no kept row is refused in words before anything is read or sent** (decision 0290), the old
    /// wire spelling `EUR_USD` included: a submit is the terminal `OrderRejected`, a replace the non-terminal
    /// `OrderModifyRejected` (port 9: any read or write would have answered with a network error instead).
    #[test]
    fn a_name_with_no_kept_row_is_refused_and_never_reaches_the_wire() {
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        for name in ["EUR_USD", "NOPEUSD"] {
            let order = OrderRequest { symbol: name.into(), ..req("limit", 1, 100.0) };
            let (evs, _) = drive(
                addr,
                vec![
                    ExecutionClientActorCommand::Submit(
                        Box::new(order.clone()),
                        std::time::Instant::now(),
                    ),
                    ExecutionClientActorCommand::Modify {
                        order: Box::new(order),
                        new_qty: None,
                        new_price: Some(1.1),
                        new_trigger_price: None,
                        queued_at: std::time::Instant::now(),
                    },
                ],
            );
            match evs.as_slice() {
                [
                    Event::OrderSubmitted(_),
                    Event::OrderRejected(r),
                    Event::OrderModifyRejected(m),
                ] => {
                    let not_sent = Some(OrderRefusalId::NotSent.word());
                    assert_eq!(r.refusal_id.as_deref(), not_sent, "{name}");
                    assert_eq!(m.refusal_id.as_deref(), not_sent, "{name}");
                    for why in [&*r.reason, &*m.reason] {
                        assert!(
                            why.contains(name)
                                && why.contains("kept list")
                                && why.contains("nothing was sent"),
                            "{name}: {why}"
                        );
                    }
                }
                other => {
                    panic!("{name}: expected [Submitted, Rejected, ModifyRejected], got {other:?}")
                }
            }
        }
    }

    /// **An order that waited past its time on this thread's queue is refused by name and costs no
    /// call**, a cancel behind it still goes out (`vike_bridge_core::exec_actor`'s `MAX_SUBMIT_WAIT`).
    /// Port 9: a read of the grid or the POST would have answered with a network error instead.
    #[test]
    fn an_order_past_its_wait_is_refused_and_never_reaches_the_wire() {
        use vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT;
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let stale = std::time::Instant::now() - MAX_SUBMIT_WAIT - std::time::Duration::from_secs(1);
        let (evs, _) = drive(
            addr,
            vec![ExecutionClientActorCommand::Submit(Box::new(req("market", 1, 100.0)), stale)],
        );
        match evs.as_slice() {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
                assert!(r.reason.contains("NOT sent"), "{}", r.reason);
            }
            other => panic!("expected [Submitted, Rejected], got {other:?}"),
        }
    }

    /// **A kind this bridge builds no order for is refused by name in the REAL loop and costs no call**
    /// (port 9: a read of the grid or the POST would have answered with a network error instead).
    #[test]
    fn a_kind_this_bridge_does_not_build_is_refused_and_never_reaches_the_wire() {
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        for kind in ["stop_limit", "take_profit", "market_if_touched"] {
            let now = std::time::Instant::now();
            let (evs, _) = drive(
                addr,
                vec![ExecutionClientActorCommand::Submit(Box::new(req(kind, 1, 100.0)), now)],
            );
            match evs.as_slice() {
                [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
                    assert!(r.reason.contains(&format!("`{kind}`")), "{}", r.reason);
                    assert_eq!(
                        r.refusal_id.as_deref(),
                        Some(OrderRefusalId::VenueCapability.word()),
                        "{kind}"
                    );
                }
                other => panic!("{kind}: expected [Submitted, Rejected], got {other:?}"),
            }
        }
    }

    /// **The reads in front of the POST spend what an order had left**: this one is young at dequeue and past its wait
    /// when the watermark read ends, so it is refused by name and the venue hears no POST.
    #[test]
    fn an_order_that_outlives_its_wait_in_the_reads_before_the_post_is_not_sent() {
        use vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT;
        let (addr, server) = spawn_double_after(
            std::time::Duration::from_millis(1500),
            vec![(200, r#"{"lastTransactionID":"1"}"#)],
        );
        let nearly =
            std::time::Instant::now() - MAX_SUBMIT_WAIT + std::time::Duration::from_secs(1);
        let (evs, _) = drive(
            addr,
            vec![ExecutionClientActorCommand::Submit(Box::new(req("market", 1, 100.0)), nearly)],
        );
        let served = server.join().expect("double joins");
        assert_eq!(served.len(), 1, "the read, and no POST: {served:?}");
        assert_matches!(
            evs.as_slice(),
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] if r.reason.contains("NOT sent")
        );
    }

    /// OANDA's own refusal of the create (a 4xx: an answer, so the order is not on the venue) is `order.venue_refused`.
    #[test]
    fn a_create_oanda_refuses_carries_the_venue_refused_id() {
        let (addr, server) = spawn_double(vec![
            (200, r#"{"lastTransactionID":"1"}"#),
            (400, r#"{"errorMessage":"Invalid value specified for 'units'"}"#),
        ]);
        let now = std::time::Instant::now();
        let (evs, _) = drive(
            addr,
            vec![ExecutionClientActorCommand::Submit(Box::new(req("market", 1, 100.0)), now)],
        );
        assert_eq!(server.join().expect("double joins").len(), 2, "the read and the POST");
        assert_matches!(
            evs.as_slice(),
            [Event::OrderSubmitted(_), Event::OrderRejected(r)]
                if r.refusal_id.as_deref() == Some(OrderRefusalId::VenueRefused.word())
        );
    }

    /// A close as a flatten leg is: market, reduce-only, on a netting account.
    fn old_close(qty: f64) -> ExecutionClientActorCommand {
        use vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT;
        let stale = std::time::Instant::now() - MAX_SUBMIT_WAIT - std::time::Duration::from_secs(1);
        let mut close = req("market", -1, qty);
        (close.reduce_only, close.trigger_price, close.price) = (true, None, None);
        ExecutionClientActorCommand::Submit(Box::new(close), stale)
    }

    /// **A close past its wait is not refused for its age**: on this netting account no word stops it opening the
    /// other way, so it reads the position and goes at what still faces it.
    #[test]
    fn a_close_past_its_wait_reads_the_position_and_goes_cut_to_it() {
        let held = r#"{"positions":[{"instrument":"EUR_USD","long":{"units":"60"},"short":{"units":"0"}}]}"#;
        let (addr, server) = spawn_double(vec![
            (200, held),
            (200, r#"{"lastTransactionID":"1"}"#),
            (201, r#"{"orderCreateTransaction":{"id":"7"},"lastTransactionID":"7"}"#),
        ]);
        let (evs, _) = drive(addr, vec![old_close(100.0)]);
        let served = server.join().expect("double joins");
        assert!(served[0].0.contains("/positions"), "{served:?}");
        let (_, body) = served.iter().find(|(line, _)| line.starts_with("POST")).expect("it went");
        assert!(body.contains(r#""units":"-60""#), "cut to what faces it: {body}");
        assert!(!evs.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{evs:?}");
    }

    /// The position went while it waited: the close is refused by name, since sent it would open the other way.
    #[test]
    fn a_close_past_its_wait_with_nothing_to_close_is_refused_and_sends_nothing() {
        let (addr, server) = spawn_double(vec![(200, r#"{"positions":[]}"#)]);
        let (evs, _) = drive(addr, vec![old_close(100.0)]);
        let served = server.join().expect("double joins");
        assert!(!served.iter().any(|(line, _)| line.starts_with("POST")), "{served:?}");
        assert_matches!(
            evs.as_slice(),
            [Event::OrderSubmitted(_), Event::OrderRejected(r)]
                if r.reason.contains("NOT sent") && r.reason.contains("flat")
        );
    }

    /// **An amend that waited past `MAX_MODIFY_WAIT` is refused unsent, a fresh one is sent, and a cancel queued
    /// behind stale amends goes out** (`vike_bridge_core::exec_actor`'s `run_loop` twin, on this venue's own loop):
    /// the double serves exactly the cancel and the fresh replace.
    #[test]
    fn a_stale_amend_is_refused_unsent_and_a_cancel_behind_stale_amends_goes_out() {
        use vike_bridge_core::exec_actor::MAX_MODIFY_WAIT;
        let stale = std::time::Instant::now() - MAX_MODIFY_WAIT - std::time::Duration::from_secs(1);
        let amend = |order_client_id: &str, queued_at| {
            let mut order = req("limit", 1, 500.0);
            order.order_client_id = order_client_id.into();
            ExecutionClientActorCommand::Modify {
                order: Box::new(order),
                new_qty: None,
                new_price: Some(1.095),
                new_trigger_price: None,
                queued_at,
            }
        };
        let (addr, server) = spawn_double(vec![
            (200, r#"{"orderCancelTransaction":{"id":"6400"}}"#),
            (201, REPLACE_REPLY),
        ]);
        let (evs, _) = drive(
            addr,
            vec![
                amend("stale-1", stale),
                amend("stale-2", stale),
                ExecutionClientActorCommand::Cancel {
                    order_client_id: "resting".into(),
                    intent: vike_model::OrderCancelIntent::Unspecified,
                },
                amend("order-client-id-1", std::time::Instant::now()),
            ],
        );
        let served = server.join().expect("double joins");
        assert_eq!(
            served.iter().map(|(line, _)| line.as_str()).collect::<Vec<_>>(),
            [
                "PUT /v3/accounts/acct-1/orders/@resting/cancel HTTP/1.1",
                "PUT /v3/accounts/acct-1/orders/@order-client-id-1 HTTP/1.1",
            ],
            "no stale amend reached the venue"
        );
        match evs.as_slice() {
            [
                Event::OrderModifyRejected(a),
                Event::OrderModifyRejected(b),
                Event::OrderCanceled(c),
                Event::OrderModified(m),
            ] => {
                for (refused, order_client_id) in [(a, "stale-1"), (b, "stale-2")] {
                    assert_eq!(refused.order_client_id, order_client_id);
                    assert!(
                        refused.reason.contains("NOT sent") && refused.reason.contains("5 s"),
                        "{}",
                        refused.reason
                    );
                    assert_eq!(refused.refusal_id.as_deref(), Some(OrderRefusalId::NotSent.word()));
                    assert!(refused.timestamp > 0, "stamped when made");
                }
                assert_eq!(
                    (c.order_client_id.as_str(), m.order_client_id.as_str()),
                    ("resting", "order-client-id-1")
                );
            }
            other => panic!("expected [ModifyRejected x2, Canceled, Modified], got {other:?}"),
        }
    }

    #[test]
    fn a_venue_refusal_is_an_order_modify_rejected_with_oandas_message() {
        let (addr, server) = spawn_double(vec![
            (400, r#"{"errorMessage":"Invalid value specified for 'price'","errorCode":"X"}"#),
            (404, r#"{"errorMessage":"The Order specified does not exist"}"#),
        ]);
        let (evs, _) = drive(addr, vec![modify(Some(1.0), None), modify(None, Some(900.0))]);
        assert_eq!(server.join().expect("double joins").len(), 2);
        assert_eq!(evs.len(), 2, "every modify answers with an event: {evs:?}");
        let refused = Some(OrderRefusalId::VenueRefused.word());
        assert_matches!(
            &evs[0], Event::OrderModifyRejected(r) if r.order_client_id == "order-client-id-1"
                && r.reason.contains("Invalid value specified for 'price'")
                && r.refusal_id.as_deref() == refused
        );
        assert_matches!(
            &evs[1], Event::OrderModifyRejected(r) if r.reason.contains("does not exist")
                && r.refusal_id.as_deref() == refused
        );
    }

    #[test]
    fn a_modify_that_cannot_be_expressed_never_touches_the_wire() {
        // port 9 (discard): if anything dialled it, the reason would be a network error instead
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let mut market = req("market", 1, 100.0);
        market.order_client_id = "m1".into();
        let (evs, _) = drive(
            addr,
            vec![ExecutionClientActorCommand::Modify {
                order: Box::new(market),
                new_qty: None,
                new_price: Some(1.0),
                new_trigger_price: None,
                queued_at: std::time::Instant::now(),
            }],
        );
        assert_eq!(evs.len(), 1);
        assert_matches!(
            &evs[0], Event::OrderModifyRejected(r) if r.order_client_id == "m1"
                && r.reason.contains("only a resting limit or stop")
        );
    }

    #[test]
    fn a_dead_venue_is_a_rejection_not_a_silent_loss() {
        // nothing listens on the discard port: the PUT fails at connect
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let (evs, _) = drive(addr, vec![modify(Some(1.0), None)]);
        assert_eq!(evs.len(), 1);
        assert_matches!(
            &evs[0], Event::OrderModifyRejected(r) if r.reason.contains("network error")
                && r.refusal_id.as_deref() == Some(OrderRefusalId::NotSent.word())
        );
    }

    // --- hedging: what a reduce-only order carries (`crate::account::modes`) -------------------------------

    const HEDGING_SUMMARY: &str =
        r#"{"account":{"hedgingEnabled":true},"lastTransactionID":"6400"}"#;
    const NETTING_SUMMARY: &str =
        r#"{"account":{"hedgingEnabled":false},"lastTransactionID":"6400"}"#;
    /// The summary the first submit reads for its watermark (it states no hedging).
    const WATERMARK: &str = r#"{"lastTransactionID":"6400"}"#;
    const FILLED: &str = r#"{"orderCreateTransaction":{"id":"6401","type":"MARKET_ORDER"},
        "orderFillTransaction":{"id":"6402","time":"1.0","instrument":"EUR_USD","units":"1000",
            "price":"1.09","commission":"0.01"},"lastTransactionID":"6402"}"#;
    const RESTING: &str = r#"{"orderCreateTransaction":{"id":"6401","type":"LIMIT_ORDER"},"lastTransactionID":"6401"}"#;
    const SUMMARY_GET: &str = "GET /v3/accounts/acct-1/summary HTTP/1.1";
    const ORDER_POST: &str = "POST /v3/accounts/acct-1/orders HTTP/1.1";

    fn reduce_only(kind: &str, order_client_id: &str) -> OrderRequest {
        let mut r = req(kind, 1, 1000.0);
        r.order_client_id = order_client_id.into();
        r.reduce_only = true;
        r
    }

    fn submit_reduce_only(kind: &str, order_client_id: &str) -> ExecutionClientActorCommand {
        ExecutionClientActorCommand::Submit(
            Box::new(reduce_only(kind, order_client_id)),
            std::time::Instant::now(),
        )
    }

    fn lines(served: &Served) -> Vec<&str> {
        served.iter().map(|(line, _)| line.as_str()).collect()
    }

    fn body_of(served: &Served, at: usize) -> serde_json::Value {
        serde_json::from_str(&served[at].1).expect("an order body is JSON")
    }

    /// The account hedges: the summary is read ONCE at the start (the first request the double serves), and
    /// the reduce-only MARKET order goes out saying it may only reduce.
    #[test]
    fn a_reduce_only_market_order_on_a_hedging_account_goes_out_with_position_fill() {
        let (addr, server) =
            spawn_double(vec![(200, HEDGING_SUMMARY), (200, WATERMARK), (201, FILLED)]);
        let (evs, _) = drive_on(
            addr,
            vec![submit_reduce_only("market", "order-client-id-1")],
            submitted_symbols(),
            AccountHedging::default(),
        );
        let served = server.join().expect("double joins");
        assert_eq!(lines(&served), [SUMMARY_GET, SUMMARY_GET, ORDER_POST]);
        let order = &body_of(&served, 2)["order"];
        assert_eq!(order["type"], "MARKET");
        assert_eq!(order["positionFill"], "REDUCE_ONLY");
        assert!(evs.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{evs:?}");
    }

    /// A netting account: the reduce-only order says it may only reduce too, so a close whose position went while it
    /// travelled cannot open the other way (the venue's `DEFAULT` there is `REDUCE_FIRST`).
    #[test]
    fn a_reduce_only_market_order_on_a_netting_account_goes_out_with_position_fill_too() {
        let (addr, server) =
            spawn_double(vec![(200, NETTING_SUMMARY), (200, WATERMARK), (201, FILLED)]);
        drive_on(
            addr,
            vec![submit_reduce_only("market", "order-client-id-1")],
            submitted_symbols(),
            AccountHedging::default(),
        );
        let served = server.join().expect("double joins");
        assert_eq!(lines(&served), [SUMMARY_GET, SUMMARY_GET, ORDER_POST]);
        assert_eq!(
            body_of(&served, 2),
            build_order_body(&reduce_only("market", "order-client-id-1"))
        );
        assert_eq!(body_of(&served, 2)["order"]["positionFill"], "REDUCE_ONLY");
    }

    /// **A fired stop is a flagged market, and it is spent by now.** The answer is unknown (the start read failed and the
    /// retry fails too): the close still goes, and says on the wire that it may only reduce, which is true of either kind
    /// of account. It used to be refused here, which left the position open over a failed read of a setting.
    #[test]
    fn an_unknown_hedging_setting_sends_the_close_saying_it_may_only_reduce() {
        let (addr, server) =
            spawn_double(vec![(500, "{}"), (500, "{}"), (200, WATERMARK), (201, FILLED)]);
        let (evs, _) = drive_on(
            addr,
            vec![submit_reduce_only("market", "order-client-id-1")],
            submitted_symbols(),
            AccountHedging::default(),
        );
        let served = server.join().expect("double joins");
        assert_eq!(lines(&served), [SUMMARY_GET, SUMMARY_GET, SUMMARY_GET, ORDER_POST]);
        assert_eq!(body_of(&served, 3)["order"]["positionFill"], "REDUCE_ONLY");
        assert!(!evs.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{evs:?}");
        assert!(evs.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{evs:?}");
    }

    /// A late close the word carried still counts: the setting comes back as netting, where no word stops the next close
    /// opening the other way, and that one, young as it is, reads the position.
    #[test]
    fn a_late_close_that_carried_the_word_keeps_the_closes_behind_it_reading() {
        let (addr, server) = spawn_double(vec![
            (500, "{}"),
            (500, "{}"),
            (200, WATERMARK),
            (201, FILLED),
            (200, NETTING_SUMMARY),
            (200, r#"{"positions":[]}"#),
        ]);
        let mut young = reduce_only("market", "order-client-id-2");
        (young.trigger_price, young.price) = (None, None);
        let (evs, _) = drive_on(
            addr,
            vec![
                old_close(1000.0),
                ExecutionClientActorCommand::Submit(Box::new(young), std::time::Instant::now()),
            ],
            submitted_symbols(),
            AccountHedging::default(),
        );
        let served = server.join().expect("double joins");
        assert!(served[5].0.contains("/positions"), "{served:?}");
        assert_eq!(lines(&served).iter().filter(|line| **line == ORDER_POST).count(), 1);
        assert_matches!(
            evs.last(),
            Some(Event::OrderRejected(r)) if r.order_client_id == "order-client-id-2" && r.reason.contains("NOT sent")
        );
    }

    /// A failed start read is not for ever: the first reduce-only order asks again, believes the venue's
    /// answer at once, and a second one reads nothing more (the answer is kept for the thread's life).
    #[test]
    fn a_reduce_only_order_asks_again_while_the_setting_is_unknown_and_then_never_does() {
        let (addr, server) = spawn_double(vec![
            (500, "{}"),
            (200, HEDGING_SUMMARY),
            (200, WATERMARK),
            (201, FILLED),
            (201, FILLED),
        ]);
        drive_on(
            addr,
            vec![
                submit_reduce_only("market", "order-client-id-1"),
                submit_reduce_only("market", "order-client-id-2"),
            ],
            submitted_symbols(),
            AccountHedging::default(),
        );
        let served = server.join().expect("double joins");
        assert_eq!(lines(&served), [SUMMARY_GET, SUMMARY_GET, SUMMARY_GET, ORDER_POST, ORDER_POST]);
        for at in [3, 4] {
            assert_eq!(body_of(&served, at)["order"]["positionFill"], "REDUCE_ONLY", "post {at}");
        }
    }

    /// Unknown, and the order RESTS (a protective stop is one): it goes, never refused for a failed read (that would
    /// take away the protection it is for), and says it may only reduce, so it cannot open a position when it fires
    /// after its own is gone.
    #[test]
    fn an_unknown_hedging_setting_sends_a_reduce_only_resting_order_saying_it_may_only_reduce() {
        let (addr, server) =
            spawn_double(vec![(500, "{}"), (500, "{}"), (200, WATERMARK), (201, RESTING)]);
        let (evs, _) = drive_on(
            addr,
            vec![submit_reduce_only("limit", "order-client-id-1")],
            submitted_symbols(),
            AccountHedging::default(),
        );
        let served = server.join().expect("double joins");
        assert_eq!(lines(&served), [SUMMARY_GET, SUMMARY_GET, SUMMARY_GET, ORDER_POST]);
        assert_eq!(body_of(&served, 3)["order"]["positionFill"], "REDUCE_ONLY");
        assert!(evs.iter().any(|e| matches!(e, Event::OrderAccepted(_))), "{evs:?}");
    }

    /// A replace is the whole order again: a reduce-only resting order on a hedging account keeps its word.
    #[test]
    fn the_replacement_of_a_reduce_only_order_on_a_hedging_account_keeps_position_fill() {
        let (addr, server) = spawn_double(vec![(200, HEDGING_SUMMARY), (201, REPLACE_REPLY)]);
        let (evs, _) = drive_on(
            addr,
            vec![ExecutionClientActorCommand::Modify {
                order: Box::new(reduce_only("limit", "order-client-id-1")),
                new_qty: None,
                new_price: Some(1.095),
                new_trigger_price: None,
                queued_at: std::time::Instant::now(),
            }],
            submitted_symbols(),
            AccountHedging::default(),
        );
        let served = server.join().expect("double joins");
        assert_eq!(
            lines(&served),
            [SUMMARY_GET, "PUT /v3/accounts/acct-1/orders/@order-client-id-1 HTTP/1.1"]
        );
        assert_eq!(body_of(&served, 1)["order"]["positionFill"], "REDUCE_ONLY");
        assert_matches!(&evs[0], Event::OrderModified(_));
    }

    /// The same on a netting account: the replacement of a reduce-only stop keeps the word, and a plain one's never
    /// gains it.
    #[test]
    fn the_replacement_of_a_reduce_only_order_on_a_netting_account_keeps_position_fill() {
        let (addr, server) = spawn_double(vec![(201, REPLACE_REPLY), (201, REPLACE_REPLY)]);
        let replace = |order: OrderRequest| ExecutionClientActorCommand::Modify {
            order: Box::new(order),
            new_qty: None,
            new_price: Some(1.07),
            new_trigger_price: None,
            queued_at: std::time::Instant::now(),
        };
        let (evs, _) = drive(
            addr,
            vec![
                replace(reduce_only("stop", "order-client-id-1")),
                replace(req("stop", 1, 1000.0)),
            ],
        );
        let served = server.join().expect("double joins");
        assert_eq!(body_of(&served, 0)["order"]["positionFill"], "REDUCE_ONLY");
        assert!(body_of(&served, 1)["order"].get("positionFill").is_none(), "{served:?}");
        assert_matches!(&evs[0], Event::OrderModified(_));
    }
}

// --- lookup by order client id (the ambiguous-submit resolution) ---------------------------------

use crate::transport::is_order_does_not_exist;

fn unknown(status: u16) -> VenueApiHttpError {
    OANDA_HTTP.error(status, "m")
}

fn inconclusive() -> VenueApiError {
    VenueApiError { code: 0, msg: "down".into() }
}

#[test]
fn only_a_post_write_failure_or_a_gateway_status_is_an_unknown_outcome() {
    assert!(unknown(VenueApiHttpError::STATUS_OUTCOME_UNKNOWN).outcome_unknown());
    for gateway in [502, 503, 504, 524] {
        assert!(unknown(gateway).outcome_unknown(), "{gateway}");
    }
    // Cloudflare's own word for an origin it could not make sense of: a terminal reject here could stand over a live order.
    assert!(unknown(520).outcome_unknown());
    // 0 = provably never sent; 400/401/404/500 are OANDA's own verdict.
    for definite in [0, 400, 401, 404, 429, 500] {
        assert!(!unknown(definite).outcome_unknown(), "{definite}");
    }
}

/// A market order the venue took and cancelled in the same reply (`MARKET_HALTED` on a closed market) read `Accepted`
/// until the stream spoke.
#[test]
fn an_order_cancelled_in_its_own_reply_ends_there() {
    let response = serde_json::json!({
        "orderCreateTransaction": {"id": "6372", "type": "MARKET_ORDER"},
        "orderCancelTransaction": {"id": "6373", "orderID": "6372", "reason": "MARKET_HALTED"},
        "lastTransactionID": "6373"
    });
    let evs = map_order_response("order-client-id-1", 111, &response, &Default::default());
    assert_matches!(
        evs.as_slice(),
        [Event::OrderAccepted(_), Event::OrderCanceled(c)]
            if c.order_client_id == "order-client-id-1" && c.reason == "MARKET_HALTED"
    );
}

/// A fill in the same reply is the order's end, and a cancel of the rest of it is not a second one.
#[test]
fn a_fill_in_the_same_reply_as_a_cancel_ends_the_order_as_filled() {
    let response = serde_json::json!({
        "orderCreateTransaction": {"id": "6372", "type": "MARKET_ORDER"},
        "orderFillTransaction": {"id": "6373", "units": "10", "price": "1.09"},
        "orderCancelTransaction": {"id": "6374", "reason": "MARKET_ORDER_PARTIAL_FILL"},
    });
    let evs = map_order_response("order-client-id-1", 111, &response, &Default::default());
    assert_matches!(
        evs.as_slice(),
        [Event::OrderAccepted(_), Event::Fill(_), Event::OrderFilled(_)]
    );
}

/// **OA-3: only OANDA's documented codes say the order is absent** (`NO_SUCH_ORDER`, the troubleshooting page's;
/// `ORDER_DOESNT_EXIST`, `TransactionRejectReason`'s), decided on the code alone (decision 0301). The spelling this
/// test used to pin, `ORDER_DOES_NOT_EXIST`, is documented nowhere, and a body with no code used to be read by its words.
/// Both bodies are written by hand from the pages: no practice 404 was ever captured.
#[test]
fn only_the_order_missing_404_reads_as_absent() {
    assert!(is_order_does_not_exist(
        r#"{"errorCode":"NO_SUCH_ORDER","errorMessage":"The order ID specified does not exist"}"#
    ));
    assert!(is_order_does_not_exist(
        r#"{"errorCode":"ORDER_DOESNT_EXIST","errorMessage":"The Order specified does not exist"}"#
    ));
    // The undocumented spelling, and words with no code, say nothing: inconclusive, never "absent".
    assert!(!is_order_does_not_exist(
        r#"{"errorCode":"ORDER_DOES_NOT_EXIST","errorMessage":"The Order specified does not exist"}"#
    ));
    assert!(!is_order_does_not_exist(r#"{"errorMessage":"The Order specified does not exist"}"#));
    // A missing ACCOUNT, another code, a non-JSON body (a proxy's HTML) are not "the order is absent".
    assert!(!is_order_does_not_exist(
        r#"{"errorCode":"ACCOUNT_DOES_NOT_EXIST","errorMessage":"The Account specified does not exist"}"#
    ));
    assert!(!is_order_does_not_exist("<html>404</html>"));
}

#[test]
fn the_venue_order_id_is_read_from_the_lookup_reply() {
    assert_eq!(
        found_order_of(&serde_json::json!({"order": {
            "id": "6372", "state": "FILLED", "fillingTransactionID": "6373"}})),
        Some(FoundOrder {
            id: "6372".into(),
            state: "FILLED".into(),
            filling_transaction: Some("6373".into()),
            replaced: false,
        })
    );
    assert_matches!(
        found_order_of(&serde_json::json!({"order": {
            "id": "6372", "state": "CANCELLED", "replacedByOrderID": "6374"}})),
        Some(FoundOrder { replaced: true, .. })
    );
    assert_eq!(found_order_of(&serde_json::json!({"order": {}})), None);
    assert_eq!(found_order_of(&serde_json::json!({})), None);
}

#[test]
fn the_ladder_stops_at_the_first_hit_and_retries_a_miss_or_an_error() {
    let pause = std::time::Duration::from_millis(1);
    let mut slept = 0;
    let mut answers = vec![Ok(Some("9".to_string())), Err(inconclusive()), Ok(None)];
    let got = lookup_with_ladder(&[pause, pause], || answers.pop().unwrap(), |_| slept += 1);
    // popped: None (miss) -> pause -> Err -> pause -> Some: found on the third attempt.
    assert_eq!(got.unwrap(), Some("9".to_string()));
    assert_eq!(slept, 2);

    let mut slept = 0;
    let got = lookup_with_ladder(&[pause, pause], || Ok(Some("1".into())), |_| slept += 1);
    assert_eq!(got.unwrap(), Some("1".to_string()));
    assert_eq!(slept, 0, "a hit on the first attempt waits for nothing");
}

/// The ambiguous submit's schedule, pinned before its ladder moved to the shared ladder (`vike_utility::retry`): three asks
/// with [`LOOKUP_BACKOFF`]'s 1 s, then 2 s, between them, no wait before the first or after the last, and
/// the LAST answer stands. The ladder logs nothing of its own.
#[test]
fn the_submit_lookup_waits_one_then_two_seconds_between_its_three_asks() {
    use std::time::Duration;
    assert_eq!(LOOKUP_BACKOFF, [Duration::from_secs(1), Duration::from_secs(2)]);
    let (mut asks, mut waits) = (0, Vec::new());
    let (got, lines) = vike_log::capture::captured(|| {
        lookup_with_ladder(
            &LOOKUP_BACKOFF,
            || {
                asks += 1;
                if asks < 3 { Ok::<Option<String>, _>(None) } else { Err(inconclusive()) }
            },
            |pause| waits.push(pause),
        )
    });
    assert_eq!(got.unwrap_err().msg, "down", "the third, last, answer stands");
    assert_eq!((asks, waits), (3, LOOKUP_BACKOFF.to_vec()));
    assert!(lines.is_empty(), "{lines:?}");
}

#[test]
fn absence_needs_every_attempt_to_say_absent_and_an_error_is_never_absence() {
    let pause = std::time::Duration::from_millis(1);
    let mut tries = 0;
    let got = lookup_with_ladder(
        &[pause, pause],
        || {
            tries += 1;
            Ok::<Option<String>, VenueApiError>(None)
        },
        |_| {},
    );
    assert_eq!(got.unwrap(), None);
    assert_eq!(tries, 3);

    // The last answer stands: an error on the last attempt is inconclusive, not "absent".
    let mut calls = 0;
    let got = lookup_with_ladder(
        &[pause],
        || {
            calls += 1;
            if calls == 1 { Ok(None) } else { Err(inconclusive()) }
        },
        |_| {},
    );
    assert!(got.is_err());
    // ... and the shared mapping turns that into nothing, never a reject.
    assert_eq!(vike_bridge_core::resolve_ambiguous_submit("c", 5, got), None);
}

// --- what a lookup may say: confirm is never terminal, a fill is delivered, absent needs the transactions ----

fn tx_fill(order_client_id: &str) -> serde_json::Value {
    serde_json::json!({"type": "ORDER_FILL", "id": "105", "orderID": "104", "time": "2.0",
        "instrument": "EUR_USD", "units": "1000", "price": "1.1", "commission": "0.02",
        "clientOrderID": order_client_id})
}

#[test]
fn a_filled_market_order_gone_from_the_pending_list_is_read_from_its_transactions() {
    let txs = [
        serde_json::json!({"type": "MARKET_ORDER", "id": "104", "clientExtensions": {"id": "c1"}}),
        tx_fill("c1"),
    ];
    let evs = events_from_transactions("c1", 9, &txs, &Default::default());
    assert_eq!(evs.len(), 3, "one accept, one dual-published fill");
    assert_matches!(&evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("104"));
    assert_matches!(&evs[1], Event::Fill(f) if f.trade_id == "105" && f.order_client_id == "c1");
    assert_matches!(&evs[2], Event::OrderFilled(_));
}

/// Every fill producer stamps the symbol its ORDER was submitted under (the engine drops a fill whose
/// symbol it neither mounts nor registered the order under, `crate::execution::submitted_symbols`); an order nobody recorded
/// books under the instrument's USER name: the venue's `EUR_USD` is `EURUSD`, ONE name per instrument (decision 0290).
#[test]
fn every_fill_producer_labels_the_fill_with_the_submitted_symbol() {
    crate::test_support::kept_list_for_tests();
    let symbols = submitted_symbols();
    symbols.record("c1", "EURUSD");
    let post = serde_json::json!({"orderFillTransaction": tx_fill("c1")});
    assert_eq!(fill_symbols(&map_order_response("c1", 9, &post, &symbols)), ["EURUSD"]);
    let replace = serde_json::json!({
        "orderCreateTransaction": {"id": "106", "type": "LIMIT_ORDER", "units": "1000"},
        "orderFillTransaction": tx_fill("c1")});
    assert_eq!(fill_symbols(&map_replace_response("c1", 9, &replace, &symbols)), ["EURUSD"]);
    let txs = [tx_fill("c1")];
    assert_eq!(fill_symbols(&events_from_transactions("c1", 9, &txs, &symbols)), ["EURUSD"]);
    assert_eq!(
        fill_symbols(&events_from_transactions("other", 9, &[tx_fill("other")], &symbols)),
        ["EURUSD"]
    );
}

#[test]
fn a_reject_transaction_is_the_terminal_reject_and_a_replace_cancel_is_nothing() {
    let reject = serde_json::json!({"type": "MARKET_ORDER_REJECT", "id": "9",
        "rejectReason": "INSUFFICIENT_MARGIN", "clientExtensions": {"id": "c1"}});
    let evs = events_from_transactions("c1", 9, &[reject], &Default::default());
    assert_matches!(evs.as_slice(), [Event::OrderRejected(r)] if r.reason == "INSUFFICIENT_MARGIN");
    let replaced = serde_json::json!({"type": "ORDER_CANCEL", "id": "9", "orderID": "5",
        "replacedByOrderID": "6", "clientOrderID": "c1"});
    assert!(events_from_transactions("c1", 9, &[replaced], &Default::default()).is_empty());
    let cancel = serde_json::json!({"type": "ORDER_CANCEL", "id": "9", "orderID": "5",
        "reason": "CLIENT_REQUEST", "clientOrderID": "c1"});
    assert_matches!(
        events_from_transactions("c1", 9, &[cancel], &Default::default()).as_slice(),
        [Event::OrderAccepted(_), Event::OrderCanceled(_)]
    );
    assert!(events_from_transactions("c1", 9, &[], &Default::default()).is_empty());
}

/// Only a refusal of the ORDER itself is its terminal rejection: a refused cancel or extension edit
/// of a LIVE order sharing the client id (`ORDER_CANCEL_REJECT`, `*_CLIENT_EXTENSIONS_MODIFY_REJECT`)
/// says nothing about the order.
#[test]
fn only_a_rejection_of_the_order_itself_is_terminal() {
    for kind in [
        "MARKET_ORDER_REJECT",
        "LIMIT_ORDER_REJECT",
        "STOP_ORDER_REJECT",
        "MARKET_IF_TOUCHED_ORDER_REJECT",
        "TAKE_PROFIT_ORDER_REJECT",
        "STOP_LOSS_ORDER_REJECT",
        "TRAILING_STOP_LOSS_ORDER_REJECT",
        "GUARANTEED_STOP_LOSS_ORDER_REJECT",
        "FIXED_PRICE_ORDER_REJECT",
    ] {
        let t = serde_json::json!({"type": kind, "rejectReason": "X", "clientOrderID": "c1"});
        assert_matches!(
            events_from_transactions("c1", 9, &[t], &Default::default()).as_slice(),
            [Event::OrderRejected(_)],
            "{kind}"
        );
    }
    for kind in [
        "ORDER_CANCEL_REJECT",
        "ORDER_CLIENT_EXTENSIONS_MODIFY_REJECT",
        "TRADE_CLIENT_EXTENSIONS_MODIFY_REJECT",
    ] {
        let t = serde_json::json!({"type": kind, "rejectReason": "X", "clientOrderID": "c1"});
        assert!(events_from_transactions("c1", 9, &[t], &Default::default()).is_empty(), "{kind}");
    }
}

#[test]
fn only_a_transaction_of_this_client_id_is_ours() {
    assert!(transaction_is_ours(&tx_fill("c1"), "c1"));
    assert!(transaction_is_ours(
        &serde_json::json!({"type": "MARKET_ORDER", "clientExtensions": {"id": "c1"}}),
        "c1"
    ));
    assert!(!transaction_is_ours(&tx_fill("c2"), "c1"));
    assert!(!transaction_is_ours(&serde_json::json!({"type": "HEARTBEAT"}), "c1"));
}

/// ⚠ The client's `confirm` must reach the confirm the actor was built with, the venue's hooks on top of it
/// included: a no-op here is what `vike_venues::venue_order_client_id_budget::order_client_id_lookup_for` (`looks_up`
/// for oanda) would never notice.
#[test]
fn the_hooked_actor_runs_its_confirm() {
    use std::sync::Mutex;
    let (events, _rx) = vike_execution::event_channel(16);
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let spy: ConfirmFn = {
        let asked = asked.clone();
        Arc::new(move |order_client_id: &str| {
            asked.lock().unwrap().push(order_client_id.to_string());
            Vec::new()
        })
    };
    let actor = ExecutionClientActor::spawn("oanda-confirm-spy", events, |rx| {
        while let Ok(cmd) = rx.recv() {
            if matches!(cmd, ExecutionClientActorCommand::Shutdown) {
                break;
            }
        }
    })
    .with_confirm(spy);
    let mut client = with_symbol_hooks(actor, &submitted_symbols());
    client.confirm("c-spy");
    client.detach(); // joins the confirm worker: deterministic
    assert_eq!(*asked.lock().unwrap(), vec!["c-spy".to_string()]);
}

/// ⚠ The hooks must RECORD the submitted symbol before the actor queues the order, and learn a restored
/// order's from `adopt_restored`: a client without either leaves the table empty and every fill unlabelled.
#[test]
fn the_hooks_record_the_submitted_symbol_and_teach_a_restored_one() {
    let (events, _rx) = vike_execution::event_channel(16);
    let actor = ExecutionClientActor::spawn("oanda-symbol-spy", events, |rx| {
        while let Ok(cmd) = rx.recv() {
            if matches!(cmd, ExecutionClientActorCommand::Shutdown) {
                break;
            }
        }
    });
    let symbols = submitted_symbols();
    let mut client = with_symbol_hooks(actor, &symbols);
    client.submit(&OrderRequest {
        order_client_id: "c-new".into(),
        symbol: "EURUSD".into(),
        ..Default::default()
    });
    client.adopt_restored(&[vike_execution::RestoredOrder::new("c-old", "GBPUSD")]);
    client.detach();
    assert_eq!(symbols.label("c-new", "EUR_USD"), "EURUSD");
    assert_eq!(symbols.label("c-old", "GBP_USD"), "GBPUSD");
}

mod lookup_loopback {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};

    /// Answers each request with the next `(status, body)`; joins to the request lines it served.
    fn double(script: Vec<(u16, String)>) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut served = Vec::new();
            for (status, body) in script {
                let Ok((mut sock, _)) = listener.accept() else { break };
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&buffer).to_string();
                served.push(text.lines().next().unwrap_or_default().to_string());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes());
                let _ = sock.flush();
            }
            served
        });
        (addr, handle)
    }

    fn config(addr: SocketAddr) -> OandaConfig {
        OandaConfig {
            api_token: vike_model::secret::Secret::new("tok".into()),
            account_id: "acct-1".into(),
            rest_base: format!("http://{addr}"),
            stream_base: format!("http://{addr}"),
        }
    }

    fn rest() -> OandaTransport {
        OandaTransport::new("tok".into())
    }

    const MISSING: &str =
        r#"{"errorCode":"NO_SUCH_ORDER","errorMessage":"The order ID specified does not exist"}"#;
    const ORDER_LINE: &str = "GET /v3/accounts/acct-1/orders/@c1 HTTP/1.1";
    const ZERO: std::time::Duration = std::time::Duration::ZERO;
    const BUDGET: std::time::Duration = RESOLVE_BUDGET;

    fn ok(body: serde_json::Value) -> (u16, String) {
        (200, body.to_string())
    }

    /// **The restore's round trip, over the scripted venue.** What OANDA lists for a door limit a previous
    /// process placed is adopted with the venue's terms, the engine's diff holds nothing for it, the engine's
    /// own cancel goes to `@{order_client_id}` over a client that never saw the order, and once the venue no longer lists
    /// it no later pass adopts it. The limit price is the core's to put back
    /// (`crates/vike-core/src/runtime/restore.rs`'s `restore_adopt`).
    #[test]
    fn a_restored_door_limit_is_adopted_holds_nothing_and_the_engines_cancel_reaches_it() {
        use vike_bridge_core::smoke::{adopts, is_gone, plan_pass, restored_limit};
        use vike_execution::recon::diff;
        use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, Ingest};
        use vike_execution::{RiskGate, event_channel};
        use vike_model::{OrderStatus, RiskLimits};

        const ORDER_CLIENT_ID: &str = "b99fxo020408";
        crate::test_support::kept_list_for_tests();
        let resting = json!({ "orders": [{
            "id": "77", "createTime": "1.0", "type": "LIMIT", "instrument": "GBP_USD",
            "units": "-3", "price": "1.29208", "state": "PENDING", "clientExtensions": { "id": ORDER_CLIENT_ID }
        }]});
        let (addr, served) = double(vec![
            ok(json!({ "account": { "currency": "USD", "lastTransactionID": "100" } })),
            ok(resting),
            ok(json!({ "transactions": [], "lastTransactionID": "100" })),
            ok(json!({ "orderCancelTransaction": { "id": "101", "orderID": "77" } })),
            ok(json!({ "orders": [] })),
            ok(json!({ "transactions": [], "lastTransactionID": "101" })),
        ]);
        let config_under_test = config(addr);
        let recon =
            crate::reconcile::ReconcileClientOanda::connect(&config_under_test, "GBPUSD", None)
                .expect("the scripted summary");
        let refs = [restored_limit(ORDER_CLIENT_ID, VENUE, "GBPUSD")];
        let (plan, orders) = plan_pass(&recon, VENUE, "GBPUSD", &refs);
        assert!(adopts(&plan, ORDER_CLIENT_ID), "{plan:?}");

        let (events, mut rx) = event_channel(64);
        let symbols = submitted_symbols();
        let actor = ExecutionClientActor::spawn("oanda-restore-test", events.clone(), {
            let (config_under_test, symbols) = (config_under_test.clone(), symbols.clone());
            move |commands| {
                let fence = Arc::new(SubmitFence::new(events.clone()));
                run(config_under_test, events, commands, Arc::default(), symbols, fence)
            }
        });
        let mut engine = Engine::new(
            Account::new(1.0, VENUE, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            with_symbol_hooks(actor, &symbols),
            VENUE,
            "GBPUSD",
        );

        // What `restore_adopt` does with the plan.
        engine
            .client
            .adopt_restored(&[vike_execution::RestoredOrder::new(ORDER_CLIENT_ID, "GBPUSD")]);
        assert_eq!(engine.reregister_orders(&plan.adopt), 1);
        let adopted = &engine.registry[ORDER_CLIENT_ID];
        assert_eq!(adopted.venue_order_id.as_deref(), Some("77"));
        assert_eq!((adopted.request.symbol.as_str(), adopted.request.side), ("GBPUSD", -1));
        assert_eq!((adopted.request.qty, adopted.filled_qty), (3.0, 0.0));
        assert_eq!(adopted.request.order_type, OrderType::Limit);
        assert_eq!(adopted.status, OrderStatus::Accepted);
        assert_eq!(
            symbols.label(ORDER_CLIENT_ID, "GBP_USD"),
            "GBPUSD",
            "its fill will book under the engine's name"
        );

        let local = engine.local_view();
        let held = diff(&orders, &[], &[], &local.as_view(), None);
        assert!(held.is_empty(), "nothing is held for an adopted order: {held:?}");

        assert!(
            engine.cancel_order(ORDER_CLIENT_ID),
            "the registry holds it, so the engine sends the cancel"
        );
        let mut outbox = EventBusOutbox::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while engine.registry[ORDER_CLIENT_ID].status != OrderStatus::Canceled {
            assert!(std::time::Instant::now() < deadline, "the venue's cancel never folded");
            match rx.try_recv() {
                Ok(Ingest::Event(event)) => {
                    assert!(!matches!(event, Event::OrderCancelRejected(_)), "{event:?}");
                    engine.on_event(&event, &mut outbox);
                }
                Ok(_) => {}
                Err(_) => thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        let (next, _) = plan_pass(&recon, VENUE, "GBPUSD", &refs);
        assert!(is_gone(&next, ORDER_CLIENT_ID) && !adopts(&next, ORDER_CLIENT_ID), "{next:?}");

        engine.client.detach();
        let lines = served.join().expect("the double");
        assert!(
            lines[3].starts_with("PUT /v3/accounts/acct-1/orders/@b99fxo020408/cancel "),
            "{lines:?}"
        );
    }

    /// ⚠ The watchdog's confirm can run while the order's POST is still queued: not found and an
    /// error are BOTH silence there, never the terminal reject.
    #[test]
    fn confirm_never_rules_an_order_absent() {
        for answer in [(404, MISSING.to_string()), (500, "boom".to_string())] {
            let (addr, server) = double(vec![answer]);
            let evs =
                confirm_by_order_client_id(&rest(), &config(addr), "c1", 5, &Default::default());
            assert!(evs.is_empty(), "{evs:?}");
            assert_eq!(server.join().unwrap(), vec![ORDER_LINE.to_string()]);
        }
    }

    #[test]
    fn confirm_of_a_filled_order_delivers_its_fill() {
        let (addr, server) = double(vec![
            ok(serde_json::json!({"order": {
                "id": "104", "state": "FILLED", "fillingTransactionID": "105"}})),
            ok(serde_json::json!({"transaction": tx_fill("c1")})),
        ]);
        let evs = confirm_by_order_client_id(&rest(), &config(addr), "c1", 5, &Default::default());
        assert_eq!(evs.len(), 3);
        assert_matches!(&evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("104"));
        assert_matches!(&evs[1], Event::Fill(f) if f.trade_id == "105");
        assert_matches!(&evs[2], Event::OrderFilled(_));
        assert_eq!(
            server.join().unwrap(),
            vec![
                ORDER_LINE.to_string(),
                "GET /v3/accounts/acct-1/transactions/105 HTTP/1.1".to_string()
            ]
        );
    }

    /// The confirm worker reads the SAME table as the exec thread: the fill it recovers carries the
    /// submitted symbol too.
    #[test]
    fn confirm_of_a_filled_order_labels_its_fill_with_the_submitted_symbol() {
        let (addr, server) = double(vec![
            ok(serde_json::json!({"order": {
                "id": "104", "state": "FILLED", "fillingTransactionID": "105"}})),
            ok(serde_json::json!({"transaction": tx_fill("c1")})),
        ]);
        let symbols = submitted_symbols();
        symbols.record("c1", "EURUSD");
        let evs = confirm_by_order_client_id(&rest(), &config(addr), "c1", 5, &symbols);
        assert_eq!(fill_symbols(&evs), ["EURUSD"]);
        server.join().unwrap();
    }

    #[test]
    fn a_pending_order_is_accepted_without_a_fill_lookup() {
        let (addr, server) =
            double(vec![ok(serde_json::json!({"order": {"id": "104", "state": "PENDING"}}))]);
        let evs = confirm_by_order_client_id(&rest(), &config(addr), "c1", 5, &Default::default());
        assert_matches!(evs.as_slice(), [Event::OrderAccepted(_)]);
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn inline_absent_needs_the_order_missing_and_no_transaction_of_it() {
        let (addr, server) = double(vec![
            (404, MISSING.to_string()),
            (404, MISSING.to_string()),
            ok(serde_json::json!({"transactions": [tx_fill("someone-else")]})),
        ]);
        let evs = resolve_submit(
            &rest(),
            &config(addr),
            100,
            &[ZERO],
            BUDGET,
            "c1",
            5,
            &Default::default(),
        );
        assert_matches!(
            evs.as_slice(),
            [Event::OrderRejected(r)] if r.reason.contains("venue confirms order absent")
        );
        let served = server.join().unwrap();
        assert_eq!(served[2], "GET /v3/accounts/acct-1/transactions/sinceid?id=100 HTTP/1.1");
    }

    /// A 200 with no `transactions` array says nothing, and read as an empty list it ruled the order ABSENT, a terminal
    /// reject over an order the POST may have placed. An empty array is the venue's answer, and stays one.
    #[test]
    fn inline_a_transactions_read_with_no_array_is_inconclusive() {
        let (addr, server) = double(vec![
            (404, MISSING.to_string()),
            ok(serde_json::json!({"lastTransactionID": "100"})),
        ]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert!(evs.is_empty(), "{evs:?}");
        server.join().unwrap();

        let (addr, server) =
            double(vec![(404, MISSING.to_string()), ok(serde_json::json!({"transactions": []}))]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert_matches!(evs.as_slice(), [Event::OrderRejected(_)]);
        server.join().unwrap();
    }

    #[test]
    fn inline_a_filled_market_order_the_pending_list_404s_is_not_rejected() {
        let (addr, server) = double(vec![
            (404, MISSING.to_string()),
            ok(serde_json::json!({"transactions": [
                {"type": "MARKET_ORDER", "id": "104", "clientExtensions": {"id": "c1"}},
                tx_fill("c1")]})),
        ]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert_eq!(evs.len(), 3);
        assert_matches!(&evs[0], Event::OrderAccepted(_));
        assert_matches!(&evs[1], Event::Fill(_));
        assert_matches!(&evs[2], Event::OrderFilled(_));
        server.join().unwrap();
    }

    #[test]
    fn inline_without_a_floor_or_with_a_failed_scan_emits_nothing() {
        // no floor: a fill cannot be ruled out -> NOTHING (the order stays Submitted for the
        // stuck-order watchdog and reconcile; an id-less accept would hide it from both)
        let (addr, server) = double(vec![(404, MISSING.to_string())]);
        let evs =
            resolve_submit(&rest(), &config(addr), 0, &[], BUDGET, "c1", 5, &Default::default());
        assert!(evs.is_empty(), "{evs:?}");
        server.join().unwrap();
        // the transactions read fails
        let (addr, server) = double(vec![(404, MISSING.to_string()), (500, "boom".to_string())]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert!(evs.is_empty(), "{evs:?}");
        server.join().unwrap();
    }

    /// The lookup itself failing (a 5xx, not OANDA's "order does not exist") is inconclusive: NOTHING.
    #[test]
    fn inline_a_failed_order_lookup_emits_nothing() {
        let (addr, server) = double(vec![(500, "boom".to_string())]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert!(evs.is_empty(), "{evs:?}");
        assert_eq!(server.join().unwrap(), vec![ORDER_LINE.to_string()]);
    }

    #[test]
    fn the_watermark_is_read_from_the_account_summary_when_it_is_zero() {
        let (addr, server) = double(vec![ok(serde_json::json!({"lastTransactionID": "777"}))]);
        let last_seen = Arc::new(AtomicU64::new(0));
        ensure_watermark(&rest(), &config(addr), &last_seen, &mut None);
        assert_eq!(last_seen.load(Ordering::Relaxed), 777);
        assert_eq!(
            server.join().unwrap(),
            vec!["GET /v3/accounts/acct-1/summary HTTP/1.1".to_string()]
        );
        // already set: no request at all
        ensure_watermark(&rest(), &config("127.0.0.1:9".parse().unwrap()), &last_seen, &mut None);
        assert_eq!(last_seen.load(Ordering::Relaxed), 777);
    }

    /// A market FOK/IOC the venue CANCELLED whose POST reply was lost: the stream's `ORDER_CANCEL`
    /// met a `Submitted` order and was dropped, so the lookup must deliver accept + cancel itself.
    #[test]
    fn inline_a_cancelled_order_is_accepted_then_canceled_from_its_transactions() {
        let (addr, server) = double(vec![
            ok(serde_json::json!({"order": {"id": "104", "state": "CANCELLED"}})),
            ok(serde_json::json!({"transactions": [
                {"type": "MARKET_ORDER", "id": "104", "clientExtensions": {"id": "c1"}},
                {"type": "ORDER_CANCEL", "id": "105", "orderID": "104",
                 "reason": "INSUFFICIENT_LIQUIDITY", "clientOrderID": "c1"}]})),
        ]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert_matches!(&evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("104"));
        assert_matches!(&evs[1], Event::OrderCanceled(c) if c.reason == "INSUFFICIENT_LIQUIDITY");
        assert_eq!(evs.len(), 2);
        server.join().unwrap();
    }

    /// **A CANCELLED order is read by its state on `confirm` too** (decision 0171: closed is the accept followed by
    /// its terminal), and when the inline scan fails: it used to get the accept alone and read LIVE for ever. The
    /// replace-cancel exception stays: an order a replace cancelled (`replacedByOrderID`) lives on as the replacement
    /// under the same client id, so it is accepted only.
    #[test]
    fn a_cancelled_order_is_accepted_then_canceled_unless_a_replace_cancelled_it() {
        let cancelled = || ok(serde_json::json!({"order": {"id": "104", "state": "CANCELLED"}}));
        let (addr, server) = double(vec![cancelled()]);
        let evs = confirm_by_order_client_id(&rest(), &config(addr), "c1", 5, &Default::default());
        assert_matches!(
            evs.as_slice(),
            [Event::OrderAccepted(a), Event::OrderCanceled(c)]
                if a.venue_order_id.as_deref() == Some("104") && c.order_client_id == "c1"
        );
        assert_eq!(server.join().unwrap().len(), 1, "the state is the answer: no other read");

        let (addr, server) = double(vec![cancelled(), (500, "boom".to_string())]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert_matches!(evs.as_slice(), [Event::OrderAccepted(a), Event::OrderCanceled(_)] if a.venue_order_id.as_deref() == Some("104"));
        server.join().unwrap();

        let replaced = ok(serde_json::json!({"order": {
            "id": "104", "state": "CANCELLED", "replacedByOrderID": "105"}}));
        let (addr, server) = double(vec![replaced]);
        let evs = confirm_by_order_client_id(&rest(), &config(addr), "c1", 5, &Default::default());
        assert_matches!(evs.as_slice(), [Event::OrderAccepted(_)]);
        server.join().unwrap();
    }

    /// A spent budget skips every step: no request leaves, and the answer is NOTHING.
    #[test]
    fn a_spent_budget_asks_the_venue_nothing() {
        let (addr, server) = double(vec![]);
        let evs = resolve_submit(
            &rest(),
            &config(addr),
            100,
            &[ZERO],
            ZERO,
            "c1",
            5,
            &Default::default(),
        );
        assert!(evs.is_empty(), "{evs:?}");
        assert!(server.join().unwrap().is_empty());
    }

    /// A failed `GET /summary` is not repeated on every submit: it waits `WATERMARK_RETRY`.
    #[test]
    fn a_failed_watermark_read_backs_off_instead_of_retrying_every_submit() {
        let last_seen = Arc::new(AtomicU64::new(0));
        let mut retry_at = None;
        let (addr, server) = double(vec![(500, "boom".to_string())]);
        ensure_watermark(&rest(), &config(addr), &last_seen, &mut retry_at);
        assert_eq!(server.join().unwrap().len(), 1);
        assert!(retry_at.is_some(), "a failure arms the backoff");
        // Inside the backoff: no request at all (nothing listens on port 9).
        ensure_watermark(
            &rest(),
            &config("127.0.0.1:9".parse().unwrap()),
            &last_seen,
            &mut retry_at,
        );
        assert_eq!(last_seen.load(Ordering::Relaxed), 0);
        // Past it: asks again, and a success clears the backoff.
        retry_at = Some(std::time::Instant::now());
        let (addr, server) = double(vec![ok(serde_json::json!({"lastTransactionID": "9"}))]);
        ensure_watermark(&rest(), &config(addr), &last_seen, &mut retry_at);
        assert_eq!(server.join().unwrap().len(), 1);
        assert_eq!(last_seen.load(Ordering::Relaxed), 9);
        assert!(retry_at.is_none());
    }
}

// --- the transactions stream wins the race against the order POST's reply ---------------------------

/// A market order is filled by the venue at once, and the fill reaches us TWICE: on the order POST's
/// reply (`Accepted`, then the fill) and on the transactions stream (the fill alone: the stream carries
/// no accept). Nothing orders the two, and the stream is routinely first. The REAL exec loop and the
/// REAL stream lane run against one loopback venue here, the POST's reply held back until the stream has
/// delivered the fill, and the events they publish are folded through a REAL engine.
mod stream_beats_post {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::mpsc::{Sender, channel};
    use std::time::{Duration, Instant};
    use vike_bridge_core::venue_adapter_fixture::ExecutionClientInert;
    use vike_execution::{
        Account, BalanceMode, Engine, EventBusOutbox, EventHandler, Ingest, RiskGate,
    };
    use vike_model::events::PositionSide;
    use vike_model::{OrderStatus, RiskLimits};

    /// The venue as the two connections see it.
    struct Venue {
        addr: SocketAddr,
        /// Lines for the open transactions stream; dropping it ends the stream.
        lines: Sender<String>,
        /// Fires when the order POST has been read and waits for its reply.
        posted: Receiver<()>,
        /// The order POST's reply body, once the test lets it go.
        reply: Sender<String>,
    }

    /// One HTTP request off `sock` (head and `content-length` body) as text.
    fn read_request(sock: &mut TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let (head_end, content_length) = loop {
            let n = sock.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break (buffer.len(), 0);
            }
            buffer.extend_from_slice(&chunk[..n]);
            if let Some(p) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..p]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                break (p + 4, len);
            }
        };
        while buffer.len() < head_end + content_length {
            let n = sock.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&buffer).to_string()
    }

    /// Hold the transactions stream open as one chunked 200 and write every line it is handed.
    fn serve_stream(mut sock: TcpStream, lines: Receiver<String>) {
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\n\
                    transfer-encoding: chunked\r\n\r\n";
        let _ = sock.write_all(head.as_bytes());
        while let Ok(line) = lines.recv() {
            let chunk = format!("{:x}\r\n{line}\n\r\n", line.len() + 1);
            if sock.write_all(chunk.as_bytes()).and_then(|()| sock.flush()).is_err() {
                return;
            }
        }
        let _ = sock.write_all(b"0\r\n\r\n");
    }

    /// Answer the order POST with the body the test sends on `reply`, once it does.
    fn serve_post(mut sock: TcpStream, posted: Sender<()>, reply: Receiver<String>) {
        let _ = posted.send(());
        let Ok(body) = reply.recv() else { return };
        let head = format!(
            "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(head.as_bytes());
        let _ = sock.flush();
    }

    fn spawn_venue() -> Venue {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let (lines, lines_rx) = channel::<String>();
        let (posted_tx, posted) = channel::<()>();
        let (reply, reply_rx) = channel::<String>();
        thread::spawn(move || {
            let (mut lines_rx, mut reply_rx) = (Some(lines_rx), Some(reply_rx));
            loop {
                let Ok((mut sock, _)) = listener.accept() else { return };
                let request = read_request(&mut sock);
                if request.starts_with("GET /v3/accounts/acct-1/summary ")
                    || request.starts_with("GET /v3/accounts/acct-1/transactions/sinceid?id=6400 ")
                {
                    // The stream lane's floor, and the empty gap it reads back behind its open.
                    let body = r#"{"lastTransactionID":"6400","transactions":[]}"#;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes());
                } else if request.starts_with("GET /v3/accounts/acct-1/transactions/stream ") {
                    let lines = lines_rx.take().expect("the stream opens once");
                    thread::spawn(move || serve_stream(sock, lines));
                } else if request.starts_with("POST /v3/accounts/acct-1/orders ") {
                    let reply = reply_rx.take().expect("one order POST");
                    let posted = posted_tx.clone();
                    thread::spawn(move || serve_post(sock, posted, reply));
                } else {
                    panic!("the venue double got an unexpected request: {request}");
                }
            }
        });
        Venue { addr, lines, posted, reply }
    }

    fn label(event: &Event) -> String {
        match event {
            Event::OrderSubmitted(_) => "Submitted".into(),
            Event::OrderAccepted(_) => "Accepted".into(),
            Event::Fill(f) => format!("Fill:{}", f.trade_id),
            Event::OrderFilled(_) => "Filled".into(),
            Event::OrderCanceled(_) => "Canceled".into(),
            other => format!("{other:?}"),
        }
    }

    /// Collect what the lanes publish into `got` until `done` holds.
    fn pump(
        ingest: &mut tokio::sync::mpsc::Receiver<Ingest>,
        got: &mut Vec<Event>,
        what: &str,
        done: impl Fn(&[Event]) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(got) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; got {:?}",
                got.iter().map(label).collect::<Vec<_>>()
            );
            match ingest.try_recv() {
                Ok(Ingest::Event(event)) => got.push(event),
                Ok(other) => panic!("expected Ingest::Event, got {other:?}"),
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    /// Submit a market order of `side` through the real exec loop and fill it at the venue with the
    /// stream's copy of the fill delivered BEFORE the POST's reply. Returns the lifecycle the engine
    /// was handed for the order, in order, and the engine after folding it.
    fn the_stream_fill_comes_first(
        side: i32,
        order_client_id: &str,
    ) -> (Vec<String>, Engine<ExecutionClientInert>) {
        let units = if side >= 0 { "1000" } else { "-1000" };
        let fill = serde_json::json!({"id": "6402", "time": "1478012500.000000000",
            "instrument": "EUR_USD", "units": units, "price": "1.09000", "commission": "0.01"});
        let mut stream_fill = fill.clone();
        stream_fill["type"] = "ORDER_FILL".into();
        stream_fill["orderID"] = "6401".into();
        stream_fill["clientExtensions"] = serde_json::json!({"id": order_client_id});
        let reply = serde_json::json!({
            "orderCreateTransaction": {"id": "6401", "type": "MARKET_ORDER"},
            "orderFillTransaction": fill,
            "lastTransactionID": "6402"
        })
        .to_string();
        let create_echo = serde_json::json!({"type": "MARKET_ORDER", "id": "6401",
            "clientExtensions": {"id": order_client_id}});
        // Another order's cancel: the stream lane handles its lines in order, so this one reaching the
        // engine's channel says the fill line before it was handled (published or held back).
        let marker = serde_json::json!({"type": "ORDER_CANCEL", "id": "6403",
            "time": "1478012500.000000000", "orderID": "6410", "reason": "CLIENT_REQUEST",
            "clientExtensions": {"id": "marker"}});

        let venue = spawn_venue();
        let config = OandaConfig {
            api_token: vike_model::secret::Secret::new("tok".to_string()),
            account_id: "acct-1".to_string(),
            rest_base: format!("http://{}", venue.addr),
            stream_base: format!("http://{}", venue.addr),
        };
        let (events, mut ingest) = vike_execution::event_channel(64);
        let symbols = submitted_symbols();
        // A real watermark, so no `GET /summary` precedes the POST.
        let last_seen = Arc::new(AtomicU64::new(6400));
        let stop = Arc::new(AtomicBool::new(false));
        // ONE fence, shared by the stream and the exec thread, as the production spawn wires it.
        let fence = Arc::new(SubmitFence::new(events.clone()));
        let stream = thread::spawn({
            let (config, events) = (config.clone(), events.clone());
            let (stop, last_seen, symbols) = (stop.clone(), last_seen.clone(), symbols.clone());
            let fence = Arc::clone(&fence);
            move || {
                crate::execution::user_data::stream_transactions(
                    config, events, stop, last_seen, symbols, fence,
                )
            }
        });

        let mut order = req("market", side, 1000.0);
        order.order_client_id = order_client_id.into();
        symbols.record(order_client_id, "EURUSD"); // what `with_symbol_hooks`' submit hook does first
        let mut engine = Engine::new(
            Account::new(1.0, "oanda", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            ExecutionClientInert,
            "oanda",
            "EURUSD",
        );
        engine.submit_order(&order, 1, &mut EventBusOutbox::default());
        let (commands, rx) = channel();
        let exec = thread::spawn(move || run(config, events, &rx, last_seen, symbols, fence));
        commands
            .send(ExecutionClientActorCommand::Submit(Box::new(order), std::time::Instant::now()))
            .expect("the loop is alive");

        venue.posted.recv_timeout(Duration::from_secs(10)).expect("the POST reaches the venue");
        // The venue fills the order: the stream pushes the fill while the POST's reply is still out.
        for line in [create_echo, stream_fill, marker] {
            venue.lines.send(line.to_string()).expect("the stream double is alive");
        }
        let mut got = Vec::new();
        pump(&mut ingest, &mut got, "the stream lane to handle the fill", |g| {
            g.iter().any(|e| e.order_client_id() == Some("marker"))
        });
        venue.reply.send(reply).expect("the POST double is waiting");
        pump(&mut ingest, &mut got, "both copies of the fill", |g| {
            g.iter().filter(|e| matches!(e, Event::OrderFilled(_))).count() >= 2
        });

        commands.send(ExecutionClientActorCommand::Shutdown).expect("the loop is alive");
        exec.join().expect("the exec loop returns on Shutdown");
        stop.store(true, Ordering::Relaxed);
        drop(venue.lines); // ends the stream
        stream.join().expect("the stream lane returns once stopped");

        let mut outbox = EventBusOutbox::default();
        let mut seen = Vec::new();
        for event in got.iter().filter(|e| e.order_client_id() == Some(order_client_id)) {
            seen.push(label(event));
            engine.on_event(event, &mut outbox);
        }
        (seen, engine)
    }

    fn assert_the_lifecycle_is_valid(side: i32, order_client_id: &str) {
        let (seen, engine) = the_stream_fill_comes_first(side, order_client_id);
        // Accepted first, then the fill; the second Fill / Filled pair is the other lane's copy of the
        // same venue transaction, which the engine takes once.
        assert_eq!(seen, ["Submitted", "Accepted", "Fill:6402", "Filled", "Fill:6402", "Filled"]);
        assert_eq!(
            engine.closing_events_dropped_order_still_live, 0,
            "a terminal was dropped on a live order"
        );
        assert_eq!(engine.registry[order_client_id].status, OrderStatus::Filled);
        assert_eq!(engine.position_size(PositionSide::Both), 1000.0 * f64::from(side));
    }

    #[test]
    fn a_market_buys_fill_from_the_stream_reaches_the_engine_behind_its_accept() {
        assert_the_lifecycle_is_valid(1, "n064of01");
    }

    #[test]
    fn a_market_sells_fill_from_the_stream_reaches_the_engine_behind_its_accept() {
        assert_the_lifecycle_is_valid(-1, "n064of02");
    }
}
