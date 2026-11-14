//! **The Deribit options lane: the venue's half of the gatewayhub's options surface** —
//! [`OptionsSpecDeribit`], the `vike_bridge_core::options::OptionsSpec` the gatewayhub's options engine
//! (`crates/vike-gatewayhub/src/rest/options.rs`'s `OptionsHub`) runs it through. MOVED from the
//! gatewayhub's `rest::options` (owner ruling of 2026-11-12: one-venue code does not live in a shared
//! crate): ONE persistent [`DeribitOptionsProvider`] behind a `Mutex`, the instrument-index fetch, and the
//! two public feeds (`markprice.options` and `ticker`) — the venue half of what the datahub's options lane
//! called in-process (decision 0164, amending 0128's verdict 2: who polls Deribit).
//!
//! The split is the plan's D7: **the datahub keeps the poll thread, the folds, the strike window and
//! the chain recorder; the gatewayhub serves the fetches and the feeds.** The datahub asks for the FULL
//! chain (`strikes: None`), records it, and applies its own ±30 window afterwards, so nothing here
//! windows or records anything.
//!
//! # ⚠ The provider is ONE per daemon, not one per request
//!
//! `DeribitOptionsProvider` carries a 5-second per-book cache (`CACHE_TTL_MS`) so the
//! `list_expiries` and the N `fetch_chain` calls of one refresh pass share a single HTTP call. A
//! provider built per request would have an empty cache for every request and change the venue cost:
//! one `get_book_summary_by_currency` per call instead of one per pass. So the daemon holds one spec,
//! and the spec one provider behind a `Mutex` (its methods take `&mut self`), and every connection's
//! calls go through it — which also means two clients' passes share a cache hit. The price is that calls
//! serialize: a call waits for another's venue request (the provider's own agent bounds one at 10 s).
//!
//! # What the lane decides that the in-process path did not need to
//!
//! * **The underlying must be one the provider serves.** The served set is the VENUE's listing (the
//!   provider reads it from Deribit's own option instrument rows and holds it for an hour:
//!   `crate::option_listing`), never a table here. On the wire the name is client-chosen; a name
//!   outside the listing is refused, naming the set, before any book is read — the 0052 cost rule (the
//!   client names no term of the cost the server did not bound).
//! * **The focus sets are capped** by the wire's constants, which the gatewayhub's engine checks
//!   (`OptionsFeedSpec::refusal`, which also bounds a name's length and charset) before a feed is asked
//!   of this lane. The datahub names two underlyings' front expiry at ±10 strikes.
//! * **A focus-set name must be a REAL one.** A name becomes a venue channel (`markprice.options.<index>`,
//!   `ticker.<instrument>.100ms`); a well-formed junk name would be subscribed, refused by the venue and
//!   retried by the bridge feed's reconnect loop against Deribit from this box for the life of the
//!   socket. So an index must be `<underlying>_<settlement>` for a served underlying (`btc_usd`,
//!   `sol_usdc`) and an instrument must parse as a Deribit option name of a served underlying
//!   (`crate::feed::options::chain::parse_instrument_name`, the grammar the datahub's own grid joins on).
//!
//! # The feeds
//!
//! Each open starts its own bridge feed (`spawn_deribit_feed_options_mark` /
//! `spawn_deribit_feed_options_ticker`), delivering into the engine's closure, and the returned stop shuts it down
//! and joins its thread. The bridge's handle stops and joins on drop too, and the engine calls the stop
//! at its handle's drop: a feed for a socket that closed must not outlive it.
//!
//! # Cancellation: what stops and what cannot
//!
//! A provider call is one venue request with no probe (a wait on the provider's lock is not cancellable
//! either); the engine checks the request before the instrument fetch. The feeds are stopped by their
//! stop, not by a context.

use std::sync::{Mutex, PoisonError};

