//! `RunProfile` — the single TOML-deserializable config artifact that names a full runtime
//! assembly (sinks, guards, validator/risk stack) as ONE auditable document instead of code-only
//! `vike_core::CoreConfig` construction by each binary/test.
//!
//! ⚠ **It states no KIND of run and no tier** (decision 0156 item 7). Its `mode` field
//! (`backtest`/`paper`/`live`) is deleted: the kind of run is the program that runs (`vike-cli
//! backtest`, with or without its `[walkforward]` table, or the `vike-tradehub` daemon), and where
//! a forward run's orders go is the tier of the account each mount runs on (`account.tier`). A
//! stored `mode` row is refused like any key the type does not have (the deny policy below).
//!
//! ## `ProfileRisk` / `GridSource` / `ProfileError` live in `vike-model`
//!
//! These three types were HOISTED down (runprofile-wiring-step2, then to vike-model beside
//! [`vike_model::RiskLimits`] itself), so `vike-backtest` — which sat ALONGSIDE `vike-core`, where this
//! module lived until decision 0157, not beneath it, and so could not depend on it — can populate `EngineParams.risk_limits` from the SAME
//! TOML `[risk]` converter paper and live use, rather than growing a second one that drifts. This
//! crate does NOT re-export them: every caller names `vike_model::{ProfileRisk, GridSource,
//! ProfileError}` (`docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`). See
//! `crates/vike-model/src/risk/profile.rs`'s module doc for the "two owners, one struct" rule and
//! the compile-checked `to_risk_limits`/`apply_to` drift alarm; what stays HERE is `RunProfile`
//! itself (the sinks/guards schema) and the tier-keyed [`RunProfile::apply_risk`] glue.
//!
//! ## Scope (audit co14) — the schema and loader; the core glue lives in `vike-mount`
//!
//! This module is the DOCUMENT: the type tree, the loader, the validation and the tier-keyed
//! [`RunProfile::apply_risk`]. It names no `vike-core` or `vike-exec` type (decision 0157 moved it
//! here, down from `vike-core`, so an edit to the run profile no longer fires the core-hop latency
//! gate). What writes a profile into a `vike_core::CoreConfig` — `vike_mount::apply_guards_and_sinks`,
//! `vike_mount::journal_config_from` — is `vike-mount`'s
//! `profile_apply`: a binary builds its own `CoreConfig` and that glue overwrites only the
//! fields a profile names. ⚠ It said it was "NOT yet wired into any binary … an explicit
//! follow-up" until 2026-09-28. `[risk]` was wired in #816 (2026-07-28) and `[guards]`/`[sinks]`
//! in #1487 (2026-08-23); `vike-tradehub` is the root that resolves and applies a profile today.
//! What ships here:
//!   - the [`RunProfile`] type tree (serde `Deserialize`, TOML),
//!   - a loader ([`RunProfile::from_toml_str`]) with semantic [`RunProfile::validate`]ation
//!     returning a clear [`ProfileError`],
//!   - three sample profiles as STORED ROWS ([`samples`]), loaded through [`rows_to_run_profile`],
//!     the path the daemon takes.
//!
//! ⚠ **No profile FILE is read here, and no binary reads one** (decision 0111, verdict 4). The
//! daemon's run profile is the ACTIVE `run` row of the settings database, and [`rows_to_run_profile`]
//! is its ONE reader: it renders the row's body (`vike_secrets::profile_store::render_run_toml`) and
//! hands the TEXT to [`RunProfile::from_toml_str`], so every refusal below applies to a row. The file
//! loader, the `--profile` flag and `VIKE_RUN_PROFILE` are gone; `vike-cli config bootstrap-run`
//! writes the rows.
//!
//! ## How it maps to the real runtime knobs
//!
//! The `[risk]`, `[guards]`, and `[sinks.journal]` sections mirror fields that ALREADY exist on
//! `vike_core::CoreConfig` / [`vike_model::RiskLimits`] / `vike_model::MarginCallConfig`. The
//! converters ([`ProfileRisk::to_risk_limits`], [`Guards::submit_ack_timeout`], … here;
//! `vike_mount::tune_journal` in `vike-mount`) return or write those REAL types, so the mapping is
//! compile-checked against them — adding a required field to `RiskLimits` breaks the build in the
//! crate that owns the converter, which is the intended drift alarm. `[guards.margin_call]` needs no
//! converter at all: the profile parses it AS `vike_model::MarginCallConfig`. The mapping,
//! field-by-field:
//!
//! ⚠ **`broker.seed_cash` → `vike_core::CoreConfig::seed_cash` stood first in this list and was
//! never true.** No runner carried that field into a `CoreConfig` in either binary, and the whole
//! `[broker]` table went with `[event_source]` for that reason — a mapping line stating a
//! destination nothing reaches is exactly the positive-confirmation-of-something-false this
//! workspace deletes settings for. [`RunProfile::broker`]'s tombstone carries the argument and the
//! refusal; the daemon's seed comes from its own `[[mounts]]` row.
//!
//! - `[risk]` → [`vike_model::RiskLimits`] (the pre-trade gate config).
//! - `guards.initial_trading_state` → [`vike_model::TradingState`], sent to the spawned core as a
//!   `SetTradingState` command by `vike-tradehub` on both its arms when it is not `active`
//!   (`halted` = the gate denies every new order). A stricter state the JOURNAL restored is never
//!   loosened by it (`vike_mount::initial_trading_state_to_send`).
//! - `guards.submit_ack_timeout_ms` → `vike_core::CoreConfig::submit_ack_timeout` (stuck-order watchdog stage 1).
//! - `guards.submit_ack_confirm_grace_ms` → `vike_core::CoreConfig::submit_ack_confirm_grace` (stage 2).
//! - `guards.max_drawdown` → `vike_core::CoreConfig::max_drawdown` (equity-drawdown liquidate-only latch).
//! - `guards.margin_call` → `vike_core::CoreConfig::margin_call` (`vike_model::MarginCallConfig`).
//! - `guards.conditionals_on_ticks` → `vike_core::CoreConfig::conditionals_on_ticks`.
//! - `[sinks.journal]` → the TUNING of `vike_core::JournalConfig` (segment size, flush and snapshot
//!   cadence), laid over the `config.journal_dir` row by `vike_mount::journal_config_from`. The directory,
//!   and whether there is a journal at all, are that row's alone (decision 0111, amended
//!   2026-10-11); the retired `dir` key is refused by name ([`ProfileJournal::dir`]).
//! - `sinks.equity_sample_ms` → `vike_core::CoreConfig::equity_sample`, the portfolio-equity sampler
//!   cadence, applied by `vike_mount::apply_guards_and_sinks`. ⚠ It said "schema-only today (no
//!   `CoreConfig` knob yet)" here after the knob existed and the key was wired — the same stale claim
//!   [`Sinks::equity_sample_ms`] already corrected on the field.
//!
//! ## No key is accepted and applied by nothing
//!
//! Every key the type declares reaches a mount. Five once did not (`guards.initial_trading_state`,
//! `guards.freshness_ms`, `sinks.gui`, `sinks.recorder`, `sinks.raw_capture_dir`) and were disclosed
//! by a table of "accepted, not applied" keys; that table is gone with its last row. The trading
//! state is WIRED (see its mapping above). The other four are RETIRED, and a stored body that still
//! carries one is REFUSED BY NAME — by [`RunProfile::validate`], so at the daemon's load and at
//! `vike-cli config activate` alike — with the `bootstrap-run` re-run that drops the key
//! ([`Sinks::gui`], [`Sinks::recorder`], [`Sinks::raw_capture_dir`], [`Guards::freshness_ms`] carry
//! the tombstones; [`ProfileJournal::dir`] is the precedent). Decision 0084 is why recording and
//! raw capture are not run-profile keys: the datahub is the store's one writer. No migration reads
//! or rewrites the old rows (decision 0117).
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
//! (a `paper` account, or the `make_engine` permissive-default fallback after a fetch failure) —
//! is gated on the caller passing [`GridSource::NoGridFetched`], never inferred from a field
//! happening to be `None`.
//!
//! ## `GridSource` is derived from the mounted account's TIER, not from the profile
//!
//! An earlier revision of this module added `ProfileRisk::allow_instrument_override` — a
//! load-time opt-in flag. That was wrong: whether a grid was fetched already answers "may this
//! profile supply the instrument grid?", and a second knob made an invalid state expressible (a
//! profile that opts into overriding the venue's real grid). The flag was deleted. Whether a grid
//! is fetched is a property of the ACCOUNT a mount runs on (decision 0156 item 7): a `paper`
//! account is the paper simulator and fetches none, a `demo`/`live` account mounts the venue's.
//! `vike_model::GridSource::for_tier` is the one mapping, and [`RunProfile::apply_risk`] — the ONE
//! path a caller holding a full `RunProfile` uses to reach [`ProfileRisk::apply_to`] — takes the
//! tier, never a `GridSource`, so a caller cannot pass a source that contradicts its account. A
//! profile that sets a venue-owned field is refused at the mount of every `demo`/`live` account it
//! reaches; until 0156 it was ALSO refused at load for a `mode = "live"` profile, which a profile
//! with no tier can no longer be.

