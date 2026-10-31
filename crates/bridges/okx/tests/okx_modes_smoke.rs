//! LIVE DEMO drills for okx's account position mode (`crates/bridges/okx/src/account_modes.rs`): the bridge
//! switches a FLAT long/short account to net mode at its start, names what blocks it otherwise, and an
//! order then goes through. DEMO ONLY: the account is the store's default DEMO okx account, resolved
//! through `vike_secrets::smoke_directory`, and every transport asserts the `x-simulated-trading: 1`
//! header, the one thing separating demo from mainnet on this venue (one host).
//!
//!     cargo test -p vike-okx --test okx_modes_smoke -- --ignored --nocapture --test-threads=1
//!
//! HAND-RUN ONLY (no workflow lists this file); the lane's environment is the live-smoke lane's
//! (`docs/ops/live-smoke-lane.md`: `VIKE_SETTINGS_DIR`, `VIKE_NO_LIVE_KEYS=true`). Every answer the
//! tests learn is printed as a `MEASURED` line before it is asserted; those lines are the record.
//!
//! * `okx_switches_a_flat_long_short_account_to_net_and_an_order_goes_through`: WRITES the demo account's
//!   position mode. It refuses (self-skips, writing nothing) unless the account is flat on all three
//!   reads the bridge makes. It puts the account in `long_short_mode`, MEASURES what OKX answers a net
//!   order there, rests one far order the account is allowed to hold in that mode and shows the bridge
//!   refusing the switch BY NAME, cancels it, shows the bridge switching and reading back `net_mode`,
//!   places and cancels a far net order, shows the second start writing nothing, and puts the account
//!   back in the mode it found. The restore is armed BEFORE the first write and also runs from a drop
//!   guard. ⚠ A daemon that runs this account keeps trading it meanwhile: its orders fail for the few
//!   seconds the account is long/short.
//! * `okx_modes_probes_smoke`: read-only except ONE same-level `set-account-level` (the demo account is
//!   asked to be at the level it already has), and prints how OKX answers an instrument it does not list.
//! * `okx_reconcile_reads_a_resting_order_on_an_admitted_instrument`: rests and cancels ONE far
//!   `ETH-USDT-SWAP` order (the reconcile client is mounted on BTC) and shows the client reading it only
//!   once told ETH was admitted, in ETH's own base units, and the restore planner adopting it. This is the
//!   venue-level proof behind okx's row of `vike_gatewayhub_client::trade::RECON_READS_ADMITTED`; the
//!   restart of a whole node across it is not drilled.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_exec::recon::{ReconClient, RestoreScope, RestoredOrderRef, plan_restore};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, now_ms};
use vike_okx::OkxReconClient;
use vike_okx::account_config::PATH_ACCOUNT_CONFIG;
use vike_okx::account_modes::{
    PATH_SET_POSITION_MODE, PositionMode, WatchVerdict, open_positions, read_position_mode,
    settle_modes, watch_verdict,
};
use vike_okx::perp::{
    OkxPerpRest, PATH_INSTRUMENTS, PATH_ORDER_CREATE, PATH_ORDERS_ALGO_PENDING,
    PATH_ORDERS_PENDING, PATH_POSITIONS, REST, parse_okx_perp_instruments,
};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTC-USDT-SWAP";
/// The only tier this file resolves an account at.
const TIER: AccountTier = AccountTier::Demo;
const NET: &str = "net_mode";
const LONG_SHORT: &str = "long_short_mode";

/// A transport that sends `x-simulated-trading: 1`, rate-gated like the neighbouring smokes.
fn demo_transport() -> UreqOkxTransport {
    assert_eq!(TIER, AccountTier::Demo, "a modes smoke resolves the DEMO account only");
    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
    assert!(transport.simulated, "the demo header is not set: this would be MAINNET");
    transport
}

fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, TIER);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

/// Raw signed calls on the demo host, and the account's mode as the venue states it.
struct Demo {
    transport: UreqOkxTransport,
    signer: OkxV5Signer,
}

impl Demo {
    fn new(creds: &Credentials) -> Self {
        Demo { transport: demo_transport(), signer: OkxV5Signer::new(creds, now_ms) }
    }

    fn get(&self, path: &str, params: &[(&str, Value)]) -> Value {
        let raw = self
            .transport
            .signed(REST, path, "GET", params, &self.signer)
            .unwrap_or_else(|e| panic!("GET {path} failed: {e}"));
        unwrap_okx(raw).unwrap_or_else(|e| panic!("GET {path} answered an error: {e}"))
    }

