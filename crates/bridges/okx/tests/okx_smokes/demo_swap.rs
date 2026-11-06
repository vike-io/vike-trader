//! The demo (simulated-trading) SWAP client the OKX algo, algo-socket and restore smokes trade
//! through, built ONE way. The `okx_smokes` binary declares it ONCE by a plain `mod demo_swap;`, and
//! each of the three reads it as `crate::demo_swap`.

use serde_json::json;
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::now_ms;
use vike_okx::perp::{OkxPerpRest, PATH_INSTRUMENTS, REST, parse_okx_perp_instruments};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_secrets::credentials::Credentials;

/// One signed SWAP REST client on `symbol`, DEMO ONLY by construction: the credentials were asked
/// for as `AccountTier::Demo` (`load_demo_creds`) and every transport is `UreqOkxTransport::new(true)`
/// (the simulated-trading header, always on). The asserts are tripwires for a later edit of this
/// function, not a second fence: they check what the lines above them just set. A fresh call is a
/// fresh client: no state about any order.
pub fn build_rest(creds: &Credentials, symbol: &str) -> OkxPerpRest<UreqOkxTransport> {
    let public = UreqOkxTransport::new(true);
    assert!(public.simulated, "this smoke is DEMO ONLY: the simulated-trading header must be on");
    let info = public
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", symbol.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[symbol]
        .clone();
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: symbol.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    assert!(rest.transport.simulated, "this smoke is DEMO ONLY");
    assert_eq!(rest.base_url, REST, "this smoke talks to the shared OKX host only");
    rest
}
