//! LIVE **testnet** smoke for the margin guard and the account's-own-network universe
//! (`crates/bridges/hyperliquid/src/exec/margin.rs`, `.../exec/universe.rs`): the proof the unit tests
//! (`exec_margin_tests.rs`, a fake venue) cannot give.
//!
//!     cargo test -p vike-hyperliquid --test hyperliquid_leverage_universe_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING, **TESTNET ONLY**: credentials come from `common`'s `demo_creds` (the default `demo`
//! hyperliquid account), the first assertion is that the network is the testnet, and nothing here names
//! the other tier. It rests ONE far-from-market GTC buy (15% under the mid, just over the 10 USD
//! minimum, cannot fill) and cancels it, and it MOVES one coin's margin mode on the testnet account and
//! puts it back: a `Drop` guard restores the original `activeAssetData` (type and figure) on every exit
//! path, and another cancels the order. A run killed from outside skips both: look at the coin named in
//! the `FINDING` lines and at the open orders of the testnet account.
//!
//! What it proves, in order (each prints a `FINDING` line, the measured answer):
//!   A. the two universes (public reads): the coins only one network lists, and that the asset indices
//!      differ between networks (so the exec universe MUST be the account's own);
//!   B. `updateLeverage` is accepted by the venue (the signature recovers): an idempotent cross at the
//!      figure the account holds;
//!   C. the guard: with the coin forced to ISOLATED (flat), the first order through the REAL exec client
//!      is preceded by the switch — afterwards the venue reports the coin CROSS at the same figure and
//!      the order is accepted; the order is cancelled and nothing rests;
//!   D. a coin only the OTHER network lists is refused in words naming the account's network, with
//!      nothing placed; and a coin only THIS network lists (when there is one today) is accepted.

mod common;
use common::demo_creds;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_catalog::CatalogProvider;
use vike_exec::{EventSender, ExecutionClient, Ingest, event_channel};
use vike_hyperliquid::base::config::Network;
use vike_hyperliquid::base::transport::HyperliquidTransport;
use vike_hyperliquid::feeds::catalog::VENUE_WORD_DELISTED;
use vike_hyperliquid::feeds::instruments::HyperliquidInstruments;
use vike_hyperliquid::leverage_cap::active_asset_data_body;
use vike_hyperliquid::signing::action::{Action, UpdateLeverageAction};
use vike_hyperliquid::signing::{NonceManager, Signer};
use vike_hyperliquid::{HyperliquidCatalog, HyperliquidExecutionClient};
use vike_model::events::Event;
use vike_model::{AssetClass, Instrument, OrderRequest, now_ms};

const VENUE: &str = "hyperliquid";

/// The listed (not delisted) core perps of a catalog read, by name.
fn perps(rows: &[Instrument]) -> BTreeSet<String> {
    rows.iter()
        .filter(|i| i.asset_class() == AssetClass::CryptoPerp)
        .filter(|i| i.contract_type.as_deref() != Some(VENUE_WORD_DELISTED))
        .map(|i| i.raw_symbol.clone())
        .collect()
}

/// What the account holds on `coin`: `(is_cross, figure)`.
fn held(transport: &HyperliquidTransport, master: &str, coin: &str) -> (bool, u32) {
    let answer = transport.info(&active_asset_data_body(master, coin)).expect("activeAssetData");
    let leverage = &answer["leverage"];
    let figure = leverage["value"].as_u64().expect("a leverage figure") as u32;
    (leverage["type"] == "cross", figure)
}

/// The signed size the account holds on `coin` (0 = flat).
fn position(transport: &HyperliquidTransport, master: &str, coin: &str) -> f64 {
    let state = transport
        .info(&json!({ "type": "clearinghouseState", "user": master }))
        .expect("clearinghouseState");
    state["assetPositions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["position"]["coin"] == coin)
        .filter_map(|row| row["position"]["szi"].as_str()?.parse::<f64>().ok())
        .sum()
}

/// Post one signed `updateLeverage` and return the venue's raw answer.
fn set_leverage(
    transport: &HyperliquidTransport,
    signer: &Signer,
    nonce: &NonceManager,
    asset: u32,
    is_cross: bool,
    leverage: u32,
) -> Value {
    let action = Action::UpdateLeverage(UpdateLeverageAction { asset, is_cross, leverage });
    transport.exchange(&action, signer, nonce.next(), None).expect("updateLeverage was posted")
}

/// Puts the coin's margin mode and figure back as the smoke found them, on every exit path.
struct RestoreLeverage {
    transport: HyperliquidTransport,
    signer: Signer,
    nonce: NonceManager,
    asset: u32,
    coin: String,
    original: (bool, u32),
}

impl Drop for RestoreLeverage {
    fn drop(&mut self) {
        // Never panics: this runs while an assertion may already be unwinding.
        let (is_cross, figure) = self.original;
        let action = Action::UpdateLeverage(UpdateLeverageAction {
            asset: self.asset,
            is_cross,
            leverage: figure,
        });
        let answer = self.transport.exchange(&action, &self.signer, self.nonce.next(), None);
        println!("FINDING: restored {} to cross={is_cross} {figure}x: {answer:?}", self.coin);
    }
}

/// Cancels `coid` through the placing client when dropped.
struct CancelOnDrop {
    client: HyperliquidExecutionClient,
    coid: String,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.client.cancel(&self.coid);
    }
}