    fn post(&self, path: &str, params: &[(&str, Value)]) -> Result<Value, VenueApiError> {
        self.transport.signed(REST, path, "POST", params, &self.signer).and_then(unwrap_okx)
    }

    fn pos_mode(&self) -> String {
        self.get(PATH_ACCOUNT_CONFIG, &[])[0]["posMode"].as_str().unwrap_or("?").to_string()
    }

    /// What keeps the demo account from being flat (empty = flat), read the way the bridge reads it.
    fn blockers(&self) -> Vec<String> {
        let mut out = open_positions(&self.get(PATH_POSITIONS, &[]));
        let count = |v: Value| v.as_array().map_or(0, Vec::len);
        let orders = count(self.get(PATH_ORDERS_PENDING, &[]));
        let stops = count(self.get(PATH_ORDERS_ALGO_PENDING, &[("ordType", json!("conditional"))]));
        if orders > 0 {
            out.push(format!("{orders} resting order(s)"));
        }
        if stops > 0 {
            out.push(format!("{stops} resting stop(s)"));
        }
        out
    }

    /// Wait (bounded) until the account reads `mode`.
    fn wait_for(&self, mode: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.pos_mode() == mode {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }
}

/// Puts the position mode back to what the account had: explicitly or, if the test panics first, from
/// `Drop`. Never panics itself (a panic in `drop` during unwinding aborts).
struct RestoreMode {
    demo: Demo,
    original: String,
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        let current =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.demo.pos_mode()))
                .unwrap_or_else(|_| "?".to_string());
        if current == self.original {
            return;
        }
        match self.demo.post(PATH_SET_POSITION_MODE, &[("posMode", json!(self.original))]) {
            Ok(_) => {
                eprintln!("restored the demo account to {} from the drop guard", self.original)
            }
            Err(e) => eprintln!(
                "RESTORE FAILED: set the DEMO okx account's position mode back to {} by hand: {e}",
                self.original
            ),
        }
    }
}

fn build_rest(creds: &Credentials, symbol: &str) -> OkxPerpRest<UreqOkxTransport> {
    let transport = demo_transport();
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", symbol.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[symbol]
        .clone();
    OkxPerpRest {
        signer: OkxV5Signer::new(creds, now_ms),
        transport,
        base_url: REST.to_string(),
        symbol: symbol.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    }
}

/// A far BUY limit (half the last price, the instrument's minimum size) through the bridge's own
/// submit, as the exec thread places it: `posSide: net`.
fn far_net_order(rest: &OkxPerpRest<UreqOkxTransport>, coid: &str) -> Vec<Event> {
    let mark = rest.last_price().expect("ticker");
    let request: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": rest.symbol,
        "side": 1, "qty": rest.properties.min_qty * rest.ct_val, "order_type": "limit",
        "price": mark * 0.5, "ts": now_ms()
    }))
    .unwrap();
    VenueRest::submit_order(rest, &request)
}

fn rejection(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| match e {
        Event::OrderRejected(r) => Some(r.reason.to_string()),
        _ => None,
    })
}

