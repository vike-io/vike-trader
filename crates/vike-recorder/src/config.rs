//! `config` — the recorder's subscription profile: ONE reviewable document naming what to record.
//!
//! ⚠ **"FILE" BECAME "DOCUMENT" ON 2026-09-16, AND THIS TYPE DID NOT CHANGE AT ALL.** The owner
//! ordered the profile into the settings database, overruling
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s NO. A stored profile
//! is a `recorder` row plus its `subscription` rows, and the daemon RENDERS those rows back into
//! the TOML document this file parses — `vike_secrets::profile_store::render_recorder_toml`, fed to
//! [`RecorderProfile::from_toml`]. So every refusal below applies to a row-loaded profile with no
//! second implementation: serde's `deny_unknown_fields` on all four structs, the missing-`store`
//! parse error and all seven `RecorderProfile::validate` rules. **This module stays the schema
//! AUTHORITY**; the migration's key table in `crates/vike-cli/src/cmd/config_mirror_recorder.rs`
//! says so at itself, and what holds the two together is a ROUND TRIP rather than a promise.
//!
//! Design authority: `docs/superpowers/specs/2026-08-02-live-recorder-design.md`. Two of its four
//! settled decisions are visible directly in this file's shape:
//!
//! - **Families are GENERAL across venues** (§8.2), so there is ONE `family` key rather than a
//!   Polymarket-shaped concept plus a symbol list for everyone else. Polymarket resolves a family to
//!   live tokens over Gamma (its token ids rotate every 5 minutes); a venue with static symbols
//!   resolves the same key as a filter (`*USDT-PERP`). One config shape, one Data Manager screen.
//! - **Backfill source is a PER-SUBSCRIPTION choice** (§8.3), so [`Backfill`] sits on the
//!   subscription rather than being a daemon-wide setting.
//!
//! Explicit `symbols` stay PER-SYMBOL (no group): a customer recording two instruments should not
//! pay for a grouped layout, and grouping only earns its keep across a wide subscription.
//!
//! # ⚠ Every struct here is `deny_unknown_fields`, and that is the same argument [`ProfileError`] makes
//!
//! Serde's DEFAULT is to ignore a key it does not recognise. On a file whose whole purpose is
//! "ONE reviewable TOML naming what to record", that turns a one-character typo into a silent
//! misconfiguration — precisely the failure every [`ProfileError`] variant below exists to refuse.
//! The validation caught a subscription that would record NOTHING and let a MISSPELLED knob
//! through, which is the sharper bug: `webhook = ["telegram"]` (singular) left the operator
//! believing a pager was armed while delivery stayed log-only, `retention_days` misspelled kept
//! the tape forever after they asked for 30 days, and `backfil = "archive"` silently fell back to
//! [`Backfill::Venue`] — which on Polymarket cannot restore the book at all, the exact lie
//! [`Backfill`]'s own doc says nothing may tell.
//!
//! `<project>/settings/*.toml` has denied unknown keys since it shipped (`vike_config`'s
//! `deny_unknown_fields` on every file struct); this file is the other operator-authored TOML in
//! the tree and had no such gate. `toml`'s refusal names the offending key AND lists the accepted
//! ones, so the message is already actionable — it just was not being asked for.
//!
//! ⚠ This is a REFUSAL, so it can reject a profile that used to start. Checked before it landed:
//! the shipped `crates/vike-recorder/recorder.example.toml`, both TOML snippets in
//! `docs/ops/recorder-deploy.md`, and the CI box's live `settings/recorder.toml` (read read-only,
//! 2026-08-10) use only known keys. A profile carrying a hand-written note as a bare key is the
//! one shape that now fails — TOML comments start with `#`, which is unaffected.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use vike_data::{CompactionConfig, MaintenanceConfig, RetentionPolicy};

/// Where a gap gets refilled from — chosen per subscription (spec §8.3).
///
/// ⚠ The two are NOT equivalent, and the difference is not a quality knob: on Polymarket no L2 book
/// history exists to fetch, so [`Backfill::Venue`] can only restore the TRADE tape and the book keeps
/// its hole. Anything reporting a venue-backfilled window as simply "filled" is lying to the
/// customer, who then discovers it when a backtest silently runs on a book-less window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backfill {
    /// The venue's own REST history. Free, and only as complete as the venue exposes.
    #[default]
    Venue,
    /// The `data.vike.io` archive. Complete L2 + trades; paid per GB, needs an archive API key.
    Archive,
    /// Detect and report gaps, refill nothing.
    Off,
}

