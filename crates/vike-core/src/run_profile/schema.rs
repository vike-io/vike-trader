//! The `RunProfile` serde type tree, its small converters, and its `mode`-derived risk glue.

use serde::Deserialize;
use std::time::Duration;
use vike_exec::{GridSource, ProfileError, ProfileRisk};

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

/// finite and strictly positive — for size/notional/exposure knobs where 0 or negative is nonsense.
pub(crate) fn is_pos(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// finite and in `(0.0, 1.0]` — the LEAN fraction shape (im/mm/warn/drawdown).
pub(crate) fn is_frac_unit(v: f64) -> bool {
    v > 0.0 && (0.0..=1.0).contains(&v)
}

impl RunProfile {
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

    /// The `[risk]` table this profile carries — but ONLY for a caller about to hand it to a REAL
    /// live venue mount (`vike_mount::MountEnv::risk_profile`, which `make_engine` hardcodes
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
}