use vike_bridge_core::options::{
    OptionsFeedStop, OptionsListedUnderlying, OptionsMarksDeliver, OptionsSpec,
    OptionsTickerDeliver,
};
use vike_catalog::CatalogError;
use vike_marketdata::{OptionChain, OptionChainExpiry};
use vike_model::Instrument;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::feed::options::chain::DeribitOptionsProvider;
use crate::feed::options::{
    mainnet_websocket, spawn_deribit_feed_options_mark, spawn_deribit_feed_options_ticker,
};
use crate::option_listing::OptionUnderlying;

/// The `currency` of the instrument-index fetch: every currency in one request, as the datahub asked.
const INDEX_CURRENCY: &str = "any";

/// Starts one feed over a focus set, delivering into `D`, and answers its stop. The production ones start
/// the bridge's feeds; a suite injects a recorder.
pub type FeedSpawn<D> = Box<dyn Fn(Vec<String>, D) -> OptionsFeedStop + Send + Sync>;

/// `public/get_instruments` for `(currency, kind)`: `crate::catalog::fetch_instruments` in production.
pub type InstrumentsFetch =
    Box<dyn Fn(&str, &str) -> Result<Vec<Instrument>, CatalogError> + Send + Sync>;

/// **Deribit's options lane.** See the module doc.
pub struct OptionsSpecDeribit {
    /// `Err` is why this lane serves nothing: an address row it needs was refused at construction
    /// ([`OptionsSpecDeribit::mainnet`]); every call, index fetch and feed open answers it, naming the row.
    provider: Result<Mutex<DeribitOptionsProvider>, String>,
    /// The underlyings the provider lists, copied out after every call so a feed open never waits on
    /// the provider's lock (a venue request may hold it for seconds). Empty until a call listed them;
    /// a feed open then lists them itself.
    listed: Mutex<Vec<OptionUnderlying>>,
    markprice: FeedSpawn<OptionsMarksDeliver>,
    ticker: FeedSpawn<OptionsTickerDeliver>,
    instruments: InstrumentsFetch,
}

impl OptionsSpecDeribit {
    /// The production lane: a mainnet provider and the bridge's feeds, on deribit's `live` address rows
    /// (`urls`, decision 0265) whatever any account's tier: deribit's option prices are mainnet's. A
    /// refused row is logged once here and answered by every request, never a default address.
    pub fn mainnet(urls: &VenueUrls) -> Self {
        match Self::on(urls, DeribitOptionsProvider::from_urls(urls)) {
            Ok(lane) => lane,
            Err(why) => {
                let why = format!("deribit options: {why}; nothing was asked of the venue");
                tracing::error!("{why} (fix the row and restart the daemon)");
                Self {
                    provider: Err(why),
                    listed: Mutex::new(Vec::new()),
                    // Never asked: every entry point answers the refusal first.
                    markprice: Box::new(no_feed::<OptionsMarksDeliver>),
                    ticker: Box::new(no_feed::<OptionsTickerDeliver>),
                    instruments: Box::new(|_: &str, _: &str| Err(CatalogError("refused".into()))),
                }
            }
        }
    }

    /// A lane over `provider` (a suite points one at a loopback book summary), with the bridge's feeds
    /// and instrument fetch on the release's mainnet rows (`VENUE_URL_SEED`).
    pub fn with_provider(provider: DeribitOptionsProvider) -> Self {
        Self::on(&VenueUrls::seed(crate::history::VENUE), Ok(provider))
            .expect("the seed holds deribit's live rows")
    }

    /// The lane over `provider`, its feeds and its index fetch on `urls`' `live` `data` rows.
    fn on(
        urls: &VenueUrls,
        provider: Result<DeribitOptionsProvider, UrlRefusal>,
    ) -> Result<Self, UrlRefusal> {
        let provider = provider?;
        let ws = mainnet_websocket(urls)?;
        let rest = urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?.to_string();
        let ticker_websocket = ws.clone();
        Ok(Self {
            provider: Ok(Mutex::new(provider)),
            listed: Mutex::new(Vec::new()),
            markprice: Box::new(move |indexes: Vec<String>, deliver: OptionsMarksDeliver| {
                spawn_markprice(&ws, indexes, deliver)
            }),
            ticker: Box::new(move |instruments: Vec<String>, deliver: OptionsTickerDeliver| {
                spawn_ticker(&ticker_websocket, instruments, deliver)
            }),
            instruments: Box::new(move |currency: &str, kind: &str| {
                crate::catalog::fetch_instruments(&rest, currency, kind)
            }),
        })
    }

