//! Feed wiring: which venues get a live market feed, the per-mount venue-feed dispatch
//! (`venue_feed_plan`), and subscribing those feeds onto the core's ingest lanes
//! (`wire_venue_feeds`). `live_runtime` (in `src/tradehub_cli/live_runtime.rs`) calls into this module once per
//! distinct started venue.
//!
//! **Every feed is a dial to the datahub's strategy lane** (wave 2 of `vike-gatewayhub`, decisions 0164 and
//! 0167): this process opens no venue socket and links no bridge. Each venue gets ONE
//! `vike_datahub_client::strat::DataClientStrategyLane` — the tube's receiving engine over a `Scope::Trade` dial — and
//! the arm subscribes the same lanes it always did (bars, quotes, trades, book, the CEX pump as `Ticks`)
//! into the same `CoreLaneSink` or `TickSink`. The datahub holds the venue clients (a key a GUI window or
//! the recorder already holds is subscribed once) and the gatewayhub holds the sockets; the CREDENTIALED
//! venues (alpaca, ctrader, oanda, ig) are credentialed streams at the gatewayhub, which resolves the
//! account itself, so a [`StrategyLanePlan`] carries no config and nothing here reads a venue credential.
//!
//! [`StrategyLanesLive`]/[`StrategyLaneCex`]/[`DataClientTracking`] (the feed-object types) and the [`StrategyLaneFactories`]
//! construction seam + its production impl [`StrategyLaneFactoriesProduction`] live here alongside the functions
//! that name them in their signatures — `reconcile_feed_statuses_of` takes `&[StrategyLanesLive]`,
//! `wire_venue_feeds` returns `Result<StrategyLanesLive, FeedRefusal>` and takes `&dyn StrategyLaneFactories` — for the
//! same reason `StrategyLanePlanCex`/`StrategyLanePlan` moved to `types.rs`: a library crate cannot name a type the
//! binary depending on it defines. The `live_runtime`/`live_runtime_with` of `src/tradehub_cli/live_runtime.rs` (which
//! stay there) and the `tradehub_cli::tests` modules such as `feed_splice` reach these through
//! `use vike_tradehub::feeds::{...}` — `use super::*`/`use super::{...}` inside those then see
//! them as if they were still the binary's own private items.
//!
//! `FEED_ARMS` is the ONE list of the venues this daemon can build a market feed for, and
//! `venue_feed_plan` dispatches on it: a venue has a feed arm exactly when it has a row there.
//!
//! Layout: this file keeps the dispatch (`FEED_ARMS`, `venue_feed_plan`), the polymarket interval gate, the
//! [`StrategyLaneFactories`] seam, the ORCHESTRATOR `wire_venue_feeds` and the price-only table (`PRICE_ONLY_ARMS`); the feed
//! types live in `feeds/live.rs`, the CEX pump's book fold in `feeds/pump_book.rs`, and each `StrategyLanePlan`
//! arm's phase functions in `feeds/<venue>.rs` (`feeds/cex.rs`: `wire_cex_kline_lane`,
//! `spawn_cex_tick_pump`, `announce_cex_arming`); a price-only venue's reading is its `feeds/<venue>.rs`
//! module doc, ctrader's and ig's `feeds/credentialed_price_only.rs`. Every tap-fronted arm starts from the one
//! `dial`, and every `announce_<venue>_arming` logs through the one `warn_arming_banner!`.

/// **The exec-live-or-paper banner a mounted arm logs once** (each `announce_<venue>_arming`): ONE WARN
/// line carrying the fields every arm states, and the sentence the arm worded. A macro, not a function, so
/// each arm's line keeps its own `tracing` target (`vike_tradehub::feeds::<venue>`), which a log filter or
/// an operator's grep may name; defined above the `mod` lines so every arm's file sees it.
macro_rules! warn_arming_banner {
    ($venue:expr, $config:ident, $configs:ident, $arming:ident, $live_venues:ident, $message:expr) => {
        tracing::warn!(
            venue = %$venue, symbol = %$config.symbol, interval = %$config.interval,
            tick_size = $config.tick_size, seed_cash = $config.seed_cash,
            mounts = $configs.len(),
            requote_lanes = $arming.requote_lanes, quote_source = $arming.quote_source,
            exec = $arming.exec, network = $arming.network, live_venues = ?$live_venues,
            "{}", $message
        )
    };
}

mod alpaca;
mod cex;
mod credentialed_price_only;
mod ctrader;
mod deribit;
mod dukascopy;
mod fxcm;
mod hyperliquid;
mod ibkr;
mod ig;
mod live;
mod oanda;
mod polymarket;
pub(crate) mod price_links;
pub(crate) mod price_tap;
mod pump_book;

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, DataClientSink};
use vike_datahub_client::strat::{
    DataClientStrategyLane, StrategyLaneTimings, StreamOpenerStrategyLane,
};
use vike_hub_protocol::auth::HubKeys;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::{AdapterPolicy, ArmingTable, MountPlacement};

#[cfg(doc)]
use crate::venue_arming::arming::{data_only_arming, other_live_accounts};
use crate::venue_arming::tier::{cex_mainnet_enabled, feed_tier};
use crate::venue_plan::{alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan};
use crate::{MountResolved, StrategyLanePlan, StrategyLanePlanCex};
use alpaca::{announce_alpaca_arming, wire_alpaca_feed};
use cex::{announce_cex_arming, spawn_cex_tick_pump, wire_cex_kline_lane};
use ctrader::{announce_ctrader_arming, wire_ctrader_feed};
use deribit::{announce_deribit_arming, wire_deribit_feed};
use hyperliquid::wire_hyperliquid_feed;
use ig::{announce_ig_arming, wire_ig_feed};
pub use live::{
    DataClientTracking, StrategyLaneCex, StrategyLaneCexWatcher, StrategyLaneQuote,
    StrategyLanesLive, StrategyLanesLiveTeardown, StrategyLanesPost, reconcile_feed_statuses_of,
};
use oanda::{announce_oanda_arming, wire_oanda_feed};
use polymarket::wire_polymarket_feeds;
use price_tap::DataClientSinkPriceTap;
pub use price_tap::PriceTap;
use vike_model::VenueNetwork;

/// **A feed's refusal to start, with its CLASS**: what the start decides on (decision 0301: a type, never the
/// words). `feed_gate`'s `survivable_refusal` holds a [`FeedRefusalClass::NotNow`] feed for the late arm,
/// `survivable_cap` keeps a [`FeedRefusalClass::Cap`] venue up and priceless, and a cap that cannot be held ends the
/// start with the status no supervisor restarts on (`feed_gate::failed_start_code`). The class comes from the
/// subscribe's `vike_data::DataClientError` ([`FeedRefusal::subscribe`]), which the stream engine builds from the
/// wire's class (`vike_market_stream::wire::StreamSubscriptionErrorKind`); every other refusal is
/// [`FeedRefusalClass::Final`] (`From<String>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRefusal {
    /// What the start decides on.
    pub class: FeedRefusalClass,
    /// The words the log and the banner carry.
    pub text: String,
}

/// A [`FeedRefusal`]'s class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedRefusalClass {
    /// Nothing started and the same start may pass on the next pass (a link that is down, an ack that never came, a
    /// full budget, a join still in progress): the late arm restarts the tradehub once the price path answers.
    NotNow,
    /// A cap on the price path is full: only an operator mends it, so nothing restarts on it.
    Cap,
    /// Any other refusal (a bad interval, credentials, an unsupported verb): a configuration fault.
    Final,
}

impl FeedRefusal {
    /// `context`'s subscribe refused with `error`, the error's class kept (`{context}: {error:?}` the words).
    pub fn subscribe(context: impl std::fmt::Display, error: &vike_data::DataClientError) -> Self {
        let class = match error {
            vike_data::DataClientError::NotNow(_) => FeedRefusalClass::NotNow,
            vike_data::DataClientError::Cap(_) => FeedRefusalClass::Cap,
            _ => FeedRefusalClass::Final,
        };
        FeedRefusal { class, text: format!("{context}: {error:?}") }
    }
}

/// A refusal that carries no class (a dial that cannot be built, a plan's refusal) is final.
impl From<String> for FeedRefusal {
    fn from(text: String) -> Self {
        FeedRefusal { class: FeedRefusalClass::Final, text }
    }
}

impl std::fmt::Display for FeedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// One feed arm: a venue's per-mount PRE-BUILD plan (see [`venue_feed_plan`]).
type FeedArm =
    fn(&MountPlacement, &ArmingTable, &AdapterPolicy) -> Result<StrategyLanePlan, String>;

