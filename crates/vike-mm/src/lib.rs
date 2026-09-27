//! `vike-mm` — the HFT market-maker crate: the [`SpreadMaker`] reference strategy and its pure
//! pricing/risk helpers, extracted from vike-core (the vike-mm extraction) into a dedicated,
//! CI-gated crate that depends on `vike-model` alone. It does NOT own the runtime — vike-core's
//! live core still MOUNTS the maker (via a `Box<dyn Strategy<LiveBroker>>`), because
//! `LiveBroker: HftBroker`; only the maker code moved here, engine-agnostic over the trait.
//!
//! HFT reference strategies. They use the tagged-resting-order verbs (tagged submits + in-place
//! modify + per-tag cancel + a signed-position read) that are NOT on the portable `Broker` surface
//! but ARE on its [`HftBroker`] extension, so they are written `impl<B: HftBroker> Strategy<B>` and
//! stay engine-agnostic over ANY HFT broker — the live `LiveBroker` (vike-core's runtime) mounts
//! them today because `LiveBroker: HftBroker`. Because `HftBroker: Broker`, a plain `Broker`-only
//! backtest engine still cannot mount them, so the type system keeps documenting the HFT
//! requirement (per the write-once rule in `vike_model`'s strategy doc). Their purpose is to
//! demonstrate that the HFT primitives compose end-to-end: tick dispatch → tagged resting orders →
//! in-place modify as the market moves.
//!
//! `SpreadMaker` also carries **inventory-skew size shaping**: it reads its own signed position
//! off the broker ([`HftBroker::position`]) and biases the bid/ask SIZES toward a target inventory (default 0 =
//! stay flat) — quoting a larger ask + smaller bid when long (lean to sell down) and the mirror
//! when short. The shaping is a pure function of position (`skew_multipliers`), unit-tested in
//! isolation; with the skew intensity at its neutral default (`0.0`) both multipliers are exactly
//! `1.0`, reducing to the original fixed-size behavior bit-for-bit.
//!
//! On top of the skew it carries a **per-side fill-rate circuit breaker** (audit mm1) — the
//! adverse-selection guard the plain maker lacked. It overrides [`Strategy::on_fill`] to track
//! recent fills per side over a sliding EVENT-TIME window, and NETS the two sides against each
//! other (round-trip netting): a bid fill later offset by an ask fill is a completed, inventory-
//! neutral round-trip (the desired MM outcome) and must NOT trip the guard — only NET
//! one-directional accumulation on one side does. When the net (this-side minus other-side) fill
//! size within the window trips the threshold, that ONE side is suppressed for a cooldown window:
//! its resting quote is PULLED ([`HftBroker::cancel_tagged`]) and not re-quoted until the
//! cooldown ts expires, while the other side keeps quoting (and re-skewing) normally. The trigger
//! is a pure function of the fill window (`net_signed_fills`), unit-tested in isolation; with the
//! breaker at its neutral defaults (window/threshold/cooldown all `0`) it never engages and the
//! strategy reduces EXACTLY to the skew-only maker above.
//!
//! On top of BOTH of those it carries a **quoting-style registry** + **own-order-book filtration**
//! (audit mm3) — the two gaps the single hardcoded mid ± fixed-spread formula left:
//! - [`QuoteStyle`] (`Top`/`Join`/`Mid`/`Depth`) are PURE functions of the book, selected via
//!   [`SpreadMaker::with_quote_style`]. `Mid` (midpoint ± `half_spread`) is the DEFAULT and reproduces the
//!   original pricing bit-for-bit, so an unconfigured maker is byte-identical to before.
//! - Own-order-book filtration (opt-in, [`SpreadMaker::with_own_order_filtration`]) subtracts the
//!   maker's OWN resting quotes from the public book before pricing (`filter_own`), so it never
//!   joins or leans on its own order — the correctness fix for quoting on the same feed it
//!   consumes. Off by default → the maker prices straight off the feed exactly as before.
//!
//! Both are pure (`BookView::priced` + `filter_own`, unit-tested in isolation) and COMPOSE with the
//! skew (which shapes the SIZES) and the breaker (which decides WHICH sides quote): the style/filter
//! decide the PRICES. The whole pipeline is shared by the L1 quote lane ([`Strategy::on_quote_tick`])
//! and the L2 book lane ([`Strategy::on_order_book`]) through one internal `requote`.
//!
//! On top of all of that it carries an **order-refresh TOLERANCE** (the anti-churn gate): before
//! re-issuing a modify for a side, the freshly computed target `(price, size)` is compared against
//! what that side already has RESTING, and a drift inside the configured
//! [`RefreshTolerance`] (basis points of the resting value, so one tuning reads sanely on both a
//! 0..1 prediction market and a five-figure crypto book) SKIPS the modify entirely — the resting
//! order, and its venue queue position, are left alone. Without it the maker emits two venue
//! modifies per book delta forever, which on a rate-limited CLOB reached over a long-haul link is
//! its biggest wire cost. `None` (the DEFAULT, and an all-zero bag too) ⇒ OFF: every tick re-quotes
//! both sides exactly as before, byte-identical. The gate is consulted ONLY on the re-price path —
//! a PULL (the breaker's suppression cancel) is never tolerance-gated, so tolerance can never
//! strand a quote that should come off the book. A FILL likewise invalidates that side's snapshot
//! ([`Strategy::on_fill`]), so a partial fill's size top-up is always re-issued rather than being
//! read as "no change" against the maker's INTENDED (pre-fill) quote.
//!
//! `SpreadMaker` is also LIVE-RE-TUNABLE (the live-parameter plane): its whole tunable bag is
//! hot-swapped by [`Strategy::on_params_updated`] from a [`StrategyParams::SpreadMaker`] payload,
//! WITHOUT unmounting, so the resting bid/ask keep their venue queue position (the next tick
//! re-prices them in place via modify — never cancel/replace unless the price actually moves).
//!
//! The `HftBroker` links above are reference-style so the trait needs no `use` in this file:
//! the maker's only generic-over-`B` code (the quote-emission unit) now lives in `quote.rs`.
//!
//! [`HftBroker`]: vike_model::HftBroker
//! [`HftBroker::position`]: vike_model::HftBroker::position
//! [`HftBroker::cancel_tagged`]: vike_model::HftBroker::cancel_tagged

use std::collections::VecDeque;

use toml::Value;
use vike_model::{
    AsParams, FlowToxicity, HorizonMode, KappaMode, LadderParams, PriceDomain, RefreshTolerance,
    ReservationModel, RewardParams, SpreadMakerParams, SpreadModel, SpreadSource, ToxicityParams,
    VarianceMode,
};
// `QuoteStyle` now lives in vike-model (the bottom layer) so it can ride the typed live-params
// payload; re-exported at this path so `vike_mm::QuoteStyle` resolves, and used by the maker + its
// tests here. The pricing arithmetic stays in this crate ([`book::BookView::priced`]).
pub use vike_model::QuoteStyle;

// The maker's pure helpers, split into cohesive modules (behavior byte-identical to the former
// single file): `skew` = inventory-skew sizing + fill-rate netting, `book` = the normalized book
// view + quote-style pricing + own-order filtration, `avellaneda` = the A-S pricing layer,
// `fits` = its κ/A estimators, `quote` = the shared quote-emission step both `Strategy` lanes
// drive, `strategy_impl` = the `Strategy` tick/fill/params lanes.
mod alpha;
mod avellaneda;
mod book;
mod fairvalue;
mod fits;
mod ladder;
// The pure correlated-inventory (multi-asset Guéant) skew core — `γ·(Σ·q)`. Written ahead of the
// live multi-symbol maker mount (a DEFERRED follow-up), so nothing on a live path calls it yet;
// `#[allow(dead_code)]` keeps the merge-gate clippy green until the mount wires it in.
#[allow(dead_code)]
mod multi_asset;
mod own_book;
mod quote;
mod refresh;
mod settlement;
mod spread_source;
// The band→taker-flatten impulse leg (Guilbaud–Pham). Formerly a Group-C DEFERRED pure core under
// `#[allow(dead_code)]`; the xEMM mount below now calls it as its hard-naked-band impulse, so the
// allow is gone and clippy gates it like any other module.
mod taker_flatten;
// The CROSS-EXCHANGE maker — likewise formerly a `#[allow(dead_code)]` pure core, now a live
// two-venue mount ([`XemmMaker`]). What unblocked it: `vike_core`'s cross-venue ORDER routing
// (`MountLeg::at` → `resolve_intent_venue`) plus the cross-venue REFERENCE-QUOTE lane
// (`Strategy::on_reference_quote`), which together give a maker on venue A both the ability to see
// venue B's touch and the ability to hedge there.
mod xemm;
// The liquidity-rewards MODEL + fold is a `pub mod` so its pure, unit-tested scoring functions
// (`spread_score`/`side_score`/`q_min`) are public API a Polymarket mount / analysis can compute
// expected reward with; the maker's `clamp_into_band`/`moas_holds` fold helpers stay `pub(crate)`.
/// The cross-platform pin for this crate's quote math. `#[cfg(test)]`, so it compiles into no
/// shipped build — and it lives INSIDE `src/` rather than under `tests/` because every function it
/// covers is `pub(crate)` in a private module, which an integration test cannot reach. Its own
/// module doc carries the measurement and what it does and does not buy.
#[cfg(test)]
mod platform_probe;
pub mod reward;
mod skew;
mod strategy_impl;
mod underlying;

