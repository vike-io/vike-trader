//! The strategy-mount layer (the `vike-run` crate until docs/decisions/0098, which merged it into
//! `vike-mount`; this module was that crate's `lib.rs`): mount a strategy on the PRODUCTION live
//! core and feed it. Its public items are re-exported at the crate root and named from there.
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
//! [`MakerMountConfig`] (with the A-S `resolution_ts` set from a market's end date, when it has
//! one) → [`build_paper_maker_core`] (`spawn_core` + `StrategyMount(SpreadMaker)` +
//! `PaperExecutionClient`) → [`MakerSink`] bridges a [`vike_data::LiveDataSink`] feed onto the
//! core's tick lane (`quote`/`trade`/`book` → `handle.tick_sender()`).
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
//! ## Where the LIVE feed comes from
//! Not from this crate. `vike-tradehub` wires each venue's real feed into the SAME [`MakerSink`] the
//! offline tests drive with a scripted one (`crates/vike-tradehub/src/feeds.rs`), so the mount is
//! identical either way and only the feed differs. This crate names no venue bridge of its own: the
//! venue registry is `vike-tradehub`'s, handed to `build_node` as `NodeConfig::registry`
//! (docs/decisions/0096, amended 2026-09-29), and so are the wired markets, handed in as
//! `NodeConfig::markets` (docs/decisions/0098). vike-run's `ibkr`, `fxcm` and `polymarket` marker
//! features, which gated its node assembly's wiring of those venues, went when that table moved,
//! and `vike-tradehub`'s own features gate the three rows, as they gate the three bridge edges and
//! registry rows.
//!
//! ⚠ vike-run DID name bridges until 2026-09-28. The `polymarket_maker_paper` and `ibkr_mount`
//! bins, the Gamma market picker they used (a `live` module) and the RTDS flow-toxicity producer
//! only the first one started (`ToxicityEmitter`) were DELETED, measured unused on the latency box, the CI box,
//! the dev box and the Dublin host: no process, no shell history, no cron or unit, no log file.
//! Their two optional bridge edges went with them. The maker's `with_flow_toxicity` guard and the bridge's
//! `ToxicityAggregator` both remain — what is gone is the producer that connected the two, so a
//! [`MakerMountConfig::toxicity`] stays inert until something feeds `on_flow` again.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_core::{CoreConfig, CoreHandle, StrategyMount, spawn_core, spawn_core_multi};
use vike_data::LiveDataSink;
use vike_exec::{
    Account, BalanceMode, BarSender, BarUpdate, ExecutionClient, ExecutionEngine, RiskGate,
    RiskLimits,
};
// `SpreadMaker` and `PaperFill` are re-exported at the crate root (`crate::SpreadMaker`,
// `crate::PaperFill`), where their reasons are written; this module names them by their own crates.
use vike_mm::{QuoteStyle, SpreadMaker};
use vike_model::{
    AsParams, Bar, HorizonMode, L2Book, PriceDomain, QuoteTick, RefreshTolerance, RewardParams,
    ToxicityParams, TradeTick, VarianceMode,
};
use vike_paper::{PaperExecutionClient, PaperFill};

use crate::node::{Node, NodeConfig, NodeError, build_node, build_node_with_preflight};

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
/// controls the whole pricing layer; [`MakerMountConfig::outcome_token`] fills the recommended
/// `[0,1]` outcome-token (Polymarket) defaults with `resolution_ts` wired from a market's `end_date`.
#[derive(Clone, Debug)]
pub struct MakerMountConfig {
    /// venue tag — the caller's, through both constructors; also the paper client's + engine's
    /// venue.
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
    /// routes that symbol's marks into the maker's `on_mark`. `None` (the [`MakerMountConfig::outcome_token`]
    /// default) ⇒ no underlying routing and the A-S underlying-anchored knobs stay inert (byte-identical).
    /// Set it — plus the `as_params.underlying_weight`/`window_secs` knobs and an RTDS feed for that
    /// symbol — to light up the anchored fair mid / ATM guard.
    pub underlying_symbol: Option<String>,
    /// Opt-in LIQUIDITY-REWARDS-aware quoting params (steal/rewards-mount-wiring). `None` (the
    /// default from [`MakerMountConfig::outcome_token`]) ⇒ the maker is built WITHOUT
    /// [`SpreadMaker::with_liquidity_rewards`], byte-identical to a pre-rewards mount. `Some` ⇒ the
    /// maker folds the venue's own reward band (`max_spread_cents`/`min_size`/`min_order_age_ms`)
    /// into its quote. Even a `Some` with `weight == 0` is inert (rewards OFF) — see
    /// [`RewardParams`]. ⚠ No caller in the tree sets it since the deleted `polymarket_maker_paper`
    /// bin's market picker was the only one (this crate's module doc).
    pub reward: Option<RewardParams>,
    /// Opt-in FLOW-TOXICITY guard params (5c toxicity producer). `None` (the default from
    /// [`MakerMountConfig::outcome_token`]) ⇒ the maker is built WITHOUT [`SpreadMaker::with_flow_toxicity`],
    /// byte-identical to a pre-toxicity mount. `Some` ⇒ the maker reacts to per-side
    /// [`vike_model::FlowToxicity`] readings, widening + cutting size on the toxic side. Even a `Some`
    /// with both knobs `0.0` is inert (guard OFF) — see [`ToxicityParams`]. ⚠ A `Some` is ALSO inert
    /// today whatever its knobs say: the only producer that fed `on_flow` was deleted with the
    /// `polymarket_maker_paper` bin (this crate's module doc), so nothing reaches the guard.
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
/// ⚠ The A-S PRICE-DOMAIN coupling deliberately does NOT live here. [`MakerMountConfig::outcome_token`]
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
    /// that account is armed on (`crate::account_symbols_for`). It is the half that was missing:
    /// credentials, per-account ceilings, the mount fan-out and the GUI could all address a second
    /// account, and a strategy could not — so a second account was armed on whatever symbol the
    /// venue happened to wire, collided with the default account there, and lost.
    ///
    /// ⚠ **A named account that is not ARMED must never fall through to the default one.** The
    /// composition root checks it against the SAME `crate::venue_account_arming` the fan-out
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

