//! **Every venue ADDRESS as rows: the `venue_url` vocabulary, the release's seed, and the ONE lookup.**
//!
//! The owner ruled on 2026-11-09 that every address a venue is dialled at (REST, WebSocket, socket,
//! stream, login and history bases) lives in the settings database, table `venue_url`, typed `live`
//! or `demo`, so that a moved address is a row change plus a restart and never a code change. This
//! module is the half below the store:
//!
//! * the vocabulary: [`UrlType`](crate::venue_urls::UrlType) (`live` and `demo`, nothing else: a
//!   testnet IS `demo`), [`UrlChannel`](crate::venue_urls::UrlChannel) and
//!   [`UrlProtocol`](crate::venue_urls::UrlProtocol);
//! * [`VENUE_URL_SEED`](crate::venue_urls::VENUE_URL_SEED), the release's default rows, the ONLY
//!   place production code writes an address. The gatewayhub's start upserts them into the table
//!   (`vike_secrets::venue_url`'s `seed_venue_urls_in`): a missing row is inserted, a `default` row
//!   the seed moved is updated, an `operator` row is never touched;
//! * [`VenueUrls`](crate::venue_urls::VenueUrls), ONE venue's rows as a composition root hands them
//!   down (they ride `vike_secrets::venue_setting::VenueSettings::urls`, so they reach a bridge as
//!   `AdapterInputs::settings`), and [`VenueUrls::url`](crate::venue_urls::VenueUrls::url), the ONE
//!   lookup every consumer calls.
//!
//! ⚠ Every link in this doc is a FULL path: the module also has an outer doc (`lib.rs`'s
//! `pub mod venue_urls`), and rustdoc then resolves these links from the crate root, where none of
//! these names is in scope (`venue_timings.rs` does the same).
//!
//! A url is a BASE: `scheme://host[:port][/base]`, or `host:port` for a socket
//! ([`UrlSocket::parse`](crate::venue_urls::UrlSocket::parse), the one reading of that shape). API
//! paths and queries stay in code ([`check_url`](crate::venue_urls::check_url) refuses a query, a
//! fragment and a trailing `/`).
//!
//! # The lookup rule ([`VenueUrls::url`](crate::venue_urls::VenueUrls::url))
//!
//! 1. the exact row `(type, channel, protocol, product)`;
//! 2. else the row with channel [`UrlChannel::All`](crate::venue_urls::UrlChannel::All) (same type,
//!    protocol and product);
//! 3. ONLY for channel `data` or `history` at type `demo`: else steps 1 and 2 at type `live`
//!    (a demo account's prices and history come from mainnet by design);
//! 4. else [`UrlRefusal::Missing`](crate::venue_urls::UrlRefusal::Missing), which names the missing
//!    row: the account is NOT RUNNING.
//!
//! ⚠ `trade`, `user` and `login` NEVER fall back from demo to live. An account's OWN signed reads
//! (its funding payments, its income log) are `trade` at the account's type, never `data`.
//!
//! # The guard
//!
//! A `demo` `trade` or `user` url that equals the venue's LIVE `trade` or `user` url for the same
//! protocol and product is refused
//! ([`UrlRefusal::DemoIsLive`](crate::venue_urls::UrlRefusal::DemoIsLive)), unless
//! [`DEMO_SHARES_LIVE`](crate::venue_urls::DEMO_SHARES_LIVE) names the key: the venues that serve
//! both tiers at one address by design.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use vike_model::AccountTier;

/// **The type of an address: `live` or `demo`, and nothing else** (the owner's ruling). A testnet,
/// a sandbox and a practice host are all `demo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UrlType {
    /// The venue's real-money network.
    Live,
    /// The venue's demo, testnet, sandbox or practice network.
    Demo,
}

impl UrlType {
    /// The address type an account's tier dials. `Paper` reads as `Demo`, the way every bridge reads
    /// it (`vike_bridge_core::venue_adapter::AdapterInputs::tier`); a paper account reaches no bridge.
    #[must_use]
    pub fn of_tier(tier: AccountTier) -> UrlType {
        match tier {
            AccountTier::Live => UrlType::Live,
            AccountTier::Demo | AccountTier::Paper => UrlType::Demo,
        }
    }