use avellaneda::AsState;
use settlement::accelerated_pull;
use skew::FillRec;
use underlying::UnderlyingTracker;

// The own-order LADDER (opt-in, [`SpreadMaker::with_own_order_book`]) — the multi-order,
// race-aware alternative to the single-snapshot `filter_own` above. Public so a mount/runtime can
// drive REAL venue lifecycle events into it via [`SpreadMaker::own_book_mut`].
pub use own_book::{
    OwnBookFiltration, OwnOrder, OwnOrderBook, OwnQtyFilter, OwnSide, OwnStatus, StatusMask,
};

// The CROSS-EXCHANGE maker (the second reference strategy in this crate): rests on venue A at
// prices derived from venue B's touch and hedges every fill on B. Same `impl<B: HftBroker>
// Strategy<B>` shape as [`SpreadMaker`], so the live core mounts it identically. See the `xemm`
// module doc for the design — in particular why the persistent A-vs-B basis is answered by a
// PLACEMENT clamp against venue A's own touch rather than by a basis term in the price.
pub use xemm::{HaltReason, XemmMaker};

/// A minimal two-sided market maker with optional inventory-skew size shaping, a selectable
/// [`QuoteStyle`], and opt-in own-order-book filtration. On every quote/book update it keeps a
/// resting bid and ask priced by the style (default `Mid` = a fixed `half_spread` off the mid),
/// RE-PRICING (and re-sizing) them in place via `modify` (which preserves venue queue priority)
/// instead of cancel/replace. The tags `"bid"`/`"ask"` name the two resting orders so the runtime
/// can resolve them across ticks without the strategy ever seeing a client-order-id.
///
/// Sizes are shaped from the current inventory vs the configured `target_inventory`
/// ([`SpreadMakerParams::target_inventory`]): when long the
/// ask grows and the bid shrinks (bias to reduce the position), when short the reverse. Construct
/// with [`SpreadMaker::new`] for the fixed-size mid-quoting maker, then opt into skew with
/// [`SpreadMaker::with_skew`], the fill-rate breaker with [`SpreadMaker::with_fill_breaker`], a
/// non-`Mid` style with [`SpreadMaker::with_quote_style`], and/or own-order filtration with
/// [`SpreadMaker::with_own_order_filtration`]. They all COMPOSE — the breaker decides WHICH sides
/// quote, the skew decides their SIZES, the style + filter decide their PRICES.
pub struct SpreadMaker {
    /// The maker's whole CONFIG bag — every live-tunable knob, held as the SAME
    /// [`SpreadMakerParams`] shape the live-parameter plane transports (audit F10), so
    /// [`SpreadMaker::params`]/`apply_params` are one struct copy instead of a field-by-field
    /// splat. Per-knob semantics live on [`SpreadMakerParams`]'s field docs and on the `with_*`
    /// builders below (skew/breaker/style/filtration/tolerance/ladder/reward/toxicity); a new knob
    /// now touches the vike-model struct + [`SpreadMaker::new`]'s literal + its consumer — never
    /// `params()`/`apply_params` again.
    ///
    /// ONE exception: the A-S sub-bag. `cfg.avellaneda_stoikov` is PINNED `None` — the live A-S
    /// params' single authority is `as_state.params` (which `with_spread_model`/`set_params`
    /// mutate in place), so holding a second copy here would drift. [`SpreadMaker::params`]
    /// re-attaches the bag from `as_state` on the way out; `apply_params` strips it on the way in.
    ///
    /// Deliberately CONFIG-ONLY — the config-vs-state split that makes `apply_params`
    /// hot-swap-safe: the runtime/mount-structural state (`last_flow`, `own_book`, the two
    /// [`SideState`]s, `underlying`, `learned_book_tick`) lives as sibling fields the swap never
    /// touches, so a live re-tune keeps resting orders, queue position, and learned state.
    cfg: SpreadMakerParams,
    /// The latest per-side FLOW-TOXICITY reading delivered by [`Strategy::on_flow`], applied at the
    /// NEXT quote by the [`SpreadMakerParams::toxicity`] guard. `None` until a reading arrives (and on every
    /// maker that is never fed one), which is what keeps a toxicity-off maker byte-identical. RUNTIME
    /// state, NOT a config knob: mount-structural like the resting-order snapshot, so `apply_params`
    /// DELIBERATELY leaves it untouched across a re-tune.
    last_flow: Option<FlowToxicity>,
    /// Whether `last_flow` was set by an EXTERNAL [`Strategy::on_flow`] reading (`true`) rather than by
    /// the OFI-toxicity synthesis fallback (Group-B, PR-3). Set once [`Strategy::on_flow`] ever fires
    /// and never cleared, so an external toxicity feed takes PRECEDENCE permanently: while `true`, the
    /// internal OFI synthesis in `requote` is skipped and never clobbers the real reading. `false` on a
    /// maker that is never fed one — the state that lets the synthesis populate `last_flow` as its
    /// fallback. RUNTIME state, not a config knob (like `last_flow` itself): `apply_params` leaves it
    /// untouched. Inert (and byte-identical) whenever `ofi_toxicity_scale == 0` or no toxicity guard is
    /// configured, since the synthesis it gates never runs then.
    flow_external: bool,
    /// The BID side's per-side runtime quote state (resting snapshot, ladder rungs, breaker
    /// deadline, refresh staleness, placement clock) — see [`SideState`] (audit F5: formerly eight
    /// mirrored `bid_*`/`ask_*` field pairs). RUNTIME state, not config: `apply_params` never
    /// touches it, so resting orders + queue position survive a re-tune.
    bid: SideState,
    /// The ASK side's [`SideState`] — the exact mirror of `bid`.
    ask: SideState,
    /// Opt-in own-order LADDER ([`SpreadMaker::with_own_order_book`]). `None` (default) ⇒ the
    /// per-side `SideState::own` snapshot pair above is what `filter_own` subtracts, byte-identical
    /// to before. `Some` ⇒ filtration instead consults this multi-order, accepted-buffer-gated
    /// [`OwnOrderBook`], which fixes the two things a single snapshot cannot express: N own orders
    /// at one level, and the accept→public-feed race (see the `own_book` module doc). Both paths
    /// are still gated by `filter_own`, so turning filtration off live disables either one.
    ///
    /// Deliberately NOT part of [`SpreadMakerParams`] (a vike-model wire type): the ladder is a
    /// mount-time structural choice, so `apply_params` leaves it untouched — same treatment as the
    /// resting-order state and the breaker window.
    own_book: Option<OwnBookFiltration>,
    /// recent fills (side, size, EVENT-ts) inside the window — the netting accumulator
    fills: VecDeque<FillRec>,
    /// Live-params version (the live-parameter plane): bumped once per applied
    /// [`Strategy::on_params_updated`]. Starts at `0`; observable so a caller/test can confirm a
    /// re-tune actually landed. Read-only from outside — only `on_params_updated` advances it.
    pub params_epoch: u64,
    /// Optional Avellaneda–Stoikov pricing layer (audit mm-quote). `None` (default, via
    /// [`SpreadMaker::new`]) ⇒ the maker prices off the fixed [`QuoteStyle`]/`half_spread` exactly as
    /// before, BYTE-IDENTICAL. `Some` (via [`SpreadMaker::with_avellaneda_stoikov`]) ⇒ the two prices
    /// come from an inventory-aware reservation price + optimal spread instead; that output still
    /// flows UNCHANGED into the size-skew → breaker → filtration → modify-in-place tail below. Holds
    /// the online σ̂²/κ estimators as plain `&mut self` state (single-writer core: no lock).
    as_state: Option<AsState>,
    /// Cross-symbol UNDERLYING tracker ("Option B"): turns the stream of underlying-spot marks routed
    /// in via [`Strategy::on_mark`](vike_model::Strategy::on_mark) into the `(s_now, s_open,
    /// sigma_per_sec)` triple the A-S underlying-anchored fair mid / ATM guard consume through
    /// `AsState::set_underlying`. Mount-STRUCTURAL (like `own_book`) — NOT part of [`SpreadMakerParams`]
    /// — so `apply_params` never touches it. INERT unless BOTH an underlying mark is routed in AND the
    /// A-S layer is on with `underlying_weight`/`atm_blackout_scale` raised; a maker that is never fed
    /// a mark keeps this cold, so it is byte-identical to before.
    underlying: UnderlyingTracker,
    /// The venue price grid LEARNED from the L2 book lane, adopted on the L1 quote lane so the
    /// `min/max_half_spread_ticks` floor/cap AND the grid snap resolve against the SAME tick on both
    /// lanes. `None` until the first L2 book with a positive `tick_size` arrives; a pure-L1 feed never
    /// sets it, so it falls back to the configured [`SpreadMakerParams::tick_size`] (the param stays the
    /// L1-only source).
    /// This closes the silent per-lane spread divergence when the configured `tick_size` disagrees
    /// with the book's grid (the quote lane used the param, the book lane the book, so ONE
    /// `*_half_spread_ticks` produced two different spreads). Runtime state, NOT a config knob —
    /// `apply_params` never touches it, like the resting-order snapshot. Byte-identical whenever the
    /// param already equals the book grid (the correct config), which is the only supported setup.
    learned_book_tick: Option<f64>,
    /// Latch for the UNPRICED-BOOK hold edge (`SpreadMaker::note_no_quote`): `true` while `requote`
    /// is returning early because the pricing step yielded no two-sided quote.
    ///
    /// It exists for the same reason [`AsState::fee_floor_refusing`] does, one layer UP — and the
    /// layer that was still silent. `requote`'s `let Some(..) = priced else { return }` swallows
    /// EVERY unpriced tick without a word: a one-sided book, a book whose sides never arrive, a
    /// blackout pinned at a wall. So a maker can hold forever with `orders:0`, `fault:null` and an
    /// empty journal, which is exactly the state the CI box sat in on 2026-08-17 across ~112k summary
    /// `seq` while its bybit market-data socket carried keepalives and no `orderbook.50` data. The
    /// fee-floor warn could not speak, because `AsState::price` returns at `fair_value(view)?`
    /// BEFORE reaching it — the guard against silent holds was itself downstream of one.
    ///
    /// Edge-triggered for the same reason as its sibling: `requote` is the vike-core hot fold, whose
    /// rule is "instrument per-order boundaries and fault transitions only". RUNTIME state, not a
    /// config knob — `apply_params` leaves it untouched, like `learned_book_tick`. NOT persisted: a
    /// restart re-derives it from the first tick.
    no_quote_holding: bool,
}

