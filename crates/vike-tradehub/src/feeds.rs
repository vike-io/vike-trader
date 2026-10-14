//! Feed wiring: which venues get a live market feed, the per-mount venue-feed dispatch
//! (`venue_feed_plan`), and subscribing those feeds onto the core's ingest lanes
//! (`wire_venue_feeds`). `live_runtime` (in `src/tradehub_cli/live_runtime.rs`) calls into this module once per
//! distinct started venue.
//!
//! [`LiveFeeds`]/[`CexBars`]/[`CexTicks`] (the feed-object types) and the [`FeedCtors`]
//! construction seam + its production impl [`ProdFeedCtors`] moved here alongside the functions
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
//! types and the health map live in `feeds/live.rs`, and each `VenuePlan` arm's phase functions
//! in `feeds/<venue>.rs` (`feeds/cex.rs`: `wire_cex_kline_lane`, `spawn_cex_tick_pump`,
//! `announce_cex_arming`). `LiveFeeds::recon_feed_statuses` is in `feeds/live.rs`.

mod alpaca;
mod cex;
mod ctrader;
mod deribit;
mod hyperliquid;
mod ig;
mod live;
mod oanda;
#[cfg(feature = "polymarket")]
mod polymarket;

use std::collections::HashMap;
use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_secrets::account_credentials::AccountCredentials;
use vike_tradehub_core::{AdapterPolicy, MakerConfig};

#[cfg(doc)]
use crate::venue_arming::arming::{data_only_arming, other_live_accounts};
use crate::venue_arming::tier::{cex_mainnet_enabled, feed_tier};
use crate::venue_plan::{alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan};
use crate::{CexVenue, VenuePlan};
// `ResolvedAdapter`'s only consumer here is `check_poly_token_intervals`, itself gated on this same
// feature — an unconditional import would go unused (and `-D warnings` would refuse it) on a
// default build.
#[cfg(feature = "polymarket")]
use crate::ResolvedAdapter;
use alpaca::{announce_alpaca_arming, wire_alpaca_feed};
use cex::{announce_cex_arming, spawn_cex_tick_pump, wire_cex_kline_lane};
use ctrader::{announce_ctrader_arming, wire_ctrader_feed};
use deribit::{announce_deribit_arming, wire_deribit_feed};
use hyperliquid::wire_hyperliquid_feed;
use ig::{announce_ig_arming, wire_ig_feed};
pub use live::{CexBars, CexTicks, LiveFeeds, LiveTeardown, PostFeeds, recon_feed_statuses_of};
use oanda::{announce_oanda_arming, wire_oanda_feed};
#[cfg(feature = "polymarket")]
use polymarket::wire_polymarket_feeds;

/// A registry tier, spelled the way the hyperliquid feed constructor takes it: the demo tier is
/// the testnet and the live tier is mainnet, through the bridge's own statement of that mapping
/// (`vike_hyperliquid::base::config::Network::for_tier`; the grid in
/// `crates/vike-tradehub/tests/daemon/feed_network_table.rs` holds the two together over every
/// account tier). The registry row decides the TIER; this only renames it.
fn hl_network(tier: vike_model::AccountTier) -> vike_hyperliquid::base::config::Network {
    vike_hyperliquid::base::config::Network::for_tier(tier)
}

/// **The credentials a CREDENTIALED-DATA feed authenticates with** (alpaca, ctrader, oanda, ig):
/// the venue's DEFAULT account, read out of the start policy's own `account` snapshot
/// (`AccountDirectory::data_feed_credentials`): its `demo` row — the account, and the one tier, each
/// of those venues' demo-only exec arm binds — or, for a `data_only` venue, the `paper` account
/// that stands behind its strategy (decision 0156) and holds the venue's demo login for the feed
/// (decision 0156 item 6: a credential belongs to its account, and no name is composed to find
/// it). No such ACTIVE row, or two of them, or a row holding nothing, is an EMPTY view, and the
/// venue's plan refuses the start naming the fields.
fn demo_feed_credentials<'p>(policy: &'p AdapterPolicy, venue: &str) -> AccountCredentials<'p> {
    policy.accounts.data_feed_credentials(venue)
}