    /// The stored word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UrlType::Live => "live",
            UrlType::Demo => "demo",
        }
    }
}

/// **What an address is FOR.** `All` is the fallback row for a channel with no row of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UrlChannel {
    /// Every channel of this type, protocol and product that has no row of its own.
    All,
    /// Orders and every signed read of the account's own book (positions, balance, its funding).
    Trade,
    /// The account's private stream (fills, order updates).
    User,
    /// Public market data: prices, books, the public funding rate, symbol lists.
    Data,
    /// Public history downloads.
    History,
    /// Logging in: an OAuth host, a host-discovery page, a JNLP.
    Login,
}

impl UrlChannel {
    /// Every channel, in stored order.
    pub const ALL: [UrlChannel; 6] = [
        UrlChannel::All,
        UrlChannel::Trade,
        UrlChannel::User,
        UrlChannel::Data,
        UrlChannel::History,
        UrlChannel::Login,
    ];

    /// The stored word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UrlChannel::All => "all",
            UrlChannel::Trade => "trade",
            UrlChannel::User => "user",
            UrlChannel::Data => "data",
            UrlChannel::History => "history",
            UrlChannel::Login => "login",
        }
    }
}

/// **How an address is dialled.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UrlProtocol {
    /// HTTP request/response: `http://` or `https://`.
    Rest,
    /// WebSocket: `ws://` or `wss://`.
    Websocket,
    /// A raw TCP (or TLS) socket: `host:port`, no scheme.
    Socket,
    /// HTTP chunked streaming: `http://` or `https://`.
    Stream,
}

impl UrlProtocol {
    /// Every protocol, in stored order.
    pub const ALL: [UrlProtocol; 4] =
        [UrlProtocol::Rest, UrlProtocol::Websocket, UrlProtocol::Socket, UrlProtocol::Stream];

    /// The stored word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UrlProtocol::Rest => "rest",
            UrlProtocol::Websocket => "ws",
            UrlProtocol::Socket => "socket",
            UrlProtocol::Stream => "stream",
        }
    }
}

macro_rules! word_impls {
    ($t:ty, $all:expr, $what:literal) => {
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl FromStr for $t {
            type Err = String;
            /// Exact and lower-case: the stored word and nothing else.
            fn from_str(s: &str) -> Result<Self, String> {
                $all.into_iter().find(|w| w.as_str() == s).ok_or_else(|| {
                    let words: Vec<&str> = $all.iter().map(|w| w.as_str()).collect();
                    format!("`{s}` is not {}: one of {}", $what, words.join(", "))
                })
            }
        }
    };
}
word_impls!(UrlType, [UrlType::Live, UrlType::Demo], "an address type");
word_impls!(UrlChannel, UrlChannel::ALL, "a channel");
word_impls!(UrlProtocol, UrlProtocol::ALL, "a protocol");

/// `Ok` when `url` is a legal BASE for `protocol`: `http(s)://` for rest and stream, `ws(s)://` for
/// ws, `host:port` for a socket. No whitespace, no query (`?`), no fragment (`#`) and no trailing
/// `/`: a path or a query the code appends stays in the code.
///
/// # Errors
/// What was expected, in words.
pub fn check_url(protocol: UrlProtocol, url: &str) -> Result<(), String> {
    if url.is_empty() || url.chars().any(char::is_whitespace) {
        return Err("an address is one word with no spaces".to_string());
    }
    if url.contains('?') || url.contains('#') || url.ends_with('/') {
        return Err(
            "an address is a BASE: no query, no fragment and no trailing `/` (the code adds paths)"
                .to_string(),
        );
    }
    let schemes: &[&str] = match protocol {
        UrlProtocol::Rest | UrlProtocol::Stream => &["https://", "http://"],
        UrlProtocol::Websocket => &["wss://", "ws://"],
        UrlProtocol::Socket => return UrlSocket::parse(url).map(|_| ()),
    };
    match schemes.iter().find(|s| url.starts_with(**s)) {
        Some(s) if url.len() > s.len() => Ok(()),
        _ => Err(format!("a {protocol} address starts with {}", schemes.join(" or "))),
    }
}

