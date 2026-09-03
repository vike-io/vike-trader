//! The supervisor's declarative TOML roster: which series to keep fresh, with which collector, at
//! what cadence. Read-only `from_str` deserialization (no serialize-back) — the same shape
//! `vike-backtest`'s `BacktestProfile` uses, and the same `deny_unknown_fields` stance (a typo'd key
//! must be a hard parse error, never a silently-ignored setting).
//!
//! Shape:
//!
//! ```toml
//! # optional: overridden by the bin's --store / --status flags
//! store       = "/mnt/market_data/hist"
//! status_file = "/var/lib/vike/collector-supervisor.json"
//! tick_secs   = 30          # how often the loop wakes to re-evaluate which sources are due
//!
//! [[source]]
//! name         = "binance-majors-1m"
//! collector    = "binance_klines"   # must name a `registry::COLLECTORS` row
//! symbols      = ["BTCUSDT", "ETHUSDT"]
//! kind         = "bar"              # must match the collector's data kind
//! interval     = "1m"
//! cadence_secs = 300                # run this source at most every 5 minutes
//! lookback_ms  = 86_400_000         # OUTER bound of the freshness window (24h); the window itself
//!                                   # starts at the series watermark, so a current series refetches
//!                                   # only what it is missing
//! heal         = true               # also fill manifest-detected historical holes
//! max_heal_jobs = 8                 # at most N gap SUB-WINDOWS healed per pass (bounded work)
//! max_backoff_secs = 3600           # cap of the post-failure exponential backoff
//! max_consecutive_failures = 20     # optional: park the source after N straight failures
//! ```
//!
//! NOTE there is deliberately NO per-source `venue` override: each collector writes under its own
//! hard-coded `VENUE` const, so an override would make the gap lookup and the ingest target
//! disagree. The venue always comes from [`super::registry::Collector::venue`].

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use super::registry::collector_by_name;
use super::SupervisorError;

/// Default loop wake cadence (seconds) — how often the supervisor re-evaluates which sources are
/// due. Independent of any source's own `cadence_secs`; it just bounds the scheduling granularity.
pub const DEFAULT_TICK_SECS: u64 = 30;
/// Default per-source cadence (seconds): five minutes.
pub const DEFAULT_CADENCE_SECS: u64 = 300;
/// Default freshness lookback (ms): 24 hours. This is the OUTER BOUND of the freshness window, NOT
/// what is refetched every pass — [`super::heal::fresh_window`] anchors the window's start on the
/// series' stored watermark (`last_ts + interval`), so a healthy series fetches only the sliver
/// since its last stored bar and this bound binds only on a cold or long-stalled series.
///
/// It must NOT be read as "re-fetching an already-ingested window is free". The store's idempotency
/// is BATCH-COMMIT-KEY dedup (`{venue}:{symbol}:{interval}:{start}-{end}`), never per-row-value
/// dedup, so a rolling `[now - lookback, now]` window would mint a NEW key every pass and append the
/// same bars again — at this default and a 5-minute cadence, ~288x row amplification per day.
pub const DEFAULT_LOOKBACK_MS: i64 = 24 * 60 * 60 * 1000;
/// Default cap on gap sub-windows healed per pass — bounded work per pass, so a series with a
/// thousand missing days heals steadily instead of monopolizing one pass. Counted in
/// [`super::heal::HEAL_CHUNK_MS`]-wide chunks, not in `series_gaps` ranges (which collapse
/// consecutive missing days, so one range can be years wide).
pub const DEFAULT_MAX_HEAL_JOBS: usize = 8;
/// Default cap of the post-failure exponential backoff (seconds): one hour.
pub const DEFAULT_MAX_BACKOFF_SECS: u64 = 3600;
/// Default series kind — every shipped collector produces OHLCV bars.
pub const DEFAULT_KIND: &str = "bar";
/// Default bar interval.
pub const DEFAULT_INTERVAL: &str = "1m";

fn default_tick_secs() -> u64 {
    DEFAULT_TICK_SECS
}
fn default_cadence_secs() -> u64 {
    DEFAULT_CADENCE_SECS
}
fn default_lookback_ms() -> i64 {
    DEFAULT_LOOKBACK_MS
}
fn default_max_heal_jobs() -> usize {
    DEFAULT_MAX_HEAL_JOBS
}
fn default_max_backoff_secs() -> u64 {
    DEFAULT_MAX_BACKOFF_SECS
}
fn default_kind() -> String {
    DEFAULT_KIND.to_string()
}
fn default_interval() -> String {
    DEFAULT_INTERVAL.to_string()
}
fn default_true() -> bool {
    true
}

