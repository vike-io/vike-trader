//! `RunProfile` — the single TOML-deserializable config artifact that names a full runtime
//! assembly ({event source, broker/execution target, sinks, validator/risk stack}) so that
//! backtest / paper / live become ONE auditable file instead of code-only [`crate::CoreConfig`]
//! construction by each binary/test.
//!
//! ## `ProfileRisk` / `GridSource` / `ProfileError` now live in `vike-exec` (runprofile-wiring-step2)
//!
//! These three types were HOISTED down to [`vike_exec::risk_profile`], beside [`vike_exec::RiskLimits`]
//! itself, so `vike-backtest` — which sits ALONGSIDE this crate, not beneath it, and so cannot
//! depend on it — can populate `EngineParams.risk_limits` from the SAME TOML `[risk]` converter
//! paper and live use, rather than growing a second one that drifts. Every name below is
//! re-exported verbatim (`pub use vike_exec::{GridSource, ProfileError, ProfileRisk};`), so every
//! existing `vike_core::{ProfileRisk, GridSource, ProfileError}` caller keeps working unchanged —
//! the same hoist shape [`vike_model::sizing`] established for
//! `units_from_percent`/`units_from_value`. See [`vike_exec::risk_profile`]'s module doc for the
//! "two owners, one struct" rule and the compile-checked `to_risk_limits`/`apply_to` drift alarm;
//! what stays HERE is `RunProfile` itself (the mode/event-source/broker/sinks/guards schema) and
//! the `mode`-derived [`RunProfile::grid_source`] / [`RunProfile::apply_risk`] glue.
//!
//! ## Scope (audit co14) — SCHEMA + LOADER ONLY, deliberately additive
//!
//! This module is NOT yet wired into any binary and does NOT change [`crate::CoreConfig`]; that
//! is an explicit follow-up. What ships here:
//!   - the [`RunProfile`] type tree (serde `Deserialize`, TOML),
//!   - a loader ([`RunProfile::from_toml_str`] / [`RunProfile::from_path`]) with semantic
//!     [`RunProfile::validate`]ation returning a clear [`ProfileError`],
//!   - [`resolve_profile`] — the shared `--profile`/`VIKE_RUN_PROFILE` resolver a binary's wiring
//!     step calls: an INJECTED `vars` map (never `std::env::var` itself, so it stays the
//!     `Layer::Injected` seam `vike-app-core`'s settings registry gate wants), explicit-beats-env
//!     precedence, and a resolved-but-broken path is always an `Err` (never a silent `Ok(None)`),
//!   - three sample profiles ([`samples`]).
//!
//! ## How it maps to the real runtime knobs
//!
//! The `[risk]`, `[guards]`, and `[sinks.journal]` sections mirror fields that ALREADY exist on
//! [`crate::CoreConfig`] / [`vike_exec::RiskLimits`] / [`vike_exec::MarginCallConfig`]. The
//! converters ([`ProfileRisk::to_risk_limits`], [`ProfileMarginCall::to_margin_call_config`],
//! [`Sinks::journal_config`], [`Guards::submit_ack_timeout`], …) return those REAL types, so the
//! mapping is compile-checked against them — adding a required field to `RiskLimits` breaks the
//! build here, which is the intended drift alarm. The mapping, field-by-field:
//!
//! - `broker.seed_cash` → [`crate::CoreConfig::seed_cash`].
//! - `[risk]` → [`vike_exec::RiskLimits`] (the pre-trade gate config).
//! - `guards.initial_trading_state` → [`vike_exec::TradingState`] (`halted` = the kill-switch HALT path — the gate then denies every new order).
//! - `guards.submit_ack_timeout_ms` → [`crate::CoreConfig::submit_ack_timeout`] (stuck-order watchdog stage 1).
//! - `guards.submit_ack_confirm_grace_ms` → [`crate::CoreConfig::submit_ack_confirm_grace`] (stage 2).
//! - `guards.max_drawdown` → [`crate::CoreConfig::max_drawdown`] (equity-drawdown liquidate-only latch).
//! - `guards.margin_call` → [`crate::CoreConfig::margin_call`] ([`vike_exec::MarginCallConfig`]).
//! - `guards.conditionals_on_ticks` → [`crate::CoreConfig::conditionals_on_ticks`].
//! - `[sinks.journal]` → [`crate::JournalConfig`] (write-ahead command journal).
//! - `guards.freshness_ms` → the per-subscription DATA-freshness threshold in the live feed loops (vike-data `FreshnessTracker` / `StreamStatus::Stale`). NOTE: this is NOT a `CoreConfig` field today — it is configured on the subscription, not the core — so the profile carries it as an opaque `ms` knob for the future wiring step. (The task's "reseed_interval" guard is this data-freshness window; there is no `reseed_interval` on `CoreConfig`.)
//! - `sinks.equity_sample_ms` → schema-only today (no `CoreConfig` knob yet); the future
//!   sink-enablement PR wires it to the portfolio-equity sampler cadence.
//!
//! ## Unknown-field policy: DENY
//!
//! Every struct is `#[serde(deny_unknown_fields)]`. A config artifact is exactly where a typo'd
//! key must fail LOUDLY (a silently-ignored `max_levarage` is a live-money footgun), so unknown
//! keys are a hard parse error, not a silent drop.
//!
//! ## Two owners, one struct: the venue's instrument grid vs the operator's risk budget
//!
//! [`vike_exec::RiskLimits`] holds two conceptually separate concerns in one struct:
//! `tick_size`/`lot_size`/`min_qty`/`min_notional` (the **venue**'s instrument grid, populated by
//! `RiskLimits::from_properties` from a real fetch at mount) and everything else —
//! `max_notional_per_order`/`max_total_exposure`/`max_orders_per_window`/`window_ms`/
//! `max_leverage`/`im_requirement`/`im_by_symbol`/`required_free_bp_pct`/
//! `block_reduce_only_overshoot` (the **operator**'s risk budget, which a [`RunProfile`]'s
//! `[risk]` section sets). **THE RULE: the venue owns the instrument grid; the operator owns the
//! risk budget. Neither overwrites the other.**
//!
//! This is not a vike invention — it mirrors how two other engines keep the same two concerns
//! apart:
//!   - **NautilusTrader's `RiskEngine`** checks instrument-level limits (`min_quantity`/
//!     `max_quantity`/`max_notional` off the instrument) AND engine-level limits
//!     (`max_notional_per_order` from `RiskEngineConfig`) as SEPARATE checks in one pass — an
//!     order must clear both, and neither overwrites the other; `set_max_notional_per_order` is a
//!     runtime override on the config side only.
//!   - **LEAN** keeps three distinct layers: `SymbolProperties` (`LotSize`/`MinimumOrderSize` —
//!     rounds and rejects), `BrokerageModel.CanSubmitOrder` (the venue veto), and
//!     `RiskManagementModel.ManageRisk` (operator policy over `PortfolioTarget`s). Separation of
//!     concerns between the instrument grid and operator policy is a stated design principle
//!     there, not an implementation accident.
//!
//! We only face a "who wins" question at all because `vike_exec::RiskLimits` is ONE struct
//! holding both concerns — a vike quirk, not a law. [`ProfileRisk::apply_to`] is the enforcement
//! point: a profile that sets a venue-owned field while a real grid was fetched is a **config
//! error at load** (`Err` naming the offending key) — never a silent clamp, because silently
//! clamping is exactly how an operator sets a limit that never takes effect and never learns.
//! The one exception — a profile MAY supply the instrument fields when no grid was fetched at all
//! (the `make_engine` permissive-default fallback after a fetch failure) — is gated on the caller
//! passing [`GridSource::NoGridFetched`], never inferred from a field happening to be `None`.
//!
//! ## `GridSource` is derived from `mode`, not a second flag
//!
//! An earlier revision of this module added `ProfileRisk::allow_instrument_override` — a
//! load-time opt-in flag INDEPENDENT of `mode`. That was wrong: `mode` already answers "may this
//! profile supply the instrument grid?" — `backtest`/`paper` never fetch a real grid, so the
//! profile is the only source; `live` always fetches one, so the profile must never contend with
//! it. A second knob just made an invalid state expressible (a `live` profile that opts into
//! overriding the venue's real grid). The flag was deleted. [`RunProfile::grid_source`] derives
//! the [`GridSource`] straight from `self.mode` — [`Mode::Backtest`]/[`Mode::Paper`] ⇒
//! [`GridSource::NoGridFetched`], [`Mode::Live`] ⇒ [`GridSource::VenueFetched`] — and
//! [`RunProfile::apply_risk`] is the ONE path a caller holding a full `RunProfile` should use to
//! reach [`ProfileRisk::apply_to`]: it always passes the mode-derived source, so a caller with a
//! `RunProfile` in hand cannot construct a `GridSource` that contradicts that profile's own
//! `mode`. [`RunProfile::validate`] additionally rejects, unconditionally, a `mode = "live"`
//! profile whose `[risk]` table sets ANY venue-owned instrument field — no escape hatch, because a
//! live profile setting the grid is never sound, not even at load time before a mount is attempted.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Top-level run mode. Selects the intended assembly; [`RunProfile::validate`] enforces that the
/// `event_source` + `broker` actually match it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// historical/replay data + paper broker (deterministic, offline)
    Backtest,
    /// live (or replayed) data + paper broker — paper-trade a real feed
    Paper,
    /// live data + real venue broker (credential-gated demo/mainnet)
    Live,
}

/// Where events come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSourceKind {
    /// the vike-data `HistStore` (bars/ticks) — offline
    Hist,
    /// recorded tick/quote/trade/book capture replayed through `run_ticks`
    Replay,
    /// a live venue feed (crypto `DataClient` / Polymarket market feed)
    LiveVenue,
}

/// What executes orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerKind {
    /// the R7 paper exchange (`vike_backtest::paper`) — no real orders
    Paper,
    /// a real venue adapter (`ExecutionClient` bridge crate) — real orders
    Venue,
}

/// Initial pre-trade gate state. Mirrors [`vike_exec::TradingState`]; `halted` is the HALT
/// kill-switch (the gate denies every new order until a manual state change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProfileTradingState {
    #[default]
    Active,
    /// only position-reducing orders allowed
    Reducing,
    /// no new orders (kill switch)
    Halted,
}

impl ProfileTradingState {
    /// Map to the real [`vike_exec::TradingState`].
    pub fn to_trading_state(self) -> vike_exec::TradingState {
        match self {
            ProfileTradingState::Active => vike_exec::TradingState::Active,
            ProfileTradingState::Reducing => vike_exec::TradingState::Reducing,
            ProfileTradingState::Halted => vike_exec::TradingState::Halted,
        }
    }
}

/// `[event_source]` — the feed. `venue`/`symbol` are always required; the rest are kind-specific
/// (opaque strings the loader validates only for presence, not format).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSource {
    pub kind: EventSourceKind,
    /// venue id (must match a bridge/data crate, e.g. `binance`, `polymarket`)
    pub venue: String,
    /// instrument symbol (e.g. `BTCUSDT`)
    pub symbol: String,
    /// bar interval (e.g. `1m`); optional for pure tick replay
    pub interval: Option<String>,
    /// hist/replay window start (RFC3339 / date — opaque here)
    pub start: Option<String>,
    /// hist/replay window end (opaque here)
    pub end: Option<String>,
    /// replay: path to the recorded capture / hist root (opaque here)
    pub path: Option<String>,
}

/// `[broker]` — the execution target.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Broker {
    pub kind: BrokerKind,
    /// venue id for a `venue` broker (required then). Ignored for a `paper` broker.
    pub venue: Option<String>,
    /// paper-broker starting cash → [`crate::CoreConfig::seed_cash`]. A `venue` broker trades a
    /// real account, so this is ignored there.
    #[serde(default)]
    pub seed_cash: f64,
}

/// `[sinks.journal]` — the opt-in write-ahead command journal. Maps 1:1 to [`crate::JournalConfig`]
/// (`dir` + [`crate::journal::JournalFileConfig`] + `snapshot_every`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileJournal {
    /// directory the segment files live in
    pub dir: String,
    /// pre-allocated segment size (bytes)
    #[serde(default = "default_segment_bytes")]
    pub segment_bytes: u64,
    /// flush cadence (records)
    #[serde(default = "default_flush_every")]
    pub flush_every: u32,
    /// write a full-state `Snap` every N appended records (MUST be >= 1)
    #[serde(default = "default_snapshot_every")]
    pub snapshot_every: u64,
}

fn default_segment_bytes() -> u64 {
    64 * 1024 * 1024
}
fn default_flush_every() -> u32 {
    256
}
fn default_snapshot_every() -> u64 {
    1024
}

impl ProfileJournal {
    /// Build the real [`crate::JournalConfig`] (compile-checked mapping).
    pub fn to_journal_config(&self) -> crate::JournalConfig {
        crate::JournalConfig {
            dir: std::path::PathBuf::from(&self.dir),
            file: crate::journal::JournalFileConfig {
                segment_bytes: self.segment_bytes,
                flush_every: self.flush_every,
            },
            snapshot_every: self.snapshot_every,
        }
    }
}

/// `[sinks]` — output toggles. All default off/absent, so an omitted `[sinks]` = a headless run
/// with no recording and no GUI observer.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Sinks {
    /// publish `CoreSnapshot`s for a GUI observer (arc-swap). Off for headless runs.
    #[serde(default)]
    pub gui: bool,
    /// `RecorderSink` → vike-data `HistStore` (records live quotes/trades/book). Only meaningful
    /// with a `live_venue` event source (it records a LIVE feed).
    #[serde(default)]
    pub recorder: bool,
    /// Polymarket raw-frame tap capture directory (gzip). `None` = disabled.
    pub raw_capture_dir: Option<String>,
    /// write-ahead command journal (durability sink). `None` = off.
    pub journal: Option<ProfileJournal>,
    /// portfolio-equity sampler interval (ms). `None` = disabled. → [`crate::CoreConfig::equity_sample`],
    /// applied by [`RunProfile::apply_guards_and_sinks`].
    ///
    /// ⚠ This doc used to say "Schema-only today — no `CoreConfig` knob consumes this yet (future
    /// sink-enablement PR)". `CoreConfig::equity_sample` had in fact existed for weeks and was
    /// already being SET, hardcoded to one second, in the GUI shell's `main.rs`
    /// (`crates/vike-desktop/src/main.rs`, then spelled `vike-app`): the target landed and the key
    /// was never pointed at it, while the doc kept saying the target did not exist. That is the
    /// exact failure this workspace deleted `Policy::max_total_exposure` for.
    ///
    /// ⚠ **That hardcoded setter is GONE, and the citation above is EVIDENCE rather than a pointer
    /// at live code.** The desktop cut took the local trading core out of the GUI — all orders and
    /// trading go through the backend now — so that binary builds no `CoreConfig` at all, and no
    /// binary in this workspace hardcodes the knob any more. This key, folded in by
    /// [`RunProfile::apply_guards_and_sinks`], is what sets it: the state the paragraph above was
    /// asking for.
    pub equity_sample_ms: Option<u64>,
}

