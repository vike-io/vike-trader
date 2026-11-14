//! Bybit-specific tests: the live per-symbol properties fetch (ignored; network) and the PIT
//! properties recording. The generic `run_loop` command→event tests (mock `VenueRest`) moved
//! WITH `run_loop` to `vike_bridge_core::exec_actor::run_loop_tests`; the bybit reality-tie
//! (`capabilities_for("bybit").supports_modify`) lives in the lib `capabilities_test`.
use super::*;
use std::assert_matches;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_execution::ExecutionClient;
use vike_model::OrderRequest;

/// **THE EXEC REFUSAL, on the venue's own word.** A linear perpetual and a DATED linear future trade (the same
/// `category=linear` wire, decision 0231), and so does a coin-margined perpetual or dated future (the bybit
/// inverse amendment of decision 0258: the mount signs in `inverse`). A word this plane signs no book for is
/// refused, and the message tells the operator which half of the instrument works.
///
/// Base: `InversePerpetual` and `InverseFutures` were refused (the mount was not built).
#[test]
fn a_linear_or_coin_margined_contract_is_traded_and_any_other_word_refused() {
    assert_eq!(non_linear_perpetual_refusal("BTCUSDT", Some("LinearPerpetual")), None);
    assert_eq!(
        non_linear_perpetual_refusal("ETHUSDT-06NOV26", Some("LinearFutures")),
        None,
        "a dated linear future is the linear wire (decision 0231)"
    );
    for (symbol, kind) in [("BTCUSD", "InversePerpetual"), ("BTCUSDZ26", "InverseFutures")] {
        assert_eq!(
            non_linear_perpetual_refusal(symbol, Some(kind)),
            None,
            "a coin-margined {kind} is mounted (decision 0258)"
        );
    }
    let why = non_linear_perpetual_refusal("BTCXYZ", Some("SomethingNew"))
        .expect("a word the venue added is refused");
    assert!(why.contains("BTCXYZ"), "the message must name the symbol: {why}");
    assert!(why.contains("SomethingNew"), "...and the venue's own word for it: {why}");
    assert!(
        why.contains("charts") || why.contains("backfill"),
        "...and which half of the instrument DOES work, since the picker offers it: {why}"
    );
}

/// The demo host's own row (`tests/fixtures/classes/`), as the exec grid fetch parses it.
fn fixture_row(file: &str, symbol: &str) -> crate::execution::rest::BybitInstrument {
    let body = match file {
        "inverse_perp" => {
            include_str!("../../tests/fixtures/classes/instruments_inverse_perp_btcusd.json")
        }
        "inverse_future" => {
            include_str!("../../tests/fixtures/classes/instruments_inverse_future_btcusdz26.json")
        }
        "linear_perp" => {
            include_str!("../../tests/fixtures/classes/instruments_linear_perp_ethusdt.json")
        }
        other => panic!("no fixture {other}"),
    };
    let payload: serde_json::Value = serde_json::from_str(body).expect("a captured fixture");
    crate::execution::rest::parse_bybit_perp_instruments(&payload)[symbol].clone()
}

/// **What a mount's money is, from the venue's own row** (decision 0258's bybit amendment): the coin-margined
/// perpetual and dated future the demo host served are `Inverse(1.0)` (one contract is one US dollar) settled in
/// BTC; the linear perpetual is nothing new (`None`, as every bybit mount was).
///
/// Base: `mount_money` did not exist; every bybit mount reported no contract value, so the engine valued an
/// inverse mount LINEAR (a 100-contract order at 76,000 read as 7.6 million).
#[test]
fn a_mounts_money_is_its_rows_inverse_value_and_settle_coin() {
    let inverse = vike_model::ContractValue::inverse(1.0);
    for (file, symbol) in [("inverse_perp", "BTCUSD"), ("inverse_future", "BTCUSDZ26")] {
        let row = fixture_row(file, symbol);
        assert_eq!(
            mount_money(symbol, Some(&row)),
            Ok(Some((inverse, "BTC".to_string()))),
            "{symbol}: the row's inverse value and settle coin"
        );
        assert_eq!(row.properties.step_size, 1.0, "{symbol}: whole USD contracts");
    }
    let linear = fixture_row("linear_perp", "ETHUSDT");
    assert_eq!(mount_money("ETHUSDT", Some(&linear)), Ok(None), "a linear mount is unchanged");
}

