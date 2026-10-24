//! A resting STOP is an ALGO order (`order-algo`), and OKX's regular endpoints do not hold it: the
//! regular `cancel-order` / `GET order` / `orders-pending` answer "unknown" for its coid. Reading that
//! as "gone" books a terminal `OrderCanceled` / `OrderRejected` over a stop that still rests. Pinned
//! here with a scripted transport that records every call: a cancel the regular endpoint does not know
//! goes on to `cancel-algos` (ARRAY body, per-row `sCode`), the lookup asks `order-algo` before
//! concluding "absent", the batch cancel retries its unknown rows there, and the order report lists the
//! stops from `orders-algo-pending`. A FAILED algo call is an error / silence, never "gone".
//!
//! Two things stay as they were: an ORDINARY submit's timeout resolves on the regular book alone (a
//! failed algo query must not turn a clean "absent" into a phantom accept), and the code sets are
//! pinned code by code (`NOT_FOUND` for the retry, `algo_unknown` for "the algo book has no such
//! stop"), measured on the demo account on 2026-10-23 by the smoke's `MEASURED` lines (the
//! codes `51400` and `51603`; `51413` and `51403` stay UNMEASURED and out of both sets).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_exec::recon::ReconClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};
use vike_okx::OkxReconClient;
use vike_okx::perp::OkxPerpRest;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;

use crate::r6_okx_parity::{fixture, okx_perp};

type Reply = Box<dyn Fn(&str) -> Result<Value, VenueApiError> + Send + Sync>;

/// Answers by the LAST path segment (`cancel-order`, `cancel-algos`, `order`, `order-algo`, ...) and
/// records `(method, tail, body-or-params)` for every call; any `public` call panics.
struct Script {
    reply: Reply,
    log: Log,
}

/// The call record, shareable: `OkxReconClient` keeps its transport private, so a test that moves the
/// script into it keeps a clone of this to read the calls back.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<(String, String, String)>>>);

impl Log {
    /// `"POST cancel-order"`-shaped, in call order.
    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().map(|(m, t, _)| format!("{m} {t}")).collect()
    }
    fn body(&self, tail: &str) -> Value {
        let calls = self.0.lock().unwrap();
        let (_, _, body) = calls.iter().find(|(_, t, _)| t == tail).expect("that call was made");
        serde_json::from_str(body).expect("a JSON body")
    }
}

