//! The `ProfileRun` serde type tree, its small converters, and its tier-keyed risk glue.

use serde::Deserialize;
use std::time::Duration;
use vike_model::{AccountTier, GridFetch, MarginCallConfig, ProfileRisk, ProfileRiskError};

// ⚠ `Mode` (`backtest`/`paper`/`live`) stood here and is DELETED with the profile's `mode` field
// (decision 0156 item 7). It answered two questions in one word: what KIND of run this is — which
// is the program that runs (`vike-cli backtest`, with or without its `[walkforward]` table, or the
// `vike-tradehub` daemon) — and where a forward run's orders go, which is the TIER of the account
// each mount runs on (`account.tier`), with `live` meaning demo OR mainnet. Its readers follow the
// account now: [`ProfileRun::apply_risk`] takes the mounted account's tier. A stored `mode` row is
// refused by `deny_unknown_fields` like any other key the type does not have.

// ⚠ `EventSourceKind` and `BrokerKind` stood here and are DELETED with the two tables they typed
// (`ProfileRun::event_source`'s doc carries the argument). They were reachable only through those
// fields and only inside `ProfileRun::validate`, so nothing outside this file lost a type.

// ⚠ `EventSource` and `Broker` stood here and are DELETED — see [`ProfileRun::event_source`] and
// [`ProfileRun::broker`], the two tombstone fields that refuse the tables by name.

/// `[sinks.journal]` — the write-ahead command journal's TUNING, and nothing else: the segment
/// size, the flush cadence and the snapshot cadence of `vike_core::JournalConfig`. Every key is
/// optional; a key left off keeps the value the rung below set (the `config.journal_snapshot_every`
/// row for `snapshot_every`, else `vike_core::JournalConfig::at`'s default — the ONE place the
/// default numbers are written).
///
/// ⚠ **This table does not turn the journal on, name its directory or turn it off.** The
/// `config.journal_dir` settings row alone does (decision 0111, amended 2026-10-11): the journal is
/// ON exactly when that row is set, so the trade daemon that WRITES the journal and the data daemon
/// that FOLLOWS it read one directory by construction. With no row this table tunes nothing.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProfileJournal {
    /// **TOMBSTONE — `dir` is RETIRED, and REFUSED rather than ignored.** Not a configuration
    /// field: it is parsed only so [`ProfileRun::validate`] can refuse the key BY NAME and name the
    /// fix. The directory is the `config.journal_dir` row's; a profile that still names one would
    /// otherwise be a second directory nothing writes to, or (under `deny_unknown_fields` alone) a
    /// bare *"unknown field"* that reads like a typo — the [`ProfileRun::event_source`] argument,
    /// one key down.
    #[serde(default)]
    pub dir: Option<serde::de::IgnoredAny>,
    /// pre-allocated segment size (bytes). `None` keeps the rung below's.
    pub segment_bytes: Option<u64>,
    /// flush cadence (records). `None` keeps the rung below's.
    pub flush_every: Option<u32>,
    /// write a full-state `Snapshot` every N appended records (MUST be >= 1). `None` keeps the rung
    /// below's.
    pub snapshot_every: Option<u64>,
}