/// **THE FEED ARMS** — the venues this daemon's live start can build a market FEED for, each with
/// its plan, in ALPHABETICAL order (the order both refusals list them in). A venue without a row
/// here has no feed, so a profile naming it as its own venue is refused by
/// `crate::config::ProfileDaemon::validate_for_live` and by [`venue_feed_plan`]. It is the ONE list
/// of feed venues: there is no second list to hold equal to it.
///
/// It is a VENUE list, not a `(venue, symbol)` allow-list — the symbol question is a separate,
/// DERIVED check (see `crate::config::ProfileDaemon::validate_for_live`), because "which venues did
/// somebody wire a feed for" and "which symbol does that venue's engine accept" are different facts
/// with different authorities, and conflating them is what made this gate a hand-copied pair table.
///
/// ## ⚠ fxcm is DELIBERATELY absent, and it is the one venue that can never join
///
/// `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` has an fxcm engine row and `assemble_runtime` an fxcm `make_engine` arm (both
/// behind the gatewayhub's `fxcm` feature: `vike_tradehub_core::wired_markets` keeps the row only where the arming table starts it), so an fxcm-armed gatewayhub now starts a real FXCM execution engine and
/// reconcile client. That does NOT make it startable as a PROFILE venue, because this table is about
/// the other half: which venues `venue_feed_plan` can build a market FEED for. `vike-fxcm` has no
/// `DataClient` a mount's feed could ride — `DataClientCapabilities::NONE` in its caps row; its prices are its
/// ForexConnect session's (the `NativeSession` market row of `crates/vike-venues/src/venue_connection.rs`'s `FXCM`),
/// which only the price-only arm reads (`PRICE_ONLY_ARMS`) — so there is no arm to write, and a row here IS a feed arm. A
/// profile naming `venue = "fxcm"` is therefore refused at step (1) of
/// `crate::config::ProfileDaemon::validate_for_live`, which is correct: a start with no prices is
/// a strategy that never gets a tick, and arming it would be arming nothing.
///
/// ⚠ `docs/ops/fxcm-forexconnect.md` used to describe the remaining wiring step as "a row in
/// the wired-market table, a `assemble_runtime` arm, and the matching entry in a feed allow-list". The first two
/// are right; the third is not, and following it would have demanded a feed arm that cannot exist.
/// The engine reached that way is reachable by ROUTING (a `vike_core::MountLeg::at` naming fxcm
/// from a start on a venue that does have prices), not by being the profile's own venue.
#[rustfmt::skip]
pub(crate) const FEED_ARMS: &[(&str, FeedArm)] = &[
    // ⚠ CREDENTIALED MARKET DATA (split-plane I9): alpaca's feed AUTHENTICATES (the OAuth2 sandbox
    // client-credentials trio of the default `demo` account) — there is no keyless price stream to
    // fall back to, so the gatewayhub REFUSES the credentialed stream when the trio is absent and the
    // alpaca arm refuses the live start on it, rather than starting a feed-less core that quotes into
    // the void. Exec stays sandbox-pinned in `crates/bridges/alpaca/src/adapter.rs`'s
    // `VenueAdapterAlpaca` (it arms only a `demo` account; no `ALPACA_MAINNET` flag; a LIVE flip is a
    // deliberate code change, not a config flip).
    ("alpaca", alpaca_arm),
    // ⚠ REAL MONEY IN PRACTICE: aster's exec network is the account's own tier
    // (`VenueAdapterAster` in `crates/bridges/aster/src/adapter.rs`), and only a `live` aster account
    // holds keys in practice — a testnet exists, the constraint is credentials; root CLAUDE.md +
    // `crates/bridges/aster/CLAUDE.md`). This row is CAPABILITY only: absent credentials still
    // start paper, exactly like every other venue.
    ("aster", aster_arm),
    ("binance", binance_arm),
    ("bybit", bybit_arm),
    // ⚠ CREDENTIALED MARKET DATA + a SYNCHRONOUS data handshake (split-plane I9): ctrader's feed
    // is a dedicated protobuf socket at the gatewayhub, the default `demo` account's credentials
    // required (absent creds refuse the live start, same argument as alpaca above), and the
    // gatewayhub's `connect_and_auth` runs AT OPEN — a data connect failure refuses daemon startup,
    // unlike the exec side's session-long demote-to-paper.
    ("ctrader", ctrader_arm),
    // ⚠ KEYLESS MARKET DATA on a venue whose two halves point at DIFFERENT NETWORKS (split-plane
    // I9): deribit's feed is `DataClientWebsocket` over `market_feed::DataClientSpecDeribit`, four `DataClient`
    // verbs off the venue's PUBLIC
    // MAINNET host with no credentials at all — so absent creds start paper here exactly like the
    // CEX venues, never a startup refusal. Exec is the other half: `vike_tradehub_core::make_engine`
    // arms only a `demo` deribit account (`cex_mainnet_enabled` has no deribit arm, so the flag
    // venues' LIVE branch is unreachable) and every authed socket the bridge opens is hardcoded TESTNET
    // (`crates/bridges/deribit/CLAUDE.md`'s two-networks section) — there is no path to deribit
    // MAINNET exec in this tree at all. So a live deribit start is testnet exec over MAINNET
    // prices, the CEX arms' shape reached structurally rather than by choice.
    ("deribit", deribit_arm),
    ("hyperliquid", hyperliquid_arm),
    // ⚠ CREDENTIALED MARKET DATA over Lightstreamer TLCP, and TWO VERBS ONLY (split-plane I9):
    // ig's feed is the data engine over `market_feed::DataClientSpecIg`, which serves
    // `subscribe_quotes` + `subscribe_bars` and refuses trades/book/depth through the declared caps
    // row — IG is a DEALER venue publishing
    // its own two-sided price, so a public tape and an L2 ladder do not exist to be wired. Every
    // subscription logs in with the exec side's own `IgConfig`, so absent credentials refuse the
    // live start (the alpaca/ctrader/oanda argument). Exec is DEMO-gateway-pinned in
    // `crates/bridges/ig/src/adapter.rs`'s `VenueAdapterIg` (it arms only a `demo` account, loading
    // `AccountTier::Demo`; `ig_rest_base` has a live-gateway arm with NO caller in the workspace, so a
    // `live` IG account holding its trio does not run, `LiveTierNotWired` —
    // `crates/bridges/ig/CLAUDE.md` records that trap) — and the feed resolves the same `demo`
    // account at the same tier, so this venue is demo exec over demo prices end to end.
    ("ig", ig_arm),
    // ⚠ CREDENTIALED MARKET DATA on BOTH lanes, and NO market-data WS at all (split-plane I9):
    // oanda's feed is `market_feed::Feeds` on the shared market pump (its `venue_connection` market row) — quotes ride a chunked-HTTP
    // `/pricing/stream` line stream and bars POLL the candles REST endpoint, both Bearer-authed,
    // so absent credentials refuse the live start (the alpaca/ctrader argument). Exec arms only
    // the PRACTICE tier: `crates/bridges/oanda/src/adapter.rs`'s `VenueAdapterOanda` goes through
    // `crates/bridges/oanda/src/config.rs`'s `demo_only_arm`, which arms a `demo` account only and
    // holds a `live` account holding its pair at paper (the fxTrade tier is implemented but has no
    // caller, so a live flip is a deliberate code change). The feed resolves the same default
    // `demo` account at the same practice tier (`crates/vike-tradehub/src/venue_plan.rs`'s
    // `oanda_plan`), so this venue is practice exec over practice prices end to end.
    ("oanda", oanda_arm),
    ("okx", okx_arm),
    ("polymarket", polymarket_arm),
];

/// Whether `venue` has a row in [`FEED_ARMS`].
pub(crate) fn has_feed_arm(venue: &str) -> bool {
    FEED_ARMS.iter().any(|(v, _)| *v == venue)
}

/// **Whether a `paper` account of `venue` is priced and filled from the quotes of the venue's DEMO login** (the
/// owner's ruling of 2026-11-16): a venue that is never a mount (no [`FEED_ARMS`] row, so no bar series ever
/// reaches its paper book) whose price-only arm is the plain quote dial (`PRICE_ONLY_ARMS`): dukascopy, fxcm and
/// ibkr. The gatewayhub serves their quotes off the session an armed demo account already holds (the JForex
/// sidecar, the reconcile session's Offers table, the gateway's one data connection), so the paper engine's own
/// symbol is held on that lane like a demo engine's (`crate::readmit::own_symbol_holds`), the tap folds its quotes
/// into the paper book's bars ([`PriceTap::set_paper_bars`]), and with no demo login the account says so.
pub(crate) fn paper_quotes_through_demo(venue: &str) -> bool {
    !has_feed_arm(venue) && PRICE_ONLY_ARMS.iter().any(|arm| arm.venue == venue)
}

/// **Whether a `paper` account of `venue` can be priced and filled from a HELD quote lane of its own symbol**: every
/// venue this node has a price feed arm for, a mount's ([`FEED_ARMS`]) or a price-only one (`PRICE_ONLY_ARMS`). Its
/// paper book fills a manual order on the venue's quotes (decision 0285's quote clock: a market buy at the CURRENT
/// ask, never a past price; the owner's example 3, 2026-11-16), so an account no strategy mount brings bars to has its
/// own symbol's lane held like a demo engine's (`crate::readmit::holds_own_lane`) instead of being refused.
pub(crate) fn paper_quote_lane(venue: &str) -> bool {
    has_feed_arm(venue) || PRICE_ONLY_ARMS.iter().any(|arm| arm.venue == venue)
}

/// **Whether `venue`'s market feed is KEYLESS**: a [`FEED_ARMS`] venue whose stream the gatewayhub serves with no account
/// login (not one of its credentialed streams, `vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES`):
/// binance, bybit, okx, aster, hyperliquid, deribit and polymarket today. A paper-only node wires exactly these
/// (`crate::tradehub_cli`'s `paper_feeds`: the owner's "3 - yes!", 2026-11-16), since nothing there logs in.
pub(crate) fn keyless_feed(venue: &str) -> bool {
    has_feed_arm(venue)
        && !vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES.contains(&venue)
}

/// **Whether a profile may declare `data_only = true` for `venue`**: a [`FEED_ARMS`] venue whose stream the gatewayhub
/// logs in as an account (one of its credentialed streams, the twin of [`keyless_feed`]): alpaca, ctrader, ig and oanda
/// today, exactly the [`crate::StrategyLanePlan`] variants that CARRY a resolved config. The declaration exists for these
/// venues alone because only here does the workspace live gate ("absent credentials ⇒ paper") fail to provide a
/// data-only mount: their feed AUTHENTICATES with the SAME credentials `vike_tradehub_core::make_engine`'s exec arm
/// reads, so a store that lets the feed mount also arms real demo exec. `data_only = true` is the declared exception —
/// `live_runtime` WITHHOLDS that venue's credentials from the exec mount (the feed keeps them; its plan resolved first),
/// so no venue client is armed while the credentialed feed streams: a `paper` account runs the paper simulator, and a
/// `demo` or `live` one does NOT RUN (decision 0156).
///
/// A KEYLESS-data venue is refused the declaration on purpose, not spared the implementation: its data-only mount
/// already exists — withhold the venue's credentials from the store — and a declaration there could only re-state the
/// live gate while making the arming disclosure (`cex_arming`'s "add {keys}" remedy) advise arming a mount the profile
/// just said not to arm.
pub(crate) fn data_only_feed(venue: &str) -> bool {
    has_feed_arm(venue)
        && vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES.contains(&venue)
}