/// **A `socket` row read as its two halves**, `host:port` with no scheme: the ONE reading of a
/// socket address, which [`check_url`] judges a write by and a bridge that dials the row splits it
/// with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlSocket {
    /// Everything before the LAST `:`; never empty.
    pub host: String,
    /// The port after it; never 0.
    pub port: u16,
}

impl UrlSocket {
    /// `url` split at its last `:` into a host that is not empty and a port in `1..=65535`, with no
    /// `://` anywhere (a socket row carries no scheme).
    ///
    /// # Errors
    /// What a socket address is, in words.
    pub fn parse(url: &str) -> Result<UrlSocket, String> {
        let split =
            url.rsplit_once(':').filter(|(host, _)| !host.is_empty() && !url.contains("://"));
        match split.map(|(host, port)| (host, port.parse::<u16>())) {
            Some((host, Ok(port))) if port > 0 => Ok(UrlSocket { host: host.to_string(), port }),
            _ => Err("a socket address is `host:port`, with no scheme".to_string()),
        }
    }
}

/// One row of the release's seed: what [`VENUE_URL_SEED`] holds and the boot upserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlSeed {
    /// The roster id (`vike_model::VENUES`) or a history vendor's name (`tardis`).
    pub venue: &'static str,
    /// `live` or `demo`.
    pub ty: UrlType,
    /// What it is for.
    pub channel: UrlChannel,
    /// How it is dialled.
    pub protocol: UrlProtocol,
    /// The venue's own book or plane word (`spot`, `usdm`, `business`, `gateway`, …); `""` for none.
    pub product: &'static str,
    /// The base address.
    pub url: &'static str,
    /// Why the row is the way it is, for a human; never parsed.
    pub note: &'static str,
}

const fn row(
    venue: &'static str,
    ty: UrlType,
    channel: UrlChannel,
    protocol: UrlProtocol,
    product: &'static str,
    url: &'static str,
    note: &'static str,
) -> UrlSeed {
    UrlSeed { venue, ty, channel, protocol, product, url, note }
}

use UrlChannel::{All, Data, History, Login, Trade, User};
use UrlProtocol::{Rest, Socket, Stream, Websocket};
use UrlType::{Demo, Live};