    /// The recommended OUTCOME-TOKEN paper-maker configuration for `token_id` on `venue` — the A-S
    /// layer tuned for `[0,1]` outcome-token prices — with the A-S time-to-resolution horizon
    /// anchored on `resolution_ts_ms` (from the market's `end_date`; `None` ⇒ A-S falls back to its
    /// constant `tau_hold`). Every field is public, so a caller tweaks freely after.
    ///
    /// It was `polymarket(token_id, resolution_ts_ms)`, with the venue spelled inside, until
    /// docs/decisions/0098: the tuning is a property of the PRICE DOMAIN, not of the venue, and this
    /// crate names no venue, so the venue is the caller's (the shape [`Self::crypto`] already has).
    /// Polymarket is today's only caller.
    pub fn outcome_token(
        venue: impl Into<String>,
        token_id: impl Into<String>,
        resolution_ts_ms: Option<i64>,
    ) -> Self {
        MakerMountConfig {
            venue: venue.into(),
            token_id: token_id.into(),
            interval: "1m".to_string(),
            interval_ms: 60_000,
            qty: 20.0,
            half_spread: 0.01,
            tick_size: 0.01,
            as_params: AsParams { resolution_ts: resolution_ts_ms, ..AsParams::default() },
            // Underlying routing OFF by default (byte-identical mount): no cross-symbol series is
            // watched and the A-S underlying-anchored knobs stay at their inert `AsParams::default()`
            // (`underlying_weight`/`window_secs`/`atm_blackout_scale` all `0.0`). A caller opts in
            // by setting `underlying_symbol` and those knobs — all operator-tunable via `as_params`.
            underlying_symbol: None,
            // Rewards OFF by default (byte-identical mount).
            reward: None,
            // Flow-toxicity guard OFF by default (byte-identical mount).
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

    /// A `$`-scale crypto paper-maker configuration — the generalization TWIN of [`Self::outcome_token`]
    /// (the crypto-domain lift). Where `::outcome_token` tunes the Avellaneda–Stoikov layer for `[0,1]`
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
    /// - `q_scale = 3e-2`: inventory is normalized `q_norm = position/q_scale`. The `::outcome_token`
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
            // Skew / breaker / refresh-tolerance OFF by default (audit F9), same as `::outcome_token`.
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
/// (`ReconClient::fetch_fee_rates`, preferred by `crate::resolve_fee_schedule`) but needs a live
/// venue's `ReconClient`, which exists only where the caller's reconcile gate is on
/// ([`NodeConfig::recon_enabled`]), and is not reachable from a pure constructor, so a mount on a
/// VIP tier cheaper than tier 0 is floored CONSERVATIVELY — the safe direction: it can refuse a
/// mount that would in fact have been marginally profitable, never admit one that is not.
/// Threading the live rate in is the follow-up.
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

// The equity sampler's sink signature, named at its canonical path. (vike-run's root, and then
// vike-mount's, re-exported it as a second name; no call site in the tree named that one, so it
// went.)
use vike_core::runtime::EquitySampleHook;

/// WHICH operator HALT sentinel a paper book built by this crate watches.
///
/// ⚠ **This type exists because a MOUNT is armed BY DESIGN, and a TEST that drives a mount must not
/// inherit the operator's kill switch.** `paper_client_for` arms every book it builds from the
/// process-wide path (`vike_bridge_core::halt::halt_path_from_env`: `<project>/settings/state/HALT`,
/// else `<exe_dir>/HALT`) — right for a daemon, and wrong for a
/// test, whose verdict would then depend on whether a file happens to exist on the box running it.
///
/// That is not hypothetical. Arming this seam with no such choice available made five tests in this
/// crate fail on any box holding a sentinel — including `tests::taker_fee_of`'s two callers, which
/// surface as a confusing `assert_eq!(fills.len(), 1)` fee panic that never mentions halt, because a
/// halted book produces no fill. MEASURED on the CI box with a real file at the resolved path (then named
/// by `VIKE_HALT_FILE`, which decision 0099 has since retired): 22 tests across the workspace
/// flipped, 5 of them here. CI was green only because no runner happens to have the file.
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
    /// `OnceLock`, so a test cannot steer it without mutating
    /// process-global state under threads (this workspace does not `set_var` under threads).
    Pinned(PathBuf),
}

impl PaperHalt {
    /// The sentinel path a book built under this choice watches.
    pub fn resolve(&self) -> PathBuf {
        match self {
            PaperHalt::ProcessWide => vike_bridge_core::halt::halt_path_from_env(),
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
    /// the row shape (one row per engine plus a `"TOTAL"` row). `None` by default, and no
    /// production caller sets it: `vike-tradehub`'s paper arm leaves both sampler knobs at their
    /// defaults, and the one caller is `crates/vike-mount/tests/mount_scripted.rs`'s
    /// `equity_sample_closure_fires_while_a_position_is_open`, which captures the rows in a
    /// closure. ⚠ This sketched a caller wiring a `RecorderSink` "(mirroring vike-app's
    /// `main.rs`)" until 2026-09-28. That `main.rs` wired its own core's `on_equity_sample` into a
    /// `RecorderSink`, and the wiring was deleted with that local core on 2026-09-09 (#1727); no
    /// root wires the sampler's sink today, so there is nothing left to mirror.
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
    /// field's doc for the trade-off (`vike-app`'s `main.rs` documented the same for its own mount,
    /// while it had one).
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
    /// The write-ahead command JOURNAL this mount opens, already resolved by the caller.
    ///
    /// `None` (the default) falls back to [`vike_core::journal_config_from_env`], which is what
    /// this builder always did — so every existing caller is byte-identical.
    ///
    /// It exists because `config.journal_dir` could not reach a paper mount: the resolution lived
    /// in a `vike-core` library env read, several frames below the binary that owns the settings
    /// sweep, and `vike_config::CONSUMPTION` carried the key as a written admission for it. Here
    /// for the same reason as the two flag knobs above — `vike-tradehub` resolves it once for the
    /// whole process (`crates/vike-tradehub/src/tradehub_cli.rs`'s `journal_vars`, where the
    /// ENVIRONMENT still beats the file) and a paper rehearsal that journalled somewhere else
    /// would rehearse the wrong thing.
    pub journal: Option<vike_core::JournalConfig>,
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
            journal: None,
        }
    }
}

/// Build + spawn the paper maker core: `PaperExecutionClient` behind the `ExecutionClient` seam, an
/// A-S [`SpreadMaker`] mounted via [`StrategyMount`], on [`spawn_core`] — the PRODUCTION runtime. This
/// is feature-free (no Polymarket/venue code): the daemon and the offline tests share it verbatim,
/// differing ONLY in what feeds the returned handle (a venue's real feed vs a scripted one), both
/// through a [`MakerSink`]. Exactly
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
/// `crate::make_engine`, so arming the eleven fallback arms over there reached this daemon not
/// at all: `touch <project>/settings/state/HALT` on the box running the paper node did nothing, silently, which is
/// precisely the mount an operator rehearses the switch on. Both seams now resolve the SAME path
/// (`vike_bridge_core::halt::halt_path_from_env`), so a node has one sentinel however it was
/// mounted. (It was spelled through vike-mount's own `halt`, a re-export kept so vike-run need not
/// learn about the transport stack, until vike-run merged into vike-mount — docs/decisions/0098.)
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
/// (`/srv/vike-<unit>/settings/state/HALT` under the shipped unit's project root). MEASURED on
/// the CI box, when the sentinel still had a `VIKE_HALT_FILE` override (decision 0099 retired it):
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
/// (`reward: None`) never calls [`SpreadMaker::with_liquidity_rewards`] and is byte-identical to
/// before this wiring. A `Some`
/// carrying `weight == 0` is itself inert (rewards OFF) per [`RewardParams`], so the opt-in gate is
/// the reward `weight`, not merely the presence of the params.
///
/// The FLOW-TOXICITY guard is threaded the same way: applied ONLY when `cfg.toxicity` is `Some`, so
/// a default mount (`toxicity: None`) never calls [`SpreadMaker::with_flow_toxicity`] and stays
/// byte-identical. A `Some` with both knobs `0.0` is itself inert per [`ToxicityParams`], and the
/// guard only ever moves a quote once a producer actually feeds the maker's `on_flow` — which no
/// mount does since the producer was deleted (see [`MakerMountConfig::toxicity`]).
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

/// The spawned LIVE maker mount: the wired-market [`Node`] with the A-S [`SpreadMaker`] folded into
/// its [`CoreConfig::strategy`]. Unlike [`MakerMount`] there is **no `fills` field** — over
/// [`build_node`] the `ExecutionClient` is a real venue (credential-gated), not the paper exchange, so
/// fills surface only through the [`CoreHandle`]'s `CoreSnapshot` / the journal, never a paper-fill log.
pub struct LiveMakerMount {
    /// The live node — destructure for `handle` / `forwarder_stop` / `live_venues`, exactly like a
    /// direct [`build_node`] caller (`let crate::Node { handle, forwarder_stop, .. } = mount.node;`).
    pub node: Node,
}

/// The live twin of [`build_paper_maker_core`]: mount the A-S [`SpreadMaker`] on the REAL wired-market
/// [`build_node`] core (per-venue exec credential-gated) instead of the paper exchange. Unlike the
/// paper builder it spawns **no feed** — the caller wires the venue's live `Feeds` onto the returned
/// `node.handle`'s ingest lanes (via [`vike_core::CoreLaneSink`]), exactly as `vike-tradehub`'s
/// `wire_venue_feeds` does.
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
/// path was already strategy-agnostic (the [`NodeConfig`] safety knobs, the `make_engine` mount of
/// every wired market, the reconcile mount, the feed wiring — none of them read a maker field),
/// and the ONLY A-S line was `build_maker(cfg)` sitting INSIDE the builder instead of at the call
/// site. Lifting it out is a signature change, not new machinery.
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
/// `crate::make_engine` sets no `extra_symbols`, so a venue engine accepts exactly the symbol
/// it was mounted on ([`vike_exec::ExecutionEngine::accepts_symbol`]) and a foreign symbol's orders
/// and fills are SILENTLY dropped — no error anywhere. The composition root's wired markets
/// ([`NodeConfig::markets`]) are the table that says what each venue was mounted on, and the caller
/// is where that check belongs (it is the only party that knows which markets it built the node
/// with).
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
/// to asserting nothing). Passing an EMPTY [`crate::preflight::PreflightReport`] both restores
/// the intent and makes such a test NETWORK-FREE, which it was not before: it was issuing a real
/// signed bybit read with a fake key and the result merely happened not to matter.
///
/// ⚠ Production callers must use [`build_live_strategy_core`]. An injected empty report is "no
/// preflight ran", which is only ever right for a caller that is not mounting a real venue.
pub fn build_live_strategy_core_with_preflight(
    strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
    spec: &MountSpec,
    node_cfg: NodeConfig,
    preflight: &crate::preflight::PreflightReport,
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
        journal,
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
        // ⚠ The CALLER's already-resolved answer first — that is what lets `config.journal_dir`
        // enable the WAL on a paper mount — falling back to the process env, which is what this
        // line always did and what every caller passing no `journal` still gets.
        journal: journal.or_else(vike_core::journal_config_from_env),
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
        journal,
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
        // ⚠ The CALLER's already-resolved answer first — that is what lets `config.journal_dir`
        // enable the WAL on a paper mount — falling back to the process env, which is what this
        // line always did and what every caller passing no `journal` still gets.
        journal: journal.or_else(vike_core::journal_config_from_env),
        oco_cancel_sibling_on_dead_exit,
        cancel_orders_on_shutdown,
        // `None` (the default) ⇒ runtime `Command::MountStrategy` refuses — see the opts field.
        strategy_factory,
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(primary_engine, engines, config);
    MultiStrategyMount { handle, fills }
}

/// N strategies on the REAL wired-market [`build_node`] core — the multi-mount twin of
/// [`build_live_strategy_core`] (split-plane I10), and what a `[[mounts]]` daemon profile mounts
/// LIVE.
///
/// The first mount becomes [`CoreConfig::strategy`] and the rest [`CoreConfig::extra_mounts`] —
/// `build_node` already mounts one engine per venue ([`NodeConfig::markets`]) and `spawn_core_multi`
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
/// a venue's real feed (`vike-tradehub`'s Polymarket `Feeds`) and a scripted feed (offline test).
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

#[path = "run_tests.rs"]
#[cfg(test)]
mod run_tests;
