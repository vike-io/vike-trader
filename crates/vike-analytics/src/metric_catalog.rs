//! The METRIC TAXONOMY as data: which performance numbers this workspace can answer, what each one
//! measures, where it is stored on a report, and the parser for an operator's selection over it.
//!
//! # Why a table rather than a `match`
//!
//! [`crate::metrics`] declares three dozen functions and [`crate::report::BacktestReport`] carried
//! eight of them. The eight were not a judgement about which numbers matter — they are the ones the
//! first version happened to need for a ranking objective — and the consequence was measurable:
//! `vike_report::LiveTearsheet` printed Sortino, Calmar, CAGR, SQN, VaR and expected shortfall over
//! the SAME [`crate::BacktestResult`] the backtest verb printed eight scalars for, so the same fills
//! answered a richer question through the live door than through the backtest door.
//!
//! Widening the report alone would not have fixed that, because the next consumer would have
//! hand-copied its own list of names — which is what happened four times already
//! (`crates/vike-report/src/html.rs`'s row vector, that crate's own finiteness test, the GUI
//! tearsheet panel's accessor, and an eleven-entry array of report keys typed out by hand in
//! `crates/vike-cli/src/cmd/runs/show.rs` — all four now DELETED, the last of them in favour of
//! that file's `report_key_order`, which asks [`MetricSelection::Compact`] for its middle eight).
//! So the roster is DATA here, [`METRICS`] is the only place it is
//! written, and the declaration order IS the render order — a consumer asks this module for the
//! ordered ids instead of keeping its own copy.
//!
//! # A metric is either computed or DECLARED ABSENT
//!
//! [`ABSENT`] is the other half and the reason this is a taxonomy rather than a list: a name an
//! operator can reasonably ask for and this tree cannot answer must say WHY, because the
//! alternative is an "unknown metric" refusal that reads as a typo. `exposure` is the worked
//! example — [`crate::metrics::exposure`] exists and is correct, and nothing in a run record can
//! feed it.

use std::fmt::Write as _;

/// Where a metric's value is stored on a serialized report.
///
/// The split is not cosmetic: [`Self::Compact`] names a field that has been in `report.json` since
/// the document's first version, so every stored run can answer it; [`Self::Extended`] names a
/// field of [`crate::report::ExtendedMetrics`], which is `None` on a report written before that
/// block existed. A reader that ignores the distinction renders `0.0` for a run whose real answer
/// is "this was never recorded" — the failure mode [`crate::report::BacktestReport`]'s own doc
/// argues against for the required-versus-defaulted scalars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricHome {
    /// A scalar field of [`crate::report::BacktestReport`] itself.
    Compact,
    /// A field of [`crate::report::ExtendedMetrics`], the opt-in long-form block.
    Extended,
}

/// How a metric's number is to be read, which is what a renderer needs in order not to lie about
/// it: a drawdown of `0.031` printed as `0.0310` invites the reading "three basis points".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnit {
    /// Account currency — two decimals, no scaling.
    Money,
    /// A FRACTION on the wire, rendered times 100 with a `%` suffix. The scaling lives with the
    /// unit rather than at each call site because the report stores fractions, and every renderer
    /// that forgot the scaling published a number a hundred times too small.
    ///
    /// ⚠ That sentence was ASPIRATIONAL for as long as there was no [`MetricUnit::render`] to hold
    /// it — the scaling was spelled at four call sites instead, and two of them disagreed. It is
    /// that method now, and a renderer that reaches past it is the defect this variant's doc has
    /// always described.
    Percent,
    /// A dimensionless ratio — four decimals, no scaling. May carry the house `inf`/`0.0` sentinel
    /// (see [`crate::report::BacktestReport::profit_factor`]).
    Ratio,
    /// A whole count. Rendered with no decimals, because `3.0000` trades is not a thing.
    Count,
}