/// The [`data_only_feed`] venues, in the credentialed roster's order: the list the `data_only` refusal prints.
pub(crate) fn data_only_venues() -> impl Iterator<Item = &'static str> {
    vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES
        .into_iter()
        .filter(|v| has_feed_arm(v))
}

/// The [`FEED_ARMS`] venues joined by `", "` — the list both refusals print.
pub(crate) fn feed_arm_list() -> String {
    FEED_ARMS.iter().map(|(v, _)| *v).collect::<Vec<_>>().join(", ")
}

/// The per-venue PRE-BUILD gate: `live_runtime`'s venue dispatch, called once per START (split-plane
/// I10 made it a function — `live_runtime` loops it over the start set, so every start passes its
/// own venue's refusals, and keeps ONE plan per distinct venue). A venue without a `FEED_ARMS`
/// row is not live-wired.
///
/// `policy` is the start's OWN [`AdapterPolicy`] — the `account` table (each account's tier and
/// `active` switch, and each account's credentials by field) and the `venue_setting` snapshot —
/// which `live_runtime_with` builds ahead of its plan loop and later moves into the `RuntimeConfig` the
/// start is built from. The four credentialed-data arms read NOTHING from it any more: the feed is a
/// credentialed stream at the gatewayhub, which resolves the venue's `demo` account itself, so no
/// credential, label or tier is resolved here. `table` is the arming snapshot of the same assembly (the
/// one backend answer the start is built from), and carries no account's credential. Which NETWORK a venue's
/// accounts start on is not read off the account rows here: for binance/bybit/okx/aster/hyperliquid
/// it is asked of the venue's registry row (`crate::venue_arming::tier::feed_tier`,
/// `cex_mainnet_enabled`), whose `resolve` turns each account's own tier and keys into a tier — the
/// same function, over the same policy, that the start acts on (decision 0095 made the tier the
/// network for the first three and hyperliquid; decision 0119 made it the ACCOUNT's own tier).
pub fn venue_feed_plan(
    config: &MountPlacement,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    match FEED_ARMS.iter().find(|(venue, _)| *venue == config.venue) {
        Some((_, arm)) => {
            default_mount_refusal(config, policy)?;
            arm(config, table, policy)
        }
        None => Err(format!(
            "venue '{}' is not wired for live yet (this build can mount: {}); refusing to mount",
            config.venue,
            feed_arm_list()
        )),
    }
}

/// **The one symbol refusal every feed plan makes, before its own**: a mount must name the symbol the venue's
/// DEFAULT account is mounted on, the symbol that account's row STATES (`account.symbol`, read through
/// `vike_config::account_arming::account_symbol`; owner, 2026-11-13: no symbol is compiled in).
/// `vike_tradehub_core::make_engine` sets no `extra_symbols`, so [`vike_execution::Engine::accepts_symbol`] is a plain
/// equality test and a foreign symbol's orders AND fills are dropped with no log. Nothing to refuse on an
/// account-wide venue (polymarket mounts no symbol) or where the default account states none (it does not run; a
/// mount naming it is refused by the node, by name).
fn default_mount_refusal(cfg: &MountPlacement, policy: &AdapterPolicy) -> Result<(), String> {
    if !vike_venues::venue_wiring::mounts_a_symbol(&cfg.venue) {
        return Ok(());
    }
    let stated = vike_config::account_arming::account_symbol(
        Some(&policy.accounts),
        &cfg.venue,
        &vike_model::accounts::account_keys::AccountLabel::Default,
    );
    match stated {
        Some(mounted) if mounted != cfg.symbol => Err(format!(
            "{}'s default account is mounted on {mounted:?} (the symbol its row states, `account.symbol`), \
             but this mount names {:?}: that engine accepts exactly its mounted symbol \
             (`Engine::accepts_symbol` — no `extra_symbols` are wired), so every order and fill would \
             be SILENTLY DROPPED",
            cfg.venue, cfg.symbol
        )),
        _ => Ok(()),
    }
}

fn binance_arm(
    config: &MountPlacement,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // ── The CEX venues. One shared gate (`cex_plan`), because they share one feed shape.
    //
    // ⚠ Their market DATA is MAINNET on all of them, while exec follows each venue's own
    // demo/mainnet credential gate inside `assemble_runtime`. On binance/bybit/okx that split is
    // STRUCTURAL, not an oversight: each pump's hosts are its venue's `live` `data` `venue_url` rows,
    // one set per venue whatever the account's tier (a demo `data` ask falls back to the live row by
    // the lookup's rule), and bybit's demo environment serves no public market WS at all. So a demo
    // start is DEMO EXEC OVER MAINNET
    // PRICES. That is the intended shape — a demo order must rest against the real book to mean
    // anything — but it is stated here so nobody later "fixes" the feed to follow the exec
    // environment and silently points a live account at a dead host. (Aster reaches the same
    // shape by CHOICE rather than structure — its arm below says how and why.)
    //
    // SPOT lane (no `.P`: the default account's row states a spot symbol), and
    // `spawn_binance_feed_tick` is the spot `@bookTicker`+`@trade`+`@depth` combined
    // stream, so the maker prices on `on_quote_tick` off a NATIVE L1 channel. (Every CEX
    // venue here drives `on_quote_tick`; only the quote's PROVENANCE differs — see
    // `StrategyLanePlanCex::quote_source`.)
    cex_plan(
        StrategyLanePlanCex::Binance,
        config,
        cex_mainnet_enabled(StrategyLanePlanCex::Binance, table, policy),
    )
}

fn bybit_arm(
    config: &MountPlacement,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // LINEAR PERP. The mount's symbol is its USER name (`BTCUSDT.P`, decision 0290), and both lanes read it by its
    // kept row: `spawn_bybit_feed_tick` dials the public `/v5/public/linear` socket and the kline lane asks the
    // linear candles (`vike_bybit::history::live_route`), each under the venue's `BTCUSDT`, and every series is
    // labelled with the engine's own `BTCUSDT.P`. So the bar close on `PriceBoard::resolve`'s last rung is the
    // perp's own (BYB-2: under the old exec spelling the bare `BTCUSDT` reached the SPOT candles here).
    cex_plan(
        StrategyLanePlanCex::Bybit,
        config,
        cex_mainnet_enabled(StrategyLanePlanCex::Bybit, table, policy),
    )
}

fn okx_arm(
    config: &MountPlacement,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // SWAP (`-SWAP`). `spawn_okx_feed_tick` subscribes `bbo-tbt` + `trades` +
    // `books5` — like binance, a NATIVE L1 channel behind `on_quote_tick`.
    //
    // ⚠ okx's grid is denominated in CONTRACTS while the engine's qty is BASE, and
    // `RiskLimits::from_properties` copies `min_qty`/`step_size` across verbatim. On
    // `BTC-USDT-SWAP` (`ctVal` 0.01, `lotSz` 0.01) that makes the daemon's effective floor
    // 0.01 BTC against a venue floor of 0.0001 BTC — 100x CONSERVATIVE, i.e. it fails safe.
    // Stated because the same confusion fails OPEN if one side is ever "corrected" alone:
    // `perp.rs`'s `to_contracts` truncates any base qty below 0.0001 BTC to `sz=0`, and the
    // only thing preventing that from reaching the wire today is the over-strict min_qty.
    cex_plan(
        StrategyLanePlanCex::Okx,
        config,
        cex_mainnet_enabled(StrategyLanePlanCex::Okx, table, policy),
    )
}

fn aster_arm(
    config: &MountPlacement,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // USDⓈ-M PERP, vike's `.P` spelling (the account's row states it; the pump
    // and the kline lane each split it to the exchange's `BTCUSDT` for the wire while the
    // series label keeps the `.P`, so `accepts_symbol` and `drive_strategy_tick` both see
    // the started spelling). A binance fork: `spawn_aster_feed_tick` is the same
    // `@bookTicker`+`@trade`+`@depth@100ms` combined stream on the futures host — a
    // NATIVE L1 channel behind `on_quote_tick`, like binance.
    //
    // ⚠ Feed hosts are tier-keyed on this venue (unlike the three above, whose
    // pumps have exactly one host each) — a testnet EXISTS; the daemon still PINS the
    // feed to `AccountTier::Live` (mainnet) in `wire_venue_feeds`, deliberately: exec
    // follows the account's own tier (`VenueAdapterAster`, no `ASTER_MAINNET` flag
    // exists), only `live` aster accounts are configured in practice, and a
    // demo/paper order must rest against the real book to mean anything — the same
    // demo-exec-over-mainnet-prices shape as the three venues above, reached by choice
    // rather than by structure. vike-app's aster start made the identical choice for the
    // identical reason (its `Feeds::with_env(.., Live)` beside the mainnet-always
    // `CatalogProviderAster`), while the desktop had one.
    //
    // ⚠⚠ REAL MONEY IN PRACTICE: a `live` aster account holding its credentials makes
    // this a LIVE MAINNET exec start. This arm is CAPABILITY only — absent credentials
    // still start paper (the workspace live gate), and nothing here changes any default.
    cex_plan(
        StrategyLanePlanCex::Aster,
        config,
        cex_mainnet_enabled(StrategyLanePlanCex::Aster, table, policy),
    )
}

