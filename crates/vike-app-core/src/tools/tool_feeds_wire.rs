//! `tool_feeds_wire` — **the News and Calendar tools' ONE connection to the backend's datahub**,
//! and the sentence for each way a pass can end without an answer.
//!
//! [`super::options_wire`] is the template, copied for the same reasons: the same [`CatalogDial`]
//! (address, key name, the desktop's OBSERVE pair: `Request::NewsFeed`, `Request::EconomicCalendar`
//! and `Request::EquityCalendar` are Observe verbs, so that pair reaches them), the same capability
//! check BEFORE a frame is written, and the same rule that a refusal is the SERVER's sentence
//! (`ToolFeedOutcome::describe`), never one assembled here.
//!
//! The connection is HELD between passes (a keyed handshake is a nonce round trip), and dropped
//! after ANY error (a failed read can leave the stream mid-frame, and a second request on a
//! desynced stream would read the wrong answer) and whenever the dial names a different address or
//! key.
//!
//! ⚠ A server that does not advertise [`FEATURE_TOOL_FEEDS`] is sent NOTHING, and is dropped
//! rather than held: a held connection that writes nothing could never notice the server being
//! upgraded and restarted under it. There is no fallback to fetching the providers directly.
//!
//! ⚠ **The held connection's replies are bounded by [`FEEDS_REPLY_TIMEOUT`]**, set on every dialled
//! client: a peer that vanished without a FIN (a suspended laptop, a dropped tunnel) would otherwise
//! park the poll thread in a read for the client's 300 s default. The timeout comes back as an
//! ordinary `Err`, so the pass drops the connection, keeps the last good rows, names the timeout in
//! the status and re-dials on the next pass.

use std::time::Duration;

use vike_datahub_client::tool_feeds::{EconomicCalendarReply, EquityCalendarReply, NewsReply};
use vike_datahub_client::{DatahubClient, FEATURE_TOOL_FEEDS};
use vike_node_proto::auth::Scope;

use crate::data::catalog_wire::{CatalogDial, NO_BACKEND_NOTE};

/// How long ONE tool-feed request may go without a reply byte before the pass calls the link dead.
///
/// The three verbs answer from the server's in-memory cache (the first poll of a feed never holds
/// the reply: it answers `Warming`), so a healthy reply is milliseconds plus the link's round trip.
/// Twenty seconds is far past a loaded box or a slow tunnel and well inside the minute between
/// asks; a wrongly clipped reply costs one re-dial a minute later, never data.
pub(crate) const FEEDS_REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// One request of a pass. `if_generation` is the generation the tool already holds for that feed
/// (`None`: send the rows), so an unchanged feed costs one small frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FeedAsk {
    News {
        if_generation: Option<u64>,
    },
    Economic {
        if_generation: Option<u64>,
    },
    /// `week_start` is the ISO Monday of the viewer's local week.
    Equity {
        week_start: String,
        if_generation: Option<u64>,
    },
}

impl FeedAsk {
    /// What the feed is called in a sentence about a failed request.
    pub(crate) fn feed_name(&self) -> &'static str {
        match self {
            Self::News { .. } => "news",
            Self::Economic { .. } => "economic calendar",
            Self::Equity { .. } => "equity calendar",
        }
    }
}

/// One answer, shaped like the request that earned it.
#[derive(Debug)]
pub(crate) enum FeedReply {
    News(NewsReply),
    Economic(EconomicCalendarReply),
    Equity(EquityCalendarReply),
}

/// How one pass ended. The first three asked nothing; `Asked` carries one answer per request, in
/// plan order, and stops at the first `Err` (the connection is dropped there).
#[derive(Debug)]
pub(crate) enum FeedsPass {
    /// No datahub address is resolved, so nothing was dialled.
    NoBackend,
    /// The dial (or the handshake riding it) failed; nothing was sent.
    ConnectFailed { addr: String, error: String },
    /// The server does not advertise [`FEATURE_TOOL_FEEDS`]; refused client-side, no frame sent.
    ServerUnsupported { addr: String, features: Vec<String> },
    /// The requests were sent: `(request, answer)` per request.
    Asked { addr: String, answers: Vec<(FeedAsk, Result<FeedReply, String>)> },
}