/// A mount whose row could not be read keeps the fallback grid when it is linear (as before), and DOES NOT RUN
/// when its name is coin-margined: the kind, the grid and the coin are the row's, never a guess. A coin-margined
/// row that names no settle coin is refused too.
#[test]
fn an_unread_coin_margined_mount_is_refused_never_valued_linear() {
    crate::history::kept_list_for_tests();
    assert_eq!(mount_money("BTCUSDT", None), Ok(None), "a linear name keeps its fallback");
    let why = mount_money("BTCUSD", None).expect_err("an inverse name with no row is refused");
    assert!(why.contains("BTCUSD") && why.contains("row"), "{why}");
    let mut row = fixture_row("inverse_perp", "BTCUSD");
    row.contract = None;
    let why = mount_money("BTCUSD", Some(&row)).expect_err("no settle coin, no mount");
    assert!(why.contains("settle coin"), "{why}");
    // A row that states no word at all leaves the name's shape to decide: an inverse name is never valued linear.
    row.contract_type = None;
    assert!(mount_money("BTCUSD", Some(&row)).is_err(), "a silent row on an inverse name");
    let mut linear = fixture_row("linear_perp", "ETHUSDT");
    linear.contract_type = None;
    assert_eq!(
        mount_money("ETHUSDT", Some(&linear)),
        Ok(None),
        "a silent row on a linear name, as before"
    );
}

/// The clients that start before the mount's row is read (the confirm re-query, the funding poll) take the book
/// from the symbol's KEPT row: `inverse` for a coin-margined contract, `linear` for every other (as all of them were).
#[test]
fn the_early_clients_of_a_mount_sign_in_the_book_its_kept_row_says() {
    crate::history::kept_list_for_tests();
    assert_eq!(named_mount_book("BTCUSD"), Category::Inverse);
    assert_eq!(named_mount_book("BTCUSDZ26"), Category::Inverse);
    assert_eq!(named_mount_book("BTCUSDT"), Category::Linear);
    assert_eq!(named_mount_book("ETHUSDT-06NOV26"), Category::Linear);
    assert_eq!(named_mount_book("BTC-31OCT26-75500-C-USDT"), Category::Linear, "as before");
}

/// **One settle coin per inverse engine** (decision 0258 item 7): an engine mounted on `BTCUSD` signs another
/// BTC-settled coin-margined contract and refuses, in words and before anything is sent, a contract of another
/// coin (another venue wallet), a linear contract and a spot pair (the other money model). A symbol with no row
/// is left to `foreign_grid`'s own refusal.
///
/// Base: the factory took every derivative row its book could sign, so `ETHUSD` and `ETHUSDT` went out from a
/// `BTCUSD` engine and their fills would have been booked in its BTC.
#[test]
fn an_inverse_engine_signs_only_contracts_of_its_own_coin() {
    let dated = fixture_row("inverse_future", "BTCUSDZ26").contract;
    assert_eq!(coin_fence("BTCUSDZ26", dated.as_ref(), "BTCUSD", "BTC"), Ok(()));
    assert_eq!(
        coin_fence("XRPUSDZ26", None, "BTCUSD", "BTC"),
        Ok(()),
        "no row: foreign_grid refuses"
    );

    let mut eth = fixture_row("inverse_perp", "BTCUSD").contract;
    if let Some(vike_model::Contract::Perp(spec)) = eth.as_mut() {
        spec.settle = "ETH".to_string();
    }
    let why = coin_fence("ETHUSD", eth.as_ref(), "BTCUSD", "BTC").expect_err("another coin");
    assert!(why.contains("ETHUSD") && why.contains("ETH") && why.contains("BTC"), "{why}");

    let linear = fixture_row("linear_perp", "ETHUSDT").contract;
    let why = coin_fence("ETHUSDT", linear.as_ref(), "BTCUSD", "BTC").expect_err("linear");
    assert!(why.contains("ETHUSDT") && why.contains("one way"), "{why}");
    let why = coin_fence("DOGEUSDT.S", Some(&vike_model::Contract::CryptoSpot), "BTCUSD", "BTC")
        .expect_err("a spot pair");
    assert!(why.contains("DOGEUSDT.S"), "{why}");
}

