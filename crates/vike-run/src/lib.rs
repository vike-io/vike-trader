//! `vike-run` — the run/composition layer: mount a strategy on the PRODUCTION live core and feed it.
//!
//! First mount (this module): the Avellaneda–Stoikov [`vike_mm::SpreadMaker`] making a market on a
//! Polymarket outcome token, validated through the REAL runtime — [`vike_core::spawn_core`] with a
//! [`vike_core::StrategyMount`] + the **paper exchange** ([`vike_paper::PaperExecutionClient`])
//! as the `ExecutionClient` + the Polymarket tick feed. This exercises the maker on the exact live-core
//! path a real venue mount uses (single-writer runtime, ingest lanes, RiskGate, the one live order
//! path), with ZERO real-money / credential / geo risk: the paper client fills the maker's resting
//! book with the backtest fill semantics instead of a venue.
//!
//! ## The wiring, in one place
//! Gamma catalog (pick a market → outcome `token_id` + `end_date`) → [`MakerMountConfig`] with the
//! A-S `resolution_ts` set from `end_date` ([`vike_polymarket::GammaMarket::resolution_ts_ms`]) →
//! [`build_paper_maker_core`] (`spawn_core` + `StrategyMount(SpreadMaker)` + `PaperExecutionClient`)
//! → [`MakerSink`] bridges the Polymarket [`vike_data::LiveDataSink`] feed onto the core's tick lane
//! (`quote`/`trade`/`book` → `handle.tick_sender()`).
//!
//! ## Why a tick→bar bridge (the one non-obvious piece)
//! The maker QUOTES on ticks (`on_quote_tick`/`on_order_book`), but `PaperExecutionClient` FILLS on
//! CLOSED BARS (`on_bar`, next-open discipline — the R7 paper law). Polymarket serves ticks, not
//! candles, so the mount SYNTHESIZES bars from the tick stream ([`TickBarSynthesizer`]): it folds the
//! feed mid into an event-time OHLC window and emits a closed [`vike_model::Bar`] on each interval
//! boundary, which drives the paper book's fills. This is pure WIRING — it changes neither the
//! `DataClient` nor the `ExecutionClient` seam — so the composition holds (no seam change was needed).
//! A maker resting inside the spread gets FILLED exactly when the market oscillates through its quote
//! within a bar (a dip-and-recover fills the resting bid; a spike-and-fall fills the resting ask) —
//! the realistic maker fill.
//!
//! ## Running it LIVE against real Polymarket (US-geo-blocked)
//! See the `polymarket_maker_paper` bin. Polymarket's CLOB is US-geo-blocked, so the REAL feed must
//! be reached from the user's **Dublin EU AWS host** (the same route the Gamma catalog + live smokes
//! use). The mount is identical either way — only the feed differs: the live path wires the real
//! [`vike_polymarket::Feeds`] into the SAME [`MakerSink`] the offline test drives with a scripted
//! feed. Everything under `#[cfg(feature = "polymarket")]` (the live [`live`] module + the bin) is the
//! only network-touching surface; the mount core + the offline test are feature-free and run anywhere.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_core::{spawn_core, spawn_core_multi, CoreConfig, CoreHandle, StrategyMount};
use vike_data::LiveDataSink;
use vike_exec::{
    Account, BalanceMode, BarSender, BarUpdate, ExecutionClient, ExecutionEngine, RiskGate,
    RiskLimits,
};
// `SpreadMaker` itself is RE-EXPORTED below rather than imported here — one `use` for both, so the
// name this module builds with is exactly the name a caller receives.
use vike_mm::QuoteStyle;
use vike_model::{
    AsParams, Bar, HorizonMode, L2Book, PriceDomain, QuoteTick, RefreshTolerance, RewardParams,
    ToxicityParams, TradeTick, VarianceMode,
};
use vike_paper::PaperExecutionClient;

// Re-exports so a caller (bin / test) names the whole mount surface from `vike_run::…`.
/// The concrete maker [`build_maker`] returns. Re-exported because a caller that builds the A-S
/// maker through this crate must be able to NAME what came back — `vike-tradehub` holds it in an
/// `-> Option<SpreadMaker>` so its default mount and its `[strategy] name = "spread_maker"` mount
/// are provably ONE construction — without taking a direct `vike-mm` edge to do it. Costs nothing:
/// vike-mm is already in every consumer's tree through this crate.
pub use vike_mm::SpreadMaker;
pub use vike_paper::PaperFill;

#[cfg(feature = "polymarket")]
pub mod live;

/// The `incident` subcommand's evidence-bundle collector (drives the `incident` bin). Feature-free
/// — it touches no venue/native code, so it builds in the default/CI lane. See the module docs.
pub mod incident;

/// PR-8 (Layer 2): the twelve-venue node assembly ([`build_node`]/[`NodeConfig`]/[`Node`]), moved
/// verbatim out of `vike-app/src/main.rs` so a non-GUI caller (the PR-9 daemon) and the GUI both
/// stand up the same live core from ONE place — including the startup preflight both now inherit.
/// See the module doc for the feed/recon boundary.
pub mod node;
pub use node::{
    account_symbols_for, armed_live_venues, build_node, build_node_with_preflight,
    journal_venue_mounts, mount_accounts, refuse_unarmed_live_venues, MountAccount, Node,
    NodeConfig, NodeError, MOUNT_FAILED, WIRED_MARKETS,
};
/// The startup preflight's report type, re-exported because it is now a PARAMETER of this crate's
/// public API ([`build_node_with_preflight`], [`build_live_strategy_core_with_preflight`]) — a
/// caller that cannot name the type cannot call them, and vike-tradehub has no vike-mount edge of
/// its own. Deliberate API surface, not a compatibility shim for a moved symbol.
pub use vike_mount::preflight::PreflightReport;
/// The machine's hard ceilings as a venue mount applies them ([`NodeConfig::policy`],
/// settings-unification Phase 6c). Re-exported from `vike-mount` so a binary that builds a
/// `NodeConfig` — `vike-app`, `vike-tradehub` — names it through the crate it already depends on
/// rather than taking a direct `vike-mount` dependency just to fill one field.
pub use vike_mount::MountPolicy;

/// The arming PRODUCER, beside the consumer that demands its output.
///
/// [`journal_venue_mounts`] takes `&[VenueArming]` precisely so a caller must compute the rows at a
/// moment it chooses (see that function's own note on the two credential maps `live_mount_with`
/// holds). That makes the producer part of THIS crate's contract: a consumer told to bring rows and
/// given no way to name or make them would have to take a `vike-mount` edge purely for the call.
/// `vike-tradehub` is exactly that consumer.
pub use vike_mount::{VenueArming, VenueMode};

/// [`vike_mount::venue_arming`], reached through this crate.
///
/// ⚠ **It supplies NOTHING any more, and saying so is the point.** This was a wrapper rather than
/// a re-export because the producer took a SYMBOL per venue and [`WIRED_MARKETS`] — which lives
/// here, below-layer `vike-mount` cannot see it — was the table that had to be handed over. That
/// parameter existed for the SYMBOL-COLLISION rule, and the rule is deleted
/// (`vike_config::venue_accounts` carries the correction: two accounts on one instrument is an
/// ordinary spread). An arming row is now a fact about ONE account, so there is no table left to
/// supply and this function's signature is identical to the one it calls.
///
/// It survives as a pass-through for call-site stability alone — four callers plus
/// `crates/vike-tradehub/tests/daemon/account_badge_wiring_pin.rs`, which pins the tradehub's call
/// by its literal source text. ⚠ So do NOT cite it as a safety seam: it can no longer make the
/// screen and the mount agree about anything, because it no longer decides anything. Reaching
/// `vike_mount::venue_arming` directly is not a hazard, it is the same call. What still DOES need
/// this crate's table is [`account_symbols_for`] — the map each account's engine is mounted on —
/// and that is the function to guard, not this one. Delete this wrapper the next time its callers
/// are touched for another reason, or give it back a job.
#[must_use]
pub fn venue_arming(
    vars: &std::collections::HashMap<String, String>,
    policy: &vike_mount::VenuePolicy,
) -> Vec<VenueArming> {
    vike_mount::venue_arming(vars, policy)
}

/// The CROSS-EXCHANGE maker mount ([`vike_mm::XemmMaker`]): rest on one venue, hedge on another.
/// A SIBLING of the single-venue mount in this module rather than an extension of it —
/// [`MakerMountConfig`] / [`build_maker`] / [`build_paper_maker_core_with`]'s multi-symbol tripwire
/// are all untouched; see that module's doc for why two single-symbol paper books are the right
/// answer to the tripwire's concern rather than an evasion of it.
pub mod xemm;
pub use xemm::{
    build_live_xemm_core, build_paper_xemm_core, build_paper_xemm_core_with, build_xemm_maker,
    LiveXemmMount, PaperXemmMount, XemmConfigError, XemmMountConfig, XemmMountError,
};

/// Inventory-skew knobs for the mounted maker — the mount-config mirror of
/// [`SpreadMaker::with_skew`]'s three arguments (audit F9: the builder existed but no mount could
/// reach it). `None` on [`MakerMountConfig::skew`] ⇒ the builder is never called and the mount is
/// byte-identical to before (skew off). Even a `Some` with `skew == 0.0` is neutral per
/// [`SpreadMaker::with_skew`]'s contract.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MakerSkew {
    /// Inventory the skew biases toward ([`vike_model::SpreadMakerParams::target_inventory`]).
    pub target_inventory: f64,
    /// Inventory band the skew ramps over ([`vike_model::SpreadMakerParams::max_inventory`]; must be
    /// `> 0` to engage).
    pub max_inventory: f64,
    /// Skew intensity in `[0, 1]` ([`vike_model::SpreadMakerParams::skew`]; `0.0` = off).
    pub skew: f64,
}

/// Per-side fill-rate circuit-breaker knobs — the mount-config mirror of
/// [`SpreadMaker::with_fill_breaker`]'s three arguments (audit F9 twin of [`MakerSkew`]). `None` on
/// [`MakerMountConfig::breaker`] ⇒ the builder is never called, byte-identical (breaker off). Note
/// even a `Some` engages only when ALL THREE knobs are positive — the breaker's own OFF gate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MakerBreaker {
    /// Sliding EVENT-TIME window (ms) over which per-side fills are netted
    /// ([`vike_model::SpreadMakerParams::fill_window_ms`]).
    pub fill_window_ms: i64,
    /// NET one-directional fill size within the window that trips suppression
    /// ([`vike_model::SpreadMakerParams::net_fill_threshold`]).
    pub net_fill_threshold: f64,
    /// How long (ms) a tripped side stays suppressed — its quote pulled
    /// ([`vike_model::SpreadMakerParams::suppress_cooldown_ms`]).
    pub suppress_cooldown_ms: i64,
}

/// Everything needed to stand up the paper maker mount. The A-S knobs live in `as_params` so a caller
/// controls the whole pricing layer; [`MakerMountConfig::polymarket`] fills the recommended Polymarket
/// defaults with `resolution_ts` wired from a market's `end_date`.
#[derive(Clone, Debug)]
pub struct MakerMountConfig {
    /// venue tag (`"polymarket"`); also the paper client's + engine's venue.
    pub venue: String,
    /// the outcome CLOB `token_id` — the engine/mount SYMBOL the maker trades.
    pub token_id: String,
    /// the mounted bar-series interval (must match the synthesized bars — see [`TickBarSynthesizer`]).
    pub interval: String,
    /// the synth-bar window in ms (e.g. `60_000` for `"1m"`).
    pub interval_ms: i64,
    /// base quote size per side (shares).
    pub qty: f64,
    /// fallback fixed half-spread (UNUSED while A-S is on — A-S prices — but carried for a non-A-S
    /// maker and as the `SpreadMaker::new` seed).
    pub half_spread: f64,
    /// the venue price grid (Polymarket = `0.01`) — set on the maker so the A-S L1 lane snaps/clamps
    /// onto it (`SpreadMaker::with_quote_style` is the L1 tick-grid setter).
    pub tick_size: f64,
    /// the Avellaneda–Stoikov knobs (with `resolution_ts` set for the time-to-resolution horizon).
    pub as_params: AsParams,
    /// Optional CROSS-SYMBOL underlying/reference series the maker WATCHES ("Option B" routing) — a
    /// DIFFERENT symbol than `token_id` (e.g. the `btcusdt` RTDS spot a BTC up/down maker anchors its
    /// fair mid on). Threaded onto the spawned [`StrategyMount::underlying_symbol`], so the runtime
    /// routes that symbol's marks into the maker's `on_mark`. `None` (the [`MakerMountConfig::polymarket`]
    /// default) ⇒ no underlying routing and the A-S underlying-anchored knobs stay inert (byte-identical).
    /// Set it — plus the `as_params.underlying_weight`/`window_secs` knobs and an RTDS feed for that
    /// symbol — to light up the anchored fair mid / ATM guard.
    pub underlying_symbol: Option<String>,
    /// Opt-in LIQUIDITY-REWARDS-aware quoting params (steal/rewards-mount-wiring). `None` (the
    /// default from [`MakerMountConfig::polymarket`]) ⇒ the maker is built WITHOUT
    /// [`SpreadMaker::with_liquidity_rewards`], byte-identical to a pre-rewards mount. `Some` (set by
    /// the live `config_for_market` from the market's parsed `vike_polymarket::RewardsConfig` when the
    /// operator opts in with a positive reward weight) ⇒ the maker folds the venue's own reward band
    /// (`max_spread_cents`/`min_size`/`min_order_age_ms`) into its quote. Even a `Some` with
    /// `weight == 0` is inert (rewards OFF) — see [`RewardParams`].
    pub reward: Option<RewardParams>,
    /// Opt-in FLOW-TOXICITY guard params (5c toxicity producer). `None` (the default from
    /// [`MakerMountConfig::polymarket`]) ⇒ the maker is built WITHOUT [`SpreadMaker::with_flow_toxicity`],
    /// byte-identical to a pre-toxicity mount. `Some` (set by the bin's `--toxicity` opt-in) ⇒ the maker
    /// reacts to per-side [`vike_model::FlowToxicity`] readings, widening + cutting size on the toxic
    /// side. Even a `Some` with both knobs `0.0` is inert (guard OFF) — see [`ToxicityParams`]. The
    /// producer that FEEDS those readings (a [`ToxicityEmitter`] over the RTDS activity tape) is wired
    /// separately in the bin; without it the maker's `on_flow` is never called and the guard stays inert.
    pub toxicity: Option<ToxicityParams>,
    /// Opt-in inventory-skew size shaping (audit F9 — [`SpreadMaker::with_skew`] existed but no
    /// mount could reach it). `None` (the default from both constructors) ⇒ the builder is never
    /// called and the maker keeps its neutral skew, byte-identical to a pre-skew mount.
    pub skew: Option<MakerSkew>,
    /// Opt-in per-side fill-rate circuit breaker (audit F9 — [`SpreadMaker::with_fill_breaker`]
    /// existed but no mount could reach it, which also made the #798 accelerated pull unreachable
    /// live). `None` (the default) ⇒ the builder is never called, byte-identical (breaker off).
    pub breaker: Option<MakerBreaker>,
    /// Opt-in order-refresh TOLERANCE — the anti-churn gate (audit F9 —
    /// [`SpreadMaker::with_refresh_tolerance`] existed but no mount could reach it). `None` (the
    /// default) ⇒ the builder is never called and every tick re-quotes both sides, byte-identical.
    /// An all-zero bag is likewise inert per [`RefreshTolerance`]'s contract.
    pub refresh_tolerance: Option<RefreshTolerance>,
    /// paper account seed (equity), and the paper fill-cost scalars.
    pub seed_cash: f64,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
}

