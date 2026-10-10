//! The feed-object types the daemon holds from mount to teardown, and the reconcile health map.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_data::DataClient;

#[cfg(doc)]
use super::{FeedCtors, wire_venue_feeds};
use crate::CexVenue;

/// The CEX quote/trade/book pump handle — the feed that actually drives the maker.
///
/// ⚠ **This is a THIRD feed shape, and the reason it exists is the whole point of the CEX arm.**
/// binance/bybit/okx all declare `live_data.quotes = false` and `live_data.book = false`
/// (`vike_model::venues::venue_caps`), so `DataClient::subscribe_quotes` is a caps-driven refusal and the
/// hyperliquid arm's model cannot be copied. The obvious substitute — `subscribe_depth` — is a
/// SILENT NO-OP: it emits `LiveDataSink::l2_snapshot`, whose body is the trait's default
/// `let _ = (venue, symbol, tick_size, bids, asks, ts);` and which NEITHER sink this daemon owns
/// overrides (`vike_core::CoreLaneSink` says so in its module doc; `vike_mount::MakerSink` never
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
/// ⚠ `StopHandle::shutdown` takes `self` BY VALUE (unlike `DataClient::shutdown(&mut self)`), which
/// is why [`LiveFeeds::Cex`] holds this in an `Option` and `take()`s it — see that variant. Every
/// venue's pump returns the SAME handle, `vike_bridge_core::poller::StopHandle`, so the four arms
/// differ only in which venue they name.
pub enum CexTicks {
    Binance(vike_bridge_core::poller::StopHandle),
    Bybit(vike_bridge_core::poller::StopHandle),
    Okx(vike_bridge_core::poller::StopHandle),
    /// The binance fork's same pump shape (`vike_aster::market_data::spawn_aster_market_data` runs
    /// the shared `family::depth` pump), futures-hosted and `Environment`-keyed — the daemon pins
    /// `Live` (mainnet hosts); see the aster arm in `wire_venue_feeds` for why.
    Aster(vike_bridge_core::poller::StopHandle),
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
    /// health-gates that venue's reconcile pass on. Same `Arc<Mutex<String>>` field
    /// `LiveFeeds::recon_feed_statuses` clones (and `vike-app`'s map of that name did, while the
    /// desktop reconciled).
    pub fn status(&self) -> Arc<std::sync::Mutex<String>> {
        match self {
            CexBars::Binance(f) => Arc::clone(&f.status),
            CexBars::Bybit(f) => Arc::clone(&f.status),
            CexBars::Okx(f) => Arc::clone(&f.status),
            CexBars::Aster(f) => Arc::clone(&f.status),
        }
    }