/// An ABSENT `contractType` is NOT a refusal. That is the shape of a row the venue changed or a
/// fetch that failed, and refusing every order on a missing field would take a working linear
/// start off the venue for a parse miss — a fetch failure already falls back to the caller's
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
    let why = non_linear_perpetual_refusal("BTCXYZ", Some("SomethingNew")).expect("refused");
    let rest = VenueRestRefusing { why: why.clone() };
    let req = OrderRequest {
        order_client_id: "order-client-id-1".into(),
        symbol: "BTCXYZ".into(),
        ..Default::default()
    };
    let events = rest.submit_order(&req);
    assert_eq!(events.len(), 2, "submitted + exactly one terminal: {events:?}");
    assert_matches!(events[0], Event::OrderSubmitted(_), "{events:?}");
    match &events[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.order_client_id.as_str(), "order-client-id-1");
            assert_eq!(
                r.reason.as_str(),
                why.as_str(),
                "the operator reads the same sentence twice"
            );
        }
        other => panic!("expected a terminal rejection, got {other:?}"),
    }
    // A cancel of something that never rested is NOT a second failure — "unknown != rejection".
    assert!(rest.cancel_order("order-client-id-1").is_ok());
}

/// The sum `vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT` rests on, against the core's own grace: a
/// raise of the wait or of the POST's limit, or a cut of the grace, goes red here instead of stranding
/// an order. Two 5 s re-queries are this bridge's realtime then history, its worst case and the
/// family's.
#[test]
fn the_slowest_submit_is_answered_before_the_core_can_reject_it() {
    // What the live root arms: `crates/vike-tradehub/src/tradehub_cli/live_runtime.rs`'s `submit_ack_timeout`.
    let ack = std::time::Duration::from_secs(30);
    let grace = vike_core::CoreConfig::default().submit_ack_confirm_grace;
    let requery = std::time::Duration::from_secs(5) * 2;
    let post = vike_bridge_core::http::DEFAULT_TIMEOUT;
    assert!(vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT + post + requery <= ack + grace * 2);
}

/// The leverage this arm POSTs to `/v5/position/set-leverage` is the OPERATOR's
/// `[risk] max_leverage`, and an unset profile still yields the historical literal (`2.0`) —
/// the property that makes this change safe to ship onto a live account. NO network:
/// `leverage_for` is the pure decision the start (`crate::adapter`'s `VenueAdapterBybit::start`) calls
/// once, before anything is spawned.
#[test]
fn perp_leverage_follows_the_operator_risk_budget() {
    vike_bridge_core::policy_pins::assert_leverage_follows_the_risk_budget(
        leverage_for,
        DEFAULT_LEVERAGE,
        2.0,
        "the historical hardcoded set-leverage value",
    );
}

/// LIVE proof the startup fetch is REAL + per-symbol (not the hardcoded fallback): BTCUSDT and
/// ETHUSDT have different venue grids, so the fetched ticks must differ. Read-only (no orders).
#[test]
#[ignore = "network + demo creds — run manually"]
fn fetch_returns_real_per_symbol_properties() {
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_secrets::credentials::api_credentials;
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        return; // no creds → skip
    };
    // The demo host, explicitly — this is a demo-creds smoke, so it must never be silently upgraded
    // to the mainnet grid by anything else in the process environment.
    let demo = crate::execution::rest::seed_endpoints(AccountTier::Demo).rest;
    let btc = fetch_bybit_properties(&creds, "BTCUSDT", &demo).expect("BTCUSDT instruments-info");
    let eth = fetch_bybit_properties(&creds, "ETHUSDT", &demo).expect("ETHUSDT instruments-info");
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

/// A fake [`RestTransportJson`] that answers `/v5/account/transaction-log` with a canned
/// SETTLEMENT row (or an empty list) — the offline twin of the live `BybitFundingPoller`
/// smoke, proving `poll_funding_once`'s spawn glue (not just the pure decoder, which
/// `r6_bybit_parity.rs` already pins).
struct FakeFundingTransport {
    rows: Vec<serde_json::Value>,
    /// The `startTime` of every request, in order (`None` = absent, a floorless probe's query).
    starts: std::cell::RefCell<Vec<Option<serde_json::Value>>>,
}

impl FakeFundingTransport {
    fn new(rows: Vec<serde_json::Value>) -> Self {
        FakeFundingTransport { rows, starts: Default::default() }
    }
}