fn tail(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl Script {
    fn new(reply: impl Fn(&str) -> Result<Value, VenueApiError> + Send + Sync + 'static) -> Self {
        Self { reply: Box::new(reply), log: Log::default() }
    }
    fn record(&self, method: &str, path: &str, body: String) -> Result<Value, VenueApiError> {
        self.log.0.lock().unwrap().push((method.to_string(), tail(path).to_string(), body));
        (self.reply)(tail(path))
    }
}

impl OkxTransport for Script {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        let obj: serde_json::Map<String, Value> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.record(method, path, Value::Object(obj).to_string())
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        self.record(method, path, body.to_string())
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn rest(script: Script) -> OkxPerpRest<Script> {
    okx_perp(&fixture(), script)
}

/// A per-row refusal, in the shape the venue sends a failed single-element request.
fn refused(code: &str) -> Value {
    json!({"code": "1", "msg": "All operations failed",
        "data": [{"sCode": code, "sMsg": "venue words"}]})
}

fn done() -> Value {
    json!({"code": "0", "msg": "", "data": [{"sCode": "0", "sMsg": ""}]})
}

fn absent() -> Value {
    json!({"code": "51603", "msg": "Order does not exist", "data": []})
}

// ---- cancel ------------------------------------------------------------------------------------

#[test]
fn a_stop_the_regular_book_does_not_know_is_cancelled_on_cancel_algos() {
    let c = rest(Script::new(|t| match t {
        "cancel-order" => Ok(refused("51400")),
        "cancel-algos" => Ok(done()),
        other => panic!("unexpected {other}"),
    }));
    c.cancel_order("sl1").expect("the stop was cancelled");
    assert_eq!(c.transport.log.calls(), ["POST cancel-order", "POST cancel-algos"]);
    // the body is a JSON ARRAY (the flat-object door cannot send it), keyed on `algoClOrdId`
    assert_eq!(
        c.transport.log.body("cancel-algos"),
        json!([{"instId": "BTC-USDT-SWAP", "algoClOrdId": "sl1"}])
    );
}

/// The Binance trap: a failed retry is an ERROR, never "gone" — the stop may still rest.
#[test]
fn a_stop_cancel_the_algo_endpoint_refuses_is_an_error_not_gone() {
    for code in ["51000", "50011", "51412"] {
        let c = rest(Script::new(move |t| match t {
            "cancel-order" => Ok(refused("51400")),
            "cancel-algos" => Ok(refused(code)),
            other => panic!("unexpected {other}"),
        }));
        assert!(c.cancel_order("sl1").is_err(), "algo answer {code} must not read as gone");
    }
    // a transport failure of the retry is an error too
    let c = rest(Script::new(|t| match t {
        "cancel-order" => Ok(refused("51400")),
        _ => Err(VenueApiError { code: 503, msg: "bad gateway".into() }),
    }));
    assert!(c.cancel_order("sl1").is_err());
}

#[test]
fn unknown_on_both_endpoints_is_already_gone() {
    let c = rest(Script::new(|t| match t {
        "cancel-order" => Ok(refused("51400")),
        "cancel-algos" => Ok(refused("51400")),
        "order-algo" => Ok(absent()),
        other => panic!("unexpected {other}"),
    }));
    c.cancel_order("gone1").expect("idempotent: unknown everywhere is gone");
    // the algo book is asked once before its "unknown" is read as gone
    assert_eq!(
        c.transport.log.calls(),
        ["POST cancel-order", "POST cancel-algos", "GET order-algo"]
    );
}

/// Finding 7: `cancel-algos` saying "unknown" is not yet "gone" — a stop that just TRIGGERED may be
/// unknown to the cancel too, and a booked `OrderCanceled` would hide the fill that follows. The algo
/// book decides: no row or `canceled` = gone; any other state (or none) = an error, no cancel booked.
#[test]
fn unknown_on_cancel_algos_is_gone_only_if_the_algo_book_agrees() {
    let cancel_with = |lookup: Result<Value, VenueApiError>| {
        let c = rest(Script::new(move |t| match t {
            "cancel-order" | "cancel-algos" => Ok(refused("51400")),
            "order-algo" => lookup.clone(),
            other => panic!("unexpected {other}"),
        }));
        c.cancel_order("sl1")
    };
    let row = |state: Value| {
        Ok(json!({"code": "0", "msg": "", "data": [{"algoId": "a1", "state": state}]}))
    };
    // `order_failed` / `partially_failed` rest nowhere yet stay an `Err` here: an OPEN item (the crate
    // CLAUDE.md), pinned as it is today, to be mapped to gone once the smoke shows their words
    for fired_or_resting in
        ["live", "pause", "effective", "partially_effective", "order_failed", "partially_failed"]
    {
        let err = cancel_with(row(json!(fired_or_resting))).expect_err(fired_or_resting);
        assert_eq!(err.code, 51412, "{fired_or_resting}");
        assert!(err.msg.contains(fired_or_resting), "the state is in the words: {}", err.msg);
    }
    assert!(cancel_with(row(Value::Null)).is_err(), "a row with no state proves nothing");
    cancel_with(row(json!("canceled"))).expect("the algo book says it is cancelled: gone");
    cancel_with(Ok(absent())).expect("the algo book does not know it either: gone");
    // CHOSEN POLICY (a doubt for the owner): a lookup that FAILS leaves the venue's own "unknown"
    // standing, so an ordinary order's cancel race never turns on a second endpoint's health
    cancel_with(Err(VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "timeout".into() }))
        .expect("a failed lookup does not turn the venue's unknown into an error");
    cancel_with(Ok(json!({"code": "50011", "msg": "rate limit", "data": []})))
        .expect("nor does a refused one");
}

/// Finding 3: the REGULAR retry set is exactly `NOT_FOUND` (51400, 51401, 51402): the words
/// `cancel_order` swallowed before stops were cancelled on `cancel-algos`. Every other regular answer
/// stays the `Err` it always was and never reaches the algo endpoint — `51603` ("does not exist") used
/// to be one, and a `cancel-algos` that also swallows it would have booked `OrderCanceled` for an
/// order that never existed. The demo smoke of 2026-10-23 measured the regular cancel of a stop's coid
/// answering 51400 (the stop survives it), the first code of the set. `51413`/`51403` ("type not
/// supported") are UNMEASURED and NOT in the set.
#[test]
fn the_regular_retry_set_is_pinned_code_by_code() {
    let shapes = |code: &str| -> Vec<(&'static str, Result<Value, VenueApiError>)> {
        vec![
            ("per-row sCode", Ok(refused(code))),
            ("envelope code", Ok(json!({"code": code, "msg": "m", "data": []}))),
            ("non-2xx", Err(VenueApiError { code: code.parse().unwrap(), msg: "m".to_string() })),
        ]
    };
    for code in ["51400", "51401", "51402"] {
        for (shape, reply) in shapes(code) {
            let c = rest(Script::new(move |t| match t {
                "cancel-order" => reply.clone(),
                "cancel-algos" => Ok(done()),
                other => panic!("unexpected {other}"),
            }));
            c.cancel_order("sl1").unwrap_or_else(|e| panic!("{code} {shape}: {e:?}"));
            assert_eq!(
                c.transport.log.calls(),
                ["POST cancel-order", "POST cancel-algos"],
                "{code} {shape}"
            );
        }
    }
    for code in ["51603", "51413", "51403", "51412", "51000", "50011"] {
        for (shape, reply) in shapes(code) {
            let c = rest(Script::new(move |t| match t {
                "cancel-order" => reply.clone(),
                other => panic!("{code}: unexpected {other}"),
            }));
            assert!(c.cancel_order("o1").is_err(), "{code} {shape} must stay an error");
            assert_eq!(c.transport.log.calls(), ["POST cancel-order"], "{code} {shape}: no retry");
        }
    }
}

/// The ALGO "no such stop" set (`algo_unknown`): 51400, 51401, 51402 and 51603. The demo smoke of
/// 2026-10-23 measured two of them (`cancel-algos` of an unknown id answers 51400, `order-algo` of an
/// unknown id 51603); 51401 and 51402 come from the regular family and no line has shown them. It reads as
/// gone on `cancel-algos` (once the algo book agrees) and as ABSENT on the `order-algo` lookup; any
/// other word is an error on the cancel and silence on the lookup. Each code, both shapes of a refusal
/// (a 200 envelope's per-row `sCode`, and a coded non-2xx — finding 4: `cancel-algos` used to return a
/// transport error before the code check, so a coded 4xx skipped the set the regular cancel and
/// `lookup_algo` do consult).
#[test]
fn the_algo_unknown_set_is_pinned_code_by_code() {
    let shapes = |code: &str| -> Vec<(&'static str, Result<Value, VenueApiError>)> {
        vec![
            ("per-row sCode", Ok(refused(code))),
            ("non-2xx", Err(VenueApiError { code: code.parse().unwrap(), msg: "m".to_string() })),
        ]
    };
    for code in ["51400", "51401", "51402", "51603"] {
        for (shape, reply) in shapes(code) {
            // cancel
            let cancel_reply = reply.clone();
            let c = rest(Script::new(move |t| match t {
                "cancel-order" => Ok(refused("51400")),
                "cancel-algos" => cancel_reply.clone(),
                "order-algo" => Ok(absent()),
                other => panic!("unexpected {other}"),
            }));
            c.cancel_order("sl1").unwrap_or_else(|e| panic!("cancel {code} {shape}: {e:?}"));
            // lookup: the regular book is absent, the algo book answers `code` = absent on both
            let c = rest(Script::new(move |t| match t {
                "order" => Ok(absent()),
                "order-algo" => reply.clone(),
                other => panic!("unexpected {other}"),
            }));
            assert!(
                matches!(c.confirm_order("x1", 5)[..], [Event::OrderRejected(_)]),
                "lookup {code} {shape}"
            );
        }
    }
    for code in ["51413", "51403", "51412", "51000", "50011"] {
        for (shape, reply) in shapes(code) {
            let cancel_reply = reply.clone();
            let c = rest(Script::new(move |t| match t {
                "cancel-order" => Ok(refused("51400")),
                "cancel-algos" => cancel_reply.clone(),
                other => panic!("{code}: unexpected {other}"),
            }));
            assert!(c.cancel_order("sl1").is_err(), "cancel {code} {shape} must stay an error");
            assert_eq!(
                c.transport.log.calls(),
                ["POST cancel-order", "POST cancel-algos"],
                "{code} {shape}: no lookup behind a refusal"
            );
            let c = rest(Script::new(move |t| match t {
                "order" => Ok(absent()),
                "order-algo" => reply.clone(),
                other => panic!("unexpected {other}"),
            }));
            assert!(c.confirm_order("x1", 5).is_empty(), "lookup {code} {shape}: silence");
        }
    }
}

/// Finding 4: a coded reply the TRANSPORT turned into `Err` (a non-2xx whose body carries the venue's
/// `code`) is judged by that code like any other `cancel-algos` answer; an uncoded transport failure
/// stays an error.
#[test]
fn a_coded_non_2xx_cancel_algos_reply_is_judged_by_its_code() {
    let cancel_with = |algos: VenueApiError| {
        let c = rest(Script::new(move |t| match t {
            "cancel-order" => Ok(refused("51400")),
            "cancel-algos" => Err(algos.clone()),
            "order-algo" => Ok(absent()),
            other => panic!("unexpected {other}"),
        }));
        c.cancel_order("sl1")
    };
    cancel_with(VenueApiError { code: 51400, msg: "Order does not exist".into() })
        .expect("a coded 'unknown' is gone, as it is in a 200 envelope");
    for code in [51000, 50011, E_TIMEOUT_AMBIGUOUS, 503] {
        assert!(
            cancel_with(VenueApiError { code, msg: "m".into() }).is_err(),
            "{code} must stay an error"
        );
    }
}

#[test]
fn an_ordinary_cancel_is_still_one_call() {
    let c = rest(Script::new(|t| match t {
        "cancel-order" => Ok(done()),
        other => panic!("unexpected {other}"),
    }));
    c.cancel_order("o1").expect("cancelled");
    assert_eq!(c.transport.log.calls(), ["POST cancel-order"]);
}

#[test]
fn a_failure_that_says_nothing_about_the_book_never_tries_the_algo_endpoint() {
    for reply in [
        Err(VenueApiError { code: 503, msg: "bad gateway".into() }),
        Ok(refused("50011")),
        Ok(refused("51412")),
    ] {
        let c = rest(Script::new(move |_| reply.clone()));
        assert!(c.cancel_order("o1").is_err());
        assert_eq!(c.transport.log.calls(), ["POST cancel-order"], "no second call");
    }
}

#[test]
fn the_batch_cancel_retries_the_rows_the_regular_book_did_not_know_on_cancel_algos() {
    let c = rest(Script::new(|t| match t {
        "cancel-batch-orders" => Ok(json!({"code": "2", "msg": "", "data": [
            {"clOrdId": "o1", "sCode": "0"}, {"clOrdId": "sl1", "sCode": "51400"}]})),
        "cancel-algos" => Ok(done()),
        other => panic!("unexpected {other}"),
    }));
    c.cancel_batch(&["o1".into(), "sl1".into()]).expect("both gone");
    assert_eq!(c.transport.log.calls(), ["POST cancel-batch-orders", "POST cancel-algos"]);
    assert_eq!(
        c.transport.log.body("cancel-algos"),
        json!([{"instId": "BTC-USDT-SWAP", "algoClOrdId": "sl1"}])
    );

    // and a refused retry is the batch's error
    let c = rest(Script::new(|t| match t {
        "cancel-batch-orders" => Ok(json!({"code": "1", "msg": "", "data": [
            {"clOrdId": "sl1", "sCode": "51400"}]})),
        "cancel-algos" => Ok(refused("51000")),
        other => panic!("unexpected {other}"),
    }));
    assert!(c.cancel_batch(&["sl1".into()]).is_err());
}

// ---- lookup by coid (the active confirm) ------------------------------------------------------

#[test]
fn the_confirm_finds_a_resting_stop_on_the_algo_endpoint() {
    let c = rest(Script::new(|t| match t {
        "order" => Ok(absent()),
        "order-algo" => Ok(json!({"code": "0", "msg": "", "data": [
            {"algoId": "algo-77", "algoClOrdId": "sl1", "state": "live"}]})),
        other => panic!("unexpected {other}"),
    }));
    match &c.confirm_order("sl1", 5)[..] {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, "sl1");
            assert_eq!(a.venue_order_id.as_deref(), Some("algo-77"));
        }
        other => panic!("a resting stop must be accepted, got {other:?}"),
    }
    assert_eq!(c.transport.log.calls(), ["GET order", "GET order-algo"]);
}