impl MetricUnit {
    /// Render `v` the way this unit is to be read — **the ONE home for the scaling and the
    /// precision**, which is what [`Self::Percent`]'s own doc has claimed since the enum was
    /// written and what was not true until this method existed.
    ///
    /// # What it refuses: a renderer deciding for itself
    ///
    /// The rule was spelled FOUR times. A private `match` inside
    /// `crate::report::BacktestReport::render_metrics`; `crates/vike-report/src/html.rs`'s
    /// `render_html_inner`, which applied `* 100.0` by hand on four rows and got two MONEY rows'
    /// precision wrong besides; `crates/vike-app-core/src/tool_views/tearsheet.rs`'s
    /// `tearsheet_rows`; and the CLI. Four spellings of one rule are four chances to forget it, and
    /// forgetting is not cosmetic: a renderer that drops the `* 100.0` publishes a max drawdown of
    /// three percent as `0.0310%`, which reads as three basis points — a number a hundred times too
    /// small, on the row an operator sizes risk from. Every renderer that forgot did exactly that.
    ///
    /// # What it deliberately does NOT do
    ///
    /// It does not rescue the house `inf`/`0.0` sentinel (see
    /// [`crate::report::BacktestReport::profit_factor`]). A non-finite value renders as Rust's own
    /// `inf`, byte-identical to what every door already prints, because substituting a placeholder
    /// here would move a published row in all of them at once — and "there is no meaningful ratio"
    /// is a statement about that METRIC, not about the unit it would have been measured in.
    ///
    /// It also does not pad, align or label. A caller owns its own column layout; this answers only
    /// "what does this number SAY", which is the half that was being answered four different ways.
    #[must_use]
    pub fn render(self, v: f64) -> String {
        match self {
            MetricUnit::Money => format!("{v:.2}"),
            // ⚠ The `* 100.0` every hand-rolled renderer forgot at least once. The report stores
            // FRACTIONS — see this variant's own doc.
            MetricUnit::Percent => format!("{:.4}%", v * 100.0),
            MetricUnit::Ratio => format!("{v:.4}"),
            // No decimals. A count reaches here as an `f64` (the `usize` fields widened by
            // [`crate::report::ExtendedMetrics::value_of`]), and `3.0000` would read as a measured
            // quantity rather than as three trades.
            MetricUnit::Count => format!("{v:.0}"),
        }
    }
}

/// One metric this tree can answer.
#[derive(Debug, Clone, Copy)]
pub struct MetricSpec {
    /// The id an operator types and the KEY the value is serialized under — deliberately the same
    /// string, so `--metrics sortino` and a `jq .extended.sortino` name one thing.
    pub id: &'static str,
    /// Which struct holds it — see [`MetricHome`].
    pub home: MetricHome,
    /// How to render it — see [`MetricUnit`].
    pub unit: MetricUnit,
    /// One line, for [`metric_list_text`]. What the number MEASURES, not how it is computed: the
    /// computation is on the `crate::metrics` function of the same name, and restating it here
    /// would be a second copy to rot.
    pub what: &'static str,
}

