//! Alpaca market-data WS -> `vike_data::live::{DataClient, DataClientSink}`.
//!
//! This module has ONE pure half (`feed/decode.rs`'s [`decode_websocket_message`], fixture-tested in
//! `tests/mapper_data.rs`, no network) and one impure half (`feed/client.rs`'s [`DataClientSpecAlpaca`],
//! this venue's half of the shared data engine `vike_bridge_core::data_client::ws::DataClientWebsocket`, with
//! `feed/connection.rs`'s route socket, the WS dial/auth/subscribe/read its lanes share); this root holds
//! the route, frame and timestamp helpers both halves share. The engine spawns one lane per subscription on the
//! market pump with this venue's `websocket_session::spec` row (a bounded dial, an idle watchdog, a fixed redial).
//!
//! **One shared connection per asset-class stream (verified live 2026-07-14).** Alpaca's
//! market-data WS enforces ONE connection per key per stream — a 2nd concurrent connection to the
//! same endpoint returns `{"T":"error","code":406,"msg":"connection limit exceeded"}`. But the two
//! streams are SEPARATE endpoints, so a crypto and an equity connection on the same OAuth key run
//! concurrently. So this client keeps at most one shared connection per [`StreamRoute`], each
//! multiplexing ALL its symbols/verbs: `subscribe_*` takes a USER name (`BTCUSD`, `AAPL`), reads its
//! kept row's asset class and wire name ([`class_of`]) and routes on THAT ([`StreamRoute::for_class`]);
//! the route's first lane opens its connection, every lane's key is one incremental `subscribe` frame (in the
//! wire name, `BTC/USD`) when it is the key's first holder and one `unsubscribe` when its last lets go, and the
//! connection is torn down when its last lane drops. Every message is delivered under the user name its lane was
//! subscribed by. A connection that ends tells every lane on it, and each lane comes back through the pump's
//! backoff with its own `subscribe`, so a drop doesn't silently lose a feed.
//!
//! **WS endpoints** (`AlpacaConfig::hosts.data_websocket` resolves sandbox-vs-live, see `transport/urls.rs`): crypto
//! ⇒ `{data_ws}/v1beta3/crypto/us`; equities ⇒ `{data_ws}/v2/iex` — the free IEX feed. `sip` needs
//! a paid entitlement (the sandbox OAuth token gets `409 insufficient subscription` on `/v2/sip`),
//! so `iex` is used for both sandbox and live (verified live 2026-07-14).
//!
//! **WS OAuth auth handshake (verified live 2026-07-14 against the sandbox crypto stream).**
//! Alpaca's market-data WS authenticates OAuth integrations with the literal key `"oauth"` and the
//! OAuth Bearer as the `secret`: `{"action":"auth","key":"oauth","secret":"<bearer>"}` → the server
//! replies `{"T":"success","msg":"authenticated"}` then the subscription confirmation, and live
//! `q`/`t`/`b` frames flow. (The earlier `{"action":"auth","token":...}` guess and passing the
//! client_id/secret as key/secret both return `{"T":"error","code":402,"msg":"auth failed"}`.) The
//! same Bearer [`crate::transport::sign::TokenSource`] mints for every other plane (REST broker, REST data,
//! SSE trade events) is reused here. See [`auth_frame`].
//!
//! Alpaca WS bars are fixed 1-minute closed bars (no intrabar "forming" frame is published), so
//! every decoded bar maps to `interval = "1m"`, `closed = true`.

mod client;
mod decode;

mod connection;

use vike_model::AssetClass;

use crate::refusals::AlpacaNameRefusal;

pub use client::DataClientSpecAlpaca;
pub use decode::{FeedMessage, decode_websocket_message};

/// Canonical venue id — the key every live-data sink call of `feed/client.rs`'s lanes
/// is filed under, and the id `crate::adapter`'s `VenueAdapterAlpaca` answers with.
pub(crate) const VENUE: &str = "alpaca";

/// A market-data subscribe verb. All verbs for a route multiplex over that route's one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Verb {
    Bars,
    Quotes,
    Trades,
}

impl Verb {
    /// The Alpaca subscribe/unsubscribe channel key for this verb.
    fn channel(self) -> &'static str {
        match self {
            Verb::Bars => "bars",
            Verb::Quotes => "quotes",
            Verb::Trades => "trades",
        }
    }
}

