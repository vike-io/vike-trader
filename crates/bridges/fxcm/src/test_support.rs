//! The scripted Offers pieces the fxcm price tests share: this crate's `home_conversion`, `quotes` and
//! `quotes_stream` tests and the gatewayhub's session-guard tests over the price stream
//! (`crates/vike-gatewayhub/src/data/session_guard_bus_tests.rs`), which reach them through a dev edge with
//! `test-support`. DOCS-SHAPED rows, nothing measured off a ForexConnect session. Gated
//! `#[cfg(any(test, feature = "test-support"))]`, so no shipped build compiles it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::account::quote_rate::{INSTRUMENT_FOREX, Offer};
use crate::feed::{OffersSource, PriceDesk};

/// A scripted reconcile session: a table that can change, whether it exists, and how often it was asked.
pub struct ScriptedSession {
    /// The Offers table every read answers.
    pub table: Mutex<Vec<Offer>>,
    /// Whether the session is registered; a read of a session that is not answers the desk's refusal.
    pub up: Mutex<bool>,
    /// How many table reads were made.
    pub asked: AtomicUsize,
}

impl ScriptedSession {
    /// A registered session answering `table`.
    pub fn up(table: Vec<Offer>) -> Arc<Self> {
        Arc::new(Self {
            table: Mutex::new(table),
            up: Mutex::new(true),
            asked: AtomicUsize::new(0),
        })
    }
}

impl OffersSource for ScriptedSession {
    fn ready(&self) -> Result<(), String> {
        if *self.up.lock().unwrap() {
            Ok(())
        } else {
            Err("fxcm: no reconcile session is running for the default account".to_string())
        }
    }

    fn offers(&self) -> Result<Vec<Offer>, String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(self.table.lock().unwrap().clone())
    }
}

/// Three open forex pairs: EUR/USD, USD/JPY and GBP/USD.
pub fn standard_offers() -> Vec<Offer> {
    vec![
        offer("EUR/USD", 1.0998, 1.1002),
        offer("USD/JPY", 150.08, 150.12),
        offer("GBP/USD", 1.3498, 1.3502),
    ]
}

/// A desk over `session`, polling fast enough for a test (every 5 ms, a failure retried after 20 ms), with fxcm's kept
/// list held first (`crate::instrument::names::kept_list_for_tests`: the desk finds a USER name's Offers row through it).
pub fn paced_desk(session: &Arc<ScriptedSession>) -> Arc<PriceDesk> {
    crate::instrument::names::kept_list_for_tests();
    let source: Arc<dyn OffersSource> = Arc::clone(session) as Arc<dyn OffersSource>;
    PriceDesk::paced(source, Duration::from_millis(5), Duration::from_millis(20))
}

/// A forex offer `name` quoted `bid`/`ask`, subscribed and open, 5 digits, a 1000 lot, aged 0 ms.
pub fn offer(name: &str, bid: f64, ask: f64) -> Offer {
    Offer {
        instrument: name.to_string(),
        bid,
        ask,
        digits: 5,
        instrument_type: INSTRUMENT_FOREX,
        contract_currency: name.split('/').next().unwrap_or_default().to_string(),
        contract_multiplier: 1.0,
        subscription: "T".to_string(),
        trading_status: "O".to_string(),
        bid_tradable: "T".to_string(),
        ask_tradable: "T".to_string(),
        base_unit: 1000,
        quote_age_ms: Some(0),
        terms: None,
        offer_id: None,
        point_size: None,
    }
}

/// A session whose first `left` reads panic (a bug in the read), then answers as `inner` does.
pub struct PanicsFirst<S> {
    inner: Arc<S>,
    left: AtomicUsize,
}

impl<S> PanicsFirst<S> {
    /// `inner`, with its first `n` reads panicking.
    pub fn first(n: usize, inner: Arc<S>) -> Arc<Self> {
        Arc::new(Self { inner, left: AtomicUsize::new(n) })
    }

    /// Whether every planned panic has fired.
    pub fn spent(&self) -> bool {
        self.left.load(Ordering::SeqCst) == 0
    }
}

impl<S: OffersSource> OffersSource for PanicsFirst<S> {
    fn ready(&self) -> Result<(), String> {
        self.inner.ready()
    }

    fn offers(&self) -> Result<Vec<Offer>, String> {
        assert!(
            self.left.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_err(),
            "a bug in the session read"
        );
        self.inner.offers()
    }
}