    /// How many feed threads this venue spawned on the ONE `Arc<Mutex<String>>` [`Self::status`]
    /// hands out — the measurement [`LiveFeeds::recon_feed_statuses`] conditions the CEX row on.
    ///
    /// ⚠ **Read off the venue's own registry rather than re-derived from the plan, deliberately.**
    /// The predicate that spawns a paired mark lane is per-venue (`.P` for binance/bybit/aster,
    /// `-SWAP` for okx, all of them additionally gated on the venue's `mark_streams` row),
    /// so a daemon-side re-implementation would be a fifth copy of four rules and would be wrong
    /// the first time a venue changed one. Counting what was ACTUALLY spawned cannot disagree with
    /// the bridge.
    pub fn status_writer_lanes(&self) -> usize {
        match self {
            CexBars::Binance(f) => f.status_writer_lanes(),
            CexBars::Bybit(f) => f.status_writer_lanes(),
            CexBars::Okx(f) => f.status_writer_lanes(),
            CexBars::Aster(f) => f.status_writer_lanes(),
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

/// **The CEX reconcile-health row's admission rule**: the venue's status handle is unambiguous
/// evidence only while at most ONE lane writes it.
///
/// ⚠ Factored out of [`LiveFeeds::recon_feed_statuses`] so BOTH halves are testable. The `false`
/// half needs a `CexBars` with two live subscriptions, and every public route to one
/// (`subscribe_bars`) spawns a thread that dials a real socket — so the chain is proven in two
/// links instead: this function answers the RULE, and each bridge's own network-free pairing test
/// (`crates/bridges/bybit/src/market_feed_tests.rs`'s
/// `a_perp_bars_subscription_reports_two_status_writer_lanes`) answers the COUNT.
///
/// `<= 1` rather than `== 1`: a handle with no subscription has no writer at all and still holds
/// `Feeds::new`'s `"connecting to …"` seed, which parses `Healthy` — unambiguous for the same
/// reason, and the shape a unit-constructed `CexBars` has.
pub(super) fn cex_status_row_is_unambiguous(status_writer_lanes: usize) -> bool {
    status_writer_lanes <= 1
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
    /// [`vike_mount::MakerSink`] the actor pushes into — the socket itself never emits a bar close.
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
    /// its `Feeds::status` is the same field `vike-app`'s `recon_feed_statuses` gated
    /// binance/bybit/okx on, while the desktop reconciled.
    ///
    /// **The alpaca/ctrader decision (split-plane I9): NO row, deliberately.** Neither
    /// `AlpacaDataClient` nor `CtraderData` exposes a `Feeds::status`-shaped handle on this seam —
    /// each buries its connection health in its own reconnect loop — so there is no evidence-grade
    /// handle to key a row on, and inventing one here would be exactly the blanket sweep the rule
    /// above forbids. This also MATCHES the exec plane's standing classification: both venues
    /// reconcile on the periodic interval only, were absent from vike-app's own
    /// `recon_feed_statuses` map as they are from this one, read `Healthy` and are never
    /// health-blocked (`crates/vike-tradehub/CLAUDE.md`'s health-gate bullet). A future
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
    /// **The bybit/CEX row: KEPT, and CONDITIONALLY — the 2026-09-10 verdict.** This row is the
    /// one that fired in anger: on the CI box it suppressed bybit's reconcile leg 2,516 times over 42
    /// hours while the venue was perfectly healthy, because the venue's feed string had latched an
    /// error no later session could rewrite. The producer is fixed
    /// (`vike_bridge_core::market_pump`'s `SessionStatus`), and the row STAYS — but the objection
    /// that bybit shares the oanda/deribit/ig shape is real and has to be answered rather than
    /// waved past, because bybit's kline, mark and trades lanes all write ONE mutex too.
    ///
    /// **The property bybit shares is not the disqualifying one.** What withholds the other three
    /// is not "N lanes on one string" in the abstract — it is a CONCRETE writer that publishes
    /// `Degraded`-reading text during ORDINARY operation: oanda's candle-POLL retry, deribit's
    /// `book_main` (whose `MdEvent::Resync` is a session fault BY DESIGN, so a healthy book feed
    /// writes an error string as normal operation), ig's weekend FX close. bybit has no such
    /// writer: every string on that mutex comes from a genuine session fault or a genuine session
    /// start. And `reconcile_config::health_from_feed_status`'s write-frequency asymmetry — a
    /// healthy lane speaks once per SESSION, a faulting one once per BACKOFF CYCLE — is what still
    /// disqualifies the other three AFTER the producer fix, not before it.
    ///
    /// **Measured on the DEPLOYED configuration**, bybit has no second writer at all:
    /// [`wire_venue_feeds`]'s CEX arm calls exactly one verb on the object this row keys on
    /// (`subscribe_bars`, once per DISTINCT interval); the tick pump is a separate object
    /// ([`CexTicks`]) that never touches `market_feed::Feeds::status`; the mount is spot
    /// `BTCUSDT`, so `vike_bybit::market_feed`'s `should_pair_mark` is false and no mark thread
    /// spawns. `feed_main` was the SOLE writer — which is exactly why the latch was total and
    /// permanent rather than thrashing.
    ///
    /// ⚠ **But that is a property of the PROFILE, not of the code, and it is one settings line
    /// from becoming ig's shape.** A second interval in the mount spawns a second `feed_main`
    /// through the `intervals` loop; a `.P` symbol spawns `mark_main`. The condition for keeping
    /// this row is therefore THREE things, all of which landed with the producer fix: (1) EVERY
    /// lane that can write that mutex has a `SessionStatus::Live` arm, `mark_main` included —
    /// without it the fix would hand bybit a NEW permanent latch the day a perp is mounted, i.e.
    /// this defect shipped inside its own cure; (2) all of bybit's healthy strings are the
    /// IDENTICAL text (`vike_bybit::market_feed`'s `LIVE_STATUS`), so a multi-lane mount's
    /// alternation is absorbed by `set_status`'s dedup rather than churning the journal; (3) the
    /// single-writer assumption is a GATE, not a reading — `tradehub_cli.rs`'s
    /// `the_cex_arm_subscribes_only_the_bar_verb_on_the_status_bearing_handle`.
    ///
    /// ⚠ **And the row is now CONDITIONED on that count at run time rather than resting on it.**
    /// This paragraph used to end "that the operator's profile names one interval and a spot
    /// symbol stays an OPERATOR FACT, unpinnable from here" — which was true of a SOURCE gate and
    /// false of the mount, because `wire_venue_feeds` holds both inputs by the time this method is
    /// called. So the row is emitted only while [`CexBars::status_writer_lanes`] reports at most
    /// one lane on the handle, read off the venue's own `FeedRegistry` (never re-derived from the
    /// plan — the mark-pairing predicate is per-venue and a daemon-side copy would be a fifth
    /// spelling of four rules). A multi-lane mount is WITHHELD, which is the fail-soft direction
    /// this whole method argues for: an absent row reads `Healthy` and every pass runs, so the
    /// worst case is a wasted fetch rather than a suppressed leg. The deployed configuration —
    /// spot `BTCUSDT`, one interval — keeps its row unchanged.
    ///
    /// `<= 1` rather than `== 1`: a `CexBars` with NO subscription has no writer at all, so its
    /// handle still holds `Feeds::new`'s `"connecting to …"` seed (which parses `Healthy`) and is
    /// unambiguous for the same reason. That is also the shape a unit test constructs.
    ///
    /// ⚠ **One more consequence of the fix, stated because it makes this string strictly LESS
    /// informative as a GUI channel.** The mutex was sticky-Degraded (only errors competed for it);
    /// it is now last-writer-wins in BOTH directions, so on a multi-lane venue a healthy kline lane
    /// can CLEAR a genuine, ongoing mark-lane fault. That is the direction
    /// `health_from_feed_status`'s fail-soft asymmetry argues for — a wasted fetch beats a
    /// suppressed leg — so it is accepted here, and named so a GUI reader is not surprised by it.
    ///
    /// **Why not simply withhold the row.** It would trade a now-curable latch for a PERMANENT
    /// blind spot in a gate that can only SUPPRESS: an absent row reads `Healthy` and every pass
    /// runs. bybit is the one venue actually mounted live on that box and balance reconciliation is
    /// explicitly enabled, so withdrawing the row would have hidden the incident's CAUSE and left
    /// its symptom — the suppressed pass would simply have become an unsuppressed one, with
    /// nothing recording that the evidence had been discarded.
    ///
    /// **The principled cure is the named follow-up, and this fix is what makes it affordable
    /// rather than unnecessary**: a per-LANE status cell would make this row unambiguous BY
    /// CONSTRUCTION and let oanda/deribit/ig earn theirs — a `Feeds`-shape change across six bridge
    /// crates. A second route worth recording beside it:
    /// `crates/bridges/binance/src/family/depth.rs`'s `disclose_link` already pushes typed,
    /// edge-triggered, both-polarity `StreamStatusUpdate`s onto the core tick lane — the right
    /// channel, on the lane the daemon actually subscribes, just not on the seam this gate reads.
    /// Re-keying the gate onto it needs a small per-venue state holder in `vike-core` (today
    /// `Ingest::StreamStatus` hands the transition to a `pub(crate)` `LinkDeadMan` and stores no
    /// queryable map), so it is follow-up scope too — but it is the better long-run answer for
    /// binance/aster, whose recorder builds one `Feeds` over a whole `*USDT.P` family glob and puts
    /// hundreds of threads on one string.
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
                // ⚠ **CONDITIONAL, and the condition is MEASURED rather than assumed.** This row
                // is safe exactly while ONE lane writes the handle it keys on; a `.P` symbol
                // (paired mark lane) or a second interval makes it two, and then the
                // write-frequency asymmetry documented below hands a faulting lane the string
                // ~100% of the time against a healthy sibling. Withholding it there is the
                // fail-soft direction — an absent row reads `Healthy` and every pass runs.
                if cex_status_row_is_unambiguous(bars.status_writer_lanes()) {
                    HashMap::from([(bars.slug().to_string(), bars.status())])
                } else {
                    HashMap::new()
                }
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
/// `crates/vike-mount/src/node.rs`'s forwarder teardown-safety note).
/// Extra teardown run AFTER `feeds.shutdown()` and BEFORE the core join — the recorder's bounded flush
/// when `record-feeds` + `VIKE_TRADEHUB_RECORD=1`, else `None` (byte-identical to no recording).
/// ⚠ Always `None` since #2093 deleted `record-feeds` (2026-09-22): the slot stays so the teardown
/// tail keeps its shape (`crates/vike-tradehub/src/tradehub_cli.rs`'s `post_feeds`). Boxed
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
    /// down in the SEQUENTIAL tail BEFORE the core join (the teardown order vike-app's local core
    /// used): it holds only a WEAK core-ingest sender so it can never wedge the core, but joining it
    /// there bounds its `vt-core-recon` thread to the daemon's lifetime rather than leaking it.
    pub recon_driver: Option<vike_core::ReconDriver>,
}