fn alpaca_arm(
    config: &MountPlacement,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // US EQUITY (the account's row states the stock), the first CREDENTIALED-DATA arm
    // (split-plane I9): NOT the CEX two-lane shape. There is no keyless pump —
    // alpaca's data client (`DataClientWebsocket` over `DataClientSpecAlpaca`) is one multiplexed,
    // OAuth-authenticated WS client serving real 1m
    // bar CLOSES (the watchdog cadence), L1 quotes (a native channel behind
    // `on_quote_tick`) and trade prints, all through `CoreLaneSink`. The plan carries nothing:
    // the gatewayhub resolves the account, and ABSENT credentials REFUSE the credentialed stream
    // (the arm refuses the start on it) rather than degrading:
    // exec-degrades-to-paper still leaves a working start, but a feed-less start quotes
    // into the void forever. Data hosts follow the SAME tier the exec side is pinned to
    // (the sandbox — the bridge arms only a `demo` account, and no `ALPACA_MAINNET` flag
    // exists), so unlike the CEX arms this venue's demo start is DEMO EXEC OVER SANDBOX
    // PRICES — the venue serves a sandbox data stream, which the CEX venues do not.
    alpaca_plan(config)
}

fn ctrader_arm(
    config: &MountPlacement,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // FX (the account's row states the pair), the second credentialed-data arm
    // (split-plane I9): one DEDICATED protobuf/TLS data socket (`connect_and_auth`,
    // SYNCHRONOUS at start — wire failure refuses startup, see `wire_venue_feeds`),
    // isolated from the exec socket `make_engine` opens, per the same isolation rule every
    // recon client follows. Serves L1 spot quotes, from which the arm synthesizes the mount's
    // closed bars via `TickSink` — the polymarket shape (the venue's trendbars are bid-priced,
    // and reach the core only on a series the bar keeper opens) — and the DEMO host follows the
    // exec side's own DEMO-tier pin. Absent credentials refuse the start (same argument as alpaca
    // above).
    ctrader_plan(config)
}

fn oanda_arm(
    config: &MountPlacement,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // FX (the account's row states the pair; `vike_catalog::to_oanda_instrument` splits it to the
    // venue's own `BASE_QUOTE` on the wire while every emitted series keeps the started
    // spelling `accepts_symbol` needs), the third credentialed-data arm (split-plane I9)
    // and the only live-wired venue with NO market-data socket of any kind: this venue
    // publishes no market-data WS, so its feed is the market pump over a transport of its own. ONE
    // `DataClient` over TWO unlike transports — quotes off the chunked-HTTP
    // `/pricing/stream` line stream, bars POLLED off the candles REST endpoint — both
    // Bearer-authed against the same account, which is why absent credentials REFUSE the
    // start here (alpaca's argument: exec-degrades-to-paper still leaves a working start,
    // a feed-less start quotes into the void).
    //
    // ⚠ Unlike ctrader, the bars are REAL venue candles on the LOSSLESS `close_bar` lane,
    // so the per-CLOSED-bar watchdogs run off the venue's own closes rather than a
    // synthesizer — hence the alpaca-shaped `CoreLaneSink` wiring in `wire_venue_feeds`,
    // and hence no same-venue interval gate (see `oanda_plan`).
    //
    // ⚠ PRACTICE hosts on BOTH planes, and this is the one venue where feed and exec
    // agree by construction rather than by choice: `make_engine` arms only a `demo`
    // account, through `load_oanda_config_from(Demo, …)` (no `OANDA_MAINNET` flag), the
    // plan resolves the SAME loader at the SAME tier over the SAME account, and
    // `oanda_hosts(Demo)` supplies both bases inside the one `OandaConfig` the plan
    // carries. So this is practice exec over PRACTICE prices — NOT the
    // demo-exec-over-mainnet-prices shape the CEX arms above describe, because this venue
    // actually serves a practice price stream.
    oanda_plan(config)
}

fn deribit_arm(
    config: &MountPlacement,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // The OPTIONS venue's inverse perp (the account's row states it; deribit
    // instrument names are already unambiguous, so there is no `.P` split and the wire
    // symbol IS the series symbol). Split-plane I9 gave it a real `DataClient` where
    // its pump row used to say it had none, and it is the WIDEST one this daemon starts: FOUR
    // verbs — bars, quotes, trades AND the lossless L2 book — off one keyless public
    // socket per subscription. So this is neither the CEX two-lane shape (no separate
    // `spawn_*_market_data` pump exists, and none is needed: `subscribe_book` emits
    // `DataClientSink::book`, which `CoreLaneSink` really forwards) nor the
    // credentialed-data shape (nothing here authenticates).
    //
    // ⚠ NO CREDENTIAL REFUSAL, deliberately — the one place this arm must NOT be copied
    // from its alpaca/ctrader/oanda neighbours. The feed is keyless, so absent credentials
    // leave a working feed over a PAPER exec book: the workspace live gate, exactly as on
    // a CEX venue. See `deribit_plan`.
    //
    // ⚠⚠ THE TWO HALVES OF THIS VENUE POINT AT DIFFERENT NETWORKS, and unlike the CEX
    // arms above that is STRUCTURAL rather than a choice this daemon makes: every public
    // read in the bridge asks the `live` (mainnet `www.deribit.com`) rows and every authed socket
    // the `demo` (`test.deribit.com`) rows whatever the tier (`crates/bridges/deribit/CLAUDE.md`).
    // There is no flag and no credential tier that reaches deribit mainnet execution — no exec
    // site asks a `live` row. So a live deribit start is TESTNET
    // exec over MAINNET prices, which `deribit_arming` discloses in words.
    deribit_plan(config)
}

fn ig_arm(
    config: &MountPlacement,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // FX CFD (the account's row states the EPIC, which is IG's own
    // instrument identifier and travels verbatim on both planes: the exec POST body and
    // the Lightstreamer `PRICE:`/`CHART:` item names alike, so there is no split of any
    // kind here). The fourth credentialed-data arm (split-plane I9) and the only venue
    // whose feed is not JSON-over-WS — Lightstreamer TLCP 2.1.0 text frames on the shared
    // tungstenite stack, the shared market pump with `subscribe_frame: false` (its `venue_connection` market row)
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
    // ⚠ Absent credentials REFUSE the live start (alpaca's argument): every subscription
    // opens its OWN `IgSession` login for streaming credentials, so there is no keyless
    // half to fall back to. DEMO gateway on both planes — `make_engine` arms only a
    // `demo` account, through `load_ig_config_from(Demo, …)`, and the plan resolves the
    // same loader at the same tier over the same account, so like oanda this venue's feed
    // and exec agree on the environment by construction. (`ig_rest_base(Live)` is
    // implemented and called by nothing: a `live` IG account holding its trio starts
    // PAPER, `LiveTierNotWired` — `ig_arming` says so.)
    ig_plan(config)
}

fn hyperliquid_arm(
    config: &MountPlacement,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // The symbol: the default account's (`default_mount_refusal`, before this arm), and on the venue's
    // kept list (`crate::config::ProfileDaemon::validate_for_live`; owner, 2026-11-13: the `"BTC"` pin went).
    // Safety gate #3 (feed side): the feed dials the network the venue's ARMED accounts'
    // exec dials, and that is asked of the venue's REGISTRY ROW (`feed_tier` — the
    // bridge's own `resolve` over each account's own tier and keys), not re-derived from
    // an account row. It is ONE feed per venue, not per account, so: every armed account
    // on one network -> that network; none armed -> the default account's selected tier
    // (an active `live` row with no key is still the mainnet feed); armed accounts on BOTH
    // networks -> MAINNET WINS and `feed_tier` says so at `warn!`. What that guarantees is
    // that the feed is never on testnet while an armed account is on mainnet (real orders
    // priced off the testnet book); it does NOT promise every armed account shares the
    // feed's network — a testnet account beside a live one is priced off the mainnet book,
    // and the warning is how the operator hears it. The start is never refused over it.
    Ok(StrategyLanePlan::Hyperliquid(VenueNetwork::for_tier(feed_tier(
        &config.venue,
        table,
        policy,
    ))))
}

