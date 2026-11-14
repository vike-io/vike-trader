//! `recording` — the RECORDING plane's venue table: per recordable venue, how a profile
//! subscription's family resolves to symbols, and whether its book already carries quotes.
//!
//! **Clients come from the broker; this table constructs none** (decision record 0092, one venue
//! client per process: `docs/decisions/0092-one-venue-client-per-process.md`). Each arm pairs a
//! resolver with a `crate::feeds::DataClientBrokerHandle` over the process's ONE `crate::feeds::FeedBroker`, so
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
//! clients from its own table, beside the market data plane's, which is why a daemon with both planes
//! used to subscribe a shared key twice; with the table here, `vike-recorder` names no bridge at
//! all.
//!
//! A venue it does not know is refused naming what IS supported.
//!
//! # Where "the book already carries quotes" lives
//!
//! The second argument of each arm's `AssembledFeed::new` — decision D1 of the plan behind 0092
//! chose a column of THIS table over a `vike_venues::venue_caps::DataClientCapabilities` field, because this table is its
//! only consumer.
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::OnceLock;

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_gatewayhub_client::catalog::CatalogProviderGateway;
use vike_gatewayhub_client::client::GatewayLink;
use vike_model::{AssetClass, Instrument};
use vike_recorder::assembled::AssembledFeed;
use vike_recorder::config::Subscription;

use crate::feeds::{DataClientBrokerSharing, FeedBroker};

/// The venues this build can actually RECORD — and, since the `rec_venue=` advertisement, the
/// handshake's source of truth, so a client learns at CONNECT which venues it may name in a
/// recorder subscription.
///
/// `crates/vike-datahub/src/server/features.rs`'s `served_features` turns each slug into a
/// `vike_datahub_client::proto::rec_venue_feature` entry. That is the same arrangement
/// `crates/vike-datahub/src/market_data/venues.rs`'s own `supported` has with `market_data_venue=` — and the two are
/// deliberately SEPARATE advertisements, because the sets differ: the shipped image serves six
/// venues live and records two.
pub fn supported() -> Vec<&'static str> {
    vec!["binance", "polymarket"]
}

/// The CREDENTIALED venues the recorder follows the strategy lane on (decision 0193
/// verdict 7): with `RecorderArgs::strategy_quotes` on, each gets one feed whose symbols are the quote keys the
/// strategy lane holds.
///
/// **A venue is followed on one fact and nothing else is written for it:**
/// `crates/vike-datahub/src/feeds/venues.rs`'s `EXPLICIT_SHARING` gives it a `Shared` row. So that row is what
/// arms the venue: without it the recorder's client would be a second login at the venue
/// (`DataClientBrokerSharing::PerHolder`), so the venue is simply not followed. A function and not a `const`, because a const
/// naming venues is a table `venue_subset_tables_gate` pins.
///
/// ⚠ **They are NOT in [`supported`], on purpose.** `supported` is the profile's venue list (and the
/// `rec_venue=` advertisement), and a venue in it must have a refining row in
/// `crates/vike-data/src/store/series_cadence.rs`'s `SERIES_CADENCE` (`series_cadence_gate`'s
/// `every_recordable_venue_lane_has_a_refining_row`): a table in `vike-data`, which is in the live core's
/// measured closure, so a row there fires the latency job. A venue the recorder only FOLLOWS needs no
/// profile row; naming such a venue's symbols in a profile is the follow-up that adds that row.
pub fn strategy_followed() -> Vec<&'static str> {
    only_shared(
        vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES.to_vec(),
        crate::feeds::venues::sharing_for,
    )
}

/// The `venues` the broker gives one client to all holders: the ones a recorder can join without a login.
fn only_shared(
    mut venues: Vec<&'static str>,
    sharing: fn(&str) -> DataClientBrokerSharing,
) -> Vec<&'static str> {
    venues.retain(|venue| sharing(venue) == DataClientBrokerSharing::Shared);
    venues
}