impl RestTransportJson for FakeFundingTransport {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
    ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
        assert_eq!(path, crate::execution::funding::LOG_PATH);
        let start = params.iter().find(|(k, _)| *k == "startTime").map(|(_, v)| v.clone());
        self.starts.borrow_mut().push(start);
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
    let rest = VenueRestBybit {
        category: crate::history::Category::Linear,
        signer: BybitV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            || 0,
        ),
        transport: FakeFundingTransport::new(vec![settlement_row("r1", "1.25", "1000")]),
        base_url: "http://example.invalid".to_string(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
    let (events, mut ingest) = vike_execution::event_channel(8);
    let mut streak = 0u32;
    poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
    assert_eq!(streak, 0, "a successful poll resets the failure streak");

    let mut got = Vec::new();
    while let Ok(vike_execution::Ingest::Event(e)) = ingest.try_recv() {
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
    assert_eq!(
        *rest.transport.starts.borrow(),
        vec![None, None],
        "a floorless poll sends no startTime"
    );
}

/// The restart law: a poller floored at its spawn instant (as `spawn_funding_poll` does) must
/// NEVER emit a settlement that posted BEFORE the floor — those are already embedded in the
/// authoritative venue balance — while a settlement AT/after the floor is emitted exactly once.
/// Simulates the (re)start-with-empty-seen-set case the in-memory id-dedup cannot cover.
#[test]
fn poll_funding_skips_settlements_before_the_spawn_floor() {
    const FLOOR: i64 = 5_000;
    let rest = VenueRestBybit {
        category: crate::history::Category::Linear,
        signer: BybitV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            || 0,
        ),
        // The venue answers with history straddling the floor (as a wide window would on a
        // restart): pre-floor, exactly-at-floor, and post-floor rows.
        transport: FakeFundingTransport::new(vec![
            settlement_row("old", "9.99", "1000"),
            settlement_row("edge", "0.25", "5000"),
            settlement_row("new", "1.25", "9000"),
        ]),
        base_url: "http://example.invalid".to_string(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", FLOOR);
    let (events, mut ingest) = vike_execution::event_channel(8);
    let mut streak = 0u32;
    poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");

    let mut got = Vec::new();
    while let Ok(vike_execution::Ingest::Event(e)) = ingest.try_recv() {
        got.push(e);
    }
    match got.as_slice() {
        [Event::Funding(edge), Event::Funding(new)] => {
            assert_eq!(edge.timestamp, 5000, "ts == floor is emitted (floor is inclusive)");
            assert_eq!(new.timestamp, 9000);
            assert_eq!(new.amount, 1.25);
        }
        other => panic!("expected exactly the at/post-floor settlements, got {other:?}"),
    }

    // The next cadence re-serves the same rows: nothing re-emits (id-dedup) and the
    // pre-floor row stays excluded.
    poll_funding_once(&mut poller, &events, &mut streak).expect("core is alive");
    assert!(ingest.try_recv().is_err(), "no duplicates on the second poll");
    // The floor also rides as the first window's startTime request param (the optimization).
    assert_eq!(rest.transport.starts.borrow()[0], Some(serde_json::json!(FLOOR.to_string())));
}

/// When the core is gone (ingest receiver dropped), the poll glue reports `Err(CoreGone)` so
/// its background thread's loop can self-exit instead of polling forever into a void.
#[test]
fn poll_funding_once_reports_core_gone() {
    let rest = VenueRestBybit {
        category: crate::history::Category::Linear,
        signer: BybitV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            || 0,
        ),
        transport: FakeFundingTransport::new(vec![settlement_row("r2", "0.5", "1000")]),
        base_url: "http://example.invalid".to_string(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
    let (events, ingest) = vike_execution::event_channel(8);
    drop(ingest); // the core is gone
    let mut streak = 0u32;
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_err());
}

/// A transport failure is NOT core-gone: the glue reports `Ok` (keep polling), counts the
/// consecutive-failure streak (the once-per-streak warn key), and a later success resets it.
#[test]
fn poll_funding_fetch_failure_counts_the_streak() {
    struct FailingTransport;
    impl RestTransportJson for FailingTransport {
        fn signed(
            &self,
            _base_url: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, serde_json::Value)],
            _signer: &dyn SignerJson,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            Err(vike_bridge_core::transport::VenueApiError {
                code: 0,
                msg: "connect refused".to_string(),
            })
        }
    }
    let rest = VenueRestBybit {
        category: crate::history::Category::Linear,
        signer: BybitV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            || 0,
        ),
        transport: FailingTransport,
        base_url: "http://example.invalid".to_string(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: DEFAULT_LEVERAGE,
    };
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", 0);
    let (events, mut ingest) = vike_execution::event_channel(8);
    let mut streak = 0u32;
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
    assert!(poll_funding_once(&mut poller, &events, &mut streak).is_ok());
    assert_eq!(streak, 2, "consecutive failures accumulate");
    assert!(ingest.try_recv().is_err(), "failures emit nothing");
}

/// A closed localhost port: connection-refused is immediate (no DNS, no network), so a request
/// fails fast and the assertions are purely on the GATE, which is consulted BEFORE the send.
const DEAD: &str = "http://127.0.0.1:1";

fn submit_client() -> VenueRestBybit<RestTransportJsonUreq> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    let grid = SymbolProperties {
        tick_size: 0.1,
        step_size: 0.001,
        min_qty: 0.001,
        min_notional: 5.0,
        ..Default::default()
    };
    let dead =
        BybitUrls { mainnet: false, rest: DEAD.into(), ws: DEAD.into(), listings: DEAD.into() };
    let clock = VenueClock::default();
    order_rest(&creds, &dead, "BTCUSDT", grid, 3.0, Some("broker".into()), Category::Linear, &clock)
}

