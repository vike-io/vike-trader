//! `RunProfile` — the single TOML-deserializable config artifact that names a full runtime
//! assembly (sinks, guards, validator/risk stack) so that
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
//! the same hoist shape [`vike_model::money::sizing`] established for
//! `units_from_percent`/`units_from_value`. See [`vike_exec::risk_profile`]'s module doc for the
//! "two owners, one struct" rule and the compile-checked `to_risk_limits`/`apply_to` drift alarm;
//! what stays HERE is `RunProfile` itself (the mode/sinks/guards schema) and
//! the `mode`-derived [`RunProfile::grid_source`] / [`RunProfile::apply_risk`] glue.
//!
//! ## Scope (audit co14) — the schema and loader, and the glue a binary calls
//!
//! This module does NOT change [`crate::CoreConfig`]'s shape: a binary builds its own and the glue
//! here ([`RunProfile::apply_risk`], [`RunProfile::apply_guards_and_sinks`]) overwrites only the
//! fields a profile names. ⚠ It said it was "NOT yet wired into any binary … an explicit
//! follow-up" until 2026-09-28. `[risk]` was wired in #816 (2026-07-28) and `[guards]`/`[sinks]`
//! in #1487 (2026-08-23); `vike-tradehub` is the root that resolves and applies a profile today.
//! What ships here:
//!   - the [`RunProfile`] type tree (serde `Deserialize`, TOML),
//!   - a loader ([`RunProfile::from_toml_str`] / [`RunProfile::from_path`]) with semantic
//!     [`RunProfile::validate`]ation returning a clear [`ProfileError`],
//!   - [`resolve_profile`] — the shared `--profile`/`VIKE_RUN_PROFILE` resolver a binary's wiring
//!     step calls: an INJECTED `vars` map (never `std::env::var` itself, so it stays the
//!     `Layer::Injected` seam `vike-ops`' settings registry gate wants), explicit-beats-env
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
//! ⚠ **`broker.seed_cash` → [`crate::CoreConfig::seed_cash`] stood first in this list and was
//! never true.** No runner carried that field into a `CoreConfig` in either binary, and the whole
//! `[broker]` table went with `[event_source]` for that reason — a mapping line stating a
//! destination nothing reaches is exactly the positive-confirmation-of-something-false this
//! workspace deletes settings for. [`RunProfile::broker`]'s tombstone carries the argument and the
//! refusal; the daemon's seed comes from its own `[[mounts]]` row.
//!
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

/// Top-level run mode. It is the profile's one structural switch: [`RunProfile::grid_source`]
/// derives from it, and [`RunProfile::validate`] refuses a `live` profile that sets any
/// venue-owned `[risk]` field. (It used to ALSO cross-check `event_source` + `broker`; those two
/// tables are deleted — [`RunProfile::event_source`].)
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

// ⚠ `EventSourceKind` and `BrokerKind` stood here and are DELETED with the two tables they typed
// (`RunProfile::event_source`'s doc carries the argument). They were reachable only through those
// fields and only inside `RunProfile::validate`, so nothing outside this file lost a type.

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

// ⚠ `EventSource` and `Broker` stood here and are DELETED — see [`RunProfile::event_source`] and
// [`RunProfile::broker`], the two tombstone fields that refuse the tables by name.