/// The STRATEGY-FREE half of a mount: everything [`spawn_core`] needs to place SOME strategy on a
/// `(venue, symbol, interval)` series and price its paper fills, with no A-S knob in it.
///
/// It exists because the two mount builders below used to take a [`MakerMountConfig`] — a bag whose
/// generic fields (venue / symbol / interval / seed_cash / the three fill-cost scalars) sat mixed in
/// with `qty` / `half_spread` / `tick_size` / `as_params` / `skew` / `breaker`, which are
/// [`SpreadMaker`] parameters and nothing else. That made the mount path itself LOOK A-S-specific
/// when only ONE line of it was (`build_maker(cfg)`), and it is why `vike-tradehub` could mount
/// exactly one strategy. Splitting the bag is what lets a caller hand in ANY
/// `Box<dyn Strategy<LiveBroker> + Send>` — from `vike_strategy::strategy_by_name`, say — and get
/// the identical mount.
///
/// ⚠ The A-S PRICE-DOMAIN coupling deliberately does NOT live here. [`MakerMountConfig::polymarket`]
/// (the `[0,1]` outcome-token domain) and [`MakerMountConfig::crypto`] (the `$`-scale unbounded
/// domain) differ in their `as_params` AND in defaults that ARE generic (`seed_cash` 1_000 vs
/// 100_000, plus `tick_size`/`qty`). Those stay on the maker config, whose constructors set them
/// together; a caller building a `MountSpec` by hand states each one itself rather than silently
/// inheriting a maker's opinion about how much paper equity a `$`-scale asset needs.
#[derive(Clone, Debug)]
pub struct MountSpec {
    /// venue tag — also the paper client's + engine's venue.
    pub venue: String,
    /// the mount SYMBOL: the instrument the strategy trades.
    pub symbol: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` — every mount that existed
    /// before this field — is the venue's DEFAULT account: the same engine, the same
    /// `route_key` (the bare venue id), the same `LIVE-<route_key>.lock` filename, byte for byte.
    ///
    /// `Some(label)` routes this mount's orders AND its `Broker` reads (position / equity /
    /// multiplier / lot size) to that account's own engine, and makes [`Self::symbol`] the symbol
    /// that account is armed on (`vike_run::account_symbols_for`). It is the half that was missing:
    /// credentials, per-account ceilings, the mount fan-out and the GUI could all address a second
    /// account, and a strategy could not — so a second account was armed on whatever symbol the
    /// venue happened to wire, collided with the default account there, and lost.
    ///
    /// ⚠ **A named account that is not ARMED must never fall through to the default one.** The
    /// composition root checks it against the SAME `vike_mount::venue_account_arming` the fan-out
    /// selects with and DROPS the mount with an error naming venue, account and strategy;
    /// `vike_core`'s `assemble_core` then panics on a named account with no engine, which is the
    /// backstop no future root can reach past. Silently trading the default account when the
    /// operator named another is the one failure this whole field exists to prevent.
    pub account: Option<vike_model::account_keys::AccountLabel>,
    /// the mounted bar-series interval (must match the synthesized bars — see [`TickBarSynthesizer`]).
    pub interval: String,
    /// the synth-bar window in ms (e.g. `60_000` for `"1m"`). A FEED / paper-fill concern, not a
    /// strategy knob: it is the [`TickBarSynthesizer`] window a tick-only venue needs to produce the
    /// closed bars the paper book fills on.
    pub interval_ms: i64,
    /// Optional CROSS-SYMBOL underlying/reference series the strategy WATCHES — threaded onto
    /// [`StrategyMount::underlying_symbol`]. `None` ⇒ no routing (byte-identical).
    pub underlying_symbol: Option<String>,
    /// Optional explicit mount id ([`StrategyMount::controller_id`]). `None` ⇒ the legacy
    /// `{venue}__{symbol}__{interval}` derivation. REQUIRED when two mounts share one triple —
    /// `assemble_core` PANICS on a duplicate rather than silently sharing durable state.
    pub controller_id: Option<String>,
    /// Opt-in ADDITIONAL symbols this mount may trade and read ([`StrategyMount::symbols`]).
    ///
    /// ⚠ EMPTY on every mount this crate builds today, and [`build_paper_strategy_core_with`]'s
    /// tripwire asserts it: a single-book [`PaperExecutionClient`] stamps its own symbol onto every
    /// fill, so a multi-leg PAPER rehearsal would book both legs under one symbol and LOOK correct
    /// while the live core routed them apart. Whoever fills this must switch that builder to
    /// `vike_paper::MultiPaperExecutionClient` in the same change.
    pub legs: Vec<vike_core::MountLeg>,
    /// paper account seed (equity), and the paper fill-cost scalars.
    pub seed_cash: f64,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
}

impl MakerMountConfig {
    /// This maker config's STRATEGY-FREE projection — the generic mount fields, with the A-S knobs
    /// left behind. The two `build_*_maker_core` entry points are exactly
    /// `build_*_strategy_core(Box::new(build_maker(cfg)), &cfg.mount_spec(), …)`, which is what
    /// makes "the A-S maker is now ONE mountable strategy rather than the hardcoded one" a refactor
    /// with no behaviour in it.
    pub fn mount_spec(&self) -> MountSpec {
        MountSpec {
            venue: self.venue.clone(),
            symbol: self.token_id.clone(),
            interval: self.interval.clone(),
            interval_ms: self.interval_ms,
            underlying_symbol: self.underlying_symbol.clone(),
            // A maker mount has never named an explicit id, a second leg or an ACCOUNT — the last
            // one for the same reason as the other two: this config predates all three, and `None`
            // is the venue's default account, i.e. exactly what it has always mounted on.
            account: None,
            controller_id: None,
            legs: Vec::new(),
            seed_cash: self.seed_cash,
            slippage: self.slippage,
            maker_fee: self.maker_fee,
            taker_fee: self.taker_fee,
        }
    }

    /// The recommended Polymarket paper-maker configuration for `token_id`, with the A-S
    /// time-to-resolution horizon anchored on `resolution_ts_ms` (from the market's `end_date`;
    /// `None` ⇒ A-S falls back to its constant `tau_hold`). Every field is public, so a caller tweaks
    /// freely after.
    pub fn polymarket(token_id: impl Into<String>, resolution_ts_ms: Option<i64>) -> Self {
        MakerMountConfig {
            venue: "polymarket".to_string(),
            token_id: token_id.into(),
            interval: "1m".to_string(),
            interval_ms: 60_000,
            qty: 20.0,
            half_spread: 0.01,
            tick_size: 0.01,
            as_params: AsParams { resolution_ts: resolution_ts_ms, ..AsParams::default() },
            // Underlying routing OFF by default (byte-identical mount): no cross-symbol series is
            // watched and the A-S underlying-anchored knobs stay at their inert `AsParams::default()`
            // (`underlying_weight`/`window_secs`/`atm_blackout_scale` all `0.0`). The bin's
            // `--underlying` flag opts in and sets those knobs — all operator-tunable via `as_params`.
            underlying_symbol: None,
            // Rewards OFF by default (byte-identical mount); the live bin / `config_for_market`
            // opts in from the market's parsed rewards config.
            reward: None,
            // Flow-toxicity guard OFF by default (byte-identical mount); the bin's `--toxicity` flag
            // opts in and also starts the RTDS activity-tape producer that feeds it.
            toxicity: None,
            // Skew / breaker / refresh-tolerance OFF by default (audit F9 — the reachability fields):
            // `None` ⇒ their builders are never called, byte-identical to a pre-F9 mount.
            skew: None,
            breaker: None,
            refresh_tolerance: None,
            seed_cash: 1_000.0,
            // Polymarket CLOB maker rebate/taker fees are effectively 0 today; keep the paper cost
            // model faithful (a caller can raise these). Slippage 0 — a resting maker fill has none.
            slippage: 0.0,
            maker_fee: 0.0,
            taker_fee: 0.0,
        }
    }

    /// A `$`-scale crypto paper-maker configuration — the generalization TWIN of [`Self::polymarket`]
    /// (the crypto-domain lift). Where `::polymarket` tunes the Avellaneda–Stoikov layer for `[0,1]`
    /// outcome-token prices, this tunes it for an UNBOUNDED `$`-priced asset (a BTC/ETH perp mid ~$64k,
    /// an equity) so the SAME [`vike_mm::SpreadMaker`] quotes it. The A-S knobs it sets and WHY:
    /// - [`VarianceMode::RawLocal`] — the Bernoulli cap `p(1−p)` is meaningless (and goes NEGATIVE) for
    ///   a price `> 1`, so the raw local vol `σ̂²·H` is the only sensible `V`.
    /// - [`HorizonMode::ConstantTau`] — an open-ended market that never resolves (no `resolution_ts`,
    ///   no near-resolution blackout).
    /// - [`PriceDomain::Unbounded`] — drop the `[tick, 1−tick]` wall clamp; a quote is bounded only by
    ///   the standoff + the `bid < s < ask` straddle.
    /// - `min_half_spread_ticks = 2` — at `$`-scale the intensity half-spread `(1/γ)·ln(1+γ/κ)` is
    ///   SUB-TICK (~0.02 at a $64k mid on a $1 tick), so without a floor `r ± δ` snaps both sides onto
    ///   ONE tick and the two-sided quote collapses to `None` (the live HL/BTC no-quote bug).
    /// - `max_half_spread_ticks = 60` — the vol half-spread `½·γ·σ̂²·H` grows as the SQUARE of realized
    ///   move, so a spike would post the quote arbitrarily far off the book (the maker silently leaves
    ///   the market exactly when spreads are richest). Cap it at ~$60 half / ~19 bp of a $64k mid: wide
    ///   enough to keep the A-S widening through an ordinary move (~$9/tick), tight enough to stay
    ///   plausibly fillable in a spike. Pinned by the `crypto_width_tuning_tests` workbench in vike-mm.
    /// - `round_trip_fee_rate` — **ARMED HERE, from the venue's own fee schedule**
    ///   ([`maker_round_trip_fee_for`]), so the half-spread is floored at the BREAK-EVEN width
    ///   `½·m·s` and a mount that cannot cover its own round-trip fee under the `max_half_spread_ticks`
    ///   ceiling posts NOTHING rather than booking a known loss forever. This is the ONE knob on this
    ///   config the library default leaves OFF and the mount turns ON — see
    ///   [`vike_model::AsParams::round_trip_fee_rate`] for the refusal contract and
    ///   `vike_mm::avellaneda::bounded_half_spread` for why it refuses instead of widening.
    ///   ⚠ It is armed by DEFAULT because the mount it was written for was measured losing money on
    ///   every completed round trip: a silent halt is strictly better than a silent bleed, and the
    ///   halt is attributable (an edge-triggered `warn!` from the maker, plus the startup
    ///   `effective_params` line). A venue whose fee SHAPE has no flat fraction of price arms nothing
    ///   and says so — never a silent `0.0`.
    ///
    /// `gamma`/`q_scale` are STARTING values (tuned against the vike-mm width workbench — every field is
    /// public, so re-tune freely):
    /// - `gamma = 5e-4`: the vol half-spread is `½·γ·V` with `V = σ̂²·H` in PRICE² (RawLocal). At a $64k
    ///   mid with the default `tau_hold_ms` horizon a BTC-scale `σ̂²·H` is O(1e3–1e4) price², so a small
    ///   `γ` keeps `½·γ·V` in the few-ticks band. Retune against live realized σ̂² (or shrink
    ///   `as_params.tau_hold_ms` for a shorter, HFT-appropriate horizon).
    /// - `q_scale = 3e-2`: inventory is normalized `q_norm = position/q_scale`. The `::polymarket`
    ///   default `100` is a SHARE count; for a ~`qty`-BTC (≈0.005) position it would make `q_norm ≈ 5e-5`
    ///   and the inventory skew vanish, so a position-scaled `q_scale` keeps it live. Sizing: the per-clip
    ///   reservation skew is `q_norm·γV = (0.005/0.03)·γV ≈ 0.17·γV`, about a THIRD of the `0.5·γV`
    ///   vol-half-spread — so it takes ~3 clips (≈0.015 BTC) to shift the reservation by a full
    ///   half-spread and ~6 to pull the risk-reducing side to the mid. That warehouses a modest inventory
    ///   band while still quoting two-sided — a step LOOSER than the initial strict `1e-2` (where ONE
    ///   clip pinned the ask to the mid). Tighten back toward `1e-2` for a strict-flat stance, or loosen
    ///   toward `5e-2` to warehouse more.
    pub fn crypto(
        venue: impl Into<String>,
        token_id: impl Into<String>,
        tick_size: f64,
        qty: f64,
    ) -> Self {
        let venue = venue.into();
        let token_id = token_id.into();
        let round_trip_fee_rate = maker_round_trip_fee_for(&venue, &token_id);
        MakerMountConfig {
            venue,
            token_id,
            interval: "1m".to_string(),
            interval_ms: 60_000,
            qty,
            // Unused while A-S prices (carried as the `SpreadMaker::new` seed); a couple ticks at scale.
            half_spread: tick_size * 2.0,
            tick_size,
            as_params: AsParams {
                variance_mode: VarianceMode::RawLocal,
                horizon_mode: HorizonMode::ConstantTau,
                price_domain: PriceDomain::Unbounded,
                min_half_spread_ticks: 2.0,
                // Cap the half-spread so the `½·γ·σ̂²·H` quadratic can't post the maker off the book in
                // a vol spike: 60 ticks ⇒ a ~$120 full spread ≈ 19 bp at a $64k mid — wide enough to
                // keep the A-S vol-widening through an ordinary move (~$9/tick), tight enough to stay
                // plausibly fillable through a spike. One number, re-tunable per venue/asset.
                max_half_spread_ticks: 60.0,
                // The break-even floor, resolved from the venue's fee schedule above. `None` when the
                // fee SHAPE has no flat fraction of price — never a silent `0.0`.
                round_trip_fee_rate,
                gamma: 5e-4,
                q_scale: 3e-2,
                ..AsParams::default()
            },
            // No cross-symbol underlying anchor / PM rewards / flow-toxicity on a plain `$`-scale mount.
            underlying_symbol: None,
            reward: None,
            toxicity: None,
            // Skew / breaker / refresh-tolerance OFF by default (audit F9), same as `::polymarket`.
            skew: None,
            breaker: None,
            refresh_tolerance: None,
            // Paper equity sized for a `$`-scale asset (a single BTC clip is ~$300+ notional). Fees left
            // 0.0 so `paper_client_for` uses the venue's `fee_schedule_for`; no resting-maker slippage.
            seed_cash: 100_000.0,
            slippage: 0.0,
            maker_fee: 0.0,
            taker_fee: 0.0,
        }
    }
}

/// Resolve the SINGLE-VENUE round-trip maker fee to arm the A-S break-even floor with — the fee half
/// of [`MakerMountConfig::crypto`], factored out so the resolution and its DIAGNOSTIC live together.
///
/// Lane-keyed exactly like [`paper_client_for`]'s paper book, and for the same reason: on
/// binance/aster the SYMBOL picks the order API (`vike_catalog::fee_lane` owns the `.P` split), and
/// the perp lane is priced apart from spot. So the maker's break-even bar and the paper book's fills
/// read the SAME schedule — a mount whose floor disagreed with its own simulated fees would be worse
/// than no floor at all.
///
/// ## ⚠ It LOGS, because a fee nobody could name must not look like a fee of zero
///
/// [`vike_model::maker_round_trip_fee`] refuses by SHAPE — every `Free` FX/CFD venue (charged through
/// the spread), ibkr's per-share schedule, polymarket's `p(1−p)` curve, deribit's
/// underlying-with-premium-cap. Each returns `None`, which arms NO floor, which is
/// indistinguishable downstream from a zero fee. So the two outcomes are announced differently and
/// once, at mount: an armed floor at `info`, an unarmed one at `warn` naming the venue, the lane and
/// the shape. That asymmetry is the whole point — this is the same defect class as
/// "credentials absent and credentials unreadable must not look the same to an operator".
///
/// ⚠ The rate is the STATIC registry row. A live account rate exists
/// (`ReconClient::fetch_fee_rates`, preferred by `vike_mount::resolve_fee_schedule`) but is gated on
/// `VIKE_RECONCILE=1` and is not reachable from a pure constructor, so a mount on a VIP tier
/// cheaper than tier 0 is floored CONSERVATIVELY — the safe direction: it can refuse a mount that
/// would in fact have been marginally profitable, never admit one that is not. Threading the live
/// rate in is the follow-up.
fn maker_round_trip_fee_for(venue: &str, symbol: &str) -> Option<f64> {
    let lane = vike_catalog::fee_lane(venue, symbol);
    let schedule = vike_model::fee_schedule_for(lane);
    match vike_model::maker_round_trip_fee(schedule) {
        Some(rate) => {
            tracing::info!(
                venue,
                symbol,
                fee_lane = lane,
                round_trip_fee_bps = rate * 1e4,
                "maker break-even floor ARMED: a quote narrower than ½·fee·mid will not be posted"
            );
            Some(rate)
        }
        None => {
            tracing::warn!(
                venue,
                symbol,
                fee_lane = lane,
                ?schedule,
                "maker break-even floor NOT armed: this venue's fee shape has no flat fraction of \
                 price, so nothing here can say what a round trip costs. The maker will quote \
                 WITHOUT a break-even floor — this is an absent bar, NOT a zero fee."
            );
            None
        }
    }
}

/// The spawned mount: the live [`CoreHandle`] plus the shared paper-fill log (the client moved into
/// the core thread; this is a clone of its `fills` `Arc` so a caller/test can observe fills without
/// touching the core).
pub struct MakerMount {
    pub handle: CoreHandle,
    pub fills: Arc<Mutex<Vec<PaperFill>>>,
}

/// The equity sampler's sink signature (portfolio-observer PR-3 T6) — re-exported from
/// [`vike_core::runtime::EquitySampleHook`] (the canonical definition) so call sites below can
/// name it without a second, duplicate `type_complexity`-dodging alias.
pub use vike_core::runtime::EquitySampleHook;

/// WHICH operator HALT sentinel a paper book built by this crate watches.
///
/// ⚠ **This type exists because a MOUNT is armed BY DESIGN, and a TEST that drives a mount must not
/// inherit the operator's kill switch.** `paper_client_for` arms every book it builds from the
/// process-wide path (`vike_mount::halt::halt_path_from_env`: `VIKE_HALT_FILE`, else
/// `<project>/settings/state/HALT`, else `<exe_dir>/HALT`) — right for a daemon, and wrong for a
/// test, whose verdict would then depend on whether a file happens to exist on the box running it.
///
/// That is not hypothetical. Arming this seam with no such choice available made five tests in this
/// crate fail on any box holding a sentinel — including `tests::taker_fee_of`'s two callers, which
/// surface as a confusing `assert_eq!(fills.len(), 1)` fee panic that never mentions halt, because a
/// halted book produces no fill. MEASURED on the CI box with `VIKE_HALT_FILE` pointing at a real file:
/// 22 tests across the workspace flipped, 5 of them here. CI was green only because no runner
/// happens to have the file.
///
/// There is deliberately no `Unarmed` variant. A mount always watches SOMETHING, and a test that
/// wants no halt names a path it owns and does not create — which also lets it engage that halt
/// deliberately, as `crates/vike-paper/tests/paper_halt.rs` does.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PaperHalt {
    /// The PROCESS-WIDE operator sentinel — what every daemon mount gets, and the `Default`, so a
    /// caller that says nothing is armed rather than silently unarmed.
    #[default]
    ProcessWide,
    /// A caller-owned sentinel path. **TEST SEAM** — the same shape, and for the same reason, as
    /// `vike_bridge_core::exec_actor::ExecActor::with_halt_path` and
    /// `vike_ctrader::exec::CtraderExec::with_halt_path`: `halt_path_from_env` memoizes in a
    /// `OnceLock` and reads `VIKE_HALT_FILE`, so a test cannot steer it without mutating
    /// process-global state under threads (this workspace does not `set_var` under threads).
    Pinned(PathBuf),
}

impl PaperHalt {
    /// The sentinel path a book built under this choice watches.
    pub fn resolve(&self) -> PathBuf {
        match self {
            PaperHalt::ProcessWide => vike_mount::halt::halt_path_from_env(),
            PaperHalt::Pinned(path) => path.clone(),
        }
    }
}

/// Every OPT-IN knob the paper maker mount takes beyond the [`MakerMountConfig`] itself — the ONE
/// options bag for [`build_paper_maker_core_with`] (audit F12: this replaces the former three
/// one-knob-per-twin builders `build_paper_maker_core_with_{risk_limits,equity_sink,state}`, whose
/// knobs could not compose without a crate edit; the caller-less `_with_state` twin is gone
/// outright). [`PaperMountOpts::default`] reproduces the plain [`build_paper_maker_core`] path
/// BYTE-IDENTICALLY, so struct-update syntax composes any subset:
/// `PaperMountOpts { risk_limits, ..Default::default() }`.
pub struct PaperMountOpts {
    /// The [`RiskLimits`] the mounted `RiskGate` is built from (RunProfile wiring, Settings STEP 2
    /// PR 1 Task 2) — the seam a caller (`vike-tradehub`) uses to arm the OPERATOR-owned risk
    /// budget (`max_notional_per_order`/`max_total_exposure`/`max_orders_per_window`/…) from a
    /// loaded `vike_core::RunProfile`, via `vike_core::ProfileRisk::apply_to`. The default is
    /// `RiskLimits::new()` — the exact value the plain builder has always hardcoded — so a caller
    /// with no profile configured sees no behavior change (the merge-safety property).
    pub risk_limits: RiskLimits,
    /// Equity sampler cadence (portfolio-observer PR-3 T6) — `Some` arms
    /// `vike_core::CoreConfig::equity_sample`; pair it with [`Self::on_equity_sample`]. `None` (the
    /// default) leaves the sampler inert — no timer ever armed.
    pub equity_sample: Option<Duration>,
    /// The equity sampler's sink closure — see `vike_core::CoreConfig::on_equity_sample`'s doc for
    /// the row shape (one row per engine plus a `"TOTAL"` row). A caller wiring a `RecorderSink`
    /// (mirroring vike-app's `main.rs`) would call
    /// `sink.record_equity("portfolio", &row.venue, row.clone())` per row. `None` by default.
    pub on_equity_sample: Option<EquitySampleHook>,
    /// Strategy-state sidecar directory (portfolio-observer PR-4 T6) — `Some` makes this mount's
    /// [`SpreadMaker`] (breaker trip count + A-S accumulator — see `vike_mm`'s
    /// `save_state`/`load_state`) survive a restart. Maps onto `vike_core::CoreConfig::state_dir`;
    /// pair with [`Self::state_save`]. `None` by default.
    pub state_dir: Option<PathBuf>,
    /// State-save cadence (`vike_core::CoreConfig::state_save`). `None` by default.
    pub state_save: Option<Duration>,
    /// Readiness gate (`vike_core::CoreConfig::readiness_gate`): `true` keeps the mount `Pending`
    /// (buffered order intents discarded) until its `(venue, token_id)` actually prices — see that
    /// field's doc for the trade-off (`vike-app`'s `main.rs` documents the same for its own mount).
    /// `false` by default.
    pub readiness_gate: bool,
    /// Cancel the surviving OCO sibling when a released bracket exit dies UNFILLED
    /// (`vike_core::CoreConfig::oco_cancel_sibling_on_dead_exit`). `false` by default — the
    /// keep-protection behaviour, byte-identical to every mount this builder made before the knob
    /// existed.
    ///
    /// Here because `vike-tradehub` reads `vike_config::Flags::oco_cancel_sibling_on_dead_exit` for
    /// its LIVE mount, and a paper rehearsal that silently ignored the same flag would rehearse the
    /// wrong bracket behaviour — the one thing a rehearsal exists to get right. The A-S maker this
    /// builder mounts places no brackets of its own, so the knob is inert for the maker alone; the
    /// daemon's order-control channel (TCP, and Telegram under its feature) submits intents into
    /// the same core, which is where it stops being inert.
    pub oco_cancel_sibling_on_dead_exit: bool,
    /// Cancel every resting order during the core's shutdown teardown
    /// (`vike_core::CoreConfig::cancel_orders_on_shutdown`). `false` by default — leave the book
    /// resting, byte-identical to every mount this builder made before the knob existed.
    ///
    /// Here for the same reason as [`Self::oco_cancel_sibling_on_dead_exit`] directly above:
    /// `vike-tradehub` reads `vike_config::Flags::cancel_orders_on_shutdown` for its LIVE mount, and
    /// a paper rehearsal that ignored it would rehearse the wrong STOP behaviour. On the paper
    /// exchange the cancels land on `PaperExecutionClient`'s own resting book rather than a venue,
    /// so a rehearsal still shows the operator exactly which orders a real stop would have pulled.
    pub cancel_orders_on_shutdown: bool,
    /// Which operator HALT sentinel this mount's paper book watches — see [`PaperHalt`], whose doc
    /// carries the measurement. [`PaperHalt::ProcessWide`] by default, so a DAEMON that says nothing
    /// is armed; a TEST that drives this mount and expects fills pins a path it owns and never
    /// creates, instead of inheriting whatever is lying around on the box.
    pub halt: PaperHalt,
    /// RUNTIME strategy-mount resolver (split-plane B5) — maps onto
    /// `vike_core::CoreConfig::strategy_factory`, so a `Command::MountStrategy` reaching this
    /// mount's core can resolve a strategy at runtime. `None` (the default) refuses every runtime
    /// mount with a recent-events note, byte-identical to every mount this builder made before the
    /// knob existed. Here for the same reason as the two flag knobs above: `vike-tradehub` arms it
    /// on its LIVE mount (via `NodeConfig::core_config`), and a paper rehearsal that could not
    /// mount at runtime would rehearse the wrong control surface.
    pub strategy_factory: Option<vike_core::StrategyFactory>,
}

impl Default for PaperMountOpts {
    /// Today's plain-path values, byte-identical to what [`build_paper_maker_core`] always mounted:
    /// `RiskLimits::new()` — deliberately NOT `RiskLimits::default()`, which differs (`new()` sets
    /// `window_ms: 1000`) — and every other knob off
    /// (`None`/`None`/`None`/`None`/`false`/`false`/`false`), with the HALT sentinel at
    /// [`PaperHalt::ProcessWide`] — the ARMED default, because the alternative silently disarms an
    /// operator's kill switch on the shipped paper daemon.
    fn default() -> Self {
        PaperMountOpts {
            risk_limits: RiskLimits::new(),
            equity_sample: None,
            on_equity_sample: None,
            state_dir: None,
            state_save: None,
            readiness_gate: false,
            oco_cancel_sibling_on_dead_exit: false,
            cancel_orders_on_shutdown: false,
            halt: PaperHalt::ProcessWide,
            strategy_factory: None,
        }
    }
}

/// Build + spawn the paper maker core: `PaperExecutionClient` behind the `ExecutionClient` seam, an
/// A-S [`SpreadMaker`] mounted via [`StrategyMount`], on [`spawn_core`] — the PRODUCTION runtime. This
/// is feature-free (no Polymarket/venue code): the live bin and the offline test share it verbatim,
/// differing ONLY in what feeds the returned handle (real [`vike_polymarket::Feeds`] vs a scripted
/// feed), both through a [`MakerSink`]. Exactly
/// `build_paper_maker_core_with(cfg, PaperMountOpts::default())` — every opt-in knob at its off
/// state (equity sampler unarmed, no durable state, readiness gate off, `RiskLimits::new()` on the
/// `RiskGate`) — kept as the named plain entry point so the common path reads as before.
pub fn build_paper_maker_core(cfg: &MakerMountConfig) -> MakerMount {
    build_paper_maker_core_with(cfg, PaperMountOpts::default())
}

/// Build the mount's paper exchange, choosing its fee model (fee model follow-up 3): default to the
/// per-venue [`vike_model::fee_schedule_for`] registry schedule (Polymarket ⇒ `Free`), but let an
/// EXPLICIT profile fee override win — a non-zero `maker_fee`/`taker_fee` in the config keeps the
/// flat-rate [`PaperExecutionClient::new`] path (the user-facing fee knobs; `MakerMountConfig::
/// polymarket` seeds them `0.0` precisely so the registry default applies, and its doc already
/// invites a caller to "raise these"). Zero/zero ⇒ the registry schedule via `with_fee_schedule`.
///
/// The paper book this mount trades on — and, because this is a MOUNT and not a simulation, one
/// with the operator HALT kill switch ARMED.
///
/// ⚠ **This is the seam the shipped paper daemon actually uses.** `vike-tradehub`'s paper variant
/// reaches its exchange through [`build_paper_maker_core_with`] and never touches
/// `vike_mount::make_engine`, so arming the eleven fallback arms over there reached this daemon not
/// at all: `touch $VIKE_HALT_FILE` on the box running the paper node did nothing, silently, which is
/// precisely the mount an operator rehearses the switch on. Both seams now resolve the SAME path
/// (`vike_mount::halt::halt_path_from_env` — vike-mount re-exports it so this crate need not learn
/// about the transport stack), so a node has one sentinel however it was mounted.
///
/// The BACKTEST path deliberately gets none: `PaperExecutionClient` defaults to no sentinel and
/// `vike-backtest`'s r7 gate constructs it directly, so the backtest == paper equivalence law cannot
/// be perturbed by a file on disk. See `crates/vike-paper/src/lib.rs`'s `with_halt_path`.
///
/// ⚠ **WHICH sentinel is a PARAMETER, because arming this seam broke two tests in a way CI CANNOT
/// SEE.** `tests::taker_fee_of` submits a plain (non-`reduce_only`) opening order through this
/// builder and reads the resulting fill's commission. Armed from the process-wide path, a HALT
/// sentinel on the box refused that submit, no fill was produced, and both callers panicked on
/// `assert_eq!(fills.len(), 1, "the market order fills at next open")` — a fee assertion that never
/// mentions halt. The trigger file is EXACTLY the one an operator touches on a live node
/// (`VIKE_HALT_FILE=/srv/vike-<unit>/settings/state/HALT` on the shipped unit). MEASURED on the CI box:
/// `VIKE_HALT_FILE=<an existing file> cargo nextest run` over the CI roster turned 22 tests red, 5
/// of them in this crate, while the same command without it ran 7147/7147 green.
///
/// That is the defect class `crates/vike-paper/tests/paper_halt_process_wide.rs` was written to
/// eliminate ("a mutation proof that holds only on the author's machine is not a proof") and that
/// `crates/vike-ops/tests/paper_mount_arming_gate.rs`'s rule 3 states for the simulation side. It
/// applies to TESTS OF A MOUNT just as much: a test whose verdict depends on a file lying around in
/// `<project>/settings/state/` is not a test of anything. Every DAEMON caller passes
/// [`PaperHalt::ProcessWide`] — `PaperMountOpts`' `Default` — so nothing about the shipped node
/// changed.
///
/// ⚠ It takes a [`MountSpec`], not a [`MakerMountConfig`]: this seam serves EVERY strategy the
/// daemon can mount, not only the A-S maker, and the arming must not depend on which one was named.
/// [`build_paper_strategy_core_with`] is the single caller, so a registry mount
/// (`[strategy] name = "grid"`) and the default maker mount reach the same armed book.
fn paper_client_for(spec: &MountSpec, halt: &PaperHalt) -> PaperExecutionClient {
    let book = if spec.maker_fee != 0.0 || spec.taker_fee != 0.0 {
        PaperExecutionClient::new(
            &spec.venue,
            &spec.symbol,
            spec.slippage,
            spec.maker_fee,
            spec.taker_fee,
        )
    } else {
        PaperExecutionClient::with_fee_schedule(
            &spec.venue,
            &spec.symbol,
            spec.slippage,
            // LANE-keyed (see `vike_catalog::fee_lane`): a `.P` symbol on binance/aster names the
            // PERP lane, which is priced apart from spot and must not fill at the spot schedule.
            // Identity for polymarket (this mount's shipped venue) and for every non-`.P` symbol.
            vike_model::fee_schedule_for(vike_catalog::fee_lane(&spec.venue, &spec.symbol)),
        )
    };
    book.with_halt_path(halt.resolve())
}

/// Build the A-S [`SpreadMaker`] this mount runs from `cfg`. Factored out of
/// [`build_paper_maker_core_with`] so the reward-param wiring is unit-testable by reading the returned
/// maker's `reward` field (the maker itself is moved into the core thread by `spawn_core`, so it
/// cannot be observed after spawn). `with_quote_style` sets ONLY the L1 tick grid the A-S lane
/// snaps/clamps on (`QuoteStyle::Mid`/`depth_levels` are ignored while A-S prices).
///
/// The liquidity-rewards fold is applied ONLY when `cfg.reward` is `Some` — so a default mount
/// (`reward: None`, every path except an opted-in live `config_for_market`) never calls
/// [`SpreadMaker::with_liquidity_rewards`] and is byte-identical to before this wiring. A `Some`
/// carrying `weight == 0` is itself inert (rewards OFF) per [`RewardParams`], so the opt-in gate is
/// the reward `weight`, not merely the presence of the params.
///
/// The FLOW-TOXICITY guard is threaded the same way: applied ONLY when `cfg.toxicity` is `Some` (the
/// bin's `--toxicity` opt-in), so a default mount (`toxicity: None`) never calls
/// [`SpreadMaker::with_flow_toxicity`] and stays byte-identical. A `Some` with both knobs `0.0` is
/// itself inert per [`ToxicityParams`], and the guard only ever moves a quote once a producer actually
/// feeds the maker's `on_flow` (the [`ToxicityEmitter`] the bin starts alongside).
///
/// The SKEW / BREAKER / REFRESH-TOLERANCE builders (audit F9 — all three existed on the maker but no
/// mount could reach them) follow the identical opt-in shape: each is applied ONLY when its
/// `Option` config field is `Some`, so a default mount (`None`/`None`/`None`) never calls
/// [`SpreadMaker::with_skew`] / [`SpreadMaker::with_fill_breaker`] /
/// [`SpreadMaker::with_refresh_tolerance`] and stays byte-identical.
///
/// PUBLIC since the mount path became strategy-generic: a caller that wants the A-S maker as a
/// `Box<dyn Strategy<LiveBroker> + Send>` — `vike-tradehub`'s default mount, which must stay
/// byte-identical to the pre-generalization daemon — builds it with THIS function rather than
/// re-deriving the knob folding, so there is exactly one definition of "the maker this config
/// means".
pub fn build_maker(cfg: &MakerMountConfig) -> SpreadMaker {
    let mut maker = SpreadMaker::new(cfg.qty, cfg.half_spread)
        .with_quote_style(QuoteStyle::Mid, 1, cfg.tick_size)
        .with_avellaneda_stoikov(cfg.as_params);
    if let Some(reward) = cfg.reward {
        maker = maker.with_liquidity_rewards(reward);
    }
    if let Some(toxicity) = cfg.toxicity {
        maker = maker.with_flow_toxicity(toxicity);
    }
    if let Some(s) = cfg.skew {
        maker = maker.with_skew(s.target_inventory, s.max_inventory, s.skew);
    }
    if let Some(b) = cfg.breaker {
        maker =
            maker.with_fill_breaker(b.fill_window_ms, b.net_fill_threshold, b.suppress_cooldown_ms);
    }
    if let Some(t) = cfg.refresh_tolerance {
        maker = maker.with_refresh_tolerance(t.price_bps, t.size_bps);
    }
    maker
}

/// The spawned LIVE maker mount: the twelve-venue [`Node`] with the A-S [`SpreadMaker`] folded into
/// its [`CoreConfig::strategy`]. Unlike [`MakerMount`] there is **no `fills` field** — over
/// [`build_node`] the `ExecutionClient` is a real venue (credential-gated), not the paper exchange, so
/// fills surface only through the [`CoreHandle`]'s `CoreSnapshot` / the journal, never a paper-fill log.
pub struct LiveMakerMount {
    /// The live node — destructure for `handle` / `forwarder_stop` / `live_venues`, exactly like a
    /// direct [`build_node`] caller (`let vike_run::Node { handle, forwarder_stop, .. } = mount.node;`).
    pub node: Node,
}

/// The live twin of [`build_paper_maker_core`]: mount the A-S [`SpreadMaker`] on the REAL twelve-venue
/// [`build_node`] core (per-venue exec credential-gated) instead of the paper exchange. Unlike the
/// paper builder it spawns **no feed** — the caller wires the venue's live `Feeds` onto the returned
/// `node.handle`'s ingest lanes (via [`vike_core::CoreLaneSink`]), exactly as `polymarket_maker_paper`
/// does over the paper core.
///
/// The caller owns `node_cfg`, so its `core_config` carries the LIVE safety knobs a live daemon must be
/// capped with (`submit_ack_timeout` / `max_drawdown` / `margin_call`, …). This fn only folds the
/// maker's [`StrategyMount`] into `node_cfg.core_config.strategy` **before** [`build_node`] consumes it,
/// so the mount lands on `(cfg.venue, cfg.token_id, cfg.interval)` — `spawn_core_multi` then wires the
/// matching extra engine's applied-fill capture on that venue automatically.
///
/// Feature-free and **network-free by itself**: with an empty credentials map every venue mounts paper
/// (proven by `vike-tradehub/tests/daemon/live_gate_paper.rs`), so no network call happens here — the network
/// is the caller's live feed. `build_node` returning [`NodeError`] (the live-event-forwarder thread
/// spawn, its one fault) rides this fn's `Result`.
pub fn build_live_maker_core(
    cfg: &MakerMountConfig,
    node_cfg: NodeConfig,
) -> Result<LiveMakerMount, NodeError> {
    build_live_strategy_core(Box::new(build_maker(cfg)), &cfg.mount_spec(), node_cfg)
}

/// [`build_live_maker_core`] with the strategy as a PARAMETER — the general form, and the one a
/// headless daemon calls to mount whatever its profile named.
///
/// This is the whole of what used to make `vike-tradehub` a one-strategy daemon: the live mount
/// path was already strategy-agnostic (the [`NodeConfig`] safety knobs, the twelve `make_engine`
/// arms, the reconcile mount, the feed wiring — none of them read a maker field), and the ONLY A-S
/// line was `build_maker(cfg)` sitting INSIDE the builder instead of at the call site. Lifting it
/// out is a signature change, not new machinery.
///
/// `strategy` is a `Box<dyn Strategy<LiveBroker> + Send>` — exactly what
/// `vike_strategy::strategy_by_name::<vike_core::LiveBroker>` returns, so "run the strategy I
/// backtested" is `resolve → hand it here`. The `+ Send` is the live core's own requirement
/// ([`StrategyMount::strategy`]): the strategy is moved onto the core thread.
///
/// The caller owns `node_cfg`, so its `core_config` carries the LIVE safety knobs a live daemon must
/// be capped with (`submit_ack_timeout` / `max_drawdown` / `margin_call`, …). This fn only folds the
/// [`StrategyMount`] into `node_cfg.core_config.strategy` **before** [`build_node`] consumes it, so
/// the mount lands on `(spec.venue, spec.symbol, spec.interval)` — `spawn_core_multi` then wires the
/// matching engine's applied-fill capture on that venue automatically.
///
/// ⚠ It does NOT validate that the venue's engine will ACCEPT `spec.symbol`.
/// `vike_mount::make_engine` sets no `extra_symbols`, so a venue engine accepts exactly the symbol
/// it was mounted on ([`vike_exec::ExecutionEngine::accepts_symbol`]) and a foreign symbol's orders
/// and fills are SILENTLY dropped — no error anywhere. [`WIRED_MARKETS`] is the table that says what
/// each venue was mounted on, and the caller is where that check belongs (it is the only party that
/// knows whether it built the node with the default markets).
pub fn build_live_strategy_core(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
    node_cfg: NodeConfig,
) -> Result<LiveMakerMount, NodeError> {
    let mut node_cfg = node_cfg;
    fold_strategy_mount(&mut node_cfg, strategy, spec);
    Ok(LiveMakerMount { node: build_node(node_cfg)? })
}

/// [`build_live_strategy_core`] over an ALREADY-RUN preflight report — the injected-report twin of
/// [`build_node_with_preflight`], for the same reason that one exists.
///
/// ⚠ It became necessary when the preflight's disposition started being ENFORCED. `build_node`
/// runs the real preflight over `node_cfg.vars`, so a test that plants credentials to express LIVE
/// INTENT now has that intent taken away again the moment the venue refuses the probe — and a test
/// about the risk-budget refusal would then silently stop testing it (measured:
/// `a_live_rhai_mount_without_a_risk_budget_is_refused_pre_connect` went from asserting a refusal
/// to asserting nothing). Passing an EMPTY [`PreflightReport`] both restores the intent and makes
/// such a test NETWORK-FREE, which it was not before: it was issuing a real signed bybit read with
/// a fake key and the result merely happened not to matter.
///
/// ⚠ Production callers must use [`build_live_strategy_core`]. An injected empty report is "no
/// preflight ran", which is only ever right for a caller that is not mounting a real venue.
pub fn build_live_strategy_core_with_preflight(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
    node_cfg: NodeConfig,
    preflight: &vike_mount::preflight::PreflightReport,
) -> Result<LiveMakerMount, NodeError> {
    let mut node_cfg = node_cfg;
    fold_strategy_mount(&mut node_cfg, strategy, spec);
    Ok(LiveMakerMount { node: build_node_with_preflight(node_cfg, preflight)? })
}

/// The one body the two builders above share: fold the [`StrategyMount`] into the config `build_node`
/// is about to consume, so the mount lands on `(spec.venue, spec.symbol, spec.interval)`.
fn fold_strategy_mount(
    node_cfg: &mut NodeConfig,
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
) {
    node_cfg.core_config.strategy = Some(StrategyMount {
        // WHICH ACCOUNT — carried through verbatim. `None` (every mount that existed before the
        // field) is the venue's default account, byte-identical.
        account: spec.account.clone(),
        symbols: spec.legs.clone(),
        controller_id: spec.controller_id.clone(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        interval: spec.interval.clone(),
        strategy,
        // "Option B" cross-symbol routing carried through verbatim (`None` by default ⇒ inert).
        underlying_symbol: spec.underlying_symbol.clone(),
    });
}

/// [`build_paper_maker_core`]'s options variant (audit F12): the SAME mount with any subset of the
/// opt-in knobs armed via [`PaperMountOpts`] — struct-update over `Default` composes them (e.g.
/// `PaperMountOpts { risk_limits, ..Default::default() }` is `vike-tradehub`'s operator-budget
/// seam, and equity-sink + durable-state now compose in ONE call, which the former
/// one-knob-per-twin builders could not without a crate edit). Builds the paper client, the A-S
/// maker and the ONE [`CoreConfig`] literal and spawns it; `PaperMountOpts::default()` reproduces
/// the plain [`build_paper_maker_core`] BYTE-IDENTICALLY (see the field docs for each knob's off
/// state).
pub fn build_paper_maker_core_with(cfg: &MakerMountConfig, opts: PaperMountOpts) -> MakerMount {
    build_paper_strategy_core_with(Box::new(build_maker(cfg)), &cfg.mount_spec(), opts)
}

/// [`build_paper_maker_core_with`] with the strategy as a PARAMETER — the paper twin of
/// [`build_live_strategy_core`], and the rehearsal path for whatever a daemon profile named.
///
/// Same shape as the live builder: the only A-S-specific thing about the old code was that it
/// called `build_maker(cfg)` itself. Everything here — the paper client, the `Account`, the
/// `RiskGate`, the ONE [`CoreConfig`] literal, `spawn_core` — is generic plumbing that never read a
/// maker field.
pub fn build_paper_strategy_core_with(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
    opts: PaperMountOpts,
) -> MakerMount {
    let PaperMountOpts {
        risk_limits,
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        halt,
        strategy_factory,
    } = opts;
    // TRIPWIRE. `paper_client_for` builds a SINGLE-symbol `PaperExecutionClient` whose
    // `emit_fill` stamps the BOOK's symbol onto every fill, whatever the request named. So a
    // multi-symbol mount rehearsed on paper would book BOTH legs under one symbol and look
    // perfectly correct while the live core routed them apart — the paper run would actively
    // conceal the very thing it is meant to rehearse.
    //
    // No config path can produce a declared-multi mount here today ([`MountSpec::legs`] is empty on
    // every spec this crate builds), so this cannot fire yet. It exists to fire the moment that
    // changes: whoever adds the operator surface for a two-leg mount must ALSO switch this builder
    // to `vike_paper::MultiPaperExecutionClient` (one book per declared symbol; it already routes
    // `submit` by `request.symbol` and already synthesizes a terminal `OrderRejected` for an unknown
    // one) rather than discover the omission from a paper run that lied.
    //
    // ⚠ It reads `spec.legs` — a CONFIGURABLE field now, where it used to read a function that
    // returned `Vec::new()` unconditionally. That is the point: the tripwire became reachable the
    // moment the mount surface stopped hardcoding "no legs", which is exactly when it starts
    // earning its keep.
    assert!(
        spec.legs.is_empty(),
        "paper rehearsal of a multi-symbol mount needs MultiPaperExecutionClient: a \
         single-symbol paper book stamps its own symbol onto every fill and would hide the \
         misrouting this rehearsal exists to catch"
    );
    let client = paper_client_for(spec, &halt);
    let fills = Arc::clone(&client.fills);
    let engine = ExecutionEngine::new(
        Account::new(1.0, &spec.venue, None, BalanceMode::Delta),
        RiskGate::new(risk_limits),
        client,
        &spec.venue,
        &spec.symbol,
    );
    let config = CoreConfig {
        seed_cash: spec.seed_cash,
        strategy: Some(StrategyMount {
            account: None,
            symbols: spec.legs.clone(),
            controller_id: spec.controller_id.clone(),
            venue: spec.venue.clone(),
            symbol: spec.symbol.clone(),
            interval: spec.interval.clone(),
            strategy,
            // "Option B" cross-symbol routing: the runtime feeds this underlying's marks into the
            // strategy's `on_mark`. `None` (the default) ⇒ no routing, byte-identical.
            underlying_symbol: spec.underlying_symbol.clone(),
        }),
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        // Write-ahead journal (live-tearsheet sink-enablement) — OFF unless `VIKE_JOURNAL_DIR` /
        // `VIKE_RUN_PROFILE` is set, so the offline mount test stays byte-identical. When enabled,
        // the maker's paper fills (synthesized via `pump_client`, journaled since #339) land on disk.
        journal: vike_core::journal_config_from_env(),
        // Off by default (see `PaperMountOpts`) — so `build_paper_maker_core` and every existing
        // `..Default::default()` caller stay byte-identical.
        oco_cancel_sibling_on_dead_exit,
        // Ditto: off ⇒ teardown detaches and exits exactly as it always did, book left resting.
        cancel_orders_on_shutdown,
        // `None` (the default) ⇒ runtime `Command::MountStrategy` refuses — see the opts field.
        strategy_factory,
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    MakerMount { handle, fills }
}

/// One strategy on one series — the unit the MULTI-mount builders take N of (split-plane I10, the
/// Pattern-A daemon: strategies sharing a venue account share a PROCESS).
///
/// Exactly the pair every single-mount builder already takes as two parameters
/// (`strategy` + [`MountSpec`]), named as a struct because a `Vec<(Box<dyn …>, MountSpec)>` at a
/// call site says nothing about which tuple slot is which.
///
/// ⚠ `spec.controller_id` is the caller's responsibility on a multi-mount: `vike_core`'s
/// `assemble_core` PANICS on two mounts deriving one mount id (a shared id would silently share a
/// durable-state sidecar and a journal attribution key), so a caller building N of these must
/// assign each a distinct id — the daemon derives
/// `{venue}__{symbol}__{interval}__{strategy-identity}` and refuses duplicates at profile LOAD
/// (`vike_tradehub::config::DaemonProfile::validate`), which is where that refusal belongs: naming
/// the two offending profile rows beats a runtime panic naming neither.
pub struct StrategyMountSpec {
    /// The resolved strategy — what `vike_strategy::strategy_by_name::<vike_core::LiveBroker>`
    /// (or [`build_maker`], boxed) returns.
    pub strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    /// The strategy-free mount half: venue / symbol / interval / seed + the paper fee scalars.
    pub spec: MountSpec,
}

impl StrategyMountSpec {
    /// Lower into the [`StrategyMount`] the core takes — the same field-for-field mapping
    /// [`build_live_strategy_core`] and [`build_paper_strategy_core_with`] both spell inline.
    fn into_mount(self) -> StrategyMount {
        StrategyMount {
            account: self.spec.account.clone(),
            symbols: self.spec.legs.clone(),
            controller_id: self.spec.controller_id.clone(),
            venue: self.spec.venue.clone(),
            symbol: self.spec.symbol.clone(),
            interval: self.spec.interval.clone(),
            strategy: self.strategy,
            underlying_symbol: self.spec.underlying_symbol.clone(),
        }
    }
}

/// One paper BOOK's fill log, keyed by the `(venue, symbol)` the book stamps onto its fills —
/// [`MultiStrategyMount::fills`]' element (a named type per clippy's `type_complexity`).
pub type BookFills = ((String, String), Arc<Mutex<Vec<PaperFill>>>);

/// The spawned multi-strategy PAPER mount: the one live [`CoreHandle`] plus each constructed paper
/// book's fill log, keyed by the `(venue, symbol)` the book stamps onto its fills — the
/// attribution a multi-mount test asserts on ("mount B's fill landed in mount B's book").
pub struct MultiStrategyMount {
    pub handle: CoreHandle,
    /// One entry per paper BOOK (one per distinct `(venue, symbol)` across the mounts — two
    /// strategies on one series share one book, exactly as they share one venue account).
    pub fills: Vec<BookFills>,
}

/// N strategies / N venues / N symbols on ONE paper core — the multi-mount twin of
/// [`build_paper_strategy_core_with`] (split-plane I10), and the rehearsal path for a
/// `[[mounts]]` daemon profile.
///
/// The engine layout answers the single-mount builder's own tripwire BY CONSTRUCTION rather than
/// by evading it (the same argument `xemm`'s module doc makes for its two books):
///
/// * **one [`ExecutionEngine`] per DISTINCT venue** — `spawn_core_multi` routes venue-tagged
///   events and `OrderIntent::Submit` by venue, and each engine keeps its own `Account`/
///   [`RiskGate`] (the cross-venue firewall);
/// * **one single-symbol [`PaperExecutionClient`] per distinct `(venue, symbol)`** — a venue whose
///   mounts span several symbols gets a [`vike_paper::MultiPaperExecutionClient`] (one book per
///   symbol, routed by `request.symbol`, a miss synthesizing the terminal `OrderRejected`) and the
///   beyond-primary symbols land in [`ExecutionEngine::extra_symbols`] so their venue events fold.
///   This is exactly the `MultiPaperExecutionClient` switch [`MountSpec::legs`]' doc demands of
///   "whoever fills this": a single-symbol book stamps its OWN symbol onto every fill and would
///   book two mounts' legs under one symbol, concealing the misrouting the rehearsal exists to
///   catch.
///
/// Per-mount `spec.legs` stays subject to the single-mount tripwire (asserted below): the
/// multi-SYMBOL support here is across MOUNTS, each mount still a single-series mount.
///
/// Each venue engine's seed is the SUM of that venue's mounts' `seed_cash` — the operator states
/// per-row capital and the venue account holds the total; `CoreConfig::seed_cash` is the primary
/// venue's sum, so the drawdown denominator (`Σ seed + own PnL`) counts every row exactly once.
///
/// PANICS on an empty `mounts` (a mount builder with nothing to mount is a programmer error, not a
/// runtime state) and — via `assemble_core` — on two mounts sharing a derived mount id; see
/// [`StrategyMountSpec`] for why the daemon refuses that at LOAD instead of ever reaching the
/// panic.
pub fn build_paper_multi_strategy_core_with(
    mounts: Vec<StrategyMountSpec>,
    opts: PaperMountOpts,
) -> MultiStrategyMount {
    assert!(!mounts.is_empty(), "a multi-strategy paper mount needs at least one mount");
    let PaperMountOpts {
        risk_limits,
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        halt,
        // B5 graft through the I10 rebase: the multi rehearsal answers runtime
        // `Command::MountStrategy` exactly as the single-mount builder does.
        strategy_factory,
    } = opts;
    // The SAME tripwire as the single-mount builder, per mount: multi-symbol here is across
    // mounts (each a single-series mount over its own book); a declared per-mount LEG still needs
    // the routed multi-book treatment at the MOUNT level, which nothing here builds.
    for m in &mounts {
        assert!(
            m.spec.legs.is_empty(),
            "paper rehearsal of a multi-symbol mount needs MultiPaperExecutionClient at the mount \
             level: a single-symbol paper book stamps its own symbol onto every fill and would \
             hide the misrouting this rehearsal exists to catch"
        );
    }
    // Distinct venues, in first-appearance order (the engine order is observable — snapshot rows —
    // so it must be the declaration order, the multi_mount reordering-stability property).
    let mut venues: Vec<String> = Vec::new();
    for m in &mounts {
        if !venues.contains(&m.spec.venue) {
            venues.push(m.spec.venue.clone());
        }
    }
    let mut fills: Vec<BookFills> = Vec::new();
    let mut engines: Vec<(f64, ExecutionEngine<Box<dyn ExecutionClient + Send>>)> = Vec::new();
    for venue in &venues {
        let venue_specs: Vec<&MountSpec> =
            mounts.iter().filter(|m| &m.spec.venue == venue).map(|m| &m.spec).collect();
        // Distinct symbols on this venue, in mount order; the FIRST mount naming a symbol supplies
        // that book's fee/slippage scalars (two mounts sharing a series share its one book).
        let mut symbol_specs: Vec<&MountSpec> = Vec::new();
        for s in venue_specs.iter().copied() {
            if !symbol_specs.iter().any(|p| p.symbol == s.symbol) {
                symbol_specs.push(s);
            }
        }
        let seed: f64 = venue_specs.iter().map(|s| s.seed_cash).sum();
        let books: Vec<PaperExecutionClient> =
            symbol_specs.iter().map(|s| paper_client_for(s, &halt)).collect();
        for b in &books {
            fills.push(((venue.clone(), b.symbol.clone()), Arc::clone(&b.fills)));
        }
        let primary_symbol = symbol_specs[0].symbol.clone();
        let extra_symbols: Vec<String> =
            symbol_specs[1..].iter().map(|s| s.symbol.clone()).collect();
        let client: Box<dyn ExecutionClient + Send> = if books.len() == 1 {
            Box::new(books.into_iter().next().expect("exactly one book"))
        } else {
            let mut multi = vike_paper::MultiPaperExecutionClient::new();
            for b in books {
                multi.add_book(b);
            }
            Box::new(multi)
        };
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(risk_limits.clone()),
            client,
            venue,
            &primary_symbol,
        );
        engine.extra_symbols = extra_symbols;
        engines.push((seed, engine));
    }
    let (primary_seed, primary_engine) = engines.remove(0);
    let mut iter = mounts.into_iter();
    let first = iter.next().expect("asserted non-empty above");
    let config = CoreConfig {
        seed_cash: primary_seed,
        strategy: Some(first.into_mount()),
        extra_mounts: iter.map(StrategyMountSpec::into_mount).collect(),
        equity_sample,
        on_equity_sample,
        state_dir,
        state_save,
        readiness_gate,
        // The same opt-in journal / flag wiring as the single-mount builder — a `[[mounts]]`
        // rehearsal must not journal differently from a single-mount one.
        journal: vike_core::journal_config_from_env(),
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        // `None` (the default) ⇒ runtime `Command::MountStrategy` refuses — see the opts field.
        strategy_factory,
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(primary_engine, engines, config);
    MultiStrategyMount { handle, fills }
}

/// N strategies on the REAL twelve-venue [`build_node`] core — the multi-mount twin of
/// [`build_live_strategy_core`] (split-plane I10), and what a `[[mounts]]` daemon profile mounts
/// LIVE.
///
/// The first mount becomes [`CoreConfig::strategy`] and the rest [`CoreConfig::extra_mounts`] —
/// `build_node` already mounts one engine per venue ([`WIRED_MARKETS`]) and `spawn_core_multi`
/// routes each mount's orders to its own venue's engine, so N mounts need no new engine machinery:
/// the CORE half of I10 shipped with `extra_mounts` and this fn only populates it from a config
/// path for the first time.
///
/// Like [`build_live_strategy_core`] it validates NOTHING about venue/symbol acceptance — the
/// caller (the daemon's `validate_for_live`, per profile row) owns that, for the same reason that
/// fn's doc gives. PANICS on an empty `mounts`.
pub fn build_live_multi_strategy_core(
    mounts: Vec<StrategyMountSpec>,
    mut node_cfg: NodeConfig,
) -> Result<LiveMakerMount, NodeError> {
    assert!(!mounts.is_empty(), "a multi-strategy live mount needs at least one mount");
    let mut iter = mounts.into_iter();
    let first = iter.next().expect("asserted non-empty above");
    node_cfg.core_config.strategy = Some(first.into_mount());
    node_cfg.core_config.extra_mounts = iter.map(StrategyMountSpec::into_mount).collect();
    Ok(LiveMakerMount { node: build_node(node_cfg)? })
}

/// Event-time OHLC bar synthesizer: folds a `(ts, mid)` stream into fixed-`interval_ms` windows and
/// emits a closed [`Bar`] when a tick crosses into a new window. This is the paper-fill driver — the
/// maker quotes on ticks; the paper client needs CLOSED bars. Event-time (never wall-clock) so it is
/// deterministic under a scripted feed AND correct live (real tick ts). O(1) per tick.
///
/// The bucket fold itself is [`vike_model::BarConsolidator`] (the shared streaming consolidator,
/// whose monotonic close rule is exactly what this lane needs — an out-of-order tick must never
/// emit a backwards-stamped bar onto the bar lane). This type adds the two things specific to the
/// paper-fill lane: each tick enters as a degenerate one-price sample ([`vike_model::one_price_bar`],
/// so `volume` stays 0.0 — Polymarket ticks carry no bar volume), and the emitted bar is stamped
/// with the window's CLOSE time rather than its start.
pub struct TickBarSynthesizer {
    inner: vike_model::BarConsolidator,
}

impl TickBarSynthesizer {
    pub fn new(interval_ms: i64) -> Self {
        // Guard a nonsensical interval to this lane's own default BEFORE handing it to the
        // consolidator (whose own floor is a bare div-by-zero guard, not a 1m fallback).
        let interval_ms = if interval_ms > 0 { interval_ms } else { 60_000 };
        TickBarSynthesizer { inner: vike_model::BarConsolidator::new(interval_ms) }
    }

    /// Fold one `(ts, mid)`. Returns `Some(bar)` iff this tick STARTS a new window — i.e. it CLOSES
    /// the previous one; the returned bar is that just-completed window. The caller must emit it
    /// BEFORE forwarding this tick to the strategy, so the paper fill uses the quotes as they rested
    /// through the completed window (the maker re-quotes only on the forwarded tick). O(1).
    pub fn on_price(&mut self, ts: i64, mid: f64) -> Option<Bar> {
        let closed = self.inner.fold(&vike_model::one_price_bar(ts, mid))?;
        Some(self.close_stamped(closed))
    }

    /// Force-close the current partial window (teardown / manual flush). `None` if nothing is open.
    pub fn flush(&mut self) -> Option<Bar> {
        let closed = self.inner.flush()?;
        Some(self.close_stamped(closed))
    }

    /// The consolidator stamps a window with its START; the paper book fills against the window's
    /// CLOSE time, so shift by one interval (`bucket_start + interval == (bucket + 1) * interval`).
    fn close_stamped(&self, mut b: Bar) -> Bar {
        b.ts += self.inner.interval_ms();
        b
    }
}

/// The feed→core adapter: a [`LiveDataSink`] that forwards the venue's quote/trade/book ticks onto the
/// core's lossless tick lane (`handle.tick_sender()`) AND drives the [`TickBarSynthesizer`], emitting
/// synthesized closed bars onto `handle.bar_sender()` to fill the paper book. The SAME sink is fed by
/// the real [`vike_polymarket::Feeds`] (live bin) and a scripted feed (offline test).
///
/// The synth is driven off the QUOTE lane's `(ts, mid)` — Polymarket's `Feeds` emits a derived L1
/// quote on every top-of-book change, so the quote stream captures all mid movement. `book`/`trade`
/// are forwarded (so the maker's `on_order_book`/`on_trade_tick` run) but do not themselves close
/// bars (an `L2Book` carries no event ts of its own).
pub struct MakerSink {
    /// shared feed→core-lane forwarder — quote/trade/book onto the tick lane AND stream-health onto
    /// a mounted strategy's `on_feed_status` (the hand-rolled sink never forwarded the latter).
    core: vike_core::CoreLaneSink,
    /// kept separately for the SYNTHESIZED bars (`send_bar`) — Polymarket has no candles, so the
    /// bar verbs stay no-ops and the only bars on this lane are the synth's. (The `mark_tick` verb is
    /// NOT a no-op: it forwards the "Option B" underlying spot through `core` onto the mark lane.)
    bars: BarSender,
    venue: String,
    symbol: String,
    interval: String,
    synth: Mutex<TickBarSynthesizer>,
}

impl MakerSink {
    /// Build the sink over a spawned mount's handle. `venue`/`symbol`/`interval` must match the
    /// [`MakerMountConfig`] (the synth bars are stamped with them so the paper book routes/fills).
    pub fn new(
        handle: &CoreHandle,
        venue: impl Into<String>,
        symbol: impl Into<String>,
        interval: impl Into<String>,
        interval_ms: i64,
    ) -> Self {
        MakerSink {
            core: vike_core::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            ),
            bars: handle.bar_sender(),
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            synth: Mutex::new(TickBarSynthesizer::new(interval_ms)),
        }
    }

    /// Force-close the current partial synth window (call at teardown so the last bar's fills land).
    pub fn flush_bar(&self) {
        let closed = self.synth.lock().expect("synth mutex").flush();
        if let Some(bar) = closed {
            self.send_bar(bar);
        }
    }

    /// Advance the synth off a `(ts, mid)` and, if a window just closed, emit its bar BEFORE the
    /// caller forwards the tick (the causal order the paper fill depends on).
    fn feed_price(&self, ts: i64, mid: f64) {
        let closed = self.synth.lock().expect("synth mutex").on_price(ts, mid);
        if let Some(bar) = closed {
            self.send_bar(bar);
        }
    }

    fn send_bar(&self, bar: Bar) {
        let _ = self.bars.close(BarUpdate {
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            interval: self.interval.clone(),
            bar,
        });
    }
}

impl LiveDataSink for MakerSink {
    // Bar lanes are unused: Polymarket has no candles, and the mount synthesizes its own bars.
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}

    /// Underlying-mark lane ("Option B" cross-symbol routing): forward the UNDERLYING spot mark (a
    /// DIFFERENT symbol than the token this mount trades — e.g. `btcusdt`) onto the core's mark lane,
    /// where `drain_market` routes it to any maker declaring it as its `underlying_symbol`. Previously
    /// a no-op (Polymarket has no candle marks of its own); the RTDS crypto-prices feed now drives the
    /// underlying through here. Inert unless a mount declares an underlying — the core dispatch
    /// early-returns — so a plain mount is byte-identical.
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.core.mark_tick(venue, symbol, px, ts);
    }

    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        // close any completed synth window FIRST (fills against the quotes that rested through it),
        // THEN forward this quote (the maker re-quotes on it).
        self.feed_price(quote.ts, quote.mid());
        self.core.quote(venue, symbol, quote);
    }

    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.core.trade(venue, symbol, trade);
    }

    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.core.book(venue, symbol, book);
    }

    fn stream_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: vike_data::StreamStatus,
    ) {
        // Forward stream-health to the mounted maker's `on_feed_status` — the previous hand-rolled
        // sink used the trait default (no-op), so the maker never learned a feed went stale/down.
        self.core.stream_status(venue, symbol, stream, status);
    }
}