mod rows;
mod schema;
mod validate;

pub use rows::rows_to_run_profile;
pub use schema::{Guards, ProfileJournal, ProfileTradingState, RunProfile, Sinks};

// `ProfileRisk` / `GridSource` / `ProfileError` are vike-model's (the module doc says why): named
// here, never re-exported. `ProfileError` is named only by docs and tests (the loaders that return
// it live in `validate.rs` and `schema.rs`), `ProfileRisk` and `GridSource` only by docs, and
// `AccountTier` (the tier `RunProfile::apply_risk` takes) only by the tests.
#[cfg(test)]
use vike_model::AccountTier;
#[cfg(any(test, doc))]
use vike_model::ProfileError;
#[cfg(doc)]
use vike_model::{GridSource, ProfileRisk};

/// Three sample profiles, written the way an operator STORES one: `profile_setting` rows, a dotted
/// path and the TOML scalar of its value (what `vike-cli config bootstrap-run` writes). They are
/// loaded through [`rows_to_run_profile`], the path the daemon takes, so a sample proves the stored
/// shape and not a document no binary reads. Each is validated by
/// `profile_tests::all_samples_validate`. A sample is named for the run it was written for; the
/// profile itself states no kind of run and no tier (the module doc), so which accounts a sample
/// suits is said beside it.
pub mod samples {
    use std::collections::BTreeMap;

