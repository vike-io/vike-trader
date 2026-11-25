//! LIVE demo smoke for the STOP-LIMIT order on the cTrader demo account, driving the REAL `ExecutionClientCtrader`
//! (through `live_runtime_for_account`, the seam production starts it through):
//!
//!     cargo test -p vike-ctrader --test integration -- --ignored --nocapture --test-threads=1 ctrader_stop_limit_smoke::
//!
//! ⚠ **NOT RUN YET, and not to be run before Monday 2026-11-09**: cTrader is an FX broker under the owner's
//! rule of 2026-11-06 (no broker contact until that Monday). It was written with the bridge code and the
//! offline tests; its first run is on Monday 2026-11-09 or later, by hand, on a the CI box lane
//! (`docs/ops/live-smoke-lane.md`), never scheduled.
//!
//! What it proves: a far BUY and a far SELL stop-limit on EURUSD (the trigger 200 pips from the market, the
//! limit 10 points past it), each built by `crates/bridges/ctrader/src/feed/decode.rs`'s
//! `order_to_new_order` as `STOP_LIMIT` with `stopPrice` and `slippageInPoints`, is ACCEPTED by the venue,
//! RESTS (the venue's own order list shows it, as a `stop_limit`), has its limit moved 10 points further IN
//! PLACE (`amend_order_req`: a new `slippageInPoints`, `stopPrice` restated; the venue's `ORDER_REPLACED`
//! must give back the new limit), and is CANCELLED; nothing is left. The order report each one reads back is
//! logged.
//!
//! What it does NOT prove, each a doubt of `crates/bridges/ctrader/CLAUDE.md`'s "A stop-limit is a stop with a
//! slippage range": the UNIT of `slippageInPoints` (read as `10^-digits`; on a five-digit symbol it is `1e-5`
//! whichever way it is read, so EURUSD cannot tell), whether a limit AT the trigger (0 points) is accepted,
//! and what the venue does with a stop-limit that triggered and could not fill inside its range. Nothing here
//! triggers: both orders are far from the market.
//!
//! DEMO ONLY (`AccountTier::Demo`; the adapter dials the demo host only), `#[ignore]`d, SELF-SKIPPING without
//! credentials, with the market closed (no quote), or when the account is not flat in EURUSD at the start (it
//! touches nothing it did not open). 1000 units (the minimum). It places REAL demo orders: never run it
//! casually. `ctrader_close_all_smoke` is the remedy if anything is ever left.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::adapter::{CtraderLiveRuntime, live_runtime_for_account};
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::feed::connect;
use vike_ctrader::test_support::RecordingSink;
use vike_data::DataClient;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, HaltAdmit, OrderRequest, OrderType, now_ms};

const SYMBOL: &str = "EURUSD";
const UNITS: f64 = 1000.0;
/// How far from the market each trigger sits: 200 pips, so nothing triggers while it rests.
const FAR: f64 = 0.0200;
/// How far past its trigger each limit sits: 10 points (`slippageInPoints = 10` at five digits).
const RANGE: f64 = 0.00010;
/// How long a venue answer (accept, cancel) may take before the step fails.
const STEP: Duration = Duration::from_secs(25);
/// How old a quote may be before it is too old to price an order from.
const QUOTE_FRESH_MS: i64 = 8_000;

fn round5(price: f64) -> f64 {
    (price * 100_000.0).round() / 100_000.0
}