/// The venues whose market CLOSES that the recorder can record: every venue the recorder follows (FX, CFDs, an
/// equity session). Their series are outside the recorder's silence watchdog's expected set (decision 0193
/// verdict 10). A held key on a closed market speaks nothing, and an alert for every weekend would teach the
/// operator to ignore the watchdog; their liveness is the stream's own `Stale` and gap words, the link
/// dead-man's, and the Gateway screen's.
pub fn session_bounded() -> Vec<&'static str> {
    strategy_followed()
}

/// The `source=` lane of the store a venue's recorded quotes go to, for a venue whose sourceless leaf holds an
/// archive (decision 0193's "Built: Dukascopy's live series is a lane of its own"): a tape in that leaf would
/// corrupt it. The lane is stored and listed, and only a read that names it reads it. A function and not a
/// `const`, because a const naming venues is a table `venue_subset_tables_gate` pins.
pub fn store_lane(venue: &str) -> Option<&'static str> {
    matches!(venue, "dukascopy").then_some("recorder")
}

/// The feed of a venue [`strategy_followed`] names: nothing of its own to record, the strategy lane's quote
/// keys to follow (decision 0193 verdict 7). The recorder mounts it only once the strategy lane holds a
/// quote key of the venue (`crate::recorder::record`): a feed that answers an empty symbol set from startup
/// is the "venue NEVER resolved a symbol" alert, and an empty set is this feed's ordinary state on a box
/// whose strategies trade nothing there.
///
/// The recorder holds no credential and opens no login: its handle is `Holder::Rec` on the broker's one
/// client of the venue, which the strategy lane's subscriptions opened (`DataClientBrokerSharing::Shared`), so a quote key a
/// strategy holds is recorded by joining it, with no second stream at the venue. The symbols are every quote
/// key the strategy lane holds right now, re-resolved each tick ([`HeldByStrategies`]), so the series is
/// `kind=quote/venue=<venue>/symbol=<the strategy's spelling>`, with [`store_lane`]'s `source=` when it names one.
///
/// ⚠ **A held key on a closed market records nothing.** That is accurate, not a fault, so
/// `crate::recorder::record` leaves [`session_bounded`] venues' series out of the silence watchdog's expected
/// set. **A stream labelled delayed or frozen records nothing either**: `crate::feeds::routing`'s
/// `DataClientSinkRouting` withholds it from the recorder, and only a `Shared` venue's client delivers through that
/// sink, which is one more reason [`strategy_followed`] names no other.
///
/// `Err` for a venue [`strategy_followed`] does not name (a caller's mistake, said in words).
pub fn build_strategy_followed_feed(
    venue: &str,
    broker: &Arc<FeedBroker>,
) -> Result<AssembledFeed, String> {
    let followed = strategy_followed();
    let Some(venue) = followed.iter().copied().find(|v| *v == venue) else {
        return Err(format!(
            "recorder: no strategy-following feed for venue `{venue}` in this build. Followed: [{}]",
            followed.join(", ")
        ));
    };
    let symbols = Box::new(HeldByStrategies::new(Arc::clone(broker), venue));
    let client = Box::new(broker.handle(crate::feeds::DataClientBrokerHolder::Rec, venue));
    // Quotes only (verdict 7; books and depth stay profile-only), and the feed ASKS for nothing else: the broker
    // checks its stream ceiling BEFORE the bridge's capability refusal, so at the ceiling the recorder's probes
    // of the other streams would be answered with the cap refusal and logged three times per held key on every
    // tick (`AssembledFeed::quotes_only`).
    Ok(AssembledFeed::new(venue, false, symbols, client).quotes_only())
}