/// **The release's default addresses: the ONLY place production code writes one.**
///
/// Every value equals what the code dialled before the addresses moved into rows, byte for byte once
/// the code's path is appended, with three exceptions the owner approved: the dukascopy JNLPs (the old
/// demo one answers 404), the IBKR gateway ports (4101/4102, what the box's gateways listen on; the
/// code said the TWS ports 7496/7497), and the venues whose code held a host+path constant (the row
/// holds the host, the code keeps the path). FXCM's Hosts.jsp is `https://`: what the box's operator
/// row dialled and `docs/ops/fxcm-forexconnect.md` names. The code's default said `http://`, and the
/// SDK puts the login name in that URL's query.
///
/// Polymarket has no demo row: it has no demo network, and a demo account stays NOT RUNNING. A
/// keyed RPC url belongs in the credential store, never here.
#[rustfmt::skip]
pub const VENUE_URL_SEED: &[UrlSeed] = &[
    row("binance", Live, All, Rest, "spot", "https://api.binance.com", ""),
    row("binance", Live, All, Rest, "usdm", "https://fapi.binance.com", ""),
    row("binance", Live, All, Rest, "coinm", "https://dapi.binance.com", ""),
    row("binance", Live, Trade, Websocket, "spot", "wss://ws-api.binance.com", "path /ws-api/v3 in code"),
    row("binance", Live, Data, Websocket, "spot", "wss://stream.binance.com:9443", ""),
    row("binance", Live, All, Websocket, "usdm", "wss://fstream.binance.com", "/ws user; /market,/public data"),
    row("binance", Live, All, Websocket, "coinm", "wss://dstream.binance.com", ""),
    row("binance", Demo, Trade, Rest, "spot", "https://demo-api.binance.com", ""),
    row("binance", Demo, Trade, Rest, "usdm", "https://demo-fapi.binance.com", ""),
    row("binance", Demo, Trade, Rest, "coinm", "https://demo-dapi.binance.com", ""),
    row("binance", Demo, Trade, Websocket, "spot", "wss://demo-ws-api.binance.com", ""),
    row("binance", Demo, User, Websocket, "usdm", "wss://fstream.binancefuture.com", ""),
    row("binance", Demo, User, Websocket, "coinm", "wss://dstream.binancefuture.com", ""),
    row("aster", Live, All, Rest, "usdm", "https://fapi.asterdex.com", ""),
    row("aster", Live, All, Rest, "spot", "https://sapi.asterdex.com", ""),
    row("aster", Live, All, Websocket, "usdm", "wss://fstream.asterdex.com", ""),
    row("aster", Live, All, Websocket, "spot", "wss://sstream.asterdex.com", ""),
    row("aster", Demo, Trade, Rest, "usdm", "https://fapi.asterdex-testnet.com", ""),
    row("aster", Demo, Trade, Rest, "spot", "https://sapi.asterdex-testnet.com", ""),
    row("aster", Demo, User, Websocket, "usdm", "wss://fstream.asterdex-testnet.com", ""),
    row("aster", Demo, User, Websocket, "spot", "wss://sstream.asterdex-testnet.com", ""),
    row("bybit", Live, All, Rest, "", "https://api.bybit.com", ""),
    row("bybit", Live, All, Websocket, "", "wss://stream.bybit.com", "/v5/private, /v5/public/{spot,linear,inverse} in code"),
    row("bybit", Demo, Trade, Rest, "", "https://api-demo.bybit.com", ""),
    row("bybit", Demo, User, Websocket, "", "wss://stream-demo.bybit.com", ""),
    row("okx", Live, All, Rest, "", "https://www.okx.com", ""),
    row("okx", Live, All, Websocket, "", "wss://ws.okx.com:8443", "venue retiring 8443"),
    row("okx", Live, All, Websocket, "business", "wss://ws.okx.com", ""),
    row("okx", Demo, Trade, Rest, "", "https://www.okx.com", "live's host by design; network = x-simulated-trading header by type"),
    row("okx", Demo, User, Websocket, "", "wss://wspap.okx.com:8443", "?brokerId=9999 appended by code"),
    row("okx", Demo, User, Websocket, "business", "wss://wspap.okx.com", ""),
    row("deribit", Live, All, Rest, "", "https://www.deribit.com", "live trading not wired in code"),
    row("deribit", Live, All, Websocket, "", "wss://www.deribit.com", "path /ws/api/v2 in code"),
    row("deribit", Demo, Trade, Rest, "", "https://test.deribit.com", ""),
    row("deribit", Demo, Trade, Websocket, "", "wss://test.deribit.com", ""),
    row("hyperliquid", Live, All, Rest, "", "https://api.hyperliquid.xyz", "signing follows type"),
    row("hyperliquid", Live, All, Websocket, "", "wss://api.hyperliquid.xyz", ""),
    row("hyperliquid", Demo, Trade, Rest, "", "https://api.hyperliquid-testnet.xyz", ""),
    row("hyperliquid", Demo, Data, Rest, "", "https://api.hyperliquid-testnet.xyz", "demo history falls back to live"),
    row("hyperliquid", Demo, All, Websocket, "", "wss://api.hyperliquid-testnet.xyz", ""),
    row("alpaca", Live, Trade, Rest, "", "https://broker-api.alpaca.markets", "live not wired in code"),
    row("alpaca", Live, Data, Rest, "", "https://data.alpaca.markets", ""),
    row("alpaca", Live, Data, Websocket, "", "wss://stream.data.alpaca.markets", ""),
    row("alpaca", Live, Login, Rest, "", "https://authx.alpaca.markets", ""),
    row("alpaca", Demo, Trade, Rest, "", "https://broker-api.sandbox.alpaca.markets", ""),
    row("alpaca", Demo, Data, Rest, "", "https://data.sandbox.alpaca.markets", ""),
    row("alpaca", Demo, Data, Websocket, "", "wss://stream.data.sandbox.alpaca.markets", ""),
    row("alpaca", Demo, Login, Rest, "", "https://authx.sandbox.alpaca.markets", ""),
    row("polymarket", Live, Trade, Rest, "", "https://clob.polymarket.com", "live only"),
    row("polymarket", Live, Trade, Rest, "relayer", "https://relayer-v2.polymarket.com", ""),
    row("polymarket", Live, User, Websocket, "", "wss://ws-subscriptions-clob.polymarket.com", "/ws/user and /ws/market in code"),
    row("polymarket", Live, Data, Websocket, "live-data", "wss://ws-live-data.polymarket.com", ""),
    row("polymarket", Live, Data, Rest, "gamma", "https://gamma-api.polymarket.com", ""),
    row("polymarket", Live, Data, Rest, "data", "https://data-api.polymarket.com", ""),
    row("polymarket", Live, Data, Rest, "rpc", "https://polygon.drpc.org", "keyed RPC -> credential store"),
    row("polymarket", Live, Login, Rest, "geoblock", "https://polymarket.com", ""),
    row("polymarket", Live, History, Rest, "pmxt", "https://r2v2.pmxt.dev", ""),
    row("ctrader", Live, Trade, Socket, "", "live.ctraderapi.com:5035", "live not wired in code"),
    row("ctrader", Demo, Trade, Socket, "", "demo.ctraderapi.com:5035", ""),
    row("ctrader", Live, Login, Rest, "", "https://openapi.ctrader.com", ""),
    row("ctrader", Demo, Login, Rest, "", "https://openapi.ctrader.com", "live's host by design: one Open API OAuth host for both networks"),
    row("oanda", Live, All, Rest, "", "https://api-fxtrade.oanda.com", "live not wired in code"),
    row("oanda", Live, All, Stream, "", "https://stream-fxtrade.oanda.com", ""),
    row("oanda", Demo, All, Rest, "", "https://api-fxpractice.oanda.com", ""),
    row("oanda", Demo, All, Stream, "", "https://stream-fxpractice.oanda.com", ""),
    row("ig", Live, All, Rest, "", "https://api.ig.com/gateway/deal", "live not wired; stream host from login reply"),
    row("ig", Demo, All, Rest, "", "https://demo-api.ig.com/gateway/deal", ""),
    row("fxcm", Live, Login, Rest, "Real", "https://www.fxcorporate.com/Hosts.jsp", "servers come from Hosts.jsp"),
    row("fxcm", Demo, Login, Rest, "Demo", "https://www.fxcorporate.com/Hosts.jsp", "live's Hosts.jsp by design: the product word Demo picks the network; https, as the box dialled"),
    row("fxcm", Live, History, Rest, "", "https://candledata.fxcorporate.com", ""),
    row("dukascopy", Live, Login, Rest, "jnlp", "https://platform.dukascopy.com/live/jforex.jnlp", "measured 200 on 2026-11-08"),
    row("dukascopy", Demo, Login, Rest, "jnlp", "https://platform.dukascopy.com/demo/jforex.jnlp", "old www.dukascopy.com/client/... = 404"),
    row("dukascopy", Live, History, Rest, "", "https://datafeed.dukascopy.com/datafeed", ""),
    row("dukascopy", Live, History, Rest, "archive", "https://cfg-public-proper-wallaby.s3.eu-west-1.amazonaws.com", "the S3 archive, Requester Pays; the datahub reads the region off the host"),
    row("ibkr", Live, Trade, Socket, "gateway", "127.0.0.1:4101", "code had 7496"),
    row("ibkr", Demo, Trade, Socket, "gateway", "127.0.0.1:4102", "code had 7497"),
    row("ibkr", Live, Trade, Rest, "cpapi", "https://127.0.0.1:5000", "one CP gateway per box"),
    row("ibkr", Demo, Trade, Rest, "cpapi", "https://127.0.0.1:5000", "live's gateway by design: one CP gateway per box serves either login"),
    row("tardis", Live, History, Rest, "", "https://datasets.tardis.dev", "path /v1 in code"),
    row("databento", Live, History, Rest, "", "https://hist.databento.com", ""),
    row("yahoo", Live, History, Rest, "", "https://query1.finance.yahoo.com", ""),
    // vike:new-venue:row row("{venue}", Live, All, Rest, "", "https://TODO-new-venue.invalid", "TODO(new-venue: {venue}): one row per address its bridge dials, live and demo"),
];

