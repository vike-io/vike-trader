//! Feed wiring: which venues get a live market feed, the per-mount venue-feed dispatch
//! (`venue_feed_plan`), and subscribing those feeds onto the core's ingest lanes
//! (`wire_venue_feeds`). `live_mount` (in `src/tradehub_cli/live_mount.rs`) calls into this module once per
//! distinct mounted venue.
//!
//! [`LiveFeeds`]/[`CexBars`]/[`CexTicks`] (the feed-object types) and the [`FeedCtors`]
//! construction seam + its production impl [`ProdFeedCtors`] moved here alongside the functions
//! that name them in their signatures — `recon_feed_statuses_of` takes `&[LiveFeeds]`,
//! `wire_venue_feeds` returns `Result<LiveFeeds, String>` and takes `&dyn FeedCtors` — for the
//! same reason `CexVenue`/`VenuePlan` moved to `types.rs`: a library crate cannot name a type the
//! binary depending on it defines. The `live_mount`/`live_mount_with` of `src/tradehub_cli/live_mount.rs` (which
//! stay there) and the `tradehub_cli::tests` modules such as `feed_splice` reach these through
//! `use vike_tradehub::feeds::{...}` — `use super::*`/`use super::{...}` inside those then see
//! them as if they were still the binary's own private items.
//!
//! ⚠ `venue_feed_plan`'s `match cfg.venue.as_str() { ... }` is text-scanned by
//! `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` to prove `LIVE_WIRED_VENUES`
//! matches this dispatch's arms exactly. That test's `MAIN` constant now points HERE, not at
//! `main.rs` — see that test's own doc for the re-anchor (tradehub-main-split refactor, Task 3).
//!
//! Layout: this file keeps the dispatch (`venue_feed_plan`), the polymarket interval gate, the
//! [`FeedCtors`] seam, `mark_streams_setting` and the ORCHESTRATOR `wire_venue_feeds`; the feed
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
use vike_mount::{MakerMountConfig, MountPolicy};

#[cfg(doc)]
use crate::venue_arming::arming::{data_only_arming, other_live_accounts};
use crate::venue_arming::tier::{cex_mainnet_enabled, feed_tier};
use crate::venue_plan::{alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan};
use crate::{CexVenue, VenuePlan};
// `ResolvedMount`'s only consumer here is `check_poly_token_intervals`, itself gated on this same
// feature — an unconditional import would go unused (and `-D warnings` would refuse it) on a
// default build.
#[cfg(feature = "polymarket")]
use crate::ResolvedMount;
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
/// the testnet and the live tier is mainnet (`vike_hyperliquid::config::Env::network` is the
/// bridge's own statement of that mapping, and
/// `crates/vike-tradehub/tests/daemon/feed_network_table.rs`'s
/// `unlabelled_boxes_choose_the_same_feed_network_as_before` holds the two together over every
/// ceiling). The registry row decides the TIER; this only renames it.
fn hl_network(tier: vike_bridge_core::venue_mount::Tier) -> vike_hyperliquid::config::Network {
    match tier {
        vike_bridge_core::venue_mount::Tier::Live => vike_hyperliquid::config::Network::Mainnet,
        vike_bridge_core::venue_mount::Tier::Demo => vike_hyperliquid::config::Network::Testnet,
    }
}

