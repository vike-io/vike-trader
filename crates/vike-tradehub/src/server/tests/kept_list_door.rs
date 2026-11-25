//! **A symbol refused for want of its venue's kept list is admitted once the list is written, with no restart**
//! (decision 0290; the owner's order of 2026-11-16, no restart for anything): the control door resolves a typed
//! symbol's user name through the venue's KEPT rows at each ask (`SymbolAdmitter::kept_rows`, by default the datahub's
//! list as this process reads it, read again on a miss), so the refusal names its fix and heals itself when the list
//! arrives. An oanda account's own session is what lists oanda (the gatewayhub's held list), so the account runs while
//! its list is missing and only a foreign symbol waits for the list.

use std::sync::{Arc, Mutex};

use vike_execution::{SymbolAdmission, VenueBlock};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::{AccountTier, Contract, ContractFx, Instrument};
use vike_tradehub_client::wire::{CommandWire, OrderRequestWire};

use crate::server::symbol_admission::{
    AccountReadiness, SymbolAdmitter, SymbolPlan, plan_submit_symbol,
};

/// A facts source whose oanda kept list is whatever the datahub has written so far (`None`: no list yet).
struct ListedLater {
    oanda: Mutex<Option<Arc<Vec<Instrument>>>>,
}

impl SymbolAdmitter for ListedLater {
    fn kept_rows(&self, venue: &str) -> Option<Arc<Vec<Instrument>>> {
        self.oanda.lock().unwrap().clone().filter(|_| venue == "oanda")
    }
    fn admission(
        &self,
        route_key: &str,
        _: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        Ok(crate::testutil::admission(route_key, symbol))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(1.25))
    }
}

impl AccountReadiness for ListedLater {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        true
    }
}

fn order(symbol: &str) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c-1".into(),
        venue: "oanda".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1000.0,
        order_type: "limit".into(),
        price: Some(1.25),
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

#[test]
fn a_symbol_refused_for_want_of_its_list_is_admitted_once_the_list_is_written() {
    let source = ListedLater { oanda: Mutex::new(None) };
    // The account runs (its session is the list's source) on its own pair, with no list kept yet.
    let engine = VenueBlock {
        venue: "oanda".into(),
        route_key: "oanda".into(),
        symbol: "EURUSD".into(),
        tier: Some(AccountTier::Demo),
        ..Default::default()
    };
    let plan = |typed: &str| {
        plan_submit_symbol(&order(typed), std::slice::from_ref(&engine), Some(&source), false)
    };
    // The engine's own symbol needs no list.
    assert!(!matches!(plan("EURUSD"), SymbolPlan::Refuse(_)), "{:?}", plan("EURUSD"));
    let SymbolPlan::Refuse(why) = plan("gbpusd") else { panic!("admitted with no list") };
    assert!(
        why.text.contains("no oanda kept list: run `vike-cli data catalog refresh --venue oanda`"),
        "the refusal names its fix: {why}"
    );

    // The datahub writes the list (the operator's refresh over the running session): the same order is admitted.
    let fx = |raw: &str, base: &str, quote: &str| {
        Instrument::new("oanda", raw, base, quote, Contract::Fx(ContractFx::default()))
    };
    // named as the datahub's write stores them (`vike_catalog::spelled`)
    *source.oanda.lock().unwrap() = Some(Arc::new(vike_catalog::spelled(vec![
        fx("EUR_USD", "EUR", "USD"),
        fx("GBP_USD", "GBP", "USD"),
    ])));
    match plan("gbpusd") {
        SymbolPlan::Place { symbol, admit, .. } => {
            assert_eq!(symbol, "GBPUSD", "placed on its user name");
            assert_eq!(admit.expect("admitted first").symbol, "GBPUSD");
        }
        other => panic!("not admitted after the list was written: {other:?}"),
    }
}
