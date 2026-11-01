//! `recording` — the RECORDING plane's venue table: per recordable venue, how a profile
//! subscription's family resolves to symbols, and whether its book already carries quotes.
//!
//! **Clients come from the broker; this table constructs none** (decision record 0092, one venue
//! client per process: `docs/decisions/0092-one-venue-client-per-process.md`). Each arm pairs a
//! resolver with a `crate::feeds::FeedHandle` over the process's ONE `crate::feeds::FeedBroker`, so
//! a key the market-data plane also holds is subscribed once. The REAL client behind the handle is
//! built by `crates/vike-datahub/src/feeds/venues.rs`'s `build_client` on the handle's first
//! subscribe, with the sink `crate::recorder::record` attached for `Holder::Rec` before any feed
//! here exists.
//!
//! **The resolvers are remote too** (decision 0164): this process links no bridge, so a family is
//! resolved by asking the gatewayhub, through the `GatewayLink` `build_recording_feed` is handed.
//! Building a feed still opens nothing and fails on nothing but a malformed family name: both
//! resolvers are lazy about the daemon, so a gatewayhub that is down when the profile loads is a
//! tick that resolves `Err` (the recorder keeps its streams and asks again), never a refused
//! profile or an exit.
//!
//! It replaced `vike_recorder::venues` on 2026-09-28. That module built the recorder its own venue
//! clients from its own table, beside the md plane's, which is why a `record` + `live-feeds` daemon
//! used to subscribe a shared key twice; with the table here, `vike-recorder` names no bridge at
//! all.
//!
//! # The shape: one `#[cfg]` arm AND one `#[cfg(not)]` arm per venue
//!
//! Kept from the table it replaced, including its two refusals: a venue this table knows but this
//! build did not compile is refused naming the `record-<venue>` feature to rebuild with; a venue
//! it does not know at all is refused naming what IS supported. Two different messages, because
//! "unknown venue" sends somebody hunting for a typo when the answer is a rebuild.
//!
//! # Why `record-<venue>` implies `venue-<venue>`
//!
//! A PLANE feature (`record-`, `live-`) says what the process SERVES; a VENUE feature says what it
//! ARMS (`crates/vike-datahub/Cargo.toml`: a marker, no dependency). The recording plane reaches a venue
//! through exactly the broker arm the md plane uses, so `record-binance` is `record` plus
//! `venue-binance` — and binance therefore arrives as the same REMOTE feed client the md plane builds,
//! the gatewayhub's feeds-only half, where the recorder's own manifest used to take the bridge with the
//! exec plane on.
//!
//! # Where "the book already carries quotes" lives
//!
//! The second argument of each arm's `AssembledFeed::new` — decision D1 of the plan behind 0092
//! chose a column of THIS table over a `vike_venues::venue_caps::LiveDataCaps` field, because this table is its
//! only consumer.
#[cfg(feature = "record-oanda")]
use std::collections::BTreeSet;
use std::sync::Arc;
#[cfg(feature = "record-binance")]
use std::sync::OnceLock;

#[cfg(feature = "record-binance")]
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
#[cfg(feature = "record-binance")]
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::client::GatewayLink;
#[cfg(feature = "record-binance")]
use vike_model::{AssetClass, Instrument};
use vike_recorder::assembled::AssembledFeed;
use vike_recorder::config::Subscription;

use crate::feeds::FeedBroker;

/// The venues this build can actually RECORD — and, since the `rec_venue=` advertisement, the
/// handshake's source of truth, so a client learns at CONNECT which venues it may name in a
/// recorder subscription.
///
/// `crates/vike-datahub/src/server/features.rs`'s `served_features` turns each slug into a
/// `vike_datahub_client::proto::rec_venue_feature` entry. That is the same arrangement
/// `crates/vike-datahub/src/md/venues.rs`'s own `supported` has with `md_venue=` — and the two are
/// deliberately SEPARATE advertisements, because the sets differ: the shipped image serves six
/// venues live and records two.
pub fn supported() -> Vec<&'static str> {
    vec![
        #[cfg(feature = "record-binance")]
        "binance",
        #[cfg(feature = "record-polymarket")]
        "polymarket",
    ]
}