/// One feed arm: a venue's per-mount PRE-BUILD plan (see [`venue_feed_plan`]).
type FeedArm =
    fn(&MakerConfig, &HashMap<String, String>, &AdapterPolicy) -> Result<VenuePlan, String>;

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
/// behind the `fxcm` feature: `vike_tradehub_core::wired_markets` keeps the row only where the registry starts it), so an fxcm-linked daemon now starts a real FXCM execution engine and
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
    // ⚠ CREDENTIALED MARKET DATA (split-plane I9): alpaca's feed is a real `DataClient` but it
    // AUTHENTICATES (the OAuth2 sandbox client-credentials trio of the default `demo` account) —
    // there is no keyless price stream to fall back to, so `venue_feed_plan`'s alpaca arm REFUSES
    // the live start outright when the trio is absent rather than starting a feed-less core that
    // quotes into the void. Exec stays sandbox-pinned in `crates/bridges/alpaca/src/adapter.rs`'s
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
    // is `CtraderData` over its own dedicated protobuf socket, the default `demo` account's
    // credentials required (`CtraderConfig::from_credentials(Demo, …)` — absent creds refuse the
    // live start, same argument as alpaca above), and `conn::connect_and_auth` connects AT START — a data connect failure
    // refuses daemon startup, unlike the exec side's session-long demote-to-paper.
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
    #[cfg(feature = "polymarket")]
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
/// start is built from. The four credentialed-data arms read their feed's credentials out of it
/// (`demo_feed_credentials`); `vars` is the DEPLOYMENT map (folded flags, attribution codes) the
/// registry rows are asked with, and carries no account's credential. Which NETWORK a venue's
/// accounts start on is not read off the account rows here: for binance/bybit/okx/aster/hyperliquid
/// it is asked of the venue's registry row (`crate::venue_arming::tier::feed_tier`,
/// `cex_mainnet_enabled`), whose `resolve` turns each account's own tier and keys into a tier — the
/// same function, over the same policy, that the start acts on (decision 0095 made the tier the
/// network for the first three and hyperliquid; decision 0119 made it the ACCOUNT's own tier).
pub fn venue_feed_plan(
    cfg: &MakerConfig,
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    match FEED_ARMS.iter().find(|(venue, _)| *venue == cfg.venue) {
        Some((_, arm)) => arm(cfg, vars, policy),
        None => Err(format!(
            "venue '{}' is not wired for live yet (this build can mount: {}); refusing to mount",
            cfg.venue,
            feed_arm_list()
        )),
    }
}

fn binance_arm(
    cfg: &MakerConfig,
    vars: &HashMap<String, String>,
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
    cex_plan(CexVenue::Binance, cfg, cex_mainnet_enabled(CexVenue::Binance, vars, policy))
}

fn bybit_arm(
    cfg: &MakerConfig,
    vars: &HashMap<String, String>,
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
    cex_plan(CexVenue::Bybit, cfg, cex_mainnet_enabled(CexVenue::Bybit, vars, policy))
}

fn okx_arm(
    cfg: &MakerConfig,
    vars: &HashMap<String, String>,
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
    cex_plan(CexVenue::Okx, cfg, cex_mainnet_enabled(CexVenue::Okx, vars, policy))
}

fn aster_arm(
    cfg: &MakerConfig,
    vars: &HashMap<String, String>,
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
    cex_plan(CexVenue::Aster, cfg, cex_mainnet_enabled(CexVenue::Aster, vars, policy))
}

