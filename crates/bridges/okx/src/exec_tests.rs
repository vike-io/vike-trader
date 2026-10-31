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

/// A venue that answers `account/bills` the way the real one narrows it: an `instId` parameter returns
/// only that instrument's rows, none returns every instrument's. It RECORDS each request's `instId`,
/// because which query went out is half of what the admitted-instrument fold claims.
struct AccountBills {
    rows: std::sync::Mutex<Vec<serde_json::Value>>,
    asked: std::sync::Mutex<Vec<Option<String>>>,
}

impl OkxTransport for AccountBills {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
        assert_eq!(path, crate::perp::PATH_BILLS);
        let inst = params
            .iter()
            .find(|(k, _)| *k == "instId")
            .and_then(|(_, v)| v.as_str().map(str::to_string));
        self.asked.lock().unwrap().push(inst.clone());
        let rows: Vec<_> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| inst.as_deref().is_none_or(|i| r["instId"] == i))
            .cloned()
            .collect();
        Ok(serde_json::json!({"code": "0", "msg": "", "data": rows}))
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

fn bill_of(inst: &str, bill_id: &str, pnl: &str) -> serde_json::Value {
    serde_json::json!({
        "billId": bill_id, "type": "8", "instId": inst, "pnl": pnl, "balChg": pnl, "ts": "9000"
    })
}

/// **The funding of an ADMITTED SWAP folds** (known-limitations "Funding settled on a SWAP other than the
/// mounted one"). With nothing admitted the poll is the mounted-only request it always was; once the engine
/// admits ETH it is ONE account-wide read, and the fold takes the mounted instrument's bills and the
/// admitted one's, never an instrument the engine did not admit (SOL). ETH's backlog (what settled before
/// the poll that first saw it admitted) is marked seen and NOT folded; its next settlement is.
#[test]
fn funding_of_an_admitted_swap_folds_from_the_poll_after_it_is_seen() {
    const BTC: &str = "BTC-USDT-SWAP";
    const ETH: &str = "ETH-USDT-SWAP";
    const SOL: &str = "SOL-USDT-SWAP";
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: AccountBills {
            rows: std::sync::Mutex::new(vec![
                bill_of(BTC, "b1", "0.10"),
                bill_of(ETH, "e0", "0.20"),
                bill_of(SOL, "s0", "0.30"),
            ]),
            asked: std::sync::Mutex::new(Vec::new()),
        },
        base_url: "http://example.invalid".to_string(),
        symbol: BTC.to_string(),
        properties: SymbolProperties::default(),
        ct_val: 0.0,
        leverage: DEFAULT_LEVERAGE,
        broker_code: None,
    };
    let admitted = Arc::<AdmittedSymbols>::default();
    let mut poller = OkxFundingPoller::new(&rest, BTC, 0).with_admitted(admitted.clone());
    let folded = |events: Vec<Event>| -> Vec<(String, f64)> {
        events
            .into_iter()
            .map(|e| match e {
                Event::Funding(f) => (f.symbol.to_string(), f.amount),
                other => panic!("only funding events are expected, got {other:?}"),
            })
            .collect()
    };
    let asked = || rest.transport.asked.lock().unwrap().clone();

    // Nothing admitted: the mounted-only request, the mounted instrument's bill.
    assert_eq!(folded(poller.poll().unwrap()), vec![(BTC.to_string(), 0.10)]);
    assert_eq!(asked(), vec![Some(BTC.to_string())]);

    // ETH admitted: ONE account-wide read. ETH's backlog `e0` is marked seen, not folded; SOL never is.
    assert!(admitted.admit(ETH, BTC));
    assert!(
        folded(poller.poll().unwrap()).is_empty(),
        "the backlog of a new admission is not folded"
    );
    assert_eq!(asked().last(), Some(&None), "no instId: the account's SWAP bills in one read");

    // The next settlements: ETH's folds (admitted and already seen), SOL's does not, BTC's still does.
    rest.transport.rows.lock().unwrap().extend([
        bill_of(ETH, "e1", "0.40"),
        bill_of(SOL, "s1", "0.50"),
        bill_of(BTC, "b2", "0.60"),
    ]);
    assert_eq!(
        folded(poller.poll().unwrap()),
        vec![(ETH.to_string(), 0.40), (BTC.to_string(), 0.60)]
    );
    // Nothing re-emits, and SOL stays out however often it is read.
    assert!(folded(poller.poll().unwrap()).is_empty());
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