/// The 5c toxicity PRODUCER sink: an [`vike_polymarket::ActivityTradeSink`] that folds the
/// wallet-classified RTDS activity tape into a [`vike_polymarket::ToxicityAggregator`] and pushes each
/// resulting per-side [`vike_model::FlowToxicity`] reading onto the core's flow lane
/// ([`vike_exec::TickSender::flow`]), where the runtime routes it to the maker's `on_flow` hook. This
/// is the emit half — the aggregator itself is the pure core in `vike-polymarket`; this owns the I/O
/// edge (the `TickSender`, the classification map, the `Mutex` the pump's thread serializes through).
///
/// Behind the `polymarket` feature (it names `vike_polymarket` types). Started by the bin ONLY under
/// `--toxicity`, fed by an [`vike_polymarket::RtdsActivityFeed`]; without that, no such sink exists and
/// the mount is byte-identical.
///
/// ## Inert until BOTH a wallet map AND non-zero maker knobs are supplied
/// An EMPTY [`vike_polymarket::WalletClassMap`] classifies every wallet as
/// [`vike_polymarket::WalletClass::Unknown`], which is never toxic, so [`ToxicityAggregator::current`]
/// stays `{ bid: 0, ask: 0 }` and the maker never widens. The producer is therefore INERT until a real
/// wallet→class map is loaded (`--wallet-classes`) AND the maker's [`ToxicityParams`] knobs are
/// non-zero (`--tox-widen`/`--tox-size-cut`): either missing ⇒ no behavior change.
#[cfg(feature = "polymarket")]
pub struct ToxicityEmitter {
    /// The venue the emitted [`vike_exec::FlowUpdate`] is keyed on (`"polymarket"`) — must match the
    /// mount so the runtime routes the reading to the right maker.
    venue: String,
    /// The outcome CLOB `token_id` this emitter is scoped to. The activity tape is PLATFORM-WIDE, so a
    /// trade whose `asset` (token id) is not this one is ignored; and the emitted `FlowUpdate.symbol`
    /// is this token, so it routes to the `(venue, token_id)` mount.
    token_id: String,
    /// The pure per-side aggregator behind a `Mutex` — the [`vike_polymarket::RtdsActivityFeed`] pump
    /// calls [`vike_polymarket::ActivityTradeSink::on_activity_trade`] from its OWN thread, so the fold
    /// is serialized here.
    agg: Mutex<vike_polymarket::ToxicityAggregator>,
    /// The wallet → [`vike_polymarket::WalletClass`] table (empty ⇒ every wallet `Unknown` ⇒ inert).
    classes: vike_polymarket::WalletClassMap,
    /// The core's flow lane — best-effort `flow(FlowUpdate)` per trade.
    tick: vike_exec::TickSender,
}