fn polymarket_arm(
    _config: &MountPlacement,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<StrategyLanePlan, String> {
    // Polymarket is the A-S maker's NATIVE [0,1] price domain — unlike a $-priced crypto perp,
    // whose $64k mid the maker's avellaneda §2.2 [0,1] wall clamp can't quote. It has NO
    // testnet: every live order is real money on Polygon mainnet, gated by the account's
    // PRIVATE_KEY + flags.poly_exec (checked INSIDE assemble_runtime) and the Dublin SOCKS egress (US-geo-
    // blocked otherwise: the start refuses, and the account does not run). The token id's shape was validated by
    // `ProfileDaemon::validate_for_live`.
    Ok(StrategyLanePlan::Polymarket)
}

/// The polymarket same-token interval gate (split-plane I10): two starts on ONE token share one
/// feed + `TickSink`, whose `TickBarSynthesizer` runs at exactly ONE window — so rows naming
/// the same token at different intervals would leave the second start's paper-fill bar lane
/// silently dead. Refused by ROW, before any core spawns.
pub fn check_polymarket_token_intervals(mounts: &[MountResolved]) -> Result<(), String> {
    for (j, m) in mounts.iter().enumerate() {
        if m.spec.venue != "polymarket" {
            continue;
        }
        let earlier = mounts[..j]
            .iter()
            .enumerate()
            .find(|(_, p)| p.spec.venue == "polymarket" && p.spec.symbol == m.spec.symbol);
        if let Some((i, first)) = earlier
            && (first.spec.interval != m.spec.interval
                || first.spec.interval_ms != m.spec.interval_ms)
        {
            return Err(format!(
                "mounts[{i}] and mounts[{j}] both trade polymarket token {} but at different \
                     intervals ({}/{} ms vs {}/{} ms): one token has ONE live book feed, and its \
                     synthesized paper-fill bars run at exactly one window — the second interval \
                     would silently never fire. Give both rows one interval, or split the tokens",
                m.spec.symbol,
                first.spec.interval,
                first.spec.interval_ms,
                m.spec.interval,
                m.spec.interval_ms,
            ));
        }
    }
    Ok(())
}

/// The FEED-CONSTRUCTION seam: how a [`wire_venue_feeds`] arm OBTAINS its venue's feed object.
/// Extracted so a deterministic test can hand the REAL arm a scripted constructor — closing
/// reason (1) of `crates/vike-tradehub/tests/daemon_spawn/venue_feed_splice_smoke.rs`'s module doc: the arm
/// used to name the venue constructor inline, the only caller-injectable seam was `wrap` (which
/// wraps the SINK, never the stream), so the splice — the arm's subscribe labels + its sink chain
/// onto the core lanes — was provable only by that live smoke.
///
/// **Production is the one dial, not a table of constructors**: every feed is a
/// [`DataClientStrategyLane`] (the datahub's strategy lane, one per venue and network), so the seam has ONE required
/// method, [`StrategyLaneFactories::strat`], and [`StrategyLaneFactories::client`] — the shape every non-pump arm holds, a
/// [`DataClient`] — defaults to it. A test impl substitutes `client` (a scripted double per venue) and
/// receives the arm's own `wrap`-ed sink chain plus the arm's own `subscribe_*` calls: everything about
/// the splice except the venue's socket. `src/tradehub_cli/tests/feed_splice.rs` is that test. The CEX
/// arm alone needs the concrete [`DataClientStrategyLane`] (the tick pump is an inherent verb, not a `DataClient`
/// one), so it calls `strat` directly.
pub trait StrategyLaneFactories {
    /// The strategy-lane dial for `venue` (on `network`: Hyperliquid's test network only; `None` is the
    /// venue's main network), delivering into `sink`. Construction never dials (the engine's reader
    /// does), so a datahub that is down is a link that is down, not a failed constructor; the `Err` is
    /// a dial that cannot even be attempted (no strategy-lane key) or an OS refusing a thread.
    fn strat(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<DataClientStrategyLane, String>;

    /// The feed object an arm that holds a [`DataClient`] subscribes on: the strategy-lane dial.
    fn client(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        Ok(Box::new(self.strat(venue, network, sink)?))
    }

    /// **Can the prices be had at all right now?** The probe `live_runtime_with` asks BEFORE a core exists
    /// (the module doc of `feed_gate.rs` says why it must be before): `None` for a constructor with nothing
    /// to ask (a scripted one), `Some` for the production dial. `venues` are the mounted venues the feeds will be
    /// wired for: the production probe asks whether the datahub serves a strategy lane for each. A probe that
    /// says "no" leaves every `demo`/`live` account of a mounted venue NOT RUNNING with the reason, and the late
    /// arm asks it again.
    fn datahub_probe(&self, venues: &[String]) -> Option<DatahubProbe> {
        let _ = venues;
        None
    }

    /// **The dials this constructor made, for an operator's reading of their links**: `None` for a constructor that
    /// makes no real dial (a scripted one).
    fn price_links(&self) -> Option<Arc<price_links::PriceLinks>> {
        None
    }

    /// **Is the process stopping?** `false` for a constructor with no stop flag (a scripted one). Production's
    /// is the process's one stop flag (`vike_boot::stop`): a wait an arm would otherwise sit out
    /// ([`StrategyLaneFactories::pause`]) ends when it is raised, so a `systemctl stop` during a start is not made to wait.
    fn stopping(&self) -> bool {
        false
    }

    /// **Wait `d` before an arm asks again** (OANDA's transient quote refusal, `feeds/oanda.rs`'s
    /// `wire_oanda_feed`), in slices that look at [`StrategyLaneFactories::stopping`]; `true` when the process is stopping,
    /// which ends the wait early and the retry with it. A scripted constructor overrides this to wait for
    /// nothing.
    fn pause(&self, d: std::time::Duration) -> bool {
        const SLICE: std::time::Duration = std::time::Duration::from_millis(50);
        let end = std::time::Instant::now() + d;
        loop {
            if self.stopping() {
                return true;
            }
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            std::thread::sleep(left.min(SLICE));
        }
    }
}

/// **The question "does the datahub's strategy lane answer?"**, askable more than once and from another
/// thread (the late arm's): `check` is `None` when it does, `Some(why)` when it does not, the words an
/// operator reads in the NOT RUNNING line.
///
/// Production's is [`StrategyLaneFactoriesProduction::datahub_probe`]: no strategy-lane key is `Some(the by-name refusal)`
/// without dialling anything, otherwise one dial, the authenticated handshake and a read of the features the
/// datahub advertised (a strategy lane, and a `strategy_venue=` entry for every mounted venue; no lane is opened,
/// so it costs no connection slot). A test's is a closure.
///
/// ⚠ It answers what the DATAHUB advertises, not what is behind it: a datahub that is up while ITS gatewayhub is
/// away still passes, and the first subscribe is refused NOT NOW ([`FeedRefusalClass::NotNow`]), which
/// `feed_gate::survivable_refusal` turns into a held feed when no `demo`/`live` account of the venue runs.
#[derive(Clone)]
pub struct DatahubProbe(Arc<dyn Fn() -> Option<FeedsDown> + Send + Sync>);

impl DatahubProbe {
    /// A probe answering with `check`.
    pub fn new(check: impl Fn() -> Option<FeedsDown> + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }

    /// `None` when the datahub's strategy lane can be dialled, else why not.
    #[must_use]
    pub fn check(&self) -> Option<FeedsDown> {
        (self.0)()
    }
}

/// **Why the price feeds are unavailable**: the [`FeedsDownCause`] the ready banner and the log name it by
/// ([`short_cause`]; decision 0301: chosen by its type where the failure is met, never found in the words) and the
/// `text` the log and the NOT RUNNING line carry (its `Display`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedsDown {
    /// What the banner names.
    pub cause: FeedsDownCause,
    /// The words.
    pub text: String,
}

/// A [`FeedsDown`]'s cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedsDownCause {
    /// The price path (the datahub's strategy lane, or the gatewayhub behind it) does not answer.
    NotAnswering,
    /// The hub-key store holds no strategy-lane key: only minting it mends it (the late arm re-reads the store).
    MissingKey,
    /// A cap on the price path is full ([`FeedRefusalClass::Cap`]); `detail` names the venue and the refusal.
    Cap { detail: String },
    /// The late arm's restart budget is spent (`crate::late_arm::LateArmPlan::within_budget`).
    RestartsSpent,
}

impl FeedsDown {
    /// A venue's feed refused by a CAP: `detail` is `{venue}: {refusal}`, the words lead with [`FEEDS_CAPPED_MARK`].
    #[must_use]
    pub fn cap(detail: String) -> Self {
        FeedsDown {
            text: format!("{FEEDS_CAPPED_MARK}: {detail}"),
            cause: FeedsDownCause::Cap { detail },
        }
    }
}

/// A refusal that names no other cause: the price path does not answer.
impl From<String> for FeedsDown {
    fn from(text: String) -> Self {
        FeedsDown { cause: FeedsDownCause::NotAnswering, text }
    }
}

impl std::fmt::Display for FeedsDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::fmt::Debug for DatahubProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DatahubProbe")
    }
}

/// Production's constructor: the dial to the datahub's strategy lane (`StreamOpenerStrategyLane`), at the address
/// and under the `Scope::Trade` key `live_runtime` resolved once.
pub struct StrategyLaneFactoriesProduction {
    addr: String,
    /// Where the tradehub's strategy-lane key (`credential_keys::datahub_trade_key()`, held in the hub-key store)
    /// is read from, ASKED AGAIN at every dial and every probe: `None` when the store holds none. A start with
    /// no key is held with the reason and the late arm polls the probe, so a key minted AFTERWARDS must be seen
    /// by the next poll (a key captured at start could never be, and the process would sit "waiting" for ever).
    /// `Debug` shows nothing of it (`HubKeys` redacts), and this type has no `Debug` of its own.
    keys: Arc<dyn Fn() -> Option<HubKeys> + Send + Sync>,
    /// The process's stop flag ([`StrategyLaneFactoriesProduction::with_stop`]); `None` for a caller that has none.
    stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Every dial made here ([`StrategyLaneFactories::price_links`]).
    links: Arc<price_links::PriceLinks>,
}

impl StrategyLaneFactoriesProduction {
    /// A constructor dialing the datahub at `addr` with `trade_key` (`None`: the store holds no
    /// strategy-lane key, and every dial refuses by name — absent credentials are the gate).
    pub fn new(addr: impl Into<String>, trade_key: Option<Vec<u8>>) -> Self {
        let keys = trade_key.map(|k| HubKeys::new(Vec::new(), Vec::new()).with_trade(k));
        Self {
            addr: addr.into(),
            keys: Arc::new(move || keys.clone()),
            stop: None,
            links: Arc::default(),
        }
    }

    /// This constructor watching the process's stop flag: [`StrategyLaneFactories::pause`] ends the moment it is raised.
    #[must_use]
    pub fn with_stop(mut self, stop: Option<Arc<std::sync::atomic::AtomicBool>>) -> Self {
        self.stop = stop;
        self
    }

    /// A constructor dialing the datahub at `addr` with the key `source` answers NOW, asked again at every dial
    /// and every probe (production: the hub-key store is re-read, so a key minted after the start is found by
    /// the late arm's next poll).
    pub fn with_key_source(
        addr: impl Into<String>,
        source: impl Fn() -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            addr: addr.into(),
            keys: Arc::new(move || {
                source().map(|k| HubKeys::new(Vec::new(), Vec::new()).with_trade(k))
            }),
            stop: None,
            links: Arc::default(),
        }
    }
}

