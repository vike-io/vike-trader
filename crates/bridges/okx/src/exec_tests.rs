//! OKX-specific tests: the PIT properties recording. The generic `run_loop` command→event tests
//! (mock `VenueRest`) moved WITH `run_loop` to `vike_bridge_core::exec_actor::run_loop_tests`;
//! the okx reality-tie (`caps_for("okx").supports_modify`) lives in the lib `caps_test`.
use super::*;

/// The leverage this arm POSTs to `/api/v5/account/set-leverage` is the OPERATOR's
/// `[risk] max_leverage`, and an unset profile still yields the historical literal (`2.0`) —
/// the property that makes this change safe to ship onto a live account. NO network:
/// `leverage_for` is the pure decision the start calls once, before anything is spawned.
#[test]
fn perp_leverage_follows_the_operator_risk_budget() {
    // Unset — both shapes of "the operator said nothing" — keeps today's literal EXACTLY.
    assert_eq!(leverage_for(None), DEFAULT_LEVERAGE);
    assert_eq!(leverage_for(None), 2.0, "the historical hardcoded set-leverage value");
    let silent = vike_model::ProfileRisk { max_leverage: None, ..Default::default() };
    assert_eq!(leverage_for(Some(&silent)), 2.0);

    // Set — the venue account is configured with the operator's own number, in BOTH
    // directions (the two ways the old hardcode was dangerous).
    let ten = vike_model::ProfileRisk { max_leverage: Some(10.0), ..Default::default() };
    assert_eq!(leverage_for(Some(&ten)), 10.0);
    let one = vike_model::ProfileRisk { max_leverage: Some(1.0), ..Default::default() };
    assert_eq!(leverage_for(Some(&one)), 1.0);

    // ONE number governs both sides: what we POST is the reciprocal of the initial-margin
    // fraction the RiskGate sizes against.
    assert_eq!(ten.im_requirement(), Some(0.1));
    assert_eq!(ten.im_requirement(), Some(1.0 / leverage_for(Some(&ten))));
    // The same invariant, via the shared helper the rule itself ships.
    assert!(vike_bridge_core::leverage::agrees_with_im_requirement(&ten, DEFAULT_LEVERAGE));
}

/// PIT filter recording (task 5): `record_properties` writes the REAL fetched grid into the store
/// when a recorder is present, keyed by venue `"okx"`. Uses the DataFusion-free `MemHistStore`
/// test double (vike-data's `test-support` dev-feature) so this venue's default build never pulls
/// DataFusion in.
#[test]
fn okx_records_fetched_properties_when_recorder_present() {
    use std::sync::Arc;
    use vike_data::HistStore;
    let store = Arc::new(vike_data::MemHistStore::new());
    let rec = Some(Arc::new(vike_data::PropertiesRecorder::new(store.clone(), true)));
    let f = SymbolProperties {
        tick_size: 0.1,
        step_size: 0.001,
        min_qty: 0.001,
        min_notional: 5.0,
        ..Default::default()
    };
    vike_data::PropertiesRecorder::record_opt(
        &rec,
        "okx",
        "BTC-USDT-SWAP",
        f,
        1_577_836_800_000_000_000,
    );
    assert_eq!(
        store.scan_symbol_properties("okx", "BTC-USDT-SWAP", vike_data::TsRange::all()).unwrap(),
        vec![(1_577_836_800_000i64, f)]
    );
}

#[test]
fn okx_no_recorder_writes_nothing() {
    use std::sync::Arc;
    use vike_data::HistStore;
    let store = Arc::new(vike_data::MemHistStore::new());
    vike_data::PropertiesRecorder::record_opt(
        &None,
        "okx",
        "BTC-USDT-SWAP",
        SymbolProperties::default(),
        1_577_836_800_000_000_000,
    );
    assert!(
        store
            .scan_symbol_properties("okx", "BTC-USDT-SWAP", vike_data::TsRange::all())
            .unwrap()
            .is_empty()
    );
}