    use vike_model::ProfileError;
    use vike_secrets::profile_store::{ProfileKind, ProfileRow, StoredProfile};

    use super::{RunProfile, rows_to_run_profile};

    /// A sample as `(path, TOML scalar)` rows.
    pub type Rows = &'static [(&'static str, &'static str)];

    /// What loading a sample returns, so a test can `.expect(..)` it as it did the old TOML sample.
    pub type Loaded = Result<RunProfile, ProfileError>;

    /// A minimal body that supplies the instrument grid itself (`tick_size`/`lot_size`/
    /// `min_notional`), so it suits `paper` accounts only: a `demo`/`live` account mounts the
    /// venue's grid and refuses those fields.
    pub const BACKTEST_ROWS: Rows = &[
        ("name", "\"btcusdt-1m-backtest\""),
        ("risk.tick_size", "0.1"),
        ("risk.lot_size", "0.001"),
        ("risk.min_notional", "5.0"),
        ("risk.max_notional_per_order", "250000.0"),
        ("guards.max_drawdown", "0.25"),
    ];

    /// Paper-trade a LIVE feed. Like [`BACKTEST_ROWS`] it supplies the instrument grid, so it suits
    /// `paper` accounts only. (It once also asked for the feed to be recorded to the history store
    /// with `sinks.recorder`; that key is retired — recording is a recorder profile run inside the
    /// datahub — so the sample no longer carries it, nor the two other retired keys it set.)
    ///
    /// ⚠ The two watchdog keys are a PAIR, and the pair must satisfy HARD LOWER BOUND (a) on
    /// `vike_core::CoreConfig::submit_ack_confirm_grace`: `2·grace > submit_ack_timeout/2 +
    /// N·requery`. 30000/15000 gives `2·15 = 30s > 15 + 2·5 = 25s` for a two-hop (Bybit) confirm.
    /// This sample shipped `5000` here — `2·5 = 10s`, which VIOLATES the bound — for as long as
    /// the constant it was copied from was stale. Omitting the grace key entirely is also correct:
    /// it then keeps whatever `CoreConfig` holds, which is this same 15 s.
    pub const PAPER_ROWS: Rows = &[
        ("name", "\"btcusdt-paper-live-feed\""),
        ("risk.tick_size", "0.1"),
        ("risk.lot_size", "0.001"),
        ("risk.min_notional", "5.0"),
        ("risk.max_notional_per_order", "50000.0"),
        ("risk.max_total_exposure", "200000.0"),
        ("risk.max_orders_per_window", "20"),
        ("risk.window_ms", "1000"),
        ("risk.max_leverage", "10.0"),
        ("risk.required_free_bp_pct", "0.05"),
        ("risk.block_reduce_only_overshoot", "true"),
        ("guards.submit_ack_timeout_ms", "30000"),
        ("guards.submit_ack_confirm_grace_ms", "15000"),
        ("guards.max_drawdown", "0.20"),
        ("guards.conditionals_on_ticks", "true"),
    ];