/// The roster. **Declaration order is render order**, so this array is also the answer to "in what
/// order does a report print", and no consumer keeps a second ordering.
///
/// Grouped by what the number is computed FROM — the trade ledger, then the equity curve — because
/// that is the grouping that predicts which half of a run record can still answer a question when
/// the other half is missing.
pub const METRICS: &[MetricSpec] = &[
    // --- the compact eight: on `BacktestReport` itself since the document's first version -------
    MetricSpec {
        id: "final_equity",
        home: MetricHome::Compact,
        unit: MetricUnit::Money,
        what: "account value at the last equity sample",
    },
    MetricSpec {
        id: "total_return",
        home: MetricHome::Compact,
        unit: MetricUnit::Percent,
        what: "first-to-last equity change as a fraction of the start",
    },
    MetricSpec {
        id: "n_trades",
        home: MetricHome::Compact,
        unit: MetricUnit::Count,
        what: "round trips CLOSED (an open position at the end counts for none)",
    },
    MetricSpec {
        id: "win_rate",
        home: MetricHome::Compact,
        unit: MetricUnit::Percent,
        what: "share of closed trades with positive PnL",
    },
    MetricSpec {
        id: "sharpe",
        home: MetricHome::Compact,
        unit: MetricUnit::Ratio,
        what: "annualized mean/stdev of per-sample returns",
    },
    MetricSpec {
        id: "max_drawdown",
        home: MetricHome::Compact,
        unit: MetricUnit::Percent,
        what: "deepest peak-to-trough fall, as a fraction of that peak",
    },
    MetricSpec {
        id: "profit_factor",
        home: MetricHome::Compact,
        unit: MetricUnit::Ratio,
        what: "gross profit / gross loss; inf when nothing lost",
    },
    MetricSpec {
        id: "funding_paid",
        home: MetricHome::Compact,
        unit: MetricUnit::Money,
        what: "net perp funding cashflow, received-positive",
    },
    // --- the trade ledger's own statistics ------------------------------------------------------
    MetricSpec {
        id: "net_profit",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "sum of closed-trade PnL",
    },
    MetricSpec {
        id: "gross_profit",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "sum of the winning trades alone",
    },
    MetricSpec {
        id: "gross_loss",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "sum of the losing trades alone",
    },
    MetricSpec {
        id: "total_fees",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "round-trip fees charged across every closed trade",
    },
    MetricSpec {
        id: "avg_win",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "mean PnL of the winning trades",
    },
    MetricSpec {
        id: "avg_loss",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "mean PnL of the losing trades",
    },
    MetricSpec {
        id: "largest_win",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "best single closed trade",
    },
    MetricSpec {
        id: "largest_loss",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "worst single closed trade",
    },
    MetricSpec {
        id: "payoff_ratio",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "avg_win over the absolute avg_loss",
    },
    MetricSpec {
        id: "expected_payoff",
        home: MetricHome::Extended,
        unit: MetricUnit::Money,
        what: "net profit per closed trade",
    },
    MetricSpec {
        id: "consecutive_wins",
        home: MetricHome::Extended,
        unit: MetricUnit::Count,
        what: "longest unbroken run of winning trades",
    },
    MetricSpec {
        id: "consecutive_losses",
        home: MetricHome::Extended,
        unit: MetricUnit::Count,
        what: "longest unbroken run of losing trades",
    },
    MetricSpec {
        id: "sqn",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "System Quality Number — trade expectancy over its own noise, scaled by sqrt(n)",
    },
    MetricSpec {
        id: "long_ratio",
        home: MetricHome::Extended,
        unit: MetricUnit::Percent,
        what: "share of closed trades entered long",
    },
    // --- the equity curve's own statistics ------------------------------------------------------
    MetricSpec {
        id: "sortino",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "Sharpe with only DOWNSIDE deviation in the denominator",
    },
    MetricSpec {
        id: "calmar",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "annualized return over max drawdown",
    },
    MetricSpec {
        id: "cagr",
        home: MetricHome::Extended,
        unit: MetricUnit::Percent,
        what: "compound annual growth rate of the curve",
    },
    MetricSpec {
        id: "mar_ratio",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "CAGR over max drawdown",
    },
    MetricSpec {
        id: "recovery_factor",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "total gain over max drawdown — how many drawdowns the run earned back",
    },
    MetricSpec {
        id: "ulcer_index",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "root-mean-square drawdown depth: how much TIME was spent underwater, not just how \
               deep it went",
    },
    MetricSpec {
        id: "ulcer_performance_index",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "annualized return over the ulcer index",
    },
    MetricSpec {
        id: "k_ratio",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "slope over standard error of the log-equity regression — how STRAIGHT the curve is",
    },
    MetricSpec {
        id: "risk_return_ratio",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "mean return over return stdev, unannualized",
    },
    MetricSpec {
        id: "returns_volatility",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "annualized standard deviation of per-sample returns",
    },
    MetricSpec {
        id: "returns_skewness",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "third moment of the return distribution — negative means rare large losses",
    },
    MetricSpec {
        id: "returns_kurtosis",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "excess fourth moment — how fat the return tails are",
    },
    MetricSpec {
        id: "tail_ratio",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "95th percentile return over the absolute 5th percentile return",
    },
    MetricSpec {
        id: "omega",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "gains above a zero threshold over losses below it, whole-distribution",
    },
    MetricSpec {
        id: "value_at_risk_95",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "historical 95% Value-at-Risk of per-sample returns",
    },
    MetricSpec {
        id: "expected_shortfall_95",
        home: MetricHome::Extended,
        unit: MetricUnit::Ratio,
        what: "historical 95% expected shortfall (CVaR) — the mean of the tail VaR cuts off",
    },
];

/// Metrics this tree COMPUTES and a run record cannot feed, each with the reason.
///
/// ⚠ **A row here is a refusal that explains itself, not a TODO.** An operator who types a name
/// from `crate::metrics` and gets "unknown metric" concludes they typed it wrong; the truth is that
/// the input does not exist on disk, which is a different problem with a different fix. Adding a
/// row is therefore cheap, and removing one means an input genuinely arrived.
pub const ABSENT: &[(&str, &str)] = &[(
    "exposure",
    "it needs the PER-STEP POSITION SIZES, and BacktestResult carries only the equity curve and \
     the closed-trade ledger — no run record can answer it, so it is declared absent rather than \
     computed against a substitute",
)];

