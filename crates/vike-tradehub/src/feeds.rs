//! Feed wiring: which venues get a live market feed, the per-mount venue-feed dispatch
//! (`venue_feed_plan`), and subscribing those feeds onto the core's ingest lanes
//! (`wire_venue_feeds`). `live_mount` (still in `main.rs`) calls into this module once per
//! distinct mounted venue.
//!
//! [`LiveFeeds`]/[`CexBars`]/[`CexTicks`] (the feed-object types) and the [`FeedCtors`]
//! construction seam + its production impl [`ProdFeedCtors`] moved here alongside the functions
//! that name them in their signatures — `recon_feed_statuses_of` takes `&[LiveFeeds]`,
//! `wire_venue_feeds` returns `Result<LiveFeeds, String>` and takes `&dyn FeedCtors` — for the
//! same reason `CexVenue`/`VenuePlan` moved to `types.rs`: a library crate cannot name a type the
//! binary depending on it defines. `main.rs`'s own `live_mount`/`live_mount_with` (which stay
//! there) and its `#[cfg(test)] mod tests`/`feed_splice_seam_tests` reach these through
//! `use vike_tradehub::feeds::{...}` — `use super::*`/`use super::{...}` inside those then see
//! them as if they were still main.rs's own private items.
//!
//! ⚠ `venue_feed_plan`'s `match cfg.venue.as_str() { ... }` is text-scanned by
//! `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` to prove `LIVE_WIRED_VENUES`
//! matches this dispatch's arms exactly. That test's `MAIN` constant now points HERE, not at
//! `main.rs` — see that test's own doc for the re-anchor (tradehub-main-split refactor, Task 3).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_run::MakerMountConfig;

use crate::venue_arming::{
    alpaca_arming, cex_arming, cex_mainnet_enabled, ctrader_arming, data_only_arming,
    deribit_arming, ig_arming, oanda_arming, other_live_accounts, with_other_live_accounts,
};
use crate::venue_plan::{alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan};
use crate::{CexVenue, VenuePlan};
// `ResolvedMount`'s only consumer here is `check_poly_token_intervals`, itself gated on this same
// feature — an unconditional import would go unused (and `-D warnings` would refuse it) on a
// default build.
#[cfg(feature = "polymarket")]
use crate::ResolvedMount;

/// The CEX quote/trade/book pump handle — the feed that actually drives the maker.
///
/// ⚠ **This is a THIRD feed shape, and the reason it exists is the whole point of the CEX arm.**
/// binance/bybit/okx all declare `live_data.quotes = false` and `live_data.book = false`
/// (`vike_model::venue_caps`), so `DataClient::subscribe_quotes` is a caps-driven refusal and the
/// hyperliquid arm's model cannot be copied. The obvious substitute — `subscribe_depth` — is a
/// SILENT NO-OP: it emits `LiveDataSink::l2_snapshot`, whose body is the trait's default
/// `let _ = (venue, symbol, tick_size, bids, asks, ts);` and which NEITHER sink this daemon owns
/// overrides (`vike_core::CoreLaneSink` says so in its module doc; `vike_run::MakerSink` never
/// mentions the verb). A depth-wired maker connects, seeds, validates checksums, reports healthy —
/// and posts zero orders forever, because `vike_mm::SpreadMaker` reaches `requote` from exactly
/// `on_quote_tick` and `on_order_book`, and neither is ever called.
///
/// `market_data::spawn_*_market_data` bypasses that seam entirely: it takes a
/// [`vike_exec::TickSender`] and pushes `Ingest::Quote`/`Trade`/`Book` straight onto the core's
/// tick lane, which the runtime dispatches to `on_quote_tick`/`on_order_book`.
///
/// ⚠ **EVERY CEX VENUE HERE PUBLISHES A QUOTE**, and each drives BOTH maker verbs. What differs is only
/// where the quote COMES FROM ([`CexVenue::quote_source`]): binance and okx decode a native
/// top-of-book channel (`@bookTicker`, `bbo-tbt`), while bybit's pump DERIVES one — its
/// `MdEvent::BookUpdated` arm sends `ticks.book(..)` and then `quote_from_book(&book, symbol)`,
/// pushing a `QuoteUpdate` on the same lane binance and okx use. The `venue_caps` row
/// (`live_data.quotes = false`) describes the `DataClient::subscribe_quotes` seam, which this pump
/// does not go through, and says nothing about it.
///
/// This paragraph previously claimed bybit "emits none and drives the maker through
/// `on_order_book`", and the mount logged a `quote_lane = "on_order_book"` field asserting it. Both
/// were FALSE — the same class as a false `LIVE_CAPABLE` row: a capability claim in a runtime log
/// field that an operator reads as measurement. The one real bybit caveat is that
/// `quote_from_book` returns `None` on a ONE-SIDED book, so its quote lane is silent until both
/// sides are populated; the book lane is live either way.
///
/// ⚠ `MarketDataFeed::shutdown` takes `self` BY VALUE on every venue here (unlike
/// `DataClient::shutdown(&mut self)`), which is why [`LiveFeeds::Cex`] holds this in an `Option`
/// and `take()`s it — see that variant.
pub enum CexTicks {
    Binance(vike_binance::market_data::MarketDataFeed),
    Bybit(vike_bybit::market_data::MarketDataFeed),
    Okx(vike_okx::market_data::MarketDataFeed),
    /// The binance fork's same pump shape (`vike_aster::market_data` re-exports the shared
    /// `family::depth` handle), futures-hosted and `Environment`-keyed — the daemon pins `Live`
    /// (mainnet hosts); see the aster arm in `wire_venue_feeds` for why.
    Aster(vike_aster::market_data::MarketDataFeed),
}

impl CexTicks {
    /// Stop + join the pump thread. BY VALUE, mirroring the venue handles it wraps.
    pub fn shutdown(self) {
        match self {
            CexTicks::Binance(f) => f.shutdown(),
            CexTicks::Bybit(f) => f.shutdown(),
            CexTicks::Okx(f) => f.shutdown(),
            CexTicks::Aster(f) => f.shutdown(),
        }
    }
}

/// The CEX kline feed — the `DataClient` half of the pair, subscribed for BARS only.
///
/// ⚠ Bars are not decoration on this mount. The account-wide margin-call watchdog and the
/// equity-drawdown latch (`CoreConfig::margin_call` / `max_drawdown`, both armed by `live_mount`)
/// fire on the per-CLOSED-BAR cadence and nowhere else — `vike_core`'s `strategy_drive` calls
/// `sweep_margin_call`/`sweep_drawdown_latch` from the `BarClose` path. A quote-only CEX mount
/// would therefore run a live account with BOTH safety watchdogs permanently dead, which is the
/// same class of failure as the silent no-quote above, one layer down.
///
/// The bar lane also supplies the venue's own feed-status handle (`Feeds::status`), which
/// `live_mount` threads into the reconcile health gate.
pub enum CexBars {
    Binance(vike_binance::market_feed::Feeds),
    Bybit(vike_bybit::market_feed::Feeds),
    Okx(vike_okx::market_feed::Feeds),
    /// Constructed via `Feeds::with_env(.., Environment::Live)` — aster's kline shell is
    /// `Environment`-keyed (its ONE real delta from the binance template) and `Feeds::new`
    /// defaults to TESTNET hosts; see the aster arm in `wire_venue_feeds`.
    Aster(vike_aster::market_feed::Feeds),
}

impl CexBars {
    /// Which venue this feed belongs to — the same slug [`CexVenue::slug`] yields, so the
    /// feed-status map below is keyed exactly as `ReconManager::should_reconcile` looks it up.
    pub fn slug(&self) -> &'static str {
        match self {
            CexBars::Binance(_) => CexVenue::Binance.slug(),
            CexBars::Bybit(_) => CexVenue::Bybit.slug(),
            CexBars::Okx(_) => CexVenue::Okx.slug(),
            CexBars::Aster(_) => CexVenue::Aster.slug(),
        }
    }

    /// The venue's live feed-status string — the handle `reconcile_config::build_recon_config`
    /// health-gates that venue's reconcile pass on. Same `Arc<Mutex<String>>` field `vike-app`'s
    /// `recon_feed_statuses` clones.
    pub fn status(&self) -> Arc<std::sync::Mutex<String>> {
        match self {
            CexBars::Binance(f) => Arc::clone(&f.status),
            CexBars::Bybit(f) => Arc::clone(&f.status),
            CexBars::Okx(f) => Arc::clone(&f.status),
            CexBars::Aster(f) => Arc::clone(&f.status),
        }
    }

    pub fn shutdown(&mut self) {
        match self {
            CexBars::Binance(f) => f.shutdown(),
            CexBars::Bybit(f) => f.shutdown(),
            CexBars::Okx(f) => f.shutdown(),
            CexBars::Aster(f) => f.shutdown(),
        }
    }
}