/// A fake [`OkxTransport`] that answers `/api/v5/account/bills` with a canned funding bill (or
/// an empty list) — the offline twin of the live `OkxFundingPoller` smoke, proving
/// `poll_funding_once`'s spawn glue (not just the pure decoder, which `r6_okx_parity.rs`
/// already pins if present).
struct FakeFundingTransport {
    rows: Vec<serde_json::Value>,
    /// When `Some`, assert the query carries this exact `begin` (the floor optimization
    /// param); `None` asserts the param is ABSENT (a floorless probe's query, unchanged).
    expect_begin: Option<String>,
}

impl OkxTransport for FakeFundingTransport {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
        assert_eq!(path, crate::perp::PATH_BILLS);
        let begin = params.iter().find(|(k, _)| *k == "begin").map(|(_, v)| v.clone());
        match &self.expect_begin {
            Some(want) => assert_eq!(begin, Some(serde_json::json!(want))),
            None => assert_eq!(begin, None, "a floorless poll must not send begin"),
        }
        Ok(serde_json::json!({"code": "0", "msg": "", "data": self.rows}))
    }

    fn public(
        &self,
        _base_url: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
        unreachable!("the funding poll never calls the public GET")
    }
}

fn funding_bill(bill_id: &str, pnl: &str, ts: &str) -> serde_json::Value {
    serde_json::json!({
        "billId": bill_id, "type": "8", "instId": "BTC-USDT-SWAP",
        "pnl": pnl, "balChg": pnl, "ts": ts
    })
}

/// The spawn glue's one poll iteration forwards decoded `Event::Funding`s onto the core
/// ingest lane — the path a background poll thread drives every cadence.
#[test]
fn poll_funding_once_forwards_decoded_events() {
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: FakeFundingTransport {
            rows: vec![funding_bill("b1", "0.42", "1000")],
            expect_begin: None,
        },
        base_url: "http://example.invalid".to_string(),
        symbol: "BTC-USDT-SWAP".to_string(),
        properties: SymbolProperties::default(),
        ct_val: 0.0,
        leverage: DEFAULT_LEVERAGE,
        broker_code: None,
    };
    let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", 0);
    let (events, mut ingest) = vike_exec::event_channel(8);
    let mut streak = 0u32;
    poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
    assert_eq!(streak, 0, "a successful poll resets the failure streak");

    let mut got = Vec::new();
    while let Ok(vike_exec::Ingest::Event(e)) = ingest.try_recv() {
        got.push(e);
    }
    match got.as_slice() {
        [Event::Funding(f)] => {
            assert_eq!(f.venue, "okx");
            assert_eq!(f.symbol, "BTC-USDT-SWAP");
            assert_eq!(f.amount, 0.42, "received-positive, no sign flip");
        }
        other => panic!("expected one Event::Funding, got {other:?}"),
    }

    // A second poll with the SAME bill (as a real re-fetch might return before settling)
    // yields nothing new — the dedup-by-billId guard the live poller already owns.
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
    assert!(ingest.try_recv().is_err(), "the already-seen bill must not re-emit");
}