/// What an operator asked for.
///
/// ⚠ [`Self::Named`] holds `&'static str` deliberately: the ids come out of [`METRICS`] during
/// parsing, so an unknown metric is UNREPRESENTABLE once [`parse_metric_selection`] has returned
/// and no downstream renderer needs a second validation pass. A `Vec<String>` would have pushed the
/// "is this a real metric" question into every consumer, which is how the eleven-key hand copy in
/// `crates/vike-cli/src/cmd/runs/show.rs` came to exist in the first place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricSelection {
    /// The eight scalars a report has always carried, rendered exactly as
    /// [`crate::report::BacktestReport`]'s `Display` renders them — the selection a door would
    /// install as its default, and the one `crates/vike-cli/src/cmd/runs/show.rs`'s
    /// `report_key_order` already asks for its ids.
    ///
    /// ⚠ **That said "byte-identical to what a bare `--metrics` printed before this flag took a
    /// value", and both halves are wrong.** The flag has not taken one: `--metrics` is STILL
    /// `value: Value::None` in `crates/vike-cli/src/surface.rs`'s `FLAGS`
    /// ([`parse_metric_selection`]'s doc carries that measurement and what wiring it owes). And a
    /// bare `--metrics` prints neither this table nor that `Display` — `show_text` in the file
    /// named above renders `report.json`'s KEYS, one `key = value` line each, a different format
    /// answering a different question. What the delegation IS byte-identical to is the human table
    /// the engine prints on its own non-`--json` path
    /// (`crates/vike-backtest/src/backtest_cli.rs`), and THAT identity is the argument for
    /// delegating: [`crate::report::BacktestReport::render_metrics`] returns that `Display`
    /// verbatim rather than re-rendering the eight rows, so the two formats cannot drift.
    Compact,
    /// Every id in [`METRICS`], in declaration order.
    Full,
    /// Exactly these ids, in [`METRICS`] order rather than in the order they were typed — so two
    /// operators who asked for the same set get the same table, and a diff between them is about
    /// the numbers.
    Named(Vec<&'static str>),
    /// The HONESTY counters instead of the metrics: what the run deferred, skipped, could not price
    /// and refused. Not a metric subset, which is why it is a variant rather than a name list —
    /// see [`crate::report::HonestyCounters`] for why they are worth a keyword of their own.
    Honesty,
    /// The resolved cost model the run actually ran under — [`crate::realism::RealismStamp`].
    Realism,
}

/// The keyword spellings [`parse_metric_selection`] accepts, kept as data because two refusals and
/// one listing all have to name the set and a third hand copy would rot.
const KEYWORDS: &[&str] = &["compact", "full", "honesty", "realism"];

impl MetricSelection {
    /// The ids this selection renders, in [`METRICS`] order. Empty for the two non-metric keywords,
    /// which a renderer must handle separately — an empty list is the honest answer there rather
    /// than a silent fall-through to [`Self::Compact`].
    pub fn ids(&self) -> Vec<&'static str> {
        match self {
            MetricSelection::Compact => {
                METRICS.iter().filter(|m| m.home == MetricHome::Compact).map(|m| m.id).collect()
            }
            MetricSelection::Full => METRICS.iter().map(|m| m.id).collect(),
            MetricSelection::Named(ids) => {
                METRICS.iter().filter(|m| ids.contains(&m.id)).map(|m| m.id).collect()
            }
            MetricSelection::Honesty | MetricSelection::Realism => Vec::new(),
        }
    }

    /// Whether this selection needs [`crate::report::ExtendedMetrics`] to be present on the report.
    /// A caller uses it to refuse a run recorded before that block existed with a sentence about
    /// the RUN rather than about the flag.
    pub fn needs_extended(&self) -> bool {
        self.ids().iter().any(|id| spec_for(id).is_some_and(|s| s.home == MetricHome::Extended))
    }
}

/// The [`MetricSpec`] for `id`, or `None` when nothing in the catalog is called that.
pub fn spec_for(id: &str) -> Option<&'static MetricSpec> {
    METRICS.iter().find(|m| m.id == id)
}