/// ONE live venue's market feed — one variant per live-wired venue, so the daemon's teardown stays
/// venue-agnostic. The HL/Polymarket feed types implement [`vike_data::DataClient`] (whose
/// `shutdown(&mut self)` stops + joins the feed threads); [`Self::shutdown`] dispatches. The
/// `Polymarket` variant only exists in a `--features polymarket` build.
///
/// Since split-plane I10 the daemon holds a COLLECTION of these — one entry per DISTINCT mounted
/// venue, in mount order (`live_mount`'s `feeds` vec; a single-mount profile holds one) — and each
/// venue's feed arm is wired exactly once however many mounts share the venue.
pub enum LiveFeeds {
    Hyperliquid(vike_hyperliquid::market_feed::Feeds),
    /// One `Feeds` (its own WS + `MakerSink` bar synth) per DISTINCT mounted token: a `MakerSink`
    /// folds EVERY price it sees into ITS token's synth bars, so two tokens through one sink would
    /// corrupt both mounts' paper-fill bars — the per-token split is correctness, not tidiness.
    /// (Two mounts on ONE token share one entry; a same-token interval mismatch is refused before
    /// the core spawns — see `live_mount`'s planning pass.)
    #[cfg(feature = "polymarket")]
    Polymarket(Vec<vike_polymarket::Feeds>),
    /// binance/bybit/okx: the tick pump AND the kline feed, both required (see [`CexTicks`] and
    /// [`CexBars`] for why neither alone is a working mount).
    ///
    /// ⚠ `ticks` is an `Option` PURELY so teardown can `take()` it: `CexTicks::shutdown` consumes
    /// `self`, while this dispatcher only borrows `&mut self`. Without the `Option` the ordered
    /// stop+join could not be called at all and the pump would fall to its `Drop` impl instead —
    /// which does join, but at an unspecified point after the core has already gone, rather than
    /// inside the daemon's bounded-shutdown budget. It is `None` only between `take()` and the end
    /// of teardown.
    Cex {
        ticks: Option<CexTicks>,
        bars: CexBars,
    },
    /// alpaca (split-plane I9): the ONE multiplexed, OAuth-authenticated market-data WS client —
    /// real 1m bar closes + quotes + trades through `CoreLaneSink`. A real
    /// [`vike_data::DataClient`], so `shutdown(&mut self)` matches this dispatcher's borrow and
    /// needs no `Option` dance (unlike [`CexTicks`]); it stops + joins every per-asset-class
    /// connection thread.
    Alpaca(vike_alpaca::AlpacaDataClient),
    /// ctrader (split-plane I9): the command-side half over the daemon's own DEDICATED data
    /// socket ([`vike_ctrader::data::CtraderData::new`] — this view OWNS the actor thread, so its
    /// `shutdown(&mut self)` unsubscribes and sends `Command::Shutdown`; the actor join happens on
    /// drop of the owned handle). Closed bars are SYNTHESIZED from quote mids by the
    /// [`vike_run::MakerSink`] the actor pushes into — the socket itself never emits a bar close.
    Ctrader(vike_ctrader::data::CtraderData),
    /// oanda (split-plane I9): ONE [`vike_data::DataClient`] over TWO unlike transports, because
    /// the venue publishes its two lanes unlike — quotes off the chunked-HTTP `/pricing/stream`
    /// line stream (this venue has no market-data WS at all, which is why it is `OwnPump` in
    /// `vike_bridge_core::pump_spec` and why no shared market pump can carry it), bars POLLED off
    /// the candles REST endpoint. Both lanes are threads owned by the client's own
    /// `vike_data::FeedRegistry`, so `shutdown(&mut self)` stops + joins every one of them and
    /// this dispatcher needs no [`CexTicks`]-style `Option` dance.
    ///
    /// ⚠ The join is DETERMINISTIC, not instant: a quote thread observes its stop flag between
    /// lines, so teardown waits for the venue's next ~5 s heartbeat (or the stream's idle bound)
    /// worst-case. That is inside the daemon's per-venue bounded shutdown task, not outside it.
    ///
    /// Held behind the [`DataClient`] seam (`Box<dyn DataClient + Send>`) since the arm was
    /// routed through [`FeedCtors`] — the deribit variant's shape and reason: production stores
    /// the venue's real `market_feed::Feeds` (the trait's `oanda` default constructs nothing
    /// else), the deterministic splice test stores its scripted double, and teardown dispatches
    /// through the same `shutdown(&mut self)` either way.
    Oanda(Box<dyn DataClient + Send>),
    /// deribit (split-plane I9): the widest [`vike_data::DataClient`] this daemon mounts — FOUR
    /// live verbs (bars, quotes, trades, book) served off the venue's KEYLESS public MAINNET
    /// JSON-RPC host, one stoppable socket per subscription through the client's own
    /// `vike_data::FeedRegistry`, every session riding the shared `run_market_feed` driver at this
    /// venue's `pump_spec` row. `shutdown(&mut self)` stops + joins all of them, so this
    /// dispatcher needs no [`CexTicks`]-style `Option` dance: unlike the CEX venues there is no
    /// second pump object, because the book verb here is the LOSSLESS `LiveDataSink::book` lane
    /// (which `vike_core::CoreLaneSink` really forwards) rather than the conflating
    /// `l2_snapshot` one the CEX arm has to route around.
    ///
    /// Held behind the [`DataClient`] seam (`Box<dyn DataClient + Send>`) because this is the arm
    /// routed through [`FeedCtors`]: production stores the venue's real `market_feed::Feeds`
    /// (that trait's default constructs nothing else), the deterministic splice test stores its
    /// scripted double, and teardown dispatches through the same `shutdown(&mut self)` either
    /// way.
    Deribit(Box<dyn DataClient + Send>),
    /// ig (split-plane I9): the NARROWEST live feed this daemon mounts, and the only one that is
    /// not JSON — Lightstreamer TLCP 2.1.0 text frames spoken raw over the shared tungstenite
    /// stack, one stoppable thread per subscription through the client's own
    /// `vike_data::FeedRegistry`, each riding the shared `run_market_feed_on` driver at this
    /// venue's `pump_spec` row. TWO verbs (`subscribe_quotes` + `subscribe_bars`) and no more:
    /// trades and book are caps refusals, because a DEALER venue publishes its own two-sided price
    /// and no public tape or ladder exists behind them.
    ///
    /// ⚠ Each subscription thread owns its OWN `IgSession` login — more REST sessions than the
    /// exec side opens, and the price of the driver's one-socket-per-subscription shape. Unlike
    /// the exec lane this one DOES recover from token expiry (a `CONERR` drops the cached token
    /// pair and the next attempt re-logs-in), which is why a long-running daemon mount is viable
    /// here at all.
    Ig(vike_ig::market_feed::Feeds),
}

impl LiveFeeds {
    /// The per-venue feed-status handles for the reconcile HEALTH GATE — `ReconManager::
    /// should_reconcile(venue)` skips a pass while THAT venue's own feed reads `Degraded`.
    ///
    /// ⚠ Only a venue with a real feed handle belongs here, and only its OWN row. An absent venue
    /// reads `Healthy` and is never health-blocked, which is the correct exec-only-venue shape (and
    /// the safe direction: the gate can only ever SUPPRESS a pass, and a wrongly-suppressed pass can
    /// stay suppressed — see `reconcile_config::health_from_feed_status`'s doc).
    ///
    /// So this stays EMPTY for hyperliquid and polymarket, byte-identically to before the CEX arm
    /// existed. Not an oversight and not laziness: adding a handle can only take passes away, so
    /// each venue's row is earned by a deliberate decision about that venue's feed, never by a
    /// blanket sweep over whatever handles happen to be reachable. The CEX row is earned because
    /// its `Feeds::status` is the same field `vike-app`'s `recon_feed_statuses` already gates
    /// binance/bybit/okx on.
    ///
    /// **The alpaca/ctrader decision (split-plane I9): NO row, deliberately.** Neither
    /// `AlpacaDataClient` nor `CtraderData` exposes a `Feeds::status`-shaped handle on this seam —
    /// each buries its connection health in its own reconnect loop — so there is no evidence-grade
    /// handle to key a row on, and inventing one here would be exactly the blanket sweep the rule
    /// above forbids. This also MATCHES the exec plane's standing classification: both venues
    /// reconcile on the periodic interval only, are absent from vike-app's own
    /// `recon_feed_statuses` map, read `Healthy` and are never health-blocked (root CLAUDE.md's
    /// per-venue health gate — "deribit/alpaca/ctrader/ig/oanda … are NOT in the map"). A future
    /// row is earned the day a bridge exposes a real status handle, not before.
    ///
    /// **The oanda decision: NO row either — but NOT for the alpaca/ctrader reason, and saying so
    /// matters.** `vike_oanda::market_feed::Feeds` DOES expose a `status` handle of exactly the
    /// `Arc<Mutex<String>>` shape the CEX row keys on, so "no handle exists" would be false here.
    /// The row is withheld because that handle is not evidence about the VENUE: one string is
    /// shared, last-writer-wins, by every subscription thread this client spawns across its two
    /// unlike transports, so a transient candle-POLL failure on the bar lane writes
    /// `"… poll failed (HTTP 5xx); retrying"` — which `vike_model::parse_feed_status` reads as
    /// `Error` and `health_from_feed_status` maps to `Degraded` — and SUPPRESSES an oanda
    /// reconcile pass that has nothing to do with the bar poll, while the quote stream underneath
    /// is perfectly alive. The gate can only ever take passes AWAY (see
    /// `reconcile_config::health_from_feed_status`'s fail-soft asymmetry), so an ambiguous handle
    /// is worse than none. It also keeps this daemon in lockstep with the exec plane's standing
    /// classification of oanda as an interval-only, never-health-blocked venue. What would EARN
    /// the row: a per-LANE status the bar poll cannot write into, or a lane-scoped health handle
    /// on the client — not this string.
    ///
    /// **The deribit decision (split-plane I9): NO row, and it is the oanda argument at its
    /// strongest.** `vike_deribit::market_feed::Feeds` also exposes a `status` handle of the CEX
    /// row's shape, and it is shared last-writer-wins across FOUR lanes rather than two — the
    /// bars, quotes, trades and book threads all write it, and each of them writes an error string
    /// on its own reconnect. The book lane in particular writes one on every deliberate resync
    /// (`route_frame`'s `MdEvent::Resync` is a session fault by design — the reconnect IS the
    /// resync), so a perfectly healthy deribit feed publishes `Degraded`-reading text as ordinary
    /// operation, and a row here would suppress reconcile passes on the venue whose exec sits on a
    /// DIFFERENT network from the feed entirely. Same "what would earn it" as oanda: a per-lane
    /// handle, not this string.
    ///
    /// **The ig decision (split-plane I9): NO row, the same shape again — and here the ambiguity
    /// is not even about failure.** `vike_ig::market_feed::Feeds` exposes a `status` handle of the
    /// CEX row's shape, shared last-writer-wins by every subscription thread; each of those
    /// threads holds its OWN `IgSession` login, so the string answers about whichever session
    /// wrote last rather than about the venue. Worse for a health gate than oanda's: FX CLOSES on
    /// the weekend, and a quote item on a closed market legitimately delivers nothing for two
    /// days, so any status this feed learns to publish about quiet would read as a fault. A gate
    /// that suppressed IG reconcile passes every weekend would be strictly worse than none, and
    /// the gate can only ever take passes AWAY. Same "what would earn it": a per-lane handle that
    /// distinguishes a dead socket from a closed market.
    pub fn recon_feed_statuses(&self) -> HashMap<String, Arc<std::sync::Mutex<String>>> {
        match self {
            LiveFeeds::Cex { bars, .. } => {
                HashMap::from([(bars.slug().to_string(), bars.status())])
            }
            LiveFeeds::Hyperliquid(_) => HashMap::new(),
            #[cfg(feature = "polymarket")]
            LiveFeeds::Polymarket(_) => HashMap::new(),
            // No status handle exists on either client — see the method doc's alpaca/ctrader
            // paragraph for why empty is the DECISION, not a gap.
            LiveFeeds::Alpaca(_) => HashMap::new(),
            LiveFeeds::Ctrader(_) => HashMap::new(),
            // A status handle DOES exist here — and is still withheld, for the reason in the
            // method doc's oanda paragraph (one last-writer-wins string across two lanes).
            LiveFeeds::Oanda(_) => HashMap::new(),
            // Same shape as oanda's, one lane count worse — see the method doc's deribit
            // paragraph (four lanes on one string, and the book lane's resync writes into it).
            LiveFeeds::Deribit(_) => HashMap::new(),
            // The same shape once more, and the closed-market case makes it sharpest — see the
            // method doc's ig paragraph.
            LiveFeeds::Ig(_) => HashMap::new(),
        }
    }

