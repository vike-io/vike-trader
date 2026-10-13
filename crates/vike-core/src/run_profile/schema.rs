//! The `RunProfile` serde type tree, its small converters, and its tier-keyed risk glue.

use serde::Deserialize;
use std::time::Duration;
use vike_model::{AccountTier, GridSource, ProfileError, ProfileRisk};

// ⚠ `Mode` (`backtest`/`paper`/`live`) stood here and is DELETED with the profile's `mode` field
// (decision 0156 item 7). It answered two questions in one word: what KIND of run this is — which
// is the program that runs (`vike-cli backtest`, with or without its `[walkforward]` table, or the
// `vike-tradehub` daemon) — and where a forward run's orders go, which is the TIER of the account
// each mount runs on (`account.tier`), with `live` meaning demo OR mainnet. Its readers follow the
// account now: [`RunProfile::apply_risk`] takes the mounted account's tier. A stored `mode` row is
// refused by `deny_unknown_fields` like any other key the type does not have.

// ⚠ `EventSourceKind` and `BrokerKind` stood here and are DELETED with the two tables they typed
// (`RunProfile::event_source`'s doc carries the argument). They were reachable only through those
// fields and only inside `RunProfile::validate`, so nothing outside this file lost a type.

/// Initial pre-trade gate state. Mirrors [`vike_model::TradingState`]; `halted` is the HALT
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
    /// Map to the real [`vike_model::TradingState`].
    pub fn to_trading_state(self) -> vike_model::TradingState {
        match self {
            ProfileTradingState::Active => vike_model::TradingState::Active,
            ProfileTradingState::Reducing => vike_model::TradingState::Reducing,
            ProfileTradingState::Halted => vike_model::TradingState::Halted,
        }
    }
}

// ⚠ `EventSource` and `Broker` stood here and are DELETED — see [`RunProfile::event_source`] and
// [`RunProfile::broker`], the two tombstone fields that refuse the tables by name.

/// `[sinks.journal]` — the write-ahead command journal's TUNING, and nothing else: the segment
/// size, the flush cadence and the snapshot cadence of [`crate::JournalConfig`]. Every key is
/// optional; a key left off keeps the value the rung below set (the `config.journal_snapshot_every`
/// row for `snapshot_every`, else [`crate::JournalConfig::at`]'s default — the ONE place the
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
    /// field: it is parsed only so [`RunProfile::validate`] can refuse the key BY NAME and name the
    /// fix. The directory is the `config.journal_dir` row's; a profile that still names one would
    /// otherwise be a second directory nothing writes to, or (under `deny_unknown_fields` alone) a
    /// bare *"unknown field"* that reads like a typo — the [`RunProfile::event_source`] argument,
    /// one key down.
    #[serde(default)]
    pub dir: Option<serde::de::IgnoredAny>,
    /// pre-allocated segment size (bytes). `None` keeps the rung below's.
    pub segment_bytes: Option<u64>,
    /// flush cadence (records). `None` keeps the rung below's.
    pub flush_every: Option<u32>,
    /// write a full-state `Snap` every N appended records (MUST be >= 1). `None` keeps the rung
    /// below's.
    pub snapshot_every: Option<u64>,
}

impl ProfileJournal {
    /// Lay the keys this table NAMES over `cfg` — a key left off leaves `cfg`'s value standing
    /// (compile-checked mapping onto the real [`crate::JournalConfig`]).
    pub fn tune(&self, cfg: &mut crate::JournalConfig) {
        if let Some(v) = self.segment_bytes {
            cfg.file.segment_bytes = v;
        }
        if let Some(v) = self.flush_every {
            cfg.file.flush_every = v;
        }
        if let Some(v) = self.snapshot_every {
            cfg.snapshot_every = v;
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
    /// the write-ahead command journal's TUNING ([`ProfileJournal`]). `None` = the rung below's
    /// cadence. It is NOT the journal's switch: the `config.journal_dir` row is
    /// ([`crate::journal_config_from`]).
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
    /// The equity-sampler interval as a [`Duration`], if enabled.
    pub fn equity_sample(&self) -> Option<Duration> {
        self.equity_sample_ms.map(Duration::from_millis)
    }
}

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

// One authority for the defaults: `vike_exec::MarginCallConfig::default`.
fn default_warn_fraction() -> f64 {
    vike_exec::MarginCallConfig::default().warn_fraction
}
fn default_buffer() -> f64 {
    vike_exec::MarginCallConfig::default().buffer
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
    /// initial gate state → [`vike_model::TradingState`]. `halted` = the HALT kill-switch.
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

    /// → the initial [`vike_model::TradingState`].
    pub fn trading_state(&self) -> vike_model::TradingState {
        self.initial_trading_state.to_trading_state()
    }
}

/// The whole run profile — one auditable config artifact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunProfile {
    /// human-readable label (for logs / audit); optional
    pub name: Option<String>,
    /// **TOMBSTONE — `[event_source]` is DELETED, and REFUSED rather than ignored.** Not a
    /// configuration field: it is parsed only so [`RunProfile::validate`] can refuse the table BY
    /// NAME, which is the whole of what this field is for.
    ///
    /// It was schema-REQUIRED, validated against the since-deleted `mode`, documented in the shipped
    /// template — and read by no runner in either binary. `docs/ops/run-profile-live.toml` said so
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

/// finite and strictly positive — for size/notional/exposure knobs where 0 or negative is nonsense.
pub(crate) fn is_pos(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// finite and in `(0.0, 1.0]` — the LEAN fraction shape (im/mm/warn/drawdown).
pub(crate) fn is_frac_unit(v: f64) -> bool {
    v > 0.0 && (0.0..=1.0).contains(&v)
}

impl RunProfile {
    /// Merge this profile's `[risk]` table onto `base` via [`ProfileRisk::apply_to`], for a mount
    /// of an account at `tier`: the [`GridSource`] is the TIER's ([`GridSource::for_tier`], the
    /// one place that mapping is made), never the profile's — a run profile states no tier
    /// (decision 0156 item 7). A `paper` account fetches no grid, so the profile may supply the
    /// instrument fields; a `demo`/`live` account mounts the venue's grid, so a profile that sets
    /// any of `tick_size`/`lot_size`/`min_qty`/`min_notional` is `Err`, naming them.
    pub fn apply_risk(
        &self,
        base: vike_model::RiskLimits,
        tier: AccountTier,
    ) -> Result<vike_model::RiskLimits, ProfileError> {
        self.risk.apply_to(base, GridSource::for_tier(tier))
    }

    // ⚠ `grid_source` and `risk_for_live_venue_mount` stood here and are DELETED with `mode`
    // (decision 0156 item 7). The first derived the grid source from the profile's `mode`; it
    // comes from the mounted account's tier now (`apply_risk`'s `tier`, and `vike-mount`'s merge
    // site for the live arm). The second refused to hand a non-`live` profile's `[risk]` to a live
    // venue mount; a profile can no longer contradict its account, and the venue-owned fields it
    // guarded against are refused where the grid is known — `ProfileRisk::apply_to` at the mount of
    // every `demo`/`live` account.
}