/// Parse an operator's `--metrics` value.
///
/// # What it refuses, and why each refusal is not a convenience
///
/// * An EMPTY value — somebody's `--metrics=` is an unfinished edit, and defaulting it to
///   `compact` would make the empty case indistinguishable from the deliberate one. ⚠ This bullet
///   opened "because the flag used to be a bare switch". It still IS one; the argument never
///   depended on the tense, and the fact is the section below.
/// * An id the catalog does not hold — with the count and the listing command, because the
///   commonest cause is a name out of another tool's vocabulary rather than a typo.
/// * An id [`ABSENT`] declares, with that row's reason. This is the whole point of that table.
/// * A REPEATED id, following `crates/vike-cli/src/cmd/runs/failif.rs`'s `parse_fail_if`: a second
///   mention renders no second row, so the list is not what its author meant and silently
///   collapsing it hides the mistake.
/// * A keyword MIXED with ids, because which one wins would be a guess and both readings are
///   defensible.
///
/// Whitespace around a comma is accepted — a shell-quoted `"sharpe, sortino"` is one value, and
/// refusing it would be a refusal about quoting rather than about metrics.
///
/// # No door reaches this function, so every refusal below is LATENT — but the command they NAME
/// is real now
///
/// ⚠ **The messages are written as though an operator could read one, and today none of them can.**
/// MEASURED, and still true: `--metrics` is declared `value: Value::None` in
/// `crates/vike-cli/src/surface.rs`'s `FLAGS`, and the arm for it in
/// `crates/vike-cli/src/cmd/backtest.rs`'s `parse_read` calls `args::no_value` — a bare switch, so
/// no value ever arrives here. This function's only callers anywhere are its own `#[cfg(test)]`
/// module and one test in [`crate::report`].
///
/// ⚠ **What CHANGED on 2026-09-16 is the half that made those refusals a trap, and it is the half
/// worth reading first.** This section said "three of the refusals name `--metrics-list` as a
/// command to RUN, and there is no such flag in this tree". There is one now:
/// `crates/vike-cli/src/cmd/backtest.rs`'s `parse_read` carries a `"--metrics-list"` arm, its
/// `FLAGS` row ships, and `crates/vike-cli/src/cmd/runs/show.rs`'s `run_show` prints
/// [`metric_list_text`] before it resolves a run. So the three sentences below now hand an operator
/// a line that WORKS. The refusals themselves stay latent, which is a different and much smaller
/// debt: nobody can reach them, so nobody is misdirected by them.
///
/// ⚠ A THIRD spelling was in the tree as well: `crates/vike-cli/Cargo.toml` argued that crate's
/// vike-analytics edge from `backtest show --metrics-set`, a name that occurs in no other file in
/// the repository. That note names `--metrics` widened to take a value as the spelling that would
/// actually wire this function, and points here for the rest of the debt. `--metrics-set` is
/// therefore still a name nothing in this tree spells — and it deliberately stays that way: adding
/// it would make every `--metrics …` sentence below false, on a surface where `--metrics` already
/// exists.
///
/// **What the SELECTION half still owes, and why it did not ride in on the listing.** `--metrics`
/// widened to take a value: the `parse_read` arm, the `FLAGS` row's
/// `value`/`value_name`/`roster_id`, a re-render of `crates/vike-cli/tests/fixtures/cli.json` — and
/// a decision this change deliberately did not make, because `show --metrics` is a SHIPPED bare
/// switch (`show_text`'s `all = !metrics && !trades && !config`) and widening it to require a value
/// breaks that line. The optional-value shape that would not break it has no PRIMITIVE on the
/// client: `crates/vike-cli/src/cmd/args.rs` carries `Flags::value` and `args::no_value` and
/// nothing between them, and `crates/vike-cli/src/surface.rs`'s `Value` enum has no variant for it
/// either. ⚠ The shape itself is not novel and this sentence should not be read as saying so —
/// `vike_datahub_client::flag_vocab`'s `Arity::OptionalValue` DECLARES it (for `--addr`, used by no
/// row), and `crates/vike-backtest/src/backtest_cli.rs`'s `AddrFlag` is a working implementation of
/// it on the engine's own parser. What is missing is the client-side primitive, which means the
/// widening is a change to `args.rs` and to that `Value` enum before it is a change to any flag.
///
/// ⚠ The three sentences are deliberately left NAMING the flag rather than softened, and the
/// argument that they were "the only written specification of it" has now been discharged by
/// building it. They stay because they are CORRECT: the listing they name is the right answer to
/// the question an operator reading one of them has.
pub fn parse_metric_selection(spec: &str) -> Result<MetricSelection, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(format!(
            "--metrics needs a selection: one of {} or a comma list of metric ids. `--metrics-list` \
             prints every id with what it measures.",
            KEYWORDS.join(" / ")
        ));
    }
    let parts: Vec<&str> = spec.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(format!(
            "--metrics was given {spec:?}, which names nothing once the commas are removed. \
             Expected one of {} or a comma list of metric ids.",
            KEYWORDS.join(" / ")
        ));
    }
    // ⚠ **The one refusal that is about the FLAG rather than about the metrics**, and it exists
    // because `--metrics` shipped as a bare switch. `backtest show --metrics @last` used to work;
    // now the flag takes a value, so that spelling feeds the RUN SELECTOR in as the selection and
    // the honest-but-useless answer would be "--metrics does not know the metric \"@last\"". An
    // operator reading that goes looking for a metric name. The shapes are unambiguous — no metric
    // id contains `@` or `/`, and `vike_model::runs`' selectors and run ids are built from them —
    // so this can say what actually happened.
    if spec.starts_with('@') || spec.contains('/') {
        return Err(format!(
            "--metrics takes a SELECTION and {spec:?} looks like a run selector. `--metrics` used \
             to be a bare switch; the spelling that reproduces its old output exactly is \
             `--metrics compact`, and the run selector stays a positional \
             (`show <run> --metrics compact`)."
        ));
    }
    let keyword_at = parts.iter().position(|p| KEYWORDS.contains(&p.to_ascii_lowercase().as_str()));
    if let Some(i) = keyword_at {
        if parts.len() > 1 {
            return Err(format!(
                "--metrics takes EITHER a keyword ({}) or a comma list of metric ids, never both — \
                 {spec:?} mixes {:?} with {} id(s), and which of the two wins would be a guess.",
                KEYWORDS.join(" / "),
                parts[i],
                parts.len() - 1
            ));
        }
        return Ok(match parts[0].to_ascii_lowercase().as_str() {
            "compact" => MetricSelection::Compact,
            "full" => MetricSelection::Full,
            "honesty" => MetricSelection::Honesty,
            // The keyword set is `KEYWORDS` and the position lookup above already matched one of
            // them, so this arm is `realism` and no input reaches it as a catch-all.
            _ => MetricSelection::Realism,
        });
    }

    let mut ids: Vec<&'static str> = Vec::with_capacity(parts.len());
    for part in parts {
        if let Some((name, why)) = ABSENT.iter().find(|(n, _)| *n == part) {
            return Err(format!(
                "--metrics cannot render {name:?}: {why}. It is declared absent rather than \
                 unknown, so this is not a typo — `--metrics-list` names what CAN be rendered."
            ));
        }
        let Some(m) = spec_for(part) else {
            return Err(format!(
                "--metrics does not know the metric {part:?}. The catalog holds {} ids and \
                 `--metrics-list` prints each one with what it measures; a name out of another \
                 tool's vocabulary is the usual cause.",
                METRICS.len()
            ));
        };
        if ids.contains(&m.id) {
            return Err(format!(
                "--metrics names {:?} twice. A repeated id renders no second row, so the list is \
                 not what its author meant.",
                m.id
            ));
        }
        ids.push(m.id);
    }
    Ok(MetricSelection::Named(ids))
}

