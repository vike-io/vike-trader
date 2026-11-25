//! `-2011 CANCEL_REJECTED` is an UMBRELLA code on aster's fork as well (`vike_binance::transport::error_codes`):
//! only the words that say the order is not there are "gone". Swallowing every `-2011` reported a LIVE
//! order CANCELED. No aster answer has been captured: the words are binance's and `-2011 Unknown order
//! sent.` is what this crate's own tests already script for aster. A word aster spells otherwise costs a
//! non-terminal `OrderCancelRejected`, never a false `OrderCanceled`.

use std::sync::Mutex;

use serde_json::Value;
use vike_aster::execution::rest::perp::VenueRestAsterPerp;
use vike_aster::execution::rest::spot::VenueRestAsterSpot;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::SymbolProperties;
use vike_model::events::Event;

const GONE: &str = "Unknown order sent.";
const REFUSED: &str = "This account may not place or cancel orders.";

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Every request answers `-2011` in these words.
struct Script {
    words: &'static str,
    wire: Mutex<usize>,
}

impl RestTransport for Script {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        *self.wire.lock().unwrap() += 1;
        Err(VenueApiError { code: -2011, msg: self.words.to_string() })
    }
    vike_bridge_core::no_public_call!();
}

fn perp(words: &'static str) -> VenueRestAsterPerp<NullSigner, Script> {
    VenueRestAsterPerp {
        builder: None,
        signer: NullSigner,
        transport: Script { words, wire: Mutex::new(0) },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn spot(words: &'static str) -> VenueRestAsterSpot<NullSigner, Script> {
    VenueRestAsterSpot {
        builder: None,
        signer: NullSigner,
        transport: Script { words, wire: Mutex::new(0) },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".into(),
    }
}

#[test]
fn a_cancel_swallows_only_the_words_that_say_the_order_is_gone() {
    assert!(perp(GONE).cancel_order("c1").is_ok());
    assert!(spot(GONE).cancel_order("c1").is_ok());
    for words in [REFUSED, "Order was not canceled due to cancel restrictions.", ""] {
        assert_eq!(perp(words).cancel_order("c1").expect_err("live").code, -2011, "{words}");
        assert_eq!(spot(words).cancel_order("c1").expect_err("live").code, -2011, "{words}");
    }
}

/// A status read: `-2011` in the words that say unknown is ABSENT; in any other words it says nothing.
#[test]
fn a_status_read_answering_minus_2011_in_other_words_says_nothing() {
    assert!(matches!(perp(GONE).confirm_order("c1", 5)[..], [Event::OrderRejected(_)]));
    assert!(matches!(spot(GONE).confirm_order("c1", 5)[..], [Event::OrderRejected(_)]));
    assert!(perp(REFUSED).confirm_order("c1", 5).is_empty());
    assert!(spot(REFUSED).confirm_order("c1", 5).is_empty());
}