/// `[sinks]` — the portfolio-equity sampler and the command journal's tuning. An omitted `[sinks]`
/// = a headless run that samples nothing and tunes nothing.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProfileSinks {
    /// **TOMBSTONE — `sinks.gui` is RETIRED, and REFUSED rather than ignored.** Not a configuration
    /// field: it is parsed only so [`ProfileRun::validate`] can refuse the key BY NAME and name the
    /// fix (the [`ProfileJournal::dir`] pattern, one table over).
    ///
    /// It asked for `CoreSnapshot`s to be published for a GUI observer and was applied by NO mount:
    /// it reached no `CoreConfig` field and no daemon code read it, so nothing published a
    /// snapshot because of it while the stored row read as if something did.
    #[serde(default)]
    pub gui: Option<serde::de::IgnoredAny>,
    /// **TOMBSTONE — `sinks.recorder` is RETIRED, and REFUSED rather than ignored.** See
    /// [`ProfileSinks::gui`].
    ///
    /// It asked for a `RecorderSink` into the history store and was applied by NO mount. Recording
    /// is a recorder profile (`vike-cli config profile create recorder`) run INSIDE the datahub, which
    /// is the store's one writer (`docs/decisions/0084-only-the-datahub-touches-the-store.md`).
    #[serde(default)]
    pub recorder: Option<serde::de::IgnoredAny>,
    /// **TOMBSTONE — `sinks.raw_capture_dir` is RETIRED, and REFUSED rather than ignored.** See
    /// [`ProfileSinks::gui`].
    ///
    /// It named a Polymarket raw-frame capture directory and was applied by NO mount: no tap was
    /// ever opened because of it. The store has one writer (decision 0084).
    #[serde(default)]
    pub raw_capture_dir: Option<serde::de::IgnoredAny>,
    /// the write-ahead command journal's TUNING ([`ProfileJournal`]). `None` = the rung below's
    /// cadence. It is NOT the journal's switch: the `config.journal_dir` row is
    /// (`vike_tradehub_core::journal_config_from`).
    pub journal: Option<ProfileJournal>,
    /// portfolio-equity sampler interval (ms). `None` = disabled. → `vike_core::CoreConfig::equity_sample`,
    /// applied by `vike_tradehub_core::apply_guards_and_sinks`.
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
    /// `vike_tradehub_core::apply_guards_and_sinks`, is what sets it: the state the paragraph above was
    /// asking for.
    pub equity_sample_ms: Option<u64>,
    /// **Journal the FORECASTS the node's strategies publish** (book W35) → `vike_core::CoreConfig::journal_forecasts`,
    /// applied by `vike_tradehub_core::apply_guards_and_sinks` on the live arm and by the paper arm's builder. `false`
    /// — the default, and every profile that never names the key — journals NONE: a strategy still publishes (its
    /// backtest records them in the run), the live forecasts drop at the step's drain, the datahub stores no
    /// `kind=exec_forecast` row and a drift rule has no live stream. One switch for the whole node, because the
    /// journal is the node's (REVIEW-LIVE-MONEY M1).
    ///
    /// ⚠ **Turning it on bumps the node's journal FORMAT** to 32 at the first forecast journaled
    /// (`vike_journal`'s `FORECAST_VERSION`): a build that reads only 31 then refuses the journal at cold start, so
    /// a ROLLBACK past that point needs every account FLAT (the journal move-aside). And an older build refuses a
    /// stored profile that carries this key (`deny_unknown_fields`): re-store the run profile without it before
    /// downgrading (`docs/ops/release.md`'s "Rollback after the book deploy").
    #[serde(default)]
    pub forecasts: bool,
}

impl ProfileSinks {
    /// The equity-sampler interval as a [`Duration`], if enabled.
    pub fn equity_sample(&self) -> Option<Duration> {
        self.equity_sample_ms.map(Duration::from_millis)
    }
}

