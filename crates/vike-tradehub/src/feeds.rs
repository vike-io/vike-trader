//! Feed wiring: which venues get a live market feed, the per-mount venue-feed dispatch
//! (`venue_feed_plan`), and subscribing those feeds onto the core's ingest lanes
//! (`wire_venue_feeds`). `live_runtime` (in `src/tradehub_cli/live_runtime.rs`) calls into this module once per
//! distinct started venue.
//!
//! **Every feed is a dial to the datahub's strategy lane** (wave 2 of `vike-gatewayhub`, decisions 0164 and
//! 0167): this process opens no venue socket and links no bridge. Each venue gets ONE
//! `vike_datahub_client::strat::StratFeed` — the tube's receiving engine over a `Scope::Trade` dial — and
//! the arm subscribes the same lanes it always did (bars, quotes, trades, book, the CEX pump as `Ticks`)
//! into the same `CoreLaneSink` or `MakerSink`. The datahub holds the venue clients (a key a GUI window or
//! the recorder already holds is subscribed once) and the gatewayhub holds the sockets; the CREDENTIALED
//! venues (alpaca, ctrader, oanda, ig) are credentialed streams at the gatewayhub, which resolves the
//! account itself, so a [`VenuePlan`] carries no config and nothing here reads a venue credential.
//!
//! [`LiveFeeds`]/[`CexFeed`]/[`Tracked`] (the feed-object types) and the [`FeedCtors`]
//! construction seam + its production impl [`ProdFeedCtors`] live here alongside the functions
//! that name them in their signatures — `recon_feed_statuses_of` takes `&[LiveFeeds]`,
//! `wire_venue_feeds` returns `Result<LiveFeeds, String>` and takes `&dyn FeedCtors` — for the
//! same reason `CexVenue`/`VenuePlan` moved to `types.rs`: a library crate cannot name a type the
//! binary depending on it defines. The `live_runtime`/`live_runtime_with` of `src/tradehub_cli/live_runtime.rs` (which
//! stay there) and the `tradehub_cli::tests` modules such as `feed_splice` reach these through
//! `use vike_tradehub::feeds::{...}` — `use super::*`/`use super::{...}` inside those then see
//! them as if they were still the binary's own private items.
//!
//! `FEED_ARMS` is the ONE list of the venues this daemon can build a market feed for, and
//! `venue_feed_plan` dispatches on it: a venue has a feed arm exactly when it has a row there.
//!
//! Layout: this file keeps the dispatch (`FEED_ARMS`, `venue_feed_plan`), the polymarket interval gate, the
//! [`FeedCtors`] seam and the ORCHESTRATOR `wire_venue_feeds`; the feed
//! types live in `feeds/live.rs`, the CEX pump's book fold in `feeds/pump_book.rs`, and each `VenuePlan`
//! arm's phase functions in `feeds/<venue>.rs` (`feeds/cex.rs`: `wire_cex_kline_lane`,
//! `spawn_cex_tick_pump`, `announce_cex_arming`).

mod alpaca;
mod cex;
mod ctrader;
mod deribit;
mod hyperliquid;
mod ig;
mod live;
mod oanda;
mod polymarket;
mod price_tap;
mod pump_book;

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_datahub_client::strat::{StratFeed, StratOpener, StratTimings};
use vike_hub_proto::auth::HubKeys;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::{AdapterPolicy, ArmingTable, MakerConfig};

#[cfg(doc)]
use crate::venue_arming::arming::{data_only_arming, other_live_accounts};
use crate::venue_arming::tier::{cex_mainnet_enabled, feed_tier};
use crate::venue_plan::{alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan};
use crate::{CexVenue, HlNetwork, ResolvedAdapter, VenuePlan};
use alpaca::{announce_alpaca_arming, wire_alpaca_feed};
use cex::{announce_cex_arming, spawn_cex_tick_pump, wire_cex_kline_lane};
use ctrader::{announce_ctrader_arming, wire_ctrader_feed};
use deribit::{announce_deribit_arming, wire_deribit_feed};
use hyperliquid::wire_hyperliquid_feed;
use ig::{announce_ig_arming, wire_ig_feed};
pub use live::{
    CexFeed, CexWatcher, LiveFeeds, LiveTeardown, PostFeeds, QuoteFeed, Tracked,
    recon_feed_statuses_of,
};
use oanda::{announce_oanda_arming, wire_oanda_feed};
use polymarket::wire_polymarket_feeds;
pub use price_tap::PriceTap;

/// One feed arm: a venue's per-mount PRE-BUILD plan (see [`venue_feed_plan`]).
type FeedArm = fn(&MakerConfig, &ArmingTable, &AdapterPolicy) -> Result<VenuePlan, String>;