    /// The provider, or the refusal every request answers.
    fn provider(&self) -> Result<&Mutex<DeribitOptionsProvider>, String> {
        self.provider.as_ref().map_err(String::clone)
    }

    /// The same lane with its two feeds replaced — the seam a suite records the focus sets through.
    #[must_use]
    pub fn with_feeds(
        mut self,
        markprice: FeedSpawn<OptionsMarksDeliver>,
        ticker: FeedSpawn<OptionsTickerDeliver>,
    ) -> Self {
        self.markprice = markprice;
        self.ticker = ticker;
        self
    }

    /// The same lane with its instrument fetch replaced.
    #[must_use]
    pub fn with_instruments(mut self, instruments: InstrumentsFetch) -> Self {
        self.instruments = instruments;
        self
    }

    /// One call to the persistent provider, under its lock: `ask` answers, then the listing the provider
    /// holds is copied out for the feed opens.
    fn locked_call<T>(
        &self,
        ask: impl FnOnce(&mut DeribitOptionsProvider) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut provider = self.provider()?.lock().unwrap_or_else(PoisonError::into_inner);
        let reply = ask(&mut provider);
        if let Some(listed) = provider.listed() {
            *self.listed.lock().unwrap_or_else(PoisonError::into_inner) = listed.to_vec();
        }
        reply
    }

    /// The listing a feed open checks names against: the copy the last call left, or (before any call
    /// listed) the provider's own, read under its lock once.
    fn listing_held(&self) -> Result<Vec<OptionUnderlying>, String> {
        let held = self.listed.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if !held.is_empty() {
            return Ok(held);
        }
        let listed =
            self.provider()?.lock().unwrap_or_else(PoisonError::into_inner).list_underlyings()?;
        *self.listed.lock().unwrap_or_else(PoisonError::into_inner) = listed.clone();
        Ok(listed)
    }

    /// The listing a feed open checks a focus set against, or the refusal when it cannot be read.
    fn feed_listing(&self) -> Result<Vec<OptionUnderlying>, String> {
        self.listing_held().map_err(|why| {
            format!(
                "deribit options: the venue's option listing could not be read ({why}); nothing \
                 was subscribed"
            )
        })
    }

    /// Why a well-formed index is not a real index of a served underlying, or `None` when every one is
    /// (the module doc: a junk channel would be retried against Deribit for the life of the socket).
    /// Runs after the engine's `OptionsFeedSpec::refusal`, so a name here is already short and
    /// channel-safe.
    fn unreal_index(&self, indexes: &[String]) -> Option<String> {
        let listed = match self.feed_listing() {
            Ok(listed) => listed,
            Err(why) => return Some(why),
        };
        let served = |base: &str| listed.iter().any(|u| u.name.eq_ignore_ascii_case(base));
        let set = listed.iter().map(|u| u.name.as_str()).collect::<Vec<_>>().join(", ");
        indexes.iter().find_map(|index| {
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
        })
    }

    /// Why a well-formed instrument name is not an option instrument of a served underlying, or `None`
    /// when every one is (as [`Self::unreal_index`]).
    fn unreal_instrument(&self, instruments: &[String]) -> Option<String> {
        let listed = match self.feed_listing() {
            Ok(listed) => listed,
            Err(why) => return Some(why),
        };
        let served = |base: &str| listed.iter().any(|u| u.name.eq_ignore_ascii_case(base));
        let set = listed.iter().map(|u| u.name.as_str()).collect::<Vec<_>>().join(", ");
        instruments.iter().find_map(|name| match crate::feed::options::chain::parse_instrument_name(name) {
            Some((base, ..)) if served(&base) => None,
            _ => Some(format!(
                "deribit options: `{name}` is not an option instrument of an underlying this daemon \
                 serves (served: [{set}]); nothing was subscribed"
            )),
        })
    }
}