/// **The keys whose `demo` `trade`/`user` url equals the live one BY DESIGN**, keyed `(venue,
/// channel, protocol, product)` as [`VenueUrls::url`] is asked. The guard lets these through and
/// refuses every other demo address that equals a live one.
///
/// * okx: one REST host for both networks; the `x-simulated-trading` header, set by the account's
///   type, is the network.
/// * ibkr cpapi: one Client Portal gateway per box serves whichever account logged in.
pub const DEMO_SHARES_LIVE: &[(&str, UrlChannel, UrlProtocol, &str)] =
    &[("okx", Trade, Rest, ""), ("ibkr", Trade, Rest, "cpapi")];

/// **The `trade`/`user`/`login` keys a venue's KEYLESS feed dials too**, keyed as
/// [`DEMO_SHARES_LIVE`]: such a row is dialled with no account ([`needs_no_account`]). The venue's
/// bridge holds its rows here with a test.
///
/// * polymarket: the tape reads its books off the CLOB REST row and its frames off the `user` ws row
///   plus `/ws/market` (`crates/bridges/polymarket/src/config.rs`'s `clob_base` and `market_websocket_url`).
pub const KEYLESS_FEED_DIALS: &[(&str, UrlChannel, UrlProtocol, &str)] =
    &[("polymarket", Trade, Rest, ""), ("polymarket", User, Websocket, "")];