/// **THE FEED ARMS** — the venues this daemon's live start can build a market FEED for, each with
/// its plan, in ALPHABETICAL order (the order both refusals list them in). A venue without a row
/// here has no feed, so a profile naming it as its own venue is refused by
/// `crate::config::DaemonProfile::validate_for_live` and by [`venue_feed_plan`]. It is the ONE list
/// of feed venues: there is no second list to hold equal to it.
///
/// It is a VENUE list, not a `(venue, symbol)` allow-list — the symbol question is a separate,
/// DERIVED check (see `crate::config::DaemonProfile::validate_for_live`), because "which venues did
/// somebody wire a feed for" and "which symbol does that venue's engine accept" are different facts
/// with different authorities, and conflating them is what made this gate a hand-copied pair table.
///
/// ## ⚠ fxcm is DELIBERATELY absent, and it is the one venue that can never join
///
/// `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` has an fxcm engine row and `assemble_runtime` an fxcm `make_engine` arm (both
/// behind the gatewayhub's `fxcm` feature: `vike_tradehub_core::wired_markets` keeps the row only where the arming table starts it), so an fxcm-armed gatewayhub now starts a real FXCM execution engine and
/// reconcile client. That does NOT make it startable as a PROFILE venue, because this table is about
/// the other half: which venues `venue_feed_plan` can build a market FEED for. `vike-fxcm` has no
/// market-data seam of any kind — `LiveDataCaps::NONE` in its caps row, `NoPump` in
/// `vike_bridge_core::pump_spec` — so there is no arm to write, and a row here IS a feed arm. A
/// profile naming `venue = "fxcm"` is therefore refused at step (1) of
/// `crate::config::DaemonProfile::validate_for_live`, which is correct: a start with no prices is
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
    // `AlpacaVenueAdapter` (it arms only a `demo` account; no `ALPACA_MAINNET` flag; a LIVE flip is a
    // deliberate code change, not a config flip).
    ("alpaca", alpaca_arm),
    // ⚠ REAL MONEY IN PRACTICE: aster's exec network is the account's own tier
    // (`AsterVenueAdapter` in `crates/bridges/aster/src/adapter.rs`), and only a `live` aster account
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
    // I9): deribit's feed is `market_feed::Feeds`, four `DataClient` verbs off the venue's PUBLIC
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
    // ig's feed is `market_feed::Feeds`, which serves `subscribe_quotes` + `subscribe_bars` and
    // refuses trades/book/depth through the declared caps row — IG is a DEALER venue publishing
    // its own two-sided price, so a public tape and an L2 ladder do not exist to be wired. Every
    // subscription logs in with the exec side's own `IgConfig`, so absent credentials refuse the
    // live start (the alpaca/ctrader/oanda argument). Exec is DEMO-gateway-pinned in
    // `crates/bridges/ig/src/adapter.rs`'s `IgVenueAdapter` (it arms only a `demo` account, loading
    // `AccountTier::Demo`; `ig_rest_base` has a live-gateway arm with NO caller in the workspace, so a
    // `live` IG account holding its trio does not run, `LiveTierNotWired` —
    // `crates/bridges/ig/CLAUDE.md` records that trap) — and the feed resolves the same `demo`
    // account at the same tier, so this venue is demo exec over demo prices end to end.
    ("ig", ig_arm),
    // ⚠ CREDENTIALED MARKET DATA on BOTH lanes, and NO market-data WS at all (split-plane I9):
    // oanda's feed is `market_feed::Feeds`, a `pump_spec` `OwnPump` — quotes ride a chunked-HTTP
    // `/pricing/stream` line stream and bars POLL the candles REST endpoint, both Bearer-authed,
    // so absent credentials refuse the live start (the alpaca/ctrader argument). Exec arms only
    // the PRACTICE tier: `crates/bridges/oanda/src/adapter.rs`'s `OandaVenueAdapter` goes through
    // `crates/bridges/oanda/src/config.rs`'s `adapter_tier`, which arms a `demo` account only and
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
    cfg: &MakerConfig,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    match FEED_ARMS.iter().find(|(venue, _)| *venue == cfg.venue) {
        Some((_, arm)) => arm(cfg, table, policy),
        None => Err(format!(
            "venue '{}' is not wired for live yet (this build can mount: {}); refusing to mount",
            cfg.venue,
            feed_arm_list()
        )),
    }
}

