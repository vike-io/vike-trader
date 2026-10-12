//! `options_wire` — **the Options tool's ONE connection to the backend's datahub**, and the
//! sentence for each way a pass can end without an answer.
//!
//! The catalog route ([`crate::data::catalog_wire`]) is the template: the same [`CatalogDial`]
//! (address, key name, the desktop's OBSERVE pair — `Request::OptionsChain` is an Observe verb, so
//! that pair reaches it), the same capability check BEFORE a frame is written, and the same rule
//! that a refusal is the SERVER's sentence (`OptionsChainReply::describe`), never one assembled
//! here.
//!
//! It differs in one way, on purpose: the connection is HELD. The catalog dials once per human
//! press; this one is asked every second for as long as a window is open, and a keyed handshake is
//! a nonce round trip. It is dropped after ANY error (a failed read can leave the stream mid-frame,
//! and a second request on a desynced stream would read the wrong answer) and whenever the dial
//! names a different address or key, and re-dialled on the next pass.
//!
//! ⚠ A server that does not advertise [`FEATURE_OPTIONS_CHAIN`] is sent NOTHING, and is dropped
//! rather than held: a held connection that writes nothing could never notice the server being
//! upgraded and restarted under it.
//!
//! ⚠ **The held connection's replies are bounded by [`OPTIONS_REPLY_TIMEOUT`]**, set on every
//! dialled client: a peer that vanished without a FIN (a suspended laptop, a dropped tunnel) would
//! otherwise park the poll thread in a read for good. The timeout comes back as an ordinary `Err`,
//! so the pass drops the connection, keeps the last good chains, names the timeout in the status
//! and re-dials on the next pass.

use std::time::Duration;

use vike_datahub_client::options::OptionsChainReply;
use vike_datahub_client::{DatahubClient, FEATURE_OPTIONS_CHAIN};
use vike_node_proto::auth::Scope;

use super::options::{OPT_POLL_MS, OPTIONS_VENUE};
use crate::data::catalog_wire::{CatalogDial, NO_BACKEND_NOTE};

/// How long ONE options request may go without a reply byte before the pass calls the link dead.
///
/// `Request::OptionsChain` answers from the server's in-memory folded snapshot (no venue call, no
/// store read), so a healthy reply is milliseconds plus the link's round trip. Ten seconds is ten
/// missed polls: long enough that a loaded box or a slow tunnel is not mistaken for a dead one,
/// short enough that the status says so while the operator is still looking. A wrongly clipped
/// reply costs one re-dial a second later, never data (last good is kept).
pub(crate) const OPTIONS_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

const _: () = assert!(
    OPTIONS_REPLY_TIMEOUT.as_millis() >= 3 * OPT_POLL_MS as u128
        && OPTIONS_REPLY_TIMEOUT.as_secs() <= 60,
    "OPTIONS_REPLY_TIMEOUT must outlast a few polls (a busy box is not a dead one) and stay \
     short enough to report a dead link while the window is open"
);

/// How one pass ended. The first three asked nothing; `Asked` carries one answer per request, in
/// plan order, and stops at the first `Err` (the connection is dropped there).
#[derive(Debug)]
pub(crate) enum OptionsPass {
    /// No datahub address is resolved, so nothing was dialled.
    NoBackend,
    /// The dial (or the handshake riding it) failed; nothing was sent.
    ConnectFailed { addr: String, error: String },
    /// The server does not advertise [`FEATURE_OPTIONS_CHAIN`]; refused client-side, no frame sent.
    ServerUnsupported { addr: String, features: Vec<String> },
    /// The requests were sent: `(underlying, answer)` per request.
    Asked { addr: String, replies: Vec<(String, Result<OptionsChainReply, String>)> },
}