/// One thing to record: a whole family, or an explicit symbol list.
///
/// `deny_unknown_fields` — see the module doc. `symbol`/`backfil` used to be accepted and dropped,
/// and a dropped `symbols` then reached [`ProfileError::Empty`] only when no `family` was set too.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub venue: String,
    /// A market family — e.g. Polymarket `btc-5m`, or a venue-side filter like `*USDT-PERP`.
    /// Recorded as ONE grouped series: one commit per family per flush instead of one per symbol.
    #[serde(default)]
    pub family: Option<String>,
    /// Explicit symbols. Recorded PER-SYMBOL (ungrouped) — grouping only pays across a wide
    /// subscription, and a handful of named instruments is not that.
    #[serde(default)]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub backfill: Backfill,
}

/// Background compaction + retention for the store this recorder writes.
///
/// **On by default, and that is the point.** A recorder commits once per buffer flush — `max_rows`
/// (5,000) or `max_age` (30 s), whichever comes first — so a busy series produces a part every few
/// seconds. Measured on the first live run (2026-08-02): ONE Polymarket family's book wrote **23
/// parts in 150 s**, ~57 KB each. Left alone that is ~13,000 files a day for one family's one kind,
/// and every later scan pays for all of them. [`vike_data::MaintenanceScheduler`] already merges
/// them and is explicitly safe alongside live appends (it takes the same per-series lock), so the
/// only real mistake available here is forgetting to run it — which is why an absent `[maintenance]`
/// table means DEFAULTS, not OFF.
///
/// ⚠ …which is exactly why `deny_unknown_fields` matters most HERE (see the module doc): every knob
/// below has a `#[serde(default)]`, so a misspelled key was indistinguishable from an omitted one
/// and silently restored the default the operator was overriding.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Maintenance {
    /// Seconds between passes. `0` disables maintenance entirely — an explicit opt-out for an
    /// operator who compacts on their own schedule, not a value to reach for casually.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// Minimum fragment count in a `date=` before it is worth compacting.
    #[serde(default = "default_min_parts")]
    pub min_parts: usize,
    /// Target sealed-part size, in MiB. The size compaction converges toward — NOT a memory knob
    /// (that is `max_merge_rows`).
    #[serde(default = "default_target_mb")]
    pub target_mb: u64,
    /// **The memory knob**: the most rows one merge may decode at once. Measured on the CI box's
    /// Polymarket book series, this store's widest rows cost roughly 1 KB of peak RSS each, so the
    /// 1,000,000 default lands near 1 GB; bars and trades cost far less. Raise it only against a
    /// measurement — the ratio is a property of the data, not of the code. If your writer runs
    /// under a `MemoryMax`, this is the number to size against it.
    #[serde(default = "default_max_merge_rows")]
    pub max_merge_rows: usize,
    /// Drop data older than this many days. `None` (the default) never prunes — a recorder exists to
    /// ACCUMULATE tape, so deleting any of it is an explicit choice, never a default.
    #[serde(default)]
    pub retention_days: Option<u32>,
}

fn default_interval_secs() -> u64 {
    300
}
fn default_min_parts() -> usize {
    4
}
fn default_target_mb() -> u64 {
    384
}
fn default_max_merge_rows() -> usize {
    1_000_000
}

impl Default for Maintenance {
    fn default() -> Self {
        Self {
            interval_secs: default_interval_secs(),
            min_parts: default_min_parts(),
            target_mb: default_target_mb(),
            max_merge_rows: default_max_merge_rows(),
            retention_days: None,
        }
    }
}

impl Maintenance {
    /// `None` when maintenance is disabled; otherwise the config + pass interval the scheduler takes.
    pub fn scheduler_args(&self) -> Option<(MaintenanceConfig, Duration)> {
        if self.interval_secs == 0 {
            return None;
        }
        Some((
            MaintenanceConfig {
                compaction: CompactionConfig {
                    target_bytes: self.target_mb.saturating_mul(1024 * 1024),
                    min_parts: self.min_parts,
                    max_merge_rows: self.max_merge_rows,
                },
                retention: self.retention_days.map(|d| RetentionPolicy {
                    before_ts: None,
                    max_age_ms: Some(i64::from(d) * 86_400_000),
                }),
            },
            Duration::from_secs(self.interval_secs),
        ))
    }
}