#[test]
#[ignore = "network + demo creds + WRITES the demo position mode, then restores it — run manually (see module doc)"]
fn okx_switches_a_flat_long_short_account_to_net_and_an_order_goes_through() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    let demo = Demo::new(&creds);

    let original = demo.pos_mode();
    println!("MEASURED the demo account's position mode before anything: {original}");
    assert!(original == NET || original == LONG_SHORT, "an unknown posMode word: {original}");
    let blockers = demo.blockers();
    if !blockers.is_empty() {
        tracing::warn!(target: "vike_okx", ?blockers, "SKIP: the demo account is not flat, so its position mode is not ours to move");
        return;
    }
    let guard = RestoreMode { demo: Demo::new(&creds), original: original.clone() };

    // 1. ARRANGE: the account the bridge must meet is long/short.
    if original == NET {
        let wrote = demo.post(PATH_SET_POSITION_MODE, &[("posMode", json!(LONG_SHORT))]);
        println!("MEASURED set-position-mode long_short_mode (arranging the drill): {wrote:?}");
        wrote.expect("the demo account must accept long_short_mode while flat");
    }
    assert!(demo.wait_for(LONG_SHORT), "the account never read long_short_mode");

    // 2. THE PREMISE: a net order is refused in long/short mode. The venue's words are the record.
    let rest = build_rest(&creds, SYMBOL);
    let premise = far_net_order(&rest, &format!("vtmodes{}", now_ms() % 100_000_000));
    let refused = rejection(&premise);
    println!(
        "MEASURED a net order (posSide net) in long_short_mode: {refused:?} (events: {premise:?})"
    );
    assert!(refused.is_some(), "premise: a net order must be refused in long/short mode");

    // 2b. THE MODE WATCH (known-limitations "does not watch the mode afterwards"): the reconcile pass's
    // own read, over the live long/short account. The pass must still SUCCEED (the watch never fails
    // it), the read must say long_short_mode, and the rule must speak.
    let watch_client = OkxReconClient::new(
        OkxV5Signer::new(&creds, now_ms),
        demo_transport(),
        REST,
        SYMBOL,
        rest.ct_val,
    );
    let pass = watch_client.fetch_position_status_reports();
    println!("MEASURED the reconcile position leg while the account is long/short: {pass:?}");
    assert!(pass.is_ok(), "the mode watch must never fail the position leg: {pass:?}");
    let word = read_position_mode(&demo.transport, &demo.signer, REST)
        .expect("the watch's read")
        .expect("the demo network is read");
    println!("MEASURED the mode watch's read in long_short_mode: {word}");
    assert_eq!(watch_verdict(Some(false), &word), (WatchVerdict::LongShort, Some(true)));

    // 3. NAMED REFUSAL: a resting order (a long-side one, which this mode accepts) blocks the switch.
    let mark = rest.last_price().expect("ticker");
    let coid = format!("vtmodesb{}", now_ms() % 100_000_000);
    let px = format!("{:.1}", (mark * 0.5).floor());
    let placed = demo.post(
        PATH_ORDER_CREATE,
        &[
            ("instId", json!(SYMBOL)),
            ("tdMode", json!("cross")),
            ("side", json!("buy")),
            ("posSide", json!("long")),
            ("ordType", json!("limit")),
            ("sz", json!(rest.properties.min_qty.to_string())),
            ("px", json!(px)),
            ("clOrdId", json!(coid)),
        ],
    );
    println!("MEASURED a long-side far order in long_short_mode: {placed:?}");
    placed.expect("a far long-side order must rest in long/short mode");
    let blocked = settle_modes(&demo.transport, &demo.signer, REST);
    println!("MEASURED the bridge's start probe with a resting order: {blocked:?}");
    match &blocked.position {
        PositionMode::Blocked(why) => assert!(why.contains("1 resting order(s)"), "{why}"),
        other => panic!("expected Blocked while an order rests, got {other:?}"),
    }
    assert_eq!(demo.pos_mode(), LONG_SHORT, "a refused switch leaves the account as it was");
    rest.cancel_order(&coid).expect("cancel the resting order");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !demo.blockers().is_empty() {
        assert!(Instant::now() < deadline, "the cancelled order never left the book");
        std::thread::sleep(Duration::from_millis(500));
    }

    // 4. THE HEAL: flat, so the bridge switches to net and reads it back.
    let healed = settle_modes(&demo.transport, &demo.signer, REST);
    println!("MEASURED the bridge's start probe on the flat long/short account: {healed:?}");
    assert_eq!(healed.position, PositionMode::Healed);
    assert_eq!(demo.pos_mode(), NET, "the venue now reads net_mode");
    let word = read_position_mode(&demo.transport, &demo.signer, REST)
        .expect("the watch's read")
        .expect("the demo network is read");
    println!("MEASURED the mode watch's read after the switch: {word}");
    assert_eq!(watch_verdict(Some(true), &word), (WatchVerdict::BackToNet, Some(false)));

    // 5. AN ORDER GOES THROUGH: the same net order that was refused in step 2 rests now.
    let coid = format!("vtmodesn{}", now_ms() % 100_000_000);
    let events = far_net_order(&rest, &coid);
    println!("MEASURED the same net order after the switch: {events:?}");
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "the net order must be ACCEPTED after the switch: {events:?}"
    );
    rest.cancel_order(&coid).expect("cancel the net order");

    // 6. IDEMPOTENT: the next start finds net mode and writes nothing.
    let again = settle_modes(&demo.transport, &demo.signer, REST);
    println!("MEASURED the next start's probe: {again:?}");
    assert_eq!(again.position, PositionMode::Net);

    drop(guard);
    let restored = demo.pos_mode();
    println!("MEASURED the demo account's position mode after the drill: {restored}");
    assert_eq!(restored, original, "the account must end in the mode it was found in");
}