#[test]
fn absent_needs_both_endpoints() {
    let c = rest(Script::new(|t| match t {
        "order" | "order-algo" => Ok(absent()),
        other => panic!("unexpected {other}"),
    }));
    assert!(matches!(c.confirm_order("x1", 5)[..], [Event::OrderRejected(_)]));
    assert_eq!(c.transport.log.calls(), ["GET order", "GET order-algo"]);
}

/// A FAILED algo query says nothing about the order — never the "venue confirms absent" reject.
#[test]
fn a_failed_algo_query_says_nothing() {
    let c = rest(Script::new(|t| match t {
        "order" => Ok(absent()),
        "order-algo" => Ok(json!({"code": "50011", "msg": "rate limit", "data": []})),
        other => panic!("unexpected {other}"),
    }));
    assert!(c.confirm_order("sl1", 5).is_empty());
}

// ---- the ambiguous submit (ordinary orders only) ----------------------------------------------

fn limit_request() -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": "t1", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

/// Finding 1: an ordinary limit order's POST times out, `GET order` answers 51603 (the venue confirms
/// it is absent) — the clean reject of the base — and the algo endpoint is NOT consulted, so no
/// failure of it (a 5 s requery timeout, a 50011) can turn the answer into an id-less accept of an
/// order that never reached OKX. (An ordinary submit can never be a stop: `submit_order` hands those
/// to `submit_stop_algo`, which never resolves.)
#[test]
fn an_ordinary_submit_timeout_is_rejected_when_the_regular_book_says_absent() {
    for algo in [
        Err(VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "requery timed out".into() }),
        Ok(json!({"code": "50011", "msg": "rate limit", "data": []})),
    ] {
        let orders_seen = AtomicUsize::new(0);
        let c = rest(Script::new(move |t| match t {
            // the first `order` call is the POST (it times out), the second the by-coid re-query
            "order" if orders_seen.fetch_add(1, Ordering::SeqCst) == 0 => {
                Err(VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "timeout".into() })
            }
            "order" => Ok(absent()),
            "order-algo" => algo.clone(),
            other => panic!("unexpected {other}"),
        }));
        match &c.submit_order(&limit_request())[..] {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
                assert_eq!(r.reason, "submit timed out; venue confirms order absent");
            }
            other => panic!("a clean 'absent' must be the terminal reject, got {other:?}"),
        }
        assert_eq!(
            c.transport.log.calls(),
            ["POST order", "GET order"],
            "the algo endpoint is not asked"
        );
    }
}

