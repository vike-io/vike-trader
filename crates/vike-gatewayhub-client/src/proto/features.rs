//! The `Welcome.features` capability vocabulary of the gatewayhub: every string the daemon
//! advertises and a client negotiates on, and the builder/reader pairs for the value-carrying
//! entries (`md_venue=`, `cd_venue=`).
//!
//! An additive verb is negotiated by one of these strings, never by a `GATEWAYHUB_PROTO_VERSION`
//! bump, and has three legs — the daemon advertises it exactly when it serves the verb, the client
//! refuses LOCALLY without sending when it is absent (naming the string), and an older daemon
//! answers the unknown variant with `Response::Error` and KEEPS the connection:
//! `docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`.
//!
//! Every capability check is whole-string equality (`f == FEATURE_*`), so a value-carrying entry
//! can neither satisfy nor shadow a named capability.
//!
//! ⚠ **What is advertised is a fact about the BUILD and the ARMED lanes of the daemon, never about
//! a venue's liveness.** The `Welcome` is the same whether or not a venue is reachable: a string
//! says "this daemon serves the verb", and the verb's own answer says "the venue just failed".

/// The plane sentinel: the one string every gatewayhub pushes UNCONDITIONALLY, so a client learns
/// BEFORE it signs which daemon answered. A client that cannot find it refuses to send `Auth`
/// (`client::GatewayClient::connect`): a mac signed toward the wrong port is a mac for another
/// service (the datahub's twin is `load_bars`).
///
/// ⚠ A constant rather than a literal because something REFUSES on it, and a sentinel renamed on one
/// side only would refuse every connection. The VALUE is the frozen token.
pub const GATEWAYHUB_PLANE_SENTINEL: &str = "gatewayhub";

/// Gates the `stream` socket class: [`Request::DataOpen`](super::Request::DataOpen) and the
/// `StreamUp` / `StreamDown` (`vike_market_stream::wire`) frames after it.
///
/// A BUILD fact. Which venues the stream serves is the per-venue [`FEATURE_MD_VENUE_PREFIX`] set.
pub const FEATURE_GW_DATA: &str = "gw_data";

/// Gates the delivery class of a stream subscription: the `class` of a `StreamUp::Sub`, the
/// `StreamUp::Delivery` frame that changes it on a live subscription and the `StreamDown::Delivery`
/// marker that tells the subscriber where in the frame order it changed (`vike_market_stream::wire`).
/// A subscriber that cannot take a superseded frame (the datahub's recorder on a depth lane) raises
/// the class; a daemon without the string conflates as it always did.
///
/// A BUILD fact, advertised with [`FEATURE_GW_DATA`] (it is a property of the stream plane). The VALUE
/// is the frozen token `stream_class`.
pub const FEATURE_GW_STREAM_CLASS: &str = "stream_class";

/// Gates the paged FETCH family — [`Request::Fetch`](super::Request::Fetch) with its
/// `FetchProgress` / `FetchPage` / `FetchDone` / `FetchFailed` replies — for the keyless lanes:
/// klines, funding, the dukascopy tick range, the catalog list and the option-contract index.
pub const FEATURE_GW_FETCH: &str = "gw_fetch";

/// Gates the progress frames' CONTRACT: the daemon answers every `Fetch` with an immediate
/// `FetchProgress` and then at least one frame every `FETCH_PROGRESS_EVERY`, so the client's idle
/// bound is per request (`max(floor, 3 × next_in_ms)`) instead of unbounded.
///
/// ⚠ A daemon without it may be silent for the whole fetch, and the client refuses it by name
/// rather than guess a bound that would clip a slow but live fetch (R8: a remote fetch must not time
/// out sooner than the in-process one).
pub const FEATURE_GW_FETCH_PROGRESS: &str = "gw_fetch_progress";

/// Gates [`Request::FetchCancel`](super::Request::FetchCancel): the client abandons a fetch it
/// started and the daemon stops at its next page or hour-file boundary.
///
/// ⚠ Absent, a client cannot stop a fetch early; it still DROPS the connection, which cancels
/// every fetch that connection started, so a stop probe keeps working — only more slowly.
pub const FEATURE_GW_FETCH_CANCEL: &str = "gw_fetch_cancel";

/// Gates [`Request::FetchCredentialed`](super::Request::FetchCredentialed) — the `Scope::Write`
/// lane: a fetch whose venue call spends a credential the DAEMON holds (the OANDA practice token).
/// The set of venues it serves is the per-venue [`FEATURE_CD_VENUE_PREFIX`] set.
pub const FEATURE_GW_FETCH_CREDENTIALED: &str = "gw_fetch_credentialed";

/// Gates the dukascopy tick range ([`FetchWhat::Ticks`](super::FetchWhat::Ticks)). A separate
/// string from [`FEATURE_GW_FETCH`] because only the build that links the `.bi5` fetch serves it.
pub const FEATURE_GW_TICKS: &str = "gw_ticks";

