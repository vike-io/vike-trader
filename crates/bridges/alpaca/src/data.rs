//! Alpaca market-data WS -> `vike_data::live::{DataClient, LiveDataSink}`.
//!
//! This module has ONE pure half (`data/decode.rs`'s [`decode_ws_message`], fixture-tested in
//! `tests/mapper_data.rs`, no network) and one impure half (`data/client.rs`'s [`AlpacaDataClient`]
//! over `data/conn.rs`'s connection thread, the WS connect/auth/subscribe/read loop); this root holds
//! the route, frame and timestamp helpers both halves share.
//!
//! **One shared connection per asset-class stream (verified live 2026-07-14).** Alpaca's
//! market-data WS enforces ONE connection per key per stream — a 2nd concurrent connection to the
//! same endpoint returns `{"T":"error","code":406,"msg":"connection limit exceeded"}`. But the two
//! streams are SEPARATE endpoints, so a crypto and an equity connection on the same OAuth key run
//! concurrently. So this client keeps at most one shared connection per [`StreamRoute`], each
//! multiplexing ALL its symbols/verbs: `subscribe_*` resolves the symbol's asset class and routes
//! on THAT ([`StreamRoute::for_class`]), lazily opening that route's connection on first use and
//! sending incremental `subscribe`/`unsubscribe`
//! frames over a command channel; the connection is torn down when its last subscription drops.
//! Each connection re-sends every active subscription after a reconnect, so a drop doesn't silently
//! lose a feed.
//!
//! **WS endpoints** (`AlpacaConfig::hosts.data_ws` resolves sandbox-vs-live, see `hosts.rs`): crypto
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
//! same Bearer [`crate::auth::TokenSource`] mints for every other plane (REST broker, REST data,
//! SSE trade events) is reused here. See [`auth_frame`].
//!
//! Alpaca WS bars are fixed 1-minute closed bars (no intrabar "forming" frame is published), so
//! every decoded bar maps to `interval = "1m"`, `closed = true`.

mod client;
mod conn;
mod decode;
mod registry;

#[cfg(doc)]
use vike_data::DataClient;
use vike_model::AssetClass;

pub use client::AlpacaDataClient;
pub use decode::{DataMsg, decode_ws_message};

/// Canonical venue id — the key every live-data sink call in `data/conn.rs` and `data/client.rs`
/// is filed under, and the id `crate::mount`'s `AlpacaVenueMount` answers with.
pub(crate) const VENUE: &str = "alpaca";

/// Tolerant RFC3339-UTC -> epoch-ms; folds to `0` on any parse failure (the same "core stamps its
/// own receive time" fallback `event_mapper::parse_ts_ms` uses elsewhere in this crate). Dependency-free
/// fixed-field parse mirroring `vike_polymarket::gamma`'s `parse_rfc3339_utc_ms`, reusing the
/// shared civil-calendar helper [`vike_model::time::days_from_civil`] rather than pulling in
/// `chrono` for this one call site.
fn parse_alpaca_ts_ms(s: &str) -> i64 {
    parse_rfc3339_ms(s).unwrap_or(0)
}

/// Tolerant fixed-field RFC3339-UTC -> epoch-ms parser (dependency-free, no `chrono`). Shared with
/// `crate::event_mapper::parse_ts_ms`, which prefers this for Alpaca's SSE `timestamp` field (a real
/// RFC3339 string, e.g. `"2026-07-14T05:39:31.4Z"`) before falling back to a bare numeric-epoch-ms
/// parse.
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    if !(0..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    // seconds may carry a `.fff...` fraction and/or a `Z`/`+HH:MM` suffix.
    let sec_field = tp.next()?;
    let sec_str: String = sec_field.chars().take_while(|c| c.is_ascii_digit()).collect();
    let se: i64 = sec_str.parse().ok()?;
    let ms: i64 = sec_field
        .split_once('.')
        .map(|(_, frac)| {
            let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
            format!("{digits:0<3}").parse().unwrap_or(0)
        })
        .unwrap_or(0);
    if !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=60).contains(&se) {
        return None;
    }
    let days = vike_model::time::days_from_civil(y, mo as u32, d as u32);
    Some((((days * 24 + h) * 60 + mi) * 60 + se) * 1000 + ms)
}

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
    /// authenticates and then answers silence (the failure this crate's `CLAUDE.md` records for a
    /// mis-routed symbol). The match is exhaustive rather than `_`-terminated so a new variant in
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
}

/// The asset class of an Alpaca symbol, GUESSED from its shape: a slash-delimited pair
/// (`"BTC/USD"`) is crypto spot, anything else is an equity.
///
/// ⚠ **This is the guess `docs/decisions/0061` wants gone, and it is still here because the class
/// is not available where it is called.** [`DataClient::subscribe_bars`] and its `subscribe_quotes`
/// / `subscribe_trades` siblings take a symbol and an interval and nothing else, so this file has
/// no class to route on. The value EXISTS and stops one call short:
/// `crates/vike-app-core/src/ui/feed_lifecycle.rs`'s `SeriesSpec` carries an
/// `Option<vike_model::AssetClass>` all the way into `ensure_feed_on`, which spends it on
/// `venue_bar_instrument` and then calls `subscribe_bars` without it. Threading it would mean
/// widening that trait's signature to carry an `Option<AssetClass>` — `Option`, because the
/// quick-pick path genuinely has no catalog hit behind it — and falling back to this function
/// only for the `None` side. Until then the guess is quarantined HERE, answering in the shared
/// vocabulary rather than in a venue-local enum, and [`StreamRoute::for_class`] — not this
/// function — picks the endpoint.
///
/// ⚠ It routes on the symbol VERBATIM and never normalizes: hand it the vike spelling of a crypto
/// pair (`"BTCUSD"`) and it answers `Equity`, which subscribes a ticker IEX does not carry. This
/// crate's `CLAUDE.md` carries what that costs and which smoke encodes it.
fn class_of(symbol: &str) -> AssetClass {
    if symbol.contains('/') { AssetClass::CryptoSpot } else { AssetClass::Equity }
}

fn ws_url(data_ws: &str, route: StreamRoute) -> String {
    match route {
        StreamRoute::Crypto => format!("{data_ws}/v1beta3/crypto/us"),
        // IEX (free) — `sip` needs a paid entitlement (`409 insufficient subscription`).
        StreamRoute::Equity => format!("{data_ws}/v2/iex"),
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

#[path = "data_tests.rs"]
#[cfg(test)]
mod data_tests;
