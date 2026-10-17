//! **The Deribit options surface**: ONE persistent `DeribitOptionsProvider` behind a `Mutex`, the
//! instrument-index fetch, and the two public feeds (`markprice.options` and `ticker`) — the venue
//! half of what the datahub's options lane called in-process (decision 0164, amending 0128's
//! verdict 2: who polls Deribit).
//!
//! The split is the plan's D7: **the datahub keeps the poll thread, the folds, the strike window and
//! the chain recorder; this daemon serves the fetches and the feeds.** The datahub asks for the FULL
//! chain (`OptionsCall::Chain { strike_window: None }`), records it, and applies its own ±30 window
//! afterwards, so nothing here windows or records anything.
//!
//! # ⚠ The provider is ONE per daemon, not one per request
//!
//! `DeribitOptionsProvider` carries a 5-second per-book cache (`CACHE_TTL_MS`) so the
//! `list_expiries` and the N `fetch_chain` calls of one refresh pass share a single HTTP call. A
//! provider built per request would have an empty cache for every request and change the venue cost:
//! one `get_book_summary_by_currency` per call instead of one per pass. So the daemon holds one,
//! behind a `Mutex` (its methods take `&mut self`), and every connection's calls go through it — which
//! also means two clients' passes share a cache hit. The price is that calls serialize: a call waits
//! for another's venue request (the provider's own agent bounds one at 10 s).
//!
//! # What the daemon decides that the in-process path did not need to
//!
//! * **The underlying must be one the provider serves.** In-process the datahub only ever passed
//!   `list_underlyings()`; on the wire the name is client-chosen and the provider interpolates it into
//!   the venue's URL (an unknown name queries `currency=<name>`). A name outside the served set is
//!   refused, naming the set, before anything is sent — the 0052 cost rule (the client names no term
//!   of the cost the server did not bound).
//! * **The focus sets are capped** by `vike_gatewayhub_client::proto::GW_OPTIONS_MAX_INDEXES` and
//!   `GW_OPTIONS_MAX_INSTRUMENTS` (`OptionsFeedSpec::refusal`, which also bounds a name's length and
//!   charset), re-checked here because a lane must not trust that the server ran it. The datahub names
//!   two underlyings' front expiry at ±10 strikes.
//! * **A focus-set name must be a REAL one.** A name becomes a venue channel (`markprice.options.<index>`,
//!   `ticker.<instrument>.100ms`); a well-formed junk name would be subscribed, refused by the venue and
//!   retried by the bridge feed's reconnect loop against Deribit from this box for the life of the
//!   socket. So an index must be `<underlying>_<settlement>` for a served underlying (`btc_usd`,
//!   `sol_usdc`) and an instrument must parse as a Deribit option name of a served underlying
//!   (`vike_deribit::chain::parse_instrument_name`, the grammar the datahub's own grid joins on).
//!
//! # The feeds
//!
//! Each `OptionsFeedOpen` starts its own bridge feed (`spawn_deribit_markprice_options_feed` /
//! `spawn_deribit_ticker_feed`), delivering into the server's sink, and the returned handle's
//! `shutdown` stops it and joins its thread. ⚠ The bridge's handle DETACHES its thread on drop (the
//! datahub's feeds ran for the process lifetime), so the handle here shuts down on drop too: a feed
//! for a socket that closed must not outlive it.
//!
//! # Cancellation: what stops and what cannot
//!
//! A provider call is one venue request with no probe (a wait on the provider's lock is not
//! cancellable either); the instrument fetch checks the context before its request. The feeds are
//! stopped by their handle, not by a context.

use std::sync::{Arc, Mutex, PoisonError};

use vike_catalog::CatalogError;
use vike_deribit::chain::DeribitOptionsProvider;
use vike_deribit::options_feed::{
    MAINNET_WS, MarkPriceFeed, TickerFeed, spawn_deribit_markprice_options_feed,
    spawn_deribit_ticker_feed,
};
use vike_gatewayhub_client::proto::{
    FetchFailure, OptionIndexKind, OptionsCall, OptionsFeedSpec, OptionsReply,
};
use vike_model::Instrument;

use super::{FetchCtx, OptionsFeedHandle, OptionsFeedSink, cancelled};