/// The CREDENTIALED venues the recorder follows the strategy lane on in this build (decision 0193
/// verdict 7), OANDA so far: with `RecordArgs::strat_quotes` on, each gets one feed whose symbols are the
/// quote keys the strategy lane holds.
///
/// ⚠ **They are NOT in [`supported`], on purpose.** `supported` is the profile's venue list (and the
/// `rec_venue=` advertisement), and a venue in it must have a refining row in
/// `crates/vike-data/src/store/series_cadence.rs`'s `SERIES_CADENCE` (`series_cadence_gate`'s
/// `every_recordable_venue_lane_has_a_refining_row`): a table in `vike-data`, which is in the live core's
/// measured closure, so a row there fires the latency job. A venue the recorder only FOLLOWS needs no
/// profile row; naming OANDA symbols in a profile is the follow-up that adds that row.
pub fn strat_followed() -> Vec<&'static str> {
    vec![
        #[cfg(feature = "record-oanda")]
        "oanda",
    ]
}

/// The venues whose market CLOSES (FX, an equity session) that this build can record: their series are
/// outside the recorder's silence watchdog's expected set (decision 0193 verdict 10). A held key on a closed
/// market speaks nothing, and an alert for every weekend would teach the operator to ignore the watchdog;
/// their liveness is the stream's own `Stale` and gap words, the link dead-man's, and the Gateway screen's.
/// A function and not a `const`, because a const naming venues is a table `venue_subset_tables_gate` pins.
pub fn session_bounded() -> Vec<&'static str> {
    vec![
        #[cfg(feature = "record-oanda")]
        "oanda",
    ]
}

/// The feed of a venue [`strat_followed`] names: nothing of its own to record, the strategy lane's quote
/// keys to follow. The recorder mounts it only once the strategy lane holds a quote key of the venue
/// (`crate::recorder::record`): a feed that answers an empty symbol set from startup is the "venue NEVER
/// resolved a symbol" alert, and an empty set is this feed's ordinary state on a box whose strategies trade
/// nothing there.
///
/// `Err` for a venue [`strat_followed`] does not name (a caller's mistake, said in words).
#[cfg_attr(not(feature = "record-oanda"), allow(unused_variables))]
pub fn build_strat_followed_feed(
    venue: &str,
    broker: &Arc<FeedBroker>,
) -> Result<AssembledFeed, String> {
    match venue {
        #[cfg(feature = "record-oanda")]
        "oanda" => Ok(oanda_feed(broker)),
        other => Err(format!(
            "recorder: no strategy-following feed for venue `{other}` in this build. Followed: [{}]",
            strat_followed().join(", ")
        )),
    }
}

/// One profile subscription as a recorder feed over the broker.
///
/// `Err` when this build cannot record that venue — the message names the Cargo feature to rebuild
/// with. A recorder that silently skipped an unsupported venue would look healthy while recording
/// nothing, which is the failure `vike_recorder::config::RecorderProfile`'s validation already
/// exists to prevent at the profile level; the same rule applies to the build.
///
/// Building a feed opens nothing: the handle reaches the broker only on its first subscribe, and
/// the resolver reaches the gatewayhub (`link`) only when it resolves.
// `broker` and `link` are consumed by whichever venue arm this build compiled; a build with NO venue
// feature (`record` alone, the mount with no venue in it) has no such arm, and the signature stays
// the same either way.
#[cfg_attr(
    not(any(feature = "record-binance", feature = "record-polymarket")),
    allow(unused_variables)
)]
pub fn build_recording_feed(
    sub: &Subscription,
    broker: &Arc<FeedBroker>,
    link: &GatewayLink,
) -> Result<AssembledFeed, String> {
    match sub.venue.as_str() {
        #[cfg(feature = "record-binance")]
        "binance" => binance_feed(sub, broker, link),
        #[cfg(not(feature = "record-binance"))]
        "binance" => Err(missing_feature("binance", "record-binance")),
        #[cfg(feature = "record-polymarket")]
        "polymarket" => polymarket_feed(sub, broker, link),
        #[cfg(not(feature = "record-polymarket"))]
        "polymarket" => Err(missing_feature("polymarket", "record-polymarket")),
        other => Err(format!(
            "recorder: venue `{other}` has no feed in this build. Supported: [{}]",
            supported().join(", ")
        )),
    }
}

/// A family resolves through the venue's own resolver; an explicit symbol list is itself.
#[cfg(any(feature = "record-binance", feature = "record-polymarket"))]
fn resolver(
    sub: &Subscription,
    family: impl FnOnce(&str) -> Result<Box<dyn vike_data::live::SymbolResolver>, String>,
) -> Result<Box<dyn vike_data::live::SymbolResolver>, String> {
    match &sub.family {
        Some(f) => family(f),
        None => Ok(Box::new(vike_data::live::FixedSymbols::new(sub.symbols.iter().cloned()))),
    }
}

