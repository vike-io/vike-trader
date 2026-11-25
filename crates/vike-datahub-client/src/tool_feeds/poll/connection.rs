//! `connection` — **a datahub poll's ONE held connection to the backend's datahub**, and the
//! sentence for each way a News and Calendar pass can end without an answer. Both halves of the
//! poll hold one ([`DatahubPollConnection`]): the News and Calendar half asks `Request::NewsFeed`,
//! `Request::EconomicCalendar` and `Request::EquityCalendar` ([`DatahubPollConnection::pass`]), the
//! Options half asks `Request::OptionsChain` ([`DatahubPollConnection::options_pass`]).
//!
//! The dial is [`crate::link::held_or_dial`]'s, over the same [`DatahubDial`] (address, key name,
//! the desktop's OBSERVE pair: the four verbs are Observe verbs, so that pair reaches them), with
//! the capability check BEFORE a frame is written, and the rule that a refusal is the SERVER's
//! sentence (`DatahubPollOutcome::describe`, `OptionsChainReply::describe`), never one assembled here.
//!
//! The connection is HELD between passes (a keyed handshake is a nonce round trip; the options poll
//! asks every second for as long as a window is open), and dropped after ANY error (a failed read
//! can leave the stream mid-frame, and a second request on a desynced stream would read the wrong
//! answer) and whenever the dial names a different address or key, and re-dialled on the next pass.
//!
//! ⚠ A server that does not advertise the half's feature ([`FEATURE_TOOL_FEEDS`],
//! [`crate::FEATURE_OPTIONS_CHAIN`]) is sent NOTHING, and is dropped rather than held: a held connection
//! that writes nothing could never notice the server being upgraded and restarted under it. There
//! is no fallback to fetching the providers or Deribit directly.
//!
//! ⚠ **The held connection's replies are bounded** ([`FEEDS_REPLY_TIMEOUT`] for the News and
//! Calendar half, [`OPTIONS_REPLY_TIMEOUT`] for the Options half), set on every dialled client: a
//! peer that vanished without a FIN (a suspended laptop, a dropped tunnel) would otherwise park the
//! poll thread in a read for the client's 300 s default. The timeout comes back as an ordinary
//! `Err`, so the pass drops the connection, keeps the last good rows, names the timeout in the
//! status and re-dials on the next pass.

use std::time::Duration;

use super::options::{OPTIONS_POLL_MS, OPTIONS_VENUE, OptionsPass};
use crate::FEATURE_TOOL_FEEDS;
use crate::dial::{DatahubDial, NO_BACKEND_NOTE};
use crate::link::{DatahubLink, DatahubLinkError, held_or_dial};
use crate::tool_feeds::{CalendarEconomicReply, CalendarEquityReply, NewsReply};

/// How long ONE tool-feed request may go without a reply byte before the pass calls the link dead.
///
/// The three verbs answer from the server's in-memory cache (the first poll of a feed never holds
/// the reply: it answers `Warming`), so a healthy reply is milliseconds plus the link's round trip.
/// Twenty seconds is far past a loaded box or a slow tunnel and well inside the minute between
/// asks; a wrongly clipped reply costs one re-dial a minute later, never data.
pub(crate) const FEEDS_REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long ONE options request may go without a reply byte before the pass calls the link dead.
///
/// `Request::OptionsChain` answers from the server's in-memory folded snapshot (no venue call, no
/// store read), so a healthy reply is milliseconds plus the link's round trip. Ten seconds is ten
/// missed polls: long enough that a loaded box or a slow tunnel is not mistaken for a dead one,
/// short enough that the status says so while the operator is still looking. A wrongly clipped
/// reply costs one re-dial a second later, never data (last good is kept).
pub(crate) const OPTIONS_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

const _: () = assert!(
    OPTIONS_REPLY_TIMEOUT.as_millis() >= 3 * OPTIONS_POLL_MS as u128
        && OPTIONS_REPLY_TIMEOUT.as_secs() <= 60,
    "OPTIONS_REPLY_TIMEOUT must outlast a few polls (a busy box is not a dead one) and stay \
     short enough to report a dead link while the window is open"
);

/// One request of a News and Calendar pass. `if_generation` is the generation the tool already
/// holds for that feed (`None`: send the rows), so an unchanged feed costs one small frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DatahubPollAsk {
    News {
        if_generation: Option<u64>,
    },
    Economic {
        if_generation: Option<u64>,
    },
    /// `week_start` is the ISO Monday of a week a Calendar window shows (one ask per week).
    Equity {
        week_start: String,
        if_generation: Option<u64>,
    },
}

