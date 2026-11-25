//! The two stateless trade queries: the arming table and the preflight legs. Each dials a fresh
//! `Scope::Trade` connection (a loopback dial plus one HMAC), asks one question and drops the
//! connection - the positional seams' rule (`crate::client`'s module doc) - so a daemon that is down is
//! one `Err` naming the address, and nothing is held between assemblies.
//!
//! Each failure is a [`HubCallError`] (`crate::client`'s module doc says which phase builds which
//! variant): a daemon-side refusal is `Refused` with the daemon's text, a missing feature string
//! `NotOffered`, a daemon that is not there `NotSent`.
//!
//! No credential travels either way: the arming table is words and static facts, the preflight legs are
//! observations (`proto/backend_dto.rs`).

use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;

use crate::client::{GatewayClient, GatewayLink, unexpected};
use crate::proto::{
    ArmingTableWire, FEATURE_GATEWAY_ARMING, FEATURE_GATEWAY_PREFLIGHT, PreflightAnswer,
    PreflightStep, Request, Response,
};

/// Every venue's static facts and every account's arming row, as the daemon computes them from its own
/// account table and credential store. Fetched ONCE per assembly. A daemon that does not advertise
/// `gateway_arming` is refused locally (nothing sent); a daemon-side refusal keeps the daemon's text.
pub fn arming_table(link: &GatewayLink) -> Result<ArmingTableWire, HubCallError> {
    let mut client = GatewayClient::connect(link, Scope::Trade)?;
    client.require_feature(FEATURE_GATEWAY_ARMING)?;
    match client.exchange(&Request::ArmingQuery)? {
        Response::ArmingTable(table) => Ok(*table),
        Response::Error(text) => Err(HubCallError::Refused { id: None, text }),
        other => Err(unexpected(link.addr(), "ArmingTable", &other)),
    }
}

/// One preflight question of `venues`, measured at the daemon (`step` says which: the caller owns the
/// schedule, the daemon only measures). One dial per question, so a resample is a second call.
pub fn preflight(
    link: &GatewayLink,
    venues: &[String],
    step: PreflightStep,
) -> Result<PreflightAnswer, HubCallError> {
    let mut client = GatewayClient::connect(link, Scope::Trade)?;
    client.require_feature(FEATURE_GATEWAY_PREFLIGHT)?;
    match client.exchange(&Request::PreflightQuery { venues: venues.to_vec(), step })? {
        Response::PreflightLegs { legs, plan } => Ok(PreflightAnswer { legs, plan }),
        Response::Error(text) => Err(HubCallError::Refused { id: None, text }),
        other => Err(unexpected(link.addr(), "PreflightLegs", &other)),
    }
}
