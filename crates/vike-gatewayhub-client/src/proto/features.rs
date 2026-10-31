//! The `Welcome.features` capability vocabulary of the gatewayhub: every string the daemon
//! advertises and a client negotiates on, and the builder/reader pairs for the value-carrying
//! entries (`md_venue=`, `cd_venue=`, `cs_venue=`).
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

/// Gates the tick PUMP op of the stream plane (`SubOp::Ticks`, `vike_market_stream::wire`): the CEX
/// `spawn_*_market_data` feeds hosted at the daemon, delivering a symbol's quote, trade and book frames
/// at the pump's own granularity on the caller's price grid. A BUILD fact, advertised when a pump table
/// is mounted. A daemon without it answers the op's frame as a desync, so a client that hosts a pump
/// refuses locally (naming the string) rather than send one.
pub const FEATURE_GW_PUMP: &str = "gw_pump";

/// Gates the `network` of [`Request::DataOpen`](super::Request::DataOpen): the daemon builds the venue
/// client for the network the session names (the one venue whose feed follows the account tier,
/// Hyperliquid). ⚠ A daemon that does NOT advertise it ignores the unknown field and would open the
/// MAIN network, so a client that asks for a network refuses locally without this string and never sends
/// the open.
pub const FEATURE_GW_STREAM_NETWORK: &str = "stream_network";

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

/// The prefix of a per-venue CREDENTIALED-STREAM entry — `cs_venue=alpaca`: the venues whose stream the
/// daemon logs in to as the account the strategy trades, because it holds the credential
/// (`Scope::Write` only; the credential never crosses the hop). A separate set from
/// [`FEATURE_MD_VENUE_PREFIX`]: a venue is keyless or credentialed, never both.
pub const FEATURE_CS_VENUE_PREFIX: &str = "cs_venue=";

/// Build the `cs_venue=<slug>` entry — the WRITE half of [`advertised_cs_venues`].
pub fn cs_venue_feature(slug: &str) -> String {
    format!("{FEATURE_CS_VENUE_PREFIX}{slug}")
}

/// Every credentialed-stream venue slug a daemon advertised, in ADVERTISEMENT ORDER. It reads ONE prefix,
/// like [`advertised_cd_venues`]: a reader that also accepted `md_venue=` would report a keyless venue as
/// credentialed.
pub fn advertised_cs_venues(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_CS_VENUE_PREFIX)
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
pub const KNOWN_FEATURES: [&str; 15] = [
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
    // The strategy lane's strings (wave 2, W10) are appended AFTER the wave-1 list, as the daemon appends them.
    FEATURE_GW_STREAM_NETWORK,
    FEATURE_GW_PUMP,
];

// ---- The TRADE class (wave 2) --------------------------------------------------------------------
//
// Armed together by a daemon that serves the trade lane (`crates/vike-gatewayhub`'s `trade/`), each a
// separate string because each is a separate DECODE leg an older daemon may lack. A client refuses
// LOCALLY, naming the string, before it signs or sends (`GatewayClient::require_feature`): `Auth` under
// `Scope::Trade` is never written to a daemon that did not advertise `gw_trade`.
//
// They are NOT in `KNOWN_FEATURES`: that list is the DATA vocabulary and the daemon's own test holds a
// full data build equal to it. The trade strings are `KNOWN_TRADE_FEATURES`, and the daemon's trade
// lane appends them when it serves the class.

/// Gates the `trade` socket class itself: [`Request::TradeOpen`](super::Request::TradeOpen), the
/// `TradeUp` / `TradeDown` frames after it and `Scope::Trade`.
pub const FEATURE_GW_TRADE: &str = "gw_trade";

/// Gates the resume cursor: `TradeOpen.resume`, `Event.seq` replay and `ReplayUnavailable`. Absent, a
/// client opens with no cursor and reconciles instead of replaying.
pub const FEATURE_GW_TRADE_CURSOR: &str = "gw_trade_cursor";

/// Gates `TradeUp::ResolveOrders` / `TradeDown::OrdersResolved`: the query-only resolution of a
/// Written-but-unacknowledged submit after a gatewayhub restart.
pub const FEATURE_GW_TRADE_RESOLVE: &str = "gw_trade_resolve";

/// Gates `TradeUp::WireIds` / `TradeDown::WireIdsReply`: the boot-time translation of restored client
/// order ids (hyperliquid's `cloid = keccak(coid)`), answered from a cache before the fold runs.
pub const FEATURE_GW_TRADE_WIRE_IDS: &str = "gw_trade_wire_ids";

/// Gates the lease: one trade client per `(venue, account)`, `LeaseHeld`, the explicit fenced takeover
/// and the duplex heartbeat.
pub const FEATURE_GW_LEASE: &str = "gw_lease";

/// Gates [`Request::ArmingQuery`](super::Request::ArmingQuery) and its `ArmingTable` reply.
pub const FEATURE_GW_ARMING: &str = "gw_arming";

/// Gates `TradeUp::AdapterStart` / `TradeDown::AdapterReply`: the venue adapter start over the lease.
pub const FEATURE_GW_ADAPTER: &str = "gw_adapter";

