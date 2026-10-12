//! What an operator asked for: `MetricSelection`, the `--metrics` parser, `--metrics-list`'s text.

use std::fmt::Write as _;

use super::rows::{ABSENT, METRICS};
use super::{MetricHome, spec_for};

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
/// `crates/vike-cli/src/cmd/backtest/read.rs`'s `parse_read` calls `args::no_value` — a bare switch, so
/// no value ever arrives here. This function's only callers anywhere are its own `#[cfg(test)]`
/// module and one test in [`crate::report`].
///
/// ⚠ **What CHANGED on 2026-09-16 is the half that made those refusals a trap, and it is the half
/// worth reading first.** This section said "three of the refusals name `--metrics-list` as a
/// command to RUN, and there is no such flag in this tree". There is one now:
/// `crates/vike-cli/src/cmd/backtest/read.rs`'s `parse_read` carries a `"--metrics-list"` arm, its
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
/// row), and `crates/vike-backtest/src/backtest_cli/args.rs`'s `AddrFlag` is a working
/// implementation of it on the engine's own parser. What is missing is the client-side primitive,
/// which means the widening is a change to `args.rs` and to that `Value` enum before it is a change
/// to any flag.
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
/// `crates/vike-cli/src/cmd/backtest/read.rs`'s `parse_read` arm plus the short-circuit at the top of
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