/// Where a SILENT SERIES goes — the alerting half of the silence watchdog
/// (`crate::liveness`), and the answer to "who is told".
///
/// **An absent `[alerting]` table means DEFAULTS, not OFF**, for the same reason
/// [`Maintenance`]'s does: the failure this watchdog catches is silent by nature, so an operator
/// who has to know to switch the alarm on is exactly the operator who will not. The defaults mount
/// the engine and deliver to the log — which is where the warning already went — and adding a
/// webhook target is what escalates the same fired alert to a pager. The alert is therefore
/// PRODUCED on every recorder, configured or not; only its reach changes.
///
/// ⚠ No credential ever appears in this table. `webhooks` names TARGETS
/// (`vike_alerting::WebhookConfig::name`, i.e. `"telegram"` / `"webhook"`), and the token behind a
/// name comes from the credential store — a profile is a reviewable file that gets pasted into
/// tickets.
///
/// ⚠ `deny_unknown_fields` (module doc). This table is the one where a silently-dropped key leaves
/// the operator believing a PAGER is armed while nothing is delivered anywhere but the log — the
/// belief class `docs/decisions/0013-degrade-vs-refuse.md` refuses on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alerting {
    /// Webhook target NAMES a silent-series alert is delivered to — `"telegram"` and `"webhook"`
    /// are what `vike_alerting::webhook_configs_from_env` builds from the credential store. Empty
    /// (the default) ⇒ the alert still fires and still reaches the log; nothing is POSTed.
    #[serde(default)]
    pub webhooks: Vec<String>,
    /// How often a series that is STILL silent re-alerts, in seconds. `0` = once per silence
    /// episode (a recovery re-arms it either way). The default is an hour: the watchdog ticks every
    /// 30s and an outage outlives a shift, so re-paging is wanted — but not 120 times an hour.
    #[serde(default = "default_repeat_secs")]
    pub repeat_secs: u64,
    /// Only alert for series whose key starts with this. Absent (the default) ⇒ every subscribed
    /// series. A PREFIX because a Polymarket token id does not exist yet when the profile is
    /// written — see `vike_alerting::RuleTrigger::SeriesStale`.
    #[serde(default)]
    pub series_prefix: Option<String>,
}

fn default_repeat_secs() -> u64 {
    3600
}

impl Default for Alerting {
    fn default() -> Self {
        Self { webhooks: Vec::new(), repeat_secs: default_repeat_secs(), series_prefix: None }
    }
}

/// The daemon profile.
///
/// `deny_unknown_fields` — see the module doc. A top-level `subscription` (for `subscribe`) or
/// `store_root` (for `store`) used to leave `store` missing or every subscription dropped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecorderProfile {
    /// Store root — and since ruling 10 it must be the SAME root the data server resolved.
    ///
    /// ⚠ It used to be a root a SEPARATE process wrote while `vike-datahub` read it (spec §8.1),
    /// which is why the store's `SeriesLock` is a lockfile. The merge did not delete this key: an
    /// operator's existing profile still names its store here and still parses. What changed is
    /// that the daemon now REFUSES to start when this names a different directory from the one
    /// `VIKE_DATAHUB_STORE`/`VIKE_HIST_STORE` resolved — `crates/vike-datahub/src/recorder.rs`'s
    /// `one_store_root` is the check, and its doc carries why a refusal beats either side silently
    /// winning.
    pub store: PathBuf,
    #[serde(default)]
    pub subscribe: Vec<Subscription>,
    #[serde(default)]
    pub maintenance: Maintenance,
    #[serde(default)]
    pub alerting: Alerting,
}

/// Why a profile was rejected. Every variant is a mistake that would otherwise record the wrong
/// thing, or nothing, without saying so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    Parse(String),
    /// A subscription naming neither a family nor any symbols records NOTHING. Silently ignoring it
    /// is how a customer discovers three days later that a venue they configured has no tape.
    Empty {
        index: usize,
        venue: String,
    },
    /// Both a family and explicit symbols in one entry is ambiguous: the symbols would be recorded
    /// per-symbol while the family is grouped, so the same instrument could land in both layouts and
    /// be read twice. Split them into two subscriptions and the intent is explicit.
    FamilyAndSymbols {
        index: usize,
        venue: String,
    },
    /// Two subscriptions claiming the same family. Which `Backfill` wins would be arbitrary.
    DuplicateFamily {
        venue: String,
        family: String,
    },
    /// `retention_days = 0` prunes everything whose `ts_max` is older than NOW — i.e. the tape this
    /// daemon exists to accumulate, continuously, as it writes it. Almost certainly a typo for
    /// "keep forever", which is what OMITTING the key means.
    ZeroRetention,
    /// `min_parts` below 2 makes every pass rewrite a `date=` that has nothing to merge — pure churn
    /// against the same per-series lock live appends need.
    MinPartsTooSmall {
        got: usize,
    },
    /// `target_mb = 0` makes every part count as already at target, so compaction selects nothing
    /// and silently never runs — the failure mode is a fragment count that grows forever.
    ZeroTargetSize,
    /// `max_merge_rows = 0` makes every part count as too big to merge — the same silent
    /// never-compacts as `ZeroTargetSize`, reached through the memory knob instead.
    ZeroMaxMergeRows,
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "recorder profile: {e}"),
            Self::Empty { index, venue } => write!(
                f,
                "recorder profile: subscription {index} ({venue}) names neither `family` nor \
                 `symbols` — it would record nothing"
            ),
            Self::FamilyAndSymbols { index, venue } => write!(
                f,
                "recorder profile: subscription {index} ({venue}) sets BOTH `family` and `symbols` \
                 — the family records grouped and the symbols record per-symbol, so an instrument \
                 in both would be stored twice. Split into two subscriptions."
            ),
            Self::DuplicateFamily { venue, family } => write!(
                f,
                "recorder profile: family `{family}` is subscribed twice for {venue} — which \
                 `backfill` applies would be arbitrary"
            ),
            Self::ZeroRetention => write!(
                f,
                "recorder profile: [maintenance].retention_days = 0 would prune every part older \
                 than NOW — i.e. the tape this daemon is writing, as it writes it. Omit the key to \
                 keep data forever (the default)."
            ),
            Self::MinPartsTooSmall { got } => write!(
                f,
                "recorder profile: [maintenance].min_parts = {got} — a `date=` needs at least 2 \
                 parts to have anything to merge, so anything below that rewrites files for nothing \
                 while holding the per-series lock live appends need"
            ),
            Self::ZeroTargetSize => write!(
                f,
                "recorder profile: [maintenance].target_mb = 0 — every part would count as already \
                 at target, so compaction would select nothing and parts would accumulate forever. \
                 Set a real budget, or turn maintenance off explicitly with \
                 [maintenance].interval_secs = 0."
            ),
            Self::ZeroMaxMergeRows => write!(
                f,
                "recorder profile: [maintenance].max_merge_rows = 0 — every part would count as \
                 too big to merge, so compaction would select nothing and parts would accumulate \
                 forever. Set a real row budget (it is what caps a merge's peak memory), or turn \
                 maintenance off explicitly with [maintenance].interval_secs = 0."
            ),
        }
    }
}