// ---- the inverse-swap refusal (decision 0170, plan S0.1) ---------------------------------------

/// The REAL fixture row of `BTC-USD-SWAP` (ctType `inverse`, `ctVal` 100, `ctValCcy` USD) reaches the
/// exec parse unguarded today: the refusal reads its `ctType` off the SAME row the grid comes from,
/// and a linear row (`BTC-USDT-SWAP`) is untouched.
#[test]
fn the_real_inverse_swap_row_is_refused_and_a_linear_one_is_not() {
    let body: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/instruments/swap.json"))
            .expect("fixture is JSON");
    let data = serde_json::json!({ "data": body["data"].clone() });
    let rows = parse_okx_perp_instruments(&data);
    let inverse = rows.get("BTC-USD-SWAP").expect("the fixture lists BTC-USD-SWAP");
    assert_eq!(inverse.contract_type.as_deref(), Some("inverse"));
    let why =
        inverse_swap_refusal("BTC-USD-SWAP", Some(inverse)).expect("an inverse swap is refused");
    assert!(why.contains("BTC-USD-SWAP") && why.contains("inverse"), "{why}");
    let linear = rows.get("BTC-USDT-SWAP").expect("the fixture lists BTC-USDT-SWAP");
    assert_eq!(linear.contract_type.as_deref(), Some("linear"));
    assert_eq!(inverse_swap_refusal("BTC-USDT-SWAP", Some(linear)), None);
}

/// An ABSENT `ctType` on a fetched row is not refused (a parse miss must not take a working linear
/// start off the venue); a row that could NOT be fetched IS (fail closed: the kind is unknown).
#[test]
fn an_absent_contract_type_is_not_refused_but_an_absent_row_is() {
    let row = OkxInstrument::default();
    assert_eq!(inverse_swap_refusal("BTC-USDT-SWAP", Some(&row)), None);
    let why = inverse_swap_refusal("BTC-USD-SWAP", None).expect("no row, no kind, no start");
    assert!(why.contains("could not be fetched"), "{why}");
}

/// The refusing client owes the contract: `OrderSubmitted` then a TERMINAL `OrderRejected` carrying
/// the reason; a cancel of what never rested is `Ok`.
#[test]
fn the_refusing_client_rejects_every_order_with_the_reason() {
    let rest = RefusingRest { why: "no inverse here".to_string() };
    let order = OrderRequest { client_order_id: "c1".into(), ..Default::default() };
    let events = rest.submit_order(&order);
    assert!(matches!(events[0], Event::OrderSubmitted(_)));
    match &events[1] {
        Event::OrderRejected(r) => assert_eq!(r.reason.as_str(), "no inverse here"),
        other => panic!("{other:?}"),
    }
    assert!(rest.cancel_order("c1").is_ok());
}

fn fixture_rows() -> indexmap::IndexMap<String, OkxInstrument> {
    let body: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/instruments/swap.json"))
            .expect("fixture is JSON");
    parse_okx_perp_instruments(&serde_json::json!({ "data": body["data"].clone() }))
}

fn foreign_creds() -> Credentials {
    Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    }
}

/// A foreign instrument's row builds a client on THAT instrument's `instId`, grid and `ctVal`, on a
/// rate window of its own (the venue meters 60 per 2 s per instrument, so two instruments must not
/// share one), and registers the `ctVal` its fills will be converted with BEFORE any order is sent.
#[test]
fn a_foreign_row_builds_a_client_on_its_own_grid_contract_size_and_rate_window() {
    let rows = fixture_rows();
    let sizes = ContractSizes::new("ETH-USDT-SWAP", 0.1);
    let btc = foreign_rest(
        &foreign_creds(),
        "BTC-USDT-SWAP",
        rows.get("BTC-USDT-SWAP").cloned(),
        &sizes,
        3.0,
        Some("broker".to_string()),
        true,
    )
    .expect("a linear swap row is signable");
    assert_eq!(btc.symbol, "BTC-USDT-SWAP");
    assert_eq!(btc.ct_val, 0.01);
    assert_eq!(btc.properties.tick_size, 0.1);
    assert_eq!(btc.properties.step_size, 0.01);
    assert_eq!(
        btc.broker_code.as_deref(),
        Some("broker"),
        "attribution rides the foreign orders too"
    );
    assert_eq!(sizes.get_or_load("BTC-USDT-SWAP", || panic!("registered")), Some(0.01));

    let other = foreign_rest(
        &foreign_creds(),
        "BTC-USDT-SWAP",
        rows.get("BTC-USDT-SWAP").cloned(),
        &sizes,
        3.0,
        None,
        true,
    )
    .unwrap();
    let (a, b) = (btc.transport.rate_gate().unwrap(), other.transport.rate_gate().unwrap());
    for _ in 0..vike_venues::venue_rate_limits::OKX.orders.admitted() {
        assert!(a.try_proceed());
    }
    assert!(!a.try_proceed(), "the first client's window is spent");
    assert!(b.try_proceed(), "the second client's window is its own");
}

