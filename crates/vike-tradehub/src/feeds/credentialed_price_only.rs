//! The PRICE-ONLY feeds of the two credentialed FX venues the control door admits foreign symbols for
//! (decision 0174): **ctrader** and **ig**. The twin of `deribit::wire_deribit_price_feed` and
//! `oanda::wire_oanda_price_feed` for a box whose accounts of the venue have NO mount: the dial and the
//! price tap, nothing subscribed. Without it the `Admits` row made every foreign symbol of an unmounted
//! ctrader or ig account answer `this node runs no price feed for <venue>`.
//!
//! * **One credentialed stream per VENUE, not per account, and lazy.** The dial is the mounted arm's:
//!   `FeedCtors::client` onto the datahub's strategy lane, whose gatewayhub opens the venue's credentialed
//!   stream as the venue's DEFAULT `demo` account (`crates/vike-gatewayhub/src/data/accounts.rs`'s
//!   `AccountSource`; no credential, label or tier crosses the hop). Constructing the feed opens nothing at
//!   the venue: the strategy-lane session is a table read at the datahub, and the gatewayhub builds the
//!   venue's client at the first `subscribe_quotes` the door sends, so a box that never trades a foreign
//!   symbol never spends a ctrader handshake or an IG login. ⚠ The price is therefore the default demo
//!   account's broker feed, whichever account asks (the same account the mounted arm's feed would use).
//! * **Refused by name when no session can be had.** An account store with no usable credentials, a
//!   ctrader handshake the venue refuses or that outlasts its budget, a datahub that advertises no lane for
//!   the venue: each reaches the door as the first subscription's error text (`cannot subscribe <venue>'s
//!   prices for ...: <the gatewayhub's words>`), and a dial that cannot even be built leaves no watcher at
//!   all, so the door keeps answering `this node runs no price feed for <venue>` (fail closed, never a
//!   price from another source).
//! * **Bounded.** ctrader: every foreign symbol is one more spot subscription on the ONE multiplexed data
//!   socket (`CtraderData::subscribe_quotes`; the venue's own subscription cap is UNMEASURED), held at most
//!   `remote_backend::facts::MAX_WATCHED` at a time and closed `IDLE_TTL` after the door lets go. ig: each
//!   lane is one more subscription on the feed's ONE IG login and ONE Lightstreamer socket, so the feed
//!   carries the mounted arm's cap
//!   (`ig::IG_MAX_QUOTE_LANES`, 4, well under IG's 40 concurrent streaming subscriptions); the lane of an
//!   ADMITTED epic is never reaped (it prices an engine's position), so it keeps its place until the engine
//!   stops or the node restarts, and a failed subscribe is remembered for 30 s (`QuoteFeed::with_lane_cap`).
//! * **The round trip holds no lock.** A first ctrader handshake (~12 s) or an IG login runs with the dial
//!   lent out of the feed: the reaper's close and the teardown's shutdown never wait for it.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::LiveDataSink;

use super::FeedCtors;
use super::ig::IG_MAX_QUOTE_LANES;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};

/// The dial both feeds start from, and the price tap in front of its sink. No `MakerSink` stands in front
/// of it (a mount's bar synthesizer exists only for a mount), so every quote goes to the core's lane
/// sink, which routes it to the venue's engine without a mount.
fn dial(
    venue: &str,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<(Tracked, Arc<PriceTap>), String> {
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    let tap = Arc::new(PriceTap::default());
    let sink = wrap(Arc::new(TapSink::new(core, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    Ok((Tracked::new(make.client(venue, None, sink)?), tap))
}

/// cTrader's PRICE-ONLY feed, for a box that mounts no cTrader market: the dial and the price tap, nothing
/// subscribed. The door subscribes a symbol's spot quotes through the [`QuoteFeed::watcher`] when an order
/// names a pair the account was not started on.
pub(super) fn wire_ctrader_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    let (feeds, tap) = dial("ctrader", handle, wrap, make)?;
    Ok(QuoteFeed::new(feeds, tap))
}

/// IG's PRICE-ONLY feed, for a box that mounts no IG market: the dial and the price tap, nothing
/// subscribed, at most [`IG_MAX_QUOTE_LANES`] extra quote lanes (each a subscription on the feed's one IG login
/// and one Lightstreamer socket).
pub(super) fn wire_ig_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    let (feeds, tap) = dial("ig", handle, wrap, make)?;
    Ok(QuoteFeed::new(feeds, tap).with_lane_cap(IG_MAX_QUOTE_LANES))
}