/// `[guards]` — the opt-in safety guards layered over the base [`ProfileRisk`] gate. Each maps to
/// an existing `vike_core::CoreConfig` knob, except `initial_trading_state`, which the daemon sends
/// to the spawned core as a `SetTradingState` command (see that field).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProfileGuards {
    /// The trading state the process STARTS in → [`vike_model::TradingState`]. `halted` = the HALT
    /// kill-switch (the gate denies every new order), `reducing` = only position-reducing orders.
    ///
    /// Applied by `vike-tradehub` on BOTH its arms, once the core is up and before any feed is
    /// wired: when it is not `active` the daemon sends `Command::SetTradingState` to the core
    /// (`vike_tradehub_core::initial_trading_state_to_send` decides). **A profile never LOOSENS a stricter
    /// state the journal restored** (a `halted` engine stays `halted` under a profile that says
    /// `reducing` or `active`; only an operator's `SetTradingState` loosens it), and it is applied
    /// at EVERY start: a `halted` here is "start halted", not "halt once". `active` — the default —
    /// sends nothing, which is why a profile that never names the key behaves exactly as before.
    /// This is NOT the HALT sentinel file (`docs/ops/kill-switches.md` sections C and 6): the
    /// sentinel stops venue submits across processes; this is the in-process gate state.
    #[serde(default)]
    pub initial_trading_state: vike_model::TradingState,
    /// stuck-order watchdog stage 1 → `vike_core::CoreConfig::submit_ack_timeout`. `None` = disabled.
    pub submit_ack_timeout_ms: Option<u64>,
    /// watchdog stage 2 → `vike_core::CoreConfig::submit_ack_confirm_grace`. `None` = **the caller's
    /// value is left exactly as it was** — this struct carries NO default of its own, so the
    /// effective grace is whatever the binary already built (a bare `CoreConfig::default`'s,
    /// unless that binary set its own).
    ///
    /// ⚠ This doc used to say "`None` = the CoreConfig default (5000 ms)", and both halves had
    /// rotted: `CoreConfig::default`'s grace was bumped to 15 s by the confirm-race hardening, and
    /// `vike_tradehub_core::apply_guards_and_sinks` was WRITING this struct's own stale 5 s over it
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
    /// equity-drawdown liquidate-only latch → `vike_core::CoreConfig::max_drawdown`. `None` = disabled.
    pub max_drawdown: Option<f64>,
    /// intra-bar stop/trailing check → `vike_core::CoreConfig::conditionals_on_ticks`.
    #[serde(default)]
    pub conditionals_on_ticks: bool,
    /// **TOMBSTONE — `guards.freshness_ms` is RETIRED, and REFUSED rather than ignored.** Not a
    /// configuration field: it is parsed only so [`ProfileRun::validate`] can refuse the key BY
    /// NAME and name the fix (the [`ProfileJournal::dir`] pattern, one table over).
    ///
    /// It was a data-freshness staleness window that belongs to each live subscription's
    /// `FreshnessTracker`, which no `CoreConfig` reaches; nothing ever read this value, so setting
    /// it changed no staleness window while the stored row read as if it did.
    #[serde(default)]
    pub freshness_ms: Option<serde::de::IgnoredAny>,
    /// opt-in margin-call watchdog → `vike_core::CoreConfig::margin_call`. `None` = disabled. The
    /// table IS the engine's own [`vike_model::MarginCallConfig`]: `maintenance_margin_requirement` is required,
    /// and a `warn_fraction` / `buffer` left off takes that type's `Default`.
    pub margin_call: Option<MarginCallConfig>,
}

impl ProfileGuards {
    // ⚠ A `const DEFAULT_CONFIRM_GRACE_MS: u64 = 5000` stood here and is DELETED, not corrected.
    // It was a SECOND authority for a default `crate::CoreConfig::default` already owns, and it
    // did what a second authority does: `CoreConfig`'s moved to 15 s with the confirm-race
    // hardening, this copy did not, and the converter below silently wrote the stale copy over the
    // live one on every profile that armed stage 1. Re-pointing it at `CoreConfig::default()`
    // would have fixed today's number and left the SHAPE — a profile-side default for a key the
    // operator did not set — free to rot again the next time either value moves. There is no
    // profile-side default now: `None` means the caller's value stands, and `CoreConfig::default`
    // is the one place the number is written.

    /// → `vike_core::CoreConfig::submit_ack_timeout`.
    pub fn submit_ack_timeout(&self) -> Option<Duration> {
        self.submit_ack_timeout_ms.map(Duration::from_millis)
    }

    /// → `vike_core::CoreConfig::submit_ack_confirm_grace`, or `None` when the profile named no
    /// grace — in which case there is nothing to apply and the caller's own value stands. The
    /// exact shape of [`ProfileGuards::submit_ack_timeout`] above, deliberately: silence is silence.
    pub fn submit_ack_confirm_grace(&self) -> Option<Duration> {
        self.submit_ack_confirm_grace_ms.map(Duration::from_millis)
    }
}

