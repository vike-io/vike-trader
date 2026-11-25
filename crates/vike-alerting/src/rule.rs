//! The PERSISTED alerting rule model — what [`crate::persist`] saves/loads. Pure data + serde; no
//! evaluation logic (that is `crate::evaluation`) and no I/O.
//!
//! **Version and byte-identical discipline** (`super::persist`'s module doc restates it): a file of
//! another [`CURRENT_VERSION`] is refused whole, never read with empty fields. Inside a version a
//! field is `#[serde(default)]` (and `skip_serializing_if` where an absent key is the common case),
//! so a rule that uses none of a field's feature writes no key for it. An EMPTY
//! [`AlertRuleSet`] (the default — no rules configured) is the OFF state: nothing is evaluated and
//! nothing is delivered, byte-identical to a build without the alerting engine.

use serde::{Deserialize, Serialize};

/// `#[serde(default)]` helper — a bool field whose absent-key default is `true`
/// ([`AlertRule::enabled`], [`AlertTargets::in_process`]).
fn default_true() -> bool {
    true
}

/// Direction of a scalar (price / indicator) threshold crossing. Both are EDGE-triggered — the
/// evaluator fires on the transition ONTO the trigger side, never every tick the value stays there
/// (see `crate::evaluation`). This is the "price cross / level" and "indicator threshold"
/// shape: an alert for "BTC crosses above 100k" (`Above`) or "RSI falls below 30" (`Below`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    /// Fires when the value rises strictly above the bound (previous sample `<= bound`, now `>`).
    Above,
    /// Fires when the value falls strictly below the bound (previous sample `>= bound`, now `<`).
    Below,
}

impl std::fmt::Display for Compare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Compare::Above => "crossed above",
            Compare::Below => "crossed below",
        })
    }
}

/// Which side of a feed-health transition a [`RuleTrigger::Feed`] listens for. The off-fold
/// consumer maps a `vike_model::StreamStatus` (GapStart/Stale ⇒ `Degraded`,
/// Live ⇒ `Recovered`) onto a `crate::evaluation::AlertSignal::Feed` at its sink boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FeedState {
    /// A transport gap opened or data went stale (`StreamStatus::GapStart` / `Stale`).
    #[default]
    Degraded,
    /// The feed is live again (`StreamStatus::Live`).
    Recovered,
}