impl OptionsSpec for OptionsSpecDeribit {
    fn venue(&self) -> &'static str {
        crate::history::VENUE
    }

    fn listing(&self, refresh: bool) -> Result<Vec<OptionsListedUnderlying>, String> {
        self.locked_call(|provider| {
            let listed =
                if refresh { provider.refresh_underlyings() } else { provider.list_underlyings() };
            listed.map(|listed| {
                listed
                    .into_iter()
                    .map(|u| OptionsListedUnderlying { name: u.name, index: u.index })
                    .collect()
            })
        })
    }

    fn expiries(&self, underlying: &str) -> Result<Vec<OptionChainExpiry>, String> {
        self.locked_call(|provider| {
            served(provider, underlying).and_then(|()| provider.list_expiries(underlying))
        })
    }

    fn chain(
        &self,
        underlying: &str,
        expiry: &str,
        strikes: Option<usize>,
        r: f64,
    ) -> Result<OptionChain, String> {
        self.locked_call(|provider| {
            served(provider, underlying)
                .and_then(|()| provider.fetch_chain(underlying, expiry, strikes, r))
        })
    }

    /// Deribit's `public/get_instruments` for `currency=any` and one kind. The failure text is the
    /// datahub's former `fetch_option_index` prefix (`deribit {kind} instruments: {error}`), so the line
    /// the datahub logs when the index refresh fails is the line it logged in-process; it fails as a
    /// whole, so the caller keeps its last-good index.
    fn instruments(&self, kind: &str) -> Result<Vec<Instrument>, String> {
        self.provider()?;
        (self.instruments)(INDEX_CURRENCY, kind)
            .map_err(|e| format!("deribit {kind} instruments: {e}"))
    }

    fn open_marks(
        &self,
        indexes: Vec<String>,
        deliver: OptionsMarksDeliver,
    ) -> Result<OptionsFeedStop, String> {
        self.provider()?;
        if let Some(why) = self.unreal_index(&indexes) {
            return Err(why);
        }
        Ok((self.markprice)(indexes, deliver))
    }

    fn open_ticker(
        &self,
        instruments: Vec<String>,
        deliver: OptionsTickerDeliver,
    ) -> Result<OptionsFeedStop, String> {
        self.provider()?;
        if let Some(why) = self.unreal_instrument(&instruments) {
            return Err(why);
        }
        Ok((self.ticker)(instruments, deliver))
    }
}

impl std::fmt::Debug for OptionsSpecDeribit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionsSpecDeribit").finish_non_exhaustive()
    }
}

/// Refuse an underlying the provider does not serve (the venue's listing does not name it), naming the
/// set, before any book is read.
fn served(provider: &mut DeribitOptionsProvider, underlying: &str) -> Result<(), String> {
    let listed = provider.list_underlyings()?;
    if listed.iter().any(|u| u.name == underlying) {
        Ok(())
    } else {
        let set: Vec<&str> = listed.iter().map(|u| u.name.as_str()).collect();
        Err(format!(
            "deribit options: `{underlying}` is not an underlying this daemon serves (served: \
             [{}]); nothing was asked of the venue",
            set.join(", ")
        ))
    }
}

/// The feed of a lane whose rows were refused: it is never started (every entry point answers first).
fn no_feed<D>(_: Vec<String>, _: D) -> OptionsFeedStop {
    Box::new(|| {})
}

fn spawn_markprice(
    ws: &str,
    indexes: Vec<String>,
    deliver: OptionsMarksDeliver,
) -> OptionsFeedStop {
    let feed = spawn_deribit_feed_options_mark(ws.to_string(), indexes, move |rows| {
        deliver(rows);
    });
    Box::new(move || feed.shutdown())
}

fn spawn_ticker(
    ws: &str,
    instruments: Vec<String>,
    deliver: OptionsTickerDeliver,
) -> OptionsFeedStop {
    let feed = spawn_deribit_feed_options_ticker(ws.to_string(), instruments, move |row| {
        deliver(row);
    });
    Box::new(move || feed.shutdown())
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod options_tests;
