//! The exec loop's own door, below the client that stamps an order: how long it waited is said here
//! without waiting that long.
use super::*;
use std::time::{Duration, Instant};
use vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::AccountTier;

/// **The order POST's answer says what the failure was** (decision 0303): Alpaca's 4xx is its refusal, one terminal
/// reject `order.venue_refused` in its words; a failure that never left is `order.not_sent`; one that may have reached
/// Alpaca (a gateway status) ends nothing; a 2xx naming the order is its accept.
#[test]
fn the_order_posts_answer_names_what_its_failure_was() {
    let refusal_of = |events: Vec<Event>| match events.as_slice() {
        [Event::OrderRejected(r)] => r.refusal_id.as_deref().map(str::to_string),
        other => panic!("expected one OrderRejected, got {other:?}"),
    };
    let refused = event_mapper::map_order_post_reply(
        "c1",
        7,
        422,
        r#"{"code":40310000,"message":"insufficient buying power"}"#,
    );
    assert!(
        matches!(&refused[..], [Event::OrderRejected(r)] if r.reason.contains("insufficient buying power"))
    );
    assert_eq!(
        refusal_of(refused).as_deref(),
        Some(vike_model::OrderRefusalId::VenueRefused.word())
    );
    let never_sent = Err(VenueApiHttpError {
        status: 0,
        message: "connection refused".into(),
        spec: &crate::transport::ALPACA_HTTP,
    });
    assert_eq!(
        refusal_of(map_submit_answer("c2", 7, &never_sent)).as_deref(),
        Some(vike_model::OrderRefusalId::NotSent.word())
    );
    assert!(
        event_mapper::map_order_post_reply("c3", 7, 504, "<html>").is_empty(),
        "may have landed"
    );
    let accepted = event_mapper::map_order_post_reply("c4", 7, 200, r#"{"id":"v-1"}"#);
    assert!(
        matches!(&accepted[..], [Event::OrderAccepted(a)] if a.venue_order_id.as_deref() == Some("v-1"))
    );
}

/// **A modify under HALT is answered**, a non-terminal refusal in the shared words, where the client's
/// inherited no-op said nothing. Any existing file stands for the sentinel.
#[test]
fn a_modify_under_halt_is_refused_in_words() {
    let (events, mut ingest) = event_channel(8);
    let halt = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let actor = ExecutionClientActor::spawn("alpaca-halt-test", events.clone(), |_rx| {})
        .with_halt_path(halt);
    let mut client = with_submitted_symbols(actor, submitted_symbols());
    let order = OrderRequest { order_client_id: "resting".into(), ..Default::default() };
    client.modify(&order, Some(2.0), None);
    client.detach();
    match ingest.try_recv() {
        Ok(Ingest::Event(Event::OrderModifyRejected(r))) => {
            assert_eq!(r.order_client_id, "resting");
            assert_eq!(r.reason, vike_execution::halt::HALT_REJECT_REASON);
        }
        other => panic!("expected the HALT refusal, got {other:?}"),
    }
}

/// **An order that waited past its time on this thread's queue is refused by name, costs no call, and
/// leaves the submitted-symbol table.** Port 9: the POST would have answered with a network error.
#[test]
fn an_order_past_its_wait_is_refused_and_never_reaches_the_wire() {
    crate::names::hold_test_rows();
    let dead = "http://127.0.0.1:9";
    let hosts =
        crate::AlpacaUrls { authx: dead.into(), broker: dead.into(), data_websocket: dead.into() };
    let config = AlpacaConfig {
        client_id: vike_model::secret::Secret::new("id".into()),
        client_secret: vike_model::secret::Secret::new("sec".into()),
        account_id: "acct".into(),
        env: AccountTier::Demo,
        hosts,
    };
    let token = Arc::new(TokenSource::new("id".into(), "sec".into(), dead.into()));
    let (events, mut ingest) = event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel();
    let order =
        OrderRequest { order_client_id: "old".into(), symbol: "AAPL".into(), ..Default::default() };
    let symbols = submitted_symbols();
    symbols.record("old", "AAPL");
    let stale = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    tx.send(ExecutionClientActorCommand::Submit(Box::new(order), stale)).unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run(config, token, events, &rx, symbols.clone());

    let mut said = Vec::new();
    while let Ok(Ingest::Event(e)) = ingest.try_recv() {
        said.push(e);
    }
    match said.as_slice() {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
            assert!(r.reason.contains("NOT sent"), "{}", r.reason);
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        }
        other => panic!("expected [Submitted, Rejected], got {other:?}"),
    }
    assert_eq!(symbols.label("old", "AAPLX"), "AAPLX", "the refused order's entry left");
}

