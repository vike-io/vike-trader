//! **The options lane's engine**: the options surface (`OptionsRest`'s calls, the `OptionIndex` fetch and
//! the two feeds) answered by a venue's `OptionsSpec` — the engine half of
//! `crates/vike-bridge-core/src/options.rs`'s contract. The venue's provider, its listing, the names it
//! admits to a channel, its index fetch, its feeds and every word they refuse in are the spec's, in the
//! venue's bridge (Deribit's is `crates/bridges/deribit/src/options.rs`'s `OptionsSpecDeribit`, where the
//! ONE persistent provider and its cache live); this file holds no venue fact.
//!
//! The split with the datahub is the plan's D7: **the datahub keeps the poll thread, the folds, the strike
//! window and the chain recorder; this daemon serves the fetches and the feeds.**
//!
//! # What the engine does
//!
//! * **Each call maps onto one spec method**, and the answer back onto the wire's reply: `Underlyings` is
//!   the listing's names, `Listing` the listing under the spec's venue, and a `Chain`'s `strike_window`
//!   becomes the spec's strike count (`None`, the FULL chain the datahub records, stays `None`).
//! * **The index fetch checks the request first**: a fetch cancelled while it waited never reaches the
//!   spec. Its kind is the spec's word (`option`, `future`).
//! * **The focus sets are capped** by `vike_gatewayhub_client::proto::GATEWAY_OPTIONS_MAX_INDEXES` and
//!   `GATEWAY_OPTIONS_MAX_INSTRUMENTS` (`OptionsFeedSpec::refusal`, which also bounds a name's length and
//!   charset), re-checked here because a lane must not trust that the server ran it, and BEFORE the spec
//!   is asked for a feed. Whether a well-formed name is a REAL one is the spec's question.
//! * **A started feed is held by a handle that stops it ONCE**: at an explicit `shutdown` or at the drop.
//!   A feed for a socket that closed must not outlive it.
//!
//! # Cancellation: what stops and what cannot
//!
//! A spec call is a venue request with no probe; the index fetch checks the context before it. The feeds
//! are stopped by their handle, not by a context.

use std::sync::Arc;

use vike_bridge_core::options::{OptionsFeedStop, OptionsSpec};
use vike_data::source::SourceError;
use vike_gatewayhub_client::proto::{
    OptionIndexKind, OptionsCall, OptionsFeedSpec, OptionsListing, OptionsReply, OptionsUnderlying,
};
use vike_model::Instrument;

use super::{FeedOptionsHandle, FeedOptionsSink, FetchContext, cancelled};

/// The options half of the daemon: the engine over a venue's spec. See the module doc.
pub struct OptionsHub {
    spec: Box<dyn OptionsSpec>,
}

impl OptionsHub {
    /// The engine over `spec`.
    pub fn new(spec: Box<dyn OptionsSpec>) -> Self {
        Self { spec }
    }

    /// One call to the spec, mirroring the wire's calls one for one.
    pub fn call(&self, call: &OptionsCall) -> Result<OptionsReply, String> {
        match call {
            OptionsCall::Underlyings => self.spec.listing(false).map(|listed| {
                OptionsReply::Underlyings(listed.into_iter().map(|u| u.name).collect())
            }),
            OptionsCall::Listing { refresh } => self.spec.listing(*refresh).map(|listed| {
                OptionsReply::Listing(OptionsListing {
                    venue: self.spec.venue().to_string(),
                    underlyings: listed
                        .into_iter()
                        .map(|u| OptionsUnderlying { name: u.name, index: u.index })
                        .collect(),
                })
            }),
            OptionsCall::Expiries { underlying } => {
                self.spec.expiries(underlying).map(OptionsReply::Expiries)
            }
            OptionsCall::Chain { underlying, expiry, strike_window, r } => {
                // `None` is the FULL chain; `Some(n)` windows it to ±n strikes around spot.
                let strikes = strike_window.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
                self.spec
                    .chain(underlying, expiry, strikes, *r)
                    .map(|chain| OptionsReply::Chain(Box::new(chain)))
            }
        }
    }

    /// `Fetch { OptionIndex }`: the spec's contract index of one kind, as a whole or failed in its words.
    pub fn instruments(
        &self,
        kind: OptionIndexKind,
        ctx: &FetchContext,
    ) -> Result<Vec<Instrument>, SourceError> {
        let word = match kind {
            OptionIndexKind::Option => "option",
            OptionIndexKind::Future => "future",
        };
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        self.spec.instruments(word).map_err(|why| SourceError::Fetch(why.into()))
    }

    /// Start one options feed delivering into `sink`. The set is re-checked (the module doc).
    pub fn open_feed(
        &self,
        spec: &OptionsFeedSpec,
        sink: Arc<dyn FeedOptionsSink>,
    ) -> Result<Box<dyn FeedOptionsHandle>, String> {
        if let Some(why) = spec.refusal() {
            return Err(why);
        }
        let stop = match spec {
            OptionsFeedSpec::MarkPrice { indexes } => {
                self.spec.open_marks(indexes.clone(), Box::new(move |rows| sink.marks(rows)))?
            }
            OptionsFeedSpec::Ticker { instruments } => {
                self.spec.open_ticker(instruments.clone(), Box::new(move |row| sink.ticker(row)))?
            }
        };
        Ok(Box::new(FeedHandle(Some(stop))))
    }
}

impl std::fmt::Debug for OptionsHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionsHub").finish_non_exhaustive()
    }
}

/// A started feed, stopped (and joined) by its handle: ONCE, at the first of `shutdown` and the drop.
struct FeedHandle(Option<OptionsFeedStop>);

impl FeedOptionsHandle for FeedHandle {
    fn shutdown(&mut self) {
        if let Some(stop) = self.0.take() {
            stop();
        }
    }
}

impl Drop for FeedHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod options_tests;