#[test]
#[ignore = "network + demo creds + ONE same-level set-account-level write — run manually (see module doc)"]
fn okx_modes_probes_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    let demo = Demo::new(&creds);

    // The venue's answer for an instrument it does not list: the recon client reads code 51001 as "holds
    // nothing" for an admitted instrument.
    let unlisted = demo.transport.public(
        REST,
        PATH_INSTRUMENTS,
        &[("instType", "SWAP".into()), ("instId", "NOPE-USDT-SWAP".into())],
    );
    println!("MEASURED public/instruments for an unlisted SWAP id: {unlisted:?}");

    // set-account-level at the level the account already has: a no-op write whose ANSWER shows whether
    // the endpoint exists for this key and what the venue says. It never changes the level.
    let level = demo.get(PATH_ACCOUNT_CONFIG, &[])[0]["acctLv"].as_str().unwrap_or("?").to_string();
    println!("MEASURED the demo account's acctLv: {level}");
    let same = demo.post("/api/v5/account/set-account-level", &[("acctLv", json!(level.clone()))]);
    println!("MEASURED set-account-level to the level the account already has ({level}): {same:?}");
    assert_eq!(
        demo.get(PATH_ACCOUNT_CONFIG, &[])[0]["acctLv"].as_str(),
        Some(level.as_str()),
        "a same-level write must not move the level"
    );
}

/// A resting order on an instrument the engine was NOT mounted on: the venue-level proof for okx's row
/// in `RECON_READS_ADMITTED`. One far ETH order is placed through a client signed on ETH, then the real
/// reconcile client (mounted on BTC) is told ETH was admitted and read: the order must appear in BASE
/// units of ETH's own contract, and the restore planner must ADOPT it for the admitted symbol. Without
/// the admission the same read does not see it.
#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn okx_reconcile_reads_a_resting_order_on_an_admitted_instrument() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    const ETH: &str = "ETH-USDT-SWAP";
    let eth = build_rest(&creds, ETH);
    let btc = build_rest(&creds, SYMBOL);
    let coid = format!("vtadm{}", now_ms() % 100_000_000);
    let events = far_net_order(&eth, &coid);
    struct CancelOnDrop<'a>(&'a OkxPerpRest<UreqOkxTransport>, &'a str);
    impl Drop for CancelOnDrop<'_> {
        fn drop(&mut self) {
            if let Err(e) = self.0.cancel_order(self.1) {
                eprintln!(
                    "CLEANUP FAILED: cancel the demo ETH order {} by hand: {}",
                    self.1, e.msg
                );
            }
        }
    }
    let _cleanup = CancelOnDrop(&eth, &coid);
    println!("MEASURED the far ETH order: {events:?}");
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");

    let recon = OkxReconClient::new(
        OkxV5Signer::new(&creds, now_ms),
        demo_transport(),
        REST,
        SYMBOL,
        btc.ct_val,
    );
    let scope = |symbols: Vec<String>| RestoreScope {
        venue: "okx".to_string(),
        account: None,
        symbols,
        lists_trigger_orders: recon.lists_trigger_orders(),
    };
    let restored = RestoredOrderRef {
        coid: coid.clone(),
        venue: Some("okx".to_string()),
        symbol: Some(ETH.to_string()),
        account: None,
        order_type: Some(vike_model::OrderType::Limit),
        ..Default::default()
    };

    // Not told: the report does not span ETH, and the claim says so.
    assert_eq!(recon.admitted_symbols(), Some(Vec::new()));
    let before = recon.fetch_order_status_reports(0).expect("fetch before the admission");
    assert!(before.iter().all(|r| r.symbol != ETH), "ETH is not read before it is admitted");

    // Told: the order is in the report, in ETH's own base units, and the planner adopts it.
    recon.admit_symbol(ETH);
    assert_eq!(recon.admitted_symbols(), Some(vec![ETH.to_string()]));
    let orders = recon.fetch_order_status_reports(0).expect("fetch after the admission");
    let row = orders
        .iter()
        .find(|r| r.client_order_id.as_deref() == Some(coid.as_str()))
        .unwrap_or_else(|| panic!("the resting ETH order is not in the report: {orders:?}"));
    println!("MEASURED the reconcile report row of the admitted ETH order: {row:?}");
    let expected = eth.properties.min_qty * eth.ct_val;
    assert!((row.qty - expected).abs() < 1e-9, "qty {} != {expected} (ETH's own ctVal)", row.qty);
    let fills = recon.fetch_fill_reports(now_ms() - 3_600_000).expect("fills");
    let plan = plan_restore(&scope(vec![SYMBOL.into(), ETH.into()]), &[restored], &orders, &fills);
    assert_eq!(plan.adopt.len(), 1, "the admitted instrument's order is ADOPTED: {plan:?}");
    let positions = recon.fetch_position_status_reports().expect("positions");
    println!("MEASURED the position rows with ETH admitted: {positions:?}");
    assert!(positions.iter().any(|p| p.symbol == ETH), "a (flat) ETH position row is reported");
}