/// `[sinks.journal]` — the opt-in write-ahead command journal. Maps 1:1 to [`crate::JournalConfig`]
/// (`dir` + [`vike_journal::JournalFileConfig`] + `snapshot_every`).
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
            file: vike_journal::JournalFileConfig {
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
// GridSource}` keeps working unchanged (the same hoist shape `vike_model::money::sizing` established for
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
    /// **TOMBSTONE — `[event_source]` is DELETED, and REFUSED rather than ignored.** Not a
    /// configuration field: it is parsed only so [`RunProfile::validate`] can refuse the table BY
    /// NAME, which is the whole of what this field is for.
    ///
    /// It was schema-REQUIRED, validated against `mode`, documented in the shipped template — and
    /// read by no runner in either binary. `docs/ops/run-profile-live.toml` said so at the table
    /// itself: *"Schema-required, but IGNORED by the multi-venue live mount … No runner does, in
    /// either binary."* A key an operator fills in truthfully and nothing consults is the
    /// `Policy::max_total_exposure` shape this workspace deletes fields for, one level down.
    ///
    /// ⚠ Deleting it is NOT free, which is why it is a tombstone rather than an absence:
    /// `deny_unknown_fields` turns a surviving `[event_source]` into an *"unknown field"* startup
    /// failure that reads like a typo. [`serde::de::IgnoredAny`] accepts whatever shape the table
    /// had — the point is presence, never content — so an operator who never edited their profile
    /// gets a sentence saying what happened instead of a parser complaining about a word.
    #[serde(default)]
    pub event_source: Option<serde::de::IgnoredAny>,
    /// **TOMBSTONE — `[broker]` is DELETED, and REFUSED rather than ignored.** See
    /// [`RunProfile::event_source`]; the two go together and for the same reason.
    ///
    /// ⚠ Its `seed_cash` looked load-bearing and was not. `RunProfile::validate` required a
    /// positive one whenever `guards.max_drawdown` was set — the drawdown latch's denominator —
    /// but nothing ever carried this number into a [`crate::CoreConfig::seed_cash`]: the daemon's
    /// seed comes from its own `[[mounts]]` row (`vike_tradehub::config::MountCfg`'s `seed_cash`,
    /// which refuses a non-positive value on the same grounds, at the place the value is actually
    /// read). So the check was guarding the latch with a number the latch never saw.
    #[serde(default)]
    pub broker: Option<serde::de::IgnoredAny>,
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
    /// feeds a live 12-venue mount — `vike-tradehub`'s live arm is the one today. (It also named
    /// `vike-app`'s `App::new` until 2026-09-28; the desktop has mounted no venue since #1727.)
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

        // --- the two DELETED tables, refused BY NAME --------------------------------------------
        //
        // ⚠ FIRST, before anything this profile actually configures. `deny_unknown_fields` would
        // refuse these two on its own the moment the fields went, and the refusal it produces says
        // only "unknown field" — which reads as a typo to the one operator who is not making one.
        // [`RunProfile::event_source`] and [`RunProfile::broker`] carry the argument; this is the
        // sentence they produce.
        for (table, present) in
            [("event_source", self.event_source.is_some()), ("broker", self.broker.is_some())]
        {
            if present {
                return bail(format!(
                    "`[{table}]` is no longer part of a run profile — it was REQUIRED by this \
                     schema, validated on load and documented in the shipped template, and NO \
                     RUNNER read it, in either binary. Keeping it would have handed you positive \
                     confirmation of something false: an `event_source`/`broker` you filled in \
                     truthfully never selected a feed or an execution target. Delete the whole \
                     `[{table}]` table. What actually decides those: the daemon's `[[mounts]]` \
                     rows pick the venue, symbol and account, and the live gate decides whether \
                     the mount is paper or real. This profile's `mode` and `[risk]`/`[guards]`/\
                     `[sinks]` tables are unchanged"
                ));
            }
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
        // ⚠ A second `if` arm stood inside this one and went with `[broker]`, and what it was
        // protecting is written here so the next reader does not conclude the protection was lost.
        // It required a positive `broker.seed_cash` whenever `max_drawdown` was set: the latch
        // measures a FRACTION of `Σ seed_cash + own PnL` (`CoreThread::sweep_drawdown_latch`), so a
        // non-positive seed leaves it with no denominator and it can never arm — a profile that
        // reads as protected and silently is not. That refusal was made against a number NO RUNNER
        // ever carried into `CoreConfig::seed_cash`: the table was read by nothing.
        //
        // The check that matters survives, at the place the seed is actually read —
        // `vike_tradehub::config::MountCfg`'s `seed_cash`, whose own refusal names the same 25%
        // latch and the same denominator. So what is deleted here is a load-time check over an
        // input with no consumer, not the guard.
        if let Some(dd) = g.max_drawdown
            && !is_frac_unit(dd)
        {
            return bail(format!(
                "`guards.max_drawdown` must be in (0.0, 1.0] (omit to disable, got {dd})"
            ));
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
        // ⚠ A `sinks.recorder` needs-a-live-feed check stood HERE and went with `[event_source]`:
        // it compared one UNWIRED key against another. `sinks.recorder` is itself on this type's
        // own unwired list ([`RunProfile::guards_report`] pushes it), so the operator who sets it
        // is already told by NAME that it arms nothing — which is a stronger and truer answer than
        // a cross-check between two keys neither of which reaches a runner. Re-deriving the rule
        // from `mode` was available and refused: `mode = "paper"` says nothing about whether the
        // feed is live, so a mode-based rule would be a DIFFERENT rule wearing the old one's name.
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
/// (the `vike-mount` cores; `vike-tradehub` resolves the same three rungs over its own map through
/// [`journal_config_from`], and the desktop — named here as `vike-app` until 2026-09-28 — builds
/// no core at all). OFF by default so a standard mount stays zero-overhead and byte-identical —
/// journaling is enabled only when one of these is set:
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
    journal_config_from(&journal_env_snapshot())
}

/// This function's three variables, read from the process env as THREE explicit `env::var` calls.
///
/// ⚠ Deliberately not a `std::env::vars()` sweep, and the reason is the settings registry rather
/// than taste: `crates/vike-ops/tests/settings_registry.rs` resolves a read by CALL SITE, so a bulk
/// sweep here would leave three declared rows with no resolvable read and the gate would call them
/// stale. `vike_data::PropertiesRecorder`'s `env_snapshot` is the same shape for the same reason and
/// says so in its own doc.
fn journal_env_snapshot() -> HashMap<String, String> {
    [
        ("VIKE_RUN_PROFILE", std::env::var("VIKE_RUN_PROFILE")),
        ("VIKE_JOURNAL_DIR", std::env::var("VIKE_JOURNAL_DIR")),
        ("VIKE_JOURNAL_SNAPSHOT_EVERY", std::env::var("VIKE_JOURNAL_SNAPSHOT_EVERY")),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.ok().map(|v| (k.to_string(), v)))
    .collect()
}

/// [`journal_config_from_env`] over a CALLER-SUPPLIED map — the same three variables, the same
/// precedence, and no process-environment read of its own.
///
/// This exists because `config.journal_dir` was a declared settings key nothing could reach:
/// `VIKE_JOURNAL_DIR` is read here, in a library, several frames below the binaries that own the
/// settings sweep, and `vike_config::CONSUMPTION` carried that as a written admission. The fix is
/// the one this workspace prefers everywhere — the value arrives as a PARAMETER and the BINARY
/// does the I/O.
///
/// ⚠ **The caller decides precedence by what it puts in the map, and the rule is unchanged: the
/// ENVIRONMENT wins.** `crates/vike-tradehub/src/tradehub_cli.rs`'s `journal_vars` is the worked
/// example — it starts from the real process env and inserts the `config.journal_dir` setting only
/// where the variable is ABSENT, so an `Environment=` line in a unit beats the `config.journal_dir`
/// row exactly as it did when this function could only read the environment.
///
/// ⚠ `VIKE_RUN_PROFILE` still short-circuits the whole thing, and a profile that names no
/// `[sinks].journal` still means "journaling off". That ordering predates this seam and is not a
/// consequence of it; [`journal_config_from_env`]'s doc above is the authority for all three rungs.
#[must_use]
pub fn journal_config_from(vars: &HashMap<String, String>) -> Option<crate::JournalConfig> {
    if let Some(path) = vars.get("VIKE_RUN_PROFILE") {
        return match RunProfile::from_path(path) {
            Ok(p) => choose_journal(Some(&p), None, None),
            Err(e) => {
                tracing::warn!(
                    "VIKE_RUN_PROFILE set but the profile did not load ({e}); journaling disabled"
                );
                None
            }
        };
    }
    let dir = vars.get("VIKE_JOURNAL_DIR")?;
    let snapshot_every =
        vars.get("VIKE_JOURNAL_SNAPSHOT_EVERY").and_then(|s| s.parse::<u64>().ok());
    choose_journal(None, Some(std::path::PathBuf::from(dir)), snapshot_every)
}

/// Pick which profile PATH wins: an explicit path (e.g. a `--profile` CLI flag) beats
/// `vars["VIKE_RUN_PROFILE"]`, which beats nothing. Pure precedence only — no filesystem access —
/// so it is the one place both [`resolve_profile`] and any caller that needs just the path (rather
/// than a fully loaded profile) can share the SAME rule instead of re-deriving it. `pub` (rather than
/// crate-private) specifically so the `incident` bin (vike-run's then, `vike-mount`'s now) — which
/// independently re-derived this exact `--profile`-over-`VIKE_RUN_PROFILE` precedence before this
/// function existed — can call this
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
/// `warn!`; this general-purpose resolver is for callers that must abort startup on a bad profile
/// rather than silently downgrade — `vike-tradehub` is the one today, at startup in
/// `crates/vike-tradehub/src/tradehub_cli.rs`. (It named "the coming vike-tradehub / vike-app
/// wiring" until 2026-09-28: the daemon's wiring has landed, and the desktop builds no core.)
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
/// test binary. Each is validated by `run_profile_tests::all_samples_validate`.
pub mod samples {
    /// A deterministic offline backtest: hist bars + paper broker.
    pub const BACKTEST_TOML: &str = r#"
name = "btcusdt-1m-backtest"
mode = "backtest"



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

#[path = "consumption_gate.rs"]
#[cfg(test)]
mod consumption_gate;

#[path = "run_profile_tests.rs"]
#[cfg(test)]
mod run_profile_tests;