/// ONE side's (bid's or ask's) runtime quote state, held as the [`SpreadMaker::bid`]/
/// [`SpreadMaker::ask`] pair (audit F5 — formerly eight mirrored `bid_*`/`ask_*` field pairs
/// driving fully mirrored per-side arms; the per-side helpers now take `is_bid` and index one
/// state via [`SpreadMaker::side`]/`side_mut`). All of it is RUNTIME state, not config:
/// `apply_params` leaves both sides untouched across a re-tune, so resting orders and their venue
/// queue position survive. [`Default`] is exactly the fresh-maker state (nothing resting, nothing
/// suppressed, clocks at `0`).
#[derive(Default)]
struct SideState {
    /// Our OWN currently-resting quote on this side as `(price, size)`, or `None` — what
    /// single-snapshot filtration subtracts from the public book next tick. Tracked whenever the
    /// side is placed/modified, cleared when pulled. NOTE it is the maker's INTENDED quote, not
    /// the venue-side remainder — see `refresh_stale`.
    own: Option<(f64, f64)>,
    /// Resting ladder rungs as `(price, size)`, index `k` ⇒ tag `"bid{k}"`/`"ask{k}"`,
    /// deepest-last — the multi-order twin of `own`, tracked ONLY on the active-ladder path. EMPTY
    /// on the default single-quote path (which uses `own`/`placed` instead), which is what keeps
    /// that path byte-identical: the ladder retire/diff code is a guarded no-op while this is
    /// empty.
    rungs: Vec<(f64, f64)>,
    /// EVENT-ts until which this side is suppressed by the fill-rate breaker (`0` = not
    /// suppressed).
    suppressed_until: i64,
    /// Whether a single quote is currently resting on this side (per-side so one side can be
    /// pulled alone).
    placed: bool,
    /// Order-refresh tolerance INVALIDATION flag: set by [`Strategy::on_fill`] whenever a fill
    /// lands on this side, cleared the moment the side is next placed/re-priced/pulled.
    ///
    /// WHY it exists: `own` is the maker's INTENDED quote, not what actually remains resting at
    /// the venue. After a PARTIAL fill the venue-side size is smaller than the intended size, yet
    /// an intended-vs-target comparison still reads "no change" — so a tolerance-gated maker would
    /// never top the side back up and would silently quote less size than configured. The flag
    /// marks that side's snapshot STALE, forcing the next tick through the modify arm (which
    /// re-issues the full configured size) exactly once.
    ///
    /// INERT WHEN THE GATE IS OFF: it is read ONLY by `refresh_skips`/`reward_moas_holds` (and the
    /// ladder's per-rung twin), which short-circuit to "re-quote" — so setting it changes nothing
    /// on the default path (byte-identical), which is why `on_fill` may set it before the
    /// breaker's early return.
    refresh_stale: bool,
    /// Event-ts this side was last placed/re-priced at (`0` = not currently resting) — the reward
    /// MIN-ORDER-AGE (moas) clock (`reward_moas_holds`) AND the OwnFillFit exposure clock
    /// (`feed_own_outcome`), inert dead state while both are off (byte-identical). Refreshed on
    /// every real place/re-price, reset to `0` on a pull, and DELIBERATELY not touched on a
    /// tolerance/moas SKIP — so a held side's age keeps accumulating toward the floor.
    quoted_ts: i64,
}

/// Read a TOML value as `f64`, accepting a TOML float OR integer (`qty = 1` == `qty = 1.0`) — the
/// lenient numeric reader convention the backtest registry's `buy_hold`/`grid` params use. Feeds
/// [`SpreadMaker::from_params`] only.
fn as_f64(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// A `[strategy.params]` key whose value NAMES A VARIANT that does not exist — a profile author's
/// typo, returned by [`SpreadMaker::from_params`] instead of being absorbed into the default.
///
/// ⚠ **This type exists because `Option` could not say which of two things happened.** Each enum
/// reader below used to return `None` for BOTH "the key is absent" and "the key is present and
/// unrecognized", and `from_params` folded that `None` into `unwrap_or(default)` — so
/// `spread_model = "gueant_lehale"` (one `l` short of the accepted `gueant_lehalle`) silently ran
/// Avellaneda–Stoikov while the profile named GLFT, and any startup line reported the fallback as
/// if it had been chosen. A backtest that ran a different model than its profile names is a result
/// nobody can trust, and nothing said so. An ABSENT key still keeps its default — that is the
/// documented reader convention and is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamError {
    /// The key is present and IS a string, but names no variant. Carries the operator's own value
    /// back so the message is greppable against the profile they wrote.
    Unrecognized {
        /// The `[strategy.params]` key, e.g. `"spread_model"`.
        key: &'static str,
        /// The value as written in the profile.
        value: String,
        /// Every accepted spelling of that key, `|`-separated (the match arms, verbatim).
        accepted: &'static str,
    },
    /// The key is present but is not a string at all (`spread_model = 3`) — the same silent
    /// fallback wearing a different shape, since `Value::as_str` also answered `None` for it.
    NotAString {
        /// The `[strategy.params]` key.
        key: &'static str,
        /// The TOML type actually found (`"integer"`, `"table"`, …).
        found: &'static str,
        /// Every accepted spelling of that key, `|`-separated.
        accepted: &'static str,
    },
}

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamError::Unrecognized { key, value, accepted } => write!(
                f,
                "strategy param {key} = {value:?} is not a recognized value (accepted: {accepted})"
            ),
            ParamError::NotAString { key, found, accepted } => write!(
                f,
                "strategy param {key} must be a string, got a TOML {found} \
                 (accepted: {accepted})"
            ),
        }
    }
}

impl std::error::Error for ParamError {}

/// The shared front half of every enum reader below: resolve `key` to its string, or say WHICH of
/// the three states the table is in. `Ok(None)` is the ABSENT key (keep the default); `Err` is the
/// present-but-not-a-string case; `Ok(Some(s))` is the value AS WRITTEN — each reader lowercases
/// for its own match and keeps this copy, so a failure quotes the operator's own spelling back at
/// them rather than a normalized one. `accepted` is threaded through only so a failure can print it.
fn param_str(
    params: &Value,
    key: &'static str,
    accepted: &'static str,
) -> Result<Option<String>, ParamError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(ParamError::NotAString { key, found: v.type_str(), accepted }),
        },
    }
}

