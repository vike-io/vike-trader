//! `config` — the recorder's subscription profile: ONE reviewable TOML file naming what to record.
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
store = "/data/tape"

[[subscribe]]
venue = "polymarket"
family = "btc-5m"
backfill = "archive"

[[subscribe]]
venue = "binance"
family = "*USDT-PERP"

[[subscribe]]
venue = "binance"
symbols = ["BTCUSDT", "ETHUSDT"]
backfill = "off"
"#;

    /// The shape both venues share — spec §8.2's "families are general" decision, in config form.
    /// Polymarket's family rotates its members every 5 minutes and Binance's is a static filter, but
    /// the customer writes the same key for both.
    #[test]
    fn parses_families_and_explicit_symbols_with_one_shape() {
        let p = RecorderProfile::from_toml(SAMPLE).unwrap();
        assert_eq!(p.store, PathBuf::from("/data/tape"));
        assert_eq!(
            p.families(),
            vec![("polymarket".into(), "btc-5m".into()), ("binance".into(), "*USDT-PERP".into())]
        );
        assert_eq!(
            p.explicit_symbols(),
            vec![("binance".into(), "BTCUSDT".into()), ("binance".into(), "ETHUSDT".into())]
        );
    }

    /// Backfill is PER SUBSCRIPTION (spec §8.3), not a daemon-wide switch.
    #[test]
    fn backfill_is_per_subscription_and_defaults_to_venue() {
        let p = RecorderProfile::from_toml(SAMPLE).unwrap();
        assert_eq!(p.backfill_for("polymarket", "btc-5m"), Some(Backfill::Archive));
        assert_eq!(
            p.backfill_for("binance", "*USDT-PERP"),
            Some(Backfill::Venue),
            "omitted ⇒ the free venue-REST path"
        );
        assert_eq!(p.backfill_for("binance", "nope"), None);
    }

    /// A subscription naming nothing records nothing — rejected rather than silently ignored, which
    /// is how a customer would otherwise find out days later that a venue has no tape.
    #[test]
    fn a_subscription_that_records_nothing_is_rejected() {
        let err = RecorderProfile::from_toml("store = \"/x\"\n\n[[subscribe]]\nvenue = \"okx\"\n")
            .unwrap_err();
        assert!(matches!(err, ProfileError::Empty { index: 0, .. }), "{err}");
        assert!(format!("{err}").contains("record nothing"), "{err}");
    }

    /// Family AND symbols in one entry would store the same instrument twice — grouped by the
    /// family and per-symbol by the list — so a read would return it from both layouts.
    #[test]
    fn family_plus_symbols_in_one_subscription_is_rejected() {
        let toml = "store = \"/x\"\n\n[[subscribe]]\nvenue = \"binance\"\nfamily = \"f\"\n\
                    symbols = [\"BTCUSDT\"]\n";
        let err = RecorderProfile::from_toml(toml).unwrap_err();
        assert!(matches!(err, ProfileError::FamilyAndSymbols { index: 0, .. }), "{err}");
    }

    #[test]
    fn the_same_family_twice_is_rejected() {
        let toml = "store = \"/x\"\n\n[[subscribe]]\nvenue = \"p\"\nfamily = \"f\"\n\n\
                    [[subscribe]]\nvenue = \"p\"\nfamily = \"f\"\nbackfill = \"archive\"\n";
        let err = RecorderProfile::from_toml(toml).unwrap_err();
        assert!(matches!(err, ProfileError::DuplicateFamily { .. }), "{err}");
    }

    /// The same family name under DIFFERENT venues is legal — families are venue-scoped.
    #[test]
    fn the_same_family_name_under_two_venues_is_fine() {
        let toml = "store = \"/x\"\n\n[[subscribe]]\nvenue = \"a\"\nfamily = \"f\"\n\n\
                    [[subscribe]]\nvenue = \"b\"\nfamily = \"f\"\n";
        assert!(RecorderProfile::from_toml(toml).is_ok());
    }

    /// An empty profile is valid — a daemon with nothing subscribed yet is a legitimate state (the
    /// customer has not picked anything in the Data Manager), not a misconfiguration.
    #[test]
    fn an_empty_profile_is_valid() {
        let p = RecorderProfile::from_toml("store = \"/x\"\n").unwrap();
        assert!(p.subscribe.is_empty());
        assert!(p.families().is_empty());
    }

    /// **An absent `[maintenance]` table means DEFAULTS, not OFF.** A recorder commits once per
    /// buffer flush, so a busy series writes a part every few seconds — measured live, 23 parts in
    /// 150 s for one family's book. Defaulting to "no compaction" would leave every customer who did
    /// not know to ask for it with ~13,000 files a day per family per kind.
    #[test]
    fn an_absent_maintenance_table_means_defaults_not_off() {
        let p = RecorderProfile::from_toml("store = \"/x\"\n").unwrap();
        assert_eq!(p.maintenance, Maintenance::default());
        let (cfg, interval) = p.maintenance.scheduler_args().expect("on by default");
        assert_eq!(interval, Duration::from_secs(300));
        assert_eq!(cfg.compaction.min_parts, 4);
        assert!(cfg.retention.is_none(), "a recorder ACCUMULATES; pruning is always explicit");
    }

    #[test]
    fn maintenance_knobs_parse_and_convert() {
        let toml = "store = \"/x\"\n\n[maintenance]\ninterval_secs = 60\nmin_parts = 8\n\
                    target_mb = 64\nretention_days = 7\n";
        let p = RecorderProfile::from_toml(toml).unwrap();
        let (cfg, interval) = p.maintenance.scheduler_args().unwrap();
        assert_eq!(interval, Duration::from_secs(60));
        assert_eq!(cfg.compaction.min_parts, 8);
        assert_eq!(cfg.compaction.target_bytes, 64 * 1024 * 1024);
        assert_eq!(cfg.retention.unwrap().max_age_ms, Some(7 * 86_400_000));
    }

    /// A partial table keeps the other defaults — turning one knob must not silently disable the
    /// rest.
    #[test]
    fn a_partial_maintenance_table_keeps_the_other_defaults() {
        let p = RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nmin_parts = 16\n")
            .unwrap();
        assert_eq!(p.maintenance.min_parts, 16);
        assert_eq!(p.maintenance.interval_secs, 300);
        assert_eq!(p.maintenance.target_mb, 384);
    }

    #[test]
    fn interval_zero_disables_maintenance() {
        let p = RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\ninterval_secs = 0\n")
            .unwrap();
        assert!(p.maintenance.scheduler_args().is_none());
    }

    /// `retention_days = 0` prunes everything older than NOW — the tape being written, as it is
    /// written. Almost certainly a typo for "keep forever", which is what omitting the key means.
    #[test]
    fn zero_retention_is_rejected_because_it_would_delete_the_tape_continuously() {
        let err =
            RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nretention_days = 0\n")
                .unwrap_err();
        assert!(matches!(err, ProfileError::ZeroRetention), "{err}");
        assert!(err.to_string().contains("keep data forever"), "{err}");
    }

    #[test]
    fn min_parts_below_two_is_rejected_as_pure_churn() {
        let err = RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nmin_parts = 1\n")
            .unwrap_err();
        assert!(matches!(err, ProfileError::MinPartsTooSmall { got: 1 }), "{err}");
    }

    /// ...but not while maintenance is off: validating a knob nothing will read would reject a
    /// perfectly coherent "disabled, and I left the old numbers in place" profile.
    #[test]
    fn min_parts_is_not_validated_when_maintenance_is_disabled() {
        let toml = "store = \"/x\"\n\n[maintenance]\ninterval_secs = 0\nmin_parts = 1\n";
        assert!(RecorderProfile::from_toml(toml).is_ok());
    }

    /// `target_mb` bounds a merge's memory AND selects what to merge, so zero means "nothing is
    /// ever compacted" — silently. An operator who wants that says `interval_secs = 0`.
    #[test]
    fn a_zero_target_size_is_rejected_rather_than_silently_disabling_compaction() {
        let err = RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\ntarget_mb = 0\n")
            .unwrap_err();
        assert!(matches!(err, ProfileError::ZeroTargetSize), "{err}");
        let off = "store = \"/x\"\n\n[maintenance]\ninterval_secs = 0\ntarget_mb = 0\n";
        assert!(RecorderProfile::from_toml(off).is_ok(), "not validated while maintenance is off");
    }

    /// The memory knob reaches the same silent never-compacts through a different door.
    #[test]
    fn a_zero_row_budget_is_rejected_too() {
        let err =
            RecorderProfile::from_toml("store = \"/x\"\n\n[maintenance]\nmax_merge_rows = 0\n")
                .unwrap_err();
        assert!(matches!(err, ProfileError::ZeroMaxMergeRows), "{err}");
    }

    /// **An absent `[alerting]` table means DEFAULTS, not OFF** — same argument as `[maintenance]`
    /// above, and the sharper one: the condition the watchdog catches raises no error anywhere, so
    /// a recorder that had to be TOLD to watch would be the one nobody told.
    #[test]
    fn an_absent_alerting_table_means_defaults_not_off() {
        let p = RecorderProfile::from_toml("store = \"/x\"\n").unwrap();
        assert_eq!(p.alerting, Alerting::default());
        assert_eq!(p.alerting.repeat_secs, 3600);
        assert!(p.alerting.webhooks.is_empty(), "log-only until a target is named");
        assert_eq!(p.alerting.series_prefix, None, "unscoped: every subscribed series");
    }

    #[test]
    fn alerting_knobs_parse_and_a_partial_table_keeps_the_other_defaults() {
        let toml = "store = \"/x\"\n\n[alerting]\nwebhooks = [\"telegram\"]\n\
                    series_prefix = \"book/polymarket/\"\n";
        let p = RecorderProfile::from_toml(toml).unwrap();
        assert_eq!(p.alerting.webhooks, vec!["telegram".to_string()]);
        assert_eq!(p.alerting.series_prefix.as_deref(), Some("book/polymarket/"));
        assert_eq!(p.alerting.repeat_secs, 3600, "the untouched knob keeps its default");
    }

    /// `repeat_secs = 0` is legal and MEANS something (once per episode) — unlike the maintenance
    /// zeros above, which silently disable the thing they configure. Nothing to reject here.
    #[test]
    fn a_zero_repeat_is_a_legal_value_not_a_disabled_one() {
        let p =
            RecorderProfile::from_toml("store = \"/x\"\n\n[alerting]\nrepeat_secs = 0\n").unwrap();
        assert_eq!(p.alerting.repeat_secs, 0);
    }

    /// **A MISSPELLED KEY IS REFUSED BY NAME, in every table.** Serde's default is to DROP an
    /// unrecognised key, which on this file meant a one-character typo silently reconfigured the
    /// daemon: each case below was accepted, and started, before `deny_unknown_fields`.
    ///
    /// Driven per-table rather than once, because each table's `#[serde(default)]` fields make the
    /// drop invisible in a DIFFERENT way — see each row's comment for what used to happen.
    #[test]
    fn a_misspelled_key_is_refused_by_name_in_every_table() {
        // (profile body, the typo'd key the message must name)
        let cases = [
            // Top level: `subscribe` dropped ⇒ a daemon that recorded NOTHING and said so nowhere
            // (an empty profile is deliberately valid, so validation could not catch it either).
            ("store = \"/x\"\n\n[[subscription]]\nvenue = \"binance\"\n", "subscription"),
            // Subscription: silently fell back to `Backfill::Venue` — which on Polymarket cannot
            // restore the book, the lie `Backfill`'s own doc says nothing may tell.
            (
                "store = \"/x\"\n\n[[subscribe]]\nvenue = \"p\"\nfamily = \"f\"\n\
                 backfil = \"archive\"\n",
                "backfil",
            ),
            // Maintenance: the operator asked for 30 days and kept the tape forever.
            ("store = \"/x\"\n\n[maintenance]\nretention_day = 30\n", "retention_day"),
            // Alerting: the operator believed a PAGER was armed; delivery stayed log-only.
            ("store = \"/x\"\n\n[alerting]\nwebhook = [\"telegram\"]\n", "webhook"),
        ];
        for (body, key) in cases {
            let err = RecorderProfile::from_toml(body)
                .expect_err("an unknown key must be refused, not dropped");
            assert!(matches!(err, ProfileError::Parse(_)), "{key}: {err:?}");
            let msg = err.to_string();
            assert!(msg.contains("unknown field"), "{key}: must say what is wrong — {msg}");
            assert!(msg.contains(key), "{key}: the refusal must NAME the offending key — {msg}");
        }
    }

    /// …and the refusal is ACTIONABLE, not merely correct: `toml` lists the keys it WOULD have
    /// accepted, so the operator does not have to go and find this file to learn the spelling.
    #[test]
    fn the_refusal_lists_the_keys_that_would_have_been_accepted() {
        let err =
            RecorderProfile::from_toml("store = \"/x\"\n\n[alerting]\nwebhook = [\"telegram\"]\n")
                .unwrap_err()
                .to_string();
        for expected in ["webhooks", "repeat_secs", "series_prefix"] {
            assert!(err.contains(expected), "the message must offer `{expected}` — {err}");
        }
    }

    /// ⚠ THE GUARD ON THE ABOVE: the shipped example must still parse. A refusal that rejects the
    /// file this repo tells operators to copy would be a worse bug than the one it fixes, and the
    /// example is not otherwise compiled by anything.
    #[test]
    fn the_shipped_example_profile_still_parses() {
        let example = include_str!("../recorder.example.toml");
        let p = RecorderProfile::from_toml(example).expect("the shipped example must parse");
        assert_eq!(p.families(), vec![("polymarket".into(), "btc-updown-5m".into())]);
    }

    /// The knob reaches `CompactionConfig` — the whole point of the row bound is that the scheduler
    /// actually receives it. (`target_mb` was plumbed and ignored for as long as it existed.)
    #[test]
    fn the_row_budget_reaches_the_compaction_config() {
        let p = RecorderProfile::from_toml(
            "store = \"/x\"\n\n[maintenance]\nmax_merge_rows = 250000\n",
        )
        .unwrap();
        let (cfg, _) = p.maintenance.scheduler_args().expect("maintenance enabled");
        assert_eq!(cfg.compaction.max_merge_rows, 250_000);
        assert_eq!(
            Maintenance::default().max_merge_rows,
            1_000_000,
            "the default is the documented one"
        );
    }
}