/// **THE ORDER PATH IS GATED.** The exec thread's submit client is built on a transport that
/// carries the `BYBIT.orders` window, and the window is the table's own: it admits exactly its
/// budget in one `window` and holds back the next. Every smoke attaches this gate to a transport it
/// builds itself, so before this the gate was proven everywhere EXCEPT on the client that puts
/// orders on the wire.
#[test]
fn the_submit_client_is_built_on_the_venues_order_rate_gate() {
    use vike_venues::venue_rate_limits::BYBIT;
    let rest = submit_client();
    let gate = rest.transport.rate_gate().expect("the submit client's transport must be gated");
    for taken in 0..BYBIT.orders.admitted() {
        assert!(
            gate.try_proceed(),
            "slot {taken} of {} is inside the budget",
            BYBIT.orders.admitted()
        );
    }
    assert!(
        !gate.try_proceed(),
        "the {}th op inside one {:?} window must be held back",
        BYBIT.orders.admitted() + 1,
        BYBIT.orders.window()
    );
    // ...and the gate changed nothing else about the client: the fields the old literal set.
    assert_eq!(rest.base_url, DEAD);
    assert_eq!(rest.symbol, "BTCUSDT");
    assert_eq!(rest.leverage, 3.0);
    assert_eq!(rest.properties.tick_size, 0.1);
}

/// Every operation the submit client speaks to the order endpoints draws ONE slot of that gate —
/// submit, cancel, amend and the startup `set_leverage` all go through the transport's gated
/// `signed`, none of them through the exempt `signed_requery`. (A connection-refused send still
/// took its slot: the gate is consulted before the send.)
#[test]
fn every_order_operation_of_the_submit_client_draws_the_gate() {
    use vike_venues::venue_rate_limits::BYBIT;
    let rest = submit_client();
    let gate = rest.transport.rate_gate().expect("gated").clone();
    let req = OrderRequest {
        order_client_id: "c1".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        ..Default::default()
    };
    let budget = BYBIT.orders.admitted();
    let _ = rest.submit_order(&req);
    let _ = rest.cancel_order("c1");
    let _ = rest.modify_order("c1", Some(2.0), None);
    let _ = rest.set_leverage();
    let used = 4;
    for _ in 0..(budget - used) {
        assert!(gate.try_proceed());
    }
    assert!(
        !gate.try_proceed(),
        "submit, cancel, amend and set_leverage took exactly {used} of {budget} slots"
    );
}

/// The gap-sentinel this adapter arms after the last command is the 5 s it armed when the number
/// was a private `FILL_SENTINEL` here: it is `vike_bridge_core::exec_actor`'s venue-free default now
/// (decision 0160), so a change to that default is a deliberate edit to every command-driven venue.
#[test]
fn the_gap_sentinel_is_the_five_seconds_it_always_was() {
    assert_eq!(DEFAULT_FILL_SENTINEL, Duration::from_secs(5));
}