/// What `--metrics-list` prints: every catalog id with where it is stored and what it measures,
/// then the declared-absent rows.
///
/// A listing rather than a table of NUMBERS, deliberately: it answers "what can I ask for" and
/// needs no run at all, which is what lets the flag short-circuit before a run id is resolved.
///
/// ⚠ **`--metrics-list` EXISTS since 2026-09-16, and this note said it did not.** The door is
/// `crates/vike-cli/src/cmd/backtest.rs`'s `parse_read` arm plus the short-circuit at the top of
/// `crates/vike-cli/src/cmd/runs/show.rs`'s `run_show`, which prints this text before a run is
/// resolved — the property the paragraph above calls the reason the flag can skip a run id. It has
/// a `FLAGS` row in `crates/vike-cli/src/surface.rs` and rides into
/// `crates/vike-cli/tests/fixtures/cli.json` with it. [`parse_metric_selection`]'s doc carries what
/// the SELECTION half still owes, which is a different and larger change.
///
/// ⚠ The one property a caller must not break: this ends with a NEWLINE (every branch writes
/// through `writeln!`), so the door prints it with `print!` and writes it to `--out` verbatim. A
/// caller that adds one publishes a blank line at the end of the table.
pub fn metric_list_text() -> String {
    let width = METRICS.iter().map(|m| m.id.len()).max().unwrap_or(0);
    let mut out = String::with_capacity(4 * 1024);
    let _ = writeln!(
        out,
        "{} metrics. `--metrics compact` is the default table, `--metrics full` is all of them, and \
         a comma list picks any subset.",
        METRICS.len()
    );
    let _ = writeln!(out);
    for m in METRICS {
        let home = match m.home {
            MetricHome::Compact => "core",
            MetricHome::Extended => "ext ",
        };
        let _ = writeln!(out, "  {home}  {:width$}  {}", m.id, m.what);
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  core = stored on every report.json; ext = stored in its `extended` block, which a run \
         recorded before that block existed does not have."
    );
    if !ABSENT.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "declared ABSENT — computable by this tree, not answerable from a run record:"
        );
        for (name, why) in ABSENT {
            let _ = writeln!(out, "  {name}: {why}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`MetricUnit`] variant, so the renderings can be iterated. The `match` in
    /// `every_unit_renders_one_documented_way_and_percent_is_the_one_that_scales` is EXHAUSTIVE,
    /// so a new variant is a compile error there rather than a rendering nobody pinned — which is
    /// the only enumeration Rust offers over an enum and the reason this array is allowed to exist.
    const ALL_UNITS: [MetricUnit; 4] =
        [MetricUnit::Money, MetricUnit::Percent, MetricUnit::Ratio, MetricUnit::Count];

    /// ⚠ **THE PERCENT SCALING, pinned.** `0.031` is the value [`MetricUnit`]'s own doc argues
    /// about: a drawdown of three percent, which the Percent unit must publish as `3.1000%` and
    /// which every renderer that forgot the `* 100.0` published as `0.0310%` — three basis points,
    /// a hundred times too small, on the row an operator sizes risk from.
    ///
    /// The other three units are pinned on the SAME input, so the test also says what percent is
    /// the exception TO: nothing else scales.
    #[test]
    fn every_unit_renders_one_documented_way_and_percent_is_the_one_that_scales() {
        for unit in ALL_UNITS {
            // Exhaustive on purpose: a fifth variant fails to compile here instead of shipping
            // with an unpinned rendering.
            let want = match unit {
                MetricUnit::Money => "0.03",
                MetricUnit::Percent => "3.1000%",
                MetricUnit::Ratio => "0.0310",
                MetricUnit::Count => "0",
            };
            assert_eq!(unit.render(0.031), want, "{unit:?}");
        }

        // ...and the factor is exactly a HUNDRED, on two values where no other factor could
        // produce these strings.
        assert_eq!(MetricUnit::Percent.render(1.0), "100.0000%");
        assert_eq!(MetricUnit::Percent.render(0.0001), "0.0100%");
        // A negative value keeps its sign and gains no parentheses or colour — `total_return` on a
        // losing run and `gross_loss`/`funding_paid` always are negative, and a renderer must not
        // have to special-case them to avoid an accounting bracket nobody asked for.
        assert_eq!(MetricUnit::Percent.render(-0.05), "-5.0000%");
        assert_eq!(MetricUnit::Money.render(-12.5), "-12.50");
    }

    /// ⚠ **The house sentinel renders as `inf`, and that is a DECISION rather than an oversight.**
    /// [`crate::report::BacktestReport::profit_factor`] carries `f64::INFINITY` for "nothing lost",
    /// and every door prints Rust's own `inf` for it today. Substituting a placeholder inside
    /// [`MetricUnit::render`] would move a published row in all four doors at once, so the choice
    /// belongs to whoever owns the metric, not to its unit — this test is what stops the
    /// substitution happening by accident inside a formatting change.
    #[test]
    fn a_nonfinite_value_renders_as_rusts_own_token_and_gains_no_placeholder() {
        assert_eq!(MetricUnit::Ratio.render(f64::INFINITY), "inf");
        assert_eq!(MetricUnit::Ratio.render(f64::NEG_INFINITY), "-inf");
        assert_eq!(MetricUnit::Ratio.render(f64::NAN), "NaN");
    }

    /// Every catalog row renders through its own unit without panicking and without an empty cell —
    /// the cheap end-to-end over [`METRICS`], so a row whose unit was chosen carelessly (a `Count`
    /// on a fraction, say) is at least exercised.
    #[test]
    fn every_catalog_row_renders_through_its_unit() {
        for m in METRICS {
            let cell = m.unit.render(1.5);
            assert!(!cell.is_empty(), "{} rendered nothing", m.id);
            assert_eq!(
                cell.ends_with('%'),
                m.unit == MetricUnit::Percent,
                "{} carries the `%` suffix if and only if it is a Percent",
                m.id
            );
        }
    }

    /// An id is a KEY as well as a name, so a duplicate would make one of the two unreachable
    /// through [`spec_for`] and silently shadow the other in any map built from this table.
    #[test]
    fn every_id_is_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for m in METRICS {
            assert!(!seen.contains(&m.id), "{} is declared twice", m.id);
            seen.push(m.id);
        }
    }

    /// The two tables must not overlap: a name cannot be both answerable and declared absent, and
    /// if it were, [`parse_metric_selection`]'s absent-check-first order would make the catalog row
    /// dead.
    #[test]
    fn nothing_is_both_computed_and_declared_absent() {
        for (name, _) in ABSENT {
            assert!(
                spec_for(name).is_none(),
                "{name} is in both METRICS and ABSENT — the absent row would shadow it"
            );
        }
    }

    /// `Compact` must be exactly the report's own scalars, which is what makes the keyword
    /// byte-identical to the pre-widening bare `--metrics`.
    #[test]
    fn compact_is_the_reports_own_scalars() {
        let ids = MetricSelection::Compact.ids();
        assert_eq!(
            ids,
            vec![
                "final_equity",
                "total_return",
                "n_trades",
                "win_rate",
                "sharpe",
                "max_drawdown",
                "profit_factor",
                "funding_paid",
            ]
        );
        assert!(!MetricSelection::Compact.needs_extended());
    }

    /// A named selection renders in CATALOG order, not in typing order — otherwise two operators
    /// asking for the same set get tables that diff against each other.
    #[test]
    fn a_named_selection_renders_in_catalog_order_not_typing_order() {
        let sel = parse_metric_selection("sortino,sharpe").unwrap();
        assert_eq!(sel.ids(), vec!["sharpe", "sortino"]);
        assert!(sel.needs_extended(), "sortino lives in the extended block");
    }

    /// The keyword set is case-insensitive, because a shell history carries whatever was typed.
    #[test]
    fn keywords_are_case_insensitive() {
        assert_eq!(parse_metric_selection("FULL").unwrap(), MetricSelection::Full);
        assert_eq!(parse_metric_selection("Compact").unwrap(), MetricSelection::Compact);
        assert_eq!(parse_metric_selection("Honesty").unwrap(), MetricSelection::Honesty);
        assert_eq!(parse_metric_selection("realism").unwrap(), MetricSelection::Realism);
    }

    /// Whitespace around a comma survives, so a quoted list is not a refusal about quoting.
    #[test]
    fn a_quoted_list_with_spaces_parses() {
        assert_eq!(
            parse_metric_selection(" sharpe , sortino ").unwrap(),
            MetricSelection::Named(vec!["sharpe", "sortino"])
        );
    }

    /// Each refusal fires for its OWN reason and says which — the property that makes them worth
    /// publishing rather than collapsing into one message.
    #[test]
    fn each_refusal_names_what_it_refused() {
        let e = parse_metric_selection("").unwrap_err();
        assert!(e.contains("needs a selection"), "{e}");

        let e = parse_metric_selection(",,").unwrap_err();
        assert!(e.contains("names nothing"), "{e}");

        let e = parse_metric_selection("full,sharpe").unwrap_err();
        assert!(e.contains("never both"), "{e}");

        let e = parse_metric_selection("sharpe,sharpe").unwrap_err();
        assert!(e.contains("twice"), "{e}");

        let e = parse_metric_selection("shrapnel").unwrap_err();
        assert!(e.contains("does not know") && e.contains("shrapnel"), "{e}");

        // The whole point of ABSENT: the reason, not "unknown metric".
        let e = parse_metric_selection("exposure").unwrap_err();
        assert!(e.contains("POSITION SIZES") && e.contains("declared absent"), "{e}");
    }

    /// ⚠ **The migration typo, answered by name.** `--metrics` shipped as a bare switch, so
    /// `show --metrics @last` used to work and now feeds the run selector in as the selection. The
    /// refusal must say THAT, not "unknown metric @last", which sends an operator looking for a
    /// metric called `@last`.
    #[test]
    fn a_run_selector_in_the_value_slot_is_named_as_such() {
        for spec in ["@last", "@baseline/main", "1756000000-1-0/x"] {
            let e = parse_metric_selection(spec).unwrap_err();
            assert!(e.contains("looks like a run selector"), "{spec}: {e}");
            assert!(
                e.contains("--metrics compact"),
                "the refusal must name the spelling that restores the old output: {e}"
            );
        }
    }

    /// ...and a legitimate id is not caught by that shape check. The guard keys on `@` and `/`,
    /// which no catalog id contains — this is the test that says so rather than assuming it.
    #[test]
    fn no_catalog_id_is_mistaken_for_a_run_selector() {
        for m in METRICS {
            assert!(
                !m.id.starts_with('@') && !m.id.contains('/'),
                "{} would be refused as a run selector",
                m.id
            );
            assert!(parse_metric_selection(m.id).is_ok(), "{} does not parse as itself", m.id);
        }
    }

    /// The listing names every id and every absent row — it is the operator's only door onto the
    /// catalog, so a metric missing from it is a metric nobody can find.
    #[test]
    fn the_listing_names_every_id_and_every_absent_row() {
        let text = metric_list_text();
        for m in METRICS {
            assert!(text.contains(m.id), "{} is missing from --metrics-list", m.id);
        }
        for (name, _) in ABSENT {
            assert!(text.contains(name), "absent row {name} is missing from --metrics-list");
        }
    }
}