/// The restart law: a poller floored at its spawn instant (as `spawn_funding_poll` does) must
/// NEVER emit a bill that posted BEFORE the floor — those are already embedded in the
/// authoritative venue balance — while a bill AT/after the floor is emitted exactly once.
/// Simulates the (re)start-with-empty-seen-set case the in-memory billId-dedup cannot cover.
#[test]
fn poll_funding_skips_bills_before_the_spawn_floor() {
    const FLOOR: i64 = 5_000;
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: FakeFundingTransport {
            // The venue answers with history straddling the floor (as the default window
            // would on a restart): pre-floor, exactly-at-floor, and post-floor bills.
            rows: vec![
                funding_bill("old", "9.99", "1000"),
                funding_bill("edge", "0.10", "5000"),
                funding_bill("new", "0.42", "9000"),
            ],
            // The floor also rides as the begin request param (the optimization).
            expect_begin: Some(FLOOR.to_string()),
        },
        base_url: "http://example.invalid".to_string(),
        symbol: "BTC-USDT-SWAP".to_string(),
        properties: SymbolProperties::default(),
        ct_val: 0.0,
        leverage: DEFAULT_LEVERAGE,
        broker_code: None,
    };
    let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", FLOOR);
    let (events, mut ingest) = vike_exec::event_channel(8);
    let mut streak = 0u32;
    poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");

    let mut got = Vec::new();
    while let Ok(vike_exec::Ingest::Event(e)) = ingest.try_recv() {
        got.push(e);
    }
    match got.as_slice() {
        [Event::Funding(edge), Event::Funding(new)] => {
            assert_eq!(edge.ts, 5000, "ts == floor is emitted (floor is inclusive)");
            assert_eq!(new.ts, 9000);
            assert_eq!(new.amount, 0.42);
        }
        other => panic!("expected exactly the at/post-floor bills, got {other:?}"),
    }

    // The next cadence re-serves the same window: nothing re-emits (billId-dedup) and the
    // pre-floor bill stays excluded.
    poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
    assert!(ingest.try_recv().is_err(), "no duplicates on the second poll");
}

/// When the core is gone (ingest receiver dropped), the poll glue reports `Err(CoreGone)` so
/// its background thread's loop can self-exit instead of polling forever into a void.
#[test]
fn poll_funding_once_reports_core_gone() {
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: FakeFundingTransport {
            rows: vec![funding_bill("b2", "0.1", "1000")],
            expect_begin: None,
        },
        base_url: "http://example.invalid".to_string(),
        symbol: "BTC-USDT-SWAP".to_string(),
        properties: SymbolProperties::default(),
        ct_val: 0.0,
        leverage: DEFAULT_LEVERAGE,
        broker_code: None,
    };
    let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", 0);
    let (events, ingest) = vike_exec::event_channel(8);
    drop(ingest); // the core is gone
    let mut streak = 0u32;
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_err());
}

/// A transport failure is NOT core-gone: the glue reports `Ok` (keep polling), counts the
/// consecutive-failure streak (the once-per-streak warn key), and a later success resets it.
#[test]
fn poll_funding_fetch_failure_counts_the_streak() {
    struct FailingTransport;
    impl OkxTransport for FailingTransport {
        fn signed(
            &self,
            _base_url: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, serde_json::Value)],
            _signer: &OkxV5Signer,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            Err(vike_bridge_core::transport::VenueApiError {
                code: 0,
                msg: "connect refused".to_string(),
            })
        }

        fn public(
            &self,
            _base_url: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            unreachable!("the funding poll never calls the public GET")
        }
    }
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: FailingTransport,
        base_url: "http://example.invalid".to_string(),
        symbol: "BTC-USDT-SWAP".to_string(),
        properties: SymbolProperties::default(),
        ct_val: 0.0,
        leverage: DEFAULT_LEVERAGE,
        broker_code: None,
    };
    let mut poller = OkxFundingPoller::new(&rest, "BTC-USDT-SWAP", 0);
    let (events, mut ingest) = vike_exec::event_channel(8);
    let mut streak = 0u32;
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
    assert_eq!(streak, 2, "consecutive failures accumulate");
    assert!(ingest.try_recv().is_err(), "failures emit nothing");
}

/// A closed localhost port: connection-refused is immediate (no DNS, no network), so a request
/// fails fast and the assertions are purely on the GATE, which is consulted BEFORE the send.
const DEAD: &str = "http://127.0.0.1:1";

fn submit_client(sim: bool) -> OkxPerpRest<UreqOkxTransport> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let grid = SymbolProperties {
        tick_size: 0.1,
        step_size: 0.01,
        min_qty: 0.01,
        min_notional: 5.0,
        ..Default::default()
    };
    order_rest(&creds, "BTC-USDT-SWAP", grid, 0.01, 3.0, Some("broker".to_string()), sim)
}