/// The condition a rule triggers on. Internally tagged (`"kind"`) so the JSON is self-describing;
/// an unknown `kind` refuses the file ([`crate::persist::load_path`]), and the daemon runs no rule.
///
/// Trigger inputs by evaluation entry point (all OFF the hot fold — see the crate contract):
/// - snapshot-driven (`crate::evaluation::evaluation_snapshot_rule`):
///   [`Price`](Self::Price), [`Drawdown`](Self::Drawdown),
///   [`ReconciliationAlert`](Self::ReconciliationAlert),
///   [`StaleRate`](Self::StaleRate);
/// - indicator-sample-driven: [`Indicator`](Self::Indicator);
/// - typed-event-driven (`crate::evaluation::evaluation_event_rule`):
///   [`Fill`](Self::Fill), [`OrderRejected`](Self::OrderRejected), [`JournalGrowth`](Self::JournalGrowth),
///   [`JournalFenceHalt`](Self::JournalFenceHalt);
/// - status-signal-driven (`crate::evaluation::evaluation_signal_rule`):
///   [`Feed`](Self::Feed), [`FillRateBreaker`](Self::FillRateBreaker),
///   [`PolymarketResolution`](Self::PolymarketResolution), [`SeriesStale`](Self::SeriesStale),
///   [`SeriesSlow`](Self::SeriesSlow), [`FamilyCollapse`](Self::FamilyCollapse),
///   [`ModelDrift`](Self::ModelDrift).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleTrigger {
    /// The LAST TRADED price of `(venue, symbol)` (`SnapshotFacts::last_price`, the line a chart draws, never a
    /// mid) crosses `level` (edge-triggered; see [`Compare`]).
    Price { venue: String, symbol: String, comparison: Compare, level: f64 },
    /// A streaming-indicator output value for `(venue, symbol)` crosses `threshold`. The off-fold
    /// consumer computes the value over the streaming indicator registry
    /// (`vike_indicators`/`vike_chart::chart::study`) and feeds it as an
    /// `crate::evaluation::IndicatorSample` — this crate stores only the
    /// registry key + output index + params so the rule round-trips without a bar dependency.
    Indicator {
        venue: String,
        symbol: String,
        /// vike-indicators registry key (e.g. `"rsi"`, `"macd"`).
        indicator: String,
        /// which output line of a multi-output indicator to test (0 = first).
        #[serde(default)]
        output: usize,
        /// live parameter overrides, index-aligned to the registry `params` (empty = defaults).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        params: Vec<f64>,
        comparison: Compare,
        threshold: f64,
    },
    /// A fill occurred, optionally scoped to a `venue`/`symbol` (both `None` ⇒ any fill).
    Fill {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        venue: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        symbol: Option<String>,
    },
    /// An order was rejected by the venue (`OrderRejected`) or denied by the RiskGate
    /// (`OrderDenied`). The event names the order client id, its venue and the reason for the body; the
    /// rule has no venue/symbol scope. A unit variant — serialized `{"kind":"order_rejected"}`.
    OrderRejected,
    /// **The node's write-ahead journal went over its size or growth alert**
    /// (`crate::evaluation::AlertEvent::JournalGrowth`). The CALLER measures, holds the two thresholds (the
    /// tradehub's `config.journal_size_alert_mib` and `config.journal_growth_alert_mib_per_hour` rows) and sends one
    /// event per crossing, so the rule fires once per episode with no latch of its own. A unit variant, serialized
    /// `{"kind":"journal_growth"}`.
    JournalGrowth,
    /// **The node HALTED because its write-ahead journal could not write** (decision 0279's addendum of 2026-11-23;
    /// `crate::evaluation::AlertEvent::JournalFenceHalt`): every account stays halted until the node restarts. The
    /// CALLER sends one event per halt, so the rule fires once per process with no latch of its own; the body is the
    /// node's own words (what happens, what to do). A unit variant, serialized `{"kind":"journal_fence_halt"}`.
    JournalFenceHalt,
    /// The daemon's OWN equity curve (`vike_execution::Portfolio::drawdown_curve` — configured capital
    /// plus realized + unrealized P&L) draws down by at least `fraction` (0.0..=1.0) from the core latch's own
    /// high-water mark (`SnapshotFacts::drawdown_peak`), or from the rule's running session peak while there is
    /// none (no `max_drawdown`, or before the latch's first sweep). LATCHED: fires once on breach and re-arms
    /// only after the curve recovers back under `fraction`.
    ///
    /// ⚠ NOT the cross-venue equity TOTAL. This deliberately tracks the same quantity
    /// `vike_core`'s own liquidate-only latch does (per CURRENCY GROUP: below), so the two measure one
    /// curve; the total includes each live venue's WALLET, which a third party can move. The `Drawdown`
    /// arm of [`crate::evaluation::evaluation_snapshot_rule`] carries the measurement.
    ///
    /// ⚠ **One currency per curve (decision 0183, item 13).** The latch folds a curve per currency group of
    /// the node's engines and trips when ANY group breaches its own threshold, because a curve may add only
    /// figures of one currency. This rule watches the PRIMARY group (the first live engine's) through the
    /// caller's `drawdown_curve`, and every OTHER group through `SnapshotFacts::group_curves` (the curve and
    /// high-water mark the latch last measured for it), firing on the rising edge of each group's own breach
    /// with the group named. `fraction` is the share of EACH group's own curve, not of a sum across groups; it
    /// is not the latch's `max_drawdown` and the two can differ.
    Drawdown { fraction: f64 },
    /// A held reconcile alert is present (a `ReconcileAlertView` in the snapshot's `ReconcileBlock`),
    /// optionally filtered to a `ReconcileDivergenceKind` label (`ReconcileAlertView::kind`). LATCHED like
    /// [`Drawdown`](Self::Drawdown): fires on the rising edge (no matching alert → one present).
    ReconciliationAlert {
        // NB: named `divergence_kind`, NOT `kind` — a field serializing to "kind" would collide
        // with this enum's `#[serde(tag = "kind")]` internal tag (a serde compile error).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        divergence_kind: Option<String>,
    },
    /// **A conversion rate is stale while a position is open in its currency** (decision 0183): an account
    /// that holds a symbol quoted in a currency other than its own values it through a rate, and a rate the
    /// node stopped renewing leaves the position valued at the last one, silently (an opening order on that
    /// currency is denied, a reduce is not, and the sweeps keep running on the old rate). The rows the caller
    /// hands over ([`crate::evaluation::StaleRateFact`]) are exactly the stale rates an open position is valued at,
    /// so a stale rate nobody holds anything in never fires it. LATCHED like [`Drawdown`](Self::Drawdown):
    /// ONE alert on the rising edge (none stale-and-held, then at least one), re-armed when every held rate is
    /// fresh again; a second currency going stale while the first still is does not page again. A unit
    /// variant, serialized `{"kind":"stale_rate"}`.
    StaleRate,
    /// A market-data feed changed health (`StreamHealth`), optionally scoped to a `venue`. `state`
    /// selects which transition to fire on (Degraded vs Recovered).
    Feed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        venue: Option<String>,
        #[serde(default)]
        state: FeedState,
    },
    /// A maker per-side fill-rate circuit breaker tripped (the `SpreadMaker` breaker), optionally
    /// scoped to `venue`/`symbol`.
    FillRateBreaker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        venue: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        symbol: Option<String>,
    },
    /// A watched Polymarket market resolved, optionally scoped to a `token_id` (else any watched
    /// resolution).
    PolymarketResolution {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_id: Option<String>,
    },
    /// **A recorded SERIES has stopped receiving rows** — the writer-stopped-writing condition, and
    /// the reason this crate was split out of `vike-ops` in the first place.
    ///
    /// It is deliberately NOT [`Feed`](Self::Feed). Feed health is VENUE-scoped and reports a
    /// transport transition: a socket that gapped or went stale. This one is SERIES-scoped and
    /// reports an absence — the socket is `ESTABLISHED`, the subscription returned `Ok`, no error is
    /// raised and no row is lost, because there are no rows. A venue-scoped trigger cannot name
    /// which of a recorder's series went quiet, and "which" is the entire diagnosis.
    ///
    /// `series_prefix` is a PREFIX match on the producer's series key, not an equality test. The
    /// key that reaches this rule is the recorder's `"{kind}/{venue}/{symbol}"`
    /// (`vike_store::RecorderHandle::liveness`'s), and on a venue whose symbols ROTATE — a Polymarket
    /// up/down family mints new token ids every 5 minutes — an exact name is unusable as a scope by
    /// construction: the series an operator wants to watch does not exist yet when the rule is
    /// written. `"book/polymarket/"` covers every token of every window; `None` covers everything.
    SeriesStale {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_prefix: Option<String>,
    },
    /// **A recorded SERIES is receiving rows, and receiving far too few of them.**
    ///
    /// [`SeriesStale`](Self::SeriesStale)'s blind spot, and it is not a small one: a binance perp
    /// depth lane recorded at 4 % of its declared cadence for FORTY DAYS while never going 30 s
    /// without a row, so the stale rule read green through the whole of it. Absence is easy to
    /// detect and a shortfall is not — a shortfall needs a number to compare against
    /// (`vike_data::store::series_cadence`).
    ///
    /// `series_prefix` is a PREFIX match with exactly [`SeriesStale`](Self::SeriesStale)'s
    /// semantics and for the same reason — read it there.
    SeriesSlow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_prefix: Option<String>,
    },
    /// **A recorded FAMILY has stopped producing, judged against its OWN recent history.**
    ///
    /// The gap [`SeriesSlow`](Self::SeriesSlow) structurally cannot reach, and it is the incident
    /// that motivated the whole watchdog effort: on 2026-08-05 a Polymarket `book` family collapsed
    /// to 782 rows over eight minutes — five of those minutes at literally zero rows, the longest
    /// consecutive dark run being two — and nothing said anything, because
    /// `vike_data::store::series_cadence` classifies that lane `EventDriven` — the market sets the rate,
    /// so no ceiling is derivable and `SeriesSlow` correctly refuses to invent one.
    ///
    /// So this trigger carries NO per-second expectation at all, deliberately. Its producer
    /// (`vike_recorder::liveness::SilenceWatch::family_collapse`) compares an ITEM COUNT over a
    /// stated window against a rolling median of that same family's own recent windows, and the
    /// signal ([`crate::evaluation::AlertSignal::FamilyCollapse`]) carries both raw counts plus the
    /// out-of-family witness that licensed the verdict. A reader who mistakes a learned baseline
    /// for a declared cadence would be reading an invented number as an authority, which is
    /// exactly what `SeriesSlow`'s `expected_per_s` exists to protect against — hence a separate
    /// variant rather than a second reading of that one.
    ///
    /// **Why a FAMILY and not a series.** A Polymarket instrument lives ~600 s and then dies ON
    /// PURPOSE; its own rate legitimately spans two orders of magnitude inside one life. The family
    /// key is the only subject with continuous existence across a rotation, so a dying member's
    /// tail and its successor's birth land in one total and ~576 legitimate instrument deaths a day
    /// are invisible by construction rather than by an exemption somebody maintains.
    ///
    /// `series_prefix` is a PREFIX match with exactly [`SeriesStale`](Self::SeriesStale)'s
    /// semantics — read it there. It is matched against the FAMILY key
    /// (`{kind}/{venue}/{family}`), which shares its first two segments with every member's series
    /// key, so a profile scoped to `"book/polymarket/"` scopes all three series rules alike.
    FamilyCollapse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_prefix: Option<String>,
    },
    /// **A live model's forecasts DRIFTED from what it published over its training window** (book W9,
    /// chapter 54). The CALLER measures (this crate names no statistic's code): it reads the forecasts the
    /// live strategies published for `(venue, symbol)` over the last `window_ms` (optionally one mount's,
    /// `mount_id`), compares them with the forecasts saved in the `vike-runs` run `reference_run` (a backtest
    /// of the model over its training window), and hands the numbers over as
    /// [`crate::evaluation::AlertSignal::ModelDrift`], keyed by this rule's id.
    ///
    /// It DRIFTED when the population stability index is at least `population_stability_index_above`, or
    /// the two-sample Kolmogorov–Smirnov p-value is below `kolmogorov_smirnov_p_value_below`, or, with an
    /// `error` block, the realised error's CUSUM against the reference's mean error raised an alarm. LATCHED like [`Drawdown`](Self::Drawdown):
    /// ONE alert on the rising edge, re-armed when a later reading is stable again. A reading over fewer
    /// than `min_forecasts` live forecasts is no verdict at all: it neither fires nor re-arms.
    ///
    /// OFF unless a rule names it, as every trigger: no rule, no read, no measurement.
    ModelDrift {
        venue: String,
        symbol: String,
        /// Only this mount's forecasts (`vike_data::ExecForecastRow::mount_id`); `None` = every mount's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mount_id: Option<String>,
        /// The `vike-runs` run id whose saved forecasts (`forecasts.json`) are the training-window reference.
        reference_run: String,
        /// How far back the live window reaches from the reading's time, in milliseconds.
        window_ms: i64,
        /// The fewest live forecasts a verdict is taken on. Default [`DEFAULT_MIN_FORECASTS`].
        #[serde(default = "default_min_forecasts")]
        min_forecasts: usize,
        /// Drifted at a population stability index of at least this. Default
        /// [`DEFAULT_POPULATION_STABILITY_INDEX_ABOVE`].
        #[serde(default = "default_population_stability_index_above")]
        population_stability_index_above: f64,
        /// Drifted at a Kolmogorov–Smirnov p-value below this. Default
        /// [`DEFAULT_KOLMOGOROV_SMIRNOV_P_VALUE_BELOW`].
        #[serde(default = "default_kolmogorov_smirnov_p_value_below")]
        kolmogorov_smirnov_p_value_below: f64,
        /// The realised-error check, when the forecast is a return over its horizon; `None` = none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ForecastErrorDrift>,
    },
}