impl Sinks {
    /// The mapped [`crate::JournalConfig`], if the journal sink is enabled.
    pub fn journal_config(&self) -> Option<crate::JournalConfig> {
        self.journal.as_ref().map(ProfileJournal::to_journal_config)
    }

    /// The equity-sampler interval as a [`Duration`], if enabled.
    pub fn equity_sample(&self) -> Option<Duration> {
        self.equity_sample_ms.map(Duration::from_millis)
    }
}

// `ProfileRisk` / `GridSource` were HOISTED to `vike_exec::risk_profile` (runprofile-wiring-step2)
// so `vike-backtest` — which sits ALONGSIDE this crate, not beneath it — can share the SAME
// TOML-`[risk]`→`RiskLimits` converter paper and live already use, instead of growing a second one
// that drifts. Re-exported below so every existing caller of `vike_core::{ProfileRisk,
// GridSource}` keeps working unchanged (the same hoist shape `vike_model::sizing` established for
// `units_from_percent`/`units_from_value`). See `vike_exec::risk_profile`'s module doc for the
// "two owners, one struct" rule and the compile-checked `to_risk_limits`/`apply_to` drift alarm.
pub use vike_exec::{GridSource, ProfileRisk};

/// `[guards.margin_call]` — mirror of [`vike_exec::MarginCallConfig`] (which has no serde derives).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileMarginCall {
    /// maintenance-margin fraction (LEAN `1/leverage`), e.g. `0.05`
    pub mm_requirement: f64,
    /// warn when margin remaining ≤ equity · this (LEAN default 0.05)
    #[serde(default = "default_warn_fraction")]
    pub warn_fraction: f64,
    /// liquidate only when margin used > equity · (1 + buffer) (LEAN default 0.10)
    #[serde(default = "default_buffer")]
    pub buffer: f64,
}

fn default_warn_fraction() -> f64 {
    0.05
}
fn default_buffer() -> f64 {
    0.10
}

impl ProfileMarginCall {
    /// Build the real [`vike_exec::MarginCallConfig`] (compile-checked mapping).
    pub fn to_margin_call_config(&self) -> vike_exec::MarginCallConfig {
        vike_exec::MarginCallConfig {
            mm_requirement: self.mm_requirement,
            warn_fraction: self.warn_fraction,
            buffer: self.buffer,
        }
    }
}

/// `[guards]` — the opt-in safety guards layered over the base [`ProfileRisk`] gate. Each maps to
/// an existing [`crate::CoreConfig`] knob (except `freshness_ms`; see the module docs).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Guards {
    /// initial gate state → [`vike_exec::TradingState`]. `halted` = the HALT kill-switch.
    #[serde(default)]
    pub initial_trading_state: ProfileTradingState,
    /// stuck-order watchdog stage 1 → [`crate::CoreConfig::submit_ack_timeout`]. `None` = disabled.
    pub submit_ack_timeout_ms: Option<u64>,
    /// watchdog stage 2 → [`crate::CoreConfig::submit_ack_confirm_grace`]. `None` = **the caller's
    /// value is left exactly as it was** — this struct carries NO default of its own, so the
    /// effective grace is whatever the binary already built (a bare `CoreConfig::default`'s,
    /// unless that binary set its own).
    ///
    /// ⚠ This doc used to say "`None` = the CoreConfig default (5000 ms)", and both halves had
    /// rotted: `CoreConfig::default`'s grace was bumped to 15 s by the confirm-race hardening, and
    /// [`RunProfile::apply_guards_and_sinks`] was WRITING this struct's own stale 5 s over it
    /// whenever `submit_ack_timeout_ms` was named. That silently halved the confirm window on a
    /// live mount — see that function for the incident and the rule that replaced it.
    ///
    /// ⚠ Not to be confused with `CoreConfig::submit_ack_confirm_grace`'s own "Ignored when
    /// `submit_ack_timeout` is `None`", which is a statement about the RUNTIME (no stage-1
    /// timeout ⇒ no watchdog ⇒ nothing consults the grace). That is still true, and it is NOT a
    /// reason to skip APPLYING this key: the two live roots arm `submit_ack_timeout` in their own
    /// `CoreConfig` literals, so a profile that names only this key is naming the grace of a
    /// watchdog that IS armed.
    pub submit_ack_confirm_grace_ms: Option<u64>,
    /// equity-drawdown liquidate-only latch → [`crate::CoreConfig::max_drawdown`]. `None` = disabled.
    pub max_drawdown: Option<f64>,
    /// intra-bar stop/trailing check → [`crate::CoreConfig::conditionals_on_ticks`].
    #[serde(default)]
    pub conditionals_on_ticks: bool,
    /// data-freshness staleness window (ms). Maps to the per-subscription `FreshnessTracker`
    /// threshold in the live feed loops (NOT a `CoreConfig` field today — see module docs).
    pub freshness_ms: Option<u64>,
    /// opt-in margin-call watchdog → [`crate::CoreConfig::margin_call`]. `None` = disabled.
    pub margin_call: Option<ProfileMarginCall>,
}

impl Guards {
    // ⚠ A `const DEFAULT_CONFIRM_GRACE_MS: u64 = 5000` stood here and is DELETED, not corrected.
    // It was a SECOND authority for a default `crate::CoreConfig::default` already owns, and it
    // did what a second authority does: `CoreConfig`'s moved to 15 s with the confirm-race
    // hardening, this copy did not, and the converter below silently wrote the stale copy over the
    // live one on every profile that armed stage 1. Re-pointing it at `CoreConfig::default()`
    // would have fixed today's number and left the SHAPE — a profile-side default for a key the
    // operator did not set — free to rot again the next time either value moves. There is no
    // profile-side default now: `None` means the caller's value stands, and `CoreConfig::default`
    // is the one place the number is written.

    /// → [`crate::CoreConfig::submit_ack_timeout`].
    pub fn submit_ack_timeout(&self) -> Option<Duration> {
        self.submit_ack_timeout_ms.map(Duration::from_millis)
    }

    /// → [`crate::CoreConfig::submit_ack_confirm_grace`], or `None` when the profile named no
    /// grace — in which case there is nothing to apply and the caller's own value stands. The
    /// exact shape of [`Guards::submit_ack_timeout`] above, deliberately: silence is silence.
    pub fn submit_ack_confirm_grace(&self) -> Option<Duration> {
        self.submit_ack_confirm_grace_ms.map(Duration::from_millis)
    }

    /// → the data-freshness threshold on the live subscription (see module docs).
    pub fn freshness(&self) -> Option<Duration> {
        self.freshness_ms.map(Duration::from_millis)
    }

    /// → [`crate::CoreConfig::margin_call`].
    pub fn margin_call_config(&self) -> Option<vike_exec::MarginCallConfig> {
        self.margin_call.as_ref().map(ProfileMarginCall::to_margin_call_config)
    }

    /// → the initial [`vike_exec::TradingState`].
    pub fn trading_state(&self) -> vike_exec::TradingState {
        self.initial_trading_state.to_trading_state()
    }
}

/// The whole run profile — one auditable config artifact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunProfile {
    /// human-readable label (for logs / audit); optional
    pub name: Option<String>,
    pub mode: Mode,
    pub event_source: EventSource,
    pub broker: Broker,
    #[serde(default)]
    pub sinks: Sinks,
    #[serde(default)]
    pub risk: ProfileRisk,
    #[serde(default)]
    pub guards: Guards,
}

// `ProfileError` was HOISTED to `vike_exec::risk_profile` together with `ProfileRisk` (whose
// `apply_to` returns it) — re-exported so every existing `vike_core::ProfileError` caller (this
// module's own loaders included) keeps working unchanged.
pub use vike_exec::ProfileError;

/// finite and strictly positive — for size/notional/exposure knobs where 0 or negative is nonsense.
fn is_pos(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// finite and in `(0.0, 1.0]` — the LEAN fraction shape (im/mm/warn/drawdown).
fn is_frac_unit(v: f64) -> bool {
    v > 0.0 && (0.0..=1.0).contains(&v)
}

/// The adapter's per-re-query REST timeout, in seconds
/// (`crates/vike-bridge-core/src/http.rs`'s `blocking_agent_with_timeout`). Restated here rather
/// than imported because `vike-core` sits BELOW `vike-bridge-core` and cannot name that constant in
/// code; the citation is the link, and
/// `the_warning_predicate_agrees_with_the_bound_the_shipped_pairing_is_gated_on` holds this pair
/// equal to the arithmetic the pin test spells for itself.
const CONFIRM_REQUERY_SECS: u64 = 5;

/// Sequential re-queries the WORST-CASE confirm makes: `N = 2` for Bybit (realtime → history),
/// `N = 1` for OKX. The worst case is the right one here, deliberately: this warning is computed
/// once at startup, before any venue is mounted, and cannot know which venues an operator will arm.
const CONFIRM_REQUERY_HOPS: u64 = 2;

/// The right-hand side of HARD LOWER BOUND (a): `submit_ack_timeout/2 + N·requery`. The first term
/// is the watchdog's tick jitter (the timer fires every `submit_ack_timeout/2`, so the active
/// confirm can be issued up to one tick late), the second the confirm's own worst-case round trip.
fn confirm_budget(timeout: Duration) -> Duration {
    timeout / 2 + Duration::from_secs(CONFIRM_REQUERY_SECS * CONFIRM_REQUERY_HOPS)
}

/// HARD LOWER BOUND (a) on [`crate::CoreConfig::submit_ack_confirm_grace`] —
/// `2·grace > submit_ack_timeout/2 + N·requery` — evaluated on ONE pair. `true` = safe.
///
/// The bound is stated in prose on that field and argued again at the live root's
/// `submit_ack_timeout` literal (`crates/vike-tradehub/src/tradehub_cli.rs`); this is the one place
/// it is COMPUTED, so a run profile can no longer invalidate that argument in silence.
///
/// ⚠ This said "both live roots" and named the GUI shell as the second. There is ONE live root now:
/// the desktop cut took the local trading core out of `crates/vike-desktop/src/main.rs` (then
/// spelled `vike-app`), which builds no `CoreConfig` and arms no watchdog. The bound is unchanged —
/// it was never a property of how many roots stated it.
fn confirm_grace_clears_bound(timeout: Duration, grace: Duration) -> bool {
    grace * 2 > confirm_budget(timeout)
}

/// A confirm-grace pairing that BREAKS HARD LOWER BOUND (a), reported by
/// [`RunProfile::apply_guards_and_sinks`] so its caller can say so out loud.
///
/// ⚠ **The residual this closes, and why it could only be closed here.** #1569 stopped
/// `apply_guards_and_sinks` from rewriting the grace to a stale 5 s whenever `submit_ack_timeout_ms`
/// was named, and declared what it deliberately left: *a profile that RAISES
/// `submit_ack_timeout_ms` without raising the grace still breaks bound (a), silently. Nothing
/// validates the pair.* [`RunProfile::validate`] cannot: it sees only the profile, so it could check
/// nothing but the case where BOTH keys are named — the case least in need of help, since an
/// operator who wrote both numbers was at least looking at both. `apply_guards_and_sinks` takes the
/// caller's [`crate::CoreConfig`], which is where the OTHER half of every pairing lives (both live
/// roots arm `submit_ack_timeout: Some(30s)` in their own literals and leave the grace at
/// `CoreConfig::default`'s), so it is the first and only place both numbers are in one hand.
///
/// ⚠ **It WARNS; it does not refuse.** `docs/decisions/0013-degrade-vs-refuse.md` decides this, and
/// its four questions are worth answering explicitly rather than by analogy:
///
/// 1. *Protection or capability?* Neither, exactly — nothing here is unarmed. The ladder IS armed;
///    it is TUNED to a value that narrows its safety margin. A refusal is the answer to a protection
///    an operator believes is on and is not, and that is not this.
/// 2. *Did the operator ask for it?* They asked for a timeout and got the timeout — there is no
///    set-but-unhonoured key, and so no false belief for a refusal to correct.
/// 3. *Does it redirect authority?* No. The failure mode is a LOCAL synthesized `OrderRejected`
///    against an order the venue may in fact hold; the watchdog calls no venue, so the damage is
///    local-state divergence that reconcile repairs, not a venue write.
/// 4. *Is it visible where the operator already looks?* Yes — the same startup line, from the same
///    call, as the still-unwired disclosure both roots already print.
///
/// And the decisive one, which is not on that list: `N` is the WORST-CASE venue's hop count, so the
/// bound is deliberately conservative and a mount that will only ever touch OKX (`N = 1`) can clear
/// the real bound while failing this one. A refusal computed from a conservative heuristic would
/// refuse correct configurations — and would refuse them at the startup of a daemon that was running
/// yesterday, which `docs/decisions/0013-degrade-vs-refuse.md`'s "What would reopen this" names as
/// the case where refusing is worse than the misconfiguration it prevents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmGraceHazard {
    /// Stage 1 as the mount ENDED UP with it — the binary's literal, or the profile's
    /// `guards.submit_ack_timeout_ms` where it named one.
    pub submit_ack_timeout: Duration,
    /// Stage 2, likewise: `CoreConfig::default`'s, or the profile's
    /// `guards.submit_ack_confirm_grace_ms` where it named one.
    pub confirm_grace: Duration,
    /// `submit_ack_timeout/2 + N·requery` — the quantity `2·grace` had to exceed and did not.
    pub confirm_budget: Duration,
}

impl ConfirmGraceHazard {
    /// The smallest whole-millisecond `guards.submit_ack_confirm_grace_ms` that CLEARS the bound at
    /// this timeout — the paste-ready number the warning hands the operator.
    ///
    /// `⌊budget/2⌋` in milliseconds, plus one: `Duration::as_millis` truncates, so `m + 1` ms is
    /// strictly greater than `budget/2` whatever sub-millisecond remainder it dropped, and
    /// `2·grace > budget ⟺ grace > ⌊budget/2⌋` over the integer nanoseconds a `Duration` is.
    /// `the_advised_minimum_grace_actually_clears_the_bound` drives that claim through the real
    /// predicate rather than restating it.
    pub fn min_grace_ms(&self) -> u128 {
        (self.confirm_budget / 2).as_millis() + 1
    }
}

