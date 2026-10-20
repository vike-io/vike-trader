//! The node's balance gate: which bindings hold a venue's balance back, that the decorator holds exactly that and
//! forwards every other verb of the reconcile client as it is.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use super::*;

/// **Only an assumed, a declined or an unanchored binding holds a balance back, and only for the venues whose
/// balance carries no currency** (oanda, ig, fxcm): an anchored engine, and every venue that fences itself
/// (ctrader, dukascopy, ibkr) or is not converting (bybit), is handed its balance whatever the binding says.
#[test]
fn only_a_binding_that_disagrees_with_the_cash_holds_it_back_and_only_for_no_currency_venues() {
    let why = "the engine is mounted on `EURUSD`, quoted in USD, and the account's money is EUR";
    let bindings = [
        Binding::Anchored,
        Binding::Assumed,
        Binding::Declined { why },
        Binding::Unanchored { ccy: "EUR" },
    ];
    for venue in ["oanda", "ig", "fxcm"] {
        assert_eq!(held_reason(venue, &bindings[0]), None, "{venue}: anchored, handed on");
        let assumed = held_reason(venue, &bindings[1]).expect("assumed: held");
        assert!(assumed.starts_with(venue) && assumed.contains("only ASSUMED"), "{assumed}");
        let declined = held_reason(venue, &bindings[2]).expect("declined: held");
        assert!(declined.contains(why) && declined.contains("not adopted"), "{declined}");
        let unanchored = held_reason(venue, &bindings[3]).expect("unanchored: held");
        assert!(
            unanchored.contains("EUR") && unanchored.contains("does not convert"),
            "{unanchored}"
        );
    }
    for venue in ["ctrader", "dukascopy", "ibkr", "bybit", "binance"] {
        for binding in &bindings {
            assert_eq!(held_reason(venue, binding), None, "{venue} {binding:?}");
        }
    }
}

/// A reconcile client that records which verbs reached it and answers a balance of 25,000.
#[derive(Default)]
struct Recording {
    seen: Arc<Mutex<Vec<&'static str>>>,
}

impl Recording {
    fn note(&self, verb: &'static str) {
        self.seen.lock().unwrap().push(verb);
    }
}

impl ReconClient for Recording {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.note("orders");
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        self.note("fills");
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.note("positions");
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.note("balance");
        Ok(Some(25_000.0))
    }
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.note("fees");
        Ok(None)
    }
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        self.note("identity");
        Ok(Some("ACC-1".to_string()))
    }
    fn admit_symbol(&self, _symbol: &str) {
        self.note("admit");
    }
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        self.note("admitted");
        Some(vec!["GBPUSD".to_string()])
    }
}

fn gated(
    held: Option<&str>,
    untold: Option<Untold>,
) -> (Box<dyn ReconClient>, Arc<Mutex<Vec<&'static str>>>) {
    let inner = Recording::default();
    let seen = Arc::clone(&inner.seen);
    (gate(Box::new(inner), "oanda", held.map(str::to_string), untold), seen)
}

/// **A held balance is `Ok(None)` and the venue is never asked; every other verb crosses unchanged**, the
/// default `fetch_mass_status` included (it composes the three forwarded fetches, in the driver's order).
#[test]
fn a_held_balance_is_not_reported_and_not_asked_and_every_other_verb_is_forwarded() {
    let (client, seen) = gated(Some("assumed"), None);
    assert_eq!(client.fetch_balance(), Ok(None));
    assert!(seen.lock().unwrap().is_empty(), "the venue was not asked: {:?}", seen.lock().unwrap());

    assert_eq!(client.fetch_account_identity(), Ok(Some("ACC-1".to_string())));
    assert_eq!(client.fetch_fee_rates(), Ok(None));
    client.admit_symbol("GBPUSD");
    assert_eq!(client.admitted_symbols(), Some(vec!["GBPUSD".to_string()]));
    assert!(client.fetch_order_status_reports(0).unwrap().is_empty());
    assert!(client.fetch_fill_reports(0).unwrap().is_empty());
    assert!(client.fetch_position_status_reports().unwrap().is_empty());
    client.fetch_mass_status(0).expect("the default composition");
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "identity",
            "fees",
            "admit",
            "admitted",
            "orders",
            "fills",
            "positions", // each verb, once
            "orders",
            "fills",
            "positions", // fetch_mass_status
        ]
    );
}

/// With nothing held and nothing owed the balance is the inner client's, and so is everything else.
#[test]
fn a_gate_with_nothing_held_hands_the_balance_on() {
    let (client, seen) = gated(None, None);
    assert_eq!(client.fetch_balance(), Ok(Some(25_000.0)));
    assert_eq!(*seen.lock().unwrap(), ["balance"]);
}

/// **ibkr's boot window**: the balance is not asked while the daemon's client is owed a journaled foreign symbol,
/// and is asked once every one was told.
#[test]
fn a_balance_waits_for_the_journaled_foreign_symbols_to_be_told() {
    let told = Arc::new(AtomicBool::new(false));
    let untold: Untold = {
        let told = Arc::clone(&told);
        Box::new(move || {
            if told.load(Ordering::SeqCst) { Vec::new() } else { vec!["SAP.SMART.EUR".to_string()] }
        })
    };
    let (client, seen) = gated(None, Some(untold));
    assert_eq!(client.fetch_balance(), Ok(None), "owed a symbol");
    assert!(seen.lock().unwrap().is_empty());
    told.store(true, Ordering::SeqCst);
    assert_eq!(client.fetch_balance(), Ok(Some(25_000.0)), "told");
    // a held binding stays held whatever was told
    let (client, _) = gated(Some("assumed"), Some(Box::new(Vec::<String>::new)));
    assert_eq!(client.fetch_balance(), Ok(None));
}
