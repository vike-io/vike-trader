//! `RunProfile` — the single TOML-deserializable config artifact that names a full runtime
//! assembly (sinks, guards, validator/risk stack) so that
//! backtest / paper / live become ONE auditable file instead of code-only [`crate::CoreConfig`]
//! construction by each binary/test.
//!
//! ## `ProfileRisk` / `GridSource` / `ProfileError` live in `vike-model`
//!
//! These three types were HOISTED down (runprofile-wiring-step2, then to vike-model beside
//! [`vike_model::RiskLimits`] itself), so `vike-backtest` — which sits ALONGSIDE this crate, not
//! beneath it, and so cannot depend on it — can populate `EngineParams.risk_limits` from the SAME
//! TOML `[risk]` converter paper and live use, rather than growing a second one that drifts. This
//! crate does NOT re-export them: every caller names `vike_model::{ProfileRisk, GridSource,
//! ProfileError}` (`docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`). See
//! `crates/vike-model/src/risk/profile.rs`'s module doc for the "two owners, one struct" rule and
//! the compile-checked `to_risk_limits`/`apply_to` drift alarm; what stays HERE is `RunProfile`
//! itself (the mode/sinks/guards schema) and the `mode`-derived [`RunProfile::grid_source`] /
//! [`RunProfile::apply_risk`] glue.
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
//!   - a loader ([`RunProfile::from_toml_str`]) with semantic [`RunProfile::validate`]ation
//!     returning a clear [`ProfileError`],
//!   - three sample profiles ([`samples`]).
//!
//! ⚠ **No profile FILE is read here, and no binary reads one** (decision 0111, verdict 4). The
//! daemon's run profile is the ACTIVE `run` row of the settings database: `vike-tradehub` renders
//! the row's body (`vike_secrets::profile_store::render_run_toml`) and hands the TEXT to
//! [`RunProfile::from_toml_str`], so every refusal below applies to a row exactly as it did to a
//! file. The file loader, the `--profile` flag and `VIKE_RUN_PROFILE` are gone; `vike-cli config
//! bootstrap-run` writes the rows.
//!
//! ## How it maps to the real runtime knobs
//!
//! The `[risk]`, `[guards]`, and `[sinks.journal]` sections mirror fields that ALREADY exist on
//! [`crate::CoreConfig`] / [`vike_model::RiskLimits`] / [`vike_exec::MarginCallConfig`]. The
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
//! - `[risk]` → [`vike_model::RiskLimits`] (the pre-trade gate config).
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
//! [`vike_model::RiskLimits`] holds two conceptually separate concerns in one struct:
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
//!   - **A reference risk engine** checks instrument-level limits (`min_quantity`/
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
//! We only face a "who wins" question at all because `vike_model::RiskLimits` is ONE struct
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

mod confirm_grace;
mod journal_env;
mod schema;
mod validate;

pub use confirm_grace::{ConfirmGraceHazard, GuardsReport};
pub use journal_env::journal_config_from;
pub use schema::{
    Guards, Mode, ProfileJournal, ProfileMarginCall, ProfileTradingState, RunProfile, Sinks,
};

// `ProfileRisk` / `GridSource` / `ProfileError` are vike-model's (the module doc says why): named
// here, never re-exported. `GridSource` and `ProfileError` are named only by docs and tests (the
// loaders that return `ProfileError` live in `validate.rs` and `schema.rs`), `ProfileRisk` only by
// docs.
#[cfg(doc)]
use vike_model::ProfileRisk;
#[cfg(any(test, doc))]
use vike_model::{GridSource, ProfileError};

/// Three ready-to-parse sample profiles — one per [`Mode`]. Shipped as string constants (audit
/// co14: "as string constants or test fixtures") so they double as documentation and stay in the
/// test binary. Each is validated by `profile_tests::all_samples_validate`.
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

#[cfg(test)]
use confirm_grace::{confirm_budget, confirm_grace_clears_bound};
#[cfg(test)]
use journal_env::choose_journal;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
mod consumption_gate;

#[cfg(test)]
mod profile_tests;