// ---- the order report (reconcile / restore planner) -------------------------------------------

fn recon(script: Script) -> OkxReconClient<Script> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    OkxReconClient::new(
        OkxV5Signer::new(&creds, || 0),
        script,
        "https://unused.invalid",
        "BTC-USDT-SWAP",
        0.01,
    )
}

#[test]
fn the_order_report_lists_the_resting_stops() {
    let script = Script::new(|t| match t {
        "orders-pending" => Ok(json!({"code": "0", "msg": "", "data": []})),
        "orders-algo-pending" => Ok(json!({"code": "0", "msg": "", "data": [{
            "instId": "BTC-USDT-SWAP", "algoId": "algo-9", "algoClOrdId": "sl1",
            "ordType": "conditional", "side": "sell", "sz": "3", "state": "live",
            "slTriggerPx": "50000", "reduceOnly": "true", "uTime": "1700000000000"}]})),
        other => panic!("unexpected {other}"),
    });
    let log = script.log.clone();
    let c = recon(script);
    let reports = c.fetch_order_status_reports(0).expect("report");
    let [r] = &reports[..] else { panic!("one resting stop, got {reports:?}") };
    assert_eq!(r.client_order_id.as_deref(), Some("sl1"));
    assert_eq!(r.venue_order_id.as_str(), "algo-9");
    assert_eq!(r.order_type, OrderType::Stop);
    assert_eq!(r.side, -1);
    assert!((r.qty - 0.03).abs() < 1e-12, "3 contracts * ctVal 0.01 = 0.03 base, got {}", r.qty);
    assert!(r.status.is_live());
    // `ordType` is REQUIRED by the endpoint; the instrument narrows the read
    let query = log.body("orders-algo-pending");
    assert_eq!(query["ordType"], "conditional");
    assert_eq!(query["instId"], "BTC-USDT-SWAP");
}

