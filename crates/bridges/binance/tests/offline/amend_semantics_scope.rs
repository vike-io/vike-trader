//! **The SCOPE tripwire for `vike_venues::venue_amend`'s `BINANCE` row.**
//!
//! That row declares `AmendSemantics::InPlaceTotal` — the ONE value that can make the pre-trade gate
//! admit an amend the old arithmetic refused — and its evidence is the fapi Modify Order endpoint,
//! i.e. PERP. But `vike_venues::amend_semantics` is keyed on the bare string `"binance"` while
//! `BinanceExecutionClient` routes spot vs perp internally (`crates/bridges/binance/src/exec.rs`'s
//! `split_symbol`, on the `.P` suffix), so the row answers for BOTH products.
//!
//! It is sound for exactly one reason, and that reason lives in this crate rather than in the
//! table: **binance SPOT amends only an UNTOUCHED order.** `crates/bridges/binance/src/spot.rs`'s
//! `BinanceSpotRest::modify_order` is `POST /api/v3/order/cancelReplace`, which is
//! `AmendSemantics::CancelReplace` — the ANTI-conservative direction for this row — but it is sent
//! with `cancelRestrictions=ONLY_NEW`, so the venue refuses the cancel half of any order that has
//! executed. The gate therefore never nets a `filled_qty` out of a spot amend that really rests a
//! FRESH order: a spot amend is only ever accepted with `filled_qty == 0`, and
//! `AmendSemantics::already_in_position(0.0)` is `0.0` under every convention.
//!
//! Dropping the restriction (or widening the spot modify to partly filled orders) without splitting
//! the row per product would leave the gate netting `filled_qty` out of an amend that rests a FRESH
//! order, admitting exposure it should refuse. Nothing else in the tree would notice: the two
//! products share one venue string, one `VenueCaps` row and one `AmendSemantics` row. So the
//! invariant is gated HERE, at the file that would have to change.

use vike_binance::perp::BinancePerpRest;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::{OrderRequest, SymbolProperties};
use vike_venues::AmendSemantics;

use std::sync::Mutex;

use crate::tif_gate::NullSigner;

/// One recorded wire touch: `(method, path, params)`.
type Touch = (String, String, Vec<(String, String)>);

/// Records every wire touch and fails the call, so a lane that DOES amend is observable without a
/// canned success body. The recorded touches are the evidence.
#[derive(Default)]
struct RecordWire {
    touched: Mutex<Vec<Touch>>,
}
impl RestTransport for RecordWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.touched.lock().unwrap().push((
            method.to_string(),
            path.to_string(),
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        ));
        Err(VenueApiError { code: -1, msg: "offline".into() })
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        self.touched.lock().unwrap().push(("GET".to_string(), path.to_string(), Vec::new()));
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
        order_type: vike_model::OrderType::Limit,
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

/// ⚠ **IF THIS FAILS, MOVE THE `BINANCE` ROW BEFORE SHIPPING THE CHANGE.** The declared
/// `AmendSemantics::InPlaceTotal` is a PERP fact; it answers for spot only while a spot amend is
/// restricted to an order that has not executed. The same call is made on both lanes with the same
/// transport: spot reaches `cancelReplace` with `cancelRestrictions=ONLY_NEW`, perp amends in place.
///
/// The fix when spot starts amending a partly filled order is NOT to relax this test: it is to give
/// the two products their own rows — an engine on spot must resolve `CancelReplace`, which nets
/// nothing, and one started on perp keeps `InPlaceTotal`.
#[test]
fn spot_amends_only_untouched_orders_so_the_binance_amend_row_stays_sound() {
    // The row this test guards.
    assert_eq!(
        vike_venues::amend_semantics("binance"),
        AmendSemantics::InPlaceTotal,
        "the row under guard"
    );

    // SPOT: an atomic cancel-replace, restricted to an order with no executions.
    let s = spot(RecordWire::default());
    let events = VenueRest::modify_order(&s, &req(), Some(2.0), Some(51_000.0));
    assert!(!events.is_empty(), "a spot modify must answer — never a silent no-op");
    let touched = s.transport.touched.lock().unwrap().clone();
    assert_eq!(touched.len(), 1, "one request: {touched:?}");
    let (method, path, params) = &touched[0];
    assert_eq!((method.as_str(), path.as_str()), ("POST", "/api/v3/order/cancelReplace"));
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
    assert_eq!(
        get("cancelRestrictions"),
        Some("ONLY_NEW"),
        "spot cancel-replace rests a FRESH order of the full quantity: it must be refused for an \
         order that has executed, or `vike_venues::venue_amend`'s InPlaceTotal row is \
         anti-conservative for spot until it is split per product"
    );

    // PERP: the override the row's evidence is about — it genuinely amends in place.
    let p = perp(RecordWire::default());
    let _ = VenueRest::modify_order(&p, &req(), Some(2.0), Some(51_000.0));
    let touched: Vec<(String, String)> = p
        .transport
        .touched
        .lock()
        .unwrap()
        .iter()
        .map(|(m, p, _)| (m.clone(), p.clone()))
        .collect();
    assert_eq!(
        touched,
        vec![("PUT".to_string(), "/fapi/v1/order".to_string())],
        "perp amends in place via PUT /fapi/v1/order — the evidence the binance row rests on"
    );
}