fn alpaca_arm(
    cfg: &MakerConfig,
    _vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // US EQUITY (`AAPL` — `VENUE_WIRING` pins it), the first CREDENTIALED-DATA arm
    // (split-plane I9): NOT the CEX two-lane shape. There is no keyless pump —
    // `AlpacaDataClient` is one multiplexed, OAuth-authenticated WS client serving real 1m
    // bar CLOSES (the watchdog cadence), L1 quotes (a native channel behind
    // `on_quote_tick`) and trade prints, all through `CoreLaneSink`. The plan therefore
    // carries the resolved sandbox config (the policy moves into the `RuntimeConfig` before
    // feed time), and ABSENT credentials REFUSE the start here rather than degrading:
    // exec-degrades-to-paper still leaves a working start, but a feed-less start quotes
    // into the void forever. Data hosts follow the SAME tier the exec side is pinned to
    // (the sandbox — the bridge arms only a `demo` account, and no `ALPACA_MAINNET` flag
    // exists), so unlike the CEX arms this venue's demo start is DEMO EXEC OVER SANDBOX
    // PRICES — the venue serves a sandbox data stream, which the CEX venues do not.
    alpaca_plan(cfg, &demo_feed_credentials(policy, "alpaca"))
}

fn ctrader_arm(
    cfg: &MakerConfig,
    _vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Result<VenuePlan, String> {
    // FX (`EURUSD` — `VENUE_WIRING` pins it), the second credentialed-data arm
    // (split-plane I9): one DEDICATED protobuf/TLS data socket (`connect_and_auth`,
    // SYNCHRONOUS at start — wire failure refuses startup, see `wire_venue_feeds`),
    // isolated from the exec socket `make_engine` opens, per the same isolation rule every
    // recon client follows. Serves L1 spot quotes; live trendbars only ever FORM through
    // this client (no close fires), so the arm synthesizes closed bars from quote mids via
    // `MakerSink` — the polymarket shape — and the DEMO host follows the exec side's own
    // DEMO-tier pin. Absent credentials refuse the start (same argument as alpaca above).
    ctrader_plan(cfg, &demo_feed_credentials(policy, "ctrader"))
}

fn oanda_arm(
    cfg: &MakerConfig,
    _vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
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
    oanda_plan(cfg, &demo_feed_credentials(policy, "oanda"))
}

fn deribit_arm(
    cfg: &MakerConfig,
    _vars: &HashMap<String, String>,
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
    _vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
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
    ig_plan(cfg, &demo_feed_credentials(policy, "ig"))
}

fn hyperliquid_arm(
    cfg: &MakerConfig,
    vars: &HashMap<String, String>,
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
    Ok(VenuePlan::Hyperliquid(hl_network(feed_tier(&cfg.venue, vars, policy))))
}

#[cfg(feature = "polymarket")]
fn polymarket_arm(
    _cfg: &MakerConfig,
    _vars: &HashMap<String, String>,
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
#[cfg(feature = "polymarket")]
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
/// **Production is byte-identical by construction, not by review**: every method's DEFAULT body
/// is exactly the constructor expression the arm spelled inline before this trait existed, and
/// [`ProdFeedCtors`] (the one production impl) overrides nothing — there is no second spelling to
/// drift. A test impl substitutes a method and receives the arm's OWN `wrap`-ed
/// [`vike_tradehub_core::CoreLaneSink`] chain plus the arm's own `subscribe_*` calls: everything about the
/// splice except the venue's socket. `src/tradehub_cli/tests/feed_splice.rs` is that test.
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
    /// (`src/tradehub_cli/tests/feed_splice.rs`'s oanda case) substitutes a scripted constructor here AND
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
/// this unit struct exists only so `crate::tradehub_cli`'s `live_runtime` has a value to pass.
pub struct ProdFeedCtors;

impl FeedCtors for ProdFeedCtors {}

/// Wire ONE venue's live market feed(s) onto the core's ingest lanes — `live_runtime`'s feed arms,
/// called once per DISTINCT started venue (split-plane I10). `cfgs` is every start on this venue,
/// in start order (never empty; the FIRST supplies the announcement fields and the venue-wide
/// knobs, e.g. the CEX pump's book grid); subscriptions dedup per series, so two starts on one
/// series subscribe once. `make` is the feed-construction seam ([`FeedCtors`]) — production
/// passes [`ProdFeedCtors`], whose defaults construct exactly the inline expressions these arms
/// used to spell; the deterministic splice test substitutes a scripted constructor.
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
/// `venue_settings` is every venue's `venue_setting` rows — the mark-stream rows are read here
/// (`vike_secrets::venue_setting::mark_streams_setting`); an empty map keeps every feed's
/// charter default.
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
    data_only: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
    venue_settings: &std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
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
            venue_settings,
        )?),
        #[cfg(feature = "polymarket")]
        VenuePlan::Polymarket => {
            LiveFeeds::Polymarket(wire_polymarket_feeds(cfgs, handle, live_venues, wrap)?)
        }
        VenuePlan::Cex { venue, mainnet } => {
            let slug = venue.slug();
            let bars = wire_cex_kline_lane(venue, slug, cfgs, handle, wrap, venue_settings)?;
            let pump = spawn_cex_tick_pump(venue, cfg, handle);
            announce_cex_arming(venue, mainnet, slug, cfg, cfgs, live_venues, venue_arming);
            LiveFeeds::Cex { ticks: Some(pump), bars }
        }
        VenuePlan::Alpaca(config) => {
            let client = wire_alpaca_feed(config, cfgs, handle, wrap)?;
            announce_alpaca_arming(cfg, cfgs, live_venues, venue_arming, data_only);
            LiveFeeds::Alpaca(client)
        }
        VenuePlan::Ctrader(config) => {
            let data = wire_ctrader_feed(config, cfg, cfgs, handle, wrap)?;
            announce_ctrader_arming(cfg, cfgs, live_venues, venue_arming, data_only);
            LiveFeeds::Ctrader(data)
        }
        VenuePlan::Oanda(config) => {
            let feeds = wire_oanda_feed(config, cfgs, handle, wrap, make)?;
            announce_oanda_arming(cfg, cfgs, live_venues, venue_arming, data_only);
            LiveFeeds::Oanda(feeds)
        }
        VenuePlan::Deribit => {
            let feeds = wire_deribit_feed(cfgs, handle, wrap, make)?;
            announce_deribit_arming(cfg, cfgs, live_venues, venue_arming);
            LiveFeeds::Deribit(feeds)
        }
        VenuePlan::Ig(config) => {
            let feeds = wire_ig_feed(config, cfgs, handle, wrap)?;
            announce_ig_arming(cfg, cfgs, live_venues, venue_arming, data_only);
            LiveFeeds::Ig(feeds)
        }
    };
    Ok(feeds)
}

