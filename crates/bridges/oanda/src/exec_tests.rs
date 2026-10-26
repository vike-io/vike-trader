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
    let evs = map_order_response("coid-1", 111, &resp, &Default::default());
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
    let evs = map_order_response("coid-1", 111, &resp, &Default::default());
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
    let evs = map_replace_response("coid-1", 111, &resp, &Default::default());
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
    let evs = map_replace_response("coid-1", 111, &resp, &Default::default());
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
    let evs = map_replace_response("coid-1", 111, &resp, &Default::default());
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
        let evs = map_replace_response("coid-1", 111, &resp, &Default::default());
        assert_eq!(evs.len(), 1, "{resp}");
        assert_matches!(&evs[0], Event::OrderModifyRejected(r) if r.client_order_id == "coid-1");
    }
    let rej = map_replace_response(
        "coid-1",
        1,
        &serde_json::json!({"orderRejectTransaction": {"rejectReason": "PRICE_PRECISION_EXCEEDED"}}),
        &Default::default(),
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
        drive_with(addr, cmds, SubmittedSymbols::default())
    }

    /// [`drive`] with the submitted-symbol table the loop reads, so a test can record one.
    fn drive_with(
        addr: SocketAddr,
        cmds: Vec<ExecCommand>,
        symbols: SubmittedSymbols,
    ) -> (Vec<Event>, u64) {
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
        let lane = thread::spawn(move || run(config, events, rx, seen, symbols));
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
        let symbols = SubmittedSymbols::default();
        symbols.record("coid-1", "EURUSD");
        let (evs, _) = drive_with(
            addr,
            vec![ExecCommand::Submit(Box::new(req("market", 1, 100.0)))],
            symbols.clone(),
        );
        server.join().expect("double joins");
        assert_eq!(fill_symbols(&evs), ["EURUSD"], "{evs:?}");
        assert_eq!(symbols.label("coid-1", "EUR_USD"), "EUR_USD", "a filled order's entry left");
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

// --- lookup by client order id (the ambiguous-submit resolution) ---------------------------------

use crate::rest::{OandaApiError, STATUS_OUTCOME_UNKNOWN, is_order_does_not_exist};

fn unknown(status: u16) -> OandaApiError {
    OandaApiError { status, message: "m".into() }
}

fn inconclusive() -> VenueApiError {
    VenueApiError { code: 0, msg: "down".into() }
}

#[test]
fn only_a_post_write_failure_or_a_gateway_status_is_an_unknown_outcome() {
    assert!(unknown(STATUS_OUTCOME_UNKNOWN).outcome_unknown());
    for gateway in [502, 503, 504, 524] {
        assert!(unknown(gateway).outcome_unknown(), "{gateway}");
    }
    // 0 = provably never sent; 400/401/404/500 are OANDA's own verdict.
    for definite in [0, 400, 401, 404, 429, 500] {
        assert!(!unknown(definite).outcome_unknown(), "{definite}");
    }
}

#[test]
fn only_the_order_missing_404_reads_as_absent() {
    assert!(is_order_does_not_exist(
        r#"{"errorCode":"ORDER_DOES_NOT_EXIST","errorMessage":"The Order specified does not exist"}"#
    ));
    // errorCode is optional in v20 error bodies: the message alone says the ORDER is missing.
    assert!(is_order_does_not_exist(r#"{"errorMessage":"The Order specified does not exist"}"#));
    // A missing ACCOUNT, another code, a non-JSON body (a proxy's HTML) are not "the order is absent".
    assert!(!is_order_does_not_exist(
        r#"{"errorCode":"ACCOUNT_DOES_NOT_EXIST","errorMessage":"The Account specified does not exist"}"#
    ));
    assert!(!is_order_does_not_exist(r#"{"errorMessage":"The Account specified does not exist"}"#));
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
            filling_txn: Some("6373".into())
        })
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
    // ... and the shared mapping turns that into an optimistic accept, never a reject.
    let ev = vike_bridge_core::resolve_ambiguous_submit("c", 5, got);
    assert_matches!(ev, Event::OrderAccepted(OrderAccepted { venue_order_id: None, .. }));
}

// --- what a lookup may say: confirm is never terminal, a fill is delivered, absent needs the transactions ----

fn tx_fill(coid: &str) -> serde_json::Value {
    serde_json::json!({"type": "ORDER_FILL", "id": "105", "orderID": "104", "time": "2.0",
        "instrument": "EUR_USD", "units": "1000", "price": "1.1", "commission": "0.02",
        "clientOrderID": coid})
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
    assert_matches!(&evs[1], Event::Fill(f) if f.trade_id == "105" && f.client_order_id == "c1");
    assert_matches!(&evs[2], Event::OrderFilled(_));
}

/// The symbols of the bare `Fill`s in `evs`.
fn fill_symbols(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.symbol.to_string()),
            _ => None,
        })
        .collect()
}