/// Every way a foreign instrument may NOT be signed ends in an `Err` carrying the reason: no row
/// (unknown, not a SWAP, unreachable), an inverse swap (a contract counted in USD), and a row with
/// no usable `ctVal`/lot/tick (it would size every order 0). Nothing is registered for any of them.
#[test]
fn a_foreign_instrument_without_a_usable_linear_row_is_refused() {
    let rows = fixture_rows();
    let sizes = ContractSizes::new("BTC-USDT-SWAP", 0.01);
    let refuse = |inst: &str, row: Option<OkxInstrument>| match foreign_rest(
        &foreign_creds(),
        inst,
        row,
        &sizes,
        2.0,
        None,
        true,
    ) {
        Err(why) => why,
        Ok(_) => panic!("{inst} must be refused"),
    };
    let why = refuse("DOGE-USDT-SWAP", None);
    assert!(why.contains("DOGE-USDT-SWAP") && why.contains("SWAP only"), "{why}");
    let why = refuse("BTC-USD-SWAP", rows.get("BTC-USD-SWAP").cloned());
    assert!(why.contains("inverse"), "{why}");
    let mut no_ct = rows["BTC-USDT-SWAP"].clone();
    no_ct.ct_val = 0.0;
    assert!(refuse("BTC-USDT-SWAP", Some(no_ct)).contains("no usable ctVal"));
    let mut no_lot = rows["BTC-USDT-SWAP"].clone();
    no_lot.properties.step_size = 0.0;
    assert!(refuse("X-USDT-SWAP", Some(no_lot)).contains("no usable ctVal"));
    assert_eq!(sizes.get_or_load("BTC-USD-SWAP", || None), None, "a refused row registers nothing");
}

/// A row whose contract multiplier is not 1 is refused: the wire `sz` divides by `ctVal` alone, so
/// a contract worth `ctVal x ctMult` would be mis-sized by the multiplier. (Every SWAP lists 1; a
/// listing that changed would be a loud refusal, not a quiet mis-size.)
#[test]
fn a_foreign_instrument_with_a_contract_multiplier_is_refused() {
    let sizes = ContractSizes::new("BTC-USDT-SWAP", 0.01);
    let mut row = fixture_rows()["BTC-USDT-SWAP"].clone();
    row.ct_mult = 10.0;
    match foreign_rest(&foreign_creds(), "X-USDT-SWAP", Some(row), &sizes, 2.0, None, true) {
        Err(why) => assert!(why.contains("ctMult"), "{why}"),
        Ok(_) => panic!("a ctMult of 10 must be refused"),
    }
    assert_eq!(sizes.get_or_load("X-USDT-SWAP", || None), None, "nothing registered");
}

// ---- the spot pair and the dated future (decision 0232) -----------------------------------------

/// One row of the measured class fixture (`tests/fixtures/classes/`), by the key it is filed under. The
/// future's row is the measured `QTUM-USDT-261225`; the tests below sign it under an id delivered in 2099
/// (`QTUM-USDT-991231`), so that none of them is a clock bomb at the real delivery.
fn class_row(key: &str) -> OkxInstrument {
    let all: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/classes/demo_instruments_2026-10-30.json"
    ))
    .expect("fixture is JSON");
    let row = all[key].clone();
    let id = row["instId"].as_str().expect("instId").to_string();
    parse_okx_perp_instruments(&serde_json::json!({ "data": [row] }))[&id].clone()
}