fn binance_arm(
    cfg: &MakerConfig,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // ── The CEX venues. One shared gate (`cex_plan`), because they share one feed shape.
    //
    // ⚠ Their market DATA is MAINNET on all of them, while exec follows each venue's own
    // demo/mainnet credential gate inside `assemble_runtime`. On binance/bybit/okx that split is
    // STRUCTURAL, not an oversight: `market_data.rs`'s host consts are exactly one per venue
    // (binance `MAINNET_WS`, bybit `PUBLIC_WS_LINEAR`, okx `PUBLIC_WS`) and bybit's demo
    // environment serves no public market WS at all. So a demo start is DEMO EXEC OVER MAINNET
    // PRICES. That is the intended shape — a demo order must rest against the real book to mean
    // anything — but it is stated here so nobody later "fixes" the feed to follow the exec
    // environment and silently points a live account at a dead host. (Aster reaches the same
    // shape by CHOICE rather than structure — its arm below says how and why.)
    //
    // SPOT lane (`BTCUSDT`, no `.P`): `VENUE_WIRING` pins the spot symbol, and
    // `spawn_binance_market_data` is the spot `@bookTicker`+`@trade`+`@depth` combined
    // stream, so the maker prices on `on_quote_tick` off a NATIVE L1 channel. (Every CEX
    // venue here drives `on_quote_tick`; only the quote's PROVENANCE differs — see
    // `CexVenue::quote_source`.)
    cex_plan(CexVenue::Binance, cfg, cex_mainnet_enabled(CexVenue::Binance, table, policy))
}

fn bybit_arm(
    cfg: &MakerConfig,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // LINEAR PERP. ⚠ The two lanes read the same `BTCUSDT` string but reach DIFFERENT
    // instruments, and that is deliberate rather than a bug to be tidied away:
    // `spawn_bybit_market_data` connects `PUBLIC_WS_LINEAR` (the perp — matching the linear
    // perp `make_engine` starts), while `market_feed`'s kline lane runs `perp_split`, which
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
    cex_plan(CexVenue::Bybit, cfg, cex_mainnet_enabled(CexVenue::Bybit, table, policy))
}

fn okx_arm(
    cfg: &MakerConfig,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
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
    cex_plan(CexVenue::Okx, cfg, cex_mainnet_enabled(CexVenue::Okx, table, policy))
}

fn aster_arm(
    cfg: &MakerConfig,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // USDⓈ-M PERP, vike's `.P` spelling (`BTCUSDT.P` — `VENUE_WIRING` pins it; the pump
    // and the kline lane each split it to the exchange's `BTCUSDT` for the wire while the
    // series label keeps the `.P`, so `accepts_symbol` and `drive_strategy_tick` both see
    // the started spelling). A binance fork: `spawn_aster_market_data` is the same
    // `@bookTicker`+`@trade`+`@depth@100ms` combined stream on the futures host — a
    // NATIVE L1 channel behind `on_quote_tick`, like binance.
    //
    // ⚠ Feed hosts are tier-keyed on this venue (unlike the three above, whose
    // pumps have exactly one host each) — a testnet EXISTS; the daemon still PINS the
    // feed to `AccountTier::Live` (mainnet) in `wire_venue_feeds`, deliberately: exec
    // follows the account's own tier (`AsterVenueAdapter`, no `ASTER_MAINNET` flag
    // exists), only `live` aster accounts are configured in practice, and a
    // demo/paper order must rest against the real book to mean anything — the same
    // demo-exec-over-mainnet-prices shape as the three venues above, reached by choice
    // rather than by structure. vike-app's aster start made the identical choice for the
    // identical reason (its `Feeds::with_env(.., Live)` beside the mainnet-always
    // `AsterCatalog`), while the desktop had one.
    //
    // ⚠⚠ REAL MONEY IN PRACTICE: a `live` aster account holding its credentials makes
    // this a LIVE MAINNET exec start. This arm is CAPABILITY only — absent credentials
    // still start paper (the workspace live gate), and nothing here changes any default.
    cex_plan(CexVenue::Aster, cfg, cex_mainnet_enabled(CexVenue::Aster, table, policy))
}

fn alpaca_arm(
    cfg: &MakerConfig,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // US EQUITY (`AAPL` — `VENUE_WIRING` pins it), the first CREDENTIALED-DATA arm
    // (split-plane I9): NOT the CEX two-lane shape. There is no keyless pump —
    // `AlpacaDataClient` is one multiplexed, OAuth-authenticated WS client serving real 1m
    // bar CLOSES (the watchdog cadence), L1 quotes (a native channel behind
    // `on_quote_tick`) and trade prints, all through `CoreLaneSink`. The plan carries nothing:
    // the gatewayhub resolves the account, and ABSENT credentials REFUSE the credentialed stream
    // (the arm refuses the start on it) rather than degrading:
    // exec-degrades-to-paper still leaves a working start, but a feed-less start quotes
    // into the void forever. Data hosts follow the SAME tier the exec side is pinned to
    // (the sandbox — the bridge arms only a `demo` account, and no `ALPACA_MAINNET` flag
    // exists), so unlike the CEX arms this venue's demo start is DEMO EXEC OVER SANDBOX
    // PRICES — the venue serves a sandbox data stream, which the CEX venues do not.
    alpaca_plan(cfg)
}

