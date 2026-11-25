//! The `Welcome.features` words more than one hub protocol speaks, and the one reader of a value-carrying entry.
//!
//! Each hub's own vocabulary stays in its client (`proto/features.rs` of `vike-datahub-client`,
//! `vike-gatewayhub-client` and `vike-backtesthub-client`, `src/proto.rs` of `vike-tradehub-client`). A word lives
//! HERE only when it means the same thing on two wires, so its bytes have one spelling: a second copy could drift
//! and a client would then refuse a hub that serves it. Which of these a hub advertises is still that hub's fact.
//!
//! A value-carrying entry is `prefix=value` (`market_data_venue=binance`). [`values_after`] and [`value_after`] read
//! it for every prefix of every hub; a writer is a `format!` of the prefix and the value. Every capability check is
//! whole-string equality (`f == FEATURE_*`), so such an entry can neither satisfy nor shadow a named capability.

/// **This hub reads and answers every frame past the handshake in msgpack with field names** (`crate::frame`'s
/// `BodyCodec`, the owner's "JSON only at the edges" of 2026-11-19): a client that sees it writes its requests in
/// the form it chose, and the hub answers each request, and every frame a request opens, in the form that request
/// came in. ONE switch per connection for every verb; negotiated per decision 0112. The datahub, the backtesthub,
/// the tradehub and the gatewayhub advertise it, each unconditionally: a BUILD fact.
///
/// ⚠ **A client sends no msgpack to a hub without it**: an older hub decodes JSON alone and would refuse every verb.
/// A client that never asks (an older one, or one that chose JSON) is answered JSON, byte for byte as before. LZ4 on
/// top is the client's choice where its link is thin (the datahub's, `DatahubClient::set_thin_link_for_this_process`);
/// a binary body a verb asked for (the datahub's bar columns) keeps its own form whatever was negotiated.
pub const FEATURE_WIRE_MSGPACK: &str = "wire_msgpack";

/// Gates `Request::GatewayStatus`, the gatewayhub's state (names and counters, never values), on the two wires that
/// answer it: the gatewayhub's own (`vike_gatewayhub_client::GatewayClient::status`) and the datahub's relay of it
/// (`vike_datahub_client::DatahubClient::gateway_status`); negotiated per decision 0112.
///
/// A BUILD fact on both, advertised UNCONDITIONALLY. On the datahub it is not gated on the link: whether the
/// gatewayhub answers travels INSIDE the reply, so a datahub whose gatewayhub is down never looks like an old one.
/// Without it a client refuses locally, naming the string, and a caller reads the gatewayhub's state as unknown,
/// never as down.
pub const FEATURE_GATEWAY_STATUS: &str = "gateway_status";

/// **This hub answers a refusal it declared with an id as the refusal FRAME** (`crate::refusal::HubRefusal`, each hub's
/// `Response::Refused`), and a stream session opened on the connection refuses a subscription with its CLASS
/// (`vike_market_stream::wire::StreamSubscriptionErrorKind`'s `NotNow` and `Cap`), toward a client that SAID it reads
/// them: the client sends `crate::handshake::HandshakeFrame::Reads` right after its handshake, and only to a hub that
/// advertised this word (decision 0112's three legs, owner's Q8 of 2026-11-23). A client that never says so (an older
/// one) is answered `Response::Error` and the old untyped kinds, byte for byte as before. A BUILD fact: the datahub,
/// the tradehub and the gatewayhub advertise it unconditionally.
pub const FEATURE_REFUSAL_FRAME: &str = "refusal_frame";

/// The prefix of a per-venue MARKET-DATA entry — `market_data_venue=binance` — one per KEYLESS venue a hub's build
/// serves a live market-data stream for, so a client learns at the HANDSHAKE which venues it may open rather than
/// one refusal at a time. The datahub's market-data plane (`vike_datahub_client::market::MarketDataSpec`) and the
/// gatewayhub's `stream` class (`DataOpen`) advertise it: the word is the same and each hub's SET is its own
/// (`crates/vike-datahub/src/market_data/venues.rs`'s `supported`, the gatewayhub's bridge features).
///
/// Written and read by a PAIR ([`market_data_venue_feature`] / [`advertised_market_data_venues`]) so the two ends
/// cannot drift on the spelling.
pub const FEATURE_MARKET_DATA_VENUE_PREFIX: &str = "market_data_venue=";

/// Build the `market_data_venue=<slug>` entry — the WRITE half of [`advertised_market_data_venues`].
///
/// ⚠ A pure `format!`: which venues a build advertises is the hub's; this crate names no venue.
pub fn market_data_venue_feature(slug: &str) -> String {
    format!("{FEATURE_MARKET_DATA_VENUE_PREFIX}{slug}")
}

/// Every market-data venue slug a hub advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`market_data_venue_feature`], through [`values_after`].
pub fn advertised_market_data_venues(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_MARKET_DATA_VENUE_PREFIX)
}

/// The values of every entry that starts with `prefix`, in ADVERTISEMENT ORDER: each trimmed, and an EMPTY one
/// dropped (an empty advertisement advertises nothing). It reads ONE prefix, so a reader of one set never reports
/// another set's entry.
pub fn values_after(features: &[String], prefix: &str) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(prefix))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// The value of the FIRST entry that starts with `prefix`, trimmed, for a prefix a hub advertises once
/// (`build=`, `datahub=`): `None` when there is none or that first one is empty (absence is the answer).
pub fn value_after(features: &[String], prefix: &str) -> Option<String> {
    features
        .iter()
        .find_map(|f| f.strip_prefix(prefix))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
