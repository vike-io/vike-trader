//! LIVE DEMO drills for the account modes the bridge now SETS (`crates/bridges/binance/src/modes.rs`):
//! network + demo creds, run by hand and never in a loop, NOT in `.github/workflows/live-smokes.yml`:
//!
//!     cargo test -p vike-binance --test binance_modes_smoke -- --ignored --nocapture --test-threads=1
//!
//! Each test prints `FINDING` lines (the measured answers the docs cite) and puts the account back in the
//! modes (and the leverage the start writes) it found it in, from a `Drop` guard that also cancels what the
//! drill rested and flattens what it opened, on the drill's two symbols ONLY. The account is SHARED: the
//! gatewayhub's binance lane twin trades it (`BTCUSDT` and a resting stop, measured by the census), so a
//! drill never touches a symbol but its own two, and the one drill that needs the WHOLE account flat (the
//! hedge switch: Binance switches the position mode only while the account holds nothing anywhere)
//! self-skips with a `SKIP:` line while the twin holds anything.
//!
//! * `binance_modes_census` — READ-ONLY. The account rows of the venue (ids, labels, tiers: no value), then the
//!   resolved account's position mode, Multi-Assets mode, margin type of the drill symbols, and what it
//!   holds.
//! * `binance_modes_the_venue_refuses_a_switch_while_something_is_held` — rests ONE far limit order on its own
//!   symbol and then opens a minimal position there, and at each step tries the two switches the bridge relies
//!   on being REFUSED, printing the venue's code and text: the proof behind `modes.rs`'s "the venue judges
//!   flatness". (The position-mode attempt is refused for whatever the ACCOUNT holds, the twin's included.)
//! * `binance_modes_a_start_switches_a_hedge_account_to_one_way` — needs the whole account flat. Puts it in
//!   HEDGE mode, starts the real `BinanceExecutionClient` for a perpetual, and requires that the START alone
//!   switches the account back to one-way and that a far limit order is then ACCEPTED (it is rejected `-4061`
//!   on the old code).
//! * `binance_modes_an_isolated_symbol_is_switched_to_cross_before_its_first_order` — puts the mounted symbol
//!   and a symbol the engine is not mounted on in ISOLATED margin (flat on both), requires the START to turn
//!   the mounted one to cross and the FIRST order on the other to turn it to cross, through the real client.
//!
//! **DEMO ONLY**: every request goes to `DEMO_FAPI_REST` (asserted), the account is the store's default demo
//! account (`vike_secrets::smoke_directory`, credentials never printed) and the owner's fence of account ids
//! no drill drives (`OFF_LIMITS`, the one `perp_algo_smoke.rs` holds) applies.

use std::time::{Duration, Instant};

use serde_json::Value;
use vike_binance::exec::BinanceExecutionClient;
use vike_binance::modes::{PATH_MARGIN_TYPE, PATH_POSITION_MODE, dual_side_position};
use vike_binance::perp::{
    DEMO_FAPI_REST, PATH_EXCHANGE_INFO, PATH_OPEN_ORDERS, PATH_POSITIONS,
    parse_binance_perp_instruments,
};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// The symbol the drill's engine is mounted on: not BTCUSDT, which the lane twin and the maker smokes mount.
const MOUNTED: &str = "ETHUSDT";
/// A second perpetual the engine is NOT mounted on.
const OTHER: &str = "SOLUSDT";
/// The owner's fence: these account ids are never driven by a drill, whatever their tier says.
const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];

fn finding(text: impl std::fmt::Display) {
    println!("FINDING: {text}");
}

// ---- the venue, raw ------------------------------------------------------------------------------

struct Venue {
    transport: UreqTransport,
    signer: BinanceHmacSigner,
}

/// What the account holds, all symbols.
struct Census {
    positions: Vec<String>,
    regular: usize,
    algo: usize,
}

impl Census {
    fn flat(&self) -> bool {
        self.positions.is_empty() && self.regular == 0 && self.algo == 0
    }
}

