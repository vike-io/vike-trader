//! LIVE demo smoke (network + fxPractice creds, a 1-unit market round trip and one resting order on the practice
//! account), written 2026-11-09 and NOT RUN: **a reduce-only order says `REDUCE_ONLY`, and the venue keeps the word on
//! a resting order larger than its position**. Run by hand on a the CI box lane, on a practice account nobody else trades:
//!
//!     cargo test -p vike-oanda --test oanda_smokes -- --ignored --nocapture --test-threads=1 --exact \
//!         oanda_reduce_only_smoke::a_reduce_only_mit_larger_than_its_position_rests_saying_reduce_only
//!
//! **Why it exists.** Without a `positionFill` an OANDA order fills under the venue's `DEFAULT`: `REDUCE_FIRST` on a
//! netting account, `OPEN_ONLY` on a hedging one, so a reduce-only order whose position is gone OPENS one (a
//! take-profit that outlived its trade would). `crates/bridges/oanda/src/execution/mod.rs`'s `build_order_body_on` writes
//! `REDUCE_ONLY_FILL` on every reduce-only order; this proves the venue takes it:
//!
//! 1. EUR_USD must be flat at the start (else SKIP: nothing is touched). A 1-unit market BUY opens the long.
//! 2. A reduce-only `take_profit` (OANDA's MARKET_IF_TOUCHED) SELL of 2 units, MORE than the long, triggered at 2.0
//!    (far above any EUR/USD rate, so it never fires), through the REAL exec client (`spawn_execution_client`): it must be accepted.
//! 3. Its `pendingOrders` row must read `type` MARKET_IF_TOUCHED, `units` -2 and `positionFill` REDUCE_ONLY (the
//!    `CAPTURED` line); it is then cancelled and must be gone.
//! 4. A reduce-only market SELL of 1 unit (carrying the word too) closes the long; EUR_USD must end flat.
//!
//! On any failure a guard cancels the order and closes exactly this run's 1 unit at the venue. Only
//! `reduce-only smoke green` is a pass. What it does not prove: what the venue does when the trigger is reached with
//! less position than the order (the word says "only reduce"). `research/fx-flip-oanda`, which lists `take_profit` on
//! the OANDA capability row, waits on this smoke.
//!
//! Credentials: the default DEMO oanda account's `API_KEY` / `ACCOUNT_ID` fields via `vike_secrets::smoke_directory`;
//! absent -> SKIP. Not wired into `.github/workflows/live-smokes.yml`: it places an order that FILLS.

use std::cell::Cell;
use std::time::Duration;

use crate::ingest_wait::wait_for;
use serde_json::{Value, json};
use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, TimeInForce, now_ms};
use vike_oanda::{OandaConfig, OandaTransport, load_oanda_config_from, spawn_execution_client};

const INSTRUMENT: &str = "EUR_USD";

/// The units of EUR_USD the account holds, signed (0 when flat).
fn held(rest: &OandaTransport, config: &OandaConfig) -> f64 {
    let path = format!("/v3/accounts/{}/openPositions", config.account_id);
    let body = rest.get(&config.rest_base, &path, "").expect("openPositions");
    let units = |p: &Value, side: &str| {
        p.pointer(&format!("/{side}/units"))
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    body.get("positions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.get("instrument").and_then(Value::as_str) == Some(INSTRUMENT))
        .map(|p| units(p, "long") + units(p, "short"))
        .sum()
}

/// The venue's `pendingOrders` row named `order_client_id`.
fn pending_row(
    rest: &OandaTransport,
    config: &OandaConfig,
    order_client_id: &str,
) -> Option<Value> {
    let path = format!("/v3/accounts/{}/pendingOrders", config.account_id);
    let body = rest.get(&config.rest_base, &path, "").expect("pendingOrders");
    body.get("orders")?
        .as_array()?
        .iter()
        .find(|o| {
            o.pointer("/clientExtensions/id").and_then(Value::as_str) == Some(order_client_id)
        })
        .cloned()
}

fn order(
    order_client_id: &str,
    side: i32,
    qty: f64,
    kind: OrderType,
    trigger: Option<f64>,
    reduce_only: bool,
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: kind,
        trigger_price: trigger,
        reduce_only,
        time_in_force: if trigger.is_some() { TimeInForce::Gtc } else { TimeInForce::Ioc },
        timestamp: now_ms(),
        ..Default::default()
    }
}

/// Submit `req` through the REAL client and wait for its fill; panics with the venue's words otherwise.
fn expect_filled(
    client: &mut ExecutionClientActor,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    req: &OrderRequest,
) {
    let order_client_id = req.order_client_id.as_str();
    client.submit(req);
    let event = wait_for(rx, Duration::from_secs(30), |event| {
        matches!(event, Event::OrderFilled(f) if f.order_client_id == order_client_id)
            || matches!(event, Event::OrderRejected(r) if r.order_client_id == order_client_id)
            || matches!(event, Event::OrderCanceled(c) if c.order_client_id == order_client_id)
    })
    .unwrap_or_else(|e| panic!("{order_client_id}: {e}"));
    assert!(matches!(event, Event::OrderFilled(_)), "{order_client_id} did not fill: {event:?}");
}

/// Undoes this run at the venue when it drops armed: cancels the resting order, closes the run's 1 unit (never more).
struct Guard {
    rest: OandaTransport,
    config: OandaConfig,
    mit: String,
    long_open: Cell<bool>,
    armed: Cell<bool>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        let (base, account) = (&self.config.rest_base, &self.config.account_id);
        let cancel = self
            .rest
            .put_empty(base, &format!("/v3/accounts/{account}/orders/@{}/cancel", self.mit));
        let close = self.long_open.get().then(|| {
            let path = format!("/v3/accounts/{account}/positions/{INSTRUMENT}/close");
            self.rest.put_json(base, &path, &json!({ "longUnits": "1" }))
        });
        tracing::error!(
            target: "vike_oanda::smoke",
            cancel_ok = cancel.is_ok(), close = ?close.map(|c| c.is_ok()),
            "FAILED: the guard cancelled {} and closed this run's unit; check pendingOrders and EUR_USD by hand",
            self.mit
        );
    }
}

