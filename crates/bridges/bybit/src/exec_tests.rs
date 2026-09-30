//! Bybit-specific tests: the live per-symbol properties fetch (ignored; network) and the PIT
//! properties recording. The generic `run_loop` command→event tests (mock `VenueRest`) moved
//! WITH `run_loop` to `vike_bridge_core::exec_actor::run_loop_tests`; the bybit reality-tie
//! (`caps_for("bybit").supports_modify`) lives in the lib `caps_test`.
use super::*;

/// **THE EXEC REFUSAL, on the venue's own word.** A linear perpetual trades; every other
/// contract type the venue can answer with is refused, and the message tells the operator which
/// half of the instrument works.
///
/// ⚠ The `XRPUSD`/`InversePerpetual` pair is not hypothetical: MEASURED 2026-09-16,
/// `instruments-info?category=linear&symbol=XRPUSD` ANSWERS with exactly that row. The fetch
/// succeeding on a wrong-category request is the whole reason a refusal has to exist here.
#[test]
fn only_a_linear_perpetual_may_be_traded_by_this_adapter() {
    assert_eq!(non_linear_perpetual_refusal("BTCUSDT", Some("LinearPerpetual")), None);

    for (symbol, kind) in [
        ("XRPUSD", "InversePerpetual"),
        ("BTCUSDZ26", "InverseFutures"),
        ("BTCUSDT-25JUL26", "LinearFutures"),
    ] {
        let why = non_linear_perpetual_refusal(symbol, Some(kind))
            .unwrap_or_else(|| panic!("{kind} must be refused"));
        assert!(why.contains(symbol), "the message must name the symbol: {why}");
        assert!(why.contains(kind), "...and the venue's own word for it: {why}");
        assert!(
            why.contains("category") && why.contains("linear"),
            "...and WHY, which is the literal this adapter has not threaded: {why}"
        );
        assert!(
            why.contains("charts") || why.contains("backfill"),
            "...and which half of the instrument DOES work, since the picker offers it: {why}"
        );
    }
}

/// An ABSENT `contractType` is NOT a refusal. That is the shape of a row the venue changed or a
/// fetch that failed, and refusing every order on a missing field would take a working linear
/// mount off the venue for a parse miss — a fetch failure already falls back to the caller's
/// default grid, exactly as it did before this refusal existed.
#[test]
fn a_missing_contract_type_is_not_a_refusal() {
    assert_eq!(non_linear_perpetual_refusal("BTCUSDT", None), None);
}

/// The refusing actor obeys the venue-adapter contract it replaces: the emitter split
/// (`OrderSubmitted` synchronously) and ONE terminal event carrying the reason. *"No order may
/// silently vanish"* is the rule, and a refusal is the easiest place in the tree to break it.
#[test]
fn a_refused_order_gets_a_terminal_rejection_carrying_the_reason() {
    let why = non_linear_perpetual_refusal("XRPUSD", Some("InversePerpetual")).expect("refused");
    let rest = RefusingRest { why: why.clone() };
    let req = OrderRequest {
        client_order_id: "coid-1".into(),
        symbol: "XRPUSD".into(),
        ..Default::default()
    };
    let events = rest.submit_order(&req);
    assert_eq!(events.len(), 2, "submitted + exactly one terminal: {events:?}");
    assert!(matches!(events[0], Event::OrderSubmitted(_)), "{events:?}");
    match &events[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id.as_str(), "coid-1");
            assert_eq!(
                r.reason.as_str(),
                why.as_str(),
                "the operator reads the same sentence twice"
            );
        }
        other => panic!("expected a terminal rejection, got {other:?}"),
    }
    // A cancel of something that never rested is NOT a second failure — "unknown != rejection".
    assert!(rest.cancel_order("coid-1").is_ok());
}

