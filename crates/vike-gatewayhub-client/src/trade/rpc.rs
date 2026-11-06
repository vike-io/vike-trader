//! The two stateless trade queries: the arming table and the preflight legs. Each dials a fresh
//! `Scope::Trade` connection (a loopback dial plus one HMAC), asks one question and drops the
//! connection - the positional seams' rule (`crate::client`'s module doc) - so a daemon that is down is
//! one `Err` naming the address, and nothing is held between assemblies.
//!
//! No credential travels either way: the arming table is words and static facts, the preflight legs are
//! observations (`proto/backend_dto.rs`).

use std::io;

use vike_hub_proto::auth::Scope;

use crate::client::{GatewayClient, GatewayLink, unexpected};
use crate::proto::{
    ArmingTable, FEATURE_GW_ARMING, FEATURE_GW_PREFLIGHT, PreflightAnswer, PreflightStep, Request,
    Response,
};

/// Every venue's static facts and every account's arming row, as the daemon computes them from its own
/// account table and credential store. Fetched ONCE per assembly. A daemon that does not advertise
/// `gw_arming` is refused locally (nothing sent); a daemon-side refusal keeps the daemon's text.
pub fn arming_table(link: &GatewayLink) -> io::Result<ArmingTable> {
    let mut client = GatewayClient::connect(link, Scope::Trade)?;
    client.require_feature(FEATURE_GW_ARMING)?;
    match client.exchange(&Request::ArmingQuery)? {
        Response::ArmingTable(table) => Ok(*table),
        Response::Error(text) => Err(io::Error::other(text)),
        other => Err(unexpected(link.addr(), "ArmingTable", &other)),
    }
}

/// One preflight question of `venues`, measured at the daemon (`step` says which: the caller owns the
/// schedule, the daemon only measures). One dial per question, so a resample is a second call.
pub fn preflight(
    link: &GatewayLink,
    venues: &[String],
    step: PreflightStep,
) -> io::Result<PreflightAnswer> {
    let mut client = GatewayClient::connect(link, Scope::Trade)?;
    client.require_feature(FEATURE_GW_PREFLIGHT)?;
    match client.exchange(&Request::PreflightQuery { venues: venues.to_vec(), step })? {
        Response::PreflightLegs { legs, plan } => Ok(PreflightAnswer { legs, plan }),
        Response::Error(text) => Err(io::Error::other(text)),
        other => Err(unexpected(link.addr(), "PreflightLegs", &other)),
    }
}