#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials + a 1-unit market round trip — run manually (see module doc)"]
fn a_reduce_only_mit_larger_than_its_position_rests_saying_reduce_only() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
    let urls = vike_venues::venue_urls::VenueUrls::seed("oanda");
    let Some(config) =
        load_oanda_config_from(AccountTier::Demo, &creds, &urls).expect("the seed's demo rows")
    else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };
    let rest = OandaTransport::new(config.api_token.expose().clone());
    let start = held(&rest, &config);
    if start != 0.0 {
        tracing::warn!(
            target: "vike_oanda::smoke", held = start,
            "SKIP: EUR_USD is not flat; this smoke touches only what it opens"
        );
        return;
    }

    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(config.clone(), events);
    let tag = now_ms() % 100_000_000;
    let guard = Guard {
        rest: OandaTransport::new(config.api_token.expose().clone()),
        config: config.clone(),
        mit: format!("vtroandaro{tag}"),
        long_open: Cell::new(false),
        armed: Cell::new(true),
    };

    // 1. The long the take-profit is for.
    expect_filled(
        &mut client,
        &mut rx,
        &order(&format!("vtroandaroe{tag}"), 1, 1.0, OrderType::Market, None, false),
    );
    guard.long_open.set(true);
    assert_eq!(held(&rest, &config), 1.0, "the venue holds this run's 1-unit long");

    // 2. A reduce-only market-if-touched SELL of 2 units over the long of 1, far above the market.
    client.submit(&order(&guard.mit, -1, 2.0, OrderType::TakeProfit, Some(2.0), true));
    let mit = guard.mit.as_str();
    match wait_for(&mut rx, Duration::from_secs(20), |event| {
        matches!(event, Event::OrderAccepted(a) if a.order_client_id == mit)
            || matches!(event, Event::OrderRejected(r) if r.order_client_id == mit)
    }) {
        Ok(Event::OrderAccepted(_)) => {}
        Ok(Event::OrderRejected(r)) => {
            panic!("the venue REFUSED the reduce-only MIT {mit}: {}", r.reason)
        }
        other => panic!("the reduce-only MIT {mit} got no answer: {other:?}"),
    }

    // 3. The venue holds it saying REDUCE_ONLY; then it is cancelled.
    let row = pending_row(&rest, &config, mit)
        .unwrap_or_else(|| panic!("the MIT {mit} must rest in pendingOrders"));
    tracing::info!(target: "vike_oanda::smoke", "CAPTURED pending reduce-only MIT row: {row}");
    assert_eq!(row["type"], "MARKET_IF_TOUCHED");
    assert_eq!(row["units"], "-2");
    assert_eq!(row["positionFill"], "REDUCE_ONLY");
    client.cancel(mit);
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |event| matches!(event, Event::OrderCanceled(c) if c.order_client_id == mit),
    )
    .unwrap_or_else(|e| panic!("the cancel of {mit}: {e}"));
    assert!(pending_row(&rest, &config, mit).is_none(), "the MIT {mit} is gone");

    // 4. A reduce-only market close of the long, saying the word too; EUR_USD ends flat.
    expect_filled(
        &mut client,
        &mut rx,
        &order(&format!("vtroandaroc{tag}"), -1, 1.0, OrderType::Market, None, true),
    );
    guard.long_open.set(false);
    assert_eq!(held(&rest, &config), 0.0, "EUR_USD ends flat");
    guard.armed.set(false);
    tracing::info!(
        target: "vike_oanda::smoke",
        "reduce-only smoke green: a reduce-only MIT of 2 over a long of 1 rested saying REDUCE_ONLY and was \
         cancelled, and a reduce-only market closed the long"
    );
    drop(client);
}