/// Gates [`Request::CatalogDescribe`](super::Request::CatalogDescribe) and the catalog lanes of
/// [`Request::Fetch`](super::Request::Fetch) (`CatalogList`, `CatalogSearch`).
pub const FEATURE_GW_CATALOG: &str = "gw_catalog";

/// Gates the options surface: [`Request::OptionsRest`](super::Request::OptionsRest), the
/// `OptionIndex` fetch and [`Request::OptionsFeedOpen`](super::Request::OptionsFeedOpen).
pub const FEATURE_GW_OPTIONS: &str = "gw_options";

/// Gates [`Request::ResolveFamily`](super::Request::ResolveFamily) — the polymarket rolling-family
/// resolver.
pub const FEATURE_GW_FAMILY: &str = "gw_family";

/// Gates [`Request::CredentialPresence`](super::Request::CredentialPresence) — a presence WORD for
/// the credential a lane reads, never a value (decision 0102's rule carried over the hop).
pub const FEATURE_GW_PRESENCE: &str = "gw_presence";

/// Gates [`Request::GwStatus`](super::Request::GwStatus) — names and counters, never values.
pub const FEATURE_GW_STATUS: &str = "gw_status";

/// The prefix of a per-venue STREAM entry — `md_venue=binance` — one per KEYLESS venue the daemon's
/// build links a market-data client for, so a client learns at the HANDSHAKE which venues it may
/// open ([`Request::DataOpen`](super::Request::DataOpen)) rather than one refusal at a time.
///
/// Written and read by a PAIR ([`md_venue_feature`] / [`advertised_md_venues`]) so the two ends
/// cannot drift on the spelling; the precedent is `vike-datahub-client`'s `md_venue=`. The
/// spelling is the same on purpose and the SETS are different: this one is the gatewayhub's.
pub const FEATURE_MD_VENUE_PREFIX: &str = "md_venue=";

/// Build the `md_venue=<slug>` entry — the WRITE half of [`advertised_md_venues`]. A pure `format!`:
/// which venues a build advertises is the daemon's (`cfg` over its bridge features).
pub fn md_venue_feature(slug: &str) -> String {
    format!("{FEATURE_MD_VENUE_PREFIX}{slug}")
}

/// Every stream venue slug a daemon advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`md_venue_feature`]. Each value is trimmed and an EMPTY value is dropped: an empty
/// advertisement advertises nothing.
pub fn advertised_md_venues(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_MD_VENUE_PREFIX)
}

/// The prefix of a per-venue CREDENTIALED-FETCH entry — `cd_venue=oanda`: the venues whose
/// credentialed lane ([`FEATURE_GW_FETCH_CREDENTIALED`]) this daemon serves, because it holds the
/// credential for them. A separate set from [`FEATURE_MD_VENUE_PREFIX`]: the stream plane serves
/// keyless venues only, and a venue is never in both.
pub const FEATURE_CD_VENUE_PREFIX: &str = "cd_venue=";

/// Build the `cd_venue=<slug>` entry — the WRITE half of [`advertised_cd_venues`].
pub fn cd_venue_feature(slug: &str) -> String {
    format!("{FEATURE_CD_VENUE_PREFIX}{slug}")
}

/// Every credentialed-fetch venue slug a daemon advertised, in ADVERTISEMENT ORDER. It reads ONE
/// prefix, deliberately: a reader that also accepted `md_venue=` would report a keyless venue as
/// credentialed.
pub fn advertised_cd_venues(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_CD_VENUE_PREFIX)
}

/// The trimmed, non-empty values of every entry that starts with `prefix`, in order.
fn values_after(features: &[String], prefix: &str) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(prefix))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every fixed capability string this crate defines, the plane sentinel first: the vocabulary a daemon
/// picks its `Welcome.features` from (which of them it advertises is the daemon's build and armed-lane
/// fact; the `md_venue=` and `cd_venue=` entries are built by [`md_venue_feature`] and
/// [`cd_venue_feature`]) and the list a test peer advertises whole. `every_fixed_feature_is_known` (in `proto_tests.rs`) holds it complete against
/// this file.
pub const KNOWN_FEATURES: [&str; 13] = [
    GATEWAYHUB_PLANE_SENTINEL,
    FEATURE_GW_DATA,
    FEATURE_GW_STREAM_CLASS,
    FEATURE_GW_FETCH,
    FEATURE_GW_FETCH_PROGRESS,
    FEATURE_GW_FETCH_CANCEL,
    FEATURE_GW_FETCH_CREDENTIALED,
    FEATURE_GW_TICKS,
    FEATURE_GW_CATALOG,
    FEATURE_GW_OPTIONS,
    FEATURE_GW_FAMILY,
    FEATURE_GW_PRESENCE,
    FEATURE_GW_STATUS,
];
