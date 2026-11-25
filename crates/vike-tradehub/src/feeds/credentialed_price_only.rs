//! The PRICE-ONLY feeds of the two credentialed FX venues the control door admits foreign symbols for
//! (decision 0174): **ctrader** and **ig**, for a box whose accounts of the venue have NO mount: the dial and
//! the price tap, nothing subscribed. Without it the `Admits` row made every foreign symbol of an unmounted
//! ctrader or ig account answer `this node runs no price feed for <venue>`. Each feed is its row of `feeds.rs`'s
//! `PRICE_ONLY_ARMS`, beside deribit's and oanda's, built by `wire_price_only_feed`; this module is their reading.
//!
//! * **One credentialed stream per VENUE, not per account, and lazy.** The dial is the mounted arm's:
//!   `StrategyLaneFactories::client` onto the datahub's strategy lane, whose gatewayhub opens the venue's credentialed
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
//!   socket (a quote key's `DataClientBusSpecHoldCtrader::ask`; the venue's own subscription cap is UNMEASURED), held at most
//!   `venue_backend::gateway::trade_lease_registry::MAX_WATCHED` at a time and closed `IDLE_TTL` after the door lets go. ig: each
//!   lane is one more subscription on the feed's ONE IG login and ONE Lightstreamer socket, so the feed
//!   carries the mounted arm's cap
//!   (`vike_gatewayhub_client::credentialed_limits::IG`, 16, under IG's 40 subscriptions per connection); the lane of an
//!   ADMITTED epic is never reaped (it prices an engine's position), so it keeps its place until the engine
//!   stops or the node restarts, and a failed subscribe is remembered for 30 s (`StrategyLaneQuote::with_lane_cap`).
//! * **The round trip holds no lock.** A first ctrader handshake (~12 s) or an IG login runs with the dial
//!   lent out of the feed: the reaper's close and the teardown's shutdown never wait for it.
