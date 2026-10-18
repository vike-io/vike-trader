//! **The real transport, and the lane this build mounts** (`tool-feeds-serve`): the one module that
//! links `ureq`. Everything the lane asks of a provider goes through [`UreqHttp`]; the providers'
//! addresses are [`Endpoints::production`], so this file names none of them.
//!
//! # ⚠ An error text from here names no URL and no credential
//!
//! The keyed providers take their key in a request header (Finnhub) or a query parameter (FMP), so a
//! URL can carry a credential, and `ureq`'s own error text is not promised to omit it. [`describe`]
//! therefore turns an error into a short fixed vocabulary (`HTTP 429`, `timed out`, `I/O error
//! (ConnectionRefused)`, `request failed`) and never formats the error itself. The lane logs these
//! strings, so this is where the rule "a credential reaches no log line" is held for the transport.

use std::sync::Arc;

use super::{Cadences, Endpoints, FeedHttp, HttpGet, StoreKeys, ToolFeedsLane};

/// The blocking `ureq` transport. One agent per request: the cadences are minutes apart, so a pooled
/// connection would only ever be stale, and the per-request timeout belongs to the request.
#[derive(Debug, Default, Clone, Copy)]
pub struct UreqHttp;

/// A short, fixed-vocabulary reason for a failed request (see the module doc).
fn describe(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        ureq::Error::Timeout(_) => "timed out".to_string(),
        ureq::Error::Io(io) => format!("I/O error ({:?})", io.kind()),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            "redirect failed".to_string()
        }
        ureq::Error::BodyExceedsLimit(_) => "the body was too large".to_string(),
        _ => "request failed".to_string(),
    }
}

impl FeedHttp for UreqHttp {
    fn get(&self, request: &HttpGet) -> Result<String, String> {
        let agent: ureq::Agent =
            ureq::Agent::config_builder().timeout_global(Some(request.timeout())).build().into();
        let mut call = agent.get(request.url());
        for (name, value) in request.headers() {
            call = call.header(*name, value.as_str());
        }
        let mut response = call.call().map_err(|e| describe(&e))?;
        response.body_mut().read_to_string().map_err(|e| describe(&e))
    }
}

/// **The lane this build mounts**: the real transport against the real providers, keys read from
/// `store` (the boot's credential store handle) each poll cycle, the system clock, each feed polled
/// on `cadences` (the box's rows, or [`Cadences::default`]). Polls nothing until the first request.
pub fn real_tool_feeds_lane(
    store: vike_secrets::CredentialStore,
    cadences: Cadences,
) -> ToolFeedsLane {
    ToolFeedsLane::new(
        Arc::new(UreqHttp),
        Arc::new(StoreKeys::new(store)),
        Endpoints::production(),
        cadences,
        Arc::new(vike_model::now_ms),
        true,
    )
}