impl DatahubPollAsk {
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
pub(crate) enum DatahubPollReply {
    News(NewsReply),
    Economic(CalendarEconomicReply),
    Equity(CalendarEquityReply),
}

/// How one News and Calendar pass ended. The first three asked nothing; `Asked` carries one answer
/// per request, in plan order, and stops at the first `Err` (the connection is dropped there).
#[derive(Debug)]
pub(crate) enum DatahubPollPass {
    /// No datahub address is resolved, so nothing was dialled.
    NoBackend,
    /// The dial (or the handshake riding it) failed; nothing was sent.
    ConnectFailed { addr: String, error: DatahubLinkError },
    /// The server does not advertise [`FEATURE_TOOL_FEEDS`]; refused client-side, no frame sent.
    ServerUnsupported { addr: String, features: Vec<String> },
    /// The requests were sent: `(request, answer)` per request.
    Asked {
        addr: String,
        answers: Vec<(DatahubPollAsk, Result<DatahubPollReply, DatahubLinkError>)>,
    },
}

impl DatahubPollPass {
    /// The status sentence of a pass that asked nothing; `None` for [`Self::Asked`], whose
    /// sentences come from its answers.
    pub(crate) fn not_asked_line(&self) -> Option<String> {
        match self {
            Self::NoBackend => Some(NO_BACKEND_NOTE.to_string()),
            Self::ConnectFailed { addr, error } => Some(format!(
                "the datahub at {addr} could not be reached ({}) — nothing was requested.",
                error.cause
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

/// A poll's datahub connection, held across passes.
pub(crate) struct DatahubPollConnection {
    held: Option<DatahubLink>,
    /// The idle reply bound set on every dialled client: [`FEEDS_REPLY_TIMEOUT`] or
    /// [`OPTIONS_REPLY_TIMEOUT`] in production.
    reply_timeout: Duration,
}

impl DatahubPollConnection {
    /// A connection whose clients bound each reply by `reply_timeout` — the seam the loopback
    /// suites drive in milliseconds; production passes the half's own timeout.
    pub(crate) fn with_reply_timeout(reply_timeout: Duration) -> Self {
        Self { held: None, reply_timeout }
    }

    /// **One News and Calendar pass over `plan`**, blocking — call it from the poll thread, never
    /// the UI thread.
    pub(crate) fn pass(
        &mut self,
        dial: Option<&DatahubDial>,
        plan: &[DatahubPollAsk],
    ) -> DatahubPollPass {
        let Some(dial) = dial else {
            self.held = None;
            return DatahubPollPass::NoBackend;
        };
        let mut held = match held_or_dial(self.held.take(), dial, self.reply_timeout) {
            Ok(held) => held,
            Err(error) => {
                return DatahubPollPass::ConnectFailed { addr: dial.address.clone(), error };
            }
        };
        if !held.client.supports_tool_feeds() {
            return DatahubPollPass::ServerUnsupported {
                addr: dial.address.clone(),
                features: held.client.features().to_vec(),
            };
        }
        let mut answers = Vec::with_capacity(plan.len());
        let mut intact = true;
        for ask in plan {
            let answer = match ask {
                DatahubPollAsk::News { if_generation } => {
                    held.client.news_feed(*if_generation).map(DatahubPollReply::News)
                }
                DatahubPollAsk::Economic { if_generation } => {
                    held.client.economic_calendar(*if_generation).map(DatahubPollReply::Economic)
                }
                DatahubPollAsk::Equity { week_start, if_generation } => held
                    .client
                    .equity_calendar(week_start, *if_generation)
                    .map(DatahubPollReply::Equity),
            }
            .map_err(|cause| DatahubLinkError::after_request(cause, &held.client));
            intact = answer.is_ok();
            answers.push((ask.clone(), answer));
            if !intact {
                break;
            }
        }
        if intact {
            self.held = Some(held);
        }
        DatahubPollPass::Asked { addr: dial.address.clone(), answers }
    }

    /// **One Options pass over `plan`** (`(underlying, expiries)` per request), blocking — call it
    /// from the poll thread, never the UI thread.
    pub(crate) fn options_pass(
        &mut self,
        dial: Option<&DatahubDial>,
        plan: &[(String, Vec<String>)],
    ) -> OptionsPass {
        let Some(dial) = dial else {
            self.held = None;
            return OptionsPass::NoBackend;
        };
        let mut held = match held_or_dial(self.held.take(), dial, self.reply_timeout) {
            Ok(held) => held,
            Err(error) => return OptionsPass::ConnectFailed { addr: dial.address.clone(), error },
        };
        if !held.client.supports_options_chain() {
            return OptionsPass::ServerUnsupported {
                addr: dial.address.clone(),
                features: held.client.features().to_vec(),
            };
        }
        let mut replies = Vec::with_capacity(plan.len());
        let mut intact = true;
        for (underlying, expiries) in plan {
            let answer = held
                .client
                .options_chain(OPTIONS_VENUE, underlying, expiries)
                .map_err(|cause| DatahubLinkError::after_request(cause, &held.client));
            intact = answer.is_ok();
            replies.push((underlying.clone(), answer));
            if !intact {
                break;
            }
        }
        if intact {
            self.held = Some(held);
        }
        OptionsPass::Asked { addr: dial.address.clone(), replies }
    }
}