/// STRICT: a failed algo read must not become "no stops" — the restore planner would plan every
/// resting stop `gone`. The whole report fails instead (the seam has no partial-listing channel, so
/// ordinary reports alone would retire a stop that still rests), and the error NAMES the algo read so
/// the staleness escalation tells an operator which request blinded the leg.
#[test]
fn a_failed_algo_read_fails_the_whole_order_report() {
    let ordinary = json!({"code": "0", "msg": "", "data": [{
        "instId": "BTC-USDT-SWAP", "ordId": "o-1", "clOrdId": "lim1", "side": "buy",
        "ordType": "limit", "sz": "1", "px": "50000", "state": "live"}]});
    for algo in [
        Ok(json!({"code": "50011", "msg": "rate limit", "data": []})),
        // a request-shape refusal (a 400 whose body carries the venue's code) arrives as `Err`
        Err(VenueApiError { code: 50014, msg: "Parameter instType can not be empty".into() }),
    ] {
        let ordinary = ordinary.clone();
        let c = recon(Script::new(move |t| match t {
            "orders-pending" => Ok(ordinary.clone()),
            "orders-algo-pending" => algo.clone(),
            other => panic!("unexpected {other}"),
        }));
        let why = c.fetch_order_status_reports(0).expect_err("no report without the stops");
        assert!(why.contains("orders-algo-pending"), "the error names the algo read: {why}");
    }
}