impl std::fmt::Display for ConfirmGraceHazard {
    /// The operator-facing sentence, owned HERE so the two live roots cannot drift about the
    /// arithmetic the way the prose copies of this bound already have once.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "run profile: the stuck-order watchdog's confirm-grace pairing breaks HARD LOWER \
             BOUND (a) — submit_ack_timeout={:?} leaves submit_ack_confirm_grace={:?}, but \
             2·grace ({:?}) must EXCEED submit_ack_timeout/2 + N·requery ({:?}). The last-resort \
             synthesized OrderRejected can fire while the adapter's own re-query is still in \
             flight: a PHANTOM REJECT of an order the venue may actually hold. Set \
             guards.submit_ack_confirm_grace_ms = {} or higher (or lower \
             guards.submit_ack_timeout_ms). Starting anyway — this is a tuning warning, not a \
             refusal; see `vike_core::ConfirmGraceHazard` for why it is not one.",
            self.submit_ack_timeout,
            self.confirm_grace,
            self.confirm_grace * 2,
            self.confirm_budget,
            self.min_grace_ms(),
        )
    }
}

/// What [`RunProfile::apply_guards_and_sinks`] found, for a caller to disclose.
///
/// It RETURNS rather than logs, which is the shape that function already had and the shape this
/// crate owes its callers: `vike-core` is a library, the two live roots word their own disclosure
/// differently on purpose (`in this mount` / `in this daemon`), and a returned value is what lets a
/// test assert the ABSENCE of a warning — which is half of what this report is gated on, and which a
/// `tracing` assertion cannot do reliably (the subscriber's `Interest` cache is process-global).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardsReport {
    /// `[guards]`/`[sinks]` keys that are SET and still reach no [`crate::CoreConfig`] field, named
    /// per key. Empty on an ordinary start.
    pub unwired: Vec<&'static str>,
    /// The confirm-grace pairing, when the FINAL pair breaks HARD LOWER BOUND (a). `None` — the
    /// ordinary case, including every shipped default — means the pairing is safe or the watchdog is
    /// disarmed entirely.
    pub confirm_grace: Option<ConfirmGraceHazard>,
}