/// One profile subscription as a recorder feed over the broker.
///
/// `Err` when the table does not name that venue. A recorder that silently skipped an unsupported
/// venue would look healthy while recording nothing, which is the failure
/// `vike_recorder::config::RecorderProfile`'s validation already exists to prevent at the profile
/// level; the same rule applies here.
///
/// Building a feed opens nothing: the handle reaches the broker only on its first subscribe, and
/// the resolver reaches the gatewayhub (`link`) only when it resolves.
pub fn build_recording_feed(
    sub: &Subscription,
    broker: &Arc<FeedBroker>,
    link: &GatewayLink,
) -> Result<AssembledFeed, String> {
    match sub.venue.as_str() {
        "binance" => binance_feed(sub, broker, link),
        "polymarket" => polymarket_feed(sub, broker, link),
        other => Err(format!(
            "recorder: venue `{other}` has no feed in this build. Supported: [{}]",
            supported().join(", ")
        )),
    }
}

/// A family resolves through the venue's own resolver; an explicit symbol list is itself.
fn resolver(
    sub: &Subscription,
    family: impl FnOnce(&str) -> Result<Box<dyn vike_data::live::SymbolResolver>, String>,
) -> Result<Box<dyn vike_data::live::SymbolResolver>, String> {
    match &sub.family {
        Some(f) => family(f),
        None => {
            Ok(Box::new(vike_data::live::SymbolResolverFixed::new(sub.symbols.iter().cloned())))
        }
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
/// listing is the gatewayhub's (`vike_gatewayhub_client::catalog::CatalogProviderGateway` over `link`; it
/// was the binance bridge's `CatalogProviderBinance` while this process linked the bridge), opened on the
/// first listing and not before ([`LazyCatalog`]). One trait, rotation as the general case, static
/// as the degenerate one.
///
/// ## ⚠ What this can actually record: TRADES and conflating DEPTH. Not quotes, not book.
///
/// Binance's declared capabilities are `DataClientCapabilities { bars, trades, depth }` — **`quotes: false`,
/// `book: false`** (`vike_venues::venue_caps`), and its `DataClient` refuses both verbs accordingly.
/// What it does serve is `subscribe_depth`, the DOM's **conflating** L2 lane, which emits through
/// `vike_data::DataClientSink::l2_snapshot` — and `RecorderSink` PERSISTS that verb, as its own
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
/// stating plainly because it interacts with `vike_backfill::capabilities`: no venue-direct backfill serves
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
/// (`crates/bridges/binance/src/family/feed/tick/mod.rs`'s `apply_depth_event`) enforced binance's SPOT
/// contiguity rule on a USDⓈ-M FUTURES stream, so the second diff of every session read as a gap
/// and the driver re-seeded on its 3 s backoff forever. Two rows per ~4.3 s cycle, both full
/// snapshots, nothing incremental. The rows that ARE there are honest snapshots; roughly 32 million
/// book states between them are not recoverable — `crates/vike-backfill/src/capabilities.rs`'s
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
    let client = Box::new(broker.handle(crate::feeds::DataClientBrokerHolder::Rec, "binance"));
    Ok(AssembledFeed::new("binance", false, symbols, client))
}

/// One venue's catalog through the gatewayhub, OPENED ON THE FIRST LISTING.
///
/// `vike_gatewayhub_client::catalog::CatalogProviderGateway::open` dials and asks the daemon to describe the
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
struct LazyCatalog {
    link: GatewayLink,
    venue: &'static str,
    opened: OnceLock<CatalogProviderGateway>,
}

impl LazyCatalog {
    fn new(link: GatewayLink, venue: &'static str) -> Self {
        Self { link, venue, opened: OnceLock::new() }
    }

    /// The opened catalog, dialling the daemon if it is not open yet. Two racing opens each ask the
    /// daemon; the second answer is dropped.
    fn catalog(&self) -> Result<&CatalogProviderGateway, CatalogError> {
        if let Some(catalog) = self.opened.get() {
            return Ok(catalog);
        }
        let catalog = CatalogProviderGateway::open(self.link.clone(), self.venue)?;
        Ok(self.opened.get_or_init(|| catalog))
    }
}

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
/// gatewayhub's polymarket `RollingFamily` (`crates/bridges/polymarket/src/catalog/rolling_family.rs`) resolves it to live tokens on every
/// tick — over the real Gamma directory, or an explicit token list — asked through
/// `vike_gatewayhub_client::resolve::SymbolResolverGateway`. Neither the broker's handle nor the
/// resolver opens a socket when the feed is built, so building cannot fail on the network; it fails
/// only on a family name with no `-<N>m`, which the resolver refuses OFFLINE with the bridge's own
/// words (`vike_catalog::polymarket_family_minutes`), gatewayhub up or not.
///
/// **`Quotes` is dropped when `Book` is requested** (`crates/vike-recorder/src/assembled.rs`'s
/// `narrow`, armed by the `true` this row passes as `book_carries_quotes`). On this venue
/// `subscribe_book` already emits derived L1 (`FeedLane::Book` → `sink.book()` **and**
/// `sink.quote()`, per `crates/bridges/polymarket/src/feed/mod.rs`'s module doc), so
/// subscribing both opens a second socket deriving the same L1 from its own copy of the book and
/// writes every quote row twice.
///
/// ⚠ Polymarket is `crate::feeds::DataClientBrokerSharing::PerHolder` (`crates/vike-datahub/src/feeds/venues.rs`'s
/// `EXPLICIT_SHARING`): its `unsubscribe` resubscribes a shard's co-tenant tokens, so the recorder
/// gets its own client, built with the recorder's own sink, and a market data release can never punch a
/// gap into a recorded book.
fn polymarket_feed(
    sub: &Subscription,
    broker: &Arc<FeedBroker>,
    link: &GatewayLink,
) -> Result<AssembledFeed, String> {
    let symbols = resolver(sub, |family| {
        Ok(Box::new(vike_gatewayhub_client::resolve::SymbolResolverGateway::new(
            link.clone(),
            family,
        )?))
    })?;
    let client = Box::new(broker.handle(crate::feeds::DataClientBrokerHolder::Rec, "polymarket"));
    Ok(AssembledFeed::new("polymarket", true, symbols, client))
}

/// The "held by strategies" resolver (decision 0193 verdict 7, R6): the symbols the recorder wants are
/// every quote key the strategy lane holds on the venue at this instant. Never `Err`: the key set is this
/// process's own memory, not a question to the gatewayhub, so there is no unknown to protect a live
/// stream from.
///
/// ⚠ It reads ONLY `Holder::Strategy`'s key set (`FeedBroker::strategy_held`), never the broker's slots, so the
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
///
/// ⚠ **A held key the store cannot name is not wanted** (a CFD like `COPPER.CMD/USD`: a symbol is the directory
/// `symbol=<symbol>`, `vike_paths::store_path::refuse_a_path_hostile_symbol`): the writer would refuse every
/// flush of it and lose the rows, so the resolver skips it and says so once. The strategy keeps its prices.
struct HeldByStrategies {
    broker: Arc<FeedBroker>,
    venue: &'static str,
    /// The instant each key was last seen held by the strategy lane.
    last_held: std::collections::BTreeMap<String, i64>,
    /// The held keys the store cannot name, each said once.
    unnameable: BTreeSet<String>,
}

/// How long a released key stays in the recorder's wanted set: two ticks of the recorder's default thirty
/// seconds. Time and not a count of ticks, so a retuned tick does not change what a restart costs.
const RELEASE_LINGER_MS: i64 = 60_000;

impl HeldByStrategies {
    fn new(broker: Arc<FeedBroker>, venue: &'static str) -> Self {
        Self {
            broker,
            venue,
            last_held: std::collections::BTreeMap::new(),
            unnameable: BTreeSet::new(),
        }
    }

    fn nameable(&mut self, symbol: &str) -> bool {
        let Err(why) = vike_paths::store_path::refuse_a_path_hostile_symbol(symbol) else {
            return true;
        };
        if self.unnameable.insert(symbol.to_string()) {
            tracing::warn!(
                venue = self.venue,
                symbol,
                why = %why,
                "recorder: a quote key the strategy lane holds cannot be named in the store, so it is not \
                 recorded; the strategy keeps its prices"
            );
        }
        false
    }
}

/// Whether the strategy lane holds a quote key of `venue` the recorder can store: when its feed is worth
/// mounting (`crate::recorder::record`). A feed of unnameable keys alone would answer an empty symbol set and
/// read as the "venue NEVER resolved a symbol" alert.
pub fn strategy_holds_a_recordable_key(broker: &FeedBroker, venue: &str) -> bool {
    broker
        .strategy_held(venue, vike_model::StreamLabel::Quotes)
        .iter()
        .any(|symbol| vike_paths::store_path::refuse_a_path_hostile_symbol(symbol).is_ok())
}

impl vike_data::live::SymbolResolver for HeldByStrategies {
    fn group(&self) -> Option<&str> {
        None
    }

    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        for symbol in self.broker.strategy_held(self.venue, vike_model::StreamLabel::Quotes) {
            if self.nameable(&symbol) {
                self.last_held.insert(symbol, now_ms);
            }
        }
        self.last_held.retain(|_, seen| now_ms.saturating_sub(*seen) < RELEASE_LINGER_MS);
        Ok(self.last_held.keys().cloned().collect())
    }
}