#[cfg(feature = "polymarket")]
impl ToxicityEmitter {
    /// Build an emitter scoped to `(venue, token_id)`. `window_ms` is the aggregator's exponential
    /// decay time-constant (from `--tox-window-secs`); `classes` is the wallet→class table
    /// (`--wallet-classes`, empty ⇒ inert); `tick` is the core handle's `tick_sender()`. The toxic
    /// class set is the aggregator default (`Sharp` + `Whale`).
    pub fn new(
        venue: impl Into<String>,
        token_id: impl Into<String>,
        window_ms: i64,
        classes: vike_polymarket::WalletClassMap,
        tick: vike_exec::TickSender,
    ) -> Self {
        ToxicityEmitter {
            venue: venue.into(),
            token_id: token_id.into(),
            agg: Mutex::new(vike_polymarket::ToxicityAggregator::new(window_ms)),
            classes,
            tick,
        }
    }
}

#[cfg(feature = "polymarket")]
impl vike_polymarket::ActivityTradeSink for ToxicityEmitter {
    /// Fold one classified trade and emit the fresh per-side reading. Trades on OTHER tokens (the tape
    /// is platform-wide) are dropped; the taker's wallet is classified, the trade folded into the
    /// aggregator, and the resulting [`vike_model::FlowToxicity`] pushed onto the flow lane. Best-effort
    /// like the other sinks — a [`vike_exec::CoreGone`] error just means the core is shutting down, so
    /// it is swallowed.
    fn on_activity_trade(&self, trade: &vike_polymarket::ActivityTrade) {
        // The activity tape is platform-wide; only this mount's token feeds this maker's toxicity.
        if trade.asset != self.token_id {
            return;
        }
        let class = vike_polymarket::classify(&trade.proxy_wallet, &self.classes);
        let flow = {
            let mut agg = self.agg.lock().expect("toxicity agg mutex");
            agg.observe(trade.side, class, trade.size, trade.ts);
            agg.current(trade.ts)
        };
        let _ = self.tick.flow(vike_exec::FlowUpdate {
            venue: self.venue.clone(),
            symbol: self.token_id.clone(),
            flow,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The account an operator NAMED survives every lowering between a [`MountSpec`] and the
    /// core** — the three field-for-field copies that stand between a profile row and
    /// `vike_core::StrategyMount::account`.
    ///
    /// [`fold_strategy_mount`] (both single-mount builders), [`StrategyMountSpec::into_mount`] (the
    /// multi-mount one) and [`mount_accounts`] (what [`build_node`]'s per-account fan-out and
    /// `refuse_unarmed_mount_accounts` BOTH read the operator's choice back off) each copy the
    /// field and each can drop it in one character. The result of dropping it is not a compile
    /// error and not a wrong answer anywhere visible: the mount comes up on the venue's DEFAULT
    /// account while the profile says `account = "ALT"`, trades the wrong book, and refuses
    /// nothing on the way — because with the field gone there is no named account left to refuse.
    ///
    /// ⚠ It needed writing because every account-aware test in this workspace enters the graph
    /// DOWNSTREAM of all three copies: `vike-core`'s build their `StrategyMount` by hand and
    /// `vike-tradehub`'s stop at `MountSpec`. Setting any of the three `account:` lines to `None`
    /// was measured GREEN across `vike-run`, `vike-tradehub` AND `vike-core` before this test
    /// existed. Do that again and this goes red, naming which copy dropped it.
    #[test]
    fn the_account_survives_every_lowering_from_a_mount_spec_into_the_core() {
        let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
        let cfg = MakerMountConfig::polymarket("tok", None);
        let spec = MountSpec { account: Some(alt.clone()), ..cfg.mount_spec() };

        // (1) the MULTI-mount lowering.
        let lowered =
            StrategyMountSpec { strategy: Box::new(build_maker(&cfg)), spec: spec.clone() }
                .into_mount();
        assert_eq!(
            lowered.account.as_ref(),
            Some(&alt),
            "`StrategyMountSpec::into_mount` must carry the operator's account into the core — \
             dropping it here mounts every multi-mount row on its venue's DEFAULT account"
        );

        // (2) the SINGLE-mount lowering, through the config `build_node` is about to consume.
        let mut node_cfg = NodeConfig {
            vars: std::collections::HashMap::new(),
            properties_rec: None,
            seed_cash: 10_000.0,
            recon_enabled: false,
            core_config: vike_core::CoreConfig::default(),
            risk_profile: None,
            policy: MountPolicy::default(),
        };
        fold_strategy_mount(&mut node_cfg, Box::new(build_maker(&cfg)), &spec);
        assert_eq!(
            node_cfg.core_config.strategy.as_ref().expect("the mount was folded").account.as_ref(),
            Some(&alt),
            "`fold_strategy_mount` must carry the operator's account into the core config — \
             dropping it here mounts the single-mount builders on the venue's DEFAULT account"
        );

        // (3) …and the read-back the fan-out and the arming refusal both select on. This is the
        // copy whose loss is worst: the mount would still be resolved correctly by `vike_core`,
        // and `refuse_unarmed_mount_accounts` would see no named account to check, so an UNARMED
        // account would stop being refused at the same time.
        node_cfg.core_config.extra_mounts.push(lowered);
        let rows = mount_accounts(&node_cfg.core_config);
        assert_eq!(rows.len(), 2, "one row per mount — the folded one and the extra one");
        for r in &rows {
            assert_eq!(
                r.account.as_ref(),
                Some(&alt),
                "`mount_accounts` must report the account each mount named: it is what \
                 `account_symbols_for` arms that account's engine on and what \
                 `refuse_unarmed_mount_accounts` checks against the arming table"
            );
        }
    }

    /// The paper book this crate mounts arms the operator HALT sentinel.
    ///
    /// ⚠ This is the seam `vike-tradehub`'s shipped PAPER daemon runs, and it does not go through
    /// `vike_mount::make_engine` at all — so arming the eleven fallback arms over there reached this
    /// node not at all. `touch $VIKE_HALT_FILE` on the box did nothing, silently, on precisely the
    /// mount an operator uses to rehearse the switch. Drop the `.with_halt_path(..)` from
    /// `paper_client_for` and this goes red.
    ///
    /// Both fee paths are covered: an explicit non-zero fee (the `new` branch) and the registry
    /// schedule (the `with_fee_schedule` branch), because the arming sits after the branch and a
    /// refactor that moved it inside one arm would otherwise half-disarm the mount.
    ///
    /// It also pins the PATH, not merely its presence: the default must be the PROCESS-WIDE
    /// sentinel. Once [`PaperHalt`] made the choice a parameter, "armed" stopped being enough —
    /// a mount armed at a path no operator knows about is a mount with no kill switch, and
    /// flipping `PaperHalt`'s `#[default]` would otherwise pass an `is_some()` check.
    #[test]
    fn the_paper_maker_mount_is_halt_armed_on_both_fee_paths() {
        let process_wide = vike_mount::halt::halt_path_from_env();

        let registry = MakerMountConfig::polymarket("tok", None);
        assert_eq!(
            paper_client_for(&registry.mount_spec(), &PaperHalt::ProcessWide).halt_path(),
            Some(process_wide.as_path()),
            "the registry-fee paper mount must arm the PROCESS-WIDE HALT sentinel"
        );

        let explicit =
            MakerMountConfig { maker_fee: 0.001, ..MakerMountConfig::polymarket("tok", None) };
        assert_eq!(
            paper_client_for(&explicit.mount_spec(), &PaperHalt::ProcessWide).halt_path(),
            Some(process_wide.as_path()),
            "the explicit-fee paper mount must arm it too — the arming must not sit inside one fee \
             branch"
        );

        assert_eq!(
            PaperMountOpts::default().halt,
            PaperHalt::ProcessWide,
            "the options bag's DEFAULT must be the armed one: `vike-tradehub`'s shipped paper \
             daemon builds it with `..Default::default()`, so a flip here silently disarms the \
             operator's kill switch on a live node"
        );
    }

    /// A PINNED sentinel is the one the book watches, and it is the seam every test in this crate
    /// that expects a fill must use.
    ///
    /// ⚠ Without this parameter, five tests in this crate flipped red on any box holding a HALT
    /// file — see [`PaperHalt`]'s doc for the the CI box measurement. Point `PaperHalt::resolve`'s
    /// `Pinned` arm back at `halt_path_from_env()` and this goes red on EVERY box, sentinel or not,
    /// which is the property the earlier proof lacked.
    #[test]
    fn a_pinned_sentinel_is_the_one_the_book_watches() {
        let pinned = PathBuf::from("a-path-this-test-owns-and-never-creates/HALT");
        let cfg = MakerMountConfig::polymarket("tok", None);
        assert_eq!(
            paper_client_for(&cfg.mount_spec(), &PaperHalt::Pinned(pinned.clone())).halt_path(),
            Some(pinned.as_path()),
            "a pinned mount must watch the caller's path and nothing else"
        );
    }

    #[test]
    fn synth_closes_a_bar_on_the_window_boundary_with_correct_ohlc() {
        let mut s = TickBarSynthesizer::new(60_000);
        // window 0: 0.50 -> 0.40 -> 0.50 (a dip-and-recover); no close yet.
        assert!(s.on_price(1_000, 0.50).is_none());
        assert!(s.on_price(2_000, 0.40).is_none());
        assert!(s.on_price(3_000, 0.50).is_none());
        // a tick in window 1 closes window 0.
        let bar = s.on_price(61_000, 0.50).expect("window 0 closes");
        assert_eq!(bar.ts, 60_000); // (0 + 1) * interval
        assert_eq!(bar.open.to_bits(), 0.50f64.to_bits());
        assert_eq!(bar.high.to_bits(), 0.50f64.to_bits());
        assert_eq!(bar.low.to_bits(), 0.40f64.to_bits()); // the dip
        assert_eq!(bar.close.to_bits(), 0.50f64.to_bits());
        // window 1 is open now; flush closes it.
        let last = s.flush().expect("window 1 flushes");
        assert_eq!(last.ts, 120_000);
        assert!(s.flush().is_none()); // idempotent once drained
    }

    // --- liquidity-rewards mount wiring (steal/rewards-mount-wiring) ---------------------------

    // OFF/default: the recommended Polymarket mount config carries NO reward params, and the maker
    // it builds has reward-aware quoting OFF — byte-identical to a pre-rewards mount. This is the
    // explicit default-does-not-enable-rewards proof; the opt-in lives in the live bin /
    // `config_for_market`, never in the default mount.
    #[test]
    fn default_polymarket_mount_leaves_rewards_off() {
        let cfg = MakerMountConfig::polymarket("TOK", None);
        assert!(cfg.reward.is_none(), "default mount config must not carry reward params");
        let maker = build_maker(&cfg);
        assert!(
            maker.params().reward.is_none(),
            "default mount must not enable reward-aware quoting"
        );
    }

    // When the config DOES carry reward params (as `config_for_market` sets from a rewarded market
    // under the operator opt-in), the mount folds them onto the maker verbatim — the actual wiring
    // this feature adds.
    #[test]
    fn mount_applies_configured_reward_params_to_the_maker() {
        let reward = RewardParams {
            weight: 0.5,
            max_spread_cents: 3.0,
            min_size: 100.0,
            min_order_age_ms: 30_000,
        };
        let mut cfg = MakerMountConfig::polymarket("TOK", None);
        cfg.reward = Some(reward);
        let maker = build_maker(&cfg);
        assert_eq!(
            maker.params().reward,
            Some(reward),
            "the mount must apply cfg.reward to the maker"
        );
    }

    // --- 5c flow-toxicity mount wiring -----------------------------------------------------------

    // OFF/default: the recommended Polymarket mount config carries NO toxicity params, and the maker
    // it builds has the flow-toxicity guard OFF — byte-identical to a pre-toxicity mount. The opt-in
    // lives in the bin's `--toxicity` flag, never in the default mount.
    #[test]
    fn default_polymarket_mount_leaves_toxicity_off() {
        let cfg = MakerMountConfig::polymarket("TOK", None);
        assert!(cfg.toxicity.is_none(), "default mount config must not carry toxicity params");
        let maker = build_maker(&cfg);
        assert!(
            maker.params().toxicity.is_none(),
            "default mount must not enable the flow-toxicity guard"
        );
    }

    // When the config DOES carry toxicity params (as the bin's `--toxicity` opt-in sets), the mount
    // folds them onto the maker verbatim — the actual wiring this feature adds.
    #[test]
    fn mount_applies_configured_toxicity_params_to_the_maker() {
        let tox = ToxicityParams { widen: 1.0, size_cut: 0.5 };
        let mut cfg = MakerMountConfig::polymarket("TOK", None);
        cfg.toxicity = Some(tox);
        let maker = build_maker(&cfg);
        assert_eq!(
            maker.params().toxicity,
            Some(tox),
            "the mount must apply cfg.toxicity to the maker"
        );
    }

    // --- F9 mount reachability: skew / breaker / refresh tolerance -------------------------------

    // OFF/default: NEITHER constructor carries the F9 opt-ins, and the maker each builds keeps the
    // neutral skew, a disengaged breaker, and NO refresh-tolerance bag — byte-identical to a
    // pre-F9 mount. This is the explicit default-does-not-enable proof, mirroring the reward /
    // toxicity twins above.
    #[test]
    fn default_mounts_leave_skew_breaker_and_refresh_off() {
        for cfg in [
            MakerMountConfig::polymarket("TOK", None),
            MakerMountConfig::crypto("hyperliquid", "BTC", 1.0, 0.005),
        ] {
            assert!(
                cfg.skew.is_none() && cfg.breaker.is_none() && cfg.refresh_tolerance.is_none(),
                "default mount config must not carry the F9 opt-ins ({})",
                cfg.venue
            );
            let p = build_maker(&cfg).params();
            assert_eq!(p.target_inventory, 0.0, "neutral skew target ({})", cfg.venue);
            assert_eq!(p.max_inventory, 1.0, "neutral skew band ({})", cfg.venue);
            assert_eq!(p.skew, 0.0, "skew intensity off ({})", cfg.venue);
            assert_eq!(p.fill_window_ms, 0, "breaker window off ({})", cfg.venue);
            assert_eq!(p.net_fill_threshold, 0.0, "breaker threshold off ({})", cfg.venue);
            assert_eq!(p.suppress_cooldown_ms, 0, "breaker cooldown off ({})", cfg.venue);
            assert!(p.refresh_tolerance.is_none(), "no tolerance bag at all ({})", cfg.venue);
        }
    }

    // When the config DOES carry the F9 opt-ins, the mount threads each through its builder onto
    // the maker verbatim — the actual reachability wiring this feature adds.
    #[test]
    fn mount_applies_configured_skew_breaker_and_refresh_to_the_maker() {
        let mut cfg = MakerMountConfig::polymarket("TOK", None);
        cfg.skew = Some(MakerSkew { target_inventory: 5.0, max_inventory: 40.0, skew: 0.6 });
        cfg.breaker = Some(MakerBreaker {
            fill_window_ms: 5_000,
            net_fill_threshold: 60.0,
            suppress_cooldown_ms: 10_000,
        });
        cfg.refresh_tolerance = Some(RefreshTolerance { price_bps: 25.0, size_bps: 50.0 });
        let p = build_maker(&cfg).params();
        assert_eq!(p.target_inventory, 5.0);
        assert_eq!(p.max_inventory, 40.0);
        assert_eq!(p.skew, 0.6);
        assert_eq!(p.fill_window_ms, 5_000);
        assert_eq!(p.net_fill_threshold, 60.0);
        assert_eq!(p.suppress_cooldown_ms, 10_000);
        assert_eq!(p.refresh_tolerance, Some(RefreshTolerance { price_bps: 25.0, size_bps: 50.0 }));
    }

    // --- crypto-domain mount (the A-S price-domain generalization) ------------------------------

    // The `::crypto` twin tunes the A-S layer for an UNBOUNDED $-scale asset: RawLocal variance (the
    // Bernoulli cap is meaningless above 1.0), ConstantTau horizon (never resolves), the Unbounded
    // price domain (no [tick,1−tick] wall clamp), and a 2-tick min-half-spread floor (the sub-tick
    // $-scale spread guard). venue/token/tick/qty thread straight through, and the maker builds.
    #[test]
    fn crypto_mount_sets_the_unbounded_dollar_scale_as_params() {
        let cfg = MakerMountConfig::crypto("hyperliquid", "BTC", 1.0, 0.005);
        assert_eq!(cfg.venue, "hyperliquid");
        assert_eq!(cfg.token_id, "BTC");
        assert_eq!(cfg.tick_size.to_bits(), 1.0_f64.to_bits());
        assert_eq!(cfg.qty.to_bits(), 0.005_f64.to_bits());
        let a = cfg.as_params;
        assert_eq!(a.variance_mode, VarianceMode::RawLocal, "raw local vol (no Bernoulli cap)");
        assert_eq!(a.horizon_mode, HorizonMode::ConstantTau, "open-ended horizon");
        assert_eq!(a.price_domain, PriceDomain::Unbounded, "no wall clamp");
        assert_eq!(a.min_half_spread_ticks.to_bits(), 2.0_f64.to_bits(), "2-tick floor");
        assert_eq!(a.max_half_spread_ticks.to_bits(), 60.0_f64.to_bits(), "60-tick spike ceiling");
        // PM rewards / toxicity / underlying anchor stay OFF on a plain $-scale mount.
        assert!(cfg.reward.is_none() && cfg.toxicity.is_none() && cfg.underlying_symbol.is_none());
        let _maker = build_maker(&cfg); // A-S enabled → builds without panicking
    }

    // --- the break-even fee floor: what the MOUNT arms, and what it refuses to invent -------------

    /// The crypto mount ARMS the A-S break-even floor from the venue's own schedule, LANE-keyed —
    /// so the maker's break-even bar and its paper book's fills read the same numbers. Pinned on
    /// the three venues that make the lane split matter.
    #[test]
    fn the_crypto_mount_arms_the_break_even_floor_from_the_venue_fee_schedule() {
        // bybit VIP0: 2.0 bps maker × 2 legs = 4 bps round trip — the the CI box mount's real number.
        let bybit = MakerMountConfig::crypto("bybit", "BTCUSDT", 0.1, 0.001);
        assert_eq!(
            bybit.as_params.round_trip_fee_rate.map(f64::to_bits),
            Some((2.0_f64 / 1e4 + 2.0_f64 / 1e4).to_bits()),
            "bybit arms 4 bps"
        );
        // The LANE split is honoured: a `.P` binance symbol is the PERP lane (2 bps maker), a bare
        // one is SPOT (10 bps maker) — five times the floor. Reading the bare id for both would
        // under-floor every perp maker on the venue.
        let perp = MakerMountConfig::crypto("binance", "BTCUSDT.P", 0.1, 0.001);
        let spot = MakerMountConfig::crypto("binance", "BTCUSDT", 0.01, 0.001);
        assert_eq!(perp.as_params.round_trip_fee_rate.map(f64::to_bits), Some(4e-4_f64.to_bits()));
        assert_eq!(spot.as_params.round_trip_fee_rate.map(f64::to_bits), Some(2e-3_f64.to_bits()));
        assert_eq!(
            perp.as_params.round_trip_fee_rate,
            vike_model::maker_round_trip_fee(vike_model::fee_schedule_for(vike_catalog::fee_lane(
                "binance",
                "BTCUSDT.P"
            ))),
            "the mount reads exactly what the lane-keyed registry says — no second derivation"
        );
    }

    /// ⚠ A venue whose fee SHAPE has no flat fraction of price arms NOTHING — never a silent `0.0`.
    /// hyperliquid (the shipped `$`-scale mount) has a percent schedule and IS floored; an FX venue
    /// that charges through the spread is not, and `maker_round_trip_fee_for` says so at `warn`.
    #[test]
    fn a_venue_with_no_flat_fee_rate_arms_no_floor_rather_than_a_zero_one() {
        let hl = MakerMountConfig::crypto("hyperliquid", "BTC", 1.0, 0.005);
        assert_eq!(
            hl.as_params.round_trip_fee_rate.map(f64::to_bits),
            Some(3e-4_f64.to_bits()),
            "hyperliquid VIP0 is 1.5 bps maker ⇒ a 3 bps round trip"
        );
        for spread_charging in ["oanda", "ig", "dukascopy", "ctrader"] {
            let c = MakerMountConfig::crypto(spread_charging, "EURUSD", 0.00001, 1_000.0);
            assert_eq!(
                c.as_params.round_trip_fee_rate, None,
                "{spread_charging} charges through the SPREAD: an absent bar, not a zero fee"
            );
        }
        // ...and the polymarket mount arms nothing either — its real fee is the price-dependent
        // p(1−p) curve, which this scalar cannot express (see `maker_round_trip_fee`'s refusal set).
        assert_eq!(MakerMountConfig::polymarket("TOK", None).as_params.round_trip_fee_rate, None);
    }

    // --- fee model follow-up 3: the paper client's fee source (registry vs. config override) ---
    use vike_exec::ExecutionClient;
    use vike_model::OrderRequest;

    fn fee_cfg(venue: &str, maker_fee: f64, taker_fee: f64) -> MakerMountConfig {
        let mut c = MakerMountConfig::polymarket("TOK", None);
        c.venue = venue.to_string();
        c.slippage = 0.0;
        c.maker_fee = maker_fee;
        c.taker_fee = taker_fee;
        c
    }

    /// Drive one market (taker) fill through the client `paper_client_for` builds and read its fee.
    ///
    /// ⚠ **The book is PINNED to a sentinel this test owns and never creates, and that is the whole
    /// point.** This helper submits a plain (non-`reduce_only`) OPENING order, which a HALT-armed
    /// book refuses — producing no fill, so `assert_eq!(fills.len(), 1, ..)` below panics with a fee
    /// message that never mentions halt. When this passed `PaperHalt::ProcessWide` (the
    /// PROCESS-WIDE mount seam), both callers failed on any box holding
    /// `$VIKE_HALT_FILE`/`<project>/settings/state/HALT` — the exact file an operator touches on a
    /// live node — and CI was green only because no runner happened to have one.
    ///
    /// The `halt_path` assertion below is what makes the guard ENVIRONMENT-INDEPENDENT: re-point
    /// this at `PaperHalt::ProcessWide` and it fires on every box, sentinel or not, instead of only
    /// where the file happens to exist.
    fn taker_fee_of(cfg: &MakerMountConfig, qty: f64, px: f64) -> f64 {
        let pinned = PathBuf::from("vike-run-fee-tests-own-this-sentinel-and-never-create-it/HALT");
        let mut c = paper_client_for(&cfg.mount_spec(), &PaperHalt::Pinned(pinned.clone()));
        assert_eq!(
            c.halt_path(),
            Some(pinned.as_path()),
            "this fee helper must drive a book whose sentinel IT owns. Through the process-wide \
             mount seam, an operator's HALT file refuses the opening submit below and the fee \
             assertion fails as a fee mismatch on any box that has one"
        );
        assert!(
            !pinned.exists(),
            "the pinned sentinel must not exist, or the submit below is refused for the reason \
             this assertion exists to rule out"
        );
        c.submit(&OrderRequest {
            client_order_id: "m".into(),
            venue: cfg.venue.clone(),
            symbol: cfg.token_id.clone(),
            side: 1,
            qty,
            order_type: "market".into(),
            ..Default::default()
        });
        c.on_bar(&Bar {
            ts: 1,
            open: px,
            high: px,
            low: px,
            close: px,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        let fills = c.fills.lock().unwrap();
        assert_eq!(fills.len(), 1, "the market order fills at next open");
        fills[0].fee
    }

    #[test]
    fn paper_fees_default_to_the_venue_registry_schedule() {
        // Polymarket registry = Free ⇒ zero fee (the migrated default for the real mount).
        assert_eq!(taker_fee_of(&fee_cfg("polymarket", 0.0, 0.0), 20.0, 0.5), 0.0);
        // A venue with a non-Free registry schedule proves the registry (not a hardcoded 0) is used:
        // binance taker = 10 bps.
        assert_eq!(
            taker_fee_of(&fee_cfg("binance", 0.0, 0.0), 2.0, 100.0),
            2.0 * 100.0 * (10.0 / 10_000.0)
        );
    }

    #[test]
    fn explicit_profile_fee_override_still_wins() {
        // A non-zero config fee keeps the flat-rate path even on a venue with a registry schedule —
        // the user-facing fee knobs are preserved.
        assert_eq!(
            taker_fee_of(&fee_cfg("binance", 0.0002, 0.0007), 2.0, 100.0),
            2.0 * 100.0 * 0.0007
        );
    }
}