    pub fn shutdown(&mut self) {
        match self {
            LiveFeeds::Hyperliquid(f) => f.shutdown(),
            #[cfg(feature = "polymarket")]
            LiveFeeds::Polymarket(list) => {
                for f in list {
                    f.shutdown();
                }
            }
            // Both are `DataClient::shutdown(&mut self)` — stop + join the WS connection threads
            // (alpaca) / unsubscribe + `Command::Shutdown` the owned actor (ctrader).
            LiveFeeds::Alpaca(f) => f.shutdown(),
            LiveFeeds::Ctrader(f) => f.shutdown(),
            // `FeedRegistry::shutdown` — raise every subscription's stop flag, then join every
            // thread (quote pump and bar poller alike). See the variant's teardown-latency note.
            LiveFeeds::Oanda(f) => f.shutdown(),
            // `FeedRegistry::shutdown` again — raise every subscription's stop flag, then join all
            // four lanes' threads. Each observes its flag within the row's `read_timeout`, so the
            // join is prompt rather than heartbeat-bound (oanda's caveat does not transfer).
            LiveFeeds::Deribit(f) => f.shutdown(),
            // `FeedRegistry::shutdown` again — every TLCP thread observes its stop flag within
            // the row's `read_timeout`, so the join is prompt. Each thread's IG session is simply
            // abandoned: this crate has no logout call, and IG expires a session on its own.
            LiveFeeds::Ig(f) => f.shutdown(),
            LiveFeeds::Cex { ticks, bars } => {
                // Ticks FIRST: the pump is the lane the strategy acts on, so stopping it before the
                // kline feed means no quote can arrive for a bar window that is already closing.
                if let Some(t) = ticks.take() {
                    t.shutdown();
                }
                bars.shutdown();
            }
        }
    }
}

/// The MERGED per-venue feed-status map across every mounted venue's feed (split-plane I10) —
/// what the reconcile health gate takes now that `live_mount` wires N venues. Each entry
/// contributes its own [`LiveFeeds::recon_feed_statuses`] rows (CEX venues only, by that method's
/// own evidence rule); the venue key is unique per entry (one CEX entry per venue), so a plain
/// extend cannot collide.
pub fn recon_feed_statuses_of(
    feeds: &[LiveFeeds],
) -> HashMap<String, Arc<std::sync::Mutex<String>>> {
    let mut merged = HashMap::new();
    for f in feeds {
        merged.extend(f.recon_feed_statuses());
    }
    merged
}

/// The LIVE arm's extra teardown handles (there are none in the paper arm). Held from mount to the
/// bounded shutdown so the venue feed threads are stopped+joined and the live-event forwarder is
/// quiesced in the load-bearing order (forwarder-stop BEFORE the core shutdown+join — see
/// `vike_run::node`'s forwarder teardown-safety note).
/// Extra teardown run AFTER `feeds.shutdown()` and BEFORE the core join — the recorder's bounded flush
/// when `record-feeds` + `VIKE_TRADEHUB_RECORD=1`, else `None` (byte-identical to no recording). Boxed
/// (and aliased) so the struct never names the feature-gated `RecorderHandle` type, and to keep the
/// recorder-open tuple under clippy's `type_complexity` bar.
pub type PostFeeds = Option<Box<dyn FnOnce() + Send>>;

pub struct LiveTeardown {
    /// One entry per mounted venue's feed (split-plane I10) — a single-mount daemon holds one.
    /// The teardown fans these out as one bounded task EACH, so every venue's feed quiesces
    /// inside the ONE shutdown deadline and a wedged venue cannot serialize its siblings.
    pub feeds: Vec<LiveFeeds>,
    pub forwarder_stop: Arc<AtomicBool>,
    pub post_feeds: PostFeeds,
    /// The reconciliation driver, `Some` when the S2 reconcile gate said yes AND ≥1 live venue
    /// produced a ReconClient (`None` on a paper mount and on any run the operator refused). Shut
    /// down in the SEQUENTIAL tail
    /// BEFORE the core join (vike-app's teardown order): it holds only a WEAK core-ingest sender so it
    /// can never wedge the core, but joining it there bounds its `vt-core-recon` thread to the daemon's
    /// lifetime rather than leaking it.
    pub recon_driver: Option<vike_core::ReconDriver>,
}