/// The whole supervisor config: global knobs plus the `[[source]]` roster.
///
/// [`Default`] is hand-written (not derived) so it is the INERT config — zero sources, a sane tick
/// — matching what an empty TOML file deserializes to. That equivalence is the off-path pin.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorConfig {
    /// Hist-store root. Lowest precedence — the bin's `--store` flag and `$VIKE_HIST_STORE` win.
    #[serde(default)]
    pub store: Option<String>,
    /// Where to write the JSON status surface. `None` (and no `--status`) = no status file.
    #[serde(default)]
    pub status_file: Option<String>,
    /// How often the loop wakes to re-evaluate due-ness. Clamped to at least 1s at spawn.
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// The roster, from the TOML `[[source]]` array-of-tables.
    #[serde(default, rename = "source")]
    pub sources: Vec<SourceConfig>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self { store: None, status_file: None, tick_secs: DEFAULT_TICK_SECS, sources: Vec::new() }
    }
}

/// One declared source: a `(collector, symbols, kind, interval)` series family plus its cadence,
/// freshness lookback, heal policy and failure policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    /// Operator-facing label; must be unique across the roster (it keys the status rows).
    pub name: String,
    /// A [`super::registry::COLLECTORS`] row name (e.g. `"binance_klines"`).
    pub collector: String,
    /// The symbols to keep fresh, in the collector's own symbol vocabulary (e.g. `"BTCUSDT"`).
    pub symbols: Vec<String>,
    /// Series kind; must equal the collector's [`super::registry::Collector::kind`].
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Bar interval (e.g. `"1m"`), validated against [`vike_model::time::interval_ms`].
    #[serde(default = "default_interval")]
    pub interval: String,
    /// Minimum spacing between two passes over this source.
    #[serde(default = "default_cadence_secs")]
    pub cadence_secs: u64,
    /// How far back the freshness window may reach, ending at "now" — an OUTER bound, not a
    /// per-pass refetch span (see [`DEFAULT_LOOKBACK_MS`]).
    #[serde(default = "default_lookback_ms")]
    pub lookback_ms: i64,
    /// Whether to also heal manifest-detected historical gaps (the point of the feature).
    #[serde(default = "default_true")]
    pub heal: bool,
    /// Cap on gap sub-windows healed per pass per symbol (see [`DEFAULT_MAX_HEAL_JOBS`]).
    #[serde(default = "default_max_heal_jobs")]
    pub max_heal_jobs: usize,
    /// Cap of the post-failure exponential backoff.
    #[serde(default = "default_max_backoff_secs")]
    pub max_backoff_secs: u64,
    /// Park the source once it has failed this many passes in a row. `None` (the default) or `0` =
    /// never park; retries continue forever at the capped backoff.
    #[serde(default)]
    pub max_consecutive_failures: Option<u32>,
}

impl SourceConfig {
    /// `cadence_secs` in ms, saturating rather than wrapping on an absurd config value.
    pub fn cadence_ms(&self) -> i64 {
        i64::try_from(self.cadence_secs).unwrap_or(i64::MAX).saturating_mul(1000)
    }

    /// `max_backoff_secs` in ms, saturating rather than wrapping on an absurd config value.
    pub fn max_backoff_ms(&self) -> i64 {
        i64::try_from(self.max_backoff_secs).unwrap_or(i64::MAX).saturating_mul(1000)
    }
}