/// The whole run profile — one auditable config artifact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRun {
    /// human-readable label (for logs / audit); optional
    pub name: Option<String>,
    /// **TOMBSTONE — `[event_source]` is DELETED, and REFUSED rather than ignored.** Not a
    /// configuration field: it is parsed only so [`ProfileRun::validate`] can refuse the table BY
    /// NAME, which is the whole of what this field is for.
    ///
    /// It was schema-REQUIRED, validated against the since-deleted `mode`, documented in the shipped
    /// template — and read by no runner in either binary. the shipped example (a TOML file under `docs/ops/`, deleted by decision 0157) said so
    /// at the table itself: *"Schema-required, but IGNORED by the multi-venue live mount … No
    /// runner does, in either binary."* A key an operator fills in truthfully and nothing consults
    /// is the `Policy::max_total_exposure` shape this workspace deletes fields for, one level down.
    ///
    /// ⚠ Deleting it is NOT free, which is why it is a tombstone rather than an absence:
    /// `deny_unknown_fields` turns a surviving `[event_source]` into an *"unknown field"* startup
    /// failure that reads like a typo. [`serde::de::IgnoredAny`] accepts whatever shape the table
    /// had — the point is presence, never content — so an operator who never edited their profile
    /// gets a sentence saying what happened instead of a parser complaining about a word.
    #[serde(default)]
    pub event_source: Option<serde::de::IgnoredAny>,
    /// **TOMBSTONE — `[broker]` is DELETED, and REFUSED rather than ignored.** See
    /// [`ProfileRun::event_source`]; the two go together and for the same reason.
    ///
    /// ⚠ Its `seed_cash` looked load-bearing and was not. `ProfileRun::validate` required a
    /// positive one whenever `guards.max_drawdown` was set — the drawdown latch's denominator —
    /// but nothing ever carried this number into a `vike_core::CoreConfig::seed_cash`: the daemon's
    /// seed comes from its own `[[mounts]]` row (`vike_tradehub::config::MountConfig`'s `seed_cash`,
    /// which refuses a non-positive value on the same grounds, at the place the value is actually
    /// read). So the check was guarding the latch with a number the latch never saw.
    #[serde(default)]
    pub broker: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    pub sinks: ProfileSinks,
    #[serde(default)]
    pub risk: ProfileRisk,
    #[serde(default)]
    pub guards: ProfileGuards,
}

/// finite and strictly positive — for size/notional/exposure knobs where 0 or negative is nonsense.
pub(crate) fn is_pos(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// finite and in `(0.0, 1.0]` — the LEAN fraction shape (im/mm/warn/drawdown).
pub(crate) fn is_fraction_unit(v: f64) -> bool {
    v > 0.0 && (0.0..=1.0).contains(&v)
}

impl ProfileRun {
    /// Merge this profile's `[risk]` table onto `base` via [`ProfileRisk::apply_to`], for a mount
    /// of an account at `tier`: the [`GridFetch`] is the TIER's ([`GridFetch::for_tier`], the
    /// one place that mapping is made), never the profile's — a run profile states no tier
    /// (decision 0156 item 7). A `paper` account fetches no grid, so the profile may supply the
    /// instrument fields; a `demo`/`live` account mounts the venue's grid, so a profile that sets
    /// any of `tick_size`/`lot_size`/`min_qty`/`min_notional` is `Err`, naming them.
    pub fn apply_risk(
        &self,
        base: vike_model::RiskLimits,
        tier: AccountTier,
    ) -> Result<vike_model::RiskLimits, ProfileRiskError> {
        self.risk.apply_to(base, GridFetch::for_tier(tier))
    }

    // ⚠ `grid_source` and `risk_for_live_venue_mount` stood here and are DELETED with `mode`
    // (decision 0156 item 7). The first derived the grid source from the profile's `mode`; it
    // comes from the mounted account's tier now (`apply_risk`'s `tier`, and `vike-tradehub-core`'s merge
    // site for the live arm). The second refused to hand a non-`live` profile's `[risk]` to a live
    // venue mount; a profile can no longer contradict its account, and the venue-owned fields it
    // guarded against are refused where the grid is known — `ProfileRisk::apply_to` at the mount of
    // every `demo`/`live` account.
}
