//! **The real transport, and the lane this build mounts** (`tool-feeds-serve`): the one module that
//! links `ureq`. Everything the lane asks of a provider goes through [`ToolFeedHttpUreq`]; the providers'
//! addresses are [`ToolFeedEndpoints::production`], so this file names none of them.
//!
//! # ⚠ An error text from here names no URL and no credential
//!
//! The keyed providers take their key in a request header (Finnhub) or a query parameter (FMP), so a
//! URL can carry a credential, and `ureq`'s own error text is not promised to omit it.
//! `vike_transport::http::describe_error` therefore turns an error into a short fixed vocabulary (`HTTP 429`,
//! `timed out`, `I/O error (ConnectionRefused)`, `request failed`) and never formats the error itself. The lane
//! logs these strings, so this is where the rule "a credential reaches no log line" is held for the transport.

use std::sync::Arc;
use std::time::Duration;

use super::{
    ToolFeedCadences, ToolFeedEndpoints, ToolFeedHttp, ToolFeedHttpGet, ToolFeedKeyReaderStore,
    ToolFeedLane,
};

/// The blocking `ureq` transport. One agent per request: the cadences are minutes apart, so a pooled
/// connection would only ever be stale, and the per-request timeout belongs to the request.
#[derive(Debug, Default, Clone, Copy)]
pub struct ToolFeedHttpUreq;

/// The agent one request runs on: the network layer's keyless-GET builder
/// (`vike_transport::http::public_get_agent_bounded`) over the request's own timeout as a total bound, 4xx/5xx as
/// errors: the DNS resolver and at most `PUBLIC_GET_MAX_REDIRECTS`, five, followed before it gives up, `redirect
/// failed` (ureq's own default of ten was an accident of the library and not a decision).
/// The feeds are public pages and JSON (news RSS, ForexFactory, Nasdaq) that do move behind a redirect, so none is
/// refused; the keyed two (Finnhub, FMP) ride the same agent. Pinned by
/// `crates/vike-datahub/tests/default_build/tool_feeds.rs`'s `a_feed_that_redirects_is_followed_up_to_the_stated_limit`.
///
/// ⚠ ureq drops `Authorization` and `Cookie` on a redirect and nothing else: Finnhub's key travels in
/// the `X-Finnhub-Token` header and FMP's in the query, which a followed redirect would carry along. So a
/// request marked `ToolFeedHttpGet::carrying_a_key` follows NO redirect
/// (`vike_transport::http::request_no_redirect`), and its 3xx is the failure `HTTP 30x`; the keyless feeds keep
/// the stated limit. No keyed provider redirects (probed 2026-11-22).
fn feed_agent(timeout: Duration) -> ureq::Agent {
    use vike_transport::http::{CallBound, StatusPolicy, public_get_agent_bounded};
    public_get_agent_bounded(None, CallBound::Total(timeout), StatusPolicy::Error)
}

impl ToolFeedHttp for ToolFeedHttpUreq {
    fn get(&self, request: &ToolFeedHttpGet) -> Result<String, String> {
        let agent = feed_agent(request.timeout());
        let mut call = agent.get(request.url());
        for (name, value) in request.headers() {
            call = call.header(*name, value.as_str());
        }
        if request.carries_key() {
            call = vike_transport::http::request_no_redirect(call);
        }
        let mut response = call.call().map_err(|e| vike_transport::http::describe_error(&e))?;
        // Only a keyed request can end on a 3xx; its body is not the provider's answer.
        if response.status().is_redirection() {
            return Err(format!("HTTP {}", response.status().as_u16()));
        }
        response.body_mut().read_to_string().map_err(|e| vike_transport::http::describe_error(&e))
    }
}

/// **The lane this build mounts**: the real transport against the real providers, keys read from
/// `store` (the boot's credential store handle) each poll cycle, the system clock, each feed polled
/// on `cadences` (the box's rows, or [`ToolFeedCadences::default`]). Polls nothing until the first request.
pub fn real_tool_feeds_lane(
    store: vike_secrets::CredentialStore,
    cadences: ToolFeedCadences,
) -> ToolFeedLane {
    ToolFeedLane::new(
        Arc::new(ToolFeedHttpUreq),
        Arc::new(ToolFeedKeyReaderStore::new(store)),
        ToolFeedEndpoints::production(),
        cadences,
        Arc::new(vike_model::now_ms),
        true,
    )
}