/// The fewest live forecasts a [`RuleTrigger::ModelDrift`] verdict is taken on, by default: below it a PSI
/// and a KS p-value are mostly sampling noise.
pub const DEFAULT_MIN_FORECASTS: usize = 30;
/// [`RuleTrigger::ModelDrift`]'s default population stability index bound: the customary "major shift"
/// reading.
pub const DEFAULT_POPULATION_STABILITY_INDEX_ABOVE: f64 = 0.25;
/// [`RuleTrigger::ModelDrift`]'s default Kolmogorov–Smirnov p-value bound.
pub const DEFAULT_KOLMOGOROV_SMIRNOV_P_VALUE_BELOW: f64 = 0.01;

fn default_min_forecasts() -> usize {
    DEFAULT_MIN_FORECASTS
}

fn default_population_stability_index_above() -> f64 {
    DEFAULT_POPULATION_STABILITY_INDEX_ABOVE
}

fn default_kolmogorov_smirnov_p_value_below() -> f64 {
    DEFAULT_KOLMOGOROV_SMIRNOV_P_VALUE_BELOW
}

/// The realised-error half of a [`RuleTrigger::ModelDrift`] rule: the forecast read as the RETURN expected over
/// its horizon (the book's convention, `vike_model::Forecast`'s doc), its error the absolute gap to the return
/// that followed, priced by the `(venue, symbol)` bars of `interval`. One-sided CUSUM (Page) over the live errors
/// against the reference run's mean error: `allowance` and `threshold` are in the error's unit (a return).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForecastErrorDrift {
    /// The bar interval the realised returns are priced on (`"1m"`, `"1h"`).
    pub interval: String,
    /// The CUSUM's slack `k`: a rise of the mean error smaller than this is tolerated.
    pub allowance: f64,
    /// The CUSUM's alarm level `h`.
    pub threshold: f64,
}

