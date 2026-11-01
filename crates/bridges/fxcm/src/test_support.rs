//! The scripted Offers pieces the fxcm price tests share: this crate's `home_conversion` and `quotes`
//! tests and the gatewayhub's price-stream tests (`crates/vike-gatewayhub/src/data/fxcm_quotes_tests.rs`),
//! which reach them through a dev edge with `test-support`. DOCS-SHAPED rows, nothing measured off a
//! ForexConnect session. Gated `#[cfg(any(test, feature = "test-support"))]`, so no shipped build
//! compiles it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::home_conversion::{INSTRUMENT_FOREX, Offer};
use crate::quotes::OffersSource;

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
            self.left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_err(),
            "a bug in the session read"
        );
        self.inner.offers()
    }
}