// ⚠ INLINE, deliberately, where the plan drew a `recording_tests.rs` beside this file — the
// `crates/vike-datahub/src/feeds/venues.rs` precedent: the code-layout rule moves a test block out
// only for a file >= 400 lines carrying >= 150 test lines, and this one is neither.
//
// These are `vike-recorder`'s former `venues` tests, moved with the table. They run only where
// this module compiles — a `record` build, i.e. every test build: the crate's self dev-dependency turns on
// every shipped feature.
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
        GatewayLink::new(
            "127.0.0.1:1",
            vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()),
        )
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

    /// Only dukascopy's tape has a lane, and it is the lane the recorder's commit keys declare, so the store's
    /// classifier and the path agree on who wrote it.
    #[test]
    fn only_dukascopy_has_a_store_lane_and_it_is_the_one_the_recorders_keys_declare() {
        assert_eq!(store_lane("dukascopy"), Some("recorder"));
        assert_eq!(
            store_lane("dukascopy"),
            vike_data::store::store_kind::source_for_key("live-dukascopy-EURUSD-quote-1-2-0")
        );
        for venue in vike_model::VENUES.iter().filter(|v| **v != "dukascopy") {
            assert_eq!(store_lane(venue), None, "{venue}");
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

    /// The polymarket row: a rolling family, and a book that carries quotes.
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
        // CONTAINS, not equals: `supported()` grows with each venue, and an equality assertion here made
        // adding the second venue fail a test about the first one.
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
    /// The exact twin of `crates/vike-datahub/src/market_data/venues.rs`'s
    /// `every_supported_venue_is_a_roster_slug`, which holds the same property for the LIVE plane's
    /// `market_data_venue=` entries. Two advertisements, two rosters, one rule — and the rule has to be
    /// stated on each side, because neither `supported()` can see the other.
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

    /// **A venue is followed only on a `Shared` row**: the recorder's client of any other would be a second
    /// login at the venue, and no `DataClientSinkRouting` would stand between it and a delayed stream. The row is the
    /// whole switch: a credentialed venue is followed exactly when it has one.
    #[test]
    fn a_venue_without_a_shared_row_is_not_followed() {
        fn sharing(venue: &str) -> DataClientBrokerSharing {
            if venue == "shares" {
                DataClientBrokerSharing::Shared
            } else {
                DataClientBrokerSharing::PerHolder
            }
        }
        assert_eq!(only_shared(vec!["shares", "per-holder", "no-row"], sharing), ["shares"]);
        for v in vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES {
            assert_eq!(
                strategy_followed().contains(&v),
                crate::feeds::venues::sharing_for(v) == DataClientBrokerSharing::Shared,
                "`{v}`"
            );
        }
    }

    /// **What following a venue takes, for every venue this build follows** (decision 0193): a credentialed
    /// roster slug, a ceiling row for all holders together, a place outside the silence watchdog, no place in
    /// the profile's list (`strategy_followed`'s doc), and a sharing row that really is `Shared`. So a venue's
    /// slice is its rows, and this test holds them together.
    #[test]
    fn every_followed_venue_has_what_following_it_takes() {
        for v in strategy_followed() {
            assert!(
                vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES.contains(&v),
                "`{v}` is no such venue"
            );
            assert_eq!(
                crate::feeds::venues::sharing_for(v),
                DataClientBrokerSharing::Shared,
                "`{v}`"
            );
            assert!(
                vike_gatewayhub_client::credentialed_limits::stream_limits_for(v).is_some(),
                "`{v}` is followed with no row in `credentialed_limits::ROWS`: nothing bounds its streams"
            );
            assert!(session_bounded().contains(&v), "`{v}` would alert every closed market");
            assert!(!supported().contains(&v), "`{v}` would need a `SERIES_CADENCE` row");
        }
    }

    /// A profile naming BOTH venues is the real multi-venue case.
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
    #[test]
    fn a_down_gatewayhub_is_an_error_at_resolve_time_never_an_empty_set() {
        use vike_recorder::runtime::VenueFeed;

        for s in [sub("binance", Some("*USDT")), sub("polymarket", Some("btc-updown-5m"))] {
            let mut f =
                feed(&s).unwrap_or_else(|e| panic!("`{}` must build offline: {e}", s.venue));
            let err = f.desired(1_700_000_000_000).expect_err("a dead link is unknown, not empty");
            assert!(err.contains("gatewayhub") || err.contains("instrument list"), "{err}");
        }
    }

    /// A malformed polymarket family is refused when the feed is BUILT, offline, with the bridge's
    /// own words: the recorder's startup refusal does not depend on the gatewayhub being up.
    #[test]
    fn a_family_without_an_interval_is_refused_at_build_with_the_gatewayhub_down() {
        let err = err_of(feed(&sub("polymarket", Some("btc-updown"))));
        assert_eq!(err, vike_catalog::polymarket_family_minutes("btc-updown").unwrap_err());
        assert!(err.contains("does not end in an interval like `-5m`"), "{err}");
    }

    /// The binance glob is matched against the listing the GATEWAYHUB serves, over the real wire:
    /// a scripted daemon lists three instruments, `*USDT` keeps the two that end that way, and the
    /// daemon was asked to describe `binance` and to list it.
    #[test]
    fn the_binance_glob_is_matched_against_the_gatewayhubs_listing() {
        use vike_gatewayhub::testkit::data::DataLaneScript;
        use vike_gatewayhub::testkit::gateway::LoopbackGateway;
        use vike_gatewayhub::testkit::rest::{RestCall, RestLaneScript};
        use vike_model::{AssetClass, Contract, Instrument};
        use vike_recorder::runtime::VenueFeed;

        let listing: Vec<Instrument> = ["BTCUSDT", "ETHUSDT", "BTCEUR"]
            .iter()
            .map(|raw| Instrument::new("binance", *raw, "BASE", "QUOTE", Contract::CryptoSpot))
            .collect();
        // Script the REST lane BEFORE the gateway starts: the server reads its facts once.
        let rest = RestLaneScript::new().with_catalog(
            "binance",
            vec![AssetClass::CryptoSpot],
            CatalogMode::Enumerable,
            listing,
        );
        let gateway = LoopbackGateway::start(DataLaneScript::new(&["binance"]), rest.clone());
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