impl RunProfile {
    /// Parse a profile from a TOML string, then [`validate`](Self::validate) it.
    pub fn from_toml_str(s: &str) -> Result<Self, ProfileError> {
        let profile: RunProfile =
            toml::from_str(s).map_err(|e| ProfileError::Parse(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Read a profile from a TOML file, then [`validate`](Self::validate) it. Every error variant
    /// is prefixed with `path`'s display form — a malformed/missing profile is exactly the moment an
    /// operator needs to know WHICH file to fix, not just that parsing failed somewhere.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ProfileError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| ProfileError::Io(format!("{}: {e}", path.display())))?;
        Self::from_toml_str(&text).map_err(|err| match err {
            ProfileError::Parse(m) => ProfileError::Parse(format!("{}: {m}", path.display())),
            ProfileError::Validation(m) => {
                ProfileError::Validation(format!("{}: {m}", path.display()))
            }
            other @ ProfileError::Io(_) => other,
        })
    }

    /// The [`GridSource`] this profile's own `mode` implies — the SOLE place that mapping is made.
    /// [`Mode::Backtest`]/[`Mode::Paper`] never fetch a real venue grid, so
    /// [`GridSource::NoGridFetched`] is the only sound value (the profile is free to be the
    /// instrument grid's source); [`Mode::Live`] always mounts a real venue and fetches one, so
    /// [`GridSource::VenueFetched`] is the only sound value. Nothing downstream should construct a
    /// `GridSource` independently once a `RunProfile` is in hand — use this (or
    /// [`RunProfile::apply_risk`], which calls it internally) instead.
    pub fn grid_source(&self) -> GridSource {
        match self.mode {
            Mode::Backtest | Mode::Paper => GridSource::NoGridFetched,
            Mode::Live => GridSource::VenueFetched,
        }
    }

    /// Merge this profile's `[risk]` table onto `base` via [`ProfileRisk::apply_to`], with the
    /// [`GridSource`] derived from `self.mode` ([`RunProfile::grid_source`]) rather than accepted
    /// as a caller-supplied parameter — so a caller holding a `RunProfile` can never pass a
    /// `GridSource` that contradicts that profile's own `mode` (a `backtest`/`paper` profile is
    /// always merged as [`GridSource::NoGridFetched`]; a `live` profile is always merged as
    /// [`GridSource::VenueFetched`], and — per `RunProfile::validate`'s unconditional live-mode
    /// check — can never carry a venue-owned instrument field for `apply_to` to reject anyway).
    pub fn apply_risk(
        &self,
        base: vike_exec::RiskLimits,
    ) -> Result<vike_exec::RiskLimits, ProfileError> {
        self.risk.apply_to(base, self.grid_source())
    }

    /// Apply every `[guards]` / `[sinks]` key that has a [`crate::CoreConfig`] counterpart, and
    /// return the keys that were SET and still reach nothing — so a caller can say so out loud
    /// instead of the operator finding out by watching a guard not fire.
    ///
    /// ⚠ **This is the wiring that was missing for the profile's whole life.** `[risk]` was wired
    /// in #816; `[guards]` and the rest of `[sinks]` were left behind and were, from that day,
    /// inert in every binary. What both composition roots did with `[guards]` instead was
    /// `if p.guards != Guards::default() { warn!("… set but NOT consumed …") }` — a settings key
    /// that nothing reads, announced as such, which is precisely the state this workspace deleted
    /// `Policy::max_total_exposure` for. Five of the seven guards map 1:1 onto `CoreConfig` fields
    /// that already existed and whose converters already returned the right types, so the wiring
    /// was field assignment; it was simply never done.
    ///
    /// ⚠ The caller passes a `CoreConfig` it has ALREADY built, and this only overwrites the fields
    /// the profile actually names — the `Option` guards are skipped when `None`, so a profile that
    /// declares no guard cannot silently reset a knob the binary set for its own reasons. The two
    /// non-`Option` fields (`conditionals_on_ticks`, and `equity_sample` when the profile sets it)
    /// are applied unconditionally, which is what "the profile is the auditable authority" means
    /// for a boolean that has no "unset".
    ///
    /// ⚠ **CONFIRM-GRACE: EVERY key is gated on ITS OWN presence, `submit_ack_confirm_grace_ms`
    /// included.** That reads like a restatement of the paragraph above and is written out because
    /// this one field did NOT obey it, for the whole life of this function, on a live order path.
    /// It was gated on `submit_ack_timeout_ms.is_some()` instead — "the grace is only meaningful
    /// alongside stage 1" — and both halves of that were wrong:
    ///
    /// - A profile arming `submit_ack_timeout_ms` and saying NOTHING about the grace still WROTE a
    ///   grace: `Guards`' own `DEFAULT_CONFIRM_GRACE_MS`, 5 s, over `CoreConfig::default`'s 15 s.
    ///   Silence was being answered with a number, and the number had rotted (the confirm-race
    ///   hardening bumped `CoreConfig`'s to 15 s and left the copy here at 5 s). The result broke
    ///   HARD LOWER BOUND (a) on [`crate::CoreConfig::submit_ack_confirm_grace`] against the 30 s
    ///   timeout the live root arms — `2·5s = 10s`, needed `> 30/2 + 2·5 = 25s` for a two-hop
    ///   (Bybit) confirm — i.e. the last-resort synthesized `OrderRejected` could fire while the
    ///   adapter's own re-query was still in flight: a PHANTOM REJECT of a live order, from a
    ///   profile that never mentioned the grace. `crates/vike-tradehub/src/tradehub_cli.rs`'s
    ///   `submit_ack_timeout` literal argues that pairing at its call site; a profile could
    ///   silently invalidate the argument.
    /// - Conversely, a profile naming ONLY `submit_ack_confirm_grace_ms` was silently DROPPED —
    ///   the same declared-but-inert defect this whole function exists to end. "Only meaningful
    ///   alongside stage 1" confuses the PROFILE's stage 1 with the CORE's: `vike-tradehub` sets
    ///   `submit_ack_timeout: Some(30s)` in its own `CoreConfig` literal, so the watchdog is armed
    ///   whether or not the profile mentions it.
    ///
    /// ⚠ Both bullets said "BOTH live roots" and named the GUI shell as the second. That was true
    /// when they were written and is not now: the desktop cut took the local trading core out of
    /// `crates/vike-desktop/src/main.rs` (then spelled `vike-app`), so `vike-tradehub` is the only
    /// root that builds a `CoreConfig` or arms this watchdog. Neither the defect nor the arithmetic
    /// moves — one root stating the pairing is still one place a profile can invalidate it.
    ///
    /// So: named ⇒ applied, unnamed ⇒ untouched, in both directions and independently per key.
    /// `the_confirm_grace_is_written_only_when_the_operator_names_it` pins all four combinations
    /// and `the_shipped_confirm_grace_pairing_satisfies_the_lower_bound` pins the arithmetic.
    ///
    /// ⚠ **AND THE PAIR IS NOW CHECKED, which is the residual that fix declared.** Gating each key
    /// on its own presence fixes the direction where an unnamed grace was overwritten; it does
    /// nothing about the other one — a profile that RAISES `submit_ack_timeout_ms` and says nothing
    /// about the grace raises the bound the untouched grace must clear, and can walk it under bound
    /// (a) without naming the key. [`RunProfile::validate`] cannot see that, because the other half
    /// of the pair is the CALLER's `CoreConfig`. This function holds both numbers, so it evaluates
    /// the bound on the FINAL pair — after every assignment above — and reports a breach in
    /// [`GuardsReport::confirm_grace`]. It WARNS rather than refusing, and
    /// [`ConfirmGraceHazard`] carries that argument against
    /// `docs/decisions/0013-degrade-vs-refuse.md`'s four questions rather than asserting it.
    ///
    /// Evaluating the FINAL pair rather than the profile's DELTA is deliberate: the hazard is a
    /// property of the two numbers a mount runs with, not of which of them an operator typed, and a
    /// check keyed on "did the profile raise the timeout" would miss a profile that LOWERS the grace
    /// under a timeout it never mentioned. The declared residual of that choice is the mirror one: a
    /// binary whose OWN literals broke the bound would only be told when a profile is present at
    /// all, since nothing calls this function without one —
    /// `the_shipped_confirm_grace_pairing_satisfies_the_lower_bound` is what covers the shipped
    /// literals, and it covers them at merge time rather than at startup, which is better.
    ///
    /// The returned report's `unwired` is the STILL-UNWIRED set, named per key. Two guards remain,
    /// and neither is an oversight left unstated:
    ///
    /// - `guards.initial_trading_state` — `CoreConfig` carries no trading-state field at all; the
    ///   state is set on the ENGINE after assembly, so wiring it is new surface rather than an
    ///   assignment. It is a real operator gap (`docs/ops/kill-switches.md` §6: "You cannot start
    ///   the headless daemon halted from its profile").
    /// - `guards.freshness_ms` — the threshold belongs to a per-subscription `FreshnessTracker` in
    ///   the live feed loops, which no `CoreConfig` reaches.
    ///
    /// `every_guard_and_sink_field_is_wired_or_declared` is the gate that keeps this list honest:
    /// it destructures both structs exhaustively, so a NEW field cannot be added without being
    /// classified here.
    pub fn apply_guards_and_sinks(&self, cfg: &mut crate::CoreConfig) -> GuardsReport {
        if let Some(t) = self.guards.submit_ack_timeout() {
            cfg.submit_ack_timeout = Some(t);
        }
        // ⚠ GATED ON THE GRACE'S OWN KEY, never on stage 1's — see the ⚠ CONFIRM-GRACE paragraph
        // in this function's doc for the live-order hazard the old gate carried.
        if let Some(g) = self.guards.submit_ack_confirm_grace() {
            cfg.submit_ack_confirm_grace = g;
        }
        if let Some(dd) = self.guards.max_drawdown {
            cfg.max_drawdown = Some(dd);
        }
        cfg.conditionals_on_ticks = self.guards.conditionals_on_ticks;
        if let Some(mc) = self.guards.margin_call_config() {
            cfg.margin_call = Some(mc);
        }
        if let Some(every) = self.sinks.equity_sample() {
            cfg.equity_sample = Some(every);
        }

        let mut unwired: Vec<&'static str> = Vec::new();
        if self.guards.initial_trading_state != ProfileTradingState::default() {
            unwired.push("guards.initial_trading_state");
        }
        if self.guards.freshness_ms.is_some() {
            unwired.push("guards.freshness_ms");
        }
        if self.sinks.gui {
            unwired.push("sinks.gui");
        }
        if self.sinks.recorder {
            unwired.push("sinks.recorder");
        }
        if self.sinks.raw_capture_dir.is_some() {
            unwired.push("sinks.raw_capture_dir");
        }

        // ⚠ THE PAIR CHECK — on `cfg`, AFTER every assignment above, so what is judged is the pair
        // the mount will actually run with rather than the half this profile happened to name. A
        // `None` timeout is not a breach: stage 1 is disarmed, no watchdog thread is spawned, and
        // the grace is inert (`CoreConfig::submit_ack_confirm_grace`: "Ignored when
        // `submit_ack_timeout` is `None`") — warning there would be the noise this file's other
        // tests exist to prevent.
        let grace = cfg.submit_ack_confirm_grace;
        let confirm_grace = match cfg.submit_ack_timeout {
            Some(timeout) if !confirm_grace_clears_bound(timeout, grace) => {
                Some(ConfirmGraceHazard {
                    submit_ack_timeout: timeout,
                    confirm_grace: grace,
                    confirm_budget: confirm_budget(timeout),
                })
            }
            _ => None,
        };

        GuardsReport { unwired, confirm_grace }
    }

    /// The `[risk]` table this profile carries — but ONLY for a caller about to hand it to a REAL
    /// live venue mount (`vike_mount::make_engine`'s `risk_profile` parameter, which hardcodes
    /// `GridSource::VenueFetched` for every one of its ~12 venue arms, never a caller-supplied
    /// mode — see that function's doc). `Err` unless `self.mode == Mode::Live`.
    ///
    /// WHY THIS GUARD EXISTS (closes a BLOCKING finding from the runprofile-wiring review): a
    /// `backtest`/`paper` profile may LEGALLY set the venue-owned instrument grid fields
    /// (`tick_size`/`lot_size`/`min_qty`/`min_notional`) — its OWN `mode` implies
    /// [`GridSource::NoGridFetched`], and [`RunProfile::validate`] only forbids those fields for
    /// `mode = "live"`. Handing such a profile's bare `[risk]` table to a live mount (which only
    /// ever sees a `ProfileRisk`, not the `RunProfile` it came from, and so cannot see `mode`)
    /// makes `ProfileRisk::apply_to`'s `VenueFetched` check reject it on EVERY venue arm — and
    /// without this guard, the caller would only learn that from a per-venue log line while the
    /// mount proceeded with an unarmed budget on all of them (the `vike-mount` merge site's
    /// fallback narrows that damage to the offending instrument fields only, but a profile that
    /// was never meant to reach a live mount at all should fail LOUD at resolution, not degrade
    /// quietly however narrowly). Call this instead of reading `.risk` directly at any site that
    /// feeds a live 12-venue mount (`vike-app`'s `App::new`, `vike-tradehub`'s live arm).
    pub fn risk_for_live_venue_mount(&self) -> Result<&vike_exec::ProfileRisk, ProfileError> {
        if self.mode != Mode::Live {
            return Err(ProfileError::Validation(format!(
                "profile{} has mode = {:?}, but a live venue mount requires mode = \"live\" — a \
                 backtest/paper profile's [risk] table may legally set venue-owned instrument \
                 fields (tick_size/lot_size/min_qty/min_notional), which the live mount's \
                 hardcoded GridSource::VenueFetched would reject on EVERY venue arm, at best \
                 leaving only a per-venue log line where a loud startup failure belongs",
                self.name.as_deref().map(|n| format!(" `{n}`")).unwrap_or_default(),
                self.mode
            )));
        }
        Ok(&self.risk)
    }

    /// Reject syntactically-valid but semantically-nonsensical profiles with a clear message.
    pub fn validate(&self) -> Result<(), ProfileError> {
        let bail = |m: String| Err(ProfileError::Validation(m));

        // --- mode ↔ broker ↔ event-source consistency -------------------------------------------
        match self.mode {
            Mode::Backtest => {
                if self.broker.kind != BrokerKind::Paper {
                    return bail("backtest mode requires a `paper` broker".into());
                }
                if self.event_source.kind == EventSourceKind::LiveVenue {
                    return bail(
                        "backtest mode cannot use a `live_venue` event source (use hist/replay)"
                            .into(),
                    );
                }
            }
            Mode::Paper => {
                if self.broker.kind != BrokerKind::Paper {
                    return bail("paper mode requires a `paper` broker".into());
                }
            }
            Mode::Live => {
                if self.broker.kind != BrokerKind::Venue {
                    return bail(
                        "live mode requires a `venue` broker (paper broker is invalid)".into(),
                    );
                }
                if self.event_source.kind != EventSourceKind::LiveVenue {
                    return bail("live mode requires a `live_venue` event source".into());
                }
            }
        }

        // --- broker -----------------------------------------------------------------------------
        match self.broker.kind {
            BrokerKind::Paper => {
                if !(is_pos(self.broker.seed_cash)) {
                    return bail(
                        "a `paper` broker needs a positive `broker.seed_cash` (starting capital)"
                            .into(),
                    );
                }
            }
            BrokerKind::Venue => {
                if self.broker.venue.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return bail("a `venue` broker requires a non-empty `broker.venue`".into());
                }
            }
        }

        // --- event source -----------------------------------------------------------------------
        if self.event_source.venue.trim().is_empty() {
            return bail("`event_source.venue` must not be empty".into());
        }
        if self.event_source.symbol.trim().is_empty() {
            return bail("`event_source.symbol` must not be empty".into());
        }

        // --- risk limits ------------------------------------------------------------------------
        let r = &self.risk;
        for (name, v) in [
            ("tick_size", r.tick_size),
            ("lot_size", r.lot_size),
            ("min_notional", r.min_notional),
            ("max_notional_per_order", r.max_notional_per_order),
            ("max_total_exposure", r.max_total_exposure),
        ] {
            if let Some(v) = v
                && !is_pos(v)
            {
                return bail(format!("`risk.{name}` must be finite and > 0 (got {v})"));
            }
        }
        // `max_leverage` is the ONE operator-facing leverage knob (issue #822) and it is ENFORCED:
        // `ProfileRisk::im_requirement` converts it to the initial-margin fraction the pre-trade
        // buying-power check reads (`im = 1/lev`). So this bound is no longer cosmetic — `0.0`
        // would divide by zero and a negative would mean unbounded buying power.
        if let Some(lev) = r.max_leverage
            && (!lev.is_finite() || lev < 1.0)
        {
            return bail(format!(
                "`risk.max_leverage` must be finite and >= 1.0 (got {lev}) — 1.0 is no \
                     leverage, 10.0 is 10x; it arms the buying-power check at an initial-margin \
                     requirement of 1/max_leverage"
            ));
        }
        if !(0.0..1.0).contains(&r.required_free_bp_pct) {
            return bail(format!(
                "`risk.required_free_bp_pct` must be in [0.0, 1.0) (got {})",
                r.required_free_bp_pct
            ));
        }
        if let Some(n) = r.max_orders_per_window {
            if n == 0 {
                return bail("`risk.max_orders_per_window` must be >= 1 (omit to disable)".into());
            }
            if r.window_ms <= 0 {
                return bail(
                    "`risk.window_ms` must be > 0 when `max_orders_per_window` is set".into(),
                );
            }
        }
        if r.window_ms < 0 {
            return bail(format!("`risk.window_ms` must be >= 0 (got {})", r.window_ms));
        }
        // STRUCTURAL load-time gate, unconditional for `live` mode — no escape hatch (see the
        // module doc's "`GridSource` is derived from `mode`, not a second flag" section): a
        // `mode = "live"` profile that sets ANY venue-owned instrument field is a config error at
        // load, before any venue connection is attempted. `live` always fetches a real grid at
        // mount (`RunProfile::grid_source` -> `GridSource::VenueFetched`), so the profile can never
        // be a sound source for these fields — there is no flag that makes it sound, unlike
        // `backtest`/`paper`, which never fetch a grid and so may freely set them (enforced only at
        // mount by `ProfileRisk::apply_to`'s semantic `GridSource` check, not needed here).
        if self.mode == Mode::Live {
            let offending = r.venue_owned_fields_set();
            if !offending.is_empty() {
                return bail(format!(
                    "`mode = \"live\"` profile's `[risk]` sets venue-owned instrument field(s) [{}] \
                     — a live mount always fetches the real venue grid, so the venue owns \
                     tick_size/lot_size/min_qty/min_notional and a profile may never set them, at \
                     load or at mount. Remove {} from `[risk]` (only `backtest`/`paper` profiles may \
                     supply the instrument grid themselves).",
                    offending.join(", "),
                    if offending.len() == 1 { "it" } else { "them" }
                ));
            }
        }

        // --- guards -----------------------------------------------------------------------------
        let g = &self.guards;
        if let Some(ms) = g.submit_ack_timeout_ms
            && ms == 0
        {
            return bail("`guards.submit_ack_timeout_ms` must be > 0 (omit to disable)".into());
        }
        if let Some(dd) = g.max_drawdown {
            if !is_frac_unit(dd) {
                return bail(format!(
                    "`guards.max_drawdown` must be in (0.0, 1.0] (omit to disable, got {dd})"
                ));
            }
            // ⚠ The latch measures a FRACTION of `Σ seed_cash + own PnL`
            // (`CoreThread::sweep_drawdown_latch`), so a non-positive `seed_cash` leaves it with no
            // denominator and it can never arm. Refusing at load is the only place that failure is
            // visible BEFORE the daemon is live: the alternative is a profile that reads as
            // protected and silently is not, which is the one direction worse than measuring the
            // drawdown against the wrong number. A `paper` broker is already covered by the check
            // above; this is what makes a `venue` broker's seed load-bearing too.
            if !(is_pos(self.broker.seed_cash)) {
                return bail(format!(
                    "`guards.max_drawdown` = {dd} needs a positive `broker.seed_cash`: the latch \
                     measures the drop as a fraction of configured capital plus own PnL, and with \
                     seed_cash = {} it can never arm",
                    self.broker.seed_cash
                ));
            }
        }
        if let Some(ms) = g.freshness_ms
            && ms == 0
        {
            return bail("`guards.freshness_ms` must be > 0 (omit to disable)".into());
        }
        if let Some(mc) = &g.margin_call {
            if !is_frac_unit(mc.mm_requirement) {
                return bail(format!(
                    "`guards.margin_call.mm_requirement` must be in (0.0, 1.0] (got {})",
                    mc.mm_requirement
                ));
            }
            if !is_frac_unit(mc.warn_fraction) {
                return bail(format!(
                    "`guards.margin_call.warn_fraction` must be in (0.0, 1.0] (got {})",
                    mc.warn_fraction
                ));
            }
            if !mc.buffer.is_finite() || mc.buffer < 0.0 {
                return bail(format!(
                    "`guards.margin_call.buffer` must be finite and >= 0 (got {})",
                    mc.buffer
                ));
            }
        }

        // --- sinks ------------------------------------------------------------------------------
        if self.sinks.recorder && self.event_source.kind != EventSourceKind::LiveVenue {
            return bail(
                "`sinks.recorder` records a LIVE feed — it needs a `live_venue` event source"
                    .into(),
            );
        }
        if let Some(j) = &self.sinks.journal {
            if j.dir.trim().is_empty() {
                return bail("`sinks.journal.dir` must not be empty".into());
            }
            if j.snapshot_every == 0 {
                // 0 would snapshot+flush on EVERY record — a silent p99 cliff (CoreThread asserts this).
                return bail("`sinks.journal.snapshot_every` must be >= 1".into());
            }
            if j.segment_bytes == 0 {
                return bail("`sinks.journal.segment_bytes` must be > 0".into());
            }
            if j.flush_every == 0 {
                return bail("`sinks.journal.flush_every` must be > 0".into());
            }
        }

        Ok(())
    }
}

/// Pure resolver behind [`journal_config_from_env`] (env-free, so it is unit-tested directly): pick
/// the write-ahead journal sink from an optional loaded profile and an optional quick-path dir.
///
/// A loaded `profile` is AUTHORITATIVE — its `[sinks].journal` decides, even when that means "no
/// journal" (a profile with no journal sink returns `None`); the `VIKE_JOURNAL_DIR` fallback is not
/// consulted when a profile is present, so a profile can never be silently overridden. Absent a
/// profile, `journal_dir` (with an optional `snapshot_every` override) builds a default-cadence
/// [`crate::JournalConfig`] via [`crate::JournalConfig::at`]. Absent both → `None`.
fn choose_journal(
    profile: Option<&RunProfile>,
    journal_dir: Option<std::path::PathBuf>,
    snapshot_every: Option<u64>,
) -> Option<crate::JournalConfig> {
    if let Some(p) = profile {
        return p.sinks.journal_config();
    }
    let mut cfg = crate::JournalConfig::at(journal_dir?);
    if let Some(v) = snapshot_every.filter(|&v| v >= 1) {
        cfg.snapshot_every = v;
    }
    Some(cfg)
}

/// Resolve the opt-in write-ahead command journal sink from the environment for a PRODUCTION binary
/// (`vike-app`, the `vike-run` cores). OFF by default so the standard desktop/headless path stays
/// zero-overhead and byte-identical — journaling is enabled only when one of these is set:
///
/// 1. **`VIKE_RUN_PROFILE=<run.toml>`** — load that [`RunProfile`] and use its `[sinks].journal`
///    mapping (the auditable-config-artifact path). A missing/malformed profile disables journaling
///    and logs a `warn` (a set-but-broken profile is a misconfiguration worth surfacing, not silently
///    falling through to the dir knob); a valid profile whose `[sinks]` omits `journal` is honored as
///    "journaling off".
/// 2. **`VIKE_JOURNAL_DIR=<dir>`** — a single-knob quick opt-in with the default cadence
///    ([`crate::JournalConfig::at`]); `VIKE_JOURNAL_SNAPSHOT_EVERY=<n>` optionally overrides the snap
///    cadence. Consulted only when `VIKE_RUN_PROFILE` is unset.
/// 3. neither set → `None`.
///
/// This mirrors the codebase's established env-driven opt-in config (e.g. `vike_exec::affinity`'s
/// `VIKE_PIN_CORES`, the `VIKE_RECORD_PROPERTIES` recorder) — absent env ⇒ inert.
pub fn journal_config_from_env() -> Option<crate::JournalConfig> {
    if let Some(path) = std::env::var_os("VIKE_RUN_PROFILE") {
        return match RunProfile::from_path(&path) {
            Ok(p) => choose_journal(Some(&p), None, None),
            Err(e) => {
                tracing::warn!(
                    "VIKE_RUN_PROFILE set but the profile did not load ({e}); journaling disabled"
                );
                None
            }
        };
    }
    let dir = std::env::var_os("VIKE_JOURNAL_DIR")?;
    let snapshot_every =
        std::env::var("VIKE_JOURNAL_SNAPSHOT_EVERY").ok().and_then(|s| s.parse::<u64>().ok());
    choose_journal(None, Some(std::path::PathBuf::from(dir)), snapshot_every)
}

/// Pick which profile PATH wins: an explicit path (e.g. a `--profile` CLI flag) beats
/// `vars["VIKE_RUN_PROFILE"]`, which beats nothing. Pure precedence only — no filesystem access —
/// so it is the one place both [`resolve_profile`] and any caller that needs just the path (rather
/// than a fully loaded profile) can share the SAME rule instead of re-deriving it. `pub` (rather than
/// crate-private) specifically so `vike-run`'s `incident` bin — which independently re-derived this
/// exact `--profile`-over-`VIKE_RUN_PROFILE` precedence before this function existed — can call this
/// one instead of keeping its own copy; see that bin's `parse_args`.
pub fn resolve_profile_path(
    explicit: Option<&Path>,
    vars: &HashMap<String, String>,
) -> Option<PathBuf> {
    explicit.map(Path::to_path_buf).or_else(|| vars.get("VIKE_RUN_PROFILE").map(PathBuf::from))
}

/// Resolve an optional [`RunProfile`] from an explicit path and an INJECTED variables map — never
/// `std::env::var` (that is the whole point: this function is a pure `Layer::Injected` seam a
/// binary's own env-reading feeds, not a second place that reads the process environment).
///
/// Precedence ([`resolve_profile_path`]): `explicit` wins when present; otherwise
/// `vars["VIKE_RUN_PROFILE"]`; otherwise `Ok(None)` — a run with neither is unchanged from today
/// (the behavior-preserving default this whole wiring program depends on).
///
/// A resolved path that fails to read OR fails to parse/validate is an `Err`, never a silent
/// `Ok(None)`: an operator who typo'd `--profile run.tml` (or a stale `VIKE_RUN_PROFILE`) must be
/// told, not quietly handed the built-in defaults — that is the dangerous failure mode this
/// function exists to close off. Contrast [`journal_config_from_env`], whose narrower job (decide
/// the journal sink only) treats a broken `VIKE_RUN_PROFILE` as "journaling disabled" with a
/// `warn!`; this general-purpose resolver is for callers (the coming vike-tradehub / vike-app
/// wiring) that must abort startup on a bad profile rather than silently downgrade.
pub fn resolve_profile(
    explicit: Option<&Path>,
    vars: &HashMap<String, String>,
) -> Result<Option<RunProfile>, ProfileError> {
    match resolve_profile_path(explicit, vars) {
        Some(path) => RunProfile::from_path(&path).map(Some),
        None => Ok(None),
    }
}

/// Three ready-to-parse sample profiles — one per [`Mode`]. Shipped as string constants (audit
/// co14: "as string constants or test fixtures") so they double as documentation and stay in the
/// test binary. Each is validated by [`samples::all_validate`]'s test.
pub mod samples {
    /// A deterministic offline backtest: hist bars + paper broker.
    pub const BACKTEST_TOML: &str = r#"
name = "btcusdt-1m-backtest"
mode = "backtest"

[event_source]
kind     = "hist"
venue    = "binance"
symbol   = "BTCUSDT"
interval = "1m"
start    = "2024-01-01T00:00:00Z"
end      = "2024-02-01T00:00:00Z"

[broker]
kind      = "paper"
seed_cash = 100000.0

[risk]
tick_size              = 0.1
lot_size               = 0.001
min_notional           = 5.0
max_notional_per_order = 250000.0

[guards]
max_drawdown = 0.25
"#;

