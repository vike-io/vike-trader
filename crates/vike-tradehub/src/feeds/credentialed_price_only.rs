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
//!   (`vike_gatewayhub_client::credentialed_limits::IG`, 16, under IG's 40 subscriptions per connection); the lane of an
//!   ADMITTED epic is never reaped (it prices an engine's position), so it keeps its place until the engine
//!   stops or the node restarts, and a failed subscribe is remembered for 30 s (`QuoteFeed::with_lane_cap`).
//! * **The round trip holds no lock.** A first ctrader handshake (~12 s) or an IG login runs with the dial
//!   lent out of the feed: the reaper's close and the teardown's shutdown never wait for it.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::LiveDataSink;
use vike_gatewayhub_client::credentialed_limits;

use super::FeedCtors;
use super::live::QuoteFeed;
use super::price_tap::PriceTap;

/// cTrader's PRICE-ONLY feed, for a box that mounts no cTrader market: the dial and the price tap, nothing
/// subscribed. The door subscribes a symbol's spot quotes through the [`QuoteFeed::watcher`] when an order
/// names a pair the account was not started on. Both feeds here start from the shared `feeds.rs` `dial` with
/// the plain tap: no `MakerSink` stands in front of it (a mount's bar synthesizer exists only for a mount), so
/// every quote goes to the core's lane sink, which routes it to the venue's engine without a mount.
pub(super) fn wire_ctrader_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    let (feeds, tap) = super::dial("ctrader", None, PriceTap::default, handle, wrap, make)?;
    Ok(QuoteFeed::new(feeds, tap))
}

/// IG's PRICE-ONLY feed, for a box that mounts no IG market: the dial and the price tap, nothing
/// subscribed, at most the venue's row of extra quote lanes (each a subscription on the feed's one IG login
/// and one Lightstreamer socket).
pub(super) fn wire_ig_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    let (feeds, tap) = super::dial("ig", None, PriceTap::default, handle, wrap, make)?;
    Ok(QuoteFeed::new(feeds, tap).with_lane_cap(credentialed_limits::IG.max_streams))
}