    /// For `demo`/`live` accounts: no instrument grid (the venue owns it), the journal tuning and
    /// the full guard stack (incl. the margin-call watchdog).
    ///
    /// ⚠ The two watchdog keys are a PAIR — see [`PAPER_ROWS`] for the bound they satisfy. This is
    /// the LIVE sample, so the violation the old `5000` encoded was a phantom-reject window on a
    /// real order, copied by every operator who started from it.
    pub const LIVE_ROWS: Rows = &[
        ("name", "\"btcusdt-live-binance-demo\""),
        ("sinks.equity_sample_ms", "1000"),
        ("sinks.journal.segment_bytes", "67108864"),
        ("sinks.journal.flush_every", "256"),
        ("sinks.journal.snapshot_every", "1024"),
        ("risk.max_notional_per_order", "50000.0"),
        ("risk.max_total_exposure", "200000.0"),
        ("risk.max_orders_per_window", "20"),
        ("risk.window_ms", "1000"),
        ("risk.max_leverage", "5.0"),
        ("risk.required_free_bp_pct", "0.05"),
        ("risk.block_reduce_only_overshoot", "true"),
        ("guards.initial_trading_state", "\"active\""),
        ("guards.submit_ack_timeout_ms", "30000"),
        ("guards.submit_ack_confirm_grace_ms", "15000"),
        ("guards.max_drawdown", "0.15"),
        ("guards.conditionals_on_ticks", "true"),
        ("guards.margin_call.mm_requirement", "0.05"),
        ("guards.margin_call.warn_fraction", "0.05"),
        ("guards.margin_call.buffer", "0.10"),
    ];

    /// A sample's label and its loader.
    pub type Sample = (&'static str, fn() -> Loaded);

    /// All three samples, for iteration in tests and binaries.
    pub const ALL: &[Sample] = &[("backtest", backtest), ("paper", paper), ("live", live)];

    /// The rows as the `run` profile the store would hold under that name.
    #[must_use]
    pub fn stored(rows: Rows) -> StoredProfile {
        let name = rows
            .iter()
            .find(|(path, _)| *path == "name")
            .map_or_else(|| "sample".to_string(), |(_, value)| value.trim_matches('"').to_string());
        StoredProfile {
            row: ProfileRow { name, kind: ProfileKind::Run, active: false, note: None },
            mounts: Vec::new(),
            params: BTreeMap::new(),
            settings: rows.iter().map(|(p, v)| ((*p).to_string(), (*v).to_string())).collect(),
            recorder: None,
        }
    }

    /// [`BACKTEST_ROWS`], loaded the way the daemon loads a stored run profile.
    pub fn backtest() -> Loaded {
        rows_to_run_profile(&stored(BACKTEST_ROWS)).map_err(ProfileError::Validation)
    }

    /// [`PAPER_ROWS`], loaded the way the daemon loads a stored run profile.
    pub fn paper() -> Loaded {
        rows_to_run_profile(&stored(PAPER_ROWS)).map_err(ProfileError::Validation)
    }

    /// [`LIVE_ROWS`], loaded the way the daemon loads a stored run profile.
    pub fn live() -> Loaded {
        rows_to_run_profile(&stored(LIVE_ROWS)).map_err(ProfileError::Validation)
    }
}

#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
mod profile_tests;