    /// Paper-trade a LIVE feed: live_venue data + paper broker, recording the feed to the hist store.
    pub const PAPER_TOML: &str = r#"
name = "btcusdt-paper-live-feed"
mode = "paper"

[event_source]
kind     = "live_venue"
venue    = "binance"
symbol   = "BTCUSDT"
interval = "1m"

[broker]
kind      = "paper"
seed_cash = 10000.0

[sinks]
gui      = true
recorder = true

[risk]
tick_size                   = 0.1
lot_size                    = 0.001
min_notional                = 5.0
max_notional_per_order      = 50000.0
max_total_exposure          = 200000.0
max_orders_per_window       = 20
window_ms                   = 1000
max_leverage                = 10.0
required_free_bp_pct        = 0.05
block_reduce_only_overshoot = true

[guards]
# ⚠ The two watchdog keys are a PAIR, and the pair must satisfy HARD LOWER BOUND (a) on
# `crate::CoreConfig::submit_ack_confirm_grace`: `2·grace > submit_ack_timeout/2 + N·requery`.
# 30000/15000 gives `2·15 = 30s > 15 + 2·5 = 25s` for a two-hop (Bybit) confirm. This sample
# shipped `5000` here — `2·5 = 10s`, which VIOLATES the bound — for as long as the constant it
# was copied from was stale. Omitting the grace key entirely is also correct now: it then keeps
# whatever `CoreConfig` holds, which is this same 15 s.
submit_ack_timeout_ms       = 30000
submit_ack_confirm_grace_ms = 15000
max_drawdown                = 0.20
conditionals_on_ticks       = true
freshness_ms                = 5000
"#;

    /// Live trading: live_venue data + real venue broker, with the journal durability sink
    /// and the full guard stack (incl. the margin-call watchdog).
    pub const LIVE_TOML: &str = r#"
name = "btcusdt-live-binance-demo"
mode = "live"

[event_source]
kind     = "live_venue"
venue    = "binance"
symbol   = "BTCUSDT"
interval = "1m"

[broker]
kind  = "venue"
venue = "binance"
# ⚠ REQUIRED whenever `guards.max_drawdown` is set, on a `venue` broker too: the drawdown latch
# measures the drop as a fraction of CONFIGURED capital plus the daemon's own PnL, never of the
# venue's wallet (which on a shared account is not this daemon's money — see
# `CoreThread::sweep_drawdown_latch`). Set it to the capital this mount is meant to risk.
seed_cash = 25000.0

[sinks]
gui              = true
recorder         = true
equity_sample_ms = 1000

[sinks.journal]
dir            = "data/journal"
segment_bytes  = 67108864
flush_every    = 256
snapshot_every = 1024

[risk]
max_notional_per_order      = 50000.0
max_total_exposure          = 200000.0
max_orders_per_window       = 20
window_ms                   = 1000
max_leverage                = 5.0
required_free_bp_pct        = 0.05
block_reduce_only_overshoot = true

[guards]
initial_trading_state       = "active"
# ⚠ A PAIR — see the same block in `PAPER_TOML` for the bound these two numbers satisfy. This is
# the LIVE sample, so the violation the old `5000` encoded was a phantom-reject window on a real
# order, copied by every operator who started from this file.
submit_ack_timeout_ms       = 30000
submit_ack_confirm_grace_ms = 15000
max_drawdown                = 0.15
conditionals_on_ticks       = true
freshness_ms                = 3000

[guards.margin_call]
mm_requirement = 0.05
warn_fraction  = 0.05
buffer         = 0.10
"#;

    /// All three samples, for iteration in tests/binaries.
    pub const ALL: &[(&str, &str)] =
        &[("backtest", BACKTEST_TOML), ("paper", PAPER_TOML), ("live", LIVE_TOML)];
}

#[cfg(test)]
mod consumption_gate {
    use super::*;

    /// THE GATE that stops `[guards]`/`[sinks]` drifting back into declared-but-inert.
    ///
    /// ⚠ It is an exhaustive DESTRUCTURE, not a list of assertions, and that is the whole point: a
    /// new field on either struct makes this a COMPILE ERROR until its author classifies it as
    /// wired (it lands in `CoreConfig`) or as declared-unwired (it is named by
    /// [`RunProfile::apply_guards_and_sinks`]'s return). `vike_config::CONSUMPTION` gates exactly
    /// this property for `config`/`preferences`/`flags`, and `POLICY_CONSUMERS` for `policy` — but
    /// both key off `vike-config`-owned types, so `RunProfile` (a `vike-core` type, with
    /// `deny_unknown_fields` and a `validate()` that rejects typos, i.e. the artifact that gives an
    /// operator the MOST confidence) was the one settings surface with no gate at all. Four of its
    /// sections were inert for their entire life and nothing could go red.
    #[test]
    fn every_guard_and_sink_field_is_wired_or_declared() {
        // Set EVERY field to a non-default, so each one is observable either in the CoreConfig it
        // lands in or in the unwired list it is named by.
        let guards = Guards {
            initial_trading_state: ProfileTradingState::Halted,
            submit_ack_timeout_ms: Some(1_234),
            submit_ack_confirm_grace_ms: Some(4_321),
            max_drawdown: Some(0.25),
            conditionals_on_ticks: true,
            freshness_ms: Some(9_000),
            margin_call: Some(ProfileMarginCall {
                mm_requirement: 0.5,
                warn_fraction: 0.8,
                buffer: 0.1,
            }),
        };
        let sinks = Sinks {
            gui: true,
            recorder: true,
            raw_capture_dir: Some("/tmp/caps".to_string()),
            journal: None, // wired ELSEWHERE (`journal_config_from_env`), so not this fn's business
            equity_sample_ms: Some(2_500),
        };

        // ⚠ Exhaustive destructure — the compile error a new field causes IS the gate.
        let Guards {
            initial_trading_state,
            submit_ack_timeout_ms,
            submit_ack_confirm_grace_ms,
            max_drawdown,
            conditionals_on_ticks,
            freshness_ms,
            margin_call,
        } = &guards;
        let Sinks { gui, recorder, raw_capture_dir, journal, equity_sample_ms } = &sinks;

        let profile = RunProfile {
            guards: guards.clone(),
            sinks: sinks.clone(),
            ..RunProfile::from_toml_str(samples::LIVE_TOML)
                .expect("the shipped live sample must parse")
        };
        let mut cfg = crate::CoreConfig::default();
        let unwired = profile.apply_guards_and_sinks(&mut cfg).unwired;

        // ── WIRED: each of these must be observable in the CoreConfig the core is built from.
        assert_eq!(
            cfg.submit_ack_timeout,
            Some(Duration::from_millis(*submit_ack_timeout_ms.as_ref().unwrap()))
        );
        assert_eq!(
            cfg.submit_ack_confirm_grace,
            Duration::from_millis(*submit_ack_confirm_grace_ms.as_ref().unwrap())
        );
        assert_eq!(cfg.max_drawdown, *max_drawdown);
        assert_eq!(cfg.conditionals_on_ticks, *conditionals_on_ticks);
        assert!(cfg.margin_call.is_some(), "guards.margin_call must reach CoreConfig");
        assert_eq!(margin_call.is_some(), cfg.margin_call.is_some());
        assert_eq!(
            cfg.equity_sample,
            Some(Duration::from_millis(*equity_sample_ms.as_ref().unwrap()))
        );

        // ── DECLARED-UNWIRED: each must be NAMED, so a binary can disclose it.
        assert!(unwired.contains(&"guards.initial_trading_state"), "{unwired:?}");
        assert_ne!(*initial_trading_state, ProfileTradingState::default(), "precondition");
        assert!(unwired.contains(&"guards.freshness_ms"), "{unwired:?}");
        assert!(freshness_ms.is_some(), "precondition");
        assert!(unwired.contains(&"sinks.gui"), "{unwired:?}");
        assert!(*gui, "precondition");
        assert!(unwired.contains(&"sinks.recorder"), "{unwired:?}");
        assert!(*recorder, "precondition");
        assert!(unwired.contains(&"sinks.raw_capture_dir"), "{unwired:?}");
        assert!(raw_capture_dir.is_some(), "precondition");

        // ── `journal` is wired, but through `journal_config_from_env`, not through this fn — so it
        //    must NOT appear in the unwired list, and must not be silently applied here either.
        assert!(journal.is_none(), "precondition: this profile arms no journal");
        assert!(
            !unwired.iter().any(|k| k.contains("journal")),
            "sinks.journal IS consumed (journal_config_from_env → CoreConfig::journal): {unwired:?}"
        );

        assert_eq!(
            unwired.len(),
            5,
            "every field above is accounted for exactly once: {unwired:?}"
        );
    }

    /// …and the quiet path: a profile that sets NO guard and NO sink names nothing as unwired, so a
    /// binary's disclosure line cannot fire on every ordinary start. A row that fires on every
    /// healthy mount is noise, and noise is what let the original defect sit for six weeks.
    #[test]
    fn a_profile_with_no_guards_declares_nothing_unwired() {
        let profile = RunProfile {
            guards: Guards::default(),
            sinks: Sinks::default(),
            ..RunProfile::from_toml_str(samples::LIVE_TOML).expect("the shipped live sample parses")
        };
        let mut cfg = crate::CoreConfig::default();
        assert!(profile.apply_guards_and_sinks(&mut cfg).unwired.is_empty());
        // …and a profile that declares nothing must not overwrite what the BINARY configured: the
        // `Option` guards are skipped, so the caller's own knobs survive.
        let base = crate::CoreConfig::default();
        assert_eq!(cfg.submit_ack_timeout, base.submit_ack_timeout);
        assert_eq!(cfg.submit_ack_confirm_grace, base.submit_ack_confirm_grace);
        assert_eq!(cfg.max_drawdown, base.max_drawdown);
        assert_eq!(cfg.equity_sample, base.equity_sample);
    }

    /// A `CoreConfig` shaped like the one the live root builds: the binary's own 30 s stage-1
    /// timeout, and a confirm grace nobody has touched.
    /// `crates/vike-tradehub/src/tradehub_cli.rs` constructs exactly this before calling
    /// [`RunProfile::apply_guards_and_sinks`], which is the configuration the hazard below lives in
    /// — a bare `CoreConfig::default()` has `submit_ack_timeout: None` and no watchdog at all, so
    /// testing against one would test the case that cannot be hurt.
    ///
    /// ⚠ The name is plural-flavoured because there WERE two: the GUI shell built the identical
    /// literal until the desktop cut removed its local trading core
    /// (`crates/vike-desktop/src/main.rs`, then spelled `vike-app`). One root now, same shape.
    fn live_root_config() -> crate::CoreConfig {
        crate::CoreConfig {
            submit_ack_timeout: Some(Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS)),
            ..crate::CoreConfig::default()
        }
    }

    /// A profile carrying `guards`, otherwise the shipped live sample.
    fn profile_with(guards: Guards) -> RunProfile {
        RunProfile {
            guards,
            ..RunProfile::from_toml_str(samples::LIVE_TOML).expect("the shipped live sample parses")
        }
    }

    /// The stage-1 timeout the live root arms, in its own `CoreConfig` literal
    /// (`crates/vike-tradehub/src/tradehub_cli.rs`'s `submit_ack_timeout`). Restated here because
    /// `vike-core` cannot read a binary above it; the citation is the link.
    const LIVE_ROOT_TIMEOUT_SECS: u64 = 30;
    /// The adapter's per-re-query REST timeout
    /// (`crates/vike-bridge-core/src/http.rs`'s `blocking_agent_with_timeout`). Restated for the
    /// same reason — `vike-core` sits BELOW `vike-bridge-core` and cannot name it in code.
    const REQUERY_SECS: u64 = 5;
    /// Sequential re-queries the worst-case confirm makes: 2 for Bybit (realtime → history).
    const REQUERY_HOPS: u64 = 2;

    /// THE PAIRING, pinned per key and in all four combinations.
    ///
    /// ⚠ The case that motivated this test is row 2 — arm stage 1, say NOTHING about the grace.
    /// That used to rewrite the grace to `Guards`' own stale `DEFAULT_CONFIRM_GRACE_MS` (5 s) over
    /// `CoreConfig::default`'s 15 s, halving the confirm window on a LIVE order path from a profile
    /// that never mentioned it. Row 3 is the mirror defect the same gate carried: a profile naming
    /// ONLY the grace was silently dropped, because the write was gated on stage 1's key.
    #[test]
    fn the_confirm_grace_is_written_only_when_the_operator_names_it() {
        let default_grace = crate::CoreConfig::default().submit_ack_confirm_grace;
        let named_grace = Duration::from_millis(16_000);
        // Deliberately a value that still clears bound (a) against the default grace
        // (`2·15s = 30s > 20/2 + 2·5 = 20s`): a test fixture should not model the pairing the docs
        // tell an operator to avoid.
        let named_timeout = Duration::from_millis(20_000);

        // 1 — NEITHER named: both knobs stay exactly as the binary built them.
        let mut cfg = live_root_config();
        profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
        assert_eq!(cfg.submit_ack_timeout, Some(Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS)));
        assert_eq!(cfg.submit_ack_confirm_grace, default_grace);