/// Check every supervisor invariant a bad config could violate. Called by
/// [`parse_supervisor_config`] and again at spawn, so a hand-built config can't skip it.
pub fn validate(cfg: &SupervisorConfig) -> Result<(), SupervisorError> {
    if cfg.tick_secs == 0 {
        return Err(SupervisorError::Invalid("tick_secs must be > 0".to_string()));
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for src in &cfg.sources {
        if src.name.trim().is_empty() {
            return Err(SupervisorError::Invalid(
                "every [[source]] needs a non-empty name".to_string(),
            ));
        }
        if !seen.insert(src.name.as_str()) {
            return Err(SupervisorError::Invalid(format!("duplicate source name {:?}", src.name)));
        }
        let Some(collector) = collector_by_name(&src.collector) else {
            let known: Vec<&str> = super::registry::COLLECTORS.iter().map(|c| c.name).collect();
            return Err(SupervisorError::Invalid(format!(
                "source {:?}: unknown collector {:?} (have: {})",
                src.name,
                src.collector,
                known.join(", ")
            )));
        };
        if src.kind != collector.kind {
            return Err(SupervisorError::Invalid(format!(
                "source {:?}: collector {:?} produces kind {:?}, not {:?}",
                src.name, src.collector, collector.kind, src.kind
            )));
        }
        if src.symbols.is_empty() || src.symbols.iter().any(|s| s.trim().is_empty()) {
            return Err(SupervisorError::Invalid(format!(
                "source {:?}: symbols must be a non-empty list",
                src.name
            )));
        }
        if src.cadence_secs == 0 {
            return Err(SupervisorError::Invalid(format!(
                "source {:?}: cadence_secs must be > 0",
                src.name
            )));
        }
        if src.lookback_ms <= 0 {
            return Err(SupervisorError::Invalid(format!(
                "source {:?}: lookback_ms must be > 0",
                src.name
            )));
        }
        if vike_model::time::interval_ms(&src.interval).is_none() {
            return Err(SupervisorError::Invalid(format!(
                "source {:?}: interval {:?} is not a recognized bar step",
                src.name, src.interval
            )));
        }
    }
    Ok(())
}

/// Parse + validate a supervisor config from TOML text. Pure — no filesystem, no network.
pub fn parse_supervisor_config(text: &str) -> Result<SupervisorConfig, SupervisorError> {
    let cfg: SupervisorConfig =
        toml::from_str(text).map_err(|e| SupervisorError::Parse(e.to_string()))?;
    validate(&cfg)?;
    Ok(cfg)
}

/// Read + [`parse_supervisor_config`] a config file.
pub fn load_supervisor_config(path: &Path) -> Result<SupervisorConfig, SupervisorError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SupervisorError::Io(format!("read {}: {e}", path.display())))?;
    parse_supervisor_config(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_config_is_the_inert_default() {
        // THE OFF-PATH PIN: an empty file (and `Default`) both mean "no sources declared", so a
        // pass has nothing to dispatch and the supervisor is a no-op over the store.
        let cfg = parse_supervisor_config("").unwrap();
        assert!(cfg.sources.is_empty());
        assert_eq!(cfg.tick_secs, DEFAULT_TICK_SECS);
        assert_eq!(cfg.store, None);
        assert_eq!(cfg.status_file, None);
        assert_eq!(cfg, SupervisorConfig::default());
    }

    #[test]
    fn a_full_config_parses_every_field() {
        let cfg = parse_supervisor_config(
            r#"
store       = "/market_data/hist"
status_file = "/var/run/sup.json"
tick_secs   = 15

[[source]]
name         = "binance-majors-1m"
collector    = "binance_klines"
symbols      = ["BTCUSDT", "ETHUSDT"]
kind         = "bar"
interval     = "1m"
cadence_secs = 60
lookback_ms  = 7200000
heal         = true
max_heal_jobs = 3
max_backoff_secs = 900
max_consecutive_failures = 5
"#,
        )
        .unwrap();
        assert_eq!(cfg.store.as_deref(), Some("/market_data/hist"));
        assert_eq!(cfg.status_file.as_deref(), Some("/var/run/sup.json"));
        assert_eq!(cfg.tick_secs, 15);
        assert_eq!(cfg.sources.len(), 1);
        let s = &cfg.sources[0];
        assert_eq!(s.name, "binance-majors-1m");
        assert_eq!(s.collector, "binance_klines");
        assert_eq!(s.symbols, vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
        assert_eq!(s.kind, "bar");
        assert_eq!(s.interval, "1m");
        assert_eq!(s.cadence_secs, 60);
        assert_eq!(s.lookback_ms, 7_200_000);
        assert!(s.heal);
        assert_eq!(s.max_heal_jobs, 3);
        assert_eq!(s.max_backoff_secs, 900);
        assert_eq!(s.max_consecutive_failures, Some(5));
        assert_eq!(s.cadence_ms(), 60_000);
        assert_eq!(s.max_backoff_ms(), 900_000);
    }

    #[test]
    fn a_minimal_source_takes_every_default() {
        let cfg = parse_supervisor_config(
            r#"
[[source]]
name      = "okx-btc"
collector = "okx_klines"
symbols   = ["BTC-USDT"]
"#,
        )
        .unwrap();
        let s = &cfg.sources[0];
        assert_eq!(s.kind, DEFAULT_KIND);
        assert_eq!(s.interval, DEFAULT_INTERVAL);
        assert_eq!(s.cadence_secs, DEFAULT_CADENCE_SECS);
        assert_eq!(s.lookback_ms, DEFAULT_LOOKBACK_MS);
        assert!(s.heal, "gap-heal is ON by default — it is the point of the supervisor");
        assert_eq!(s.max_heal_jobs, DEFAULT_MAX_HEAL_JOBS);
        assert_eq!(s.max_backoff_secs, DEFAULT_MAX_BACKOFF_SECS);
        assert_eq!(s.max_consecutive_failures, None);
    }

    #[test]
    fn multiple_sources_keep_declaration_order() {
        let cfg = parse_supervisor_config(
            r#"
[[source]]
name = "a"
collector = "binance_klines"
symbols = ["BTCUSDT"]

[[source]]
name = "b"
collector = "bybit_klines"
symbols = ["BTCUSDT"]

[[source]]
name = "c"
collector = "okx_klines"
symbols = ["BTC-USDT"]
"#,
        )
        .unwrap();
        let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn an_unknown_key_is_a_hard_parse_error() {
        // deny_unknown_fields: a typo must never silently no-op.
        let e = parse_supervisor_config("tick_secsss = 5").unwrap_err();
        assert!(matches!(e, SupervisorError::Parse(_)), "got {e:?}");
        let e = parse_supervisor_config(
            r#"
[[source]]
name = "a"
collector = "binance_klines"
symbols = ["BTCUSDT"]
cadence_sec = 60
"#,
        )
        .unwrap_err();
        assert!(matches!(e, SupervisorError::Parse(_)), "got {e:?}");
    }

    #[test]
    fn an_unknown_collector_is_rejected_with_the_known_list() {
        let e = parse_supervisor_config(
            r#"
[[source]]
name = "a"
collector = "nasdaq_klines"
symbols = ["AAPL"]
"#,
        )
        .unwrap_err();
        let msg = e.to_string();
        assert!(matches!(e, SupervisorError::Invalid(_)), "got {e:?}");
        assert!(msg.contains("nasdaq_klines"), "{msg}");
        assert!(msg.contains("binance_klines"), "the error names the known collectors: {msg}");
    }

    #[test]
    fn a_duplicate_source_name_is_rejected() {
        let e = parse_supervisor_config(
            r#"
[[source]]
name = "dup"
collector = "binance_klines"
symbols = ["BTCUSDT"]

[[source]]
name = "dup"
collector = "okx_klines"
symbols = ["BTC-USDT"]
"#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("duplicate source name"), "{e}");
    }

    #[test]
    fn a_kind_the_collector_does_not_produce_is_rejected() {
        let e = parse_supervisor_config(
            r#"
[[source]]
name = "a"
collector = "binance_klines"
symbols = ["BTCUSDT"]
kind = "trade"
"#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("produces kind"), "{e}");
    }

    #[test]
    fn zero_cadence_empty_symbols_bad_interval_and_zero_tick_are_all_rejected() {
        let base = |extra: &str| {
            format!(
                r#"
[[source]]
name = "a"
collector = "binance_klines"
symbols = ["BTCUSDT"]
{extra}
"#
            )
        };
        assert!(parse_supervisor_config(&base("cadence_secs = 0")).is_err());
        assert!(parse_supervisor_config(&base("lookback_ms = 0")).is_err());
        assert!(parse_supervisor_config(&base("lookback_ms = -1")).is_err());
        assert!(parse_supervisor_config(&base("interval = \"nope\"")).is_err());
        assert!(parse_supervisor_config("tick_secs = 0").is_err());
        assert!(parse_supervisor_config(
            r#"
[[source]]
name = "a"
collector = "binance_klines"
symbols = []
"#
        )
        .is_err());
    }

    #[test]
    fn validate_accepts_the_hand_built_default() {
        assert!(validate(&SupervisorConfig::default()).is_ok());
    }
}