/// **A mint in front of the POST counts against the queue's wait**: an order picked up inside its wait whose token mint
/// then carries it past the wait is refused by name before the POST, so the chain behind the POST is only the POST and
/// one re-mint (`SUBMIT`, the table's `ALPACA` row). The fake mint takes 3 s against an order 1 s short of its wait.
#[test]
fn a_mint_that_outlives_the_orders_wait_refuses_it_before_the_post() {
    crate::names::hold_test_rows();
    let dead = "http://127.0.0.1:9";
    let hosts =
        crate::AlpacaUrls { authx: dead.into(), broker: dead.into(), data_websocket: dead.into() };
    let config = AlpacaConfig {
        client_id: vike_model::secret::Secret::new("id".into()),
        client_secret: vike_model::secret::Secret::new("sec".into()),
        account_id: "acct".into(),
        env: AccountTier::Demo,
        hosts,
    };
    let slow_mint = TokenSource::with_clock_and_exchanger(
        Box::new(|| 1_000),
        Box::new(|_| {
            std::thread::sleep(Duration::from_secs(3));
            Ok(("token".to_string(), 900))
        }),
    );
    let (events, mut ingest) = event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel();
    let order = OrderRequest {
        order_client_id: "late".into(),
        symbol: "AAPL".into(),
        ..Default::default()
    };
    let nearly = Instant::now() - MAX_SUBMIT_WAIT + Duration::from_secs(1);
    tx.send(ExecutionClientActorCommand::Submit(Box::new(order), nearly)).unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run(config, Arc::new(slow_mint), events, &rx, submitted_symbols());

    let mut said = Vec::new();
    while let Ok(Ingest::Event(e)) = ingest.try_recv() {
        said.push(e);
    }
    match said.as_slice() {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
            assert!(
                r.reason.contains("NOT sent"),
                "refused by its age, not by the POST: {}",
                r.reason
            );
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        }
        other => panic!("expected [Submitted, Rejected], got {other:?}"),
    }
}

/// **A close past its wait is not refused for its age.** This venue ignores the flag, so it asks the position first;
/// nothing answers on port 9, a failed read is no evidence, and the POST is tried: the dead port leaves it UNKNOWN or
/// rejected in the transport's words, never "NOT sent".
#[test]
fn a_close_past_its_wait_goes_on_to_the_wire() {
    crate::names::hold_test_rows();
    let dead = "http://127.0.0.1:9";
    let hosts =
        crate::AlpacaUrls { authx: dead.into(), broker: dead.into(), data_websocket: dead.into() };
    let config = AlpacaConfig {
        client_id: vike_model::secret::Secret::new("id".into()),
        client_secret: vike_model::secret::Secret::new("sec".into()),
        account_id: "acct".into(),
        env: AccountTier::Demo,
        hosts,
    };
    let token = Arc::new(TokenSource::new("id".into(), "sec".into(), dead.into()));
    let (events, mut ingest) = event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel();
    let order = OrderRequest {
        order_client_id: "leg".into(),
        symbol: "AAPL".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        ..Default::default()
    };
    let stale = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    tx.send(ExecutionClientActorCommand::Submit(Box::new(order), stale)).unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run(config, token, events, &rx, submitted_symbols());

    while let Ok(Ingest::Event(e)) = ingest.try_recv() {
        if let Event::OrderRejected(r) = e {
            // The POST's failure: its mint on port 9 is unreachable (`AlpacaTransport::auth_err`).
            assert!(
                r.reason.starts_with("alpaca HTTP 0: alpaca token mint unreachable: "),
                "{}",
                r.reason
            );
        }
    }
}