/// Where a fired alert is delivered. A rule with the default (`in_process: true`, no webhooks)
/// surfaces only in-app (the in-process buffer). Webhook names resolve against the runtime-built
/// [`WebhookConfig`](crate::delivery::WebhookConfig) registry (built from the caller's credential
/// map). A name with no matching config would deliver nowhere, so the tradehub's loader switches such
/// a rule off and says so at `error` (`crates/vike-tradehub/src/alerts.rs`'s `refuse_unconfigured_targets`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertTargets {
    /// deliver to the in-process sink (toast / OS notification). Default `true`.
    #[serde(default = "default_true")]
    pub in_process: bool,
    /// named webhook targets (Telegram / generic) to POST to. Empty ⇒ none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webhooks: Vec<String>,
}

impl Default for AlertTargets {
    fn default() -> Self {
        AlertTargets { in_process: true, webhooks: Vec::new() }
    }
}

/// One persisted alert rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    /// stable unique id — the per-rule evaluation-state key, the cooldown/once key, and the
    /// delivery correlation id. An editor mints it; keep it stable across edits of a rule.
    pub id: String,
    /// human label included in the delivered message. May be empty.
    #[serde(default)]
    pub name: String,
    /// a disabled rule is persisted but never evaluated (byte-identical to its absence).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// the condition.
    pub trigger: RuleTrigger,
    /// delivery targets. Absent ⇒ [`AlertTargets::default`] (in-process only).
    #[serde(default)]
    pub targets: AlertTargets,
    /// minimum ms between re-fires of the SAME rule (0 = no cooldown). For a re-notifying LATCHED
    /// trigger this is redundant (the latch already prevents re-fire until re-arm); it is the
    /// primary rate limit for discrete-event triggers (fill / reject / signal).
    #[serde(default)]
    pub cooldown_ms: i64,
    /// fire at most once per process session (until the engine is rebuilt), regardless of cooldown.
    #[serde(default)]
    pub once: bool,
}