/// **A foreign symbol is traded only on a grid the venue itself answered.** An unknown symbol or a
/// failed `instruments-info` fetch (`None`) never falls back to a guessed grid, and an inverse or
/// dated contract gets the same refusal the mounted symbol would; a linear perpetual gets ITS grid.
#[test]
fn a_foreign_symbol_is_signed_only_on_the_grid_the_venue_answered() {
    let linear = crate::execution::rest::BybitInstrument {
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() },
        contract_type: Some("LinearPerpetual".to_string()),
        ..Default::default()
    };
    let (grid, book) = foreign_grid("ETHUSDT", Some(linear.clone())).expect("linear perp trades");
    assert_eq!((grid.tick_size, grid.step_size), (0.01, 0.01));
    assert_eq!(book, Category::Linear);

    let why = foreign_grid("ETHUSDT", None).expect_err("no row, no order");
    assert!(why.contains("ETHUSDT") && why.contains("not sent"), "{why}");
    let untyped = crate::execution::rest::BybitInstrument { contract_type: None, ..linear.clone() };
    let why = foreign_grid("ETHUSDT", Some(untyped)).expect_err("a row with no contract type");
    assert!(why.contains("contract type") && why.contains("not sent"), "{why}");

    // A contract type this adapter has no book for stays refused, with the venue's own word.
    let option = crate::execution::rest::BybitInstrument {
        contract_type: Some("SomethingNew".to_string()),
        ..linear
    };
    let why = foreign_grid("XYZ", Some(option)).expect_err("an unknown book is refused");
    assert!(why.contains("SomethingNew"), "{why}");
}

/// **Each derivative class is signed in the book the venue's OWN row names** (decision 0231), on the grid of the
/// row the demo host served on 2026-10-30 (`tests/fixtures/classes/`): the perpetual and the dated future in
/// `linear`, the coin-margined perpetual and the dated coin-margined future in `inverse`, the quantity step of
/// an inverse contract one whole contract (a USD notional) and a dated future's the base asset's.
#[test]
fn a_foreign_derivative_is_signed_in_the_book_its_row_names() {
    let row = |text: &str, symbol: &str| {
        let payload: serde_json::Value = serde_json::from_str(text).unwrap();
        crate::execution::rest::parse_bybit_perp_instruments(&payload)[symbol].clone()
    };
    let perp = row(
        include_str!("../../tests/fixtures/classes/instruments_linear_perp_ethusdt.json"),
        "ETHUSDT",
    );
    let dated = row(
        include_str!("../../tests/fixtures/classes/instruments_linear_future_ethusdt_06nov26.json"),
        "ETHUSDT-06NOV26",
    );
    let inverse = row(
        include_str!("../../tests/fixtures/classes/instruments_inverse_perp_btcusd.json"),
        "BTCUSD",
    );
    let inverse_dated = row(
        include_str!("../../tests/fixtures/classes/instruments_inverse_future_btcusdz26.json"),
        "BTCUSDZ26",
    );
    for (symbol, instrument, book, step) in [
        ("ETHUSDT", perp, Category::Linear, 0.01),
        ("ETHUSDT-06NOV26", dated, Category::Linear, 0.01),
        ("BTCUSD", inverse, Category::Inverse, 1.0),
        ("BTCUSDZ26", inverse_dated, Category::Inverse, 1.0),
    ] {
        let (grid, got) = foreign_grid(symbol, Some(instrument))
            .unwrap_or_else(|why| panic!("{symbol} must be signed: {why}"));
        assert_eq!(got, book, "{symbol}");
        assert_eq!(grid.step_size, step, "{symbol}");
        assert!(grid.tick_size > 0.0 && grid.min_qty > 0.0, "{symbol}: {grid:?}");
    }
}

/// **A client built without a grid fetch (a cancel, a status query) is built in the book the NAME says, or not at
/// all.** A cancel in the wrong book answers `110001`, which the router swallows as "already gone": a live order
/// reported cancelled. So a name the shape cannot place (and the venue's listings cannot, offline here) is `Err`,
/// never a default to `linear`.
#[test]
fn a_client_without_a_grid_is_built_in_the_book_the_name_says() {
    crate::history::kept_list_for_tests();
    for (symbol, book) in [
        ("BTCUSDT", Category::Linear),
        ("ETHUSDT-27NOV26", Category::Linear),
        ("BTCUSD", Category::Inverse),
        ("BTCUSDZ26", Category::Inverse),
        ("DOGEUSDT.S", Category::Spot),
    ] {
        assert_eq!(light_category("http://127.0.0.1:1", symbol), Ok(book), "{symbol}");
    }
}