fn ctrader_arm(
    cfg: &MakerConfig,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // FX (`EURUSD` — `VENUE_WIRING` pins it), the second credentialed-data arm
    // (split-plane I9): one DEDICATED protobuf/TLS data socket (`connect_and_auth`,
    // SYNCHRONOUS at start — wire failure refuses startup, see `wire_venue_feeds`),
    // isolated from the exec socket `make_engine` opens, per the same isolation rule every
    // recon client follows. Serves L1 spot quotes; live trendbars only ever FORM through
    // this client (no close fires), so the arm synthesizes closed bars from quote mids via
    // `MakerSink` — the polymarket shape — and the DEMO host follows the exec side's own
    // DEMO-tier pin. Absent credentials refuse the start (same argument as alpaca above).
    ctrader_plan(cfg)
}

fn oanda_arm(
    cfg: &MakerConfig,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // FX (`EURUSD` — `VENUE_WIRING` pins it; `vike_catalog::to_oanda_instrument` splits it to the
    // venue's own `EUR_USD` on the wire while every emitted series keeps the started
    // spelling `accepts_symbol` needs), the third credentialed-data arm (split-plane I9)
    // and the only live-wired venue with NO market-data socket of any kind: this venue
    // publishes no market-data WS, so its feed is `pump_spec`'s `OwnPump` class. ONE
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
    oanda_plan(cfg)
}

fn deribit_arm(
    cfg: &MakerConfig,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // The OPTIONS venue's inverse perp (`BTC-PERPETUAL` — `VENUE_WIRING` pins it; deribit
    // instrument names are already unambiguous, so there is no `.P` split and the wire
    // symbol IS the series symbol). Split-plane I9 gave it a real `DataClient` where
    // `pump_spec` used to say `NoPump`, and it is the WIDEST one this daemon starts: FOUR
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
    // `MAINNET_REST` has no caller in the workspace. So a live deribit start is TESTNET
    // exec over MAINNET prices, which `deribit_arming` discloses in words.
    deribit_plan(cfg)
}

fn ig_arm(
    cfg: &MakerConfig,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // FX CFD (`CS.D.EURUSD.MINI.IP` — `VENUE_WIRING` pins the EPIC, which is IG's own
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
    // ⚠ Absent credentials REFUSE the live start (alpaca's argument): every subscription
    // opens its OWN `IgSession` login for streaming credentials, so there is no keyless
    // half to fall back to. DEMO gateway on both planes — `make_engine` arms only a
    // `demo` account, through `load_ig_config_from(Demo, …)`, and the plan resolves the
    // same loader at the same tier over the same account, so like oanda this venue's feed
    // and exec agree on the environment by construction. (`ig_rest_base(Live)` is
    // implemented and called by nothing: a `live` IG account holding its trio starts
    // PAPER, `LiveTierNotWired` — `ig_arming` says so.)
    ig_plan(cfg)
}

fn hyperliquid_arm(
    cfg: &MakerConfig,
    table: &ArmingTable,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // `assemble_runtime` hardcodes HL's market as "BTC"; any other symbol would silently never
    // receive its feed (the feed subscribes "BTC"; the start routes on its own symbol).
    if cfg.token_id != "BTC" {
        return Err(format!(
            "hyperliquid mount symbol must be \"BTC\" (assemble_runtime's hardcoded HL market), got {:?}",
            cfg.token_id
        ));
    }
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
    Ok(VenuePlan::Hyperliquid(HlNetwork::for_tier(feed_tier(&cfg.venue, table, policy))))
}

fn polymarket_arm(
    _cfg: &MakerConfig,
    _table: &ArmingTable,
    _policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // Polymarket is the A-S maker's NATIVE [0,1] price domain — unlike a $-priced crypto perp,
    // whose $64k mid the maker's avellaneda §2.2 [0,1] wall clamp can't quote. It has NO
    // testnet: every live order is real money on Polygon mainnet, gated by the account's
    // PRIVATE_KEY + flags.poly_exec (checked INSIDE assemble_runtime) and the Dublin SOCKS egress (US-geo-
    // blocked otherwise → silent paper fallback). `token_id` shape was validated by
    // `DaemonProfile::validate_for_live`.
    Ok(VenuePlan::Polymarket)
}