/// **A USER name with no kept row is rejected in words naming the fix, and nothing is sent** (decision 0290): the
/// venue's own pair `BTC/USD` is no user name, and a ticker ending in `BTC` is never guessed a coin. Port 9: a POST
/// would have answered with a network error, not these words.
#[test]
fn an_order_whose_name_has_no_kept_row_is_rejected_with_the_fix() {
    crate::names::hold_test_rows();
    let dead = "http://127.0.0.1:9";
    let hosts =
        crate::AlpacaUrls { authx: dead.into(), broker: dead.into(), data_websocket: dead.into() };
    let config = AlpacaConfig {
        client_id: vike_model::secret::Secret::new("id".into()),
        client_secret: vike_model::secret::Secret::new("sec".into()),
        account_id: "acct".into(),
        env: AccountTier::Demo,
        hosts,
    };
    let token = Arc::new(TokenSource::new("id".into(), "sec".into(), dead.into()));
    let (events, mut ingest) = event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel();
    let symbols = submitted_symbols();
    for (order_client_id, symbol) in [("guess", "GBTC"), ("old", "BTC/USD")] {
        symbols.record(order_client_id, symbol);
        let order = OrderRequest {
            order_client_id: order_client_id.into(),
            symbol: symbol.into(),
            ..Default::default()
        };
        tx.send(ExecutionClientActorCommand::Submit(Box::new(order), Instant::now())).unwrap();
    }
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run(config, token, events, &rx, symbols.clone());

    let mut rejected = Vec::new();
    while let Ok(Ingest::Event(e)) = ingest.try_recv() {
        if let Event::OrderRejected(r) = e {
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
            rejected.push(r.reason.to_string());
        }
    }
    assert_eq!(rejected.len(), 2, "{rejected:?}");
    for (why, name) in rejected.iter().zip(["GBTC", "BTC/USD"]) {
        assert!(why.contains(name), "{why}");
        assert!(
            why.contains("vike-cli data instruments add") && why.contains("nothing was sent"),
            "{why}"
        );
    }
    assert_eq!(symbols.label("guess", "X"), "X", "the refused order's entry left");
}