/// A spot pair is signed on its own row's grid (`basePrecision` is the quantity step, `minOrderAmt` the minimum
/// order value), and only while the venue lists it as Trading.
#[test]
fn a_spot_pair_is_signed_on_the_spot_rows_grid() {
    let payload: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/classes/instruments_spot_dogeusdt.json"
    ))
    .unwrap();
    let row = payload["result"]["list"][0].clone();
    let grid = spot_grid("DOGEUSDT", Some(&row)).expect("a Trading pair trades");
    assert_eq!((grid.tick_size, grid.step_size), (0.00001, 0.1));
    assert_eq!(grid.min_notional, 5.0, "minOrderAmt");

    let why = spot_grid("DOGEUSDT", None).expect_err("no row, no order");
    assert!(why.contains("spot") && why.contains("not sent"), "{why}");
    let mut closed = row;
    closed["status"] = serde_json::json!("Closed");
    let why = spot_grid("DOGEUSDT", Some(&closed)).expect_err("not trading");
    assert!(why.contains("not trading"), "{why}");
}

/// **A symbol admitted on demand is written through the bridge's OWN set-leverage verb.** The client
/// is the order client's build pointed at the ADMITTED symbol (upper-cased) on the host its tier
/// selects, the POST names that symbol and carries the figure on both sides, a venue "not modified" is
/// swallowed (a repeat is idempotent), and any other refusal keeps the venue's words.
#[test]
fn a_symbol_admitted_on_demand_is_written_through_the_bridges_own_set_leverage() {
    use std::cell::RefCell;

    /// `(method, path, params)` of every call.
    type Sent = Vec<(String, String, Vec<(String, serde_json::Value)>)>;
    struct Wire {
        sent: RefCell<Sent>,
        answer: serde_json::Value,
    }
    impl RestTransportJson for Wire {
        fn signed(
            &self,
            _base_url: &str,
            path: &str,
            method: &str,
            params: &[(&str, serde_json::Value)],
            _signer: &dyn SignerJson,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            let params = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
            self.sent.borrow_mut().push((method.to_string(), path.to_string(), params));
            Ok(self.answer.clone())
        }
    }
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };

    // The client: the admitted symbol, the figure, the tier's host, the order window.
    let (demo_hosts, live_hosts) = (
        crate::execution::rest::seed_endpoints(vike_model::AccountTier::Demo),
        crate::execution::rest::seed_endpoints(vike_model::AccountTier::Live),
    );
    let demo = symbol_leverage_rest(&creds, &demo_hosts, "ethusdt", 10.0, None);
    assert_eq!((demo.symbol.as_str(), demo.leverage), ("ETHUSDT", 10.0));
    assert_eq!((demo.base_url.as_str(), demo.mainnet), (demo_hosts.rest.as_str(), false));
    assert!(demo.transport.rate_gate().is_some(), "the write rides the order-rate window");
    let main = symbol_leverage_rest(&creds, &live_hosts, "ETHUSDT", 3.0, Some("broker".into()));
    assert_eq!((main.base_url.as_str(), main.mainnet), (live_hosts.rest.as_str(), true));
    assert_ne!(demo.base_url, main.base_url, "demo and mainnet are two hosts");

    let over = |answer: serde_json::Value| VenueRestBybit {
        category: crate::history::Category::Linear,
        signer: BybitV5Signer::new(&creds, || 0),
        transport: Wire { sent: RefCell::new(Vec::new()), answer },
        base_url: "https://demo".to_string(),
        mainnet: false,
        symbol: "ETHUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 10.0,
    };

    // A clean write: ONE POST to set-leverage, buy == sell, naming the admitted symbol.
    let rest = over(serde_json::json!({ "retCode": 0, "retMsg": "OK", "result": {} }));
    assert_eq!(post_symbol_leverage(&rest), Ok(()));
    let sent = rest.transport.sent.borrow();
    assert_eq!(sent.len(), 1, "one write");
    assert_eq!(
        (sent[0].0.as_str(), sent[0].1.as_str()),
        ("POST", crate::execution::rest::PATH_SET_LEVERAGE)
    );
    let param = |name: &str| sent[0].2.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    assert_eq!(param("symbol"), Some(serde_json::json!("ETHUSDT")));
    assert_eq!(param("category"), Some(serde_json::json!("linear")));
    assert_eq!(param("buyLeverage"), Some(serde_json::json!("10.0")));
    assert_eq!(param("sellLeverage"), Some(serde_json::json!("10.0")));
    drop(sent);

    // 110043 "leverage not modified" is the venue saying it already holds the figure: not an error.
    let rest = over(serde_json::json!({ "retCode": 110043, "retMsg": "leverage not modified" }));
    assert_eq!(post_symbol_leverage(&rest), Ok(()));

    // Anything else is refused with the venue's own words and the symbol.
    let rest = over(serde_json::json!({ "retCode": 10005, "retMsg": "Permission denied" }));
    let why = post_symbol_leverage(&rest).unwrap_err();
    assert!(matches!(why, LeverageWriteError::Refused(_)), "{why:?}");
    let why = why.to_string();
    assert!(why.contains("ETHUSDT") && why.contains("Permission denied"), "{why}");

    // 10000 "Server Timeout" is the venue saying the outcome is unknown: AMBIGUOUS (the POST may have
    // landed), so the lane reads the venue back instead of reporting a refusal.
    let rest = over(serde_json::json!({ "retCode": 10000, "retMsg": "Server Timeout" }));
    let got = post_symbol_leverage(&rest).unwrap_err();
    assert!(matches!(got, LeverageWriteError::Ambiguous(_)), "{got:?}");
    assert!(got.to_string().contains("ETHUSDT"), "{got}");

    // The verb is guarded HERE: a `live` (mainnet) account is refused before any client or request exists,
    // whatever `LEVERAGE_VENUES` says (`write_symbol_leverage` calls this first; the write itself needs the
    // network, so only the guard is run).
    let refused = write_demo_only(crate::execution::rest::VENUE, true, "ETHUSDT").unwrap_err();
    assert!(matches!(refused, LeverageWriteError::Refused(_)), "{refused:?}");
    assert!(refused.to_string().contains("demo account only"), "{refused}");
    assert!(refused.to_string().starts_with("bybit: "), "{refused}");
    let demo = write_demo_only(crate::execution::rest::VENUE, false, "ETHUSDT");
    assert_eq!(demo, Ok(()), "a demo account passes");
}