/// Every fill producer stamps the symbol its ORDER was submitted under (the engine drops a fill whose
/// symbol it neither mounts nor registered the order under, `crate::symbols`); an order nobody recorded
/// keeps the venue's `EUR_USD`.
#[test]
fn every_fill_producer_labels_the_fill_with_the_submitted_symbol() {
    let symbols = SubmittedSymbols::default();
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
        ["EUR_USD"]
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

/// ⚠ The wrapper must DELEGATE `confirm`: a newtype that omits it inherits the trait's no-op, which
/// `bridge_trade::COID_LOOKUP` (`true` for oanda) would never notice.
#[test]
fn the_wrapper_delegates_confirm_to_the_actor() {
    use std::sync::Mutex;
    let (events, _rx) = vike_exec::event_channel(16);
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let spy: ConfirmFn = {
        let asked = asked.clone();
        Arc::new(move |coid: &str| {
            asked.lock().unwrap().push(coid.to_string());
            Vec::new()
        })
    };
    let actor = ExecActor::spawn("oanda-confirm-spy", events, |rx| {
        while let Ok(cmd) = rx.recv() {
            if matches!(cmd, ExecCommand::Shutdown) {
                break;
            }
        }
    })
    .with_confirm(spy);
    let mut client = OandaExecutionClient::from_parts(actor, Default::default());
    client.confirm("c-spy");
    client.detach(); // joins the confirm worker: deterministic
    assert_eq!(*asked.lock().unwrap(), vec!["c-spy".to_string()]);
}

/// ⚠ The wrapper must RECORD the submitted symbol before it queues the order, and learn a restored order's
/// from `adopt_restored`: a wrapper that omits either leaves the table empty and every fill unlabelled.
#[test]
fn the_wrapper_records_the_submitted_symbol_and_teaches_a_restored_one() {
    let (events, _rx) = vike_exec::event_channel(16);
    let actor = ExecActor::spawn("oanda-symbol-spy", events, |rx| {
        while let Ok(cmd) = rx.recv() {
            if matches!(cmd, ExecCommand::Shutdown) {
                break;
            }
        }
    });
    let symbols = SubmittedSymbols::default();
    let mut client = OandaExecutionClient::from_parts(actor, symbols.clone());
    client.submit(&OrderRequest {
        client_order_id: "c-new".into(),
        symbol: "EURUSD".into(),
        ..Default::default()
    });
    client.adopt_restored(&[("c-old".to_string(), "GBPUSD".to_string())]);
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
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&buf).to_string();
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
            api_token: "tok".into(),
            account_id: "acct-1".into(),
            rest_base: format!("http://{addr}"),
            stream_base: format!("http://{addr}"),
        }
    }

    fn rest() -> OandaRest {
        OandaRest::new("tok".into())
    }

    const MISSING: &str = r#"{"errorCode":"ORDER_DOES_NOT_EXIST","errorMessage":"The Order specified does not exist"}"#;
    const ORDER_LINE: &str = "GET /v3/accounts/acct-1/orders/@c1 HTTP/1.1";
    const ZERO: std::time::Duration = std::time::Duration::ZERO;
    const BUDGET: std::time::Duration = RESOLVE_BUDGET;

    fn ok(body: serde_json::Value) -> (u16, String) {
        (200, body.to_string())
    }

    /// ⚠ The watchdog's confirm can run while the order's POST is still queued: not found and an
    /// error are BOTH silence there, never the terminal reject.
    #[test]
    fn confirm_never_rules_an_order_absent() {
        for answer in [(404, MISSING.to_string()), (500, "boom".to_string())] {
            let (addr, server) = double(vec![answer]);
            let evs = confirm_by_coid(&rest(), &config(addr), "c1", 5, &Default::default());
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
        let evs = confirm_by_coid(&rest(), &config(addr), "c1", 5, &Default::default());
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
        let symbols = SubmittedSymbols::default();
        symbols.record("c1", "EURUSD");
        let evs = confirm_by_coid(&rest(), &config(addr), "c1", 5, &symbols);
        assert_eq!(fill_symbols(&evs), ["EURUSD"]);
        server.join().unwrap();
    }

    #[test]
    fn a_pending_order_is_accepted_without_a_fill_lookup() {
        let (addr, server) =
            double(vec![ok(serde_json::json!({"order": {"id": "104", "state": "PENDING"}}))]);
        let evs = confirm_by_coid(&rest(), &config(addr), "c1", 5, &Default::default());
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

    /// The replace-cancel exception stays: `confirm` (and an unreadable scan) never reads a
    /// CANCELLED order as cancelled, because a replaced order is CANCELLED too.
    #[test]
    fn a_cancelled_order_is_only_accepted_on_confirm_and_when_the_scan_fails() {
        let cancelled = || ok(serde_json::json!({"order": {"id": "104", "state": "CANCELLED"}}));
        let (addr, server) = double(vec![cancelled()]);
        let evs = confirm_by_coid(&rest(), &config(addr), "c1", 5, &Default::default());
        assert_matches!(evs.as_slice(), [Event::OrderAccepted(_)]);
        assert_eq!(server.join().unwrap().len(), 1);

        let (addr, server) = double(vec![cancelled(), (500, "boom".to_string())]);
        let evs =
            resolve_submit(&rest(), &config(addr), 100, &[], BUDGET, "c1", 5, &Default::default());
        assert_matches!(evs.as_slice(), [Event::OrderAccepted(a)] if a.venue_order_id.as_deref() == Some("104"));
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
    use vike_exec::{
        Account, BalanceMode, EventHandler, ExecutionEngine, Ingest, Outbox, RiskGate,
    };
    use vike_model::events::PositionSide;
    use vike_model::{OrderStatus, RiskLimits};

    /// An engine client that sends nothing: the events are fed to the engine by hand.
    struct NoClient;

    impl ExecutionClient for NoClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, _client_order_id: &str) {}
    }

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
        String::from_utf8_lossy(&buf).to_string()
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
            for _ in 0..2 {
                let Ok((mut sock, _)) = listener.accept() else { return };
                let request = read_request(&mut sock);
                if request.starts_with("GET /v3/accounts/acct-1/transactions/stream ") {
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

    fn coid_of(ev: &Event) -> &str {
        match ev {
            Event::OrderSubmitted(e) => &e.client_order_id,
            Event::OrderAccepted(e) => &e.client_order_id,
            Event::Fill(e) => &e.client_order_id,
            Event::OrderFilled(e) => &e.client_order_id,
            Event::OrderCanceled(e) => &e.client_order_id,
            _ => "",
        }
    }

    fn label(ev: &Event) -> String {
        match ev {
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
                Ok(Ingest::Event(ev)) => got.push(ev),
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
        coid: &str,
    ) -> (Vec<String>, ExecutionEngine<NoClient>) {
        let units = if side >= 0 { "1000" } else { "-1000" };
        let fill = serde_json::json!({"id": "6402", "time": "1478012500.000000000",
            "instrument": "EUR_USD", "units": units, "price": "1.09000", "commission": "0.01"});
        let mut stream_fill = fill.clone();
        stream_fill["type"] = "ORDER_FILL".into();
        stream_fill["orderID"] = "6401".into();
        stream_fill["clientExtensions"] = serde_json::json!({"id": coid});
        let reply = serde_json::json!({
            "orderCreateTransaction": {"id": "6401", "type": "MARKET_ORDER"},
            "orderFillTransaction": fill,
            "lastTransactionID": "6402"
        })
        .to_string();
        let create_echo = serde_json::json!({"type": "MARKET_ORDER", "id": "6401",
            "clientExtensions": {"id": coid}});
        // Another order's cancel: the stream lane handles its lines in order, so this one reaching the
        // engine's channel says the fill line before it was handled (published or held back).
        let marker = serde_json::json!({"type": "ORDER_CANCEL", "id": "6403",
            "time": "1478012500.000000000", "orderID": "6410", "reason": "CLIENT_REQUEST",
            "clientExtensions": {"id": "marker"}});

        let venue = spawn_venue();
        let config = OandaConfig {
            api_token: "tok".to_string(),
            account_id: "acct-1".to_string(),
            rest_base: format!("http://{}", venue.addr),
            stream_base: format!("http://{}", venue.addr),
        };
        let (events, mut ingest) = vike_exec::event_channel(64);
        let symbols = SubmittedSymbols::default();
        // A real watermark, so no `GET /summary` precedes the POST.
        let last_seen = Arc::new(AtomicU64::new(6400));
        let stop = Arc::new(AtomicBool::new(false));
        let stream = thread::spawn({
            let (config, events) = (config.clone(), events.clone());
            let (stop, last_seen, symbols) = (stop.clone(), last_seen.clone(), symbols.clone());
            move || crate::stream::stream_transactions(config, events, stop, last_seen, symbols)
        });

        let mut order = req("market", side, 1000.0);
        order.client_order_id = coid.into();
        symbols.record(coid, "EURUSD"); // what the wrapper's `submit` does before it queues
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, "oanda", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            NoClient,
            "oanda",
            "EURUSD",
        );
        engine.submit_order(&order, 1, &mut Outbox::default());
        let (cmds, rx) = channel();
        let exec = thread::spawn(move || run(config, events, rx, last_seen, symbols));
        cmds.send(ExecCommand::Submit(Box::new(order))).expect("the loop is alive");

        venue.posted.recv_timeout(Duration::from_secs(10)).expect("the POST reaches the venue");
        // The venue fills the order: the stream pushes the fill while the POST's reply is still out.
        for line in [create_echo, stream_fill, marker] {
            venue.lines.send(line.to_string()).expect("the stream double is alive");
        }
        let mut got = Vec::new();
        pump(&mut ingest, &mut got, "the stream lane to handle the fill", |g| {
            g.iter().any(|e| coid_of(e) == "marker")
        });
        venue.reply.send(reply).expect("the POST double is waiting");
        pump(&mut ingest, &mut got, "both copies of the fill", |g| {
            g.iter().filter(|e| matches!(e, Event::OrderFilled(_))).count() >= 2
        });

        cmds.send(ExecCommand::Shutdown).expect("the loop is alive");
        exec.join().expect("the exec loop returns on Shutdown");
        stop.store(true, Ordering::Relaxed);
        drop(venue.lines); // ends the stream
        stream.join().expect("the stream lane returns once stopped");

        let mut outbox = Outbox::default();
        let mut seen = Vec::new();
        for ev in got.iter().filter(|e| coid_of(e) == coid) {
            seen.push(label(ev));
            engine.on_event(ev, &mut outbox);
        }
        (seen, engine)
    }

    fn assert_the_lifecycle_is_valid(side: i32, coid: &str) {
        let (seen, engine) = the_stream_fill_comes_first(side, coid);
        // Accepted first, then the fill; the second Fill / Filled pair is the other lane's copy of the
        // same venue transaction, which the engine takes once.
        assert_eq!(seen, ["Submitted", "Accepted", "Fill:6402", "Filled", "Fill:6402", "Filled"]);
        assert_eq!(engine.dropped_terminal_on_live, 0, "a terminal was dropped on a live order");
        assert_eq!(engine.registry[coid].status, OrderStatus::Filled);
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