/// The leverage this arm POSTs to `/v5/position/set-leverage` is the OPERATOR's
/// `[risk] max_leverage`, and an unset profile still yields the historical literal (`2.0`) —
/// the property that makes this change safe to ship onto a live account. NO network:
/// `leverage_for` is the pure decision `make_engine` calls once, before anything is spawned.
#[test]
fn perp_leverage_follows_the_operator_risk_budget() {
    // Unset — both shapes of "the operator said nothing" — keeps today's literal EXACTLY.
    assert_eq!(leverage_for(None), DEFAULT_LEVERAGE);
    assert_eq!(leverage_for(None), 2.0, "the historical hardcoded set-leverage value");
    let silent = vike_exec::ProfileRisk { max_leverage: None, ..Default::default() };
    assert_eq!(leverage_for(Some(&silent)), 2.0);

    // Set — the venue account is configured with the operator's own number, in BOTH
    // directions (the two ways the old hardcode was dangerous).
    let ten = vike_exec::ProfileRisk { max_leverage: Some(10.0), ..Default::default() };
    assert_eq!(leverage_for(Some(&ten)), 10.0);
    let one = vike_exec::ProfileRisk { max_leverage: Some(1.0), ..Default::default() };
    assert_eq!(leverage_for(Some(&one)), 1.0);

    // ONE number governs both sides: what we POST is the reciprocal of the initial-margin
    // fraction the RiskGate sizes against.
    assert_eq!(ten.im_requirement(), Some(0.1));
    assert_eq!(ten.im_requirement(), Some(1.0 / leverage_for(Some(&ten))));
    // The same invariant, via the shared helper the rule itself ships.
    assert!(vike_bridge_core::leverage::agrees_with_im_requirement(&ten, DEFAULT_LEVERAGE));
}

/// LIVE proof the startup fetch is REAL + per-symbol (not the hardcoded fallback): BTCUSDT and
/// ETHUSDT have different venue grids, so the fetched ticks must differ. Read-only (no orders).
#[test]
#[ignore = "network + demo creds — run manually"]
fn fetch_returns_real_per_symbol_properties() {
    use vike_bridge_core::credentials::{
        Environment, load_credentials_from, load_workspace_dotenv,
    };
    vike_log::test_init();
    let vars = load_workspace_dotenv();
    let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
        return; // no creds → skip
    };
    // `false` = the demo host, explicitly — this is a demo-creds smoke, so it must never be
    // silently upgraded to the mainnet grid by anything else in the process environment. (Decision
    // 0095 deleted `BYBIT_MAINNET`, the variable this comment used to name here; the explicit `false`
    // is unaffected — it was never derived from that flag.)
    let btc = fetch_bybit_properties(&creds, "BTCUSDT", false).expect("BTCUSDT instruments-info");
    let eth = fetch_bybit_properties(&creds, "ETHUSDT", false).expect("ETHUSDT instruments-info");
    tracing::info!(
        target: "vike_bybit",
        "fetched BTC tick={} step={} | ETH tick={} step={}",
        btc.tick_size, btc.step_size, eth.tick_size, eth.step_size
    );
    assert!(btc.tick_size > 0.0 && eth.tick_size > 0.0);
    assert_ne!(
        btc.tick_size, eth.tick_size,
        "per-symbol fetch must differ (BTC {} vs ETH {}) — else it's the fallback",
        btc.tick_size, eth.tick_size
    );
}

/// PIT filter recording (task 4): `record_properties` writes the REAL fetched grid into the store
/// when a recorder is present, keyed by venue `"bybit"`. Uses the DataFusion-free `MemHistStore`
/// test double (vike-data's `test-support` dev-feature) so this venue's default build never pulls
/// DataFusion in.
#[test]
fn bybit_records_fetched_properties_when_recorder_present() {
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
        "bybit",
        "BTCUSDT",
        f,
        1_577_836_800_000_000_000,
    );
    assert_eq!(
        store.scan_symbol_properties("bybit", "BTCUSDT", vike_data::TsRange::all()).unwrap(),
        vec![(1_577_836_800_000i64, f)]
    );
}

#[test]
fn bybit_no_recorder_writes_nothing() {
    use std::sync::Arc;
    use vike_data::HistStore;
    let store = Arc::new(vike_data::MemHistStore::new());
    vike_data::PropertiesRecorder::record_opt(
        &None,
        "bybit",
        "BTCUSDT",
        SymbolProperties::default(),
        1_577_836_800_000_000_000,
    );
    assert!(
        store
            .scan_symbol_properties("bybit", "BTCUSDT", vike_data::TsRange::all())
            .unwrap()
            .is_empty()
    );
}

/// A fake [`BybitTransport`] that answers `/v5/account/transaction-log` with a canned
/// SETTLEMENT row (or an empty list) — the offline twin of the live `BybitFundingPoller`
/// smoke, proving `poll_funding_once`'s spawn glue (not just the pure decoder, which
/// `r6_bybit_parity.rs` already pins).
struct FakeFundingTransport {
    rows: Vec<serde_json::Value>,
    /// When `Some`, assert the query carries this exact `startTime` (the floor optimization
    /// param); `None` asserts the param is ABSENT (a floorless probe's query, unchanged).
    expect_start_time: Option<String>,
}