/// **Whether a row is dialled with NO account**: every `all`, `data` and `history` row (the feeds,
/// the public history downloads) and every key [`KEYLESS_FEED_DIALS`] names. Any other `trade`,
/// `user` or `login` row is dialled for an account of its venue at its type alone.
#[must_use]
pub fn needs_no_account(
    venue: &str,
    channel: UrlChannel,
    protocol: UrlProtocol,
    product: &str,
) -> bool {
    !matches!(channel, Trade | User | Login)
        || KEYLESS_FEED_DIALS
            .iter()
            .any(|&(v, c, p, pr)| v == venue && c == channel && p == protocol && pr == product)
}

/// **Why no ACTIVE account dials a row, or `None` when one may**: the gatewayhub's address probe
/// reports such a row NOT IN USE, probed but never down (decision 0265's verdict 8).
///
/// * a row that [`needs_no_account`] is in use;
/// * an ibkr row follows the box's ONE IBKR session (`crate::ibkr_session`'s `ibkr_row_unused`);
/// * any other row is in use while an active account of its venue reads as its type
///   ([`UrlType::of_tier`]: a `paper` row reads as `demo`, as it may hold the demo login a
///   credentialed feed signs in with).
///
/// `active` is the `(venue, tier)` of every ACTIVE account row; `setting` reads the row's venue's
/// tier-scoped `venue_setting` word (`venue.<venue>.<tier>.<field>`, trimmed, a blank one absent),
/// which only ibkr's rule asks (its `backend`).
#[must_use]
pub fn unused_row<'a>(
    venue: &str,
    ty: UrlType,
    channel: UrlChannel,
    protocol: UrlProtocol,
    product: &str,
    active: &[(String, AccountTier)],
    setting: impl Fn(AccountTier, &str) -> Option<&'a str>,
) -> Option<String> {
    if needs_no_account(venue, channel, protocol, product) {
        return None;
    }
    let mut tiers = active.iter().filter(|(v, _)| v == venue).map(|&(_, tier)| tier);
    if venue == "ibkr" {
        let session = crate::ibkr_session(tiers, |tier| setting(tier, "backend"));
        return crate::ibkr_session::ibkr_row_unused(session, ty, product);
    }
    let dialled = tiers.any(|tier| UrlType::of_tier(tier) == ty);
    (!dialled).then(|| format!("no active {venue} {ty} account"))
}