impl Venue {
    fn new(creds: &Credentials) -> Self {
        // DEMO ONLY: the one base URL this file ever signs against.
        assert_eq!(DEMO_FAPI_REST, "https://demo-fapi.binance.com", "demo host constant moved");
        Self {
            transport: UreqTransport::new("binance"),
            signer: BinanceHmacSigner::new(creds, now_ms),
        }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        self.transport.signed(DEMO_FAPI_REST, path, method, params, &self.signer)
    }

    fn read(&self, path: &str, params: &[(&str, String)]) -> Value {
        self.call("GET", path, params).unwrap_or_else(|e| panic!("GET {path} failed: {e}"))
    }

    fn dual(&self) -> bool {
        dual_side_position(&self.read(PATH_POSITION_MODE, &[]))
            .expect("GET positionSide/dual answered no dualSidePosition")
    }

    fn set_dual(&self, hedge: bool) -> Result<Value, VenueApiError> {
        self.call("POST", PATH_POSITION_MODE, &[("dualSidePosition", hedge.to_string())])
    }

    fn isolated(&self, symbol: &str) -> bool {
        let rows = self.read(PATH_POSITIONS, &[("symbol", symbol.to_string())]);
        vike_binance::modes::margin_is_isolated(&rows, symbol).expect("a marginType")
    }

    fn set_isolated(&self, symbol: &str, isolated: bool) -> Result<Value, VenueApiError> {
        let kind = if isolated { "ISOLATED" } else { "CROSSED" };
        self.call(
            "POST",
            PATH_MARGIN_TYPE,
            &[("symbol", symbol.to_string()), ("marginType", kind.to_string())],
        )
    }

    fn position(&self, symbol: &str) -> f64 {
        self.read(PATH_POSITIONS, &[("symbol", symbol.to_string())])
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("positionAmt")?.as_str()?.parse::<f64>().ok())
            .sum()
    }

    fn census(&self) -> Census {
        let positions = self
            .read(PATH_POSITIONS, &[])
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| {
                r.get("positionAmt")
                    .and_then(Value::as_str)
                    .and_then(|a| a.parse::<f64>().ok())
                    .is_some_and(|a| a != 0.0)
            })
            .map(|r| r["symbol"].as_str().unwrap_or("?").to_string())
            .collect();
        let count = |path: &str| self.read(path, &[]).as_array().map_or(0, Vec::len);
        Census {
            positions,
            regular: count(PATH_OPEN_ORDERS),
            algo: count("/fapi/v1/openAlgoOrders"),
        }
    }

    fn last_price(&self, symbol: &str) -> f64 {
        let t = self
            .transport
            .public(DEMO_FAPI_REST, "/fapi/v1/ticker/price", &[("symbol", symbol.to_string())])
            .expect("ticker/price");
        t["price"].as_str().and_then(|p| p.parse().ok()).expect("a ticker price")
    }

    fn properties(&self, symbol: &str) -> SymbolProperties {
        let info = self
            .transport
            .public(DEMO_FAPI_REST, PATH_EXCHANGE_INFO, &[("symbol", symbol.to_string())])
            .expect("exchangeInfo");
        parse_binance_perp_instruments(&info)[symbol].properties
    }
}

/// The resolved demo account, or `None` (with a `SKIP:` line) when it is absent or fenced off.
fn account() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        println!("SKIP: BINANCE_DEMO creds absent");
        return None;
    };
    let id = directory
        .account_row("binance", &AccountLabel::Default, AccountTier::Demo)
        .map(|row| row.id);
    println!("DRILL account id {id:?} (the default binance account at the demo tier)");
    if id.is_none_or(|id| OFF_LIMITS.contains(&id)) {
        println!("SKIP: the default binance demo account is off limits or unresolved ({id:?})");
        return None;
    }
    Some(creds)
}

/// The WHOLE account must hold nothing before a drill switches the position mode (the venue refuses
/// otherwise, and a shared account's other tenants are not ours to disturb).
fn require_account_flat(venue: &Venue) -> bool {
    let census = venue.census();
    if census.flat() {
        return true;
    }
    println!(
        "SKIP: the demo account is not flat/clean (positions on {:?}, {} regular and {} algo orders resting)",
        census.positions, census.regular, census.algo
    );
    false
}

