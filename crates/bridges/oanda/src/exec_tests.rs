use super::*;
use std::assert_matches;

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: "coid-1".into(),
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
        ts: 111,
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
    assert_eq!(o["clientExtensions"]["id"], "coid-1");
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
fn tif_flows_through() {
    let mut r = req("limit", 1, 500.0);
    r.time_in_force = TimeInForce::Ioc;
    assert_eq!(build_order_body(&r)["order"]["timeInForce"], "IOC");
    // market coerces a non-immediate TIF to FOK
    let mut m = req("market", 1, 500.0);
    m.time_in_force = TimeInForce::Gtc;
    assert_eq!(build_order_body(&m)["order"]["timeInForce"], "FOK");
}

/// Equivalence gate for the `venue_tif` routing: the working-order arm's five recorded wire
/// strings, asserted BOTH through `oanda_tif` and against this venue's row of the cross-venue
/// table (byte-for-byte), plus the local MARKET arm's FOK/IOC-only coercion (a genuine venue
/// constraint kept OUT of the resting-path table — see `vike_venues::venue_tif`'s module doc).
#[test]
fn tif_truth_table_matches_the_venue_tif_row() {
    use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
    // Working-order arm — recorded strings; must equal the "oanda" table row.
    for (tif, want) in [(Gtc, "GTC"), (Ioc, "IOC"), (Fok, "FOK"), (Gtd, "GTD"), (Day, "GFD")] {
        assert_eq!(oanda_tif(tif, false), want, "working {tif:?}");
        assert_eq!(
            vike_venues::venue_tif::venue_tif(VENUE, tif).wire(),
            Some(want),
            "table row {tif:?}"
        );
    }
    // MARKET arm — local by design (OANDA MARKET accepts only FOK/IOC): recorded strings.
    for (tif, want) in [(Gtc, "FOK"), (Ioc, "IOC"), (Fok, "FOK"), (Gtd, "FOK"), (Day, "FOK")] {
        assert_eq!(oanda_tif(tif, true), want, "market {tif:?}");
    }
}