        // 2 — ONLY the timeout named: the timeout moves, THE GRACE DOES NOT.
        let mut cfg = live_root_config();
        profile_with(Guards {
            submit_ack_timeout_ms: Some(named_timeout.as_millis() as u64),
            ..Guards::default()
        })
        .apply_guards_and_sinks(&mut cfg);
        assert_eq!(cfg.submit_ack_timeout, Some(named_timeout));
        assert_eq!(
            cfg.submit_ack_confirm_grace, default_grace,
            "an unnamed grace must survive an armed stage 1 — this is the live-order hazard"
        );

        // 3 — ONLY the grace named: it is APPLIED (the roots' own stage 1 is armed regardless of
        //     what the profile says), and the timeout the binary set is untouched.
        let mut cfg = live_root_config();
        profile_with(Guards {
            submit_ack_confirm_grace_ms: Some(named_grace.as_millis() as u64),
            ..Guards::default()
        })
        .apply_guards_and_sinks(&mut cfg);
        assert_eq!(cfg.submit_ack_timeout, Some(Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS)));
        assert_eq!(
            cfg.submit_ack_confirm_grace, named_grace,
            "a named grace must reach the core even with no `submit_ack_timeout_ms` beside it"
        );

        // 4 — BOTH named: both are the operator's.
        let mut cfg = live_root_config();
        profile_with(Guards {
            submit_ack_timeout_ms: Some(named_timeout.as_millis() as u64),
            submit_ack_confirm_grace_ms: Some(named_grace.as_millis() as u64),
            ..Guards::default()
        })
        .apply_guards_and_sinks(&mut cfg);
        assert_eq!(cfg.submit_ack_timeout, Some(named_timeout));
        assert_eq!(cfg.submit_ack_confirm_grace, named_grace);

        // …and the accessor itself carries the same meaning, so no caller can reintroduce a
        // profile-side default by reading it: unnamed is `None`, not a number.
        assert_eq!(Guards::default().submit_ack_confirm_grace(), None);
    }

    /// HARD LOWER BOUND (a) — `2·grace > submit_ack_timeout/2 + N·requery` — evaluated on what
    /// this workspace actually SHIPS, not on an example.
    ///
    /// The bound is stated in prose on [`crate::CoreConfig::submit_ack_confirm_grace`] and argued
    /// again at both roots' `submit_ack_timeout` literals, and prose cannot notice when one of its
    /// terms moves. It nearly did: `CoreConfig::default`'s grace was bumped 5 s → 15 s and the
    /// profile converter's copy was not, which is the defect the test above pins from the wiring
    /// side. This one pins it from the ARITHMETIC side, so a future re-tune of either shipped
    /// number reddens here rather than in a live phantom reject.
    #[test]
    fn the_shipped_confirm_grace_pairing_satisfies_the_lower_bound() {
        let requery = Duration::from_secs(REQUERY_SECS * REQUERY_HOPS);
        let bound = |timeout: Duration, grace: Duration| grace * 2 > timeout / 2 + requery;

        let timeout = Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS);
        let grace = crate::CoreConfig::default().submit_ack_confirm_grace;
        assert!(
            bound(timeout, grace),
            "the SHIPPED pairing violates HARD LOWER BOUND (a): 2·{grace:?} must exceed \
             {timeout:?}/2 + {requery:?} — the last-resort synthesized OrderRejected can fire \
             while the adapter's own re-query is still in flight, which is a phantom reject of a \
             live order. Raise `CoreConfig::default`'s `submit_ack_confirm_grace` or lower the \
             `submit_ack_timeout` both live roots arm."
        );

        // …and the bound can actually FAIL, so the assertion above is not vacuous: the 5 s the
        // deleted `DEFAULT_CONFIRM_GRACE_MS` used to write is exactly what it refuses.
        assert!(
            !bound(timeout, Duration::from_secs(5)),
            "5 s was the value the profile converter wrote over the default; if this passes the \
             bound has been re-derived and this whole test needs re-reading"
        );

        // Every shipped SAMPLE that arms stage 1 must clear the same bound — these are the files
        // an operator copies, and one of them shipping a violating pair is how the operator
        // acquires it.
        for (name, toml) in samples::ALL {
            let p = RunProfile::from_toml_str(toml).expect("shipped sample parses");
            let Some(t) = p.guards.submit_ack_timeout() else { continue };
            let g = p.guards.submit_ack_confirm_grace().unwrap_or(grace);
            assert!(bound(t, g), "sample `{name}` pairs {t:?} with {g:?}, violating bound (a)");
        }
    }

    /// The RUNTIME warning and this file's own pin must be the SAME bound.
    ///
    /// ⚠ Two spellings of one arithmetic, held equal — the idiom
    /// `crates/vike-bridge-core/tests/settings_dir_spellings.rs` established for the duplicated
    /// settings-dir resolver. The alternative (the test simply calling
    /// [`confirm_grace_clears_bound`]) would make the pin a re-assertion of the code it gates, and
    /// this whole thread exists because a bound stated in two places drifted: `Guards`' own 5 s
    /// constant against `CoreConfig::default`'s 15 s. So the closure above stays an INDEPENDENT
    /// re-derivation from [`REQUERY_SECS`]/[`REQUERY_HOPS`], and this is what refuses to let the two
    /// part company — including when only one side's `N` or requery budget is re-tuned.
    #[test]
    fn the_warning_predicate_agrees_with_the_bound_the_shipped_pairing_is_gated_on() {
        let requery = Duration::from_secs(REQUERY_SECS * REQUERY_HOPS);
        let bound = |timeout: Duration, grace: Duration| grace * 2 > timeout / 2 + requery;

        // A grid that straddles the boundary at several timeouts, so agreement is checked ON the
        // knife edge rather than only in the comfortable interior.
        for t_secs in [1_u64, 5, 20, 30, 45, 60, 90, 300] {
            let timeout = Duration::from_secs(t_secs);
            for g_ms in [0_u64, 1, 999, 5_000, 12_499, 12_500, 12_501, 15_000, 27_501, 60_000] {
                let grace = Duration::from_millis(g_ms);
                assert_eq!(
                    bound(timeout, grace),
                    confirm_grace_clears_bound(timeout, grace),
                    "the runtime predicate and this file's pin disagree at {timeout:?}/{grace:?} — \
                     one of the two spellings of HARD LOWER BOUND (a) has been re-tuned alone"
                );
            }
        }
    }

    /// The number the warning tells an operator to type must actually WORK — driven through the
    /// real predicate, so the claim in [`ConfirmGraceHazard::min_grace_ms`]'s doc is proven rather
    /// than restated. `⌊budget/2⌋ + 1 ms` is also checked to be the SMALLEST such whole
    /// millisecond, so the advice is not merely safe but not needlessly wasteful either.
    #[test]
    fn the_advised_minimum_grace_actually_clears_the_bound() {
        for t_secs in [1_u64, 7, 30, 55, 90, 301] {
            let timeout = Duration::from_secs(t_secs);
            let hazard = ConfirmGraceHazard {
                submit_ack_timeout: timeout,
                confirm_grace: Duration::ZERO,
                confirm_budget: confirm_budget(timeout),
            };
            let advised = Duration::from_millis(hazard.min_grace_ms() as u64);
            assert!(
                confirm_grace_clears_bound(timeout, advised),
                "the warning advises {advised:?} at {timeout:?} and that does not clear the bound"
            );
            assert!(
                !confirm_grace_clears_bound(timeout, advised - Duration::from_millis(1)),
                "the advised {advised:?} at {timeout:?} is one whole millisecond larger than it \
                 needs to be — the operator is being told to over-tune"
            );
        }
    }

    /// THE PAIR WARNING — the residual #1569 declared and did not close.
    ///
    /// ⚠ The case that motivated it is row B: raise `submit_ack_timeout_ms`, say NOTHING about the
    /// grace. That is legal, silent, and walks the untouched grace under bound (a) — #1569's fix
    /// (each key gated on its own presence) is precisely what makes the grace stay put while the
    /// bound it must clear moves out from under it. Rows A and C are the no-fire half and matter as
    /// much: a warning that fires on the shipped defaults, or on an operator who paired the two keys
    /// correctly, is noise, and noise is what let the original defect sit for six weeks.
    #[test]
    fn a_raised_timeout_with_an_untouched_grace_warns_and_a_sound_pairing_does_not() {
        let shipped_grace = crate::CoreConfig::default().submit_ack_confirm_grace;

        // ── A — the shipped defaults, profile naming NEITHER key. 2·15s = 30s > 30/2 + 2·5 = 25s.
        let mut cfg = live_root_config();
        let report = profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
        assert_eq!(
            report.confirm_grace, None,
            "the SHIPPED pairing must not warn — a row that fires on every healthy mount is noise"
        );

        // ── B — THE CASE THAT MATTERS: the timeout is raised, the grace is left where the binary
        //        put it. 2·15s = 30s, against 90/2 + 2·5 = 55s.
        let raised = Duration::from_secs(90);
        let mut cfg = live_root_config();
        let report = profile_with(Guards {
            submit_ack_timeout_ms: Some(raised.as_millis() as u64),
            ..Guards::default()
        })
        .apply_guards_and_sinks(&mut cfg);
        let hazard = report
            .confirm_grace
            .expect("a raised timeout over an untouched grace must be reported");
        assert_eq!(hazard.submit_ack_timeout, raised);
        assert_eq!(hazard.confirm_grace, shipped_grace, "the grace is the one #1569 left in place");
        assert_eq!(hazard.confirm_budget, Duration::from_secs(55));
        assert_eq!(hazard.min_grace_ms(), 27_501);
        // …and the sentence an operator reads names the key they must edit and the number to put
        // in it — the whole point of returning DATA the roots render rather than a bare bool.
        let text = hazard.to_string();
        assert!(text.contains("guards.submit_ack_confirm_grace_ms = 27501"), "{text}");
        assert!(text.contains("PHANTOM REJECT"), "{text}");

        // ── C — the same raised timeout, PAIRED. 2·30s = 60s > 55s: silence.
        let mut cfg = live_root_config();
        let report = profile_with(Guards {
            submit_ack_timeout_ms: Some(raised.as_millis() as u64),
            submit_ack_confirm_grace_ms: Some(30_000),
            ..Guards::default()
        })
        .apply_guards_and_sinks(&mut cfg);
        assert_eq!(
            report.confirm_grace, None,
            "an operator who named a correct pair must not be warned at"
        );
        // The advised minimum is the boundary, and it is INCLUSIVE — the warning would be lying if
        // typing its own number still warned on the next start.
        let mut cfg = live_root_config();
        let report = profile_with(Guards {
            submit_ack_timeout_ms: Some(raised.as_millis() as u64),
            submit_ack_confirm_grace_ms: Some(27_501),
            ..Guards::default()
        })
        .apply_guards_and_sinks(&mut cfg);
        assert_eq!(report.confirm_grace, None, "the advised minimum must silence the warning");

        // ── D — THE MIRROR DIRECTION, which is why the check reads the FINAL pair rather than the
        //        profile's delta: the timeout is never mentioned, the GRACE is lowered under it.
        //        A check keyed on "did this profile raise the timeout" would miss this entirely.
        let mut cfg = live_root_config();
        let report =
            profile_with(Guards { submit_ack_confirm_grace_ms: Some(5_000), ..Guards::default() })
                .apply_guards_and_sinks(&mut cfg);
        let hazard =
            report.confirm_grace.expect("a grace lowered under an untouched timeout must warn");
        assert_eq!(hazard.submit_ack_timeout, Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS));
        assert_eq!(hazard.confirm_grace, Duration::from_secs(5));

        // ── E — the watchdog DISARMED (`CoreConfig::default`'s `submit_ack_timeout: None`). No
        //        stage 1, no ladder, nothing to race: the grace is inert and warning is noise.
        let mut cfg = crate::CoreConfig {
            submit_ack_confirm_grace: Duration::from_millis(1),
            ..crate::CoreConfig::default()
        };
        assert_eq!(cfg.submit_ack_timeout, None, "precondition: stage 1 is disarmed");
        let report = profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
        assert_eq!(
            report.confirm_grace, None,
            "a disarmed watchdog has no pairing to break, whatever the grace says"
        );

        // …and the two halves of the report are INDEPENDENT. The live sample's `[sinks]` names two
        // keys that reach no `CoreConfig` whatever the guards say, so the same profile is applied
        // twice — once tripping the bound, once not — and the unwired set must be the SAME list.
        // A hazard is not a reason to call a key unwired, nor a reason to stop naming one.
        let mut cfg = live_root_config();
        let hazardous = profile_with(Guards {
            submit_ack_timeout_ms: Some(raised.as_millis() as u64),
            ..Guards::default()
        })
        .apply_guards_and_sinks(&mut cfg);
        let mut cfg = live_root_config();
        let sound = profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
        assert!(hazardous.confirm_grace.is_some(), "precondition");
        assert_eq!(sound.confirm_grace, None, "precondition");
        assert_eq!(
            hazardous.unwired, sound.unwired,
            "the pair check must not disturb the still-unwired disclosure"
        );
        assert_eq!(
            hazardous.unwired,
            vec!["sinks.gui", "sinks.recorder"],
            "…and it is a real list, not two empty ones compared to each other"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::samples::{BACKTEST_TOML, LIVE_TOML, PAPER_TOML};
    use super::*;

    // ---------------------------------------------------------------------------------------------
    // Round-trip: parse each sample -> assert the expected structured values.
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn backtest_sample_round_trips() {
        let p = RunProfile::from_toml_str(BACKTEST_TOML).expect("backtest parses");
        assert_eq!(p.name.as_deref(), Some("btcusdt-1m-backtest"));
        assert_eq!(p.mode, Mode::Backtest);
        assert_eq!(p.event_source.kind, EventSourceKind::Hist);
        assert_eq!(p.event_source.venue, "binance");
        assert_eq!(p.event_source.symbol, "BTCUSDT");
        assert_eq!(p.event_source.interval.as_deref(), Some("1m"));
        assert_eq!(p.event_source.start.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(p.broker.kind, BrokerKind::Paper);
        assert_eq!(p.broker.seed_cash, 100_000.0);
        // sinks omitted -> all off (headless)
        assert!(!p.sinks.gui);
        assert!(!p.sinks.recorder);
        assert!(p.sinks.journal.is_none());
        // risk
        assert_eq!(p.risk.tick_size, Some(0.1));
        // No `max_leverage`: a backtest sample must not arm the buying-power check. (#817 set
        // `max_leverage = 1.0` here when the knob was inert; now that it CONVERTS to
        // `im_requirement`, leaving it would silently arm a 1× margin gate on every backtest.)
        assert_eq!(p.risk.max_leverage, None);
        assert_eq!(p.risk.to_risk_limits().im_requirement, None);
        // guards
        assert_eq!(p.guards.max_drawdown, Some(0.25));
        assert_eq!(p.guards.initial_trading_state, ProfileTradingState::Active);
        assert_eq!(p.guards.submit_ack_timeout(), None);
    }

    #[test]
    fn paper_sample_round_trips() {
        let p = RunProfile::from_toml_str(PAPER_TOML).expect("paper parses");
        assert_eq!(p.mode, Mode::Paper);
        assert_eq!(p.event_source.kind, EventSourceKind::LiveVenue);
        assert_eq!(p.broker.kind, BrokerKind::Paper);
        assert_eq!(p.broker.seed_cash, 10_000.0);
        assert!(p.sinks.gui);
        assert!(p.sinks.recorder);
        // risk
        assert_eq!(p.risk.max_orders_per_window, Some(20));
        assert_eq!(p.risk.window_ms, 1000);
        // `max_leverage` is the only leverage key; 10x ⇒ the enforced 10% initial margin.
        assert_eq!(p.risk.max_leverage, Some(10.0));
        assert_eq!(p.risk.im_requirement(), Some(0.1));
        assert!(p.risk.block_reduce_only_overshoot);
        // `min_qty` absent from PAPER_TOML's [risk] block -> Option field defaults to None.
        assert_eq!(p.risk.min_qty, None);
        // guards -> Duration mapping
        assert_eq!(p.guards.submit_ack_timeout(), Some(Duration::from_millis(30_000)));
        // The grace is `Option` now — `None` would mean "the sample names none", not "5 s".
        assert_eq!(p.guards.submit_ack_confirm_grace(), Some(Duration::from_millis(15_000)));
        assert_eq!(p.guards.max_drawdown, Some(0.20));
        assert!(p.guards.conditionals_on_ticks);
        assert_eq!(p.guards.freshness(), Some(Duration::from_millis(5_000)));
        assert!(p.guards.margin_call_config().is_none());
    }

    #[test]
    fn live_sample_round_trips() {
        let p = RunProfile::from_toml_str(LIVE_TOML).expect("live parses");
        assert_eq!(p.mode, Mode::Live);
        assert_eq!(p.broker.kind, BrokerKind::Venue);
        assert_eq!(p.broker.venue.as_deref(), Some("binance"));
        assert_eq!(p.event_source.kind, EventSourceKind::LiveVenue);
        assert_eq!(p.sinks.equity_sample(), Some(Duration::from_millis(1000)));
        let j = p.sinks.journal.as_ref().expect("journal sink present");
        assert_eq!(j.dir, "data/journal");
        assert_eq!(j.segment_bytes, 67_108_864);
        assert_eq!(j.snapshot_every, 1024);
        assert_eq!(p.guards.initial_trading_state, ProfileTradingState::Active);
        assert_eq!(p.guards.max_drawdown, Some(0.15));
        assert!(p.guards.margin_call.is_some());
        // A `live` profile must never carry a venue-owned instrument field (see
        // `RunProfile::validate`'s unconditional live-mode check) — the sample carries none.
        assert_eq!(p.risk.min_qty, None);
        assert_eq!(p.risk.tick_size, None);
        assert_eq!(p.risk.lot_size, None);
        assert_eq!(p.risk.min_notional, None);
    }

    #[test]
    fn all_samples_validate() {
        for (name, toml) in samples::ALL {
            RunProfile::from_toml_str(toml)
                .unwrap_or_else(|e| panic!("sample `{name}` must be valid: {e}"));
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Mapping to the real runtime types (the compile-checked converters).
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn maps_to_real_risk_limits() {
        let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
        let got = p.risk.to_risk_limits();
        let want = vike_exec::RiskLimits {
            // The live sample carries no instrument-grid fields (a live profile may never set
            // them — see `RunProfile::validate`'s unconditional live-mode check).
            tick_size: None,
            lot_size: None,
            min_notional: None,
            min_qty: None,
            max_notional_per_order: Some(50_000.0),
            max_total_exposure: Some(200_000.0),
            max_orders_per_window: Some(20),
            window_ms: 1000,
            max_leverage: Some(5.0),
            block_reduce_only_overshoot: true,
            // DERIVED from `max_leverage` at the config edge — LIVE_TOML has no `im_requirement`
            // key at all (issue #822: one operator-facing name, one enforced storage field).
            im_requirement: Some(0.2),
            im_by_symbol: Default::default(),
            required_free_bp_pct: 0.05,
            max_slippage_bps: None,
            require_fillable: false,
            price_collar: None,
            collar_by_symbol: Default::default(),
            grid_by_symbol: Default::default(),
            // ⚠ `None`, and this assertion is what PINS that a run profile cannot arm the
            // ACCOUNT-aggregate ceiling: `LIVE_TOML` is the fullest `[risk]` table in the tree, and
            // the converter still produces `None` here. That ceiling's authority is
            // `<project>/settings/policy.toml`, deliberately — see
            // `vike_exec::ProfileRisk::to_risk_limits`'s own line. If a `[risk]
            // max_account_exposure` key is ever accepted, this line is where the change becomes
            // visible.
            max_account_exposure: None,
            // ⚠ `None`, and the same pin one axis over: the fullest `[risk]` table in the tree
            // still cannot arm the SIZING-EQUITY ceiling. Its authority is
            // `<project>/settings/policy.toml`; if a `[risk] max_sizing_equity` key is ever
            // accepted, this line is where the change becomes visible.
            max_sizing_equity: None,
        };
        assert_eq!(got, want);
    }

    #[test]
    fn maps_to_real_margin_call_and_trading_state() {
        let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
        let mc = p.guards.margin_call_config().expect("margin call present");
        assert_eq!(mc.mm_requirement, 0.05);
        assert_eq!(mc.warn_fraction, 0.05);
        assert_eq!(mc.buffer, 0.10);
        assert_eq!(p.guards.trading_state(), vike_exec::TradingState::Active);
    }

    #[test]
    fn maps_to_real_journal_config() {
        let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
        let jc = p.sinks.journal_config().expect("journal config present");
        assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
        assert_eq!(jc.file.segment_bytes, 67_108_864);
        assert_eq!(jc.file.flush_every, 256);
        assert_eq!(jc.snapshot_every, 1024);
    }

    #[test]
    fn margin_call_defaults_apply() {
        // omit warn_fraction/buffer -> LEAN defaults
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[guards.margin_call]
mm_requirement = 0.04
"#;
        let p = RunProfile::from_toml_str(toml).unwrap();
        let mc = p.guards.margin_call.unwrap();
        assert_eq!(mc.mm_requirement, 0.04);
        assert_eq!(mc.warn_fraction, 0.05);
        assert_eq!(mc.buffer, 0.10);
    }

    #[test]
    fn sinks_parses_equity_sample_ms() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[sinks]
equity_sample_ms = 1000
"#;
        let p = RunProfile::from_toml_str(toml).unwrap();
        assert_eq!(p.sinks.equity_sample(), Some(Duration::from_millis(1000)));

        // absent -> None
        let toml2 = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[sinks]
"#;
        let p2 = RunProfile::from_toml_str(toml2).unwrap();
        assert_eq!(p2.sinks.equity_sample(), None);
    }

    #[test]
    fn halted_maps_to_kill_switch() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[guards]
initial_trading_state = "halted"
"#;
        let p = RunProfile::from_toml_str(toml).unwrap();
        assert_eq!(p.guards.trading_state(), vike_exec::TradingState::Halted);
    }

    // ---------------------------------------------------------------------------------------------
    // Unknown-field policy: DENY (top-level AND nested).
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn unknown_top_level_field_rejected() {
        let toml = r#"
mode = "backtest"
surprise = true
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
"#;
        let err = RunProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
        assert!(err.to_string().contains("surprise"), "message names the key: {err}");
    }

    #[test]
    fn unknown_nested_field_rejected() {
        // a typo'd risk key must NOT be silently ignored
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[risk]
max_levarage = 10.0
"#;
        let err = RunProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // Malformed input -> a clear Parse error (not a panic).
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn malformed_toml_is_parse_error() {
        let err = RunProfile::from_toml_str("this is not = = toml [[[").unwrap_err();
        assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn bad_enum_value_is_parse_error() {
        let toml = r#"
mode = "hyperspeed"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
"#;
        let err = RunProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // Semantic validation -> clear Validation errors.
    // ---------------------------------------------------------------------------------------------

    fn assert_validation(toml: &str, needle: &str) {
        let err = RunProfile::from_toml_str(toml).unwrap_err();
        match err {
            ProfileError::Validation(m) => {
                assert!(m.contains(needle), "expected `{needle}` in validation message, got: {m}")
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[test]
    fn live_with_paper_broker_rejected() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 100.0
"#;
        assert_validation(toml, "live mode requires a `venue` broker");
    }

    #[test]
    fn live_with_hist_source_rejected() {
        let toml = r#"
mode = "live"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
"#;
        assert_validation(toml, "live mode requires a `live_venue` event source");
    }

    #[test]
    fn backtest_with_venue_broker_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
"#;
        assert_validation(toml, "backtest mode requires a `paper` broker");
    }

    #[test]
    fn backtest_with_live_source_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
"#;
        assert_validation(toml, "backtest mode cannot use a `live_venue`");
    }

    #[test]
    fn paper_broker_zero_seed_cash_rejected() {
        let toml = r#"
mode = "paper"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 0.0
"#;
        assert_validation(toml, "positive `broker.seed_cash`");
    }

    #[test]
    fn venue_broker_missing_venue_rejected() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
"#;
        assert_validation(toml, "requires a non-empty `broker.venue`");
    }

    #[test]
    fn empty_symbol_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "   "
[broker]
kind = "paper"
seed_cash = 1.0
"#;
        assert_validation(toml, "`event_source.symbol` must not be empty");
    }

    #[test]
    fn negative_tick_size_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[risk]
tick_size = -0.1
"#;
        assert_validation(toml, "`risk.tick_size`");
    }

    #[test]
    fn leverage_below_one_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[risk]
max_leverage = 0.5
"#;
        assert_validation(toml, "`risk.max_leverage` must be finite and >= 1.0");
    }

    /// Issue #822: `[risk] im_requirement` is RETIRED from the operator surface — `max_leverage`
    /// is the only leverage key, and the config edge derives `im_requirement` from it. Because
    /// `ProfileRisk` is `deny_unknown_fields`, a profile still setting the old key fails LOUDLY
    /// naming it, rather than parsing fine and silently arming nothing (a silently-ignored risk
    /// key is exactly the live-money footgun the DENY policy exists for).
    #[test]
    fn retired_im_requirement_key_is_rejected_by_name() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[risk]
im_requirement = 0.1
"#;
        let err = RunProfile::from_toml_str(toml).expect_err("the retired key must not parse");
        let msg = err.to_string();
        assert!(msg.contains("im_requirement"), "the error must name the retired key: {msg}");
    }

    #[test]
    fn max_orders_without_window_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[risk]