/// What leads a held feed's cause when a venue's feed was refused by a CAP ([`FeedRefusalClass::Cap`]: a pump,
/// session or stream cap that is full): the one cause no restart mends and only an operator does.
pub const FEEDS_CAPPED_MARK: &str = "a cap on the price path is full";

/// **What the ready banner and the log name a held feed's cause with**, by its [`FeedsDownCause`]: the cap that is
/// full and what to change ([`FEEDS_CAPPED_MARK`]), "no strategy-lane key" for the missing key (mended by minting
/// the key: the late arm re-reads the store at every poll), the spent restart budget, else the lane not answering.
#[must_use]
pub fn short_cause(why: &FeedsDown) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    match &why.cause {
        FeedsDownCause::Cap { detail } => Cow::Owned(format!(
            "{FEEDS_CAPPED_MARK} ({detail}); an operator must raise that cap or drop a subscription, \
             and no restart is ordered for it"
        )),
        FeedsDownCause::RestartsSpent => Cow::Borrowed(
            "the late-arm restart budget is spent: the price path keeps refusing (restart by hand once it is fixed)",
        ),
        FeedsDownCause::MissingKey => Cow::Borrowed("no strategy-lane key"),
        FeedsDownCause::NotAnswering => {
            Cow::Borrowed("the price path (datahub or gatewayhub) does not answer")
        }
    }
}

/// The refusal a missing strategy-lane key is, for `who` (a venue's feed, or the probe's "the price feeds").
fn missing_key_text(who: &str) -> String {
    format!(
        "{who}: this daemon takes its prices from the datahub's strategy lane and holds no strategy-lane key \
         ({key}) in the hub-key store: mint it on the \
         datahub's box with `vike-cli backend mint-missing` and copy it here (the tradehub \
         notices a key minted later and restarts itself armed; otherwise restart it). Nothing was dialled",
        key = vike_venues::credential_keys::datahub_trade_key()
    )
}