type Pull = Box<dyn FnMut() -> Option<Ingest>>;

fn drain(rx: &mut Pull) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(ing) = rx() {
        if let Ingest::Event(e) = ing {
            out.push(e);
        }
    }
    out
}

/// Collect `rx`'s events until `done` accepts the accumulated list or 30 s pass.
fn wait_for(rx: &mut Pull, what: &str, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut all = Vec::new();
    loop {
        all.extend(drain(rx));
        if done(&all) {
            return all;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; saw {all:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn is_for(event: &Event, coid: &str) -> bool {
    match event {
        Event::OrderAccepted(e) => e.client_order_id == coid,
        Event::OrderRejected(e) => e.client_order_id == coid,
        Event::OrderCanceled(e) => e.client_order_id == coid,
        Event::OrderCancelRejected(e) => e.client_order_id == coid,
        _ => false,
    }
}

/// A far-from-market (15% under the mid) GTC buy of just over 10 USD on `coin`.
fn far_buy(coin: &str, coid: &str, mid: f64, sz_decimals: u32) -> OrderRequest {
    let price = (mid * 0.85).round().max(1.0);
    let step = 10f64.powi(-(sz_decimals as i32));
    let qty = ((11.0 / price) / step).ceil() * step;
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: coin.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        ts: now_ms(),
        ..Default::default()
    }
}

#[test]
#[ignore = "network + TESTNET creds — moves one coin's margin mode and rests+cancels one far testnet order — run manually (see module doc)"]
fn hyperliquid_margin_guard_and_the_accounts_own_universe_on_the_testnet() {
    vike_log::test_init();
    let Some(creds) = demo_creds() else { return };
    assert_eq!(creds.network, Network::Testnet, "this smoke places a real order: TESTNET ONLY");

    let signer = Signer::from_private_key(&creds.private_key, creds.network)
        .expect("the demo account's PRIVATE_KEY must be a valid secp256k1 key");
    let master = creds.account_address.clone().unwrap_or_else(|| signer.address().to_string());
    let transport = HyperliquidTransport::new(creds.network);
    let nonce = NonceManager::new();

    // --- A. the two universes (public reads) -------------------------------------------------------
    let mainnet_rows =
        HyperliquidCatalog::new().list_instruments().expect("the public mainnet list");
    let testnet_rows = HyperliquidCatalog::for_network(creds.network)
        .list_instruments()
        .expect("the public testnet list");
    let (mainnet_perps, testnet_perps) = (perps(&mainnet_rows), perps(&testnet_rows));
    let mainnet_only: Vec<&String> = mainnet_perps.difference(&testnet_perps).collect();
    let testnet_only: Vec<&String> = testnet_perps.difference(&mainnet_perps).collect();
    println!(
        "FINDING: listed core perps: mainnet {}, testnet {}; only on mainnet {mainnet_only:?}; only on testnet {testnet_only:?}",
        mainnet_perps.len(),
        testnet_perps.len()
    );
    let id = |rows: &[Instrument], coin: &str| {
        rows.iter().find(|i| i.raw_symbol == coin).and_then(|i| i.wire.asset_id)
    };
    println!(
        "FINDING: BTC asset id: mainnet {:?}, testnet {:?} (the exec universe must be the account's own)",
        id(&mainnet_rows, "BTC"),
        id(&testnet_rows, "BTC")
    );
    assert_ne!(id(&mainnet_rows, "BTC"), id(&testnet_rows, "BTC"), "the networks index BTC alike");

    let instruments = Arc::new(HyperliquidInstruments::load(&transport, false).expect("meta"));
    let mids = transport.info(&json!({ "type": "allMids" })).expect("allMids");
    let mid_of = |coin: &str| -> f64 {
        mids.get(coin).and_then(Value::as_str).and_then(|s| s.parse().ok()).expect("a live mid")
    };

    // --- B. a coin to work on: cross-capable, listed on both networks, and FLAT -----------------------
    let coin = ["ETH", "SOL", "BTC"]
        .into_iter()
        .find(|c| {
            instruments.symbology().by_symbol(c).is_some_and(|i| !i.only_isolated && !i.no_cross)
                && position(&transport, &master, c) == 0.0
        })
        .expect("one of ETH/SOL/BTC is cross-capable and flat on the testnet account");
    let inst = instruments.symbology().by_symbol(coin).expect("listed").clone();
    let original = held(&transport, &master, coin);
    println!(
        "FINDING: {coin} (asset {}) is held cross={} at {}x before the smoke",
        inst.asset_id, original.0, original.1
    );
    let _restore = RestoreLeverage {
        transport: HyperliquidTransport::new(creds.network),
        signer: Signer::from_private_key(&creds.private_key, creds.network).expect("key"),
        nonce: NonceManager::new(),
        asset: inst.asset_id,
        coin: coin.to_string(),
        original,
    };

    // B. `updateLeverage` is accepted: cross at the figure already held (idempotent for a cross coin).
    let accepted = set_leverage(&transport, &signer, &nonce, inst.asset_id, true, original.1);
    println!("FINDING: updateLeverage cross {}x -> {accepted}", original.1);
    assert_eq!(accepted["status"], "ok", "the venue refused the signed updateLeverage: {accepted}");

    // C. force the coin ISOLATED (flat), then let the real exec client place the first order.
    let forced = set_leverage(&transport, &signer, &nonce, inst.asset_id, false, original.1);
    println!("FINDING: updateLeverage isolated {}x -> {forced}", original.1);
    assert_eq!(forced["status"], "ok", "could not force the coin isolated: {forced}");
    assert_eq!(held(&transport, &master, coin), (false, original.1), "the venue shows it isolated");

    let (events, mut rx): (EventSender, _) = event_channel(1024);
    let client_signer = Signer::from_private_key(&creds.private_key, creds.network).expect("key");
    let mut client = HyperliquidExecutionClient::spawn(
        client_signer,
        master.clone(),
        Arc::clone(&instruments),
        events,
        None,
        None,
    );
    let mut pull: Pull = Box::new(move || rx.try_recv().ok());
    let coid = format!("vtlevuni{}", now_ms() % 100_000_000);
    let wire = client.wire_id_for(&coid).expect("a wire id");
    client.submit(&far_buy(coin, &coid, mid_of(coin), inst.sz_decimals));
    let cleanup = CancelOnDrop { client, coid: coid.clone() };
    let placed =
        wait_for(&mut pull, "the first order's accept", |evs| evs.iter().any(|e| is_for(e, &coid)));
    println!(
        "FINDING: first order on the isolated coin -> {:?}",
        placed.iter().find(|e| is_for(e, &coid))
    );
    assert!(
        placed.iter().any(|e| matches!(e, Event::OrderAccepted(a) if a.client_order_id == coid)),
        "the order must be ACCEPTED after the guard switched the coin: {placed:?}"
    );
    let after = held(&transport, &master, coin);
    println!("FINDING: {coin} after the first order: cross={} at {}x", after.0, after.1);
    assert_eq!(after, (true, original.1), "the guard must leave the coin CROSS at the same figure");

    // Cancel it and prove nothing rests.
    drop(cleanup);
    std::thread::sleep(Duration::from_secs(3));
    let open = transport
        .info(&json!({ "type": "frontendOpenOrders", "user": master }))
        .expect("frontendOpenOrders");
    let resting = open
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["cloid"].as_str() == Some(wire.as_str()))
        .count();
    assert_eq!(resting, 0, "the smoke's order still rests after the cancel: {open}");
    assert_eq!(position(&transport, &master, coin), 0.0, "the smoke ends flat");

    // --- D. the account's own network: refused in words / admitted --------------------------------------
    let (events, mut rx): (EventSender, _) = event_channel(1024);
    let mut client = HyperliquidExecutionClient::spawn(
        Signer::from_private_key(&creds.private_key, creds.network).expect("key"),
        master.clone(),
        Arc::clone(&instruments),
        events,
        None,
        None,
    );
    let mut pull: Pull = Box::new(move || rx.try_recv().ok());
    if let Some(foreign) = mainnet_only.first() {
        let foreign_coid = format!("vtlevfor{}", now_ms() % 100_000_000);
        let order =
            OrderRequest { symbol: (*foreign).clone(), ..far_buy(coin, &foreign_coid, 1.0, 2) };
        client.submit(&order);
        let seen = wait_for(&mut pull, "the foreign coin's refusal", |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == foreign_coid))
        });
        let reason = seen
            .iter()
            .find_map(|e| match e {
                Event::OrderRejected(r) if r.client_order_id == foreign_coid => {
                    Some(r.reason.to_string())
                }
                _ => None,
            })
            .expect("rejected");
        println!("FINDING: a coin only on mainnet ({foreign}) is refused: {reason}");
        assert!(reason.contains("testnet") && reason.starts_with("unknown symbol"), "{reason}");
    } else {
        println!(
            "FINDING: no coin is listed on mainnet and not on testnet today; the refusal is unit-tested only"
        );
    }
    if let Some(own) = testnet_only.first() {
        let inst =
            instruments.symbology().by_symbol(own).expect("the exec universe is the testnet's");
        let own_coid = format!("vtlevown{}", now_ms() % 100_000_000);
        client.submit(&far_buy(own, &own_coid, mid_of(own), inst.sz_decimals));
        let seen = wait_for(&mut pull, "the testnet-only coin's answer", |evs| {
            evs.iter().any(|e| is_for(e, &own_coid))
        });
        println!(
            "FINDING: a coin only on testnet ({own}) -> {:?}",
            seen.iter().find(|e| is_for(e, &own_coid))
        );
        client.cancel(&own_coid);
        std::thread::sleep(Duration::from_secs(3));
    } else {
        println!(
            "FINDING: no coin is listed on testnet and not on mainnet today, so none could be admitted live"
        );
    }
    drop(client);
}