/// [`VENUE_URL_SEED`] in [`VenueUrls::from_rows`]'s shape.
fn seed_rows()
-> impl Iterator<Item = (&'static str, UrlType, UrlChannel, UrlProtocol, &'static str, &'static str)>
{
    VENUE_URL_SEED.iter().map(|s| (s.venue, s.ty, s.channel, s.protocol, s.product, s.url))
}

/// **The release seed's address of one exact key** `(venue, type, channel, protocol, product)`, as
/// the row holds it: no `all` row, no fallback and no guard ([`VenueUrls::url`] is the lookup). What
/// a test compares a lookup's answer with; `None` when the seed holds no such row.
#[must_use]
pub fn seed_url(
    venue: &str,
    ty: UrlType,
    channel: UrlChannel,
    protocol: UrlProtocol,
    product: &str,
) -> Option<&'static str> {
    let key = (venue, ty, channel, protocol, product);
    VENUE_URL_SEED
        .iter()
        .find(|s| (s.venue, s.ty, s.channel, s.protocol, s.product) == key)
        .map(|s| s.url)
}

/// The key of one row inside one venue: `(type, channel, protocol, product)`.
type Key = (UrlType, UrlChannel, UrlProtocol, String);

/// **One venue's addresses, as a composition root hands them down**, and [`VenueUrls::url`], the
/// ONE lookup. Built from the database's rows by `vike_secrets::venue_setting::load_venue_settings`,
/// or from [`VENUE_URL_SEED`] by [`VenueUrls::seed`] (a root with no settings database, a test).
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct VenueUrls {
    venue: String,
    rows: BTreeMap<Key, String>,
}

impl VenueUrls {
    /// `venue`'s rows among `rows` `(venue, type, channel, protocol, product, url)`; every other
    /// venue's row is ignored.
    pub fn from_rows<'a>(
        venue: &str,
        rows: impl IntoIterator<Item = (&'a str, UrlType, UrlChannel, UrlProtocol, &'a str, &'a str)>,
    ) -> VenueUrls {
        let rows = rows
            .into_iter()
            .filter(|r| r.0 == venue)
            .map(|(_, ty, channel, protocol, product, url)| {
                ((ty, channel, protocol, product.to_string()), url.to_string())
            })
            .collect();
        VenueUrls { venue: venue.to_string(), rows }
    }

    /// `venue`'s rows of the release's seed, [`VENUE_URL_SEED`]: what a fresh box's table holds.
    #[must_use]
    pub fn seed(venue: &str) -> VenueUrls {
        VenueUrls::from_rows(venue, seed_rows())
    }

    /// `venue`'s rows of the release's seed of type `ty` ONLY: a table that holds none of the other
    /// type's rows (a test's "only the live rows" box, which no demo lookup may borrow from).
    #[must_use]
    pub fn seed_typed(venue: &str, ty: UrlType) -> VenueUrls {
        VenueUrls::from_rows(venue, seed_rows().filter(|row| row.1 == ty))
    }