/// Gates [`Request::PreflightQuery`](super::Request::PreflightQuery) and its `PreflightLegs` reply.
pub const FEATURE_GW_PREFLIGHT: &str = "gw_preflight";

/// Gates `AdapterReply::Live::contract_value`: the notional kind and per-unit factor of the engine symbol
/// (decision 0170). A daemon without it sends the old scalar and no value, which this client reads as
/// `None` (`Linear`, 1.0): right for a venue whose bridge reports none, WRONG for one that can report an
/// inverse contract (deribit) or an exchange multiplier (ibkr), so the tradehub refuses to run those on a
/// daemon that does not advertise it (`TradeSession::reports_contract_value`).
pub const FEATURE_GW_TRADE_CONTRACT_VALUE: &str = "gw_trade_contract_value";

/// Gates `TradeUp::SymbolFacts` / `TradeDown::SymbolFactsReply`: the venue facts of ANY symbol of the
/// lease's exchange over the trade key alone, so the node can admit it (`Command::AdmitSymbol`, decision
/// 0173). Only a venue whose `TradeLane` answers it (bybit, binance, aster) gives a fact; the rest answer a reason.
pub const FEATURE_GW_TRADE_SYMBOL_FACTS: &str = "gw_trade_symbol_facts";

/// Gates `TradeUp::VenueRead` / `TradeDown::VenueReadReply`: an operator's READ of the lease's account at
/// the venue (its balance with the per-exchange allowance, or its open orders), over the trade key
/// alone. A DECODE capability: a daemon that predates it cannot decode the variant, so a client refuses
/// locally against one that does not advertise it. Advertised by every build that carries the trade lane;
/// a venue whose bridge offers no such read answers `not offered by <venue>` in the reply, not here.
pub const FEATURE_GW_TRADE_VENUE_READ: &str = "gw_trade_venue_read";

/// Gates the THIRD kind of that read, `VenueReadKind::Positions` on `TradeUp::VenueRead` (the positions the
/// venue says the account holds), and the `VenueRead::Positions` answer. A DECODE capability, and
/// ⚠ the one that protects the lease: `TradeUp::VenueRead` already exists on a daemon that advertises
/// `gw_trade_venue_read`, but that daemon cannot decode the new kind's name, and a trade frame the daemon
/// cannot decode ENDS the lease (the account's trading lease, not only the read). So a client asks for
/// positions only of a daemon that advertises THIS string, and says in words that it is too old otherwise.
/// A daemon that advertises it advertises `gw_trade_venue_read` too.
pub const FEATURE_GW_TRADE_VENUE_POSITIONS: &str = "gw_trade_venue_positions";

/// Gates `TradeDown::ReconReply::read`: the admitted symbols the daemon's reconcile client READ for the
/// answer, named by the daemon on page 0 of a list kind (decision 0174's reopener; the reply field is
/// additive, so an older node ignores the key). A node that sees the string stops trusting its own
/// mirror alone (`trade/told.rs`) for the venues whose client names a list: what the lease claims is
/// what it told AND the daemon names. Absent, the claim is the mirror's, as it always was.
pub const FEATURE_GW_TRADE_RECON_READ: &str = "gw_trade_recon_read";

/// The prefix of a per-venue TRADE entry - `trade_venue=binance` - one per venue whose adapter this
/// daemon build carries, so a client learns at the handshake which venues it may open a lease for.
/// A pair ([`trade_venue_feature`] / [`advertised_trade_venues`]) so the two ends cannot drift on the
/// spelling, like `md_venue=` and `cd_venue=`.
pub const FEATURE_TRADE_VENUE_PREFIX: &str = "trade_venue=";

/// Build the `trade_venue=<slug>` entry - the WRITE half of [`advertised_trade_venues`].
pub fn trade_venue_feature(slug: &str) -> String {
    format!("{FEATURE_TRADE_VENUE_PREFIX}{slug}")
}

/// Every trade venue slug a daemon advertised, in ADVERTISEMENT ORDER - the READ half of
/// [`trade_venue_feature`]. It reads ONE prefix, deliberately, like its two siblings.
pub fn advertised_trade_venues(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_TRADE_VENUE_PREFIX)
}

/// Every fixed capability string of the TRADE class, in the order a daemon that serves it appends them
/// to its data vocabulary ([`KNOWN_FEATURES`]); the `trade_venue=` entries are built by
/// [`trade_venue_feature`]. `every_fixed_feature_is_known` (in `proto_tests.rs`) holds the two lists
/// complete against this file.
pub const KNOWN_TRADE_FEATURES: [&str; 13] = [
    FEATURE_GW_TRADE,
    FEATURE_GW_TRADE_CURSOR,
    FEATURE_GW_TRADE_RESOLVE,
    FEATURE_GW_TRADE_WIRE_IDS,
    FEATURE_GW_LEASE,
    FEATURE_GW_ARMING,
    FEATURE_GW_ADAPTER,
    FEATURE_GW_PREFLIGHT,
    FEATURE_GW_TRADE_CONTRACT_VALUE,
    FEATURE_GW_TRADE_SYMBOL_FACTS,
    FEATURE_GW_TRADE_VENUE_READ,
    FEATURE_GW_TRADE_VENUE_POSITIONS,
    FEATURE_GW_TRADE_RECON_READ,
];