#[test]
fn market_fill_maps_to_accepted_plus_filled() {
    let resp: serde_json::Value = serde_json::from_str(
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
    let evs = map_order_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 3);
    assert_matches!(
        &evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("6372")
    );
    match &evs[1] {
        Event::Fill(fill) => {
            assert_eq!(fill.trade_id, "6373");
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0);
            assert_eq!(fill.last_px, 1.09);
            assert_eq!(fill.commission, 0.04);
            assert_eq!(fill.ts, 1_478_012_400_000);
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

#[test]
fn reject_maps_to_rejected() {
    let resp: serde_json::Value = serde_json::from_str(
        r#"{"orderRejectTransaction": {"id": "6372", "rejectReason": "INSUFFICIENT_MARGIN"}}"#,
    )
    .unwrap();
    let evs = map_order_response("coid-1", 111, &resp);
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
    // THE IDENTITY: the replacement carries the SAME client id, so `@coid` and every later fill's
    // `clientOrderID` still name this order.
    assert_eq!(o["clientExtensions"]["id"], "coid-1");
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
        "clientOrderID": "coid-1", "reason": "CLIENT_REQUEST_REPLACED", "replacedByOrderID": "6402"},
    "orderCreateTransaction": {"id": "6402", "type": "LIMIT_ORDER", "time": "1478012400.5",
        "instrument": "EUR_USD", "units": "-500", "price": "1.09500", "replacesOrderID": "6372",
        "clientExtensions": {"id": "coid-1"}},
    "relatedTransactionIDs": ["6401", "6402"],
    "lastTransactionID": "6402"
}"#;

#[test]
fn replace_reply_is_one_order_modified_with_the_new_venue_id() {
    let resp: serde_json::Value = serde_json::from_str(REPLACE_REPLY).unwrap();
    let evs = map_replace_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 1, "no cancel, no accept: ONE logical order");
    match &evs[0] {
        Event::OrderModified(m) => {
            assert_eq!(m.client_order_id, "coid-1", "the platform identity does not change");
            assert_eq!(m.venue_order_id.as_deref(), Some("6402"), "the venue's NEW order id");
            assert_eq!(m.new_qty, Some(500.0), "units echoed by the venue, unsigned");
            assert_eq!(m.new_price, Some(1.095));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

#[test]
fn replace_reply_with_an_immediate_fill_dual_publishes_after_the_modify() {
    let resp = serde_json::json!({
        "orderCancelTransaction": {"id": "1", "reason": "CLIENT_REQUEST_REPLACED"},
        "orderCreateTransaction": {"id": "2", "units": "500", "price": "1.1"},
        "orderFillTransaction": {"id": "3", "time": "1478012400.000000000",
            "instrument": "EUR_USD", "units": "500", "price": "1.1", "commission": "0.01"}
    });
    let evs = map_replace_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 3);
    assert_matches!(&evs[0], Event::OrderModified(_));
    assert_matches!(&evs[1], Event::Fill(f) if f.client_order_id == "coid-1" && f.trade_id == "3");
    assert_matches!(&evs[2], Event::OrderFilled(w) if w.fill.trade_id == "3");
}

#[test]
fn a_replacement_cancelled_at_once_ends_the_order() {
    let resp = serde_json::json!({
        "orderCancelTransaction": {"id": "1", "reason": "CLIENT_REQUEST_REPLACED"},
        "orderCreateTransaction": {"id": "2", "units": "500", "price": "1.1"},
        "replacingOrderCancelTransaction": {"id": "3", "reason": "TIME_IN_FORCE_EXPIRED"}
    });
    let evs = map_replace_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 2);
    assert_matches!(&evs[0], Event::OrderModified(_));
    assert_matches!(
        &evs[1], Event::OrderCanceled(c) if c.client_order_id == "coid-1"
            && c.reason == "TIME_IN_FORCE_EXPIRED"
    );
}

#[test]
fn a_replace_reply_that_created_nothing_is_a_rejection_never_assumed_success() {
    for resp in [
        serde_json::json!({}),
        serde_json::json!({"orderCancelTransaction": {"id": "1"}}),
        serde_json::json!({"orderRejectTransaction": {"rejectReason": "PRICE_PRECISION_EXCEEDED"}}),
    ] {
        let evs = map_replace_response("coid-1", 111, &resp);
        assert_eq!(evs.len(), 1, "{resp}");
        assert_matches!(&evs[0], Event::OrderModifyRejected(r) if r.client_order_id == "coid-1");
    }
    let rej = map_replace_response(
        "coid-1",
        1,
        &serde_json::json!({"orderRejectTransaction": {"rejectReason": "PRICE_PRECISION_EXCEEDED"}}),
    );
    assert_matches!(
        &rej[0], Event::OrderModifyRejected(r) if r.reason == "PRICE_PRECISION_EXCEEDED"
    );
}

// --- the REAL run loop against a loopback OANDA double ---------------------------------------

mod loopback {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::mpsc::channel;

    type Served = Vec<(String, String)>;

    /// A loopback HTTP server answering each request with the next `(status, body)` of `script`.
    /// Its join value is every request it served as `(request line, body)`.
    fn spawn_double(script: Vec<(u16, &'static str)>) -> (SocketAddr, thread::JoinHandle<Served>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let handle = thread::spawn(move || {
            let mut served = Vec::new();
            for (status, body) in script {
                let Ok((mut sock, _)) = listener.accept() else { break };
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 1024];
                let (head_end, content_length) = loop {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break (buf.len(), 0);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (p + 4, len);
                    }
                };
                while buf.len() < head_end + content_length {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let request_line = text.lines().next().unwrap_or_default().to_string();
                served.push((request_line, text[head_end.min(text.len())..].to_string()));
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

    /// Drive the REAL `run` loop with `cmds` against `addr`; return every event it emitted and the
    /// final A3 watermark.
    fn drive(addr: SocketAddr, cmds: Vec<ExecCommand>) -> (Vec<Event>, u64) {
        let config = OandaConfig {
            api_token: "tok".to_string(),
            account_id: "acct-1".to_string(),
            rest_base: format!("http://{addr}"),
            stream_base: format!("http://{addr}"),
        };
        let (events, mut ingest) = vike_exec::event_channel(64);
        let (tx, rx) = channel();
        let last_seen = Arc::new(AtomicU64::new(0));
        let seen = last_seen.clone();
        let lane = thread::spawn(move || run(config, events, rx, seen));
        for c in cmds {
            tx.send(c).expect("the loop is alive");
        }
        tx.send(ExecCommand::Shutdown).expect("the loop is alive");
        lane.join().expect("the loop returns on Shutdown");
        let mut out = Vec::new();
        while let Some(i) = ingest.blocking_recv() {
            match i {
                vike_exec::Ingest::Event(e) => out.push(e),
                other => panic!("expected Ingest::Event, got {other:?}"),
            }
        }
        (out, last_seen.load(Ordering::Relaxed))
    }

    fn modify(price: Option<f64>, qty: Option<f64>) -> ExecCommand {
        ExecCommand::Modify {
            order: Box::new(req("limit", 1, 500.0)),
            new_qty: qty,
            new_price: price,
        }
    }

    #[test]
    fn a_modify_puts_the_replacing_order_at_the_coid_and_emits_order_modified() {
        let (addr, server) = spawn_double(vec![(201, REPLACE_REPLY)]);
        let (evs, last_seen) = drive(addr, vec![modify(Some(1.095), None)]);

        let served = server.join().expect("double joins");
        assert_eq!(served.len(), 1);
        assert_eq!(served[0].0, "PUT /v3/accounts/acct-1/orders/@coid-1 HTTP/1.1");
        let body: serde_json::Value = serde_json::from_str(&served[0].1).expect("a JSON body");
        assert_eq!(body["order"]["type"], "LIMIT");
        assert_eq!(body["order"]["price"], "1.09500");
        assert_eq!(body["order"]["units"], "500");
        assert_eq!(body["order"]["clientExtensions"]["id"], "coid-1");

        assert_eq!(evs.len(), 1);
        assert_matches!(
            &evs[0], Event::OrderModified(m) if m.client_order_id == "coid-1"
                && m.venue_order_id.as_deref() == Some("6402")
        );
        assert_eq!(last_seen, 6402, "the A3 watermark advances from the replace reply too");
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
        assert_matches!(
            &evs[0], Event::OrderModifyRejected(r) if r.client_order_id == "coid-1"
                && r.reason.contains("Invalid value specified for 'price'")
        );
        assert_matches!(
            &evs[1], Event::OrderModifyRejected(r) if r.reason.contains("does not exist")
        );
    }

    #[test]
    fn a_modify_that_cannot_be_expressed_never_touches_the_wire() {
        // port 9 (discard): if anything dialled it, the reason would be a network error instead
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let mut market = req("market", 1, 100.0);
        market.client_order_id = "m1".into();
        let (evs, _) = drive(
            addr,
            vec![ExecCommand::Modify {
                order: Box::new(market),
                new_qty: None,
                new_price: Some(1.0),
            }],
        );
        assert_eq!(evs.len(), 1);
        assert_matches!(
            &evs[0], Event::OrderModifyRejected(r) if r.client_order_id == "m1"
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
        );
    }
}