    /// The venue these rows belong to (`""` for [`VenueUrls::default`]).
    #[must_use]
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// **THE lookup: the base address of `(type, channel, protocol, product)` for this venue**, by
    /// the four steps of this module's doc, then the guard.
    ///
    /// A bridge asks it with its account's type, `UrlType::of_tier(inputs.tier)`:
    /// `inputs.settings.urls().url(UrlType::of_tier(inputs.tier), UrlChannel::Trade, UrlProtocol::Rest, "spot")`.
    ///
    /// # Errors
    /// [`UrlRefusal::Missing`] when no step finds a row; [`UrlRefusal::DemoIsLive`] when a demo
    /// `trade`/`user` address equals the live one and [`DEMO_SHARES_LIVE`] does not name the key.
    /// Either way the account does NOT RUN, with the refusal's words as the reason.
    pub fn url(
        &self,
        ty: UrlType,
        channel: UrlChannel,
        protocol: UrlProtocol,
        product: &str,
    ) -> Result<&str, UrlRefusal> {
        let public = matches!(channel, UrlChannel::Data | UrlChannel::History);
        let found = self.at(ty, channel, protocol, product).or_else(|| {
            (ty == UrlType::Demo && public)
                .then(|| self.at(UrlType::Live, channel, protocol, product))
                .flatten()
        });
        let key = || (self.venue.clone(), ty, channel, protocol, product.to_string());
        let Some(url) = found else {
            let (venue, ty, channel, protocol, product) = key();
            return Err(UrlRefusal::Missing { venue, ty, channel, protocol, product });
        };
        let private = matches!(channel, UrlChannel::Trade | UrlChannel::User);
        let shared = DEMO_SHARES_LIVE.iter().any(|&(v, c, p, pr)| {
            v == self.venue && c == channel && p == protocol && pr == product
        });
        let is_live = |c| self.at(UrlType::Live, c, protocol, product) == Some(url);
        if ty == UrlType::Demo
            && private
            && !shared
            && (is_live(UrlChannel::Trade) || is_live(UrlChannel::User))
        {
            let (venue, _, channel, protocol, product) = key();
            return Err(UrlRefusal::DemoIsLive {
                venue,
                channel,
                protocol,
                product,
                url: url.to_string(),
            });
        }
        Ok(url)
    }

    /// Steps 1 and 2 at one type: the exact row, else the channel-`all` row.
    fn at(
        &self,
        ty: UrlType,
        channel: UrlChannel,
        protocol: UrlProtocol,
        product: &str,
    ) -> Option<&str> {
        let get = |c| self.rows.get(&(ty, c, protocol, product.to_string()));
        get(channel).or_else(|| get(UrlChannel::All)).map(String::as_str)
    }
}

/// Why [`VenueUrls::url`] answered no address. Its `Display` is the account's NOT RUNNING reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlRefusal {
    /// No row answers the key, nor its channel-`all` row (nor, for demo data and history, the live
    /// rows).
    Missing {
        venue: String,
        ty: UrlType,
        channel: UrlChannel,
        protocol: UrlProtocol,
        product: String,
    },
    /// A demo `trade`/`user` address equals the venue's live `trade` or `user` address.
    DemoIsLive {
        venue: String,
        channel: UrlChannel,
        protocol: UrlProtocol,
        product: String,
        url: String,
    },
}

/// `product=<p>` when there is one.
fn product_words(product: &str) -> String {
    if product.is_empty() { String::new() } else { format!(" product={product}") }
}

impl fmt::Display for UrlRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlRefusal::Missing { venue, ty, channel, protocol, product } => write!(
                f,
                "{venue}: no address row for {ty} {channel} {protocol}{} (nor its `all` row) in the \
                 venue_url table; nothing is dialled. See `vike-cli config url ls --venue {venue}`, \
                 add one with `vike-cli config url set`",
                product_words(product)
            ),
            UrlRefusal::DemoIsLive { venue, channel, protocol, product, url } => write!(
                f,
                "{venue}: the demo {channel} {protocol}{} address {url} is the venue's LIVE address; \
                 a demo account never dials live. Refused. Fix the row with `vike-cli config url \
                 reset {venue} demo {channel} {protocol}` or `config url set`",
                product_words(product)
            ),
        }
    }
}

impl std::error::Error for UrlRefusal {}

#[cfg(test)]
#[path = "venue_urls_tests.rs"]
mod tests;