impl std::error::Error for ProfileError {}

impl RecorderProfile {
    /// Parse and VALIDATE. Validation is not decoration: every rejection here is a config that would
    /// otherwise record the wrong thing, or nothing, without saying so.
    pub fn from_toml(s: &str) -> Result<Self, ProfileError> {
        let p: Self = toml::from_str(s).map_err(|e| ProfileError::Parse(e.to_string()))?;
        p.validate()?;
        Ok(p)
    }

    fn validate(&self) -> Result<(), ProfileError> {
        let mut seen: BTreeMap<(&str, &str), ()> = BTreeMap::new();
        for (i, s) in self.subscribe.iter().enumerate() {
            match (&s.family, s.symbols.is_empty()) {
                (None, true) => {
                    return Err(ProfileError::Empty { index: i, venue: s.venue.clone() });
                }
                (Some(_), false) => {
                    return Err(ProfileError::FamilyAndSymbols {
                        index: i,
                        venue: s.venue.clone(),
                    });
                }
                _ => {}
            }
            if let Some(fam) = &s.family
                && seen.insert((&s.venue, fam), ()).is_some()
            {
                return Err(ProfileError::DuplicateFamily {
                    venue: s.venue.clone(),
                    family: fam.clone(),
                });
            }
        }
        if self.maintenance.retention_days == Some(0) {
            return Err(ProfileError::ZeroRetention);
        }
        // Only meaningful while maintenance actually runs; a disabled table is not validated for
        // knobs nothing will read.
        if self.maintenance.interval_secs > 0 && self.maintenance.min_parts < 2 {
            return Err(ProfileError::MinPartsTooSmall { got: self.maintenance.min_parts });
        }
        if self.maintenance.interval_secs > 0 && self.maintenance.target_mb == 0 {
            return Err(ProfileError::ZeroTargetSize);
        }
        if self.maintenance.interval_secs > 0 && self.maintenance.max_merge_rows == 0 {
            return Err(ProfileError::ZeroMaxMergeRows);
        }
        Ok(())
    }

    /// The families this profile records, as `(venue, family)`. What the resolvers must keep a live
    /// membership for.
    pub fn families(&self) -> Vec<(String, String)> {
        self.subscribe
            .iter()
            .filter_map(|s| s.family.as_ref().map(|f| (s.venue.clone(), f.clone())))
            .collect()
    }

    /// The explicitly-named `(venue, symbol)` pairs — recorded per-symbol, never grouped.
    pub fn explicit_symbols(&self) -> Vec<(String, String)> {
        self.subscribe
            .iter()
            .flat_map(|s| s.symbols.iter().map(|sym| (s.venue.clone(), sym.clone())))
            .collect()
    }

    /// The backfill source for a family, if it is subscribed.
    pub fn backfill_for(&self, venue: &str, family: &str) -> Option<Backfill> {
        self.subscribe
            .iter()
            .find(|s| s.venue == venue && s.family.as_deref() == Some(family))
            .map(|s| s.backfill)
    }
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