impl BybitTransport for FakeFundingTransport {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
        assert_eq!(path, crate::funding::LOG_PATH);
        let start = params.iter().find(|(k, _)| *k == "startTime").map(|(_, v)| v.clone());
        match &self.expect_start_time {
            Some(want) => assert_eq!(start, Some(serde_json::json!(want))),
            None => assert_eq!(start, None, "a floorless poll must not send startTime"),
        }
        Ok(serde_json::json!({"retCode": 0, "retMsg": "OK", "result": {"list": self.rows}}))
    }
}

fn settlement_row(id: &str, funding: &str, ts: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "type": "SETTLEMENT", "symbol": "BTCUSDT",
        "funding": funding, "feeRate": "0.0001", "transactionTime": ts
    })
}

/// The spawn glue's one poll iteration forwards decoded `Event::Funding`s onto the core
/// ingest lane — the path a background poll thread drives every cadence.
#[test]
fn poll_funding_once_forwards_decoded_events() {
    let rest = BybitPerpRest {
        signer: BybitV5Signer::new(
            &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
            || 0,
        ),
        transport: FakeFundingTransport {
            rows: vec![settlement_row("r1", "1.25", "1000")],
            expect_start_time: None,
        },
        base_url: "http://example.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
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
            assert_eq!(f.venue, "bybit");
            assert_eq!(f.symbol, "BTCUSDT");
            assert_eq!(f.amount, 1.25, "received-positive, no sign flip");
        }
        other => panic!("expected one Event::Funding, got {other:?}"),
    }

    // A second poll with the SAME row (as a real re-fetch might return before settling)
    // yields nothing new — the dedup-by-id guard the live poller already owns.
    let more = poll_funding_once(&mut poller, &events, &mut streak);
    assert!(more.is_ok());
    assert!(ingest.try_recv().is_err(), "the already-seen row must not re-emit");
}

/// The restart law: a poller floored at its spawn instant (as `spawn_funding_poll` does) must
/// NEVER emit a settlement that posted BEFORE the floor — those are already embedded in the
/// authoritative venue balance — while a settlement AT/after the floor is emitted exactly once.
/// Simulates the (re)start-with-empty-seen-set case the in-memory id-dedup cannot cover.
#[test]
fn poll_funding_skips_settlements_before_the_spawn_floor() {
    const FLOOR: i64 = 5_000;
    let rest = BybitPerpRest {
        signer: BybitV5Signer::new(
            &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
            || 0,
        ),
        transport: FakeFundingTransport {
            // The venue answers with history straddling the floor (as the default ~7d window
            // would on a restart): pre-floor, exactly-at-floor, and post-floor rows.
            rows: vec![
                settlement_row("old", "9.99", "1000"),
                settlement_row("edge", "0.25", "5000"),
                settlement_row("new", "1.25", "9000"),
            ],
            // The floor also rides as the startTime request param (the optimization).
            expect_start_time: Some(FLOOR.to_string()),
        },
        base_url: "http://example.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", FLOOR);
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
            assert_eq!(new.amount, 1.25);
        }
        other => panic!("expected exactly the at/post-floor settlements, got {other:?}"),
    }

    // The next cadence re-serves the same window: nothing re-emits (id-dedup) and the
    // pre-floor row stays excluded.
    poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
    assert!(ingest.try_recv().is_err(), "no duplicates on the second poll");
}

/// When the core is gone (ingest receiver dropped), the poll glue reports `Err(CoreGone)` so
/// its background thread's loop can self-exit instead of polling forever into a void.
#[test]
fn poll_funding_once_reports_core_gone() {
    let rest = BybitPerpRest {
        signer: BybitV5Signer::new(
            &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
            || 0,
        ),
        transport: FakeFundingTransport {
            rows: vec![settlement_row("r2", "0.5", "1000")],
            expect_start_time: None,
        },
        base_url: "http://example.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
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
    impl BybitTransport for FailingTransport {
        fn signed(
            &self,
            _base_url: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, serde_json::Value)],
            _signer: &BybitV5Signer,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            Err(vike_bridge_core::transport::VenueApiError {
                code: 0,
                msg: "connect refused".to_string(),
            })
        }
    }
    let rest = BybitPerpRest {
        signer: BybitV5Signer::new(
            &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
            || 0,
        ),
        transport: FailingTransport,
        base_url: "http://example.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
    let (events, mut ingest) = vike_exec::event_channel(8);
    let mut streak = 0u32;
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
    assert_eq!(streak, 2, "consecutive failures accumulate");
    assert!(ingest.try_recv().is_err(), "failures emit nothing");
}
