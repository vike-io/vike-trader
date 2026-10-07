//! The PERSISTED alerting rule model — what [`crate::persist`] saves/loads. Pure data + serde; no
//! evaluation logic (that is `crate::eval`) and no I/O.
//!
//! **Forward-compat / byte-identical discipline** (the workspace-v2 shape, `super::persist`'s
//! module doc restates it): every field added over time is `#[serde(default)]` (and
//! `skip_serializing_if` where an absent key is the common case) so an older `alerts.json` still
//! loads and a rule that uses none of a field's feature writes no key for it. An EMPTY
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
/// (see `crate::eval`). This is the "price cross / level" and "indicator threshold"
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
/// consumer maps a `vike_bridge_core::stream_health::HealthEvent` (Gap/Stale ⇒ `Degraded`,
/// Live ⇒ `Recovered`) onto a `crate::eval::AlertSignal::Feed` at its sink boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FeedState {
    /// A transport gap opened or data went stale (`HealthEvent::Gap` / `Stale`).
    #[default]
    Degraded,
    /// The feed is live again (`HealthEvent::Live`).
    Recovered,
}

/// The condition a rule triggers on. Internally tagged (`"kind"`) so the JSON is self-describing
/// and forward-compatible — an unknown `kind` from a newer build simply fails to parse THAT rule,
/// never the whole file (see [`crate::persist::load_path`]'s never-brick contract).
///
/// Trigger inputs by evaluation entry point (all OFF the hot fold — see the crate contract):
/// - snapshot-driven (`crate::eval::eval_snapshot_rule`):
///   [`Price`](Self::Price), [`Drawdown`](Self::Drawdown), [`ReconAlert`](Self::ReconAlert);
/// - indicator-sample-driven: [`Indicator`](Self::Indicator);
/// - typed-event-driven (`crate::eval::eval_event_rule`):
///   [`Fill`](Self::Fill), [`OrderRejected`](Self::OrderRejected);
/// - status-signal-driven (`crate::eval::eval_signal_rule`):
///   [`Feed`](Self::Feed), [`FillRateBreaker`](Self::FillRateBreaker),
///   [`PolymarketResolution`](Self::PolymarketResolution), [`SeriesStale`](Self::SeriesStale),
///   [`SeriesSlow`](Self::SeriesSlow), [`FamilyCollapse`](Self::FamilyCollapse).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleTrigger {
    /// The published mark for `(venue, symbol)` crosses `level` (edge-triggered; see [`Compare`]).
    Price { venue: String, symbol: String, op: Compare, level: f64 },
    /// A streaming-indicator output value for `(venue, symbol)` crosses `threshold`. The off-fold
    /// consumer computes the value over the streaming indicator registry
    /// (`vike_indicators`/`vike_chart::indicators`) and feeds it as an
    /// `crate::eval::IndicatorSample` — this crate stores only the
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
        op: Compare,
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
    /// (`OrderDenied`). Reject/deny events carry only a client-order-id + reason, so there is no
    /// venue/symbol scope here. A unit variant — serialized `{"kind":"order_rejected"}`.
    OrderRejected,
    /// The daemon's OWN equity curve (`vike_core::Portfolio::drawdown_curve` — configured capital
    /// plus realized + unrealized P&L) draws down from its running session peak by at least `pct`
    /// (0.0..=1.0). LATCHED: fires once on breach and re-arms only after the curve recovers back
    /// under `pct`.
    ///
    /// ⚠ NOT the cross-venue equity TOTAL. This deliberately tracks the same quantity
    /// `vike_core`'s own liquidate-only latch does, so the two cannot disagree; the total includes
    /// each live venue's WALLET, which a third party can move. The `Drawdown` arm of
    /// [`crate::eval::eval_snapshot_rule`] carries the measurement.
    Drawdown { pct: f64 },
    /// A held reconcile alert is present (a `ReconAlertView` in the snapshot's `ReconBlock`),
    /// optionally filtered to a `DivergenceKind` label (`ReconAlertView::kind`). LATCHED like
    /// [`Drawdown`](Self::Drawdown): fires on the rising edge (no matching alert → one present).
    ReconAlert {
        // NB: named `divergence_kind`, NOT `kind` — a field serializing to "kind" would collide
        // with this enum's `#[serde(tag = "kind")]` internal tag (a serde compile error).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        divergence_kind: Option<String>,
    },
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
    /// (`vike_data::RecorderHandle::liveness`'s), and on a venue whose symbols ROTATE — a Polymarket
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
    /// signal ([`crate::eval::AlertSignal::FamilyCollapse`]) carries both raw counts plus the
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
}

/// Where a fired alert is delivered. A rule with the default (`in_process: true`, no webhooks)
/// surfaces only in-app (the in-process buffer). Webhook names resolve against the runtime-built
/// [`WebhookConfig`](crate::delivery::WebhookConfig) registry (built from the caller's credential
/// map) — a name with no matching config delivers nowhere, byte-identical to having no webhook (the
/// "absent credentials ⇒ inert" idiom).
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
    /// schema version, for future migrations. `#[serde(default)]` so a file predating the field
    /// loads as [`CURRENT_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<AlertRule>,
}

/// Current [`AlertRuleSet::version`].
pub const CURRENT_VERSION: u32 = 1;

fn default_version() -> u32 {
    CURRENT_VERSION
}

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