impl OptionsPass {
    /// The status sentence of a pass that asked nothing; `None` for [`Self::Asked`], whose
    /// sentence comes from its answers.
    pub(crate) fn not_asked_line(&self) -> Option<String> {
        match self {
            Self::NoBackend => Some(NO_BACKEND_NOTE.to_string()),
            Self::ConnectFailed { addr, error } => Some(format!(
                "the datahub at {addr} could not be reached ({error}) — no option chain was \
                 requested."
            )),
            Self::ServerUnsupported { addr, features } => Some(format!(
                "the datahub at {addr} serves no option chains: it does not advertise \
                 \"{FEATURE_OPTIONS_CHAIN}\" (it advertised [{}]) — nothing was sent. That server \
                 predates the verb: upgrade it.",
                features.join(", ")
            )),
            Self::Asked { .. } => None,
        }
    }
}

/// A request that was sent and came back as an error (the server's `Response::Error`, or a
/// transport failure mid-request).
pub(crate) fn failed_line(addr: &str, underlying: &str, error: &str) -> String {
    format!("the datahub at {addr} could not answer the {underlying} option chain: {error}")
}

/// Every request answered with chains.
pub(crate) fn live_line(addr: &str) -> String {
    format!("LIVE · Deribit via the datahub at {addr}")
}

/// A window is open, but no underlying is known yet to ask for: the instrument list that names
/// them has not been adopted. `note` is that list's own sentence, when it was refused or failed.
pub(crate) fn waiting_line(note: Option<&str>) -> String {
    match note {
        Some(note) => {
            format!("no option underlying is known yet — the deribit instrument list: {note}")
        }
        None => "waiting for the datahub's deribit instrument list".to_string(),
    }
}

/// The held connection, with the dial it was made for.
struct Held {
    addr: String,
    key_name: String,
    client: DatahubClient,
}

/// The Options tool's datahub connection, held across passes.
pub(crate) struct OptionsWire {
    held: Option<Held>,
    /// The idle reply bound set on every dialled client: [`OPTIONS_REPLY_TIMEOUT`] in production.
    reply_timeout: Duration,
}

impl Default for OptionsWire {
    fn default() -> Self {
        Self::with_reply_timeout(OPTIONS_REPLY_TIMEOUT)
    }
}

impl OptionsWire {
    /// A wire whose clients bound each reply by `reply_timeout` — the seam the loopback suite drives
    /// in milliseconds; production is [`Default`].
    pub(crate) fn with_reply_timeout(reply_timeout: Duration) -> Self {
        Self { held: None, reply_timeout }
    }

    /// **One pass over `plan`**, blocking — call it from the poll thread, never the UI thread.
    pub(crate) fn pass(
        &mut self,
        dial: Option<&CatalogDial>,
        plan: &[(String, Vec<String>)],
    ) -> OptionsPass {
        let Some(dial) = dial else {
            self.held = None;
            return OptionsPass::NoBackend;
        };
        let mut held = match self.held.take() {
            Some(h) if h.addr == dial.addr && h.key_name == dial.key_name => h,
            _ => {
                let dialled = match &dial.keys {
                    Some(k) => DatahubClient::connect_authed(&dial.addr, k, Scope::Read),
                    None => DatahubClient::connect(&dial.addr),
                };
                match dialled {
                    Ok(mut client) => {
                        client.set_reply_timeout(Some(self.reply_timeout));
                        Held { addr: dial.addr.clone(), key_name: dial.key_name.clone(), client }
                    }
                    Err(e) => {
                        return OptionsPass::ConnectFailed {
                            addr: dial.addr.clone(),
                            error: e.to_string(),
                        };
                    }
                }
            }
        };
        if !held.client.supports_options_chain() {
            return OptionsPass::ServerUnsupported {
                addr: dial.addr.clone(),
                features: held.client.features().to_vec(),
            };
        }
        let mut replies = Vec::with_capacity(plan.len());
        let mut intact = true;
        for (underlying, expiries) in plan {
            let answer = held.client.options_chain(OPTIONS_VENUE, underlying, expiries);
            intact = answer.is_ok();
            replies.push((underlying.clone(), answer));
            if !intact {
                break;
            }
        }
        if intact {
            self.held = Some(held);
        }
        OptionsPass::Asked { addr: dial.addr.clone(), replies }
    }
}