impl AlertRule {
    /// A minimal enabled rule with the given id + trigger and default targets — a constructor for
    /// callers/tests that don't set name/cooldown/once.
    pub fn new(id: impl Into<String>, trigger: RuleTrigger) -> Self {
        AlertRule {
            id: id.into(),
            name: String::new(),
            enabled: true,
            trigger,
            targets: AlertTargets::default(),
            cooldown_ms: 0,
            once: false,
        }
    }
}

/// The persisted container — the whole rule set. Empty is the OFF default (see the module doc).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRuleSet {
    /// The file's version, [`CURRENT_VERSION`]: a file of another version is refused whole
    /// ([`crate::persist::load_path`]), so the key is required.
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<AlertRule>,
}

/// Current [`AlertRuleSet::version`]. Version 2 spells every word out: `comparison` (was `op`),
/// `fraction` (was `pct`), `reconciliation_alert` (was `recon_alert`).
pub const CURRENT_VERSION: u32 = 2;

impl Default for AlertRuleSet {
    fn default() -> Self {
        AlertRuleSet { version: CURRENT_VERSION, rules: Vec::new() }
    }
}

impl AlertRuleSet {
    /// No rules configured — the OFF state (`skip_serializing_if` uses it so an empty set writes
    /// nothing when embedded, and the engine short-circuits on it).
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[path = "rule_tests.rs"]
#[cfg(test)]
mod rule_tests;