/// The polymarket same-token interval gate (split-plane I10): two starts on ONE token share one
/// `Feeds` + `MakerSink`, whose `TickBarSynthesizer` runs at exactly ONE window — so rows naming
/// the same token at different intervals would leave the second start's paper-fill bar lane
/// silently dead. Refused by ROW, before any core spawns.
pub fn check_poly_token_intervals(mounts: &[ResolvedAdapter]) -> Result<(), String> {
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
/// **Production is the one dial, not a table of constructors**: every feed is a
/// [`StratFeed`] (the datahub's strategy lane, one per venue and network), so the seam has ONE required
/// method, [`FeedCtors::strat`], and [`FeedCtors::client`] — the shape every non-pump arm holds, a
/// [`DataClient`] — defaults to it. A test impl substitutes `client` (a scripted double per venue) and
/// receives the arm's own `wrap`-ed sink chain plus the arm's own `subscribe_*` calls: everything about
/// the splice except the venue's socket. `src/tradehub_cli/tests/feed_splice.rs` is that test. The CEX
/// arm alone needs the concrete [`StratFeed`] (the tick pump is an inherent verb, not a `DataClient`
/// one), so it calls `strat` directly.
pub trait FeedCtors {
    /// The strategy-lane dial for `venue` (on `network`: Hyperliquid's test network only; `None` is the
    /// venue's main network), delivering into `sink`. Construction never dials (the engine's reader
    /// does), so a datahub that is down is a link that is down, not a failed constructor; the `Err` is
    /// a dial that cannot even be attempted (no strategy-lane key) or an OS refusing a thread.
    fn strat(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<StratFeed, String>;

    /// The feed object an arm that holds a [`DataClient`] subscribes on: the strategy-lane dial.
    fn client(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
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
}

/// **The question "does the datahub's strategy lane answer?"**, askable more than once and from another
/// thread (the late arm's): `check` is `None` when it does, `Some(why)` when it does not, the words an
/// operator reads in the NOT RUNNING line.
///
/// Production's is [`ProdFeedCtors::datahub_probe`]: no strategy-lane key is `Some(the by-name refusal)`
/// without dialling anything, otherwise one dial, the authenticated handshake and a read of the features the
/// datahub advertised (a strategy lane, and a `strat_venue=` entry for every mounted venue; no lane is opened,
/// so it costs no connection slot). A test's is a closure.
///
/// ⚠ It answers what the DATAHUB advertises, not what is behind it: a datahub that is up while ITS gatewayhub is
/// away still passes, and the first subscribe is refused as a transient (`TRANSIENT_MARK`), which
/// `feed_gate::survivable_refusal` turns into a held feed when no `demo`/`live` account of the venue runs.
#[derive(Clone)]
pub struct DatahubProbe(Arc<dyn Fn() -> Option<String> + Send + Sync>);

impl DatahubProbe {
    /// A probe answering with `check`.
    pub fn new(check: impl Fn() -> Option<String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }

    /// `None` when the datahub's strategy lane can be dialled, else why not.
    #[must_use]
    pub fn check(&self) -> Option<String> {
        (self.0)()
    }
}

impl std::fmt::Debug for DatahubProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DatahubProbe")
    }
}

/// Production's constructor: the dial to the datahub's strategy lane (`StratOpener`), at the address
/// and under the `Scope::Trade` key `live_runtime` resolved once.
pub struct ProdFeedCtors {
    addr: String,
    /// Where the tradehub's strategy-lane key (`credential_keys::datahub_trade_key()`, held in the hub-key store)
    /// is read from, ASKED AGAIN at every dial and every probe: `None` when the store holds none. A start with
    /// no key is held with the reason and the late arm polls the probe, so a key minted AFTERWARDS must be seen
    /// by the next poll (a key captured at start could never be, and the process would sit "waiting" for ever).
    /// `Debug` shows nothing of it (`HubKeys` redacts), and this type has no `Debug` of its own.
    keys: Arc<dyn Fn() -> Option<HubKeys> + Send + Sync>,
}

impl ProdFeedCtors {
    /// A constructor dialing the datahub at `addr` with `trade_key` (`None`: the store holds no
    /// strategy-lane key, and every dial refuses by name — absent credentials are the gate).
    pub fn new(addr: impl Into<String>, trade_key: Option<Vec<u8>>) -> Self {
        let keys = trade_key.map(|k| HubKeys::new(Vec::new(), Vec::new()).with_trade(k));
        Self { addr: addr.into(), keys: Arc::new(move || keys.clone()) }
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
        }
    }
}

/// The phrase that marks a refusal as the MISSING strategy-lane key (the cause nothing but an operator mends).
const MISSING_KEY_MARK: &str = "holds no strategy-lane key";

/// What leads a held feed's cause when a venue's feed was refused by a CAP (`vike_market_stream::market::CAP_MARK`:
/// a pump, session or stream cap that is full): the one cause no restart mends and only an operator does.
pub const FEEDS_CAPPED_MARK: &str = "a cap on the price path is full";