/// `symbols` must hold nothing (a position, a regular or an algo order) before a drill switches THEIR margin
/// type: the venue's refusal is per symbol, and the account's other symbols are not the drill's.
fn require_flat_on(venue: &Venue, symbols: &[&str]) -> bool {
    for symbol in symbols {
        let one = [("symbol", (*symbol).to_string())];
        let orders = |path: &str| venue.read(path, &one).as_array().map_or(0, Vec::len);
        let (pos, regular, algo) =
            (venue.position(symbol), orders(PATH_OPEN_ORDERS), orders("/fapi/v1/openAlgoOrders"));
        if pos != 0.0 || regular != 0 || algo != 0 {
            println!(
                "SKIP: {symbol} is not flat/clean (position {pos}, {regular} regular and {algo} algo orders resting)"
            );
            return false;
        }
    }
    true
}

/// The leverage the account holds on `symbol` (`positionRisk`'s `leverage`), to put back after the start's
/// own write.
fn leverage_of(venue: &Venue, symbol: &str) -> Option<i64> {
    leverage_in(&venue.read(PATH_POSITIONS, &[("symbol", symbol.to_string())]))
}

/// [`leverage_of`] for the `Drop` guard: a failed read is `None`, never a panic.
fn leverage_of_quiet(venue: &Venue, symbol: &str) -> Option<i64> {
    leverage_in(&venue.call("GET", PATH_POSITIONS, &[("symbol", symbol.to_string())]).ok()?)
}

fn leverage_in(rows: &Value) -> Option<i64> {
    // MEASURED 2026-10-26: a symbol the account never traded reads `"0"` here, which no write can restore.
    rows.as_array()?.first()?.get("leverage")?.as_str()?.parse().ok().filter(|l| *l >= 1)
}

/// Puts the account back in the modes it was found in, cancels what the drill rested and flattens what it
/// opened, on the drill's two symbols only. Runs on a failing test too; NEVER panics (a second panic during
/// unwinding aborts before the cleanup is done).
struct Restore {
    creds: Credentials,
    hedge: bool,
    isolated: Vec<(&'static str, bool)>,
    /// The leverage of the symbol the drill's client is MOUNTED on, which every start writes (2x).
    leverage: Option<(&'static str, i64)>,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let v = Venue::new(&self.creds);
        for symbol in [MOUNTED, OTHER] {
            let sym = || ("symbol", symbol.to_string());
            let _ = v.call("DELETE", "/fapi/v1/allOpenOrders", &[sym()]);
            let amt: f64 = v
                .call("GET", PATH_POSITIONS, &[sym()])
                .ok()
                .and_then(|rows| {
                    Some(
                        rows.as_array()?
                            .iter()
                            .filter_map(|r| r.get("positionAmt")?.as_str()?.parse::<f64>().ok())
                            .sum(),
                    )
                })
                .unwrap_or(0.0);
            if amt != 0.0 {
                let params = vec![
                    sym(),
                    ("side", if amt > 0.0 { "SELL" } else { "BUY" }.to_string()),
                    ("type", "MARKET".to_string()),
                    ("quantity", format!("{}", amt.abs())),
                    ("reduceOnly", "true".to_string()),
                ];
                println!(
                    "CLEANUP flatten {symbol} {amt}: {}",
                    v.call("POST", "/fapi/v1/order", &params)
                        .map_or_else(|e| e.to_string(), |_| "sent".into())
                );
            }
        }
        std::thread::sleep(Duration::from_secs(2));
        for (symbol, isolated) in &self.isolated {
            if let Ok(now) = v.call("GET", PATH_POSITIONS, &[("symbol", (*symbol).to_string())])
                && vike_binance::modes::margin_is_isolated(&now, symbol).ok() != Some(*isolated)
            {
                println!(
                    "CLEANUP margin type of {symbol} back to isolated={isolated}: {:?}",
                    v.set_isolated(symbol, *isolated).map(|_| "done")
                );
            }
        }
        if let Some((symbol, was)) = self.leverage
            && leverage_of_quiet(&v, symbol) != Some(was)
        {
            println!(
                "CLEANUP leverage of {symbol} back to {was}x: {:?}",
                v.call(
                    "POST",
                    "/fapi/v1/leverage",
                    &[("symbol", symbol.to_string()), ("leverage", was.to_string())]
                )
                .map(|_| "done")
            );
        }
        match v.call("GET", PATH_POSITION_MODE, &[]).map(|b| dual_side_position(&b)) {
            Ok(Some(now)) if now != self.hedge => println!(
                "CLEANUP position mode back to hedge={}: {:?}",
                self.hedge,
                v.set_dual(self.hedge).map(|_| "done")
            ),
            Ok(_) => {}
            Err(e) => println!("CLEANUP could not read the position mode ({e}): CHECK IT BY HAND"),
        }
    }
}

/// A far-from-market limit BUY on `symbol` (half the last price, notional above the minimum): it must REST.
fn far_buy(venue: &Venue, symbol: &str, coid: &str) -> OrderRequest {
    let props = venue.properties(symbol);
    let px = venue.last_price(symbol) * 0.5;
    let qty = ((props.min_notional.max(5.0) * 1.6 / px).max(props.min_qty) / props.step_size)
        .ceil()
        * props.step_size;
    serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": format!("{symbol}.P"),
        "side": 1, "qty": qty, "order_type": "limit", "price": px, "ts": now_ms()
    }))
    .expect("an order")
}

