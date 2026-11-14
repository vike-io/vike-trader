//! The `GatewayLinks` verb: the venue sessions the gatewayhub recorded, read from the settings store this datahub already
//! opens. It dials nothing, so it answers with the gatewayhub down, which is when an operator wants it, and a read loop costs
//! one bounded read of one table. The table and its contract are `vike_secrets::link_sessions`'s. A row crosses as the store
//! read it (`vike_model::LinkSessionRow`, one type), so what an Observe key may read is that type's test's to pin.

use vike_datahub_client::gateway_links::GatewayLinksReply;
use vike_datahub_client::proto::Response;
use vike_secrets::CredentialStore;
use vike_secrets::link_sessions::LINK_SESSIONS_READ_LIMIT;

/// The answer of a datahub mounted with no store: a test's server. `crate::datahub_cli`'s `run` always mounts one.
const NO_STORE: &str =
    "this datahub was started with no settings store, so it holds no venue-session history";

/// The `Request::GatewayLinks` handler. A store that will not read is an error in words, never an empty history: that would
/// read as a venue with no drops.
pub(super) fn gateway_links_verb(
    store: Option<&CredentialStore>,
    since_ms: i64,
    route: Option<&str>,
    venue: Option<&str>,
) -> Response {
    let Some(store) = store else {
        return Response::Error(NO_STORE.to_string());
    };
    match store.link_sessions(since_ms, route, venue) {
        Ok(rows) => {
            let truncated = rows.len() >= LINK_SESSIONS_READ_LIMIT;
            Response::GatewayLinks(GatewayLinksReply { rows, truncated })
        }
        Err(e) => Response::Error(format!("the settings database could not be read: {e}")),
    }
}