/// The `currency` of the instrument-index fetch: every currency in one request, as the datahub asked.
const INDEX_CURRENCY: &str = "any";

/// Starts one feed over a focus set, delivering into the sink. The production ones start the
/// bridge's feeds; a suite injects a recorder.
pub type FeedSpawn =
    Box<dyn Fn(Vec<String>, Arc<dyn OptionsFeedSink>) -> Box<dyn OptionsFeedHandle> + Send + Sync>;

/// `public/get_instruments` for `(currency, kind)`: `vike_deribit::catalog::fetch_instruments` in
/// production.
pub type InstrumentsFetch =
    Box<dyn Fn(&str, &str) -> Result<Vec<Instrument>, CatalogError> + Send + Sync>;

/// The options half of the daemon. See the module doc.
pub struct OptionsHub {
    provider: Mutex<DeribitOptionsProvider>,
    /// The underlyings the provider serves, copied out at construction so a feed open never waits on
    /// the provider's lock (a venue request may hold it for seconds).
    underlyings: Vec<&'static str>,
    markprice: FeedSpawn,
    ticker: FeedSpawn,
    instruments: InstrumentsFetch,
}

impl OptionsHub {
    /// The production hub: a mainnet provider and the bridge's feeds.
    pub fn mainnet() -> Self {
        Self::with_provider(DeribitOptionsProvider::new())
    }

    /// A hub over `provider` (a suite points one at a loopback book summary), with the bridge's feeds
    /// and instrument fetch.
    pub fn with_provider(provider: DeribitOptionsProvider) -> Self {
        let underlyings = provider.list_underlyings().to_vec();
        Self {
            provider: Mutex::new(provider),
            underlyings,
            markprice: Box::new(spawn_markprice),
            ticker: Box::new(spawn_ticker),
            instruments: Box::new(vike_deribit::catalog::fetch_instruments),
        }
    }

    /// The same hub with its two feeds replaced — the seam a suite records the focus sets through.
    #[must_use]
    pub fn with_feeds(mut self, markprice: FeedSpawn, ticker: FeedSpawn) -> Self {
        self.markprice = markprice;
        self.ticker = ticker;
        self
    }

    /// The same hub with its instrument fetch replaced.
    #[must_use]
    pub fn with_instruments(mut self, instruments: InstrumentsFetch) -> Self {
        self.instruments = instruments;
        self
    }

    /// One call to the persistent provider, mirroring its methods one for one.
    pub fn call(&self, call: &OptionsCall) -> Result<OptionsReply, String> {
        let mut provider = self.provider.lock().unwrap_or_else(PoisonError::into_inner);
        match call {
            OptionsCall::Underlyings => Ok(OptionsReply::Underlyings(
                provider.list_underlyings().iter().map(|u| (*u).to_string()).collect(),
            )),
            OptionsCall::Expiries { underlying } => {
                served(&provider, underlying)?;
                provider.list_expiries(underlying).map(OptionsReply::Expiries)
            }
            OptionsCall::Chain { underlying, expiry, strike_window, r } => {
                served(&provider, underlying)?;
                // `None` is the FULL chain; `Some(n)` windows it to ±n strikes around spot.
                let strikes = strike_window.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
                provider
                    .fetch_chain(underlying, expiry, strikes, *r)
                    .map(|chain| OptionsReply::Chain(Box::new(chain)))
            }
        }
    }

    /// `Fetch { OptionIndex }`: Deribit's `public/get_instruments` for `currency=any` and one kind. The
    /// failure text is the datahub's former `fetch_option_index` prefix (`deribit {kind}
    /// instruments: {error}`), so the line the datahub logs when the index refresh fails is the line
    /// it logged in-process; it fails as a whole, so the caller keeps its last-good index.
    pub fn instruments(
        &self,
        kind: OptionIndexKind,
        ctx: &FetchCtx,
    ) -> Result<Vec<Instrument>, FetchFailure> {
        let word = match kind {
            OptionIndexKind::Option => "option",
            OptionIndexKind::Future => "future",
        };
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        (self.instruments)(INDEX_CURRENCY, word)
            .map_err(|e| FetchFailure::Fetch(format!("deribit {word} instruments: {e}")))
    }