/// The Binance row — the STATIC-family case, and the proof that "families are general" (spec §8.2
/// of `docs/superpowers/specs/2026-08-02-live-recorder-design.md`) is not a Polymarket-shaped
/// claim.
///
/// Polymarket resolves a family by asking Gamma what is live right now, because its token ids rotate
/// every 5 minutes. Binance resolves the same key as a **filter over an instrument list that does not
/// change from tick to tick** — a glob (`*USDT`, `BTC*`, `*USDT.P`) matched against
/// binance's `list_instruments`, through the venue-free `vike_recorder::resolve::CatalogGlob`. The
/// listing is the gatewayhub's (`vike_gatewayhub_client::catalog::RemoteCatalog` over `link`; it
/// was the binance bridge's `BinanceCatalog` while this process linked the bridge), opened on the
/// first listing and not before ([`LazyCatalog`]). One trait, rotation as the general case, static
/// as the degenerate one.
///
/// ## ⚠ What this can actually record: TRADES and conflating DEPTH. Not quotes, not book.
///
/// Binance's declared capabilities are `LiveDataCaps { bars, trades, depth }` — **`quotes: false`,
/// `book: false`** (`vike_venues::venue_caps`), and its `DataClient` refuses both verbs accordingly.
/// What it does serve is `subscribe_depth`, the DOM's **conflating** L2 lane, which emits through
/// `vike_data::LiveDataSink::l2_snapshot` — and `RecorderSink` PERSISTS that verb, as its own
/// `kind=depth` series.
///
/// ⚠ **That last clause used to read "…and `RecorderSink` does not implement that verb, so the
/// trait's default no-op swallows it", and it stopped being true in #995** — the commit that added
/// the impl (`crates/vike-store/src/live_rec.rs`'s `RecorderSink`, gated by
/// `crates/vike-store/tests/store/depth_and_budget.rs`) edited this very paragraph without editing that
/// sentence. It is called out rather than quietly replaced because the stale sentence went on to be
/// used as EVIDENCE: a later change reasoned "so a depth socket is never opened by this daemon" from
/// it, and left `crates/vike-bridge-core/src/depth.rs`'s `connect_depth` dialing unbounded inside the
/// recorder's own teardown budget. A wrong doc is load-bearing right up until somebody trusts it.
/// (This paragraph was the module doc of `vike-recorder`'s binance venue module until decision 0092
/// moved the recording table here; it moved with the row it describes.)
///
/// So a Binance recording is the **trade tape plus a conflated depth series**, and that is worth
/// stating plainly because it interacts with `vike_backfill::caps`: no venue-direct backfill serves
/// `trade` or `book` either. Combined:
///
/// | binance | backfill | record live |
/// |---|---|---|
/// | `bar` | ✓ (klines) | — (derived; the recorder does not store bars) |
/// | `trade` | ✗ | **✓ — this row is the only path** |
/// | `quote` | ✗ | ✗ (venue serves none) |
/// | `depth` | ✗ | **✓ — the conflating lane, under its own kind** |
/// | `book` | ✗ (archive rights-blocked) | ✗ (venue serves no lossless lane) |
///
/// ⚠ **And for forty days the depth column above was true in NAME only.** From 2026-08-01 to
/// 2026-09-10 the `kind=depth/venue=binance/symbol=BTCUSDT.P` series recorded **0.41–0.43
/// updates/s** against the ~10/s a `@depth@100ms` stream carries — the shared decoder
/// (`crates/bridges/binance/src/family/depth.rs`'s `apply_depth_event`) enforced binance's SPOT
/// contiguity rule on a USDⓈ-M FUTURES stream, so the second diff of every session read as a gap
/// and the driver re-seeded on its 3 s backoff forever. Two rows per ~4.3 s cycle, both full
/// snapshots, nothing incremental. The rows that ARE there are honest snapshots; roughly 32 million
/// book states between them are not recoverable — `crates/vike-backfill/src/caps.rs`'s
/// `PLANNABLE_KINDS` omits depth and no vendor sells binance L2, so this kind can only ever be
/// re-recorded LIVE. **The catalog surfaces still advertise that range as complete and will keep
/// doing so**: coverage is computed from rows present, and rows were present.
///
/// **A LOSSLESS Binance L2 book is still not obtainable in this workspace by any means**, and
/// `kind=depth` is deliberately not a rename of one: a full snapshot every 100 ms with every
/// intermediate state discarded is not a lossless book, and writing it as `kind=book` would let a
/// maker-fill backtest run on it and report fills it could never have got. THE PATH IS THE
/// DISCLOSURE.
///
/// No special-casing was needed for any of this. `vike_recorder::session::SubscriptionSet` asks for
/// each stream once, learns `Unsupported` as a permanent capability answer, and never asks again —
/// so a Binance feed subscribes trades and depth, is told twice that quotes and book do not exist,
/// and records the two lanes it got.
///
/// `book_carries_quotes = false`: binance serves no book lane, so its book never carries quotes.
#[cfg(feature = "record-binance")]
fn binance_feed(
    sub: &Subscription,
    broker: &Arc<FeedBroker>,
    link: &GatewayLink,
) -> Result<AssembledFeed, String> {
    let symbols = resolver(sub, |family| {
        Ok(Box::new(vike_recorder::resolve::CatalogGlob::new(
            family,
            Box::new(LazyCatalog::new(link.clone(), "binance")),
        )))
    })?;
    let client = Box::new(broker.handle(crate::feeds::Holder::Rec, "binance"));
    Ok(AssembledFeed::new("binance", false, symbols, client))
}