/// **THE ORDER PATH IS GATED.** The exec thread's submit client is built on a transport that
/// carries the `OKX.orders` window, and the window is the table's own: it admits exactly its budget
/// in one `window` and holds back the next. Every smoke attaches this gate to a transport it builds
/// itself, so before this the gate was proven everywhere EXCEPT on the client that puts orders on
/// the wire — and `crate::ratelimit`'s per-transport-is-per-instrument model was a model of nothing.
#[test]
fn the_submit_client_is_built_on_the_venues_order_rate_gate() {
    use vike_venues::venue_rate_limits::OKX;
    let rest = submit_client(true);
    let gate = rest.transport.rate_gate().expect("the submit client's transport must be gated");
    for taken in 0..OKX.orders.admitted() {
        assert!(
            gate.try_proceed(),
            "slot {taken} of {} is inside the budget",
            OKX.orders.admitted()
        );
    }
    assert!(
        !gate.try_proceed(),
        "the {}th op inside one {:?} window must be held back",
        OKX.orders.admitted() + 1,
        OKX.orders.window()
    );
    // ...and the gate changed nothing else about the client: the fields the old literal set.
    assert_eq!(rest.base_url, REST);
    assert_eq!(rest.symbol, "BTC-USDT-SWAP");
    assert_eq!(rest.ct_val, 0.01);
    assert_eq!(rest.leverage, 3.0);
    assert_eq!(rest.broker_code.as_deref(), Some("broker"));
    assert_eq!(rest.properties.tick_size, 0.1);
}

/// The demo/mainnet verdict still reaches the transport: `sim` is the `x-simulated-trading` header
/// switch, and a wrong literal here sends real-money orders to a URL that looks like the demo one
/// (the crate's `simulated` trap). Both values pass straight through.
#[test]
fn the_submit_client_keeps_the_simulated_trading_switch() {
    assert!(submit_client(true).transport.simulated, "demo keeps the simulated-trading header");
    assert!(!submit_client(false).transport.simulated, "mainnet drops it");
}

/// Every operation the submit client speaks to the order endpoints draws ONE slot of that gate —
/// submit, cancel, amend, a stop (algo) order and the startup `set_leverage` all go through the
/// transport's gated `signed`, none of them through the exempt `signed_requery`. (A
/// connection-refused send still took its slot: the gate is consulted before the send.)
#[test]
fn every_order_operation_of_the_submit_client_draws_the_gate() {
    use vike_venues::venue_rate_limits::OKX;
    let mut rest = submit_client(true);
    rest.base_url = DEAD.to_string();
    let gate = rest.transport.rate_gate().expect("gated").clone();
    let req = OrderRequest {
        client_order_id: "c1".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: 1,
        qty: 1.0,
        ..Default::default()
    };
    let stop = OrderRequest {
        client_order_id: "c2".into(),
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(50_000.0),
        ..req.clone()
    };
    let budget = OKX.orders.admitted();
    let _ = rest.submit_order(&req);
    let _ = rest.submit_order(&stop);
    let _ = rest.cancel_order("c1");
    let _ = rest.modify_order("c1", Some(2.0), None);
    let _ = rest.set_leverage();
    let used = 5;
    for _ in 0..(budget - used) {
        assert!(gate.try_proceed());
    }
    assert!(
        !gate.try_proceed(),
        "submit, stop, cancel, amend and set_leverage took exactly {used} of {budget} slots"
    );
}

/// The gap-sentinel this adapter arms after the last command is the 5 s it armed when the number
/// was a private `FILL_SENTINEL` here: it is `vike_bridge_core::exec_actor`'s venue-free default now
/// (decision 0160), so a change to that default is a deliberate edit to every command-driven venue.
#[test]
fn the_gap_sentinel_is_the_five_seconds_it_always_was() {
    assert_eq!(DEFAULT_FILL_SENTINEL, Duration::from_secs(5));
}