impl FeedsPass {
    /// The status sentence of a pass that asked nothing; `None` for [`Self::Asked`], whose
    /// sentences come from its answers.
    pub(crate) fn not_asked_line(&self) -> Option<String> {
        match self {
            Self::NoBackend => Some(NO_BACKEND_NOTE.to_string()),
            Self::ConnectFailed { addr, error } => Some(format!(
                "the datahub at {addr} could not be reached ({error}) — nothing was requested."
            )),
            Self::ServerUnsupported { addr, features } => Some(format!(
                "the datahub at {addr} serves no news or calendars: it does not advertise \
                 \"{FEATURE_TOOL_FEEDS}\" (it advertised [{}]) — nothing was sent. That server \
                 predates the verbs: upgrade it.",
                features.join(", ")
            )),
            Self::Asked { .. } => None,
        }
    }
}

/// A request that was sent and came back as an error (the server's `Response::Error`, or a
/// transport failure mid-request).
pub(crate) fn failed_line(addr: &str, feed: &str, error: &str) -> String {
    format!("the datahub at {addr} could not answer the {feed} request: {error}")
}

/// The held connection, with the dial it was made for.
struct Held {
    addr: String,
    key_name: String,
    client: DatahubClient,
}

/// The tools' datahub connection, held across passes.
pub(crate) struct ToolFeedsWire {
    held: Option<Held>,
    /// The idle reply bound set on every dialled client: [`FEEDS_REPLY_TIMEOUT`] in production.
    reply_timeout: Duration,
}

impl Default for ToolFeedsWire {
    fn default() -> Self {
        Self::with_reply_timeout(FEEDS_REPLY_TIMEOUT)
    }
}

impl ToolFeedsWire {
    /// A wire whose clients bound each reply by `reply_timeout` — the seam the loopback suite
    /// drives in milliseconds; production is [`Default`].
    pub(crate) fn with_reply_timeout(reply_timeout: Duration) -> Self {
        Self { held: None, reply_timeout }
    }

    /// **One pass over `plan`**, blocking — call it from the poll thread, never the UI thread.
    pub(crate) fn pass(&mut self, dial: Option<&CatalogDial>, plan: &[FeedAsk]) -> FeedsPass {
        let Some(dial) = dial else {
            self.held = None;
            return FeedsPass::NoBackend;
        };
        let mut held = match self.held.take() {
            Some(h) if h.addr == dial.addr && h.key_name == dial.key_name => h,
            _ => {
                let dialled =
                    DatahubClient::connect_with_keys(&dial.addr, dial.keys.as_ref(), Scope::Read);
                match dialled {
                    Ok(mut client) => {
                        client.set_reply_timeout(Some(self.reply_timeout));
                        Held { addr: dial.addr.clone(), key_name: dial.key_name.clone(), client }
                    }
                    Err(e) => {
                        return FeedsPass::ConnectFailed {
                            addr: dial.addr.clone(),
                            error: e.to_string(),
                        };
                    }
                }
            }
        };
        if !held.client.supports_tool_feeds() {
            return FeedsPass::ServerUnsupported {
                addr: dial.addr.clone(),
                features: held.client.features().to_vec(),
            };
        }
        let mut answers = Vec::with_capacity(plan.len());
        let mut intact = true;
        for ask in plan {
            let answer = match ask {
                FeedAsk::News { if_generation } => {
                    held.client.news_feed(*if_generation).map(FeedReply::News)
                }
                FeedAsk::Economic { if_generation } => {
                    held.client.economic_calendar(*if_generation).map(FeedReply::Economic)
                }
                FeedAsk::Equity { week_start, if_generation } => {
                    held.client.equity_calendar(week_start, *if_generation).map(FeedReply::Equity)
                }
            };
            intact = answer.is_ok();
            answers.push((ask.clone(), answer));
            if !intact {
                break;
            }
        }
        if intact {
            self.held = Some(held);
        }
        FeedsPass::Asked { addr: dial.addr.clone(), answers }
    }
}
