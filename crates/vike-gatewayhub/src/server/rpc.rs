//! The positional (`rpc`) verbs: one request, one reply, on a connection that survives.
//!
//! Each verb is answered in three steps, in this order, so a refusal is always the cheapest one:
//! the build must serve the verb (otherwise `Response::Error` naming the feature string, decision 0112's
//! server leg), the venue must be one that lane serves (otherwise `Response::Error` naming the
//! roster), and only then does the lane run. Lane failures reach the peer UNCHANGED as
//! `Response::Error(text)`: the client forwards a server text verbatim, so the in-process error words
//! survive the hop. ⚠ A failure is an `Error`, never an empty answer - an empty family resolve would
//! unsubscribe every live stream and an empty catalog would be cached.
//!
//! The multi-frame `Fetch` family is `crate::server::fetch`'s; the mode switches (`DataOpen`,
//! `OptionsFeedOpen`) never reach this module.

use std::sync::Arc;

use vike_gatewayhub_client::proto::{
    FEATURE_GW_CATALOG, FEATURE_GW_FAMILY, FEATURE_GW_OPTIONS, FEATURE_GW_PRESENCE, Request,
    Response, request_kind,
};

use super::features::not_served;
use super::fetch;
use super::shared::{After, Conn};

/// Answer one positional request.
pub fn serve(conn: &mut Conn, request: Request) -> After {
    let shared = Arc::clone(&conn.shared);
    let rest = &shared.rest;
    let lane = &shared.lanes.rest;
    let response = match request {
        Request::Ping => Response::Pong,
        Request::GwStatus => {
            let report = shared.stats.report(
                &shared.data_venues,
                &rest.credentialed,
                lane.budget_counters(),
            );
            Response::StatusIs(Box::new(report))
        }
        Request::CatalogDescribe { venue } => {
            if rest.catalog.is_empty() {
                Response::Error(not_served("CatalogDescribe", FEATURE_GW_CATALOG))
            } else if !rest.catalog.contains(&venue) {
                Response::Error(roster_refusal("catalog", &venue, &rest.catalog))
            } else {
                match lane.catalog_describe(&conn.memo, &venue) {
                    Ok(traits) => Response::CatalogDescribed {
                        asset_classes: traits.asset_classes,
                        mode: traits.mode,
                    },
                    Err(text) => Response::Error(text),
                }
            }
        }
        Request::OptionsRest { call } => {
            if rest.options {
                match lane.options(&conn.memo, &call) {
                    Ok(reply) => Response::OptionsReplied(reply),
                    Err(text) => Response::Error(text),
                }
            } else {
                Response::Error(not_served("OptionsRest", FEATURE_GW_OPTIONS))
            }
        }
        Request::ResolveFamily { venue, family, now_ms } => {
            if rest.family.is_empty() {
                Response::Error(not_served("ResolveFamily", FEATURE_GW_FAMILY))
            } else if !rest.family.contains(&venue) {
                Response::Error(roster_refusal("family", &venue, &rest.family))
            } else {
                match lane.resolve_family(&conn.memo, &venue, &family, now_ms) {
                    Ok(symbols) => Response::FamilyResolved { symbols },
                    Err(text) => Response::Error(text),
                }
            }
        }
        Request::CredentialPresence { venue } => {
            if rest.presence {
                Response::PresenceIs { presence: lane.credential_presence(&venue) }
            } else {
                Response::Error(not_served("CredentialPresence", FEATURE_GW_PRESENCE))
            }
        }
        Request::Fetch { rid, what } => return fetch::serve(conn, rid, what, false),
        Request::FetchCredentialed { rid, what } => return fetch::serve(conn, rid, what, true),
        // No fetch is running (a running one reads its own cancel): a stray cancel after completion.
        // No reply is owed, and writing one would answer a request nobody is waiting on.
        Request::FetchCancel { .. } => return After::Continue,
        other @ (Request::Hello { .. }
        | Request::Auth { .. }
        | Request::DataOpen { .. }
        | Request::OptionsFeedOpen { .. }) => Response::Error(format!(
            "gatewayhub: {} is not a positional verb on an established connection",
            request_kind(&other)
        )),
    };
    match conn.reply(&response) {
        Ok(()) => After::Continue,
        Err(_) => After::Close,
    }
}

/// The text for a venue a served lane does not carry.
fn roster_refusal(lane: &str, venue: &str, roster: &[String]) -> String {
    format!(
        "gatewayhub: `{venue}` is not served by the {lane} lane (served: [{}]); nothing was asked of \
         any venue",
        roster.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_roster_refusal_names_the_venue_and_the_served_set() {
        let text =
            roster_refusal("catalog", "deribit", &["binance".to_string(), "bybit".to_string()]);
        assert!(text.contains("`deribit`") && text.contains("catalog lane"), "{text}");
        assert!(text.contains("binance, bybit"), "{text}");
    }
}