fn sign(inst: &str, row: Option<OkxInstrument>) -> Result<OkxPerpRest<UreqOkxTransport>, String> {
    let sizes = ContractSizes::new("BTC-USDT-SWAP", 0.01);
    foreign_rest(&foreign_creds(), inst, row, &sizes, 2.0, None, true)
}

/// The measured USDT-quoted spot pair and USDT-linear future build clients on their own grid and
/// contract size: one unit of a spot pair's `sz` is one coin, a future's contract is `ctVal` coins.
#[test]
fn a_spot_pair_and_a_dated_future_build_clients_on_their_own_rows() {
    let sizes = ContractSizes::new("BTC-USDT-SWAP", 0.01);
    let spot = foreign_rest(
        &foreign_creds(),
        "BTC-USDT",
        Some(class_row("spot")),
        &sizes,
        2.0,
        None,
        true,
    )
    .expect("a USDT-quoted live spot pair is signable");
    assert_eq!((spot.symbol.as_str(), spot.ct_val), ("BTC-USDT", 1.0));
    assert_eq!(spot.properties.step_size, 0.00000001);
    assert_eq!(sizes.get_or_load("BTC-USDT", || panic!("registered")), Some(1.0));
    let future = foreign_rest(
        &foreign_creds(),
        "QTUM-USDT-991231",
        Some(class_row("future")),
        &sizes,
        2.0,
        None,
        true,
    )
    .expect("a USDT-linear live future not near its delivery is signable");
    assert_eq!((future.symbol.as_str(), future.ct_val), ("QTUM-USDT-991231", 10.0));
}

/// Every way a spot pair or a future may NOT be signed ends in an `Err` that says why, and registers nothing.
#[test]
fn a_spot_pair_or_a_future_the_class_rules_refuse_is_refused_in_words() {
    // quoted in another asset: the engine's ledger is USDT
    let mut usdc = class_row("spot");
    usdc.quote_ccy = Some("USDC".to_string());
    let why = sign("BTC-USDC", Some(usdc)).err().expect("refused");
    assert!(why.contains("quoted in USDC") && why.contains("ledger is USDT"), "{why}");
    // not live on this network
    let mut suspended = class_row("spot");
    suspended.state = Some("suspend".to_string());
    assert!(sign("BTC-USDT", Some(suspended)).err().expect("refused").contains("not live"));
    // a name that lies about its class: a future's row under a spot-shaped id, and the reverse
    let why = sign("BTC-USDT", Some(class_row("future"))).err().expect("refused");
    assert!(why.contains("reads as a SPOT instrument") && why.contains("says FUTURES"), "{why}");
    let why = sign("QTUM-USDT-991231", Some(class_row("spot"))).err().expect("refused");
    assert!(why.contains("reads as a FUTURES instrument") && why.contains("says SPOT"), "{why}");
    // a row that states no instType cannot be told from a SWAP's: only a SWAP row may omit it
    let mut untyped = class_row("spot");
    untyped.inst_type = None;
    assert!(sign("BTC-USDT", Some(untyped)).err().expect("refused").contains("no instType"));
    // a future settled in USDC, an inverse one, and one already delivered
    let mut usdc_future = class_row("future");
    usdc_future.settle_ccy = Some("USDC".to_string());
    assert!(
        sign("QTUM-USDT-991231", Some(usdc_future))
            .err()
            .expect("refused")
            .contains("settles in USDC")
    );
    let mut inverse = class_row("future");
    inverse.contract_type = Some("inverse".to_string());
    assert!(sign("QTUM-USDT-991231", Some(inverse)).err().expect("refused").contains("inverse"));
    let why = sign("QTUM-USDT-200101", Some(class_row("future"))).err().expect("refused");
    assert!(why.contains("delivered"), "{why}");
    // an option's row under an option-shaped id is none of the three (the id reads as a SWAP and the row says OPTION)
    let why = sign("BTC-USD-261106-76000-C", Some(class_row("option"))).err().expect("refused");
    assert!(why.contains("says OPTION"), "{why}");
    // no row at all: named by the class the id reads as
    assert!(sign("BTC-USDT", None).err().expect("refused").contains("lists no SPOT row"));
    assert!(
        sign("QTUM-USDT-991231", None).err().expect("refused").contains("lists no FUTURES row")
    );
}