/// **What the ready banner and the log name a held feed's cause with**: the cap that is full and what to change
/// ([`FEEDS_CAPPED_MARK`]), "no strategy-lane key" for the missing key (mended by minting the key: the late arm
/// re-reads the store at every poll), the spent restart budget, else the lane not answering.
#[must_use]
pub fn short_cause(why: &str) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    if let Some(at) = why.find(FEEDS_CAPPED_MARK) {
        let detail = why[at + FEEDS_CAPPED_MARK.len()..].trim_start_matches(": ");
        Cow::Owned(format!(
            "{FEEDS_CAPPED_MARK} ({detail}); an operator must raise that cap or drop a subscription, \
             and no restart is ordered for it"
        ))
    } else if why.starts_with(crate::late_arm::RESTARTS_SPENT_MARK) {
        Cow::Borrowed(
            "the late-arm restart budget is spent: the price path keeps refusing (restart by hand once it is fixed)",
        )
    } else if why.contains(MISSING_KEY_MARK) {
        Cow::Borrowed("no strategy-lane key")
    } else {
        Cow::Borrowed("the price path (datahub or gatewayhub) does not answer")
    }
}

/// The refusal a missing strategy-lane key is, for `who` (a venue's feed, or the probe's "the price feeds").
fn missing_key_text(who: &str) -> String {
    format!(
        "{who}: this daemon takes its prices from the datahub's strategy lane and {MISSING_KEY_MARK} \
         ({key}) in the hub-key store: mint it on the \
         datahub's box with `vike-cli datahub setup --strategy-lane` and copy it here (the tradehub \
         notices a key minted later and restarts itself armed; otherwise restart it). Nothing was dialled",
        key = vike_venues::credential_keys::datahub_trade_key()
    )
}