/// Drain the client's events until `coid` reaches `want` (true) or `secs` pass (false).
fn wait_for(mut next: impl FnMut() -> Option<Ingest>, coid: &str, want: &str, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match next() {
            Some(Ingest::Event(e)) => {
                let (c, k, why) = match &e {
                    Event::OrderAccepted(x) => {
                        (x.client_order_id.clone(), "Accepted", String::new())
                    }
                    Event::OrderRejected(x) => {
                        (x.client_order_id.clone(), "Rejected", x.reason.to_string())
                    }
                    Event::OrderCanceled(x) => {
                        (x.client_order_id.clone(), "Canceled", String::new())
                    }
                    _ => continue,
                };
                println!("EVENT {c} -> {k} {why}");
                if c == coid && k == want {
                    return true;
                }
                if c == coid && k == "Rejected" {
                    return false;
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    false
}

// ---- the drills ----------------------------------------------------------------------------------

#[test]
#[ignore = "network + DEMO creds: READ-ONLY - run by hand (see module doc)"]
fn binance_modes_census() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    if let Some(Ok(accounts)) = directory.rows()
        && let Some(rows) = accounts.known()
    {
        for r in rows.iter().filter(|r| r.venue == "binance") {
            println!(
                "ACCOUNT id={} label={:?} tier={:?} active={}",
                r.id, r.label, r.tier, r.active
            );
        }
    }
    let Some(creds) = account() else { return };
    let venue = Venue::new(&creds);
    finding(format!("positionSide/dual -> {}", venue.read(PATH_POSITION_MODE, &[])));
    finding(format!("multiAssetsMargin -> {}", venue.read("/fapi/v1/multiAssetsMargin", &[])));
    for symbol in [MOUNTED, OTHER, "BTCUSDT"] {
        finding(format!(
            "{symbol} isolated={} leverage={:?} position={}",
            venue.isolated(symbol),
            leverage_of(&venue, symbol),
            venue.position(symbol)
        ));
    }
    let census = venue.census();
    finding(format!(
        "account holds: positions on {:?}, {} regular and {} algo orders resting",
        census.positions, census.regular, census.algo
    ));
}

#[test]
#[ignore = "network + DEMO creds: rests and cancels one order, opens and closes a minimal position - run by hand"]
fn binance_modes_the_venue_refuses_a_switch_while_something_is_held() {
    vike_log::test_init();
    let Some(creds) = account() else { return };
    let venue = Venue::new(&creds);
    if !require_flat_on(&venue, &[MOUNTED]) {
        return;
    }
    let (hedge0, iso0) = (venue.dual(), venue.isolated(MOUNTED));
    let _restore = Restore {
        creds: creds.clone(),
        hedge: hedge0,
        isolated: vec![(MOUNTED, iso0)],
        leverage: leverage_of(&venue, MOUNTED).map(|l| (MOUNTED, l)),
    };

    // 1. ONE far resting order on MOUNTED: both switches must be refused while it rests.
    let probe = |what: &str| {
        let census = venue.census();
        finding(format!(
            "{what}: the account holds positions on {:?}, {} regular and {} algo orders",
            census.positions, census.regular, census.algo
        ));
        let hedge = venue.set_dual(!hedge0);
        let margin = venue.set_isolated(MOUNTED, !iso0);
        finding(format!("{what}: POST dual={} -> {hedge:?}", !hedge0));
        finding(format!("{what}: POST marginType {MOUNTED} isolated={} -> {margin:?}", !iso0));
        assert!(hedge.is_err(), "{what}: the venue let the position mode change");
        assert!(margin.is_err(), "{what}: the venue let the margin type change");
    };
    let (events, mut rx) = event_channel(256);
    let mut client = BinanceExecutionClient::spawn_with_recorder(
        AccountTier::Demo,
        creds.clone(),
        format!("{MOUNTED}.P"),
        SymbolProperties::default(),
        events,
        None,
        None,
        None,
        2.0,
        false,
        std::sync::Arc::default(),
    );
    std::thread::sleep(Duration::from_secs(4));
    let coid = format!("vtmodes{}", now_ms() % 100_000_000);
    client.submit(&far_buy(&venue, MOUNTED, &coid));
    assert!(wait_for(|| rx.try_recv().ok(), &coid, "Accepted", 20), "{coid} was not accepted");
    probe("while an order rests");
    client.cancel(&coid);
    std::thread::sleep(Duration::from_secs(3));

    // 2. a minimal POSITION on MOUNTED (a market BUY that fills, flattened by the guard below).
    let props = venue.properties(MOUNTED);
    let qty = ((props.min_notional.max(5.0) * 1.6 / venue.last_price(MOUNTED)).max(props.min_qty)
        / props.step_size)
        .ceil()
        * props.step_size;
    let open = format!("vtmodesP{}", now_ms() % 100_000_000);
    let request: OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": open, "venue": "binance", "symbol": format!("{MOUNTED}.P"),
        "side": 1, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .expect("an order");
    client.submit(&request);
    let until = Instant::now() + Duration::from_secs(20);
    while venue.position(MOUNTED) == 0.0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(venue.position(MOUNTED) != 0.0, "the minimal position never opened");
    probe("while a position is open");
    client.detach();
    // `_restore` flattens the position, cancels anything left and puts the modes back.
}

#[test]
#[ignore = "network + DEMO creds: switches the demo account to HEDGE and back, rests and cancels one order - run by hand"]
fn binance_modes_a_start_switches_a_hedge_account_to_one_way() {
    vike_log::test_init();
    let Some(creds) = account() else { return };
    let venue = Venue::new(&creds);
    if !require_account_flat(&venue) {
        return;
    }
    let (hedge0, iso0) = (venue.dual(), venue.isolated(MOUNTED));
    finding(format!("the account was found in hedge={hedge0}, {MOUNTED} isolated={iso0}"));
    let _restore = Restore {
        creds: creds.clone(),
        hedge: hedge0,
        isolated: vec![(MOUNTED, iso0)],
        leverage: leverage_of(&venue, MOUNTED).map(|l| (MOUNTED, l)),
    };

    // The state the old code could not trade in: HEDGE (and the mounted symbol ISOLATED, to see both healed).
    if !hedge0 {
        venue.set_dual(true).expect("the demo account could not be put in hedge mode");
    }
    if !iso0 {
        venue.set_isolated(MOUNTED, true).expect("the mounted symbol could not be put in isolated");
    }
    assert!(venue.dual(), "the drill's precondition: hedge mode");
    assert!(venue.isolated(MOUNTED), "the drill's precondition: isolated margin");

    // THE START, as production does it.
    let (events, mut rx) = event_channel(256);
    let mut client = BinanceExecutionClient::spawn_with_recorder(
        AccountTier::Demo,
        creds.clone(),
        format!("{MOUNTED}.P"),
        SymbolProperties::default(),
        events,
        None,
        None,
        None,
        2.0,
        false,
        std::sync::Arc::default(),
    );
    let until = Instant::now() + Duration::from_secs(30);
    while venue.dual() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
    }
    finding(format!(
        "after the start: hedge={} {MOUNTED} isolated={}",
        venue.dual(),
        venue.isolated(MOUNTED)
    ));
    assert!(!venue.dual(), "the START did not switch the account to one-way");
    assert!(!venue.isolated(MOUNTED), "the START did not switch the mounted symbol to cross");

    // ...and an order goes through (on the old code: rejected -4061, hedge mode).
    let coid = format!("vtmodesH{}", now_ms() % 100_000_000);
    client.submit(&far_buy(&venue, MOUNTED, &coid));
    assert!(
        wait_for(|| rx.try_recv().ok(), &coid, "Accepted", 20),
        "{coid} was not accepted after the switch"
    );
    finding(format!("{coid} ACCEPTED on {MOUNTED}.P after the start's switch"));
    client.cancel(&coid);
    std::thread::sleep(Duration::from_secs(3));
    client.detach();
}

#[test]
#[ignore = "network + DEMO creds: switches one symbol to ISOLATED and back, rests and cancels one order - run by hand"]
fn binance_modes_an_isolated_symbol_is_switched_to_cross_before_its_first_order() {
    vike_log::test_init();
    let Some(creds) = account() else { return };
    let venue = Venue::new(&creds);
    if !require_flat_on(&venue, &[MOUNTED, OTHER]) {
        return;
    }
    let (hedge0, iso_mounted, iso_other) =
        (venue.dual(), venue.isolated(MOUNTED), venue.isolated(OTHER));
    finding(format!(
        "found: hedge={hedge0}, {MOUNTED} isolated={iso_mounted}, {OTHER} isolated={iso_other}"
    ));
    let _restore = Restore {
        creds: creds.clone(),
        hedge: hedge0,
        isolated: vec![(MOUNTED, iso_mounted), (OTHER, iso_other)],
        leverage: leverage_of(&venue, MOUNTED).map(|l| (MOUNTED, l)),
    };
    for (symbol, was) in [(MOUNTED, iso_mounted), (OTHER, iso_other)] {
        if !was {
            venue.set_isolated(symbol, true).expect("a symbol could not be put in isolated");
        }
        assert!(venue.isolated(symbol), "the drill's precondition: {symbol} on isolated margin");
    }

    let (events, mut rx) = event_channel(256);
    let mut client = BinanceExecutionClient::spawn_with_recorder(
        AccountTier::Demo,
        creds.clone(),
        format!("{MOUNTED}.P"),
        SymbolProperties::default(),
        events,
        None,
        None,
        None,
        2.0,
        false,
        std::sync::Arc::default(),
    );
    // The START turns the MOUNTED symbol to cross...
    let until = Instant::now() + Duration::from_secs(30);
    while venue.isolated(MOUNTED) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
    }
    finding(format!("after the start: {MOUNTED} isolated={}", venue.isolated(MOUNTED)));
    assert!(!venue.isolated(MOUNTED), "the START did not switch the mounted symbol to cross");
    assert!(venue.isolated(OTHER), "the start must leave a symbol it was not asked about alone");
    // ...and the FIRST order on a symbol the engine is not mounted on turns that one.
    let coid = format!("vtmodesI{}", now_ms() % 100_000_000);
    client.submit(&far_buy(&venue, OTHER, &coid));
    assert!(
        wait_for(|| rx.try_recv().ok(), &coid, "Accepted", 30),
        "{coid} was not accepted on {OTHER}.P"
    );
    finding(format!("{OTHER} isolated={} after its first order", venue.isolated(OTHER)));
    assert!(!venue.isolated(OTHER), "the first order did not switch {OTHER} to cross");
    client.cancel(&coid);
    std::thread::sleep(Duration::from_secs(3));
    client.detach();
}