/// The inverse swap stays refused, with the measurement behind it in the words.
#[test]
fn the_inverse_refusal_carries_the_demo_measurement() {
    let why = sign("BTC-USD-SWAP", Some(class_row("inverse_swap"))).err().expect("refused");
    assert!(why.contains("inverse") && why.contains("USDT balance did not move"), "{why}");
}

/// A fill of a spot pair or a future the engine never admitted is not sized (the `orders` channel is
/// subscribed for every instrument class: a coin traded by hand in the OKX app must not reach the
/// engine), and the answer is given before any request is made.
#[test]
fn a_fill_of_an_unadmitted_spot_pair_or_future_is_not_sized() {
    let admitted = AdmittedSymbols::default();
    for inst in ["BTC-USDT", "QTUM-USDT-991231", "btc-usdt"] {
        assert!(!fill_may_be_sized(inst, &admitted), "{inst}");
        assert_eq!(
            load_linear_ct_val(inst, true, &admitted),
            None,
            "{inst}: answered before any request"
        );
    }
    // a SWAP is sized as it always was, admitted or not
    assert!(fill_may_be_sized("ETH-USDT-SWAP", &admitted));
    // once the engine admits them (any case), they are
    for inst in ["BTC-USDT", "QTUM-USDT-991231"] {
        admitted.admit(inst, "BTC-USDT-SWAP");
        assert!(fill_may_be_sized(&inst.to_lowercase(), &admitted), "{inst}");
    }
}

// ---- admitted symbols (decision 0174): restored orders, and a bounded foreign row read ----------

/// The core announces every restored order's `(coid, symbol)` at boot and after a gatewayhub restart
/// (`ExecutionClient::adopt_restored`): the router learns where each rests, so a cancel of an order
/// this process never submitted goes to ITS instrument (not the mounted one, where okx answers
/// `51401` and the cancel reads "already gone" over a live order).
#[test]
fn adopt_restored_teaches_the_router_where_each_restored_order_rests() {
    let coids = Arc::new(CoidSymbols::default());
    let (events, _ingest) = vike_exec::event_channel(8);
    let actor = ExecActor::spawn("okx-adopt-test", events, |rx| {
        for cmd in rx {
            if matches!(cmd, ExecCommand::Shutdown) {
                break;
            }
        }
    });
    let mut client = OkxExecutionClient(actor, coids.clone());
    client.adopt_restored(&[
        ("c-eth".to_string(), "ETH-USDT-SWAP".to_string()),
        ("c-unknown".to_string(), String::new()),
    ]);
    assert_eq!(coids.symbol_of("c-eth").as_deref(), Some("ETH-USDT-SWAP"));
    assert_eq!(
        coids.symbol_of("c-unknown"),
        None,
        "an announced order with no symbol teaches nothing"
    );
    // announced again by a later pass: idempotent
    client.adopt_restored(&[("c-eth".to_string(), "ETH-USDT-SWAP".to_string())]);
    assert_eq!(coids.symbol_of("c-eth").as_deref(), Some("ETH-USDT-SWAP"));
}

/// The foreign-instrument row read (the factory's, and the one a fill with no client triggers on
/// the user-data pump) is bounded by `FOREIGN_FETCH_TIMEOUT`, not the 30 s order agent: a venue that
/// accepts the connection and says nothing must not hold the exec thread or the pump for 30 s.
#[test]
fn a_foreign_row_read_gives_up_quickly_on_a_silent_venue() {
    assert!(FOREIGN_FETCH_TIMEOUT <= Duration::from_secs(10), "{FOREIGN_FETCH_TIMEOUT:?}");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let silent = thread::spawn(move || {
        let held = listener.accept();
        thread::sleep(Duration::from_millis(1500));
        drop(held);
    });
    let transport = UreqOkxTransport::with_timeout(true, Duration::from_millis(300));
    let started = std::time::Instant::now();
    assert!(fetch_row_on(&transport, &base, "ETH-USDT-SWAP").is_none());
    assert!(started.elapsed() < Duration::from_millis(1400), "{:?}", started.elapsed());
    silent.join().expect("silent listener");
}