impl StrategyLaneFactories for StrategyLaneFactoriesProduction {
    fn stopping(&self) -> bool {
        self.stop.as_ref().is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn strat(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<DataClientStrategyLane, String> {
        let Some(keys) = (self.keys)() else {
            return Err(missing_key_text(venue));
        };
        let opener =
            StreamOpenerStrategyLane::new(self.addr.clone(), keys, StrategyLaneTimings::default())
                .on_network(network);
        let feed = DataClientStrategyLane::new(opener, venue, sink).map_err(|e| {
            format!("{venue}: the strategy-lane dial could not start its reader thread: {e}")
        })?;
        self.links.add(venue, feed.link_probe());
        Ok(feed)
    }

    fn price_links(&self) -> Option<Arc<price_links::PriceLinks>> {
        Some(Arc::clone(&self.links))
    }

    /// The dial and the authenticated handshake of a strategy-lane connection, then the features it advertised
    /// read for every mounted venue, dropped at once: it opens no lane (`StrategyOpen` is never sent), so it takes
    /// no connection slot at the datahub. No key is a refusal by name with nothing dialled, and the key store is
    /// read again at every ask (a key minted later is found).
    fn datahub_probe(&self, venues: &[String]) -> Option<DatahubProbe> {
        let addr = self.addr.clone();
        let keys = Arc::clone(&self.keys);
        let venues = venues.to_vec();
        Some(DatahubProbe::new(move || {
            let Some(keys) = keys() else {
                return Some(FeedsDown {
                    cause: FeedsDownCause::MissingKey,
                    text: missing_key_text("the price feeds"),
                });
            };
            let client = match vike_datahub_client::DatahubClient::connect_strategy_lane(
                &addr,
                &keys,
                |_| {},
            ) {
                Ok(client) => client,
                Err(e) => return Some(format!("datahub {addr}: strategy lane: {e}").into()),
            };
            // A datahub that cannot serve a mounted venue's lane (an older or smaller build mid-deploy) is
            // found here, with its words, and not at the first subscribe after a core exists.
            venues
                .iter()
                .find_map(|venue| client.check_strategy_lane(venue).err())
                .map(|e| format!("datahub {addr}: strategy lane: {e}").into())
        }))
    }
}

/// Wire ONE venue's live market feed(s) onto the core's ingest lanes — `live_runtime`'s feed arms,
/// called once per DISTINCT started venue (split-plane I10). `configs` is every start on this venue,
/// in start order (never empty; the FIRST supplies the announcement fields and the venue-wide
/// knobs, e.g. the CEX pump's book grid); subscriptions dedup per series, so two starts on one
/// series subscribe once. `make` is the feed-construction seam ([`StrategyLaneFactories`]) — production
/// passes [`StrategyLaneFactoriesProduction`], the dial to the datahub's strategy lane; the deterministic splice test
/// substitutes a scripted constructor.
///
/// `data_only` is the profile's declared data-plane-only venue set
/// ([`crate::config::ProfileDaemon`]'s `data_only` key, collected per venue by
/// `live_runtime`'s withhold pass) — consulted ONLY by the four credentialed-data arms' arming
/// DISCLOSURE: a declared venue whose exec really stayed paper announces the declaration
/// ([`data_only_arming`]) instead of that venue's "bug to report"/"restart" remedy, which would
/// be false advice about a state the operator asked for. Empty (every undeclared start) leaves
/// every banner byte-identical.
///
/// `venue_arming` is `vike_tradehub_core::venue_arming`'s row set — the SAME `arming` binding
/// `live_runtime_with` journals from, computed after the withhold and before `vars` moves into the
/// `RuntimeConfig`. It is here for ONE job: [`other_live_accounts`], which needs the rows in order to
/// spell a labelled account's route key through `ArmingAccount::route_key` rather than parsing one.
/// EMPTY leaves every badge at its per-venue answer, which is why
/// `crates/vike-tradehub/tests/daemon/account_badge_wiring_pin.rs` pins the call site rather than
/// trusting the parameter to be passed.
///
/// `mount_accounts` is the account each of this venue's mounts TRADES (`MountPlacement::account`, the
/// default account for a mount naming none). `live_venues` is keyed by ACCOUNT route key, so the
/// arms' `exec` badge asks for those keys and not for the bare venue id, which is the default
/// account's alone (decision 0172).
///
/// There is no `venue_settings` parameter any more: the mark-stream and socket-batching rows
/// (`vike_secrets::venue_setting`) configure the venue client, which the gatewayhub builds and reads
/// them for (decision 0095).
///
/// This function is the ORCHESTRATOR: one arm per [`StrategyLanePlan`] variant, each calling the phase
/// functions below it in the order the arm's statements always ran. Every venue but hyperliquid and
/// polymarket gets `wire_<venue>_feed` (build the feed object and subscribe its lanes — every `?`
/// in it is a refusal to start, returned from here at the point it always was) and then
/// `announce_<venue>_arming` (the exec-live-or-paper disclosure); the CEX feed half is two lanes,
/// `wire_cex_kline_lane` then `spawn_cex_tick_pump`.
pub fn wire_venue_feeds(
    plan: &StrategyLanePlan,
    configs: &[&MountPlacement],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::ArmingAccount],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLanesLive, FeedRefusal> {
    let config = configs[0];
    let feeds = match plan {
        StrategyLanePlan::Hyperliquid(network) => StrategyLanesLive::Hyperliquid(
            wire_hyperliquid_feed(network, config, configs, handle, live_venues, wrap, make)?,
        ),
        StrategyLanePlan::Polymarket => StrategyLanesLive::Polymarket(wire_polymarket_feeds(
            configs,
            handle,
            live_venues,
            wrap,
            make,
        )?),
        StrategyLanePlan::Cex { venue, mainnet } => {
            let slug = venue.slug();
            let mut feed = wire_cex_kline_lane(slug, configs, handle, wrap, make)?;
            spawn_cex_tick_pump(venue, config, &mut feed)?;
            announce_cex_arming(
                venue,
                mainnet,
                slug,
                config,
                configs,
                live_venues,
                venue_arming,
                mount_accounts,
            );
            StrategyLanesLive::Cex(feed)
        }
        StrategyLanePlan::Alpaca => {
            let client = wire_alpaca_feed(configs, handle, wrap, make)?;
            announce_alpaca_arming(
                config,
                configs,
                live_venues,
                venue_arming,
                mount_accounts,
                data_only,
            );
            StrategyLanesLive::Alpaca(client)
        }
        StrategyLanePlan::Ctrader => {
            let data = wire_ctrader_feed(config, configs, handle, wrap, make)?;
            announce_ctrader_arming(
                config,
                configs,
                live_venues,
                venue_arming,
                mount_accounts,
                data_only,
            );
            StrategyLanesLive::Ctrader(data)
        }
        StrategyLanePlan::Oanda => {
            let feeds = wire_oanda_feed(configs, handle, wrap, make)?;
            announce_oanda_arming(
                config,
                configs,
                live_venues,
                venue_arming,
                mount_accounts,
                data_only,
            );
            StrategyLanesLive::Oanda(feeds)
        }
        StrategyLanePlan::Deribit => {
            let feeds = wire_deribit_feed(configs, handle, wrap, make)?;
            announce_deribit_arming(config, configs, live_venues, venue_arming, mount_accounts);
            StrategyLanesLive::Deribit(feeds)
        }
        StrategyLanePlan::Ig => {
            let feeds = wire_ig_feed(configs, handle, wrap, make)?;
            announce_ig_arming(
                config,
                configs,
                live_venues,
                venue_arming,
                mount_accounts,
                data_only,
            );
            StrategyLanesLive::Ig(feeds)
        }
    };
    Ok(feeds)
}

/// **The dial every tap-fronted arm starts from** (alpaca, deribit, hyperliquid, ig, oanda and every
/// price-only feed): the venue-agnostic `CoreLaneSink` onto the core's three lanes, a [`PriceTap`] built by
/// `tap` in front of it (it only RECORDS the last price per symbol, so the control door can tell whether a
/// symbol it admitted has a price yet, decision 0174; every verb is forwarded unchanged), `wrap` around
/// both, and the venue's feed object through the [`StrategyLaneFactories`] seam, in that order. Production's seam is
/// the strategy-lane dial (a client the DATAHUB's broker holds, built at the gatewayhub); the deterministic
/// splice test (`src/tradehub_cli/tests/feed_splice.rs`) substitutes a scripted constructor here. No
/// `TickSink` stands in front (a mount's bar synthesizer is the ctrader and polymarket arms' own), and
/// `network` is what the dial NAMES (Hyperliquid's test network alone; `None` everywhere else). Which tap a
/// venue gets is its arm's choice, argued there.
fn dial(
    venue: &str,
    network: Option<&str>,
    tap: fn() -> PriceTap,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<(DataClientTracking, Arc<PriceTap>), String> {
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn DataClientSink>;
    let tap = Arc::new(tap());
    let sink = wrap(Arc::new(
        DataClientSinkPriceTap::new(core, Arc::clone(&tap)).with_bars(handle.bar_sender()),
    ) as Arc<dyn DataClientSink>);
    Ok((DataClientTracking::new(make.client(venue, network, sink)?), tap))
}

/// How many times an arm asks for one symbol's quote stream before a NOT NOW refusal ([`FeedRefusalClass::NotNow`])
/// ends the start: three tries, [`QUOTE_RETRY_PAUSE`] apart, so ten seconds at the most.
const QUOTE_TRIES: u32 = 3;

/// The wait between two tries of one symbol's quote stream.
const QUOTE_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_secs(5);

/// Ask for `venue`'s quote stream of `symbol`, asking again on a TRANSIENT refusal, up to [`QUOTE_TRIES`] tries,
/// [`QUOTE_RETRY_PAUSE`] apart (a wait that ends when the process is asked to stop, `StrategyLaneFactories::pause`). The
/// tradehub restarts last in a deploy, and its boot-time subscribe can meet the gatewayhub still joining the
/// streams the recorder dropped 30 to 90 seconds after the old tradehub let them go (`gatewayhub`'s session
/// guard refuses a subscribe that waited too long behind a join, as transient); the joins are done by the next
/// try. Without this a running demo account turned that into a plain failed start and a supervisor restart.
/// Every try refused still ends the start, loudly, with the last refusal.
///
/// A CAP refusal is returned at once, like any other: a retry would meet the same ceiling. This is the verb of an
/// arm whose quote stream is its whole feed (cTrader's bars are folded from it), where a cap on the mount's own
/// symbol leaves an engine that hears nothing; every other arm takes [`subscribe_quotes_surviving`].
fn subscribe_quotes_retrying(
    venue: &str,
    feeds: &mut DataClientTracking,
    symbol: &str,
    make: &dyn StrategyLaneFactories,
) -> Result<(), FeedRefusal> {
    let mut tries = 0;
    loop {
        tries += 1;
        let Err(e) = feeds.subscribe_quotes(symbol) else { return Ok(()) };
        let refusal = FeedRefusal::subscribe(format!("{venue} subscribe_quotes {symbol}"), &e);
        if tries < QUOTE_TRIES && refusal.class == FeedRefusalClass::NotNow {
            tracing::warn!(
                symbol,
                reason = %refusal,
                try_number = tries,
                of = QUOTE_TRIES,
                "{venue}: the QUOTE stream of this mount's symbol was refused as TRANSIENT (the gatewayhub is \
                 still busy with the streams an earlier session released): asking again in a few seconds"
            );
            if !make.pause(QUOTE_RETRY_PAUSE) {
                continue;
            }
        }
        return Err(refusal);
    }
}

/// [`subscribe_quotes_retrying`], surviving a CAP refusal too (decision 0193): the datahub's or the
/// gatewayhub's stream ceiling costs THAT key's quote stream and nothing else. This start would otherwise end
/// for every venue's engines on one symbol too many (the core is up and cannot drop a running account's engine,
/// so the arm's `Err` is `OPERATOR_NEEDED_EXIT`). The mount keeps its candle closes, which the ceiling does not
/// count; only its quote lane is lost. So this is the verb of an arm with a bar lane of its own.
///
/// Any other refusal is returned as it came.
fn subscribe_quotes_surviving(
    venue: &str,
    feeds: &mut DataClientTracking,
    symbol: &str,
    make: &dyn StrategyLaneFactories,
) -> Result<(), FeedRefusal> {
    match subscribe_quotes_retrying(venue, feeds, symbol, make) {
        Err(refusal) if refusal.class == FeedRefusalClass::Cap => {
            tracing::warn!(
                symbol,
                reason = %refusal,
                "{venue}: the QUOTE stream of this mount's symbol was refused by a stream ceiling (a cap, \
                 not a link): the mount runs on its candle closes WITHOUT quotes, and every other \
                 symbol is unaffected. Free a stream (stop a strategy or a recorded symbol that holds \
                 one) and restart this tradehub"
            );
            Ok(())
        }
        asked => asked,
    }
}

/// A CEX venue's PRICE-ONLY feed: the strategy-lane dial and the price tap with NO subscription, for a venue
/// no mount names. Its [`StrategyLanesLive::price_watcher`] is how the control door prices a symbol it admits on
/// that venue (decision 0174): without it the door answers `this node runs no price feed for <venue>` and
/// refuses every foreign symbol of an account whose venue has no mount. Construction never dials on the
/// caller's thread and the dial parks idle while nothing is held, so an unused feed costs no subscription.
pub fn wire_cex_price_feed(
    venue: &crate::StrategyLanePlanCex,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLanesLive, FeedRefusal> {
    wire_cex_kline_lane(venue.slug(), &[], handle, wrap, make).map(StrategyLanesLive::Cex)
}

/// The network of Hyperliquid's price-only feed: the one the mounted arm would dial for the same rows
/// ([`feed_tier`]: mainnet when a `live` account is armed, else the test network).
pub fn hl_price_network(table: &ArmingTable, policy: &AdapterPolicy) -> VenueNetwork {
    VenueNetwork::for_tier(feed_tier("hyperliquid", table, policy))
}

/// Is `slug` a PRICE-ONLY venue: one the control door admits symbols for (`admitting`) that no mount names.
fn is_price_only(slug: &str, admitting: &[String], mounted: &[String]) -> bool {
    admitting.iter().any(|a| a == slug) && !mounted.iter().any(|m| m == slug)
}

/// Whose login `venue`'s price stream rides at the gatewayhub, for a venue whose paper accounts are priced through the
/// demo login ([`paper_quotes_through_demo`]); `None` for every other venue. The answer is the venue's credentialed row
/// (`crates/vike-gatewayhub-client/src/credentialed_limits.rs`'s `stream_login_for`), the one the gatewayhub's stream
/// logs in by, so the order check (`TradeLeaseRegistry::demo_login_key`) and the stream agree.
pub(crate) fn paper_price_login(
    venue: &str,
) -> Option<vike_gatewayhub_client::credentialed_limits::CredentialedStreamLogin> {
    paper_quotes_through_demo(venue)
        .then(|| vike_gatewayhub_client::credentialed_limits::stream_login_for(venue))
        .flatten()
}

/// One venue whose PRICE-ONLY feed is the shared [`dial`] with nothing subscribed (decision 0174), for a box
/// that mounts none of its markets: the control door subscribes a symbol's quote lane through the feed's
/// [`StrategyLaneQuote::watcher`] when an order names a symbol the account was not started on, and reads its price
/// back from the same tap. Nothing is asked of the venue until then, and a dial that cannot be built leaves
/// no watcher, so the door answers `this node runs no price feed for <venue>` (fail closed).
struct PriceOnlyArm {
    /// The venue's slug, which the dial names (on no network: the venue's main one).
    venue: &'static str,
    /// The tap in front of the core's sink, which the door reads the price from.
    tap: fn() -> PriceTap,
    /// The most quote lanes the door may hold open on the feed ([`StrategyLaneQuote::with_lane_cap`]); `None` is no
    /// cap of the feed's own.
    lane_cap: Option<usize>,
    /// The [`StrategyLanesLive`] variant that holds the feed.
    feeds: fn(StrategyLaneQuote) -> StrategyLanesLive,
}

/// **The price-only arms that are the plain [`dial`]**, in the order [`price_only_venues`] lists them and
/// [`wire_price_only_feeds`] builds them (the teardown's order), after the CEX venues and Hyperliquid, which
/// keep their own arms (a CEX feed is its kline lane; Hyperliquid's dial names the network its accounts
/// decide). Each row's comment says why its tap; the venue's whole reading is its `feeds/<venue>.rs` module
/// doc (ctrader's and ig's: `feeds/credentialed_price_only.rs`).
const PRICE_ONLY_ARMS: &[PriceOnlyArm] = &[
    // The plain tap: a perpetual's last price, as Hyperliquid's. ⚠ The dial reads deribit's PUBLIC MAINNET
    // host while exec signs on the testnet (`deribit_arm`), so the price is a mainnet reference.
    PriceOnlyArm {
        venue: "deribit",
        tap: PriceTap::default,
        lane_cap: None,
        feeds: StrategyLanesLive::Deribit,
    },
    // The plain tap, and no `TickSink` in front (a mount's bar synthesizer is the mount's): every quote goes
    // to the core's lane sink, which routes it to the venue's engine without a mount.
    PriceOnlyArm {
        venue: "ctrader",
        tap: PriceTap::default,
        lane_cap: None,
        feeds: StrategyLanesLive::Ctrader,
    },
    // The venue-clocked tap, the mounted arm's (`feeds/oanda.rs`'s `wire_oanda_feed`): the bridge's REST
    // first price arrives up to its bound old.
    PriceOnlyArm {
        venue: "oanda",
        tap: PriceTap::venue_clocked,
        lane_cap: None,
        feeds: StrategyLanesLive::Oanda,
    },
    // The plain tap, and the venue's row of extra quote lanes: each lane is one more subscription on the
    // feed's one IG login and one Lightstreamer socket.
    PriceOnlyArm {
        venue: "ig",
        tap: PriceTap::default,
        lane_cap: Some(vike_gatewayhub_client::credentialed_limits::IG.max_streams),
        feeds: StrategyLanesLive::Ig,
    },
    // The labelled-quotes tap: a two-sided quote of a stream that has stated a label, a delayed one included
    // (decision 0140).
    PriceOnlyArm {
        venue: ibkr::VENUE,
        tap: PriceTap::labelled_quotes,
        lane_cap: None,
        feeds: StrategyLanesLive::Ibkr,
    },
    // The plain tap: every quote the gatewayhub emits is already a real-time, two-sided, uncrossed one.
    PriceOnlyArm {
        venue: dukascopy::VENUE,
        tap: PriceTap::default,
        lane_cap: None,
        feeds: StrategyLanesLive::Dukascopy,
    },
    // The venue-clocked tap: the gatewayhub re-delivers a live quote at every poll, stamped with its own time.
    PriceOnlyArm {
        venue: fxcm::VENUE,
        tap: PriceTap::venue_clocked,
        lane_cap: None,
        feeds: StrategyLanesLive::Fxcm,
    },
    // The plain tap over the book-derived best bid and ask, one pump per token. A box that mounts a token
    // wires no such feed (the venue is then not price-only), so a new token is priced nowhere there.
    PriceOnlyArm {
        venue: polymarket::VENUE,
        tap: PriceTap::default,
        lane_cap: None,
        feeds: StrategyLanesLive::PolymarketQuotes,
    },
];

/// Every venue a price-only feed can be wired for, in the one order: the CEX venues (with their
/// [`StrategyLanePlanCex`]), Hyperliquid, then the [`PRICE_ONLY_ARMS`].
fn price_only_candidates() -> impl Iterator<Item = (&'static str, Option<StrategyLanePlanCex>)> {
    StrategyLanePlanCex::ALL
        .into_iter()
        .map(|v| (v.slug(), Some(v)))
        .chain([("hyperliquid", None)])
        .chain(PRICE_ONLY_ARMS.iter().map(|arm| (arm.venue, None)))
}

/// **The price-only venues, as the start knows them before any core exists**: the CEX venues, Hyperliquid and
/// the `PRICE_ONLY_ARMS` (ibkr, dukascopy and fxcm are never a mount: no profile names them; polymarket is not
/// price-only where a mount names a token) that the
/// door admits for and no mount names, whether or not the datahub later serves a strategy lane for them ([`wire_price_only_feeds`] asks
/// that per venue, and a venue it skips just never discloses anything).
/// The ONE set the feed wiring, the link dead-man's arming and the core's price-only sweep all read.
pub fn price_only_venues(admitting: &[String], mounted: &[String]) -> Vec<String> {
    price_only_candidates()
        .map(|(slug, _)| slug)
        .filter(|slug| is_price_only(slug, admitting, mounted))
        .map(str::to_string)
        .collect()
}

/// What [`wire_price_only_feeds`] built.
pub struct StrategyLanesPriceOnly {
    /// The feeds, for the teardown (they are released with the mounts' feeds, in order).
    pub feeds: Vec<StrategyLanesLive>,
    /// The door's price handle per venue (`TradeLeaseRegistry::bind_prices`).
    pub watchers: std::collections::HashMap<String, StrategyLaneCexWatcher>,
    /// The network Hyperliquid's price-only feed was wired for, `None` when it was not wired.
    pub hl_network: Option<VenueNetwork>,
}

/// **The PRICE-ONLY feeds of the venues the control door admits symbols for (`admitting`) that no mount names
/// (`mounted`)**: the CEX venues, Hyperliquid when `hl_network` is `Some`, and each venue of `PRICE_ONLY_ARMS`
/// (deribit, ctrader, oanda, ig, ibkr, dukascopy, fxcm and polymarket: [`wire_price_only_feed`]). The control door prices
/// a symbol it admits through its venue's watcher, and a mount's feed is the only other source of one, so a box
/// that mounts bybit alone refused every foreign binance, okx, aster, hyperliquid, deribit, ctrader, oanda, ig, dukascopy or fxcm symbol
/// (oanda's `Admits` row made an unmounted OANDA account lose the foreign pairs a priced limit order could always
/// reach). The core routes a venue's
/// quote, trade and book to that venue's engine without a mount, so a dial with nothing subscribed is all the
/// door needs. Each venue is asked of the datahub first (`StrategyLaneFactories::datahub_probe`: is a strategy lane
/// advertised for it?), so a datahub that serves no lane for a venue costs no dial that would be refused for
/// ever. A refusal is said at `warn` and costs only that venue's foreign symbols (the door fails closed); it
/// never ends the start.
///
/// ⚠ No closed-bar feed comes with these (the bar lane is the MOUNT's); the core's price-only sweep stands in
/// for it (margin call and drawdown latch off the clock) and the link dead-man hears these feeds' statuses
/// while the venue holds an admitted symbol or a live order (`price_only_venues` is the one set both read).
/// `docs/ops/price-only-venue-safety-plan.md` carries the design.
pub fn wire_price_only_feeds(
    admitting: &[String],
    mounted: &[String],
    hl_network: Option<VenueNetwork>,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> StrategyLanesPriceOnly {
    let mut out = StrategyLanesPriceOnly {
        feeds: Vec::new(),
        watchers: std::collections::HashMap::new(),
        hl_network: None,
    };
    let candidates =
        price_only_candidates().filter(|(slug, _)| is_price_only(slug, admitting, mounted));
    for (slug, cex) in candidates {
        if let Some(why) = make.datahub_probe(&[slug.to_string()]).and_then(|p| p.check()) {
            tracing::warn!(
                venue = slug,
                reason = %why,
                "{slug}: no price-only feed (the datahub's strategy lane does not answer for it): an \
                 order on a symbol its engine was not started on is refused there (no price)"
            );
            continue;
        }
        let built = match (cex, slug, hl_network.as_ref()) {
            // A price-only refusal is said and never decided on: its words alone.
            (Some(venue), _, _) => {
                wire_cex_price_feed(&venue, handle, wrap, make).map_err(|refusal| refusal.text)
            }
            (None, "hyperliquid", Some(network)) => wire_hl_price_feed(network, handle, wrap, make),
            (None, "hyperliquid", None) => continue,
            (None, _, _) => wire_price_only_feed(slug, handle, wrap, make),
        };
        match built {
            Ok(wired) => {
                if let Some(watcher) = wired.price_watcher() {
                    out.watchers.insert(slug.to_string(), watcher);
                }
                if slug == "hyperliquid" {
                    out.hl_network = hl_network;
                }
                out.feeds.push(wired);
            }
            Err(why) => tracing::warn!(
                venue = slug,
                reason = %why,
                "{slug}'s price-only feed could not be built: an order on a symbol its engine was not \
                 started on is refused there (no price) until the next start"
            ),
        }
    }
    out
}

/// Hyperliquid's PRICE-ONLY feed on `network`, for a box that mounts no Hyperliquid market (the twin of
/// [`wire_cex_price_feed`]): the dial and the price tap, nothing subscribed. ONE feed per venue, as the
/// mounted arm's is: the core keys a quote by venue and symbol, so two networks' feeds of one coin would
/// interleave their prices; `network` is `feed_tier`'s answer (mainnet when a live account is armed).
pub fn wire_hl_price_feed(
    network: &VenueNetwork,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLanesLive, String> {
    hyperliquid::wire_hyperliquid_price_feed(network, handle, wrap, make)
        .map(StrategyLanesLive::Hyperliquid)
}

/// **`venue`'s PRICE-ONLY feed, from its `PRICE_ONLY_ARMS` row** (the twin of [`wire_hl_price_feed`]): the
/// shared `dial` with the row's tap and nothing subscribed, so nothing is asked of the venue until the door
/// asks for a symbol, held in the row's [`StrategyLanesLive`] variant with the row's lane cap. A credentialed venue's
/// dial is the mounted arm's (a credentialed stream at the gatewayhub, as the venue's default `demo`
/// account), so an account with no usable credentials, or a handshake the venue refuses, is the FIRST
/// subscription's refusal (the door's refusal of that order), not a wiring failure.
///
/// # Errors
/// A venue with no row, and a dial that cannot be built.
pub fn wire_price_only_feed(
    venue: &str,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLanesLive, String> {
    let arm = PRICE_ONLY_ARMS
        .iter()
        .find(|arm| arm.venue == venue)
        .ok_or_else(|| format!("{venue} has no price-only feed arm"))?;
    let (feeds, tap) = dial(arm.venue, None, arm.tap, handle, wrap, make)?;
    let feed = StrategyLaneQuote::new(feeds, tap);
    Ok((arm.feeds)(match arm.lane_cap {
        Some(cap) => feed.with_lane_cap(cap),
        None => feed,
    }))
}

#[cfg(test)]
#[path = "feeds/announce_tests.rs"]
mod announce_tests;

#[cfg(test)]
#[path = "feeds/ctors_tests.rs"]
mod ctors_tests;

#[cfg(test)]
#[path = "feeds/dial_tests.rs"]
mod dial_tests;

#[cfg(test)]
#[path = "feeds/quote_feed_tests.rs"]
mod quote_feed_tests;

#[cfg(test)]
#[path = "feeds/paper_quotes_tests.rs"]
mod paper_quotes_tests;