max_orders_per_window = 10
"#;
        assert_validation(toml, "`risk.window_ms` must be > 0");
    }

    // ---------------------------------------------------------------------------------------------
    // Structural load-time gate: `mode = "live"` may never set a venue-owned `[risk]` field — no
    // escape hatch (the corrected design: `GridSource` is derived from `mode`, not a second flag).
    // ---------------------------------------------------------------------------------------------

    /// One live-mode profile template per venue-owned field name, so each can be proven
    /// independently to fail at load (each check narrowed to a single field must still fail this
    /// test if a regression drops one field from the gate).
    fn live_toml_with_risk_field(field_line: &str) -> String {
        format!(
            r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[risk]
{field_line}
"#
        )
    }

    #[test]
    fn live_mode_with_min_qty_rejected_at_load() {
        assert_validation(&live_toml_with_risk_field("min_qty = 0.5"), "min_qty");
    }

    #[test]
    fn live_mode_with_tick_size_rejected_at_load() {
        assert_validation(&live_toml_with_risk_field("tick_size = 0.1"), "tick_size");
    }

    #[test]
    fn live_mode_with_lot_size_rejected_at_load() {
        assert_validation(&live_toml_with_risk_field("lot_size = 0.01"), "lot_size");
    }

    #[test]
    fn live_mode_with_min_notional_rejected_at_load() {
        assert_validation(&live_toml_with_risk_field("min_notional = 5.0"), "min_notional");
    }

    #[test]
    fn live_mode_names_every_offending_field_when_several_are_set() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[risk]