/// Parse the `variance_mode` param string (case-insensitive) into a [`VarianceMode`]. `Ok(None)`
/// only for an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] mode; a
/// present-but-unrecognized value is a [`ParamError`], never a silent fallback.
fn read_variance_mode(params: &Value) -> Result<Option<VarianceMode>, ParamError> {
    const KEY: &str = "variance_mode";
    const ACCEPTED: &str = "local_capped | capped | pure_bernoulli | bernoulli | raw_local | raw";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "local_capped" | "capped" => Ok(Some(VarianceMode::LocalCapped)),
        "pure_bernoulli" | "bernoulli" => Ok(Some(VarianceMode::PureBernoulli)),
        "raw_local" | "raw" => Ok(Some(VarianceMode::RawLocal)),
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `spread_model` param string (case-insensitive) into a [`SpreadModel`]. `Ok(None)` only
/// for an ABSENT key (keep the [`AsParams::default`] model); a present-but-unrecognized value is a
/// [`ParamError`] — this is the reader whose silent fallback ran A-S under a profile naming GLFT.
fn read_spread_model(params: &Value) -> Result<Option<SpreadModel>, ParamError> {
    const KEY: &str = "spread_model";
    const ACCEPTED: &str = "avellaneda_stoikov | avellaneda | as | gueant | glft | gueant_lehalle";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "avellaneda_stoikov" | "avellaneda" | "as" => Ok(Some(SpreadModel::AvellanedaStoikov)),
        "gueant" | "glft" | "gueant_lehalle" => Ok(Some(SpreadModel::Gueant)),
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `reservation_model` param string (case-insensitive) into a [`ReservationModel`].
/// `Ok(None)` only for an ABSENT key, so [`SpreadMaker::from_params`] keeps the A-S linear
/// reservation; a present-but-unrecognized value is a [`ParamError`].
fn read_reservation_model(params: &Value) -> Result<Option<ReservationModel>, ParamError> {
    const KEY: &str = "reservation_model";
    const ACCEPTED: &str = "as_linear | as | linear | lmsr | logistic";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "as_linear" | "as" | "linear" => Ok(Some(ReservationModel::AsLinear)),
        "lmsr" | "logistic" => Ok(Some(ReservationModel::Lmsr)),
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `spread_source` param string (case-insensitive) into a [`SpreadSource`]. `Ok(None)`
/// only for an ABSENT key, so [`SpreadMaker::from_params`] keeps the A-S optimal half-spread; a
/// present-but-unrecognized value is a [`ParamError`].
fn read_spread_source(params: &Value) -> Result<Option<SpreadSource>, ParamError> {
    const KEY: &str = "spread_source";
    const ACCEPTED: &str =
        "as_optimal | as | optimal | ls_lmsr | lslmsr | glosten_milgrom | glosten | gm";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "as_optimal" | "as" | "optimal" => Ok(Some(SpreadSource::AsOptimal)),
        "ls_lmsr" | "lslmsr" => Ok(Some(SpreadSource::LsLmsr)),
        "glosten_milgrom" | "glosten" | "gm" => Ok(Some(SpreadSource::GlostenMilgrom)),
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `style` param string (case-insensitive) into a [`QuoteStyle`] — the PLACEMENT of the two
/// quotes on the book (`top` rests at the touch, `join` joins the best, `mid` = reservation ±
/// half-spread inside the spread, `depth` rests `depth_levels` into the book). `Ok(None)` only for an
/// ABSENT key, so [`SpreadMaker::from_params`] keeps [`QuoteStyle::Mid`] (the original behavior); a
/// present-but-unrecognized value is a [`ParamError`]. Exposing this lets a backtest profile sweep
/// the fill-rate-driving placement, not just the pricing model.
fn read_quote_style(params: &Value) -> Result<Option<QuoteStyle>, ParamError> {
    const KEY: &str = "style";
    const ACCEPTED: &str = "top | join | mid | depth";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "top" => Ok(Some(QuoteStyle::Top)),
        "join" => Ok(Some(QuoteStyle::Join)),
        "mid" => Ok(Some(QuoteStyle::Mid)),
        "depth" => Ok(Some(QuoteStyle::Depth)),
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `pricing` param selector (case-insensitive): decides whether
/// [`SpreadMaker::from_params`] chains the A-S layer. `"as"` — and absent/unrecognized, today's
/// behavior, byte-identical — ⇒ `true`: [`SpreadMaker::with_avellaneda_stoikov`] is chained and
/// PRICES, which makes the `style` key INERT (`requote` bypasses the [`QuoteStyle`] placement
/// whenever A-S state is `Some`). `"style"` ⇒ `false`: the A-S layer is SKIPPED so the selected
/// placement (`style`/`depth_levels`) actually prices — the registry path where the `style` key
/// finally does something (audit F8).
///
/// ⚠ Deliberately NOT one of the [`ParamError`] readers: it maps to a `bool`, not to an enum
/// variant, so it has no `_ => None` arm to disambiguate. The residual that leaves is real and
/// named here rather than left implicit — `pricing = "styel"` reads as `"as"` and chains A-S, the
/// same silent-fallback shape the enum readers no longer have. Closing it is a behavior change to
/// this selector's documented "absent/unrecognized ⇒ `as`" contract, not a `None`-vs-`None` fix.
fn read_pricing_chains_as(params: &Value) -> bool {
    !matches!(
        params.get("pricing").and_then(Value::as_str).map(str::to_ascii_lowercase).as_deref(),
        Some("style" | "quote_style")
    )
}

/// Parse the `horizon_mode` param string (case-insensitive) into a [`HorizonMode`]. `Ok(None)` only
/// for an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] mode; a
/// present-but-unrecognized value is a [`ParamError`].
fn read_horizon_mode(params: &Value) -> Result<Option<HorizonMode>, ParamError> {
    const KEY: &str = "horizon_mode";
    const ACCEPTED: &str = "time_to_resolution | resolution | constant_tau | constant";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "time_to_resolution" | "resolution" => Ok(Some(HorizonMode::TimeToResolution)),
        "constant_tau" | "constant" => Ok(Some(HorizonMode::ConstantTau)),
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `kappa_mode` param string (case-insensitive) into a [`KappaMode`]. `Ok(None)` only for
/// an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] mode (`Fixed` —
/// the online fits stay "available but gated"); a present-but-unrecognized value is a
/// [`ParamError`]. This is the audit-F3 selector that makes the whole #790 κ-fit family reachable
/// from a registry/harness table.
fn read_kappa_mode(params: &Value) -> Result<Option<KappaMode>, ParamError> {
    const KEY: &str = "kappa_mode";
    const ACCEPTED: &str = "fixed | live_fit | livefit | live | own_fill_fit | ownfillfit | own";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "fixed" => Ok(Some(KappaMode::Fixed)),
        "live_fit" | "livefit" | "live" => Ok(Some(KappaMode::LiveFit)),
        "own_fill_fit" | "ownfillfit" | "own" => Ok(Some(KappaMode::OwnFillFit)),
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

/// Parse the `price_domain` param string (case-insensitive) into a [`PriceDomain`]. `Ok(None)` only
/// for an ABSENT key, so [`SpreadMaker::from_params`] keeps the [`AsParams::default`] domain; a
/// present-but-unrecognized value is a [`ParamError`].
/// `"band"` reads its walls from `band_lo`/`band_hi` (defaulting to the `[0, 1]` unit interval).
fn read_price_domain(params: &Value) -> Result<Option<PriceDomain>, ParamError> {
    const KEY: &str = "price_domain";
    const ACCEPTED: &str = "unit_interval | unit | unbounded | band";
    let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
    match raw.to_ascii_lowercase().as_str() {
        "unit_interval" | "unit" => Ok(Some(PriceDomain::UnitInterval)),
        "unbounded" => Ok(Some(PriceDomain::Unbounded)),
        "band" => {
            let f = |k: &str| params.get(k).and_then(as_f64);
            Ok(Some(PriceDomain::Band {
                lo: f("band_lo").unwrap_or(0.0),
                hi: f("band_hi").unwrap_or(1.0),
            }))
        }
        _ => Err(ParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
    }
}

impl SpreadMaker {
    /// Fixed-size two-sided maker — NO inventory skew (`skew = 0`), identical to the original
    /// behavior. `target_inventory`/`max_inventory` default to `0.0`/`1.0` (inert while `skew`
    /// is `0`).
    pub fn new(qty: f64, half_spread: f64) -> Self {
        SpreadMaker {
            // The one remaining knob-listing site (with the vike-model struct itself): a new knob
            // is added here with its neutral default, and `params()`/`apply_params` pick it up for
            // free as struct copies.
            cfg: SpreadMakerParams {
                qty,
                half_spread,
                target_inventory: 0.0,
                max_inventory: 1.0,
                skew: 0.0,
                fill_window_ms: 0,
                net_fill_threshold: 0.0,
                suppress_cooldown_ms: 0,
                style: QuoteStyle::Mid,
                depth_levels: 1,
                tick_size: 0.0,
                filter_own: false,
                // PINNED None — the A-S bag's single authority is `as_state.params` (see `cfg`).
                avellaneda_stoikov: None,
                refresh_tolerance: None,
                ladder: None,
                reward: None,
                toxicity: None,
            },
            last_flow: None,
            flow_external: false,
            bid: SideState::default(),
            ask: SideState::default(),
            own_book: None,
            fills: VecDeque::new(),
            params_epoch: 0,
            as_state: None,
            underlying: UnderlyingTracker::new(),
            learned_book_tick: None,
            no_quote_holding: false,
        }
    }

    /// Read a harness/registry TOML params table into a working Avellaneda–Stoikov maker — the
    /// `BuyHold::from_params` reader convention (a READER, not a schema: unknown keys ignored, missing
    /// keys fall back to a default), so the backtest strategy registry can resolve + tune a
    /// `SpreadMaker` by name. It builds the fixed-size `Mid` seed ([`SpreadMaker::new`]) then layers
    /// A-S on via [`SpreadMaker::with_avellaneda_stoikov`] (unless `pricing = "style"` — see the
    /// table), so a bare `{}` yields the recommended A-S maker ([`AsParams::default`]) and each
    /// present key overrides one knob. Mirrors how `vike_run::build_maker` composes the live maker
    /// (fixed seed → `with_quote_style` → A-S), minus the reward mount-only sub-bag (the toxicity
    /// bag IS readable here via the `toxicity_*` keys below).
    ///
    /// | key | meaning | default |
    /// |---|---|---|
    /// | `qty` | base quote size per side | `1.0` |
    /// | `tick_size` | venue price grid (the A-S L1 snap/clamp) | `0.0` |
    /// | `half_spread` | fixed-spread seed (unused while A-S prices) | `2·tick_size`, else `0.01` |
    /// | `gamma` | A-S risk aversion `γ` | `AsParams::default` (`0.1`) |
    /// | `q_scale` | A-S inventory normaliser | `AsParams::default` (`100.0`) |
    /// | `min_half_spread_ticks` | A-S half-spread floor (ticks) | `0.0` |
    /// | `max_half_spread_ticks` | A-S half-spread ceiling (ticks) | `0.0` |
    /// | `round_trip_fee_rate` | venue round-trip maker fee (fraction of price) — floors the half-spread at BREAK-EVEN `½·m·s`, and REFUSES the quote when that exceeds the ceiling | unset (no floor) |
    /// | `variance_mode` | `local_capped` \| `pure_bernoulli` \| `raw_local` | `local_capped` |
    /// | `horizon_mode` | `time_to_resolution` \| `constant_tau` | `time_to_resolution` |
    /// | `price_domain` | `unit_interval` \| `unbounded` \| `band` (+ `band_lo`/`band_hi`) | `unit_interval` |
    /// | `pricing` | `as` (A-S prices; the `style` key is then INERT) \| `style` (skip A-S — the [`QuoteStyle`] placement prices) | `as` |
    /// | `style` / `depth_levels` | [`QuoteStyle`] placement (`top`/`join`/`mid`/`depth`) — prices only under `pricing = "style"` | `mid` / `1` |
    /// | `target_inventory` / `max_inventory` / `skew` | inventory-skew size shaping ([`Self::with_skew`]) | `0.0` / `1.0` / `0.0` (off) |
    /// | `fill_window_ms` / `net_fill_threshold` / `suppress_cooldown_ms` | fill-rate breaker ([`Self::with_fill_breaker`]) | `0` / `0.0` / `0` (off) |
    /// | `pull_accel_ramp_ms` / `pull_accel_max` | breaker pull-ACCEL near resolution (needs `resolution_ts`) | `0` / `0.0` (off) |
    /// | `kappa_mode` | `fixed` \| `live_fit` \| `own_fill_fit` (the κ-fit family) | `fixed` |
    /// | `kappa_default` / `kappa_min` / `kappa_max` | fixed κ + fitted-κ clamps | `AsParams::default` |
    /// | `n_min` / `trade_window_ms` | κ-MLE sample floor / tape window (ms) | `AsParams::default` |
    /// | `resolution_ts` | resolution anchor `T` (epoch-ms) — EVERY settlement feature keys off it | unset |
    /// | `tau_hold_ms` / `resolution_blackout_ms` | holding tenor / blackout window (ms) | `AsParams::default` |
    /// | `terminal_penalty_gamma` / `terminal_ramp_ms` | terminal settlement-variance penalty | `0.0` / `0` (off) |
    /// | `underlying_weight` / `underlying_beta` / `window_secs` / `atm_blackout_scale` | underlying-anchored fair mid + ATM blackout guard | `AsParams::default` (off) |
    /// | `flatten_by_ms` / `flatten_strength` | settlement force-flatten schedule | `0` / `0.0` (off) |
    /// | `toxicity_widen` / `toxicity_size_cut` | [`ToxicityParams`] flow-toxicity guard — EITHER key present builds the bag | unset (no bag) |
    /// | `ofi_toxicity_scale` | internal OFI→toxicity synthesis scale (needs the guard + `alpha_lambda_ofi`) | `0.0` (off) |
    /// | `refresh_price_bps` / `refresh_size_bps` | anti-churn re-quote tolerance (bps) | `0.0` (off) |
    ///
    /// NOTE the crypto profile: an unbounded `$`-scale asset wants `price_domain = "unbounded"`,
    /// `variance_mode = "raw_local"`, `horizon_mode = "constant_tau"`, `min_half_spread_ticks = 2`,
    /// `max_half_spread_ticks = 60` — the exact knobs `vike_run::MakerMountConfig::crypto` sets.
    ///
    /// # Errors
    ///
    /// A key from the ENUM column above (`variance_mode`, `horizon_mode`, `price_domain`,
    /// `spread_model`, `reservation_model`, `spread_source`, `kappa_mode`, `style`) that is PRESENT
    /// but names no variant is a [`ParamError`] — the profile fails to resolve rather than running
    /// a different model than it names. An ABSENT key keeps its default, exactly as the reader
    /// convention documents and as the table above states; that path is unchanged, so every
    /// profile that resolves today still resolves. The numeric keys stay lenient (an unparsable
    /// `gamma` is not an enum's problem and has no wrong-variant failure mode).
    pub fn from_params(params: &Value) -> Result<Self, ParamError> {
        let f = |k: &str| params.get(k).and_then(as_f64);
        // i64 twin of `f` for the epoch-ms / window knobs (`x = 5000` == `x = 5000.0`).
        let ms = |k: &str| f(k).map(|v| v as i64);
        let d = AsParams::default();
        let as_params = AsParams {
            gamma: f("gamma").unwrap_or(d.gamma),
            q_scale: f("q_scale").unwrap_or(d.q_scale),
            min_half_spread_ticks: f("min_half_spread_ticks").unwrap_or(d.min_half_spread_ticks),
            max_half_spread_ticks: f("max_half_spread_ticks").unwrap_or(d.max_half_spread_ticks),
            // ABSENT ⇒ `None` ⇒ NO break-even floor, not a zero fee (the `resolution_ts` shape:
            // `.or(default)`, never `.unwrap_or(0.0)`). A harness that wants the floor states the
            // venue's round-trip maker rate; the LIVE mount resolves it from `fee_schedule_for`.
            round_trip_fee_rate: f("round_trip_fee_rate").or(d.round_trip_fee_rate),
            variance_mode: read_variance_mode(params)?.unwrap_or(d.variance_mode),
            horizon_mode: read_horizon_mode(params)?.unwrap_or(d.horizon_mode),
            price_domain: read_price_domain(params)?.unwrap_or(d.price_domain),
            // κ-fit family (audit F3 — the #790 family was unreachable): the mode selector plus the
            // fixed/fitted κ clamps and the MLE's tape window/sample floor. Absent ⇒ `Fixed` at
            // `kappa_default`, byte-identical (the online fits stay "available but gated", exactly
            // as `AsParams::default` documents).
            kappa_mode: read_kappa_mode(params)?.unwrap_or(d.kappa_mode),
            kappa_default: f("kappa_default").unwrap_or(d.kappa_default),
            kappa_min: f("kappa_min").unwrap_or(d.kappa_min),
            kappa_max: f("kappa_max").unwrap_or(d.kappa_max),
            n_min: f("n_min").map(|v| v as usize).unwrap_or(d.n_min),
            trade_window_ms: ms("trade_window_ms").unwrap_or(d.trade_window_ms),
            // SETTLEMENT family (audit F4): `resolution_ts` is the anchor EVERY settlement feature
            // keys off (force-flatten τ / blackout / terminal penalty / underlying anchor /
            // pull-accel — all of them read it), plus the tenor + blackout + terminal-penalty +
            // underlying/ATM knobs. Absent ⇒ each stays at its inert default, byte-identical.
            resolution_ts: ms("resolution_ts").or(d.resolution_ts),
            tau_hold_ms: ms("tau_hold_ms").unwrap_or(d.tau_hold_ms),
            resolution_blackout_ms: ms("resolution_blackout_ms")
                .unwrap_or(d.resolution_blackout_ms),
            terminal_penalty_gamma: f("terminal_penalty_gamma").unwrap_or(d.terminal_penalty_gamma),
            terminal_ramp_ms: ms("terminal_ramp_ms").unwrap_or(d.terminal_ramp_ms),
            atm_blackout_scale: f("atm_blackout_scale").unwrap_or(d.atm_blackout_scale),
            underlying_weight: f("underlying_weight").unwrap_or(d.underlying_weight),
            underlying_beta: f("underlying_beta").unwrap_or(d.underlying_beta),
            window_secs: f("window_secs").unwrap_or(d.window_secs),
            // GLFT selector + its base intensity `A` (see `SpreadModel`). Absent ⇒ A-S, byte-identical.
            spread_model: read_spread_model(params)?.unwrap_or(d.spread_model),
            base_intensity_a: f("base_intensity_a").unwrap_or(d.base_intensity_a),
            // GROUP-B pluggables (see `ReservationModel`/`SpreadSource`). Absent ⇒ the A-S/GLFT path,
            // byte-identical. `lmsr_b`/`ls_lmsr_alpha`/`gm_mu` are read only by their selected model.
            reservation_model: read_reservation_model(params)?.unwrap_or(d.reservation_model),
            lmsr_b: f("lmsr_b").unwrap_or(d.lmsr_b),
            spread_source: read_spread_source(params)?.unwrap_or(d.spread_source),
            ls_lmsr_alpha: f("ls_lmsr_alpha").unwrap_or(d.ls_lmsr_alpha),
            gm_mu: f("gm_mu").unwrap_or(d.gm_mu),
            // GROUP-B reservation shifts (see the `alpha`/`settlement` modules). Absent ⇒ 0 ⇒ inert,
            // byte-identical; `ofi_decay` is read only when `alpha_lambda_ofi != 0`.
            alpha_beta_imbalance: f("alpha_beta_imbalance").unwrap_or(d.alpha_beta_imbalance),
            alpha_lambda_ofi: f("alpha_lambda_ofi").unwrap_or(d.alpha_lambda_ofi),
            ofi_decay: f("ofi_decay").unwrap_or(d.ofi_decay),
            running_penalty_phi: f("running_penalty_phi").unwrap_or(d.running_penalty_phi),
            flatten_by_ms: ms("flatten_by_ms").unwrap_or(d.flatten_by_ms),
            flatten_strength: f("flatten_strength").unwrap_or(d.flatten_strength),
            // Breaker PULL-ACCEL + OFI-toxicity synthesis scale (audit F2/F1 — the #798 pair was
            // unreachable). Absent ⇒ 0 ⇒ inert, byte-identical (the accel also needs a
            // `resolution_ts`; the synthesis also needs a toxicity guard + `alpha_lambda_ofi != 0`).
            pull_accel_ramp_ms: ms("pull_accel_ramp_ms").unwrap_or(d.pull_accel_ramp_ms),
            pull_accel_max: f("pull_accel_max").unwrap_or(d.pull_accel_max),
            ofi_toxicity_scale: f("ofi_toxicity_scale").unwrap_or(d.ofi_toxicity_scale),
            ..d
        };
        let qty = f("qty").unwrap_or(1.0);
        let tick_size = f("tick_size").unwrap_or(0.0);
        let default_half = if tick_size > 0.0 { tick_size * 2.0 } else { 0.01 };
        let half_spread = f("half_spread").unwrap_or(default_half);
        // Opt-in re-quote tolerance (bps of the resting value): the maker HOLDS its resting quote
        // while the target price/size stay within tolerance, instead of re-quoting every tick. This
        // is load-bearing for a queue-position fill backtest — a quote that re-prices every tick
        // forfeits its FIFO slot and can never be hit by a taker trade. `0.0`/`0.0` (the default)
        // ⇒ every tick re-quotes, byte-identical to before ([`with_refresh_tolerance`]'s OFF path).
        let refresh_price_bps = f("refresh_price_bps").unwrap_or(0.0);
        let refresh_size_bps = f("refresh_size_bps").unwrap_or(0.0);
        // Placement style + depth (the fill-rate lever): `style` selects Top/Join/Mid/Depth, absent ⇒
        // Mid (the original behavior, byte-identical). `depth_levels` is only read by `Depth`. NOTE
        // the style only PRICES under `pricing = "style"` — while the A-S layer is chained (the
        // default), `requote` bypasses the QuoteStyle placement entirely.
        let style = read_quote_style(params)?.unwrap_or(QuoteStyle::Mid);
        let depth_levels = f("depth_levels").map(|v| v as usize).filter(|&n| n >= 1).unwrap_or(1);
        // Inventory-skew size shaping (audit F1): `with_skew`'s three knobs verbatim. The absent-key
        // defaults (`0.0`/`1.0`/`0.0`) are exactly `SpreadMaker::new`'s neutral values, so a keyless
        // table is byte-identical (skew off).
        let target_inventory = f("target_inventory").unwrap_or(0.0);
        let max_inventory = f("max_inventory").unwrap_or(1.0);
        let skew = f("skew").unwrap_or(0.0);
        // Fill-rate breaker (audit F2): `with_fill_breaker`'s three knobs verbatim. Absent ⇒
        // `0`/`0.0`/`0` — the breaker's own OFF gate (`breaker_enabled` needs all three positive) —
        // byte-identical. The pull-accel pair above composes with it near a resolution.
        let fill_window_ms = ms("fill_window_ms").unwrap_or(0);
        let net_fill_threshold = f("net_fill_threshold").unwrap_or(0.0);
        let suppress_cooldown_ms = ms("suppress_cooldown_ms").unwrap_or(0);
        // Flow-toxicity guard (audit F1): the bag is built ONLY when a `toxicity_*` key is present,
        // so an absent pair keeps `toxicity = None` — byte-identical (an all-zero bag would also be
        // inert, but `None` keeps the observable `params()` surface identical too). Pairs with
        // `ofi_toxicity_scale` above so the internally-synthesized OFI reading (#798) can actually
        // fire from a registry table — no external `on_flow` feed exists in a backtest.
        let toxicity = (params.get("toxicity_widen").is_some()
            || params.get("toxicity_size_cut").is_some())
        .then(|| ToxicityParams {
            widen: f("toxicity_widen").unwrap_or(0.0),
            size_cut: f("toxicity_size_cut").unwrap_or(0.0),
        });
        let mut maker = SpreadMaker::new(qty, half_spread)
            .with_quote_style(style, depth_levels, tick_size)
            .with_skew(target_inventory, max_inventory, skew)
            .with_fill_breaker(fill_window_ms, net_fill_threshold, suppress_cooldown_ms)
            .with_refresh_tolerance(refresh_price_bps, refresh_size_bps);
        if let Some(tox) = toxicity {
            maker = maker.with_flow_toxicity(tox);
        }
        // Pricing selector (audit F8, the #803 inert-`style` finding): `"as"` (absent/default)
        // chains the A-S layer exactly as before; `"style"` SKIPS it so the selected QuoteStyle
        // placement finally prices in the registry path.
        if read_pricing_chains_as(params) {
            maker = maker.with_avellaneda_stoikov(as_params);
        }
        Ok(maker)
    }

    /// Builder: enable inventory-skew size shaping. Bias the bid/ask sizes to pull the position
    /// toward `target_inventory`, ramping over `max_inventory`, at intensity `skew` in `[0, 1]`.
    /// `with_skew(t, m, 0.0)` is equivalent to the fixed-size maker.
    pub fn with_skew(mut self, target_inventory: f64, max_inventory: f64, skew: f64) -> Self {
        self.cfg.target_inventory = target_inventory;
        self.cfg.max_inventory = max_inventory;
        self.cfg.skew = skew;
        self
    }

    /// Builder: enable the per-side fill-rate circuit breaker (audit mm1). Over a sliding
    /// `window_ms` EVENT-TIME window, net this-side-minus-other-side fill size; when it reaches
    /// `net_fill_threshold` on one side (net one-directional accumulation = adverse selection),
    /// suppress that side's quoting for `cooldown_ms`. `with_fill_breaker(_, 0.0, _)` or any
    /// non-positive `window_ms`/`cooldown_ms` leaves the breaker OFF (the maker is unchanged).
    pub fn with_fill_breaker(
        mut self,
        window_ms: i64,
        net_fill_threshold: f64,
        cooldown_ms: i64,
    ) -> Self {
        self.cfg.fill_window_ms = window_ms;
        self.cfg.net_fill_threshold = net_fill_threshold;
        self.cfg.suppress_cooldown_ms = cooldown_ms;
        self
    }

    /// Builder: choose the [`QuoteStyle`] (default [`QuoteStyle::Mid`], which reproduces the original
    /// midpoint pricing). `depth_levels` is how many levels into the book [`QuoteStyle::Depth`] rests
    /// (ignored by the other styles); `tick_size` is the venue grid [`QuoteStyle::Top`] steps by on
    /// the L1 quote lane (the L2 book lane uses the book's own tick size, so pass `0.0` there).
    /// `with_quote_style(QuoteStyle::Mid, _, _)` leaves the maker mid-quoting exactly as before.
    pub fn with_quote_style(
        mut self,
        style: QuoteStyle,
        depth_levels: usize,
        tick_size: f64,
    ) -> Self {
        self.cfg.style = style;
        self.cfg.depth_levels = depth_levels;
        self.cfg.tick_size = tick_size;
        self
    }

    /// Builder: enable own-order-book filtration — subtract this maker's own resting quotes from the
    /// public book before pricing, so it never joins or leans on its own order (the correctness fix
    /// for quoting on the same feed it consumes). OFF by default, so the default maker prices straight
    /// off the feed exactly as before. Matching needs a known tick grid: the L2 book carries one; for
    /// the L1 quote lane set `tick_size` via [`SpreadMaker::with_quote_style`].
    pub fn with_own_order_filtration(mut self) -> Self {
        self.cfg.filter_own = true;
        self
    }

    /// Builder: enable the order-refresh TOLERANCE (the anti-churn gate) — stop re-issuing a modify
    /// for a side whose target barely moved. Both thresholds are RELATIVE, in BASIS POINTS of what
    /// that side currently has RESTING (1 bp = 1e-4), so ONE tuning reads sanely on both a 0..1
    /// prediction market and a five-figure crypto book, with no tick grid needed:
    ///
    /// - `price_bps` — how far the target PRICE may drift from the resting price and still be left
    ///   alone (e.g. `25.0` = 25 bp = 0.25%).
    /// - `size_bps` — the same for the target SIZE. `0.0` means "no tolerance on this axis": any
    ///   size change at all still re-quotes (the conservative choice when the inventory skew is
    ///   shaping sizes).
    ///
    /// A side is skipped only when BOTH axes are inside tolerance. `with_refresh_tolerance(0.0,
    /// 0.0)` is INERT (never skips) — identical to leaving the gate off. OFF by default ⇒ the
    /// default maker re-quotes every tick exactly as before, byte-identical. The gate never touches
    /// the PULL path: a suppressed side is still canceled whatever the tolerance is.
    ///
    /// SIZING THE `size_bps` AXIS AGAINST THE SKEW: because `0.0` demands BIT-EXACT size equality,
    /// it only fires on a FIXED-size maker (`skew == 0.0`, i.e. no [`SpreadMaker::with_skew`]).
    /// With inventory skew active the sizes are recomputed as `qty · bid_mult`/`qty · ask_mult`
    /// from a CONTINUOUSLY varying position every tick, so the size axis would essentially never
    /// compare equal and the gate would never fire at all — a NON-ZERO `size_bps` is effectively
    /// REQUIRED there. Pick one that swallows the inventory jitter you don't want to chase while
    /// still re-quoting on a real re-size (and see [`RefreshTolerance`] for the matching ceiling on
    /// the price axis relative to `half_spread`).
    pub fn with_refresh_tolerance(mut self, price_bps: f64, size_bps: f64) -> Self {
        self.cfg.refresh_tolerance = Some(RefreshTolerance { price_bps, size_bps });
        self
    }

    /// Builder: enable LADDER quoting — rest `params.levels` rungs per side instead of one, stepping
    /// out from the same reservation price + half-spread the single quote uses (rung 0 IS today's
    /// quote; rung `k` sits `k · offset_step` further out at a [`LadderSizeProfile`](vike_model::LadderSizeProfile)-shaped
    /// size). The rungs carry tags `"bid0".."bidN"` / `"ask0".."askN"`, diff against the resting set
    /// each tick (minimal place/modify/cancel), honor the [`SpreadMaker::with_refresh_tolerance`] gate
    /// per rung, and — under the fill-rate breaker — a suppressed side pulls ALL its rungs (never
    /// strands one). COMPOSES with everything else: the style/A-S set rung 0's price, the skew shapes
    /// the base size every rung multiplies, own-order filtration (prefer
    /// [`SpreadMaker::with_own_order_book`], which tracks N orders) subtracts them. A `levels <= 1`
    /// bag is INERT — identical to leaving the ladder off — so `with_ladder(LadderParams { levels: 1,
    /// .. })` is today's single-quote maker, byte-identical. OFF by default. Rides the live-parameter
    /// plane, so `on_params_updated` can turn it on/off / re-shape it without a remount.
    pub fn with_ladder(mut self, params: LadderParams) -> Self {
        self.cfg.ladder = Some(params);
        self
    }

    /// Builder: enable own-order filtration backed by the full [`OwnOrderBook`] LADDER instead of
    /// the single `(price, size)` snapshot per side — and gate it on an **accepted-buffer** so a
    /// freshly-accepted order the public feed has not yet echoed is NOT double-subtracted from
    /// displayed depth. Implies [`SpreadMaker::with_own_order_filtration`] (it sets `filter_own`),
    /// so this one call is the whole opt-in. OFF by default ⇒ byte-identical to before.
    ///
    /// - `tick_size` — the venue grid own prices and public level prices are BOTH quantized on.
    ///   Pass the grid the maker actually quotes on; a non-positive value builds an INERT book that
    ///   subtracts nothing (the same no-grid rule `book::subtract_own` already follows). NOTE this
    ///   is the ladder's OWN fixed grid: unlike the snapshot path — which reads whatever
    ///   `tick_size` the live [`L2Book`] carries — it does not adopt the book's grid per tick, so on
    ///   the L2 lane pass that venue's grid here explicitly.
    /// - `accepted_buffer` — how long after an accept our size is assumed NOT yet visible in the
    ///   public feed, in the SAME unit as the maker's event clock (epoch-MILLIS on the live
    ///   quote/book lanes). `0` trusts the accept immediately, which reduces this path to the
    ///   pre-existing snapshot behavior.
    ///
    /// The maker drives the ladder itself, keyed by its `"bid"`/`"ask"` order tags with an
    /// OPTIMISTIC ack (a `Strategy` never sees client-order-ids or venue accepts) — so the buffer
    /// is measured from SEND. A runtime that knows the tag↔coid mapping can drive real venue
    /// accepts/cancels through [`SpreadMaker::own_book_mut`], after which the buffer covers
    /// market-data propagation alone. See `OwnBookFiltration::place` and the `own_book` module doc
    /// for the full contract.
    pub fn with_own_order_book(mut self, tick_size: f64, accepted_buffer: i64) -> Self {
        self.own_book = Some(OwnBookFiltration::new(tick_size, accepted_buffer));
        self.cfg.filter_own = true;
        self
    }

    /// Read the own-order ladder, or `None` when [`SpreadMaker::with_own_order_book`] was not used.
    pub fn own_book(&self) -> Option<&OwnBookFiltration> {
        self.own_book.as_ref()
    }

    /// MUTABLE access to the own-order ladder — the seam a mount/runtime uses to drive REAL venue
    /// lifecycle events ([`OwnOrderBook::on_submit`]/[`on_accepted`](OwnOrderBook::on_accepted)/
    /// [`on_partial_fill`](OwnOrderBook::on_partial_fill)/
    /// [`on_cancel_pending`](OwnOrderBook::on_cancel_pending)/
    /// [`on_terminal`](OwnOrderBook::on_terminal)) into the book, replacing the maker's optimistic
    /// stance with the venue's actual acks. `None` when the ladder is not enabled.
    pub fn own_book_mut(&mut self) -> Option<&mut OwnBookFiltration> {
        self.own_book.as_mut()
    }

    /// Builder: enable the Avellaneda–Stoikov PRICING layer (audit mm-quote). The two quote PRICES
    /// then come from an inventory-aware reservation price `r = s − q_norm·γ·V` and optimal
    /// half-spread `δ = ½·[γ·V + (2/γ)·ln(1+γ/κ)]`, adapted to Polymarket's 0–1 bounded prices by the
    /// bounded variance `V = min(σ̂²·H, p(1−p))` (all three modeling choices are [`AsParams`] knobs).
    /// Its `(bid, ask)` output flows UNCHANGED into the existing size-skew → fill-rate breaker →
    /// own-order filtration → modify-in-place tail, so A-S inherits the adverse-selection guard it
    /// classically lacks. Leaving this OFF (the default) keeps the maker byte-identical to before.
    /// `tick_size` (for the grid snap / wall clamp) is taken from the L2 book on the book lane, and
    /// from the configured `tick_size` (set via [`SpreadMaker::with_quote_style`]) on the L1 lane.
    pub fn with_avellaneda_stoikov(mut self, params: AsParams) -> Self {
        self.as_state = Some(AsState::new(params));
        self
    }

    /// Builder: override the pricing [`SpreadModel`] on an already-built A-S maker (e.g. the
    /// `gueant_maker` registry alias forces [`SpreadModel::Gueant`] so the same `spread_maker` knobs
    /// price with the GLFT closed form). No-op if the maker has no A-S state.
    pub fn with_spread_model(mut self, model: SpreadModel) -> Self {
        if let Some(st) = self.as_state.as_mut() {
            st.params.spread_model = model;
        }
        self
    }

    /// Builder: enable LIQUIDITY-REWARDS-aware quoting — fold a reward-EV term into the quote so it
    /// stays in a venue's reward band at `min_size` on both sides, `weight`-deep toward the mid,
    /// holding an in-band quote at least `min_order_age_ms` so it does not churn below the reward
    /// floor. See [`SpreadMakerParams::reward`] / [`RewardParams`]. `with_liquidity_rewards(RewardParams {
    /// weight: 0.0, .. })` is INERT (OFF), identical to not calling it. OFF by default ⇒ the maker is
    /// byte-identical to before. The A-S blackout / fill-rate breaker stay the safety authority.
    pub fn with_liquidity_rewards(mut self, params: RewardParams) -> Self {
        self.cfg.reward = Some(params);
        self
    }

    /// Builder: enable the FLOW-TOXICITY guard (RTDS wallet-toxicity, 5c) — react to the per-side
    /// toxic-flow reading routed in via [`Strategy::on_flow`] by WIDENING the toxic side away from mid
    /// (`tox · widen · half_spread`) and CUTTING its size (`× (1 − tox · size_cut).max(0)`), the
    /// size-to-zero case routing through the maker's existing per-side suppression PULL path.
    /// `with_flow_toxicity(ToxicityParams { widen: 0.0, size_cut: 0.0 })` is INERT (OFF), identical to
    /// not calling it. OFF by default ⇒ the maker is byte-identical to before, and it stays inert until
    /// a reading actually arrives. Rides the live-parameter plane like every other knob. See
    /// [`ToxicityParams`].
    pub fn with_flow_toxicity(mut self, params: ToxicityParams) -> Self {
        self.cfg.toxicity = Some(params);
        self
    }

    /// This side's [`SideState`] (`is_bid` selects [`SpreadMaker::bid`]/[`SpreadMaker::ask`]) —
    /// the read half of the per-side indexing every collapsed `is_bid`-parameterized helper uses.
    fn side(&self, is_bid: bool) -> &SideState {
        if is_bid { &self.bid } else { &self.ask }
    }

    /// Mutable twin of [`SpreadMaker::side`].
    fn side_mut(&mut self, is_bid: bool) -> &mut SideState {
        if is_bid { &mut self.bid } else { &mut self.ask }
    }

    /// The breaker engages only when ALL three knobs are positive; any non-positive value leaves
    /// it OFF, so the strategy reduces EXACTLY to the skew-only maker (no fill tracking, no
    /// suppression). This is the single gate every breaker code path checks.
    fn breaker_enabled(&self) -> bool {
        self.cfg.fill_window_ms > 0
            && self.cfg.net_fill_threshold > 0.0
            && self.cfg.suppress_cooldown_ms > 0
    }

    /// The time-accelerated fill-rate-breaker pull MULTIPLIER for the current event ts (Group-B, PR-3)
    /// — the factor [`Strategy::on_fill`] DIVIDES the effective net-fill threshold by so the breaker
    /// trips sooner as τ = (resolution_ts − event_ts) → 0 near a binary resolution. Exactly `1.0`
    /// (inert, so the threshold is unchanged bit-for-bit) unless the A-S layer is on with a known
    /// `resolution_ts` AND `pull_accel_ramp_ms > 0`; see [`settlement::accelerated_pull`], which
    /// guarantees the result is always `>= 1.0`. The `pull_accel_*` knobs live on [`AsParams`], so a
    /// breaker-only maker with no A-S layer reads `1.0` here and is byte-identical.
    fn pull_accel_mult(&self, event_ts: i64) -> f64 {
        let Some(params) = self.as_state.as_ref().map(|st| &st.params) else {
            return 1.0;
        };
        if params.pull_accel_ramp_ms <= 0 {
            return 1.0;
        }
        let Some(t_res) = params.resolution_ts else {
            return 1.0;
        };
        accelerated_pull(t_res - event_ts, params.pull_accel_ramp_ms, params.pull_accel_max)
    }

    /// Mirror a place/re-price of the tagged quote into the own-order ladder. While the ladder is
    /// `None` (the default) this is one `Option` test and a return — no state is touched, which is
    /// what keeps the requote tail byte-identical. See [`OwnBookFiltration::place`] for the
    /// optimistic-ack semantics.
    fn own_book_place(&mut self, tag: &str, side: OwnSide, price: f64, qty: f64, ts: i64) {
        if let Some(own) = self.own_book.as_mut() {
            own.place(tag, side, price, qty, ts);
        }
    }

    /// Mirror a PULL (suppression cancel) of the tagged quote into the own-order ladder. No-op
    /// while the ladder is `None`.
    fn own_book_pull(&mut self, tag: &str) {
        if let Some(own) = self.own_book.as_mut() {
            own.pull(tag);
        }
    }

    /// Feed one own-order OUTCOME into the A-S κ MLE's own-fill tape, for
    /// [`KappaMode::OwnFillFit`](vike_model::KappaMode). A GUARDED no-op unless the A-S layer is on
    /// AND its `kappa_mode` is `OwnFillFit` — so every other maker (default / `Fixed` / `LiveFit`) is
    /// byte-identical, this whole path being inert.
    ///
    /// `is_bid` picks which resting snapshot + placement clock the `(δ, exposure)` is read from;
    /// `filled` marks a fill vs a censored (breaker-pulled) exposure. δ is the resting quote's
    /// distance to the A-S fair mid it was last priced against (`last_quote_mid`); exposure is the
    /// event-time it rested since its last place/re-price (`*_quoted_ts`). Skipped when that side has
    /// no tracked resting order, no placement clock, or no fair mid yet.
    fn feed_own_outcome(&mut self, is_bid: bool, filled: bool, ts: i64) {
        let side = self.side(is_bid);
        let (resting, placed_ts) = (side.own, side.quoted_ts);
        let Some((rpx, _)) = resting else { return };
        if placed_ts <= 0 {
            return;
        }
        let Some(as_state) = self.as_state.as_mut() else { return };
        if as_state.params.kappa_mode != KappaMode::OwnFillFit {
            return;
        }
        let Some(mid) = as_state.last_quote_mid else { return };
        let delta = (rpx - mid).abs();
        let exposure = (ts - placed_ts).max(0) as f64;
        as_state.record_own_outcome(delta, exposure, filled, ts);
    }

    /// Snapshot the maker's current live tunables as a [`SpreadMakerParams`] — the READ side of the
    /// live-parameter plane (e.g. for a GUI to seed its tuning editor). This is exactly the bag
    /// [`Strategy::on_params_updated`] consumes.
    pub fn params(&self) -> SpreadMakerParams {
        SpreadMakerParams {
            // The A-S bag's authority is the live `as_state.params` (see `cfg`'s doc — the copy in
            // `cfg` is pinned `None`), re-attached here on the way out.
            avellaneda_stoikov: self.as_state.as_ref().map(|st| st.params),
            ..self.cfg
        }
    }

    /// Hot-swap ALL live tunables ATOMICALLY from `p` and bump [`SpreadMaker::params_epoch`]. Only
    /// the CONFIG knobs change — the per-side [`SideState`]s (resting orders/rungs, staleness,
    /// moas clocks, suppression deadlines) and the fill window are DELIBERATELY left untouched, so resting orders +
    /// queue position survive a re-tune: the next `requote` re-prices them IN PLACE via modify (never
    /// cancel/replace, and a no-move re-price doesn't disturb the book at all). Runs on the single
    /// core thread between ticks, so the whole swap is atomic w.r.t. tick dispatch.
    fn apply_params(&mut self, p: &SpreadMakerParams) {
        // ONE struct copy swaps every plain knob (audit F10) — and by construction it can ONLY
        // touch config: all runtime state (the per-side [`SideState`]s — resting orders, rungs,
        // moas clocks, suppression deadlines — plus the fill window, `last_flow`, `own_book`,
        // `underlying`, `learned_book_tick`) lives OUTSIDE `cfg`, so the swap leaves it alone by
        // type-shape, not by hand-copied omission. Consequences, unchanged from the field-by-field
        // era: a re-tune keeps resting orders + queue position (the next tick re-prices in place);
        // resting ladder RUNGS are reconciled to the new shape on the next `requote` (re-price
        // kept rungs, submit new ones, cancel beyond the new count — turning the ladder off
        // retires them on the next tick, never here); reward re-shapes take effect next tick with
        // the accrued moas age kept; a toxicity re-tune keeps whatever `on_flow` last delivered.
        // No order op ever fires on a bare re-tune. The A-S sub-bag is STRIPPED (pinned `None`,
        // see `cfg`'s doc) — it is applied to the live `as_state` below instead.
        self.cfg = SpreadMakerParams { avellaneda_stoikov: None, ..*p };
        // A-S plane: a re-tune PRESERVES the warm σ̂²/κ estimators (like the breaker window / resting
        // orders) — only the params swap. `Some` on a live maker updates in place; `Some` on an
        // A-S-off maker turns it on; `None` turns A-S off.
        match p.avellaneda_stoikov {
            Some(ap) => match self.as_state.as_mut() {
                Some(st) => st.set_params(ap),
                None => self.as_state = Some(AsState::new(ap)),
            },
            None => self.as_state = None,
        }
        self.params_epoch += 1;
    }
}

#[cfg(test)]
mod tests;