impl FeedCtors for ProdFeedCtors {
    fn strat(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<StratFeed, String> {
        let Some(keys) = (self.keys)() else {
            return Err(missing_key_text(venue));
        };
        let opener =
            StratOpener::new(self.addr.clone(), keys, StratTimings::default()).on_network(network);
        StratFeed::new(opener, venue, sink).map_err(|e| {
            format!("{venue}: the strategy-lane dial could not start its reader thread: {e}")
        })
    }

    /// The dial and the authenticated handshake of a strategy-lane connection, then the features it advertised
    /// read for every mounted venue, dropped at once: it opens no lane (`StratOpen` is never sent), so it takes
    /// no connection slot at the datahub. No key is a refusal by name with nothing dialled, and the key store is
    /// read again at every ask (a key minted later is found).
    fn datahub_probe(&self, venues: &[String]) -> Option<DatahubProbe> {
        let addr = self.addr.clone();
        let keys = Arc::clone(&self.keys);
        let venues = venues.to_vec();
        Some(DatahubProbe::new(move || {
            let Some(keys) = keys() else {
                return Some(missing_key_text("the price feeds"));
            };
            let client = match vike_datahub_client::DatahubClient::connect_strat_lane(
                &addr,
                &keys,
                |_| {},
            ) {
                Ok(client) => client,
                Err(e) => return Some(format!("datahub {addr}: strategy lane: {e}")),
            };
            // A datahub that cannot serve a mounted venue's lane (an older or smaller build mid-deploy) is
            // found here, with its words, and not at the first subscribe after a core exists.
            venues
                .iter()
                .find_map(|venue| client.check_strat_lane(venue).err())
                .map(|e| format!("datahub {addr}: strategy lane: {e}"))
        }))
    }
}

/// Wire ONE venue's live market feed(s) onto the core's ingest lanes — `live_runtime`'s feed arms,
/// called once per DISTINCT started venue (split-plane I10). `cfgs` is every start on this venue,
/// in start order (never empty; the FIRST supplies the announcement fields and the venue-wide
/// knobs, e.g. the CEX pump's book grid); subscriptions dedup per series, so two starts on one
/// series subscribe once. `make` is the feed-construction seam ([`FeedCtors`]) — production
/// passes [`ProdFeedCtors`], the dial to the datahub's strategy lane; the deterministic splice test
/// substitutes a scripted constructor.
///
/// `data_only` is the profile's declared data-plane-only venue set
/// ([`crate::config::DaemonProfile`]'s `data_only` key, collected per venue by
/// `live_runtime`'s withhold pass) — consulted ONLY by the four credentialed-data arms' arming
/// DISCLOSURE: a declared venue whose exec really stayed paper announces the declaration
/// ([`data_only_arming`]) instead of that venue's "bug to report"/"restart" remedy, which would
/// be false advice about a state the operator asked for. Empty (every undeclared start) leaves
/// every banner byte-identical.
///
/// `venue_arming` is `vike_tradehub_core::venue_arming`'s row set — the SAME `arming` binding
/// `live_runtime_with` journals from, computed after the withhold and before `vars` moves into the
/// `RuntimeConfig`. It is here for ONE job: [`other_live_accounts`], which needs the rows in order to
/// spell a labelled account's route key through `VenueArming::route_key` rather than parsing one.
/// EMPTY leaves every badge at its per-venue answer, which is why
/// `crates/vike-tradehub/tests/daemon/account_badge_wiring_pin.rs` pins the call site rather than
/// trusting the parameter to be passed.
///
/// `mount_accounts` is the account each of this venue's mounts TRADES (`MountSpec::account`, the
/// default account for a mount naming none). `live_venues` is keyed by ACCOUNT route key, so the
/// arms' `exec` badge asks for those keys and not for the bare venue id, which is the default
/// account's alone (decision 0172).
///
/// There is no `venue_settings` parameter any more: the mark-stream and socket-batching rows
/// (`vike_secrets::venue_setting`) configure the venue client, which the gatewayhub builds and reads
/// them for (decision 0095).
///
/// This function is the ORCHESTRATOR: one arm per [`VenuePlan`] variant, each calling the phase
/// functions below it in the order the arm's statements always ran. Every venue but hyperliquid and
/// polymarket gets `wire_<venue>_feed` (build the feed object and subscribe its lanes — every `?`
/// in it is a refusal to start, returned from here at the point it always was) and then
/// `announce_<venue>_arming` (the exec-live-or-paper disclosure); the CEX feed half is two lanes,
/// `wire_cex_kline_lane` then `spawn_cex_tick_pump`.
pub fn wire_venue_feeds(
    plan: &VenuePlan,
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<LiveFeeds, String> {
    let cfg = cfgs[0];
    let feeds = match plan {
        VenuePlan::Hyperliquid(network) => LiveFeeds::Hyperliquid(wire_hyperliquid_feed(
            network,
            cfg,
            cfgs,
            handle,
            live_venues,
            wrap,
            make,
        )?),
        VenuePlan::Polymarket => {
            LiveFeeds::Polymarket(wire_polymarket_feeds(cfgs, handle, live_venues, wrap, make)?)
        }
        VenuePlan::Cex { venue, mainnet } => {
            let slug = venue.slug();
            let mut feed = wire_cex_kline_lane(slug, cfgs, handle, wrap, make)?;
            spawn_cex_tick_pump(venue, cfg, &mut feed)?;
            announce_cex_arming(
                venue,
                mainnet,
                slug,
                cfg,
                cfgs,
                live_venues,
                venue_arming,
                mount_accounts,
            );
            LiveFeeds::Cex(feed)
        }
        VenuePlan::Alpaca => {
            let client = wire_alpaca_feed(cfgs, handle, wrap, make)?;
            announce_alpaca_arming(cfg, cfgs, live_venues, venue_arming, mount_accounts, data_only);
            LiveFeeds::Alpaca(client)
        }
        VenuePlan::Ctrader => {
            let data = wire_ctrader_feed(cfg, cfgs, handle, wrap, make)?;
            announce_ctrader_arming(
                cfg,
                cfgs,
                live_venues,
                venue_arming,
                mount_accounts,
                data_only,
            );
            LiveFeeds::Ctrader(data)
        }
        VenuePlan::Oanda => {
            let feeds = wire_oanda_feed(cfgs, handle, wrap, make)?;
            announce_oanda_arming(cfg, cfgs, live_venues, venue_arming, mount_accounts, data_only);
            LiveFeeds::Oanda(feeds)
        }
        VenuePlan::Deribit => {
            let feeds = wire_deribit_feed(cfgs, handle, wrap, make)?;
            announce_deribit_arming(cfg, cfgs, live_venues, venue_arming, mount_accounts);
            LiveFeeds::Deribit(feeds)
        }
        VenuePlan::Ig => {
            let feeds = wire_ig_feed(cfgs, handle, wrap, make)?;
            announce_ig_arming(cfg, cfgs, live_venues, venue_arming, mount_accounts, data_only);
            LiveFeeds::Ig(feeds)
        }
    };
    Ok(feeds)
}

/// A CEX venue's PRICE-ONLY feed: the strategy-lane dial and the price tap with NO subscription, for a venue
/// no mount names. Its [`LiveFeeds::price_watcher`] is how the control door prices a symbol it admits on
/// that venue (decision 0174): without it the door answers `this node runs no price feed for <venue>` and
/// refuses every foreign symbol of an account whose venue has no mount. Construction never dials on the
/// caller's thread and the dial parks idle while nothing is held, so an unused feed costs no subscription.
pub fn wire_cex_price_feed(
    venue: &crate::CexVenue,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<LiveFeeds, String> {
    wire_cex_kline_lane(venue.slug(), &[], handle, wrap, make).map(LiveFeeds::Cex)
}

/// The network of Hyperliquid's price-only feed: the one the mounted arm would dial for the same rows
/// ([`feed_tier`]: mainnet when a `live` account is armed, else the test network).
pub fn hl_price_network(table: &ArmingTable, policy: &AdapterPolicy) -> HlNetwork {
    HlNetwork::for_tier(feed_tier("hyperliquid", table, policy))
}

/// Is `slug` a PRICE-ONLY venue: one the control door admits symbols for (`admitting`) that no mount names.
fn is_price_only(slug: &str, admitting: &[String], mounted: &[String]) -> bool {
    admitting.iter().any(|a| a == slug) && !mounted.iter().any(|m| m == slug)
}

/// **The price-only venues, as the start knows them before any core exists**: the CEX venues, Hyperliquid
/// and deribit that the door admits for and no mount names, whether or not the datahub later serves a strategy lane for
/// them ([`wire_price_only_feeds`] asks that per venue, and a venue it skips just never discloses anything).
/// The ONE set the feed wiring, the link dead-man's arming and the core's price-only sweep all read.
pub fn price_only_venues(admitting: &[String], mounted: &[String]) -> Vec<String> {
    CexVenue::ALL
        .iter()
        .map(|v| v.slug())
        .chain(["hyperliquid", "deribit"])
        .filter(|slug| is_price_only(slug, admitting, mounted))
        .map(str::to_string)
        .collect()
}

/// What [`wire_price_only_feeds`] built.
pub struct PriceOnlyFeeds {
    /// The feeds, for the teardown (they are released with the mounts' feeds, in order).
    pub feeds: Vec<LiveFeeds>,
    /// The door's price handle per venue (`LeaseFacts::bind_prices`).
    pub watchers: std::collections::HashMap<String, CexWatcher>,
    /// The network Hyperliquid's price-only feed was wired for, `None` when it was not wired.
    pub hl_network: Option<HlNetwork>,
}

/// **The PRICE-ONLY feeds of the venues the control door admits symbols for (`admitting`) that no mount names
/// (`mounted`)**: the CEX venues, Hyperliquid when `hl_network` is `Some`, and deribit. The control door prices
/// a symbol it admits through its venue's watcher, and a mount's feed is the only other source of one, so a box
/// that mounts bybit alone refused every foreign binance, okx, aster, hyperliquid or deribit symbol. The core routes a venue's
/// quote, trade and book to that venue's engine without a mount, so a dial with nothing subscribed is all the
/// door needs. Each venue is asked of the datahub first (`FeedCtors::datahub_probe`: is a strategy lane
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
    hl_network: Option<HlNetwork>,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> PriceOnlyFeeds {
    let mut out = PriceOnlyFeeds {
        feeds: Vec::new(),
        watchers: std::collections::HashMap::new(),
        hl_network: None,
    };
    let candidates = CexVenue::ALL
        .iter()
        .map(|v| (v.slug(), Some(v)))
        .chain([("hyperliquid", None), ("deribit", None)])
        .filter(|(slug, _)| is_price_only(slug, admitting, mounted));
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
            (Some(venue), _, _) => wire_cex_price_feed(venue, handle, wrap, make),
            (None, "hyperliquid", Some(network)) => wire_hl_price_feed(network, handle, wrap, make),
            (None, "hyperliquid", None) => continue,
            (None, _, _) => wire_deribit_price_feed(handle, wrap, make),
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
    network: &HlNetwork,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<LiveFeeds, String> {
    hyperliquid::wire_hyperliquid_price_feed(network, handle, wrap, make)
        .map(LiveFeeds::Hyperliquid)
}

/// Deribit's PRICE-ONLY feed, for a box that mounts no deribit market (the twin of [`wire_hl_price_feed`]):
/// the dial and the price tap, nothing subscribed. ⚠ It reads deribit's PUBLIC MAINNET host while exec signs
/// on the testnet (`deribit_arm`), so the price is a mainnet reference for a testnet account; which host is
/// dialled is the gatewayhub's, unchanged here.
pub fn wire_deribit_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<LiveFeeds, String> {
    deribit::wire_deribit_price_feed(handle, wrap, make).map(LiveFeeds::Deribit)
}

#[cfg(test)]
#[path = "feeds/ctors_tests.rs"]
mod ctors_tests;

#[cfg(test)]
#[path = "feeds/quote_feed_tests.rs"]
mod quote_feed_tests;