/// One venue's catalog through the gatewayhub, OPENED ON THE FIRST LISTING.
///
/// `vike_gatewayhub_client::catalog::RemoteCatalog::open` dials and asks the daemon to describe the
/// venue, and it can fail. Building it when the feed is built would turn a gatewayhub that is down
/// at startup into a refused profile: `crate::recorder::record` returns the error and the process
/// exits, under `Restart=on-failure` a crash loop (drift register R17). The provider this replaced
/// was a unit struct that opened nothing, and its listing failed on the TICK, where
/// `CatalogGlob::desired` turns the failure into an `Err` (never an empty set, which would
/// unsubscribe every live stream) and asks again next tick. This keeps that shape: the open happens
/// inside `list_instruments`, a failure leaves the cell empty, and the next listing opens afresh.
///
/// `vike_recorder::resolve::CatalogGlob` reads only [`CatalogProvider::venue`] and
/// [`CatalogProvider::list_instruments`]. The other two answer from the opened catalog once there is
/// one, and say what a glob needs (an enumerable listing, no asset-class claim) before it.
#[cfg(feature = "record-binance")]
struct LazyCatalog {
    link: GatewayLink,
    venue: &'static str,
    opened: OnceLock<RemoteCatalog>,
}

#[cfg(feature = "record-binance")]
impl LazyCatalog {
    fn new(link: GatewayLink, venue: &'static str) -> Self {
        Self { link, venue, opened: OnceLock::new() }
    }

    /// The opened catalog, dialling the daemon if it is not open yet. Two racing opens each ask the
    /// daemon; the second answer is dropped.
    fn catalog(&self) -> Result<&RemoteCatalog, CatalogError> {
        if let Some(catalog) = self.opened.get() {
            return Ok(catalog);
        }
        let catalog = RemoteCatalog::open(self.link.clone(), self.venue)?;
        Ok(self.opened.get_or_init(|| catalog))
    }
}

#[cfg(feature = "record-binance")]
impl CatalogProvider for LazyCatalog {
    fn venue(&self) -> &str {
        self.venue
    }

    fn asset_classes(&self) -> &[AssetClass] {
        self.opened.get().map_or(&[] as &[AssetClass], |c| c.asset_classes())
    }

    fn mode(&self) -> CatalogMode {
        self.opened.get().map_or(CatalogMode::Enumerable, |c| c.mode())
    }

    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        self.catalog()?.list_instruments()
    }
}

