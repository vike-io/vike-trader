//! The demo (simulated-trading) client the OKX algo, algo-socket, restore and spot smokes trade
//! through, built ONE way. The `okx_smokes` binary declares it ONCE by a plain `mod demo_swap;`, and
//! each member reads it as `crate::demo_swap`.

use serde_json::json;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_model::now_ms;
use vike_okx::execution::rest::{PATH_INSTRUMENTS, VenueRestOkx, parse_okx_perp_instruments};
use vike_okx::instrument::InstrumentType;
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_okx::transport::client::unwrap_okx;
use vike_secrets::credentials::Credentials;

/// One signed REST client on `symbol`, DEMO ONLY by construction: the credentials were asked
/// for as `AccountTier::Demo` (`load_demo_credentials`) and every transport is `rest_transport(true)`
/// (the simulated-trading header, always on). The instrument row is read under the `instType` the id's
/// shape names (`SWAP` for every swap id, as always; `SPOT` for a `BASE-QUOTE` pair), as the exec
/// thread's own foreign client does. The asserts are tripwires for a later edit of this
/// function, not a second fence: they check what the lines above them just set. A fresh call is a
/// fresh client: no state about any order.
pub fn build_rest(creds: &Credentials, symbol: &str) -> VenueRestOkx<RestTransportJsonUreq> {
    let net = demo_net();
    let public = rest_transport(true);
    assert_eq!(
        public.network(),
        vike_model::VenueNetwork::Testnet,
        "this smoke is DEMO ONLY: the simulated-trading header must be on"
    );
    let inst_type = InstrumentType::of_inst_id_or_swap(symbol).inst_type();
    let info = public
        .public(
            &net.rest,
            PATH_INSTRUMENTS,
            &[("instType", inst_type.into()), ("instId", symbol.into())],
        )
        .expect("public instruments");
    let instrument = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[symbol]
        .clone();
    let rest = VenueRestOkx {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: rest_transport(true)
            .with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        base_url: net.rest.clone(),
        symbol: symbol.to_string(),
        properties: instrument.properties,
        ct_val: instrument.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    assert_eq!(
        rest.transport.network(),
        vike_model::VenueNetwork::Testnet,
        "this smoke is DEMO ONLY"
    );
    assert_eq!(rest.base_url, net.rest, "this smoke talks to the demo account's own host only");
    rest
}