/// The per-venue PRE-BUILD gate: `live_mount`'s venue dispatch, called once per MOUNT (split-plane
/// I10 made it a function — `live_mount` loops it over the mount set, so every mount passes its
/// own venue's refusals, and keeps ONE plan per distinct venue). A venue without an arm here is
/// not live-wired; `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` pins these arms
/// equal to `LIVE_WIRED_VENUES`, both directions, under this build's features.
///
/// `hyperliquid_mainnet` is the ALREADY-RESOLVED `HYPERLIQUID_MAINNET` verdict — folded in
/// `main.rs` (this function's only caller) rather than read here: this file is a LIBRARY module,
/// and `crates/vike-ops/src/settings.rs`'s settings-registry gate refuses a library reading
/// process env its caller cannot see, override, or even know about. See the `"hyperliquid"` arm
/// below for the fold this parameter replaces.
pub fn venue_feed_plan(
    cfg: &MakerMountConfig,
    vars: &HashMap<String, String>,
    hyperliquid_mainnet: bool,
) -> Result<VenuePlan, String> {
    let plan = match cfg.venue.as_str() {
        // ── The CEX venues. One shared gate (`cex_plan`), because they share one feed shape.
        //
        // ⚠ Their market DATA is MAINNET on all of them, while exec follows each venue's own
        // demo/mainnet credential gate inside `build_node`. On binance/bybit/okx that split is
        // STRUCTURAL, not an oversight: `market_data.rs`'s host consts are exactly one per venue
        // (binance `MAINNET_WS`, bybit `PUBLIC_WS_LINEAR`, okx `PUBLIC_WS`) and bybit's demo
        // environment serves no public market WS at all. So a demo mount is DEMO EXEC OVER MAINNET
        // PRICES. That is the intended shape — a demo order must rest against the real book to mean
        // anything — but it is stated here so nobody later "fixes" the feed to follow the exec
        // environment and silently points a live account at a dead host. (Aster reaches the same
        // shape by CHOICE rather than structure — its arm below says how and why.)
        "binance" => {
            // SPOT lane (`BTCUSDT`, no `.P`): `WIRED_MARKETS` pins the spot symbol, and
            // `spawn_binance_market_data` is the spot `@bookTicker`+`@trade`+`@depth` combined
            // stream, so the maker prices on `on_quote_tick` off a NATIVE L1 channel. (Every CEX
            // venue here drives `on_quote_tick`; only the quote's PROVENANCE differs — see
            // `CexVenue::quote_source`.)
            cex_plan(CexVenue::Binance, cfg, cex_mainnet_enabled(CexVenue::Binance, vars))?
        }
        "bybit" => {
            // LINEAR PERP. ⚠ The two lanes read the same `BTCUSDT` string but reach DIFFERENT
            // instruments, and that is deliberate rather than a bug to be tidied away:
            // `spawn_bybit_market_data` connects `PUBLIC_WS_LINEAR` (the perp — matching the linear
            // perp `make_engine` mounts), while `market_feed`'s kline lane runs `perp_split`, which
            // maps a suffix-less `BTCUSDT` to SPOT klines. Passing `BTCUSDT.P` to fix the bar lane
            // is NOT available: the feed then labels the series `BTCUSDT.P`, which
            // `accepts_symbol("BTCUSDT")` rejects, and every bar is silently dropped — taking the
            // margin/drawdown sweeps with it.
            //
            // The consequence is bounded and it is the right trade. `PriceBoard::resolve` walks
            // mark → bid/ask → last trade → BAR CLOSE, so the spot close occupies the LAST rung
            // only: the perp's own bid/ask and last trade (both from the linear pump) outrank it
            // whenever that pump is alive, and the bar lane's real job here is the per-closed-bar
            // CADENCE that fires the watchdogs. A few bp of spot/perp basis on the final fallback
            // rung beats the alternative, which is no watchdog cadence at all.
            cex_plan(CexVenue::Bybit, cfg, cex_mainnet_enabled(CexVenue::Bybit, vars))?
        }
        "okx" => {
            // SWAP (`BTC-USDT-SWAP`). `spawn_okx_market_data` subscribes `bbo-tbt` + `trades` +
            // `books5` — like binance, a NATIVE L1 channel behind `on_quote_tick`.
            //
            // ⚠ okx's grid is denominated in CONTRACTS while the engine's qty is BASE, and
            // `RiskLimits::from_properties` copies `min_qty`/`step_size` across verbatim. On
            // `BTC-USDT-SWAP` (`ctVal` 0.01, `lotSz` 0.01) that makes the daemon's effective floor
            // 0.01 BTC against a venue floor of 0.0001 BTC — 100x CONSERVATIVE, i.e. it fails safe.
            // Stated because the same confusion fails OPEN if one side is ever "corrected" alone:
            // `perp.rs`'s `to_contracts` truncates any base qty below 0.0001 BTC to `sz=0`, and the
            // only thing preventing that from reaching the wire today is the over-strict min_qty.
            cex_plan(CexVenue::Okx, cfg, cex_mainnet_enabled(CexVenue::Okx, vars))?
        }
        "aster" => {
            // USDⓈ-M PERP, vike's `.P` spelling (`BTCUSDT.P` — `WIRED_MARKETS` pins it; the pump
            // and the kline lane each split it to the exchange's `BTCUSDT` for the wire while the
            // series label keeps the `.P`, so `accepts_symbol` and `drive_strategy_tick` both see
            // the mounted spelling). A binance fork: `spawn_aster_market_data` is the same
            // `@bookTicker`+`@trade`+`@depth@100ms` combined stream on the futures host — a
            // NATIVE L1 channel behind `on_quote_tick`, like binance.
            //
            // ⚠ Feed hosts are `Environment`-keyed on this venue (unlike the three above, whose
            // pumps have exactly one host each) — a testnet EXISTS; the daemon still PINS the
            // feed to `Environment::Live` (mainnet) in `wire_venue_feeds`, deliberately: exec
            // resolves MAINNET-FIRST from credentials (`make_engine`'s aster arm, no
            // `ASTER_MAINNET` flag exists), only the LIVE tier is configured in practice, and a
            // demo/paper order must rest against the real book to mean anything — the same
            // demo-exec-over-mainnet-prices shape as the three venues above, reached by choice
            // rather than by structure. vike-app's aster mount makes the identical choice for the
            // identical reason (its `Feeds::with_env(.., Live)` beside the mainnet-always
            // `AsterCatalog`).
            //
            // ⚠⚠ REAL MONEY IN PRACTICE: present `ASTER_LIVE_*` credentials make this a LIVE
            // MAINNET exec mount. This arm is CAPABILITY only — absent credentials still mount
            // paper (the workspace live gate), and nothing here changes any default.
            cex_plan(CexVenue::Aster, cfg, cex_mainnet_enabled(CexVenue::Aster, vars))?
        }
        "alpaca" => {
            // US EQUITY (`AAPL` — `WIRED_MARKETS` pins it), the first CREDENTIALED-DATA arm
            // (split-plane I9): NOT the CEX two-lane shape. There is no keyless pump —
            // `AlpacaDataClient` is one multiplexed, OAuth-authenticated WS client serving real 1m
            // bar CLOSES (the watchdog cadence), L1 quotes (a native channel behind
            // `on_quote_tick`) and trade prints, all through `CoreLaneSink`. The plan therefore
            // carries the resolved SANDBOX credentials (`vars` is gone by feed time), and ABSENT
            // credentials REFUSE the mount here rather than degrading: exec-degrades-to-paper
            // still leaves a working mount, but a feed-less mount quotes into the void forever.
            // Data hosts follow the SAME tier the exec side is pinned to (SANDBOX — `make_engine`
            // resolves `Demo` for alpaca and no `ALPACA_MAINNET` flag exists), so unlike the CEX
            // arms this venue's demo mount is DEMO EXEC OVER SANDBOX PRICES — the venue serves a
            // sandbox data stream, which the CEX venues do not.
            alpaca_plan(cfg, vars)?
        }
        "ctrader" => {
            // FX (`EURUSD` — `WIRED_MARKETS` pins it), the second credentialed-data arm
            // (split-plane I9): one DEDICATED protobuf/TLS data socket (`connect_and_auth`,
            // SYNCHRONOUS at mount — wire failure refuses startup, see `wire_venue_feeds`),
            // isolated from the exec socket `make_engine` opens, per the same isolation rule every
            // recon client follows. Serves L1 spot quotes; live trendbars only ever FORM through
            // this client (no close fires), so the arm synthesizes closed bars from quote mids via
            // `MakerSink` — the polymarket shape — and the DEMO host follows the exec side's own
            // DEMO-tier pin. Absent credentials refuse the mount (same argument as alpaca above).
            ctrader_plan(cfg, vars)?
        }
        "oanda" => {
            // FX (`EURUSD` — `WIRED_MARKETS` pins it; `to_oanda_instrument` splits it to the
            // venue's own `EUR_USD` on the wire while every emitted series keeps the mounted
            // spelling `accepts_symbol` needs), the third credentialed-data arm (split-plane I9)
            // and the only live-wired venue with NO market-data socket of any kind: this venue
            // publishes no market-data WS, so its feed is `pump_spec`'s `OwnPump` class. ONE
            // `DataClient` over TWO unlike transports — quotes off the chunked-HTTP
            // `/pricing/stream` line stream, bars POLLED off the candles REST endpoint — both
            // Bearer-authed against the same account, which is why absent credentials REFUSE the
            // mount here (alpaca's argument: exec-degrades-to-paper still leaves a working mount,
            // a feed-less mount quotes into the void).
            //
            // ⚠ Unlike ctrader, the bars are REAL venue candles on the LOSSLESS `close_bar` lane,
            // so the per-CLOSED-bar watchdogs run off the venue's own closes rather than a
            // synthesizer — hence the alpaca-shaped `CoreLaneSink` wiring in `wire_venue_feeds`,
            // and hence no same-venue interval gate (see `oanda_plan`).
            //
            // ⚠ PRACTICE hosts on BOTH planes, and this is the one venue where feed and exec
            // agree by construction rather than by choice: `make_engine` resolves
            // `load_oanda_config_from(Demo, …)` unconditionally (no `OANDA_MAINNET` flag), the
            // plan resolves the SAME loader at the SAME tier, and `oanda_hosts(Demo)` supplies
            // both bases inside the one `OandaConfig` the plan carries. So this is practice exec
            // over PRACTICE prices — NOT the demo-exec-over-mainnet-prices shape the CEX arms
            // above describe, because this venue actually serves a practice price stream.
            oanda_plan(cfg, vars)?
        }
        "deribit" => {
            // The OPTIONS venue's inverse perp (`BTC-PERPETUAL` — `WIRED_MARKETS` pins it; deribit
            // instrument names are already unambiguous, so there is no `.P` split and the wire
            // symbol IS the series symbol). Split-plane I9 gave it a real `DataClient` where
            // `pump_spec` used to say `NoPump`, and it is the WIDEST one this daemon mounts: FOUR
            // verbs — bars, quotes, trades AND the lossless L2 book — off one keyless public
            // socket per subscription. So this is neither the CEX two-lane shape (no separate
            // `spawn_*_market_data` pump exists, and none is needed: `subscribe_book` emits
            // `LiveDataSink::book`, which `CoreLaneSink` really forwards) nor the
            // credentialed-data shape (nothing here authenticates).
            //
            // ⚠ NO CREDENTIAL REFUSAL, deliberately — the one place this arm must NOT be copied
            // from its alpaca/ctrader/oanda neighbours. The feed is keyless, so absent credentials
            // leave a working feed over a PAPER exec book: the workspace live gate, exactly as on
            // a CEX venue. See `deribit_plan`.
            //
            // ⚠⚠ THE TWO HALVES OF THIS VENUE POINT AT DIFFERENT NETWORKS, and unlike the CEX
            // arms above that is STRUCTURAL rather than a choice this daemon makes: every public
            // read in the bridge is hardcoded mainnet `www.deribit.com` and every authed socket is
            // hardcoded `test.deribit.com` (`crates/bridges/deribit/CLAUDE.md`). There is no flag
            // and no credential tier that reaches deribit mainnet execution — `transport.rs`'s
            // `MAINNET_REST` has no caller in the workspace. So a live deribit mount is TESTNET
            // exec over MAINNET prices, which `deribit_arming` discloses in words.
            deribit_plan(cfg)?
        }
        "ig" => {
            // FX CFD (`CS.D.EURUSD.MINI.IP` — `WIRED_MARKETS` pins the EPIC, which is IG's own
            // instrument identifier and travels verbatim on both planes: the exec POST body and
            // the Lightstreamer `MARKET:`/`CHART:` item names alike, so there is no split of any
            // kind here). The fourth credentialed-data arm (split-plane I9) and the only venue
            // whose feed is not JSON-over-WS — Lightstreamer TLCP 2.1.0 text frames on the shared
            // tungstenite stack, `pump_spec`'s `OnDriver` class with `subscribe_frame: false`
            // because the TLCP handshake (`create_session` must answer CONOK before `control` may
            // subscribe) cannot be a replayed frame and lives in the venue's connect closure.
            //
            // ⚠ TWO VERBS, AND THE ABSENCE IS STRUCTURAL — this is the arm not to widen. IG is a
            // DEALER venue: it publishes its own two-sided price and nothing else, so there is no
            // public trade tape and NO L2 ladder at all. `venue_caps::IG` declares
            // `trades: false`/`book: false`/`depth: false` and the bridge refuses all three
            // through `require_live_verb`. It is also why IG stays DEFERRED in
            // `market_data_conformance.rs` now that it has a feed: that harness's three invariants
            // are all L2-BOOK invariants and there is no book here to hold them on. No amount of
            // wiring changes either fact — only IG publishing depth would.
            //
            // ⚠ Absent credentials REFUSE the live mount (alpaca's argument): every subscription
            // opens its OWN `IgSession` login for streaming credentials, so there is no keyless
            // half to fall back to. DEMO gateway on both planes — `make_engine` resolves
            // `load_ig_config_from(Demo, …)` and the plan resolves the same loader at the same
            // tier, so like oanda this venue's feed and exec agree on the environment by
            // construction. (`Environment::Live` is implemented and called by nothing, which is a
            // trap for `IG_LIVE_*` keys rather than a path — `ig_arming` says so.)
            ig_plan(cfg, vars)?
        }
        "hyperliquid" => {
            // `build_node` hardcodes HL's market as "BTC"; any other symbol would silently never
            // receive its feed (the feed subscribes "BTC"; the mount routes on its own symbol).
            if cfg.token_id != "BTC" {
                return Err(format!(
                    "hyperliquid mount symbol must be \"BTC\" (build_node's hardcoded HL market), got {:?}",
                    cfg.token_id
                ));
            }
            // Safety gate #3 (feed side): the live feed follows the SAME testnet/mainnet gate the exec
            // side reads (`HYPERLIQUID_MAINNET`, the EXACT string `"1"`, process env OR the workspace
            // `.env`, process winning) — both sites fold their reads through the ONE converged rule +
            // hyperliquid's switch row (`vike_bridge_core::mainnet`), so this feed plan and
            // `vike-mount`'s `hl_env` can never disagree on how the flag parses. ⚠ STEP 2 narrowed the
            // grammar: the fuzzy case-insensitive `"true"` spelling this venue used to accept no
            // longer arms anything, so `HYPERLIQUID_MAINNET=true` now plans TESTNET. Default = Testnet.
            //
            // ⚠ The fold itself (the `env::var` read + `vike_bridge_core::mainnet::mainnet_for` call)
            // happens in `main.rs`, NOT here — this file is a library module, and the
            // settings-registry gate (`crates/vike-ops/src/settings.rs`) refuses a library reading
            // process env its caller cannot see, override, or even know about. `hyperliquid_mainnet`
            // is that fold's already-resolved answer, passed in by `main.rs` (this function's one
            // caller).
            VenuePlan::Hyperliquid(if hyperliquid_mainnet {
                vike_hyperliquid::config::Network::Mainnet
            } else {
                vike_hyperliquid::config::Network::Testnet
            })
        }
        #[cfg(feature = "polymarket")]
        "polymarket" => {
            // Polymarket is the A-S maker's NATIVE [0,1] price domain — unlike a $-priced crypto perp,
            // whose $64k mid the maker's avellaneda §2.2 [0,1] wall clamp can't quote. It has NO
            // testnet: every live order is real money on Polygon mainnet, gated by POLY_PRIVATE_KEY
            // creds + POLY_EXEC=1 (checked INSIDE build_node) and the Dublin SOCKS egress (US-geo-
            // blocked otherwise → silent paper fallback). `token_id` shape was validated by
            // `DaemonProfile::validate_for_live`.
            VenuePlan::Polymarket
        }
        v => {
            // The venue list comes from `LIVE_WIRED_VENUES` rather than a hand-written literal:
            // `live_wired_venues_pin.rs` pins that const equal to THIS match's arms, so the message
            // and the arms cannot drift. It used to name 'hyperliquid' inline, which was already a
            // second place to remember when a venue was added.
            return Err(format!(
                "venue '{v}' is not wired for live yet (this build can mount: {}); refusing to mount",
                crate::config::LIVE_WIRED_VENUES.join(", ")
            ));
        }
    };
    Ok(plan)
}