/// The Polymarket row — the rotating-family case, and the reason families exist at all.
///
/// A Polymarket up/down market lives **5 minutes**. Each window is a different market with a
/// different `condition_id` and different outcome token ids, so a customer cannot subscribe by token
/// id: the ids they picked would be dead before the next flush. They name the FAMILY, and the
/// gatewayhub's polymarket `RollingFamily` (`crates/bridges/polymarket/src/rolling_family.rs`) resolves it to live tokens on every
/// tick — over the real Gamma directory, or an explicit token list — asked through
/// `vike_gatewayhub_client::resolve::RemoteRollingFamily`. Neither the broker's handle nor the
/// resolver opens a socket when the feed is built, so building cannot fail on the network; it fails
/// only on a family name with no `-<N>m`, which the resolver refuses OFFLINE with the bridge's own
/// words (`vike_catalog::polymarket_family_minutes`), gatewayhub up or not.
///
/// **`Quotes` is dropped when `Book` is requested** (`crates/vike-recorder/src/assembled.rs`'s
/// `narrow`, armed by the `true` this row passes as `book_carries_quotes`). On this venue
/// `subscribe_book` already emits derived L1 (`PumpMode::Book` → `sink.book()` **and**
/// `sink.quote()`, per `crates/bridges/polymarket/src/market_feed.rs`'s module doc), so
/// subscribing both opens a second socket deriving the same L1 from its own copy of the book and
/// writes every quote row twice.
///
/// ⚠ Polymarket is `crate::feeds::Sharing::PerHolder` (`crates/vike-datahub/src/feeds/venues.rs`'s
/// `EXPLICIT_SHARING`): its `unsubscribe` resubscribes a shard's co-tenant tokens, so the recorder
/// gets its own client, built with the recorder's own sink, and an md release can never punch a
/// gap into a recorded book.
#[cfg(feature = "record-polymarket")]
fn polymarket_feed(
    sub: &Subscription,
    broker: &Arc<FeedBroker>,
    link: &GatewayLink,
) -> Result<AssembledFeed, String> {
    let symbols = resolver(sub, |family| {
        Ok(Box::new(vike_gatewayhub_client::resolve::RemoteRollingFamily::new(
            link.clone(),
            family,
        )?))
    })?;
    let client = Box::new(broker.handle(crate::feeds::Holder::Rec, "polymarket"));
    Ok(AssembledFeed::new("polymarket", true, symbols, client))
}

/// The OANDA feed — a CREDENTIALED venue recorded by FOLLOWING THE STRATEGY LANE (decision 0193
/// verdict 7), the first of its kind.
///
/// The recorder holds no credential and opens no login: its handle is `Holder::Rec` on the broker's
/// one OANDA client, which the strategy lane's subscriptions opened (`Sharing::Shared`), so a quote key a
/// strategy holds is recorded by joining it, with no second `/pricing/stream`. The symbols are every quote
/// key the strategy lane holds right now, re-resolved each tick ([`HeldByStrategies`]).
///
/// ## What this can record: QUOTES. Nothing else exists.
///
/// OANDA declares `quotes` and `bars` (`vike_venues::venue_caps`) and refuses trades, book and depth, so
/// the feed asks for quotes alone (`AssembledFeed::quotes_only`). Bars are never recorded (derived). So
/// the series is `kind=quote/venue=oanda/symbol=<the strategy's spelling>`.
///
/// ## ⚠ A held key on a closed market records nothing
///
/// FX closes from Friday evening to Sunday evening: a held key then speaks no quotes and its
/// recorded series falls silent. That is accurate, not a fault, so `crate::recorder::record` leaves
/// [`session_bounded`] venues' series out of the silence watchdog's expected set (decision 0193 verdict 10).
#[cfg(feature = "record-oanda")]
fn oanda_feed(broker: &Arc<FeedBroker>) -> AssembledFeed {
    let symbols = Box::new(HeldByStrategies::new(Arc::clone(broker), "oanda"));
    let client = Box::new(broker.handle(crate::feeds::Holder::Rec, "oanda"));
    // Quotes only: the venue serves nothing else, and the broker checks its stream ceiling BEFORE the bridge's
    // capability refusal, so at the ceiling the recorder's probes of the other streams would be answered with the
    // cap refusal and logged three times per held key on every tick (`AssembledFeed::quotes_only`).
    AssembledFeed::new("oanda", false, symbols, client).quotes_only()
}