/// **A spot pair's leverage write is REFUSED, never "POSTED"** (owner ruling L-D2, the
/// `VenueAdapter::set_symbol_leverage` contract, as binance and okx answer): a spot pair holds no leverage, and
/// the client's `set_leverage` would send nothing and say `Ok`. Refused before any request (no network here).
#[test]
fn a_spot_pairs_leverage_write_is_refused_as_binance_and_okx_refuse_theirs() {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    let demo = crate::execution::rest::seed_endpoints(vike_model::AccountTier::Demo);
    for symbol in ["DOGEUSDT.S", "dogeusdt.s"] {
        let refused = write_symbol_leverage(&creds, &demo, symbol, 3.0, None).unwrap_err();
        assert!(matches!(refused, LeverageWriteError::Refused(_)), "{symbol}: {refused:?}");
        let why = refused.to_string();
        assert!(why.starts_with("bybit: ") && why.contains("spot pair"), "{why}");
        assert!(why.contains("nothing was sent"), "{why}");
    }
}

/// **A trigger amend reaches the command loop.** The client is the actor as `over` returns it, the step the real spawn
/// takes, which declares the trigger amend; without it the trait's default would refuse the amend in words before the
/// channel.
#[test]
fn the_execution_client_queues_a_trigger_amend_for_its_loop() {
    let (events, _ingest) = vike_execution::event_channel(8);
    let (seen_tx, seen) = mpsc::channel();
    let actor = ExecutionClientActor::spawn("bybit-exec-trigger-test", events, move |rx| {
        while let Ok(command) = rx.recv() {
            match command {
                ExecutionClientActorCommand::Modify { order, new_trigger_price, .. } => {
                    seen_tx.send((order.order_client_id.clone(), new_trigger_price)).unwrap();
                }
                ExecutionClientActorCommand::Shutdown => break,
                _ => {}
            }
        }
    });
    let mut client = over(actor, Arc::default());
    let order = OrderRequest {
        order_client_id: "sl1".into(),
        order_type: vike_model::OrderType::StopLimit,
        ..Default::default()
    };
    assert_eq!(client.modify_trigger(&order, None, None, 58100.4), Ok(()));
    assert_eq!(
        seen.recv_timeout(Duration::from_secs(5)).expect("the loop is handed the amend"),
        ("sl1".to_string(), Some(58100.4))
    );
    client.detach();
}