/// **Alpaca's `204` to a cancel is "request accepted", not "cancelled"** (the Broker API reference's cancel: "Upon
/// acceptance of the cancel request, it returns status 204"). It is `Requested`, the loop says nothing for it, and the
/// order rests until the trade-event stream says how it ended: its `canceled` ends it Canceled, and a `fill` that beat
/// the cancel ends it Filled, where the old terminal read that filled order CANCELED for good (the order table has no
/// `Canceled -> Filled` edge). The venue is a loopback answering the lookup and the DELETE as documented.
#[test]
fn a_204_to_a_cancel_leaves_the_order_to_the_trade_event_stream() {
    use crate::execution::event_mapper::decode_trade_event;
    use vike_model::{OrderRecord, OrderStatus};
    let lookup = r#"{"id":"v-1","client_order_id":"c1","status":"new","symbol":"AAPL"}"#;
    let order = r#"{"client_order_id":"c1","id":"v-1","symbol":"AAPL","side":"buy"}"#;
    let canceled = format!(r#"{{"event":"canceled","order":{order}}}"#);
    let fill = format!(
        r#"{{"event":"fill","execution_id":"e-1","timestamp":"2026-07-14T05:39:31.4Z","price":"151.00","qty":"1","order":{order}}}"#
    );
    for (stream, ends) in [(canceled, OrderStatus::Canceled), (fill, OrderStatus::Filled)] {
        let answers = || [(200, lookup.to_string()), (204, String::new())];
        let (base, seen) =
            vike_log::loopback::serve_replies("", answers().into_iter().chain(answers()).collect());
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Ok(("token".to_string(), 900))),
        );
        let (events, _ingest) = event_channel(8);
        let symbols = submitted_symbols();
        symbols.record("c1", "AAPL");
        let rest = VenueRestAlpaca {
            rest: AlpacaTransport::new(Arc::new(token)),
            base,
            account: "acct".into(),
            events,
            symbols: symbols.clone(),
            late: RefCell::default(),
        };
        assert_eq!(rest.cancel_order("c1").map_err(|e| e.msg), Ok(OrderCancelAck::Requested));
        let answered = vike_bridge_core::execution_drive::drive_cancel(&rest, "c1");
        assert!(answered.is_empty(), "a request is no answer: {answered:?}");
        let lines = seen.lock().unwrap().clone();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(
            lines[2].starts_with("GET ") && lines[2].contains("client_order_id=c1"),
            "{lines:?}"
        );
        assert!(lines[3].starts_with("DELETE ") && lines[3].contains("/orders/v-1 "), "{lines:?}");

        let mut record = OrderRecord::new(OrderRequest {
            order_client_id: "c1".into(),
            symbol: "AAPL".into(),
            side: 1,
            qty: 1.0,
            ..Default::default()
        });
        let submitted =
            Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".into(), timestamp: 1 });
        let accepted =
            event_mapper::map_order_response("c1", 1, &serde_json::from_str(lookup).unwrap());
        for event in std::iter::once(&submitted).chain(&accepted).chain(&answered) {
            record.apply(event).expect("a legal step");
        }
        assert_eq!(record.status, OrderStatus::Accepted, "still resting after the 204");
        let said = decode_trade_event(&serde_json::from_str(&stream).unwrap(), &symbols);
        for event in said.iter().filter(|e| !matches!(e, Event::Fill(_))) {
            record.apply(event).expect("the stream's terminal is legal");
        }
        assert_eq!(record.status, ends, "{said:?}");
    }
}

/// **AL-3: a failed cancel says what it is of the cancel** (decision 0303), where every failure was the never-sent `0`
/// and a failed lookup read "unknown order": a lookup that failed sent nothing (NOT SENT, and no DELETE goes out); the
/// lookup's `404` and a DELETE's `422` are Alpaca's refusals (the status as the code); a DELETE's 5xx may have been
/// taken, so it is OUTCOME UNKNOWN. The venue is a loopback answering each request in turn.
#[test]
fn a_failed_cancel_says_whether_the_delete_went_out_and_what_it_met() {
    use vike_model::OrderRefusalId::{NotSent, OutcomeUnknown, VenueRefused};
    let lookup = (200, r#"{"id":"v-1","client_order_id":"c1"}"#.to_string());
    let refusal = |status: u16, message: &str| (status, format!(r#"{{"message":"{message}"}}"#));
    for (answers, code, id, requests) in [
        (vec![refusal(500, "internal server error")], 0, NotSent, 1),
        (vec![refusal(404, "order not found")], 404, VenueRefused, 1),
        (vec![lookup.clone(), refusal(422, "order is not cancelable")], 422, VenueRefused, 2),
        (
            vec![lookup, refusal(500, "internal server error")],
            E_TIMEOUT_AMBIGUOUS,
            OutcomeUnknown,
            2,
        ),
    ] {
        let (base, seen) = vike_log::loopback::serve_replies("", answers);
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Ok(("token".to_string(), 900))),
        );
        let (events, _ingest) = event_channel(8);
        let rest = VenueRestAlpaca {
            rest: AlpacaTransport::new(Arc::new(token)),
            base,
            account: "acct".into(),
            events,
            symbols: submitted_symbols(),
            late: RefCell::default(),
        };
        let e = rest.cancel_order("c1").expect_err("a failed cancel");
        assert_eq!((e.code, e.refusal_id_word()), (code, id.word()), "{}", e.msg);
        let lines = seen.lock().unwrap().clone();
        assert_eq!(lines.len(), requests, "{lines:?}");
    }
}