/// The "held by strategies" resolver (decision 0193 verdict 7, R6): the symbols the recorder wants are
/// every quote key the strategy lane holds on the venue at this instant. Never `Err`: the key set is this
/// process's own memory, not a question to the gatewayhub, so there is no unknown to protect a live
/// stream from.
///
/// ⚠ It reads ONLY `Holder::Strat`'s key set (`FeedBroker::strat_held`), never the broker's slots, so the
/// recorder's own hold of a key cannot keep that key looking strategy-held: a key nobody but the recorder
/// holds leaves, and its stream closes with it.
///
/// ⚠ **A key the strategy lane let go of stays wanted for [`RELEASE_LINGER_MS`]** (about two recorder
/// ticks). A tradehub that restarts lets every key go at once and takes the same keys a few seconds later;
/// were the recorder to release each of them on its next tick, the gatewayhub would join every stream
/// (up to a heartbeat each, one after another) just as the returning tradehub subscribes them again, and
/// the recorder would write a hole where the strategies never lost a price. Held through the gap, the
/// stream is never closed and the returning strategy simply joins it. The price is the key counting
/// against the venue's stream ceiling for that long.
#[cfg(feature = "record-oanda")]
struct HeldByStrategies {
    broker: Arc<FeedBroker>,
    venue: &'static str,
    /// The instant each key was last seen held by the strategy lane.
    last_held: std::collections::BTreeMap<String, i64>,
}

/// How long a released key stays in the recorder's wanted set: two ticks of the recorder's default thirty
/// seconds. Time and not a count of ticks, so a retuned tick does not change what a restart costs.
#[cfg(feature = "record-oanda")]
const RELEASE_LINGER_MS: i64 = 60_000;

#[cfg(feature = "record-oanda")]
impl HeldByStrategies {
    fn new(broker: Arc<FeedBroker>, venue: &'static str) -> Self {
        Self { broker, venue, last_held: std::collections::BTreeMap::new() }
    }
}

#[cfg(feature = "record-oanda")]
impl vike_data::live::SymbolResolver for HeldByStrategies {
    fn group(&self) -> Option<&str> {
        None
    }

    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        for symbol in self.broker.strat_held(self.venue, vike_data::StreamLabel::Quotes) {
            self.last_held.insert(symbol, now_ms);
        }
        self.last_held.retain(|_, seen| now_ms.saturating_sub(*seen) < RELEASE_LINGER_MS);
        Ok(self.last_held.keys().cloned().collect())
    }
}

#[cfg(not(all(feature = "record-binance", feature = "record-polymarket")))]
fn missing_feature(venue: &str, feature: &str) -> String {
    format!(
        "recorder: venue `{venue}` is not compiled into this build — rebuild with \
         `--features {feature}`. (This build's features do not arm the venue: the feature arms a remote arm and links no \
         bridge, because the gatewayhub serves the venues.)"
    )
}

// ⚠ INLINE, deliberately, where the plan drew a `recording_tests.rs` beside this file — the
// `crates/vike-datahub/src/feeds/venues.rs` precedent: the code-layout rule moves a test block out
// only for a file >= 400 lines carrying >= 150 test lines, and this one is neither.
//
// These are `vike-recorder`'s former `venues` tests, moved with the table. They run only where
// this module compiles — a `record*` build, i.e. `ci.yml`'s `datahub-all` job, whose
// `-p vike-datahub` build turns on every shipped feature, `record-polymarket` and `record-binance`
// among them.
#[cfg(test)]
mod tests {
    use super::*;

    fn sub(venue: &str, family: Option<&str>) -> Subscription {
        Subscription {
            venue: venue.into(),
            family: family.map(str::to_string),
            symbols: Vec::new(),
        }
    }

    /// A link to nothing: a port nobody listens on and no key. The feeds built over it are the
    /// gatewayhub being down, which building one must survive.
    fn unreachable_link() -> GatewayLink {
        GatewayLink::new("127.0.0.1:1", vike_hub_proto::auth::HubKeys::new(Vec::new(), Vec::new()))
    }

    /// The broker on the real client table over `link` — the production wiring, minus the process.
    fn broker_over(link: &GatewayLink) -> Arc<FeedBroker> {
        FeedBroker::new(
            crate::feeds::venues::real_client_table(link),
            crate::feeds::venues::sharing_for,
        )
    }

    /// One feed built the way `crate::recorder::record` builds it, over a gatewayhub that is down.
    fn feed(sub: &Subscription) -> Result<AssembledFeed, String> {
        let link = unreachable_link();
        build_recording_feed(sub, &broker_over(&link), &link)
    }