tick_size = 0.1
lot_size = 0.01
"#;
        let err = RunProfile::from_toml_str(toml).unwrap_err();
        let ProfileError::Validation(m) = err else { panic!("expected Validation error: {err:?}") };
        assert!(m.contains("risk.tick_size"), "message: {m}");
        assert!(m.contains("risk.lot_size"), "message: {m}");
    }

    #[test]
    fn live_mode_with_no_instrument_fields_is_allowed() {
        // A `live` profile confined to operator-budget fields is unaffected by the gate — the
        // common case must stay exactly as easy as before.
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[risk]
max_notional_per_order = 1000.0
max_total_exposure = 5000.0
"#;
        RunProfile::from_toml_str(toml)
            .expect("operator-only fields are always allowed under live");
    }

    #[test]
    fn backtest_mode_with_instrument_fields_is_allowed() {
        // backtest never fetches a real venue grid, so the profile is free to set every
        // instrument-grid field with no opt-in of any kind.
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[risk]
tick_size = 0.1
lot_size = 0.01
min_qty = 0.5
min_notional = 5.0
"#;
        let p = RunProfile::from_toml_str(toml).expect("backtest may set instrument fields freely");
        assert_eq!(p.risk.tick_size, Some(0.1));
        assert_eq!(p.risk.lot_size, Some(0.01));
        assert_eq!(p.risk.min_qty, Some(0.5));
        assert_eq!(p.risk.min_notional, Some(5.0));
    }

    #[test]
    fn paper_mode_with_instrument_fields_is_allowed() {
        // paper never fetches a real venue grid either — same freedom as backtest.
        let toml = r#"
mode = "paper"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[risk]
tick_size = 0.1
lot_size = 0.01
min_qty = 0.5
min_notional = 5.0
"#;
        let p = RunProfile::from_toml_str(toml).expect("paper may set instrument fields freely");
        assert_eq!(p.risk.tick_size, Some(0.1));
        assert_eq!(p.risk.lot_size, Some(0.01));
        assert_eq!(p.risk.min_qty, Some(0.5));
        assert_eq!(p.risk.min_notional, Some(5.0));
    }

    // ---------------------------------------------------------------------------------------------
    // RunProfile::grid_source — derived straight from `mode`.
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn grid_source_from_live_profile_is_venue_fetched() {
        let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
        assert_eq!(p.grid_source(), GridSource::VenueFetched);
    }

    #[test]
    fn grid_source_from_backtest_profile_is_no_grid_fetched() {
        let p = RunProfile::from_toml_str(BACKTEST_TOML).unwrap();
        assert_eq!(p.grid_source(), GridSource::NoGridFetched);
    }

    #[test]
    fn grid_source_from_paper_profile_is_no_grid_fetched() {
        let p = RunProfile::from_toml_str(PAPER_TOML).unwrap();
        assert_eq!(p.grid_source(), GridSource::NoGridFetched);
    }

    #[test]
    fn apply_risk_uses_the_mode_derived_grid_source() {
        // `RunProfile::apply_risk` must route through the SAME derivation as `grid_source` — a
        // backtest/paper profile's own instrument fields take effect (NoGridFetched), while a
        // fresh `RiskLimits::new()` base for a live profile stays untouched (VenueFetched, and a
        // live profile can never carry instrument fields anyway per `validate`).
        let backtest = RunProfile::from_toml_str(BACKTEST_TOML).unwrap();
        let got = backtest.apply_risk(vike_exec::RiskLimits::new()).expect("backtest is always Ok");
        assert_eq!(got.tick_size, backtest.risk.tick_size);

        let live = RunProfile::from_toml_str(LIVE_TOML).unwrap();
        let got = live
            .apply_risk(vike_exec::RiskLimits::new())
            .expect("live with no instrument fields is Ok");
        assert_eq!(got.tick_size, None, "live has no instrument fields to begin with");
    }

    // ---------------------------------------------------------------------------------------------
    // RunProfile::risk_for_live_venue_mount — the BLOCKING-2(a) guard: a non-`live`-mode profile
    // must never reach a real 12-venue live mount, which cannot see `mode` at all.
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn risk_for_live_venue_mount_ok_for_a_live_profile() {
        let live = RunProfile::from_toml_str(LIVE_TOML).unwrap();
        let risk = live.risk_for_live_venue_mount().expect("mode = live must be accepted");
        assert_eq!(risk, &live.risk);
    }

    #[test]
    fn risk_for_live_venue_mount_rejects_a_backtest_profile() {
        let backtest = RunProfile::from_toml_str(BACKTEST_TOML).unwrap();
        let err = backtest
            .risk_for_live_venue_mount()
            .expect_err("a backtest-mode profile must never arm a live 12-venue mount");
        let ProfileError::Validation(m) = err else { panic!("expected Validation error") };
        assert!(m.contains("live"), "error must mention the mode requirement: {m}");
    }

    #[test]
    fn risk_for_live_venue_mount_rejects_a_paper_profile() {
        let paper = RunProfile::from_toml_str(PAPER_TOML).unwrap();
        let err = paper
            .risk_for_live_venue_mount()
            .expect_err("a paper-mode profile must never arm a live 12-venue mount");
        assert!(matches!(err, ProfileError::Validation(_)));
    }

    #[test]
    fn bad_max_drawdown_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[guards]
max_drawdown = 1.5
"#;
        assert_validation(toml, "`guards.max_drawdown` must be in (0.0, 1.0]");
    }

    /// ⚠ An ARMED drawdown latch with no capital base is worse than no latch at all: the profile
    /// reads as protected and `CoreThread::sweep_drawdown_latch` can never form the fraction, so it
    /// never trips. Refused at load, on a VENUE broker specifically — a paper broker's positive
    /// `seed_cash` is already required, and the venue arm is where "the exchange knows my balance,
    /// why would I declare capital" makes omitting it feel reasonable.
    #[test]
    fn max_drawdown_on_a_venue_broker_without_a_positive_seed_cash_rejected() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
interval = "1m"
[broker]
kind = "venue"
venue = "binance"
[risk]
max_notional_per_order = 100.0
max_total_exposure = 1000.0
[guards]
max_drawdown = 0.2
"#;
        assert_validation(toml, "needs a positive `broker.seed_cash`");
        // ...and the SAME profile with a capital base declared validates.
        let ok = toml.replace("kind = \"venue\"", "kind = \"venue\"\nseed_cash = 25000.0");
        RunProfile::from_toml_str(&ok).expect("a declared capital base arms the latch");
    }

    /// The converse: NO `guards.max_drawdown` means no latch, so a zero `seed_cash` on a venue
    /// broker is nobody's problem and must still load. Without this the guard above would be a
    /// silent tightening of every live profile in the wild.
    #[test]
    fn a_venue_broker_without_a_drawdown_guard_still_loads_with_no_seed_cash() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
interval = "1m"
[broker]
kind = "venue"
venue = "binance"
[risk]
max_notional_per_order = 100.0
max_total_exposure = 1000.0
"#;
        let p = RunProfile::from_toml_str(toml).expect("no latch armed ⇒ no capital base needed");
        assert_eq!(p.guards.max_drawdown, None);
        assert_eq!(p.broker.seed_cash, 0.0);
    }

    #[test]
    fn recorder_without_live_source_rejected() {
        let toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "paper"
seed_cash = 1.0
[sinks]
recorder = true
"#;
        assert_validation(toml, "`sinks.recorder` records a LIVE feed");
    }

    #[test]
    fn journal_snapshot_every_zero_rejected() {
        let toml = r#"
mode = "live"
[event_source]
kind = "live_venue"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
[sinks.journal]
dir = "data/journal"
snapshot_every = 0
"#;
        assert_validation(toml, "`sinks.journal.snapshot_every` must be >= 1");
    }

    // ---------------------------------------------------------------------------------------------
    // from_path.
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn from_path_reads_and_validates() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("vike_run_profile_{}.toml", std::process::id()));
        std::fs::write(&dir, PAPER_TOML).expect("write temp profile");
        let p = RunProfile::from_path(&dir).expect("from_path parses");
        assert_eq!(p.mode, Mode::Paper);
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn from_path_missing_file_is_io_error() {
        let err = RunProfile::from_path("does/not/exist/vike-nope.toml").unwrap_err();
        assert!(matches!(err, ProfileError::Io(_)), "got {err:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // choose_journal — the pure resolver behind journal_config_from_env (env-free, so tested here).
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn choose_journal_neither_is_none() {
        assert!(super::choose_journal(None, None, None).is_none());
    }

    #[test]
    fn choose_journal_dir_fallback_uses_default_cadence() {
        let jc = super::choose_journal(None, Some("data/journal".into()), None)
            .expect("a dir enables the journal");
        assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
        // JournalConfig::at defaults (mirror the [sinks.journal] defaults)
        assert_eq!(jc.file.segment_bytes, 64 * 1024 * 1024);
        assert_eq!(jc.file.flush_every, 256);
        assert_eq!(jc.snapshot_every, 1024);
    }

    #[test]
    fn choose_journal_dir_snapshot_override_applies() {
        let jc = super::choose_journal(None, Some("d".into()), Some(64)).unwrap();
        assert_eq!(jc.snapshot_every, 64);
        // a 0 override is ignored (0 would snapshot on every record — a p99 cliff the CoreThread asserts against)
        let jc0 = super::choose_journal(None, Some("d".into()), Some(0)).unwrap();
        assert_eq!(jc0.snapshot_every, 1024);
    }

    #[test]
    fn choose_journal_profile_is_authoritative() {
        // a LIVE profile carries a [sinks.journal] → that config wins, dir fallback ignored.
        let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
        let jc = super::choose_journal(Some(&p), Some("ignored/dir".into()), Some(7))
            .expect("profile journal sink present");
        assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
        assert_eq!(jc.snapshot_every, 1024); // from the profile, NOT the ignored override
    }

    #[test]
    fn choose_journal_profile_without_journal_sink_is_off_even_with_dir() {
        // a profile present but with no journal sink → OFF; the dir fallback must NOT resurrect it
        // (a present profile is authoritative, so it can never be silently overridden).
        let p = RunProfile::from_toml_str(PAPER_TOML).unwrap();
        assert!(p.sinks.journal.is_none());
        assert!(super::choose_journal(Some(&p), Some("data/journal".into()), None).is_none());
    }

    // ---------------------------------------------------------------------------------------------
    // resolve_profile — the INJECTED-vars resolver (never reads std::env::var itself).
    // ---------------------------------------------------------------------------------------------

    /// Write `toml` to a fresh temp file and return its path (caller cleans up).
    fn write_temp_profile(tag: &str, toml: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("vike_resolve_profile_{}_{tag}_{}.toml", std::process::id(), tag));
        std::fs::write(&p, toml).expect("write temp profile");
        p
    }

    #[test]
    fn resolve_profile_no_explicit_no_env_is_ok_none() {
        // Neither an explicit path nor VIKE_RUN_PROFILE in the injected vars → Ok(None), and
        // critically NOT an Err — a run with no profile configured must proceed untouched.
        let vars: HashMap<String, String> = HashMap::new();
        let got = resolve_profile(None, &vars).expect("no profile configured is not an error");
        assert!(got.is_none(), "expected no profile resolved, got {got:?}");
    }

    #[test]
    fn resolve_profile_explicit_wins_over_env() {
        // The env var points at a profile that would fail validation (backtest + venue broker is
        // rejected — see `backtest_with_venue_broker_rejected` above), while the explicit path
        // points at a good one. If explicit did NOT win, this would return Err, not the backtest
        // sample — so this test fails loudly under either a swapped precedence OR an ignored
        // explicit path.
        let good = write_temp_profile("explicit-good", BACKTEST_TOML);
        let bad_toml = r#"
mode = "backtest"
[event_source]
kind = "hist"
venue = "binance"
symbol = "BTCUSDT"
[broker]
kind = "venue"
venue = "binance"
"#;
        let bad = write_temp_profile("env-bad", bad_toml);

        let mut vars = HashMap::new();
        vars.insert("VIKE_RUN_PROFILE".to_string(), bad.display().to_string());

        let got = resolve_profile(Some(&good), &vars)
            .expect("explicit path must be used, not the broken env path")
            .expect("a profile must resolve");
        assert_eq!(got.mode, Mode::Backtest);
        assert_eq!(got.event_source.venue, "binance");

        let _ = std::fs::remove_file(&good);
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn resolve_profile_env_used_when_no_explicit_path() {
        // No explicit path → the injected VIKE_RUN_PROFILE entry is used.
        let path = write_temp_profile("env-only", PAPER_TOML);
        let mut vars = HashMap::new();
        vars.insert("VIKE_RUN_PROFILE".to_string(), path.display().to_string());

        let got = resolve_profile(None, &vars)
            .expect("env-resolved profile must load")
            .expect("a profile must resolve");
        assert_eq!(got.mode, Mode::Paper);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolve_profile_missing_explicit_file_is_err_not_none() {
        // A typo'd/deleted profile path must be a loud Err, never a silent Ok(None) — the dangerous
        // failure mode this function exists to close off.
        let vars: HashMap<String, String> = HashMap::new();
        let bogus = Path::new("does/not/exist/vike-resolve-profile-nope.toml");
        let err = resolve_profile(Some(bogus), &vars)
            .expect_err("a missing explicit profile path must be an Err");
        assert!(matches!(err, ProfileError::Io(_)), "expected Io error, got {err:?}");
        assert!(
            err.to_string().contains("vike-resolve-profile-nope.toml"),
            "error should name the missing file: {err}"
        );
    }

    #[test]
    fn resolve_profile_missing_env_file_is_err_not_none() {
        // Same dangerous-failure-mode guard, but reached via the env-var path rather than explicit.
        let mut vars = HashMap::new();
        vars.insert(
            "VIKE_RUN_PROFILE".to_string(),
            "does/not/exist/vike-resolve-profile-env-nope.toml".to_string(),
        );
        let err = resolve_profile(None, &vars)
            .expect_err("a missing env-resolved profile path must be an Err");
        assert!(matches!(err, ProfileError::Io(_)), "expected Io error, got {err:?}");
    }

    #[test]
    fn resolve_profile_malformed_toml_is_err_naming_the_file() {
        // A malformed TOML file must fail with a message that names the offending file, so an
        // operator staring at a startup failure knows exactly which file to fix.
        let path = write_temp_profile("malformed", "this is not = = toml [[[");
        let mut vars = HashMap::new();
        vars.insert("VIKE_RUN_PROFILE".to_string(), path.display().to_string());

        let err = resolve_profile(None, &vars).expect_err("malformed TOML must be an Err");
        assert!(matches!(err, ProfileError::Parse(_)), "expected Parse error, got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains(&path.display().to_string()),
            "error message must name the offending file {path:?}: {msg}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolve_profile_path_precedence_is_pure() {
        // The precedence helper itself: explicit > env > None, with no filesystem access — proven
        // directly so a future edit to resolve_profile can't silently invert the rule undetected.
        let mut vars = HashMap::new();
        vars.insert("VIKE_RUN_PROFILE".to_string(), "from/env.toml".to_string());

        assert_eq!(
            super::resolve_profile_path(Some(Path::new("from/explicit.toml")), &vars),
            Some(PathBuf::from("from/explicit.toml"))
        );
        assert_eq!(super::resolve_profile_path(None, &vars), Some(PathBuf::from("from/env.toml")));
        assert_eq!(super::resolve_profile_path(None, &HashMap::new()), None);
    }

    // NOTE: the direct `ProfileRisk::apply_to` unit tests (the venue-grid / operator-budget split)
    // MOVED with the type to `vike_exec::risk_profile`'s own `#[cfg(test)]` module — see that
    // module for `apply_to_operator_only_leaves_venue_fields_byte_identical`,
    // `apply_to_each_venue_owned_field_is_config_error_when_grid_fetched`,
    // `apply_to_names_every_offending_field_in_one_error`,
    // `apply_to_no_grid_fetched_profile_supplies_instrument_fields`,
    // `apply_to_no_grid_fetched_ignores_bases_venue_fields`,
    // `apply_to_matches_to_risk_limits_when_no_grid_fetched`, and
    // `apply_to_preserves_fields_neither_side_owns`. This module keeps only the RunProfile-level
    // integration coverage (`apply_risk_uses_the_mode_derived_grid_source`, the live-mode
    // structural gate tests, `grid_source_from_*`), which exercises the re-export end-to-end.
}