/// The polymarket same-token interval gate (split-plane I10): two mounts on ONE token share one
/// `Feeds` + `MakerSink`, whose `TickBarSynthesizer` runs at exactly ONE window — so rows naming
/// the same token at different intervals would leave the second mount's paper-fill bar lane
/// silently dead. Refused by ROW, before any core spawns.
#[cfg(feature = "polymarket")]
pub fn check_poly_token_intervals(mounts: &[ResolvedMount]) -> Result<(), String> {
    for (j, m) in mounts.iter().enumerate() {
        if m.cfg.venue != "polymarket" {
            continue;
        }
        let earlier = mounts[..j]
            .iter()
            .enumerate()
            .find(|(_, p)| p.cfg.venue == "polymarket" && p.cfg.token_id == m.cfg.token_id);
        if let Some((i, first)) = earlier
            && (first.cfg.interval != m.cfg.interval || first.cfg.interval_ms != m.cfg.interval_ms)
        {
            return Err(format!(
                "mounts[{i}] and mounts[{j}] both trade polymarket token {} but at different \
                     intervals ({}/{} ms vs {}/{} ms): one token has ONE live book feed, and its \
                     synthesized paper-fill bars run at exactly one window — the second interval \
                     would silently never fire. Give both rows one interval, or split the tokens",
                m.cfg.token_id,
                first.cfg.interval,
                first.cfg.interval_ms,
                m.cfg.interval,
                m.cfg.interval_ms,
            ));
        }
    }
    Ok(())
}

/// The FEED-CONSTRUCTION seam: how a [`wire_venue_feeds`] arm OBTAINS its venue's feed object.
/// Extracted so a deterministic test can hand the REAL arm a scripted constructor — closing
/// reason (1) of `crates/vike-tradehub/tests/venue_feed_splice_smoke.rs`'s module doc: the arm
/// used to name the venue constructor inline, the only caller-injectable seam was `wrap` (which
/// wraps the SINK, never the stream), so the splice — the arm's subscribe labels + its sink chain
/// onto the core lanes — was provable only by that live smoke.
///
/// **Production is byte-identical by construction, not by review**: every method's DEFAULT body
/// is exactly the constructor expression the arm spelled inline before this trait existed, and
/// [`ProdFeedCtors`] (the one production impl) overrides nothing — there is no second spelling to
/// drift. A test impl substitutes a method and receives the arm's OWN recorder-teed
/// [`vike_core::CoreLaneSink`] chain plus the arm's own `subscribe_*` calls: everything about the
/// splice except the venue's socket. `src/feed_splice_seam_tests.rs` is that test.
///
/// VENUE-GENERIC BY CONSTRUCTION: one method per venue arm whose feed is held behind the
/// [`DataClient`] seam, each returning `Box<dyn DataClient + Send>` — the same shape whatever the
/// venue. Deribit is the arm routed through it today (the keyless, config-free constructor — the
/// venue the smoke also chose, and for the same reason: nothing to authenticate, nothing to
/// fake). An arm whose constructor takes a resolved config (the alpaca/oanda/ig shape) joins by
/// adding a method that takes the config as a parameter — one more default method, never a second
/// mechanism.
pub trait FeedCtors {
    /// Deribit's feed object — the venue's real four-verb keyless `DataClient`
    /// (`crates/bridges/deribit/src/market_feed.rs`'s `Feeds`), constructed with the daemon's
    /// headless wake, exactly as [`wire_venue_feeds`]' deribit arm always spelled inline.
    fn deribit(&self, sink: Arc<dyn LiveDataSink>) -> Box<dyn DataClient + Send> {
        Box::new(vike_deribit::market_feed::Feeds::new(sink, || {}))
    }

    /// OANDA's feed object — the venue's real two-lane `DataClient`
    /// (`crates/bridges/oanda/src/market_feed.rs`'s `Feeds`: chunked-HTTP pricing stream +
    /// candles REST poll, both Bearer-authed with the plan's resolved PRACTICE session), exactly
    /// as [`wire_venue_feeds`]' oanda arm always spelled inline. The construction is network-free
    /// (each `subscribe_*` spawns the thread that dials), so the seam costs no failure mode.
    ///
    /// This is the CREDENTIALED-data venue routed through the seam: its deterministic splice test
    /// (`src/feed_splice_seam_tests.rs`'s oanda case) substitutes a scripted constructor here AND
    /// asserts the config it received carries the plan's own credentials — the feed half of the
    /// `data_only` declaration's contract.
    fn oanda(
        &self,
        config: &vike_oanda::OandaConfig,
        sink: Arc<dyn LiveDataSink>,
    ) -> Box<dyn DataClient + Send> {
        Box::new(vike_oanda::market_feed::Feeds::new(sink, || {}).with_config(config.clone()))
    }
}

/// Production's constructor set: overrides NOTHING, so every arm constructs exactly what its
/// inline expression always constructed — [`FeedCtors`]' defaults ARE the production code, and
/// this unit struct exists only so [`live_mount`] has a value to pass.
pub struct ProdFeedCtors;

impl FeedCtors for ProdFeedCtors {}