/// The per-venue PRE-BUILD gate: `live_mount`'s venue dispatch, called once per MOUNT (split-plane
/// I10 made it a function — `live_mount` loops it over the mount set, so every mount passes its
/// own venue's refusals, and keeps ONE plan per distinct venue). A venue without an arm here is
/// not live-wired; `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` pins these arms
/// equal to `LIVE_WIRED_VENUES`, both directions, under this build's features.
///
/// `policy` is the mount's OWN [`MountPolicy`] — the ceilings, the `account` table and the
/// `venue_setting` snapshot — which `live_mount_with` builds ahead of its plan loop and later moves
/// into the `NodeConfig` the mount is built from. Which NETWORK a venue's accounts mount on is not
/// read off the ceilings here: for binance/bybit/okx/aster/hyperliquid it is asked of the venue's
/// registry row (`crate::venue_arming::tier::feed_tier`, `cex_mainnet_enabled`), whose `resolve` turns
/// each account's own ceiling and keys into a tier — the same function, over the same policy, that
/// the mount acts on (decision 0095 made the ceiling the network for the first three and
/// hyperliquid).
pub fn venue_feed_plan(
    cfg: &MakerMountConfig,
    vars: &HashMap<String, String>,
    policy: &MountPolicy,
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
            cex_plan(CexVenue::Binance, cfg, cex_mainnet_enabled(CexVenue::Binance, vars, policy))?
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
            cex_plan(CexVenue::Bybit, cfg, cex_mainnet_enabled(CexVenue::Bybit, vars, policy))?
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
            cex_plan(CexVenue::Okx, cfg, cex_mainnet_enabled(CexVenue::Okx, vars, policy))?
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
            // resolves MAINNET-FIRST from credentials (`AsterVenueMount`, no
            // `ASTER_MAINNET` flag exists), only the LIVE tier is configured in practice, and a
            // demo/paper order must rest against the real book to mean anything — the same
            // demo-exec-over-mainnet-prices shape as the three venues above, reached by choice
            // rather than by structure. vike-app's aster mount made the identical choice for the
            // identical reason (its `Feeds::with_env(.., Live)` beside the mainnet-always
            // `AsterCatalog`), while the desktop had one.
            //
            // ⚠⚠ REAL MONEY IN PRACTICE: present `ASTER_LIVE_*` credentials make this a LIVE
            // MAINNET exec mount. This arm is CAPABILITY only — absent credentials still mount
            // paper (the workspace live gate), and nothing here changes any default.
            cex_plan(CexVenue::Aster, cfg, cex_mainnet_enabled(CexVenue::Aster, vars, policy))?
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
            // Safety gate #3 (feed side): the feed dials the network the venue's ARMED accounts'
            // exec dials, and that is asked of the venue's REGISTRY ROW (`feed_tier` — the
            // bridge's own `resolve` over each account's own ceiling and keys), not re-derived from
            // the venue line. It is ONE feed per venue, not per account, so: every armed account
            // on one network -> that network; none armed -> the default account's selected tier
            // (a `live` ceiling with no key is still the mainnet feed); armed accounts on BOTH
            // networks -> MAINNET WINS and `feed_tier` says so at `warn!`. What that guarantees is
            // that the feed is never on testnet while an armed account is on mainnet (real orders
            // priced off the testnet book); it does NOT promise every armed account shares the
            // feed's network — a testnet account beside a live one is priced off the mainnet book,
            // and the warning is how the operator hears it. The start is never refused over it.
            VenuePlan::Hyperliquid(hl_network(feed_tier(&cfg.venue, vars, policy)))
        }
        #[cfg(feature = "polymarket")]
        "polymarket" => {
            // Polymarket is the A-S maker's NATIVE [0,1] price domain — unlike a $-priced crypto perp,
            // whose $64k mid the maker's avellaneda §2.2 [0,1] wall clamp can't quote. It has NO
            // testnet: every live order is real money on Polygon mainnet, gated by POLY_PRIVATE_KEY
            // creds + flags.poly_exec (checked INSIDE build_node) and the Dublin SOCKS egress (US-geo-
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
/// drift. A test impl substitutes a method and receives the arm's OWN `wrap`-ed
/// [`vike_core::CoreLaneSink`] chain plus the arm's own `subscribe_*` calls: everything about the
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
/// this unit struct exists only so `crate::tradehub_cli`'s `live_mount` has a value to pass.
pub struct ProdFeedCtors;

impl FeedCtors for ProdFeedCtors {}

/// `venue`'s stored `mark_streams` row (decision 0095), or `None` — the feed then keeps its charter
/// default (`vike_bridge_core::mark_streams_from`).
fn mark_streams_setting<'a>(
    venue_settings: &'a std::collections::BTreeMap<
        String,
        vike_secrets::venue_setting::VenueSettings,
    >,
    venue: &str,
) -> Option<&'a str> {
    venue_settings
        .get(venue)
        .and_then(|s| s.get(vike_secrets::venue_setting::SettingTier::Any, "mark_streams"))
}

/// Wire ONE venue's live market feed(s) onto the core's ingest lanes — `live_mount`'s feed arms,
/// called once per DISTINCT mounted venue (split-plane I10). `cfgs` is every mount on this venue,
/// in mount order (never empty; the FIRST supplies the announcement fields and the venue-wide
/// knobs, e.g. the CEX pump's book grid); subscriptions dedup per series, so two mounts on one
/// series subscribe once. `make` is the feed-construction seam ([`FeedCtors`]) — production
/// passes [`ProdFeedCtors`], whose defaults construct exactly the inline expressions these arms
/// used to spell; the deterministic splice test substitutes a scripted constructor.
///
/// `data_only` is the profile's declared data-plane-only venue set
/// ([`crate::config::DaemonProfile`]'s `data_only` key, collected per venue by
/// `live_mount`'s withhold pass) — consulted ONLY by the four credentialed-data arms' arming
/// DISCLOSURE: a declared venue whose exec really stayed paper announces the declaration
/// ([`data_only_arming`]) instead of that venue's "bug to report"/"restart" remedy, which would
/// be false advice about a state the operator asked for. Empty (every undeclared mount) leaves
/// every banner byte-identical.
///
/// `venue_arming` is `vike_mount::venue_arming`'s row set — the SAME `arming` binding
/// `live_mount_with` journals from, computed after the withhold and before `vars` moves into the
/// `NodeConfig`. It is here for ONE job: [`other_live_accounts`], which needs the rows in order to
/// spell a labelled account's route key through `VenueArming::route_key` rather than parsing one.
/// EMPTY leaves every badge at its per-venue answer, which is why
/// `crates/vike-tradehub/tests/daemon/account_badge_wiring_pin.rs` pins the call site rather than
/// trusting the parameter to be passed.
///
/// `venue_settings` is every venue's `venue_setting` rows — the mark-stream rows are read here
/// (`mark_streams_setting`); an empty map keeps every feed's charter default.
///
/// This function is the ORCHESTRATOR: one arm per [`VenuePlan`] variant, each calling the phase
/// functions below it in the order the arm's statements always ran. Every venue but hyperliquid and
/// polymarket gets `wire_<venue>_feed` (build the feed object and subscribe its lanes — every `?`
/// in it is a refusal to mount, returned from here at the point it always was) and then
/// `announce_<venue>_arming` (the exec-live-or-paper disclosure); the CEX feed half is two lanes,
/// `wire_cex_kline_lane` then `spawn_cex_tick_pump`.
pub fn wire_venue_feeds(
    plan: &VenuePlan,
    cfgs: &[&MakerMountConfig],
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