    /// `build_recording_feed` returns an `AssembledFeed`, which has no `Debug`, so `unwrap_err`
    /// cannot be used on it.
    fn err_of(r: Result<AssembledFeed, String>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    /// An unknown venue must FAIL at startup, not be skipped. A recorder that skipped it would
    /// report itself healthy while recording nothing for that venue — the exact silent-nothing
    /// failure `RecorderProfile::from_toml`'s validation exists to prevent one layer up.
    #[test]
    fn an_unsupported_venue_is_a_startup_error_naming_what_is_supported() {
        let err = err_of(feed(&sub("kalshi", Some("x-5m"))));
        assert!(err.contains("kalshi"), "{err}");
        assert!(err.contains("Supported"), "{err}");
    }

    /// A venue that EXISTS but was compiled out gets a different, actionable message: the feature to
    /// rebuild with, rather than "unknown venue", which would send someone hunting for a typo.
    #[cfg(not(feature = "record-polymarket"))]
    #[test]
    fn a_compiled_out_venue_names_the_feature() {
        let err = err_of(feed(&sub("polymarket", Some("btc-updown-5m"))));
        assert!(err.contains("--features record-polymarket"), "{err}");
    }

    /// The polymarket row: a rolling family, and a book that carries quotes.
    #[cfg(feature = "record-polymarket")]
    #[test]
    fn the_polymarket_row_rolls_and_its_book_carries_quotes() {
        use vike_recorder::{runtime::VenueFeed, session::Stream};

        let f = feed(&sub("polymarket", Some("btc-updown-5m"))).unwrap();
        assert_eq!(f.venue(), "polymarket");
        assert_eq!(f.family(), Some("btc-updown-5m"));
        assert!(
            !f.narrow(&Stream::ALL).contains(&Stream::Quotes),
            "the book pump derives L1, so Quotes beside Book would write every quote row twice"
        );
        // CONTAINS, not equals: `supported()` grows with each venue feature, and this build may have
        // several on. An equality assertion here made adding the second venue fail a test about the
        // first one.
        assert!(supported().contains(&"polymarket"), "{:?}", supported());
    }

    /// The binance row — the STATIC-family venue, built through the same dispatch.
    ///
    /// ⚠ **The `family()` assertion INVERTED on 2026-09-19 and that is the point of the test now.**
    /// It read `Some("*USDT.P")` — the operator's glob, verbatim, straight out to the store as a
    /// `group=*USDT.P` directory. `*` may not be in a directory name on Windows, so that series
    /// could be written on Linux and never opened. The glob stays where the MATCHING happens
    /// (`vike_recorder::resolve::CatalogGlob`'s `pattern`, which this test cannot reach and
    /// `crates/vike-recorder/src/resolve_tests.rs` covers); what `family()` answers is the GROUP
    /// NAME, and a group name has to be a directory.
    #[cfg(feature = "record-binance")]
    #[test]
    fn the_binance_row_is_a_static_family_with_a_path_safe_group() {
        use vike_recorder::{runtime::VenueFeed, session::Stream};

        let f = feed(&sub("binance", Some("*USDT.P"))).unwrap();
        assert_eq!(f.venue(), "binance");
        assert_eq!(
            f.family(),
            Some("USDT.P"),
            "the group NAME is rendered path-safe; the glob is kept on the match pattern"
        );
        assert_eq!(
            f.narrow(&Stream::ALL),
            Stream::ALL.to_vec(),
            "binance's book carries no quotes"
        );
        assert!(supported().contains(&"binance"), "{:?}", supported());
    }

    /// Every venue this build claims it can RECORD must be a real `vike_model::VENUES` slug —
    /// otherwise the `rec_venue=` advertisement names something no operator could ever write into a
    /// subscription row, and a CLI refusing an unadvertised venue would refuse the correct spelling
    /// while accepting nothing.
    ///
    /// The exact twin of `crates/vike-datahub/src/md/venues.rs`'s
    /// `every_supported_venue_is_a_roster_slug`, which holds the same property for the LIVE plane's
    /// `md_venue=` entries. Two advertisements, two rosters, one rule — and the rule has to be
    /// stated on each side, because neither `supported()` can see the other.
    ///
    /// ⚠ It is VACUOUS on a `record` build with no venue feature, exactly as the md twin is on a
    /// build with no `live-<venue>`. `ci.yml`'s `datahub-all` job (every shipped feature, both
    /// `record-*` among them) is where it has something to assert.
    #[test]
    fn every_supported_venue_is_a_roster_slug() {
        for v in supported() {
            assert!(
                vike_model::VENUES.contains(&v),
                "`{v}` is not in vike_model::VENUES, so the `rec_venue={v}` this build advertises \
                 names a venue no subscription row could legally carry"
            );
        }
    }

    /// A profile naming BOTH venues is the real multi-venue case, and the one whose `#[cfg]` shape
    /// differs from either single-venue build.
    #[cfg(all(feature = "record-binance", feature = "record-polymarket"))]
    #[test]
    fn both_venues_dispatch_in_one_build() {
        use vike_recorder::runtime::VenueFeed;

        assert_eq!(supported(), vec!["binance", "polymarket"]);
        let link = unreachable_link();
        let b = broker_over(&link);
        assert_eq!(
            build_recording_feed(&sub("polymarket", Some("btc-updown-5m")), &b, &link)
                .unwrap()
                .venue(),
            "polymarket"
        );
        assert_eq!(
            build_recording_feed(&sub("binance", Some("*USDT")), &b, &link).unwrap().venue(),
            "binance"
        );
    }

    /// **A gatewayhub that is down is a tick that resolves `Err`, never an empty set and never a
    /// refused profile** (decision 0164, drift register R12 and R17). The feed BUILDS over a link to
    /// nothing, and its resolver answers `Err`: an empty `Ok` would tell the recorder "nothing
    /// should be streaming" and unsubscribe every live key because the daemon restarted.
    #[cfg(any(feature = "record-binance", feature = "record-polymarket"))]
    #[test]
    fn a_down_gatewayhub_is_an_error_at_resolve_time_never_an_empty_set() {
        use vike_recorder::runtime::VenueFeed;

        let mut asked = Vec::new();
        #[cfg(feature = "record-binance")]
        asked.push(sub("binance", Some("*USDT")));
        #[cfg(feature = "record-polymarket")]
        asked.push(sub("polymarket", Some("btc-updown-5m")));
        for s in asked {
            let mut f =
                feed(&s).unwrap_or_else(|e| panic!("`{}` must build offline: {e}", s.venue));
            let err = f.desired(1_700_000_000_000).expect_err("a dead link is unknown, not empty");
            assert!(err.contains("gatewayhub") || err.contains("instrument list"), "{err}");
        }
    }

    /// A malformed polymarket family is refused when the feed is BUILT, offline, with the bridge's
    /// own words: the recorder's startup refusal does not depend on the gatewayhub being up.
    #[cfg(feature = "record-polymarket")]
    #[test]
    fn a_family_without_an_interval_is_refused_at_build_with_the_gatewayhub_down() {
        let err = err_of(feed(&sub("polymarket", Some("btc-updown"))));
        assert_eq!(err, vike_catalog::polymarket_family_minutes("btc-updown").unwrap_err());
        assert!(err.contains("does not end in an interval like `-5m`"), "{err}");
    }

    /// The binance glob is matched against the listing the GATEWAYHUB serves, over the real wire:
    /// a scripted daemon lists three instruments, `*USDT` keeps the two that end that way, and the
    /// daemon was asked to describe `binance` and to list it.
    #[cfg(feature = "record-binance")]
    #[test]
    fn the_binance_glob_is_matched_against_the_gatewayhubs_listing() {
        use vike_gatewayhub::testkit::data::ScriptedData;
        use vike_gatewayhub::testkit::gateway::LoopbackGateway;
        use vike_gatewayhub::testkit::rest::{RestCall, ScriptedRest};
        use vike_gatewayhub_client::proto::WireCatalogMode;
        use vike_model::{AssetClass, Contract, Instrument};
        use vike_recorder::runtime::VenueFeed;

        let listing: Vec<Instrument> = ["BTCUSDT", "ETHUSDT", "BTCEUR"]
            .iter()
            .map(|raw| Instrument::new("binance", *raw, "BASE", "QUOTE", Contract::CryptoSpot))
            .collect();
        // Script the REST lane BEFORE the gateway starts: the server reads its facts once.
        let rest = ScriptedRest::new().with_catalog(
            "binance",
            vec![AssetClass::CryptoSpot],
            WireCatalogMode::Enumerable,
            listing,
        );
        let gateway = LoopbackGateway::start(ScriptedData::new(&["binance"]), rest.clone());
        let link = gateway.link();

        let mut f =
            build_recording_feed(&sub("binance", Some("*USDT")), &broker_over(&link), &link)
                .unwrap();
        let wanted = f.desired(1_700_000_000_000).expect("the scripted daemon lists binance");
        assert_eq!(
            wanted.into_iter().collect::<Vec<_>>(),
            vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]
        );
        let calls = rest.calls();
        assert!(
            calls.iter().any(|c| matches!(c, RestCall::Fetch(_))),
            "the listing is a fetch at the daemon: {calls:?}"
        );
    }
}