    /// Start one options feed delivering into `sink`. The set is re-checked (the module doc).
    pub fn open_feed(
        &self,
        spec: &OptionsFeedSpec,
        sink: Arc<dyn OptionsFeedSink>,
    ) -> Result<Box<dyn OptionsFeedHandle>, String> {
        if let Some(why) = spec.refusal() {
            return Err(why);
        }
        if let Some(why) = self.unreal_name(spec) {
            return Err(why);
        }
        Ok(match spec {
            OptionsFeedSpec::MarkPrice { indexes } => (self.markprice)(indexes.clone(), sink),
            OptionsFeedSpec::Ticker { instruments } => (self.ticker)(instruments.clone(), sink),
        })
    }
}

impl OptionsHub {
    /// Why a well-formed focus-set name is not a real index or instrument of a served underlying, or
    /// `None` when every name is (the module doc: a junk channel would be retried against Deribit for
    /// the life of the socket). Runs after `OptionsFeedSpec::refusal`, so a name here is already
    /// short and channel-safe.
    fn unreal_name(&self, spec: &OptionsFeedSpec) -> Option<String> {
        let served = |base: &str| self.underlyings.iter().any(|u| u.eq_ignore_ascii_case(base));
        let set = self.underlyings.join(", ");
        match spec {
            OptionsFeedSpec::MarkPrice { indexes } => indexes.iter().find_map(|index| {
                let real = index
                    .split_once('_')
                    .is_some_and(|(base, settlement)| served(base) && !settlement.is_empty());
                (!real).then(|| {
                    format!(
                        "deribit options: `{index}` is not an index of an underlying this daemon \
                         serves (an index is `<underlying>_<settlement>`, e.g. `btc_usd`; \
                         served: [{set}]); nothing was subscribed"
                    )
                })
            }),
            OptionsFeedSpec::Ticker { instruments } => instruments.iter().find_map(|name| {
                match vike_deribit::chain::parse_instrument_name(name) {
                    Some((base, ..)) if served(&base) => None,
                    _ => Some(format!(
                        "deribit options: `{name}` is not an option instrument of an underlying \
                         this daemon serves (served: [{set}]); nothing was subscribed"
                    )),
                }
            }),
        }
    }
}

impl std::fmt::Debug for OptionsHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionsHub").finish_non_exhaustive()
    }
}

/// Refuse an underlying the provider does not serve, naming the set.
fn served(provider: &DeribitOptionsProvider, underlying: &str) -> Result<(), String> {
    let set = provider.list_underlyings();
    if set.contains(&underlying) {
        Ok(())
    } else {
        Err(format!(
            "deribit options: `{underlying}` is not an underlying this daemon serves (served: \
             [{}]); nothing was asked of the venue",
            set.join(", ")
        ))
    }
}

/// The bridge's markprice feed, stopped (and joined) by its handle.
struct MarkpriceHandle(Option<MarkPriceFeed>);

impl OptionsFeedHandle for MarkpriceHandle {
    fn shutdown(&mut self) {
        if let Some(feed) = self.0.take() {
            feed.shutdown();
        }
    }
}

impl Drop for MarkpriceHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The bridge's ticker feed, stopped (and joined) by its handle.
struct TickerHandle(Option<TickerFeed>);

impl OptionsFeedHandle for TickerHandle {
    fn shutdown(&mut self) {
        if let Some(feed) = self.0.take() {
            feed.shutdown();
        }
    }
}

impl Drop for TickerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_markprice(
    indexes: Vec<String>,
    sink: Arc<dyn OptionsFeedSink>,
) -> Box<dyn OptionsFeedHandle> {
    let feed = spawn_deribit_markprice_options_feed(MAINNET_WS.to_string(), indexes, move |rows| {
        sink.marks(rows);
    });
    Box::new(MarkpriceHandle(Some(feed)))
}

fn spawn_ticker(
    instruments: Vec<String>,
    sink: Arc<dyn OptionsFeedSink>,
) -> Box<dyn OptionsFeedHandle> {
    let feed = spawn_deribit_ticker_feed(MAINNET_WS.to_string(), instruments, move |row| {
        sink.ticker(row);
    });
    Box::new(TickerHandle(Some(feed)))
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod options_tests;