/// **An instrument admitted on demand is written through the bridge's OWN set-leverage verb.** The
/// client is the order client's build pointed at the ADMITTED instId (upper-cased) under the tier's
/// simulated-trading switch, the POST names that instrument, `cross`, and the figure as a whole
/// number, and a refusal keeps the venue's words.
#[test]
fn an_instrument_admitted_on_demand_is_written_through_the_bridges_own_set_leverage() {
    use std::cell::RefCell;

    /// `(method, path, params)` of every call.
    type Sent = Vec<(String, String, Vec<(String, serde_json::Value)>)>;
    struct Wire {
        sent: RefCell<Sent>,
        answer: serde_json::Value,
    }
    impl OkxTransport for Wire {
        fn signed(
            &self,
            _base_url: &str,
            path: &str,
            method: &str,
            params: &[(&str, serde_json::Value)],
            _signer: &OkxV5Signer,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            let params = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
            self.sent.borrow_mut().push((method.to_string(), path.to_string(), params));
            Ok(self.answer.clone())
        }
        fn public(
            &self,
            _base_url: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            unreachable!("a set-leverage write reads nothing public")
        }
    }
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };

    // The client: the admitted instrument, the figure, the order window, the tier's header.
    let demo = symbol_leverage_rest(&creds, true, "eth-usdt-swap", 10.0);
    assert_eq!((demo.symbol.as_str(), demo.leverage), ("ETH-USDT-SWAP", 10.0));
    assert_eq!(demo.base_url, REST);
    assert!(demo.transport.rate_gate().is_some(), "the write rides the order window");

    let over = |answer: serde_json::Value| OkxPerpRest {
        signer: OkxV5Signer::new(&creds, || 0),
        transport: Wire { sent: RefCell::new(Vec::new()), answer },
        base_url: REST.to_string(),
        symbol: "ETH-USDT-SWAP".to_string(),
        properties: SymbolProperties::default(),
        ct_val: 0.0,
        leverage: 10.0,
        broker_code: None,
    };

    // A clean write: ONE POST, cross, a whole-number lever, naming the admitted instrument.
    let rest = over(serde_json::json!({ "code": "0", "msg": "", "data": [{}] }));
    assert_eq!(post_symbol_leverage(&rest), Ok(()));
    let sent = rest.transport.sent.borrow();
    assert_eq!(sent.len(), 1, "one write");
    assert_eq!((sent[0].0.as_str(), sent[0].1.as_str()), ("POST", crate::perp::PATH_SET_LEVERAGE));
    let param = |name: &str| sent[0].2.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    assert_eq!(param("instId"), Some(serde_json::json!("ETH-USDT-SWAP")));
    assert_eq!(param("lever"), Some(serde_json::json!("10")));
    assert_eq!(param("mgnMode"), Some(serde_json::json!("cross")));
    drop(sent);

    // A refusal is the venue's words and the instrument; okx swallows nothing without a demo proof.
    let refusal = serde_json::json!({ "code": "59000", "msg": "Settings cannot be changed" });
    let why = post_symbol_leverage(&over(refusal)).unwrap_err();
    assert!(matches!(why, LeverageWriteError::Refused(_)), "{why:?}");
    let why = why.to_string();
    assert!(why.contains("ETH-USDT-SWAP") && why.contains("Settings cannot be changed"), "{why}");

    // 50004 "Endpoint request timeout" is the venue saying the outcome is unknown: AMBIGUOUS (the POST may
    // have landed), so the lane reads the venue back instead of reporting a refusal.
    let timeout = serde_json::json!({ "code": "50004", "msg": "Endpoint request timeout" });
    let got = post_symbol_leverage(&over(timeout)).unwrap_err();
    assert!(matches!(got, LeverageWriteError::Ambiguous(_)), "{got:?}");
    assert!(got.to_string().contains("ETH-USDT-SWAP"), "{got}");

    // The verb is guarded HERE: a real-network (`live`) account is refused before any client or request
    // exists, whatever `LEVERAGE_VENUES` says (`write_symbol_leverage` calls this first; the write itself
    // needs the network, so only the guard is run).
    let refused = demo_only(false, "ETH-USDT-SWAP").unwrap_err();
    assert!(matches!(refused, LeverageWriteError::Refused(_)), "{refused:?}");
    assert!(refused.to_string().contains("demo account only"), "{refused}");
    assert_eq!(demo_only(true, "ETH-USDT-SWAP"), Ok(()), "a simulated (demo) account passes");
}