#[cfg(test)]
mod cex_row_admission {
    //! **The CEX reconcile-health row is CONDITIONAL, and this is the rule half of the condition.**
    //!
    //! On 2026-09-10 that row suppressed bybit's reconcile leg 2,516 times over 42 hours while the
    //! venue was healthy. The producer defect is fixed in
    //! `vike_bridge_core::market_pump`'s `SessionStatus`, but the row's remaining sharp edge is
    //! WRITE-FREQUENCY ASYMMETRY (`vike_tradehub::reconcile_config`'s `health_from_feed_status` argues
    //! it): a healthy lane writes its string once per SESSION, a faulting one once per BACKOFF
    //! CYCLE, so on a handle shared by two lanes a single permanently-broken lane owns the string
    //! essentially always and a once-a-minute gate reads `Degraded` essentially always. That is the
    //! incident reproduced, after the fix, on a `.P` symbol or a second interval.
    //!
    //! So the row is withheld whenever more than one lane writes the handle. See
    //! [`super::cex_status_row_is_unambiguous`] for why the rule and the count are proven
    //! separately.

    use super::live::cex_status_row_is_unambiguous;

    #[test]
    fn one_writer_is_evidence_and_two_are_not() {
        assert!(
            cex_status_row_is_unambiguous(0),
            "an unsubscribed handle still holds the `connecting to …` seed, which parses Healthy"
        );
        assert!(cex_status_row_is_unambiguous(1), "the deployed shape: spot, one interval");
        assert!(
            !cex_status_row_is_unambiguous(2),
            "a `.P` symbol pairs a mark lane onto the SAME mutex; a faulting mark lane would then \
             hold the string against a healthy kline sibling and suppress every pass — the \
             incident, one settings line away"
        );
        assert!(!cex_status_row_is_unambiguous(5), "…and more lanes are no better");
    }
}