/// The two Alpaca market-data streams. Each is a SEPARATE endpoint with its own one-connection-per-
/// key limit, so the client keeps at most one shared connection per route (see the module doc).
///
/// ⚠ **This is a ROUTE, not a taxonomy, and it was called `AssetClass` until it was renamed.** With
/// two variants spelling the two classes this venue lists, it read as a second, venue-local copy of
/// the workspace vocabulary — `docs/decisions/0061` names that shadowing as one of this axis's
/// partial encodings. It never was one. It answers "which WS URL does this subscription open", a
/// question with two answers here and NO answer for a class Alpaca does not stream; the taxonomy
/// answers "what kind of instrument is this", a question with eleven. [`StreamRoute::for_class`] is
/// the seam between them, so the class is what DECIDES and this enum is only what is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StreamRoute {
    Crypto,
    Equity,
}

impl StreamRoute {
    /// Which stream serves an instrument of `class`, or `None` when Alpaca streams none.
    ///
    /// The route is taken FROM THE CLASS rather than from the shape of the symbol string — the
    /// implicit encoding `docs/decisions/0061` exists to remove. What this venue LISTS is declared
    /// by `crates/bridges/alpaca/src/catalog.rs`'s `asset_classes` (`Equity` + `CryptoSpot`), and
    /// those are the two that route. [`AssetClass::Etf`] joins the equity arm because the equity
    /// stream is IEX's tape, which carries listed ETFs — and because Alpaca's own `/v1/assets` returns
    /// `class == "us_equity"` for an ETF, so `parse_assets` in that same file hands one back as
    /// `Equity` and this arm only ever matters for a class threaded in from another source.
    ///
    /// Everything else is `None`: Alpaca lists no perp, no future, no option, no FX, no index, no
    /// CFD, no bond, fund, warrant, commodity or combo, so there is no endpoint to pick and guessing one would open a socket that
    /// authenticates and then answers silence (a mis-routed symbol's failure: the venue's error frame
    /// decodes to no data). The match is exhaustive rather than `_`-terminated so a new variant in
    /// the taxonomy is a compile error HERE, where somebody must decide what Alpaca does with it.
    fn for_class(class: AssetClass) -> Option<StreamRoute> {
        match class {
            AssetClass::CryptoSpot => Some(StreamRoute::Crypto),
            AssetClass::Equity | AssetClass::Etf => Some(StreamRoute::Equity),
            AssetClass::CryptoPerp
            | AssetClass::CryptoFuture
            | AssetClass::Option
            | AssetClass::Fx
            | AssetClass::Future
            | AssetClass::Index
            | AssetClass::PredictionMarket
            | AssetClass::Cfd
            | AssetClass::Bond
            | AssetClass::Fund
            | AssetClass::Warrant
            | AssetClass::Commodity
            | AssetClass::Combo => None,
        }
    }

    /// The feed letter [`decode_websocket_message`] takes for a frame of this stream.
    fn prefix(self) -> char {
        match self {
            StreamRoute::Crypto => 'c',
            StreamRoute::Equity => 's',
        }
    }
}

/// **The asset class and the venue's spelling of the USER name `symbol`, both off its KEPT row** (decision 0290, audit
/// flag ALP-1; `crate::names::wire_of`, the one translation the order path and the reconcile read too): `BTCUSD` is
/// the coin `BTC/USD` ([`AssetClass::CryptoSpot`]), `AAPL` the share `AAPL` ([`AssetClass::Equity`]). The class is the
/// row's, never the shape of a name; [`StreamRoute::for_class`] picks the endpoint from it, and the subscribe frame
/// carries the wire name.
///
/// # Errors
/// A name with no kept row (or two), in words naming the fix: nothing is dialled.
fn class_of(symbol: &str) -> Result<(AssetClass, String), AlpacaNameRefusal> {
    crate::names::wire_of(symbol).map(|(wire, class)| (class, wire))
}

fn websocket_url(data_websocket: &str, route: StreamRoute) -> String {
    match route {
        StreamRoute::Crypto => format!("{data_websocket}/v1beta3/crypto/us"),
        // IEX (free) — `sip` needs a paid entitlement (`409 insufficient subscription`).
        StreamRoute::Equity => format!("{data_websocket}/v2/iex"),
    }
}

fn auth_frame(bearer: &str) -> serde_json::Value {
    // OAuth market-data WS handshake, verified live 2026-07-14: key = literal "oauth", secret =
    // the OAuth Bearer. `{"action":"auth","token":..}` returns 402 "auth failed".
    serde_json::json!({"action": "auth", "key": "oauth", "secret": bearer})
}

/// Build a `{"action":<action>,"<channel>":["<symbol>"]}` frame. `action` is `"subscribe"` or
/// `"unsubscribe"`; the channel key is dynamic (`verb.channel()`), so it's built on a `Map` rather
/// than via `json!` (which needs a literal key).
fn sub_action(action: &str, verb: Verb, symbol: &str) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("action".to_string(), serde_json::Value::String(action.to_string()));
    m.insert(verb.channel().to_string(), serde_json::json!([symbol]));
    serde_json::Value::Object(m)
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