/// Drain the lane into `events`, polling until `pred` holds over everything seen or `STEP` passes.
fn wait(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    events: &mut Vec<Event>,
    pred: impl Fn(&[Event]) -> bool,
) -> bool {
    let deadline = Instant::now() + STEP;
    loop {
        while let Ok(ing) = rx.try_recv() {
            if let Ingest::Event(e) = ing {
                events.push(e);
            }
        }
        if pred(events) || Instant::now() >= deadline {
            return pred(events);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The order client ids of the EURUSD orders the venue lists as resting, and the open EURUSD positions' count.
fn venue_state(runtime: &CtraderLiveRuntime) -> (Vec<String>, usize) {
    let recon = runtime.reconcile.as_ref().expect("the start's own reconcile client");
    let orders = recon.fetch_order_status_reports(0).expect("read the resting orders");
    let positions = recon.fetch_position_status_reports().expect("read the positions");
    let resting = orders
        .into_iter()
        .filter(|o| o.symbol == SYMBOL)
        .map(|o| {
            o.order_client_id
                .unwrap_or_else(|| format!("(no order client id) {}", o.venue_order_id))
        })
        .collect();
    (resting, positions.iter().filter(|p| p.symbol == SYMBOL && p.qty != 0.0).count())
}

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds: places REAL demo orders; not before Monday 2026-11-09 (see module doc)"]
fn ctrader_stop_limit_rests_and_cancels_on_the_demo_account() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let endpoints = vike_ctrader::test_support::demo_endpoints();
    let Some(config) = CtraderConfig::from_credentials(&endpoints, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent (live gate)");
        return;
    };
    let (tx, mut rx) = event_channel(1024);
    let mut runtime = live_runtime_for_account(
        &AccountLabel::Default,
        &credentials,
        &endpoints,
        None,
        SYMBOL,
        true,
        &tx,
        HaltAdmit::default(),
        // A sentinel this smoke owns and never creates.
        std::path::Path::new("ctrader-stop-limit-smoke-never-engaged/HALT"),
        None, // no FX login gate: a smoke logs in whenever it is run
    )
    .expect("creds present: the exec handshake must succeed");
    // Decision 0290: orders and quotes name a USER name, read through this account's own listing as the datahub keeps it.
    vike_ctrader::test_support::keep_the_accounts_listing(&runtime.symbols);

    // Quotes from a data-only socket (it cannot place anything), to price the far triggers.
    let sink = Arc::new(RecordingSink::default());
    let Ok(mut data) = connect(config.to_connection_config(), sink.clone()) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: data connect failed");
        return;
    };
    data.subscribe_quotes(SYMBOL).expect("subscribe EURUSD quotes");
    let deadline = Instant::now() + Duration::from_secs(45);
    let quote = loop {
        let fresh = sink
            .quotes()
            .last()
            .filter(|(_, _, q)| now_ms() - q.local_timestamp <= QUOTE_FRESH_MS)
            .map(|(_, _, q)| (q.bid, q.ask));
        if fresh.is_some() || Instant::now() >= deadline {
            break fresh;
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    let Some((bid, ask)) = quote else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: no fresh EURUSD quote (FX closed)");
        return;
    };
    let (resting, positions) = venue_state(&runtime);
    if !resting.is_empty() || positions != 0 {
        tracing::warn!(
            target: "vike_ctrader::smoke",
            ?resting, positions,
            "SKIP: the account is not flat in EURUSD; this smoke touches only what it opens"
        );
        return;
    }

    let mut events = Vec::new();
    let stamp = now_ms() % 10_000_000;
    let orders: Vec<OrderRequest> = [(1, ask + FAR), (-1, bid - FAR)]
        .into_iter()
        .map(|(side, trigger)| {
            let trigger = round5(trigger);
            OrderRequest {
                order_client_id: format!("vtsl-{}-{stamp}", if side > 0 { "buy" } else { "sell" }),
                venue: "ctrader".into(),
                symbol: SYMBOL.into(),
                side,
                qty: UNITS,
                order_type: OrderType::StopLimit,
                trigger_price: Some(trigger),
                price: Some(round5(trigger + f64::from(side) * RANGE)),
                timestamp: now_ms(),
                ..Default::default()
            }
        })
        .collect();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for order in &orders {
            let order_client_id = order.order_client_id.clone();
            runtime.client.submit(order);
            assert!(
                wait(&mut rx, &mut events, |e| e.iter().any(
                    |x| matches!(x, Event::OrderAccepted(a) if a.order_client_id == order_client_id)
                )),
                "{order_client_id} was not accepted within {STEP:?}: {events:?}"
            );
            let recon = runtime.reconcile.as_ref().expect("reconcile client");
            let report = recon
                .fetch_order_status_reports(0)
                .expect("read the resting orders")
                .into_iter()
                .find(|o| o.order_client_id.as_deref() == Some(order_client_id.as_str()));
            tracing::info!(target: "vike_ctrader::smoke", %order_client_id, ?report, "the venue's report of the resting order");
            let report =
                report.unwrap_or_else(|| panic!("{order_client_id} does not rest at the venue"));
            assert_eq!(
                report.order_type,
                OrderType::StopLimit,
                "{order_client_id} rests as a stop_limit"
            );

            // Move the limit in place: 10 more points past the trigger. The venue's ORDER_REPLACED
            // carries its own stop and slippage, from which the bridge rebuilds the limit it reports.
            let trigger = order.trigger_price.expect("a stop-limit has a trigger");
            let new_limit = round5(trigger + f64::from(order.side) * 2.0 * RANGE);
            runtime.client.modify(order, None, Some(new_limit));
            assert!(
                wait(&mut rx, &mut events, |e| e.iter().any(|x| matches!(
                    x,
                    Event::OrderModified(m) if m.order_client_id == order_client_id
                ))),
                "{order_client_id} was not amended within {STEP:?}: {events:?}"
            );
            let reported = events.iter().rev().find_map(|x| match x {
                Event::OrderModified(m) if m.order_client_id == order_client_id => {
                    Some(m.new_price)
                }
                _ => None,
            });
            tracing::info!(target: "vike_ctrader::smoke", %order_client_id, new_limit, ?reported, "amended in place");
            assert_eq!(
                reported,
                Some(Some(new_limit)),
                "{order_client_id}: the venue's limit after the amend"
            );
        }
        for order in &orders {
            let order_client_id = order.order_client_id.clone();
            runtime.client.cancel(&order_client_id);
            assert!(
                wait(&mut rx, &mut events, |e| e.iter().any(
                    |x| matches!(x, Event::OrderCanceled(c) if c.order_client_id == order_client_id)
                )),
                "{order_client_id} was not cancelled within {STEP:?}"
            );
        }
    }));
    if outcome.is_err() {
        tracing::error!(target: "vike_ctrader::smoke", "FAILED: cancelling what this run placed before failing");
        for order in &orders {
            runtime.client.cancel(&order.order_client_id);
        }
    }
    let deadline = Instant::now() + STEP;
    let (resting, positions) = loop {
        let state = venue_state(&runtime);
        if (state.0.is_empty() && state.1 == 0) || Instant::now() >= deadline {
            break state;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    tracing::info!(target: "vike_ctrader::smoke", ?resting, positions, "END STATE");
    assert!(
        resting.is_empty() && positions == 0,
        "left behind: orders {resting:?}, positions {positions}: run ctrader_close_all_smoke"
    );
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