/// Wire ONE venue's live market feed(s) onto the core's ingest lanes — `live_mount`'s feed arms,
/// called once per DISTINCT mounted venue (split-plane I10). `cfgs` is every mount on this venue,
/// in mount order (never empty; the FIRST supplies the announcement fields and the venue-wide
/// knobs, e.g. the CEX pump's book grid); subscriptions dedup per series, so two mounts on one
/// series subscribe once. `make` is the feed-construction seam ([`FeedCtors`]) — production
/// passes [`ProdFeedCtors`], whose defaults construct exactly the inline expressions these arms
/// used to spell; the deterministic splice test substitutes a scripted constructor.
///
/// `data_only` is the profile's declared data-plane-only venue set
/// ([`vike_tradehub::config::DaemonProfile`]'s `data_only` key, collected per venue by
/// [`live_mount`]'s withhold pass) — consulted ONLY by the four credentialed-data arms' arming
/// DISCLOSURE: a declared venue whose exec really stayed paper announces the declaration
/// ([`data_only_arming`]) instead of that venue's "bug to report"/"restart" remedy, which would
/// be false advice about a state the operator asked for. Empty (every undeclared mount) leaves
/// every banner byte-identical.
///
/// `venue_arming` is `vike_run::venue_arming`'s row set — the SAME `arming` binding
/// [`live_mount_with`] journals from, computed after the withhold and before `vars` moves into the
/// `NodeConfig`. It is here for ONE job: [`other_live_accounts`], which needs the rows in order to
/// spell a labelled account's route key through `VenueArming::route_key` rather than parsing one.
/// EMPTY leaves every badge at its per-venue answer, which is why
/// `crates/vike-tradehub/tests/daemon/account_badge_wiring_pin.rs` pins the call site rather than
/// trusting the parameter to be passed.
#[allow(clippy::too_many_arguments)]
pub fn wire_venue_feeds(
    plan: &VenuePlan,
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    data_only: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<LiveFeeds, String> {
    let cfg = cfgs[0];
    let feeds = match plan {
        VenuePlan::Hyperliquid(network) => {
            // The venue-agnostic `CoreLaneSink` (LiveDataSink → core-lane bridge). `subscribe_bars`
            // drives the account-wide margin/drawdown sweeps (per CLOSED bar) + the mark lane;
            // `subscribe_quotes` drives the maker's `on_quote_tick` (the A-S maker prices on
            // quotes/book, NOT on_bar). KEYLESS-public feed — the exec creds gate is separate (inside
            // build_node).
            let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            )) as Arc<dyn LiveDataSink>);
            let mut feeds =
                vike_hyperliquid::market_feed::Feeds::new(sink, || {}).with_network(*network);
            // One BAR subscription per distinct (symbol, interval) across this venue's mounts, one
            // QUOTE subscription per distinct symbol — two mounts on one series subscribe once
            // (split-plane I10; a single mount is one of each, exactly as before).
            let mut bar_keys: Vec<(&str, &str)> = Vec::new();
            let mut quote_keys: Vec<&str> = Vec::new();
            for c in cfgs {
                if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
                    bar_keys.push((&c.token_id, &c.interval));
                    feeds.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                        format!("subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
                    })?;
                }
                if !quote_keys.contains(&c.token_id.as_str()) {
                    quote_keys.push(&c.token_id);
                    feeds
                        .subscribe_quotes(&c.token_id)
                        .map_err(|e| format!("subscribe_quotes {}: {e:?}", c.token_id))?;
                }
            }
            tracing::info!(
                venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                mounts = cfgs.len(),
                network = ?network, live_venues = ?live_venues,
                "LIVE build_node core up; Hyperliquid feed subscribed (bars + quotes)"
            );
            LiveFeeds::Hyperliquid(feeds)
        }
        #[cfg(feature = "polymarket")]
        VenuePlan::Polymarket => {
            // Polymarket's feed serves TICKS, not bars (`subscribe_bars` is Unsupported), so wire it
            // through `MakerSink` (NOT the bare CoreLaneSink): its inner CoreLaneSink forwards the
            // book-derived L1 quote onto the tick/market lanes (drives the maker's on_quote_tick /
            // on_order_book), AND its `TickBarSynthesizer` emits synth bars onto the bar lane (drives
            // the account-wide margin/drawdown sweeps the HL path gets from real bars). One
            // `subscribe_book` drives all of it. Keyless-public, but egresses through the SAME Dublin
            // SOCKS tunnel the exec side uses (US-geo-blocked otherwise).
            // One `Feeds` + `MakerSink` per DISTINCT token (split-plane I10): a `MakerSink` folds
            // EVERY price it sees into ITS token's synth bars, so two tokens through one sink
            // would corrupt both mounts' paper-fill bar lanes — see `LiveFeeds::Polymarket`. Two
            // mounts on ONE token share one entry (their interval agreement was refused-or-proven
            // by `check_poly_token_intervals` before the core spawned).
            let mut list: Vec<vike_polymarket::Feeds> = Vec::new();
            let mut seen: Vec<&str> = Vec::new();
            for c in cfgs {
                if seen.contains(&c.token_id.as_str()) {
                    continue;
                }
                seen.push(&c.token_id);
                let sink = wrap(Arc::new(vike_run::MakerSink::new(
                    handle,
                    c.venue.clone(),
                    c.token_id.clone(),
                    c.interval.clone(),
                    c.interval_ms,
                )) as Arc<dyn LiveDataSink>);
                let mut feeds = vike_polymarket::Feeds::new(sink, || {});
                feeds
                    .subscribe_book(&c.token_id)
                    .map_err(|e| format!("subscribe_book {}: {e:?}", c.token_id))?;
                tracing::info!(
                    venue = %c.venue, symbol = %c.token_id, interval = %c.interval,
                    mounts = cfgs.len(), tokens = seen.len(),
                    live_venues = ?live_venues,
                    "LIVE build_node core up; Polymarket feed subscribed (book → derived L1 quote) — real \
                     orders require POLY_EXEC=1 + POLY_PRIVATE_KEY creds + Dublin egress"
                );
                list.push(feeds);
            }
            LiveFeeds::Polymarket(list)
        }
        VenuePlan::Cex { venue, mainnet } => {
            let slug = venue.slug();
            // ── LANE 1: the KLINE feed, through the venue-agnostic `CoreLaneSink` (recorder-teed by
            // `wrap` exactly like the HL arm, so `record-feeds` covers the bar lane here too). Bars
            // are what give the account-wide margin-call watchdog and the equity-drawdown latch a
            // cadence — both sweep per CLOSED BAR and nowhere else — so this subscription is a
            // SAFETY dependency, not a display one.
            let bar_sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            )) as Arc<dyn LiveDataSink>);
            let mut bars = match venue {
                CexVenue::Binance => {
                    CexBars::Binance(vike_binance::market_feed::Feeds::new(bar_sink, || {}))
                }
                CexVenue::Bybit => {
                    CexBars::Bybit(vike_bybit::market_feed::Feeds::new(bar_sink, || {}))
                }
                CexVenue::Okx => CexBars::Okx(vike_okx::market_feed::Feeds::new(bar_sink, || {})),
                // `with_env(.., Live)` and NOT `new`: aster's kline shell is `Environment`-keyed
                // (its one real delta from the binance template) and `Feeds::new` defaults to
                // TESTNET hosts — while this daemon's aster feed deliberately runs MAINNET, the
                // same choice vike-app's aster mount makes, because exec resolves MAINNET-FIRST
                // from credentials and a paper/demo order must rest against the real book. See
                // `venue_feed_plan`'s aster arm for the whole argument.
                CexVenue::Aster => CexBars::Aster(vike_aster::market_feed::Feeds::with_env(
                    bar_sink,
                    || {},
                    vike_bridge_core::Environment::Live,
                )),
            };
            // One subscription per DISTINCT interval across this venue's mounts (their symbol is
            // the venue's one wired symbol — `cex_plan` refused anything else per mount): two
            // mounts on one series subscribe once, two intervals subscribe twice on the ONE feed.
            let mut intervals: Vec<&str> = Vec::new();
            for c in cfgs {
                if intervals.contains(&c.interval.as_str()) {
                    continue;
                }
                intervals.push(&c.interval);
                match &mut bars {
                    CexBars::Binance(f) => f.subscribe_bars(&c.token_id, &c.interval),
                    CexBars::Bybit(f) => f.subscribe_bars(&c.token_id, &c.interval),
                    CexBars::Okx(f) => f.subscribe_bars(&c.token_id, &c.interval),
                    // `BTCUSDT.P` here: the kline lane's own `split_perp` maps it to the fapi
                    // perp stream while the SERIES keeps the `.P` label `accepts_symbol` needs.
                    CexBars::Aster(f) => f.subscribe_bars(&c.token_id, &c.interval),
                }
                .map_err(|e| {
                    format!("{slug} subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
                })?;
            }

            // ── LANE 2: the QUOTE/TRADE/BOOK pump, straight onto the core tick lane. This is the
            // lane the maker acts on (`on_quote_tick`/`on_order_book` are the only two verbs
            // `SpreadMaker` reaches `requote` from). ⚠ Do NOT "simplify" this into the `Feeds`
            // object above by calling `subscribe_depth`: that lane emits `LiveDataSink::l2_snapshot`,
            // which is a DEFAULT NO-OP in every sink this daemon owns — the mount would look
            // perfectly healthy and post zero orders forever. See `CexTicks`' doc.
            //
            // ⚠ **IT IS ALSO THIS DAEMON'S ONLY LINK-DEATH SIGNAL FOR A CEX VENUE.** The pump
            // discloses its own transport state onto the same tick lane (one `Disconnected` per
            // outage, one `Live` on the first frame back — `crates/bridges/binance/src/family/
            // depth.rs`'s `disclose_link` and its bybit/okx twins), and that is what arms the
            // CONNECTION-state dead-man here: `mount_link_disclosure`'s `VenuePlan::Cex` arm reads
            // exactly this subscription. Drop it, or move the maker to a lane that discloses
            // nothing, and four venues silently stop arming a switch that is ON by default —
            // `crates/vike-tradehub/src/tradehub_cli.rs`'s
            // `the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link` is the gate that
            // refuses to let that happen quietly.
            //
            // `cfg.tick_size` sizes the book's price grid and was proven positive+finite by
            // `cex_plan`; it is the SAME number `MakerMountConfig::crypto` gave the maker, so the
            // pump's grid and the maker's quoting grid cannot disagree.
            let ticks = handle.tick_sender();
            let pump = match venue {
                CexVenue::Binance => {
                    CexTicks::Binance(vike_binance::market_data::spawn_binance_market_data(
                        ticks,
                        &cfg.token_id,
                        cfg.tick_size,
                    ))
                }
                CexVenue::Bybit => {
                    CexTicks::Bybit(vike_bybit::market_data::spawn_bybit_market_data(
                        ticks,
                        &cfg.token_id,
                        cfg.tick_size,
                    ))
                }
                CexVenue::Okx => CexTicks::Okx(vike_okx::market_data::spawn_okx_market_data(
                    ticks,
                    &cfg.token_id,
                    cfg.tick_size,
                )),
                // `Environment::Live` pins the pump to the MAINNET futures hosts (same choice and
                // reason as the kline lane above). The `.P` mount symbol goes in whole: the pump
                // splits it to the exchange's `btcusdt` for the stream/snapshot wire and labels
                // every emitted tick with the caller's own spelling, which is what
                // `drive_strategy_tick` and `accepts_symbol` dispatch on — see
                // `vike_aster::market_data`'s `pump_wire`.
                CexVenue::Aster => {
                    CexTicks::Aster(vike_aster::market_data::spawn_aster_market_data(
                        vike_bridge_core::Environment::Live,
                        ticks,
                        &cfg.token_id,
                        cfg.tick_size,
                    ))
                }
            };

            // ⚠ SAY WHETHER THIS VENUE'S EXEC IS LIVE OR PAPER, IN WORDS. `build_node` mounts the
            // real `ExecutionClient` only when that venue's credentials are present, and the
            // fallback to paper is otherwise indistinguishable from a working live mount: same
            // startup, same feed, same quotes, and orders that go nowhere real. `live_venues` is
            // `build_node`'s own record of which venues got a live client, so this reports what
            // HAPPENED rather than what was configured.
            let exec_live = live_venues.contains(slug);
            // Everything this announcement CLAIMS is resolved by one pure function, so it is
            // testable without a socket and so the two arms below cannot drift apart. `network` and
            // `remedy` both exist because the previous pair got them wrong: it never said which
            // network a LIVE mount was about to trade on, and its PAPER advice named the DEMO key
            // tier unconditionally — advice that arms nothing whenever `{VENUE}_MAINNET=1` is set,
            // which is precisely the state an operator is in while trying to go live. See
            // `CexArming::remedy`.
            let arming = with_other_live_accounts(
                cex_arming(*venue, *mainnet, exec_live),
                slug,
                other_live_accounts(slug, venue_arming, live_venues),
            );
            match &arming.remedy {
                None => tracing::warn!(
                    venue = %slug, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; {slug} feed subscribed (klines + quote/trade/book \
                     pump) — ⚠ EXEC IS LIVE ON {network}: orders from this mount are REAL orders \
                     against the {slug} {network} account whose credentials were found",
                    network = arming.network
                ),
                Some(remedy) => tracing::warn!(
                    venue = %slug, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; {slug} feed subscribed (klines + quote/trade/book \
                     pump) — but EXEC IS PAPER for {slug}: no {network} credentials for it were \
                     found in the settings store, so `build_node` mounted the paper exchange and NO \
                     ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE. To arm it: {remedy}",
                    network = arming.network
                ),
            }
            LiveFeeds::Cex { ticks: Some(pump), bars }
        }
        VenuePlan::Alpaca(config) => {
            // The CREDENTIALED-DATA shape (split-plane I9): ONE authenticated WS client serves
            // every lane through the venue-agnostic `CoreLaneSink` (recorder-teed by `wrap` like
            // the HL arm) — real 1m bar CLOSES drive the margin/drawdown watchdogs (a SAFETY
            // dependency, like the CEX kline lane), quotes drive `on_quote_tick` (alpaca has no
            // book lane — `subscribe_book` is a caps refusal — so the maker's only requote verb
            // here is the quote), and trade prints feed the PriceBoard's last-trade rung.
            //
            // `AlpacaDataClient::new` is INFALLIBLE and network-free: connections open lazily on
            // first subscribe, on their own threads, and reconnect forever — so a wrong secret
            // surfaces as a reconnect-looping feed in the log, never a mount error (mirroring
            // `AlpacaExecutionClient::spawn`, this venue's exec shape). The credentials were
            // resolved by `alpaca_plan` from the SAME map exec reads; absent creds never reach
            // here (the plan refused them).
            let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            )) as Arc<dyn LiveDataSink>);
            let mut client = vike_alpaca::AlpacaDataClient::new((**config).clone(), sink, || {});
            // One BAR subscription per distinct (symbol, interval) across this venue's mounts
            // (interval is "1m" per mount — `alpaca_plan` refused anything else), one QUOTE+TRADE
            // pair per distinct symbol — the HL arm's dedup shape.
            let mut bar_keys: Vec<(&str, &str)> = Vec::new();
            let mut tick_keys: Vec<&str> = Vec::new();
            for c in cfgs {
                if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
                    bar_keys.push((&c.token_id, &c.interval));
                    client.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                        format!("alpaca subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
                    })?;
                }
                if !tick_keys.contains(&c.token_id.as_str()) {
                    tick_keys.push(&c.token_id);
                    client
                        .subscribe_quotes(&c.token_id)
                        .map_err(|e| format!("alpaca subscribe_quotes {}: {e:?}", c.token_id))?;
                    client
                        .subscribe_trades(&c.token_id)
                        .map_err(|e| format!("alpaca subscribe_trades {}: {e:?}", c.token_id))?;
                }
            }
            let exec_live = live_venues.contains("alpaca");
            let arming = with_other_live_accounts(
                if data_only.contains("alpaca") && !exec_live {
                    // The DECLARED exception: paper-by-declaration, not the gate disagreement the
                    // ordinary remedy reports. Guarded on `!exec_live` so a build where the withhold
                    // failed to hold still announces `EXEC IS LIVE` honestly.
                    data_only_arming(alpaca_arming(false), "alpaca")
                } else {
                    alpaca_arming(exec_live)
                },
                "alpaca",
                other_live_accounts("alpaca", venue_arming, live_venues),
            );
            match &arming.remedy {
                None => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; alpaca feed subscribed (1m bars + quotes + trades, \
                     credentialed WS) — ⚠ EXEC IS LIVE ON {network}: orders from this mount are \
                     REAL orders against the alpaca {network} account whose credentials were found",
                    network = arming.network
                ),
                Some(remedy) => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; alpaca feed subscribed (1m bars + quotes + trades, \
                     credentialed WS) — but EXEC IS PAPER for alpaca and NO ORDER FROM THIS MOUNT \
                     WILL EVER REACH THE VENUE: {remedy}",
                ),
            }
            LiveFeeds::Alpaca(client)
        }
        VenuePlan::Ctrader(config) => {
            // The daemon's OWN data socket — `connect_and_auth` (data-only: no `EventSender`),
            // isolated from the exec socket `make_engine`'s ctrader arm opened inside
            // `build_node`, the same one-connection-per-plane rule every inline recon client
            // follows. The sink is a `MakerSink` (the polymarket shape), NOT a bare
            // `CoreLaneSink`: cTrader's live trendbar lane only ever emits FORMING snapshots
            // through this client (`conn::on_inbound` — no close fires), so real venue bars can
            // never drive the per-CLOSED-bar watchdogs; the `MakerSink` synthesizes closed bars
            // from quote mids at the mount interval instead, and every ctrader mount shares its
            // one window (`check_ctrader_intervals` refused disagreement before the core
            // spawned). Trendbars are deliberately NOT subscribed — the sink would no-op their
            // seed/forming verbs anyway, and mixing bid-priced venue history into a mid-priced
            // synth series helps nothing.
            //
            // ⚠ MOUNT-FAILURE SEMANTICS, and they deliberately DIVERGE from exec: this handshake
            // is SYNCHRONOUS, and a connect/auth failure FAILS `live_mount` — the daemon refuses
            // to start. The exec side demotes a failed ctrader connect to paper for the session
            // (a working paper mount is still a mount), but a live daemon whose FEED never
            // connected would hold a core that ticks nothing and watches nothing, the
            // silent-do-nothing this repo refuses to ship. Loud at startup beats dead at 3am.
            let sink = wrap(Arc::new(vike_run::MakerSink::new(
                handle,
                cfg.venue.clone(),
                cfg.token_id.clone(),
                cfg.interval.clone(),
                cfg.interval_ms,
            )) as Arc<dyn LiveDataSink>);
            let conn = vike_ctrader::conn::connect_and_auth(config.to_conn_config(), sink)
                .map_err(|e| {
                    format!(
                        "ctrader DATA connect/auth failed (the dedicated data socket; exec may \
                         separately have demoted to paper inside build_node): {e:?} — the daemon \
                         refuses to start rather than mount a feed-less live core; check \
                         demo.ctraderapi.com reachability and the DEMO token pair, then restart"
                    )
                })?;
            let mut data = vike_ctrader::data::CtraderData::new(conn);
            // One QUOTE subscription per distinct symbol (every ctrader mount was pinned to the
            // one wired symbol by `ctrader_plan`, so this is one subscription today; the loop is
            // the same dedup shape as every other arm). The quote stream is the whole feed: it
            // drives `on_quote_tick` AND the bar synth above.
            let mut quote_keys: Vec<&str> = Vec::new();
            for c in cfgs {
                if !quote_keys.contains(&c.token_id.as_str()) {
                    quote_keys.push(&c.token_id);
                    data.subscribe_quotes(&c.token_id)
                        .map_err(|e| format!("ctrader subscribe_quotes {}: {e:?}", c.token_id))?;
                }
            }
            let exec_live = live_venues.contains("ctrader");
            let arming = with_other_live_accounts(
                if data_only.contains("ctrader") && !exec_live {
                    // Paper-by-declaration — here it REPLACES the "restart to retry the handshake"
                    // remedy, which would be doubly false: no handshake failed, and a restart would
                    // change nothing (the withhold is deliberate and repeats every boot).
                    data_only_arming(ctrader_arming(false), "ctrader")
                } else {
                    ctrader_arming(exec_live)
                },
                "ctrader",
                other_live_accounts("ctrader", venue_arming, live_venues),
            );
            match &arming.remedy {
                None => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; ctrader feed subscribed (spot quotes over the \
                     dedicated data socket; closed bars SYNTHESIZED from quote mids) — ⚠ EXEC IS \
                     LIVE ON {network}: orders from this mount are REAL orders against the \
                     ctrader {network} account whose tokens were found",
                    network = arming.network
                ),
                Some(remedy) => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; ctrader feed subscribed (spot quotes over the \
                     dedicated data socket; closed bars SYNTHESIZED from quote mids) — but EXEC \
                     IS PAPER for ctrader and NO ORDER FROM THIS MOUNT WILL EVER REACH THE \
                     VENUE: {remedy}",
                ),
            }
            LiveFeeds::Ctrader(data)
        }
        VenuePlan::Oanda(config) => {
            // The credentialed-data shape again, but over a client that owns NO socket: quotes
            // arrive on a chunked-HTTP `/pricing/stream` reader thread and bars on a candles-REST
            // POLL thread, both spawned by the client's own `FeedRegistry`. Everything lands on
            // the venue-agnostic `CoreLaneSink` (recorder-teed by `wrap` like every other arm),
            // NOT a `MakerSink`: this venue's poll lane emits REAL `close_bar`s on the lossless
            // lane, so the margin/drawdown watchdogs sweep on the venue's own candle closes and
            // nothing needs synthesizing (the ctrader arm's synth exists only because cTrader
            // never fires a close).
            //
            // ⚠ MOUNT-FAILURE SEMANTICS, and they are ctrader's OPPOSITE — deliberately.
            // `Feeds::with_config` performs NO network work, and each `subscribe_*` only spawns a
            // thread that then dials with its own stop-aware backoff, so a bad token or an
            // unreachable host surfaces as a reconnect-looping feed in the log, never a mount
            // error. That is the alpaca shape and it is the right one here: there is nothing
            // SYNCHRONOUS to fail, so a startup refusal would have to be invented rather than
            // observed. The credential half is already refused up front by `oanda_plan`, which is
            // where the honest gate for this venue lives.
            let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            )) as Arc<dyn LiveDataSink>);
            // Constructed through the [`FeedCtors`] seam (the deribit arm's shape): production
            // is the trait's default — the real `vike_oanda::market_feed::Feeds` over the plan's
            // resolved PRACTICE session, byte-identical to the inline constructor this line used
            // to spell — and the deterministic splice test (`src/feed_splice_seam_tests.rs`)
            // substitutes a scripted constructor HERE, which is what lets it drive this very arm
            // with scripted frames on a weekend the real venue is closed for.
            let mut feeds = make.oanda(config, sink);
            // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
            // each is its OWN poll thread at its own cadence, so unlike ctrader two intervals
            // genuinely both fire — and one QUOTE subscription per distinct symbol. The HL arm's
            // dedup shape; every interval here was proven mappable by `oanda_plan`.
            let mut bar_keys: Vec<(&str, &str)> = Vec::new();
            let mut quote_keys: Vec<&str> = Vec::new();
            for c in cfgs {
                if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
                    bar_keys.push((&c.token_id, &c.interval));
                    feeds.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                        format!("oanda subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
                    })?;
                }
                if !quote_keys.contains(&c.token_id.as_str()) {
                    quote_keys.push(&c.token_id);
                    feeds
                        .subscribe_quotes(&c.token_id)
                        .map_err(|e| format!("oanda subscribe_quotes {}: {e:?}", c.token_id))?;
                }
            }
            let exec_live = live_venues.contains("oanda");
            let arming = with_other_live_accounts(
                if data_only.contains("oanda") && !exec_live {
                    // The DECLARED exception: paper-by-declaration, not the gate disagreement the
                    // ordinary remedy reports. Guarded on `!exec_live` so a build where the withhold
                    // failed to hold still announces `EXEC IS LIVE` honestly.
                    data_only_arming(oanda_arming(false), "oanda")
                } else {
                    oanda_arming(exec_live)
                },
                "oanda",
                other_live_accounts("oanda", venue_arming, live_venues),
            );
            match &arming.remedy {
                None => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; oanda feed subscribed (streamed L1 quotes + POLLED \
                     candle closes, both Bearer-authed) — ⚠ EXEC IS LIVE ON {network}: orders \
                     from this mount are REAL orders against the oanda {network} account whose \
                     token was found",
                    network = arming.network
                ),
                Some(remedy) => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; oanda feed subscribed (streamed L1 quotes + POLLED \
                     candle closes, both Bearer-authed) — but EXEC IS PAPER for oanda and NO \
                     ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
                ),
            }
            LiveFeeds::Oanda(feeds)
        }
        VenuePlan::Deribit => {
            // The KEYLESS shape (the hyperliquid arm's, widened): one `DataClient` onto the
            // venue-agnostic `CoreLaneSink` (recorder-teed by `wrap` like every other arm), no
            // config to thread because nothing here authenticates.
            //
            // FOUR verbs, and each is subscribed because `vike_model::venue_caps::DERIBIT`
            // declares it — never one lane more:
            //   • bars   → REAL venue candle closes (`BarFolder` infers the close from the
            //              successor's first push) on the lossless `close_bar` lane, so the
            //              margin-call watchdog and the 25% drawdown latch sweep on this venue's
            //              own closes. A SAFETY dependency, like the CEX kline lane.
            //   • quotes → the venue-throttled `quote.{inst}` channel, a NATIVE L1 stream behind
            //              the maker's `on_quote_tick`.
            //   • trades → executed prints, feeding the `PriceBoard`'s last-trade rung.
            //   • book   → the `change_id`-chained L2, folded to one standing `L2Book` and pushed
            //              at `LiveDataSink::book`, behind the maker's `on_order_book`.
            // ⚠ `subscribe_depth` is NOT wired and must not be "added for completeness": the caps
            // row declares `depth: false`, so the bridge refuses it through `require_live_verb` —
            // and that refusal is the right one, because the conflating DOM lane emits
            // `LiveDataSink::l2_snapshot`, a DEFAULT NO-OP in every sink this daemon owns (the
            // exact trap `CexTicks`' doc records). The lossless `book` verb above is this venue's
            // L2 surface.
            //
            // ⚠ MOUNT-FAILURE SEMANTICS are alpaca's, not ctrader's: `Feeds::new` does no network
            // work and each `subscribe_*` only spawns a thread that dials with the shared driver's
            // stop-aware backoff, so an unreachable host surfaces as a reconnect-looping feed in
            // the log, never a mount error. Nothing SYNCHRONOUS exists here to fail.
            let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            )) as Arc<dyn LiveDataSink>);
            // Constructed through the [`FeedCtors`] seam: production is the trait's default —
            // the real `vike_deribit::market_feed::Feeds`, byte-identical to the inline
            // constructor this line used to spell — and the deterministic splice test
            // (`src/feed_splice_seam_tests.rs`) substitutes a scripted constructor HERE, which
            // is what lets it drive this very arm with scripted frames.
            let mut feeds = make.deribit(sink);
            // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
            // each is its own chart socket, so two intervals genuinely both fire — and one
            // QUOTE/TRADE/BOOK triple per distinct symbol. The HL arm's dedup shape; every
            // interval here was proven mappable by `deribit_plan`.
            let mut bar_keys: Vec<(&str, &str)> = Vec::new();
            let mut tick_keys: Vec<&str> = Vec::new();
            for c in cfgs {
                if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
                    bar_keys.push((&c.token_id, &c.interval));
                    feeds.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                        format!("deribit subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
                    })?;
                }
                if !tick_keys.contains(&c.token_id.as_str()) {
                    tick_keys.push(&c.token_id);
                    feeds
                        .subscribe_quotes(&c.token_id)
                        .map_err(|e| format!("deribit subscribe_quotes {}: {e:?}", c.token_id))?;
                    feeds
                        .subscribe_trades(&c.token_id)
                        .map_err(|e| format!("deribit subscribe_trades {}: {e:?}", c.token_id))?;
                    feeds
                        .subscribe_book(&c.token_id)
                        .map_err(|e| format!("deribit subscribe_book {}: {e:?}", c.token_id))?;
                }
            }
            let arming = with_other_live_accounts(
                deribit_arming(live_venues.contains("deribit")),
                "deribit",
                other_live_accounts("deribit", venue_arming, live_venues),
            );
            match &arming.remedy {
                None => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; deribit feed subscribed (keyless MAINNET bars + \
                     quotes + trades + L2 book) — ⚠ EXEC IS LIVE ON {network}: orders from this \
                     mount are REAL orders against the deribit {network} account whose \
                     credentials were found, priced off the MAINNET book this feed reads",
                    network = arming.network
                ),
                Some(remedy) => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; deribit feed subscribed (keyless MAINNET bars + \
                     quotes + trades + L2 book) — but EXEC IS PAPER for deribit and NO ORDER FROM \
                     THIS MOUNT WILL EVER REACH THE VENUE. To arm it: {remedy}",
                ),
            }
            LiveFeeds::Deribit(feeds)
        }
        VenuePlan::Ig(config) => {
            // The credentialed-data shape over a THIRD kind of transport: Lightstreamer TLCP text
            // frames. Everything lands on the venue-agnostic `CoreLaneSink` (recorder-teed by
            // `wrap` like every other arm), NOT a `MakerSink`: IG's candle lane emits REAL
            // `close_bar`s on the lossless lane (`CandleFold` closes on `CONS_END` or on a `UTM`
            // advance, whichever arrives first — the second signal exists so a lost frame cannot
            // silently drop a close), so the margin/drawdown watchdogs sweep on the venue's own
            // candle closes and nothing needs synthesizing.
            //
            // ⚠ EXACTLY TWO VERBS, and this is where the arm must not be padded out to look like
            // its neighbours: `subscribe_trades`/`subscribe_book`/`subscribe_depth` are all caps
            // refusals on this venue (`venue_caps::IG` — a dealer publishes no tape and no
            // ladder), so calling one here would turn a structural venue fact into a mount error.
            // The maker requotes on `on_quote_tick` alone; `ig_arming` discloses exactly that.
            //
            // ⚠ MOUNT-FAILURE SEMANTICS are alpaca's, not ctrader's: `Feeds::new` performs NO
            // network work and each `subscribe_*` only spawns a thread that then logs in and dials
            // with the driver's stop-aware backoff, so a wrong password or an unreachable gateway
            // surfaces as a reconnect-looping feed in the log, never a mount error. The credential
            // half is already refused up front by `ig_plan`, which is where the honest gate lives.
            //
            // ⚠ ONE IG REST SESSION PER SUBSCRIPTION — more than the exec side opens, and IG
            // publishes no rate gate this workspace paces against
            // (`vike_model::venue_rate_limits`'s `IG` declares none), so keep the mount set small.
            // Unlike the exec lane, these sessions DO re-login after token expiry.
            let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            )) as Arc<dyn LiveDataSink>);
            let mut feeds = vike_ig::market_feed::Feeds::new(sink, || {}, (**config).clone());
            // One BAR subscription per distinct (epic, interval) across this venue's mounts — each
            // is its own TLCP session, so unlike ctrader two intervals genuinely both fire — and
            // one QUOTE subscription per distinct epic. The HL arm's dedup shape; every interval
            // here was proven streamable by `ig_plan`.
            let mut bar_keys: Vec<(&str, &str)> = Vec::new();
            let mut quote_keys: Vec<&str> = Vec::new();
            for c in cfgs {
                if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
                    bar_keys.push((&c.token_id, &c.interval));
                    feeds.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                        format!("ig subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
                    })?;
                }
                if !quote_keys.contains(&c.token_id.as_str()) {
                    quote_keys.push(&c.token_id);
                    feeds
                        .subscribe_quotes(&c.token_id)
                        .map_err(|e| format!("ig subscribe_quotes {}: {e:?}", c.token_id))?;
                }
            }
            let exec_live = live_venues.contains("ig");
            let arming = with_other_live_accounts(
                if data_only.contains("ig") && !exec_live {
                    // The DECLARED exception — same shape as the alpaca/oanda branches above.
                    data_only_arming(ig_arming(false), "ig")
                } else {
                    ig_arming(exec_live)
                },
                "ig",
                other_live_accounts("ig", venue_arming, live_venues),
            );
            match &arming.remedy {
                None => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; ig feed subscribed (Lightstreamer L1 quotes + \
                     streamed candle closes, both over per-subscription IG logins) — ⚠ EXEC IS \
                     LIVE ON {network}: orders from this mount are REAL orders against the ig \
                     {network} account whose credentials were found",
                    network = arming.network
                ),
                Some(remedy) => tracing::warn!(
                    venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
                    tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
                    mounts = cfgs.len(),
                    requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
                    exec = arming.exec, network = arming.network, live_venues = ?live_venues,
                    "LIVE build_node core up; ig feed subscribed (Lightstreamer L1 quotes + \
                     streamed candle closes, both over per-subscription IG logins) — but EXEC IS \
                     PAPER for ig and NO ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
                ),
            }
            LiveFeeds::Ig(feeds)
        }
    };
    Ok(feeds)
}
