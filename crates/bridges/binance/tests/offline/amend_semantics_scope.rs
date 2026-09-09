//! **The SCOPE tripwire for `vike_model::venue_amend`'s `BINANCE` row.**
//!
//! That row declares `AmendSemantics::InPlaceTotal` — the ONE value that can make the pre-trade gate
//! admit an amend the old arithmetic refused — and its evidence is the fapi Modify Order endpoint,
//! i.e. PERP. But `vike_model::amend_semantics` is keyed on the bare string `"binance"` while
//! `BinanceExecutionClient` routes spot vs perp internally (`crates/bridges/binance/src/exec.rs`'s
//! `split_symbol`, on the `.P` suffix), so the row answers for BOTH products.
//!
//! It is sound today for exactly one reason, and that reason lives in this crate rather than in the
//! table: **binance SPOT has no native amend at all.** `crates/bridges/binance/src/spot.rs`'s
//! `BinanceSpotRest` takes `VenueRest`'s DEFAULT no-op `modify_order`, so a spot amend never reaches
//! the venue and the row cannot mis-judge a real order.
//!
//! Binance spot DOES publish `POST /api/v3/order/cancelReplace`, which is
//! `AmendSemantics::CancelReplace` — the ANTI-conservative direction for this row. Wiring it without
//! splitting the row per product would leave the gate netting `filled_qty` out of an amend that
//! actually rests a FRESH order, admitting exposure it should refuse. Nothing else in the tree would
//! notice: the two products share one venue string, one `VenueCaps` row and one `AmendSemantics`
//! row. So the wiring is gated HERE, at the file that would have to change.

use vike_binance::perp::BinancePerpRest;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::{AmendSemantics, OrderRequest, SymbolProperties};

use std::sync::Mutex;

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Records every wire touch and fails the call, so a lane that DOES amend is observable without a
/// canned success body. The recorded `(method, path)` pairs are the evidence.
#[derive(Default)]
struct RecordWire {
    touched: Mutex<Vec<(String, String)>>,
}
impl RestTransport for RecordWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.touched.lock().unwrap().push((method.to_string(), path.to_string()));
        Err(VenueApiError { code: -1, msg: "offline".into() })
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        self.touched.lock().unwrap().push(("GET".to_string(), path.to_string()));
        Err(VenueApiError { code: -1, msg: "offline".into() })
    }
}

fn req() -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(50_000.0),
        ..Default::default()
    }
}

fn spot(transport: RecordWire) -> BinanceSpotRest<NullSigner, RecordWire> {
    BinanceSpotRest {
        signer: NullSigner,
        transport,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".to_string(),
        link_id: None,
    }
}

fn perp(transport: RecordWire) -> BinancePerpRest<NullSigner, RecordWire> {
    BinancePerpRest {
        signer: NullSigner,
        transport,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
        link_id: None,
    }
}

/// ⚠ **IF THIS FAILS, MOVE THE `BINANCE` ROW BEFORE SHIPPING THE WIRING.** The declared
/// `AmendSemantics::InPlaceTotal` is a PERP fact; it answers for spot only while spot sends no amend
/// at all. The same call is made on both lanes with the same transport, and exactly one of them
/// touches the wire.
///
/// The fix when spot's `cancelReplace` is wired is NOT to relax this test: it is to give the two
/// products their own rows — an engine mounted on spot must resolve `CancelReplace`, which nets
/// nothing, and one mounted on perp keeps `InPlaceTotal`.
#[test]
fn spot_has_no_native_amend_so_the_binance_amend_row_stays_perp_only() {
    // The row this test guards.
    assert_eq!(
        vike_model::amend_semantics("binance"),
        AmendSemantics::InPlaceTotal,
        "the row under guard"
    );

    // SPOT: the `VenueRest` default no-op. Nothing is sent, nothing comes back.
    let s = spot(RecordWire::default());
    let events = VenueRest::modify_order(&s, &req(), Some(2.0), Some(51_000.0));
    assert!(events.is_empty(), "spot's modify is the VenueRest default no-op — it emits nothing");
    assert!(
        s.transport.touched.lock().unwrap().is_empty(),
        "…and it must not touch the wire: binance spot has no amend endpoint wired. If this now \
         fails, `POST /api/v3/order/cancelReplace` was wired — CANCEL-REPLACE semantics — and \
         `vike_model::venue_amend`'s InPlaceTotal row is anti-conservative for spot until it is \
         split per product"
    );

    // PERP: the override the row's evidence is about — it genuinely amends.
    let p = perp(RecordWire::default());
    let _ = VenueRest::modify_order(&p, &req(), Some(2.0), Some(51_000.0));
    let touched = p.transport.touched.lock().unwrap().clone();
    assert_eq!(
        touched,
        vec![("PUT".to_string(), "/fapi/v1/order".to_string())],
        "perp amends in place via PUT /fapi/v1/order — the evidence the binance row rests on"
    );
}
