//! Restore-into-registry LIVE practice smoke (the proof `vike_venues::venue_restore`'s oanda row waits for):
//! an order a PREVIOUS session left resting on the fxPractice account is found by the real
//! `OandaReconClient`, planned as ADOPT by `vike_exec::recon::plan_restore`, put into a FRESH
//! `ExecutionEngine` with `reregister_orders`, raises nothing in the diff, and is then cancelled THROUGH
//! that engine, by a client that has never seen it.
//!
//!     cargo test -p vike-oanda --test oanda_smokes -- --ignored --nocapture oanda_restore_smoke::
//!
//! ORDER-PLACING (practice): one LIMIT BUY of 1 unit of EUR_USD at 0.10, nowhere near any rate, so it cannot
//! fill. Absent credentials skip it, as for every member of this binary. A panic or a failed assertion still
//! cancels it (`Cleanup`); a run killed from outside leaves it resting: cancel it in the practice UI.

use crate::ingest_wait;
use std::time::Duration;

use vike_bridge_core::smoke::{adopts, plan_pass, restored_limit, wait_until_gone};
use vike_exec::recon::{Divergence, diff};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Ingest, Outbox, RiskGate,
    event_channel,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderStatus, RiskLimits, TimeInForce, now_ms};
use vike_oanda::{OandaConfig, OandaExecutionClient, OandaReconClient, OandaRest};

const SYMBOL: &str = "EURUSD";

/// Cancels the coid at the venue when dropped, so no failure path leaves it resting. A cancel of an order
/// already gone answers an error, which is the good case and only logged.
struct Cleanup<'a> {
    config: &'a OandaConfig,
    coid: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let path = format!("/v3/accounts/{}/orders/@{}/cancel", self.config.account_id, self.coid);
        let rest = OandaRest::new(self.config.api_token.clone());
        match rest.put_empty(&self.config.rest_base, &path) {
            Ok(_) => {
                tracing::info!(target: "vike_oanda::smoke", coid = self.coid, "cleanup: cancelled")
            }
            Err(e) => {
                tracing::info!(target: "vike_oanda::smoke", coid = self.coid, %e, "cleanup: nothing to cancel")
            }
        }
    }
}

fn wait_for(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    pred: impl FnMut(&Event) -> bool,
) -> Event {
    ingest_wait::wait_for(rx, Duration::from_secs(20), pred).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials: places and cancels a non-filling order (see module doc)"]
fn oanda_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = vike_oanda::load_oanda_config_from(AccountTier::Demo, &creds) else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };

    // --- 1. the "previous session": a far resting LIMIT BUY, and then the process is gone ----------
    let coid = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let (events, mut rx) = event_channel(64);
    let mut previous = OandaExecutionClient::spawn(config.clone(), events);
    let price = 0.10;
    previous.submit(&OrderRequest {
        client_order_id: coid.clone(),
        venue: "oanda".into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        time_in_force: TimeInForce::Gtc,
        ts: now_ms(),
        ..Default::default()
    });
    let answer = wait_for(&mut rx, |ev| {
        matches!(ev, Event::OrderAccepted(a) if a.client_order_id == coid)
            || matches!(ev, Event::OrderRejected(r) if r.client_order_id == coid)
    });
    assert!(matches!(answer, Event::OrderAccepted(_)), "practice refused the order: {answer:?}");
    let _cleanup = Cleanup { config: &config, coid: &coid };
    drop(previous);

    // --- 2. the restart: the real recon client, the planner ----------------------------------------
    let recon =
        OandaReconClient::connect(&config, SYMBOL, None).expect("the recon client connects");
    let restored =
        [restored_limit(&coid, "oanda", SYMBOL), restored_limit(&ghost, "oanda", SYMBOL)];
    let (plan, orders) = plan_pass(&recon, "oanda", SYMBOL, &restored);
    assert!(adopts(&plan, &coid), "the resting coid must be ADOPTED: {plan:?}");
    assert_eq!(plan.adopt.len(), 1, "only the real coid is adopted: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a client that has never seen the order -----------------------------
    let (events, mut rx) = event_channel(64);
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "oanda", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        OandaExecutionClient::spawn(config.clone(), events),
        "oanda",
        SYMBOL,
    );
    engine.client.adopt_restored(&[(coid.clone(), SYMBOL.to_string())]);
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let adopted = &engine.registry[&coid];
    assert!(adopted.status.is_live(), "restored live: {:?}", adopted.status);
    assert!(adopted.venue_order_id.is_some(), "the venue order id came across");
    assert_eq!((adopted.request.side, adopted.request.qty), (1, 1.0), "the venue's terms");
    let local = engine.local_view();
    let held: Vec<_> = diff(&orders, &[], &[], &local.as_view(), None)
        .into_iter()
        .filter(|d| {
            matches!(d, Divergence::UnknownOrder(o) if o.client_order_id.as_deref() == Some(coid.as_str()))
        })
        .collect();
    assert!(held.is_empty(), "an adopted order raises no UnknownOrder: {held:?}");

    // --- 4. the engine's own cancel, the venue's answer folded, and gone at the venue --------------
    assert!(engine.cancel_order(&coid), "the registry holds it, so the engine sends the cancel");
    let canceled = wait_for(&mut rx, |ev| {
        matches!(ev, Event::OrderCanceled(c) if c.client_order_id == coid)
            || matches!(ev, Event::OrderCancelRejected(r) if r.client_order_id == coid)
    });
    engine.on_event(&canceled, &mut Outbox::default());
    assert_eq!(
        engine.registry[&coid].status,
        OrderStatus::Canceled,
        "the engine's cancel of an adopted order must reach the venue: {canceled:?}"
    );
    wait_until_gone(&recon, "oanda", SYMBOL, &restored, &coid);

    engine.client.detach();
    tracing::info!(target: "vike_oanda::smoke", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
