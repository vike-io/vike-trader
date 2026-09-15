//! WHICH SEARCH a run performs: the method, the knob each method owns, the ranking, and the
//! evaluator the two together imply. **One implementation, called by both surfaces that select a
//! search**, so a remote run and a `--local` run cannot answer differently.
//!
//! # Why this is a module and not two copies
//!
//! It was one copy until stage 7 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`, and that copy was PRIVATE in
//! `crates/vike-backtest/src/backtest_cli.rs` — behind `datafusion-store`, which
//! `crate::compute_server` (behind the smaller `hist-replay`) cannot enable. So when the wire
//! learned to carry a method, the server had no way to NAME the rule it had to apply, and the
//! obvious move — writing the ownership table again server-side — is the two-rosters defect the
//! stage exists to kill. The rule lives here instead, one feature rung below both callers.
//!
//! # What deliberately did NOT move
//!
//! Everything argv-shaped. `backtest_cli`'s `parse_search_flags` still owns the retired `--search`
//! refusal, `required_value`'s guard against a written-but-value-less flag, the artifact flags
//! (`--keep-trials`, `--resume`) that belong to no method, and the PRESENCE-based ownership check
//! that must fire before any value is parsed. [`resolve`] performs the same ownership check over
//! `Option` presence, because that is the only presence a wire frame has. The rule is therefore
//! consulted twice on the argv path — from ONE table ([`METHOD_KNOBS`]) with ONE message
//! ([`refuse_unowned_knob`]), which is the property that matters. Collapsing the two would change
//! which refusal fires for a bare trailing knob under a non-owning method, and
//! `crates/vike-backtest/tests/optimizer_cli.rs` drives the real binary over that family.
//!
//! # The messages name FLAGS, on both routes, deliberately
//!
//! Every human who reaches one typed flags: `vike-cli backtest --optimizer tpe --trials 8` gets
//! here through a socket or through a spawn, and the same sentence is the right answer on both. An
//! MCP agent passing `trials` reads a message naming `--trials`; that is an accepted cost of one
//! spelling, and `crates/vike-cli/src/cmd/mcp.rs`'s tool schema names the flag each argument
//! corresponds to.

use std::sync::Arc;

use vike_data::HistStore;
use vike_datahub_client::{DEFAULT_SEARCH_METHOD, SEARCH_METHODS};

use super::{
    BacktestProfile, HarnessError, Optimizer, ParamscanExec, RankMetric, StoreEvaluator, TpeConfig,
};
use crate::objective::{self, Objective};
use crate::search::EulerConfig;

/// Which METHODS own each method-specific knob — moved from `backtest_cli`'s `METHOD_FLAGS`,
/// including the argument for why the second element is a SET.
///
/// ⚠ **The second element is a SET, and `--seed` is why.** A seed is genuinely the same knob under
/// tpe and genetic — same parse, same meaning, same `u64` reaching a searcher's constructor. Two
/// rows would scatter a flag's owners with nothing binding them, and would refuse
/// `--optimizer genetic --seed 7` BY THE TPE ROW; dropping the row would let grid and euler discard
/// the flag in silence. An EMPTY slice refuses the flag under every method, which is the safe
/// direction for a typo.
pub const METHOD_KNOBS: &[(&str, &[&str])] = &[
    ("--euler-depth", &["euler"]),
    ("--trials", &["tpe"]),
    // The reproducibility seed is a knob BOTH stochastic searchers take, and the only asymmetry is
    // what their ABSENCE means — tpe defaults to 0, genetic refuses. That belongs in the
    // construction arms, where a method's own semantics live, not here: this table answers "may
    // this flag be written at all", and the answer is yes for both.
    ("--seed", &["tpe", "genetic"]),
];

/// Which parameter-search METHOD was named, carrying the config THAT method — and only that method
/// — takes.
///
/// ⚠ `PartialEq` but NOT `Eq`: `TpeConfig` holds an `f64` gamma and derives only `PartialEq`.
/// `Copy` is what lets [`evaluator_for`] and [`optimizer_for`] each read it without a move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchMethod {
    Grid,
    Euler(EulerConfig),
    Tpe(TpeConfig),
    /// ⚠ `super::genetic::GeneticConfig` BY MODULE PATH, where `TpeConfig` is a re-export: a
    /// method's knowledge stays at the method's module path, and `genetic` deliberately joins no
    /// re-export block.
    Genetic(super::genetic::GeneticConfig),
}

/// What the ranking name selected. `Multi` picks the composite objective; the four classic names
/// keep the score-clearing classic shape on the grid path (see [`uses_classic_evaluator`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RankChoice {
    Metric(RankMetric),
    Multi,
}

/// The knob TOKENS a caller has — argv values, or the wire's `WireSearch` fields. Borrowed rather
/// than owned, so neither caller allocates to ask a question.
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchSelection<'a> {
    pub optimizer: Option<&'a str>,
    pub euler_depth: Option<&'a str>,
    pub trials: Option<&'a str>,
    pub seed: Option<&'a str>,
}

impl<'a> SearchSelection<'a> {
    /// One knob BY ITS FLAG NAME, so [`METHOD_KNOBS`] can be iterated rather than unrolled. An
    /// unknown flag answers `None`, which makes a typo in the table inert rather than a panic — and
    /// the table's rows are held to real flags by this module's tests.
    pub fn knob(&self, flag: &str) -> Option<&'a str> {
        match flag {
            "--euler-depth" => self.euler_depth,
            "--trials" => self.trials,
            "--seed" => self.seed,
            _ => None,
        }
    }
}

/// The ONE refusal text for a knob handed to a method that does not own it.
///
/// ⚠ EVERY owner, joined — not the first one. A refusal naming only one of a two-owner flag's
/// methods would send an operator who typed `--optimizer grid --seed 7` to tpe while genetic, which
/// may be the method they wanted, went unmentioned. A single-owner row renders the bare owner name,
/// so `--trials` and `--euler-depth` refuse with the string they always did.
pub fn refuse_unowned_knob(flag: &str, owners: &[&str], method: &str) -> String {
    format!(
        "{flag} is a {} flag, but this run selected the {method:?} optimizer. It was silently \
         discarded before; it is refused now, because a knob that configures a search you did not \
         select cannot do what it says",
        owners.join(" or ")
    )
}

/// Ownership, then construction. PURE — no store, no profile, no environment, no clock.
pub fn resolve(sel: &SearchSelection<'_>) -> Result<SearchMethod, String> {
    let name = sel.optimizer.unwrap_or(DEFAULT_SEARCH_METHOD);
    // ⚠ OWNERSHIP BEFORE VALUE, for the reason `parse_search_flags` states: a knob that does not
    // belong reports THAT, rather than a parse error about a value nobody wanted — and the answer
    // to `--trials 0` is then uniform across methods instead of fatal on one path and ignored on
    // the others.
    for (flag, owners) in METHOD_KNOBS {
        if sel.knob(flag).is_some() && !owners.iter().any(|o| name.eq_ignore_ascii_case(o)) {
            return Err(refuse_unowned_knob(flag, owners, name));
        }
    }
    if name.eq_ignore_ascii_case(DEFAULT_SEARCH_METHOD) {
        Ok(SearchMethod::Grid)
    } else if name.eq_ignore_ascii_case("euler") {
        Ok(SearchMethod::Euler(EulerConfig::with_depth(parse_euler_depth(sel.euler_depth)?)))
    } else if name.eq_ignore_ascii_case("tpe") {
        // `.unwrap_or(0)` — tpe's documented default. The ABSENCE is resolved in the METHOD's arm,
        // not in the parser, so genetic can refuse the same absence one arm down.
        Ok(SearchMethod::Tpe(TpeConfig::new(
            parse_trials(sel.trials)?,
            parse_seed(sel.seed)?.unwrap_or(0),
        )))
    } else if name.eq_ignore_ascii_case("genetic") {
        Ok(SearchMethod::Genetic(super::genetic::GeneticConfig::new(require_seed(sel.seed)?)))
    } else {
        Err(format!("invalid --optimizer {name:?} (expected {})", SEARCH_METHODS.join("|")))
    }
}

/// The ONE place a [`SearchMethod`] becomes a trait object.
///
/// ⚠ The three non-grid methods are deliberately NOT re-exported from `harness::` — `harness/mod.rs`
/// says only the seam is vocabulary and a method's knowledge belongs in the method's file — so
/// these are module paths.
pub fn optimizer_for(method: SearchMethod) -> Box<dyn Optimizer> {
    match method {
        SearchMethod::Grid => Box::new(super::sweep::GridSearch),
        SearchMethod::Euler(cfg) => Box::new(super::euler::EulerSearch::new(cfg)),
        SearchMethod::Tpe(cfg) => Box::new(super::tpe::TpeSearch::new(cfg)),
        SearchMethod::Genetic(cfg) => Box::new(super::genetic::GeneticSearch::new(cfg)),
    }
}

/// The `(method name, seed, budget)` triple a run's ARTIFACT identity records.
///
/// Lives here rather than in `backtest_cli` because the three values are properties of the METHOD,
/// and a second reading of a `SearchMethod` is a second chance to disagree about what `budget`
/// means for euler.
pub fn identity_parts(method: &SearchMethod) -> (&'static str, Option<u64>, Option<u64>) {
    match method {
        SearchMethod::Grid => ("grid", None, None),
        SearchMethod::Euler(c) => ("euler", None, Some(u64::from(c.max_depth))),
        SearchMethod::Tpe(c) => ("tpe", Some(c.seed), Some(c.n_trials as u64)),
        SearchMethod::Genetic(c) => ("genetic", Some(c.seed), c.max_evaluations.map(|v| v as u64)),
    }
}

/// The ranking name, resolved. `None` is the annualized-Sharpe default both surfaces have always
/// applied.
pub fn resolve_rank(name: Option<&str>) -> Result<RankChoice, String> {
    match name {
        None => Ok(RankChoice::Metric(RankMetric::default())),
        Some(s) if s.eq_ignore_ascii_case("multi") => Ok(RankChoice::Multi),
        Some(s) => RankMetric::from_str_ci(s).map(RankChoice::Metric).ok_or_else(|| {
            format!("invalid --rank-by {s:?} (expected sharpe|return|max_dd|equity|multi)")
        }),
    }
}

/// The `(objective, label)` pair a ranking implies.
///
/// ⚠ The caller must DECLARE the objective before the evaluator: [`StoreEvaluator::new`] borrows it
/// for the evaluator's whole life, and locals drop in reverse declaration order.
pub fn objective_for(rank: RankChoice) -> (Objective, String) {
    match rank {
        RankChoice::Metric(m) => (m.objective(), m.name().to_string()),
        RankChoice::Multi => (objective::multi_metric(Default::default()), "multi".to_string()),
    }
}

/// ⚠ **WHICH CONSTRUCTOR is a PRESERVATION decision rather than a preference**, and this predicate
/// is the whole of it. `grid` with one of the four classic metrics is the ONE combination whose
/// rows have their `score` CLEARED (`super::report_from_outcome`'s `RankBy::Metric` arm), and it is
/// EXACTLY the invocation `vike-cli backtest --local` spawns, `scripts/cli_mcp_smoke.sh` runs, and
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
/// `profile_sweep_is_byte_identical_local_and_remote` compares byte-for-byte. Building one uniform
/// objective evaluator would move that shipped wire TWICE over: every row would gain a `score` key,
/// and `rank_by` would change STRING for three of the four metrics, because `RankMetric` serializes
/// `snake_case` (`total_return`) while `RankMetric::name` answers with the short name (`return`) and
/// `RankBy` is `#[serde(untagged)]`.
///
/// euler and tpe have ALWAYS stamped a score under a metric rank, and this keeps that true too.
pub(crate) fn uses_classic_evaluator(method: SearchMethod, rank: RankChoice) -> bool {
    matches!((method, rank), (SearchMethod::Grid, RankChoice::Metric(_)))
}

/// The evaluator a `(method, rank)` pair implies, over `store`.
///
/// ⚠ `exec` is RESOLVED BY THE CALLER, at its composition root, not read here: the concrete
/// adapters still call `ParamscanExec::from_env` themselves, so the
/// `("vike-backtest", "VIKE_SWEEP_SEQUENTIAL")` row on
/// `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` does not move and this module adds
/// no `env::var` literal for the scanner to find. ⚠ TPE is handed whatever the caller resolved and
/// stays byte-identical BY THE POOL RULE rather than by the exec value: `StoreEvaluator::evaluate`
/// enters rayon only when `exec == Parallel && batch.len() > 1`, and `TpeSearch::search` submits
/// width-1 batches because each proposal reads the whole observation history.
pub fn evaluator_for<'a>(
    method: SearchMethod,
    rank: RankChoice,
    base: &'a BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &'a Objective,
    label: String,
    exec: ParamscanExec,
) -> Result<StoreEvaluator<'a>, HarnessError> {
    match (uses_classic_evaluator(method, rank), rank) {
        (true, RankChoice::Metric(m)) => StoreEvaluator::classic(base, store, m, exec),
        _ => StoreEvaluator::new(base, store, objective, label, exec),
    }
}

/// `--euler-depth`, RANGE-CHECKED rather than clamped.
///
/// ⚠ §7.5 of the design, signed off. `EulerConfig::with_depth` clamps silently at
/// `EulerConfig::MAX_DEPTH_CAP`, and `harness::EulerBudget`'s `max_depth` then reports a depth the
/// operator never typed — in the very line whose whole job is to report the budget. The clamp stays
/// exactly as it is; the refusal lives ABOVE it, here.
///
/// `0` is legal and means coarse-grid-only, per `with_depth`'s own doc — so this is a range check,
/// not a positive-integer one.
fn parse_euler_depth(value: Option<&str>) -> Result<u32, String> {
    match value {
        None => Ok(EulerConfig::DEFAULT_MAX_DEPTH),
        Some(s) => match s.parse::<u32>() {
            Ok(d) if d <= EulerConfig::MAX_DEPTH_CAP => Ok(d),
            Ok(d) => Err(format!(
                "--euler-depth {d} is past the cap of {} — it was silently clamped before, which \
                 made the budget line report a depth you did not ask for",
                EulerConfig::MAX_DEPTH_CAP
            )),
            Err(_) => Err(format!("invalid --euler-depth {s:?} (expected an integer)")),
        },
    }
}

/// `--trials` — tpe's budget. The positive-integer rule is unchanged; what changed is that it is
/// now the ONLY answer to a `--trials` value, because a non-tpe run refuses the flag outright.
fn parse_trials(value: Option<&str>) -> Result<usize, String> {
    match value {
        None => Ok(TpeConfig::DEFAULT_TRIALS),
        Some(s) => match s.parse::<usize>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(format!("invalid --trials {s:?} (expected a positive integer)")),
        },
    }
}

/// `--seed` — the reproducibility knob tpe AND genetic take. ONE parse, and it answers `None` for
/// "not written" rather than substituting a default.
///
/// ⚠ **The default lives OUT of this function because the flag has a second owner.** The VALUE's
/// rules are a property of the flag and stay here — one `u64` parser, one refusal string — so the
/// two methods cannot start disagreeing about what `--seed abc` means. Its ABSENCE is a property of
/// the METHOD and belongs in the method's own arm: tpe substitutes `0` (its documented, shipped
/// default), genetic refuses through [`require_seed`]. Returning `u64` with a baked-in `0` would
/// have forced the genetic arm to distinguish "not written" from "written as 0" — two inputs that
/// mean different things, collapsed into one value before anybody could tell them apart.
fn parse_seed(value: Option<&str>) -> Result<Option<u64>, String> {
    match value {
        None => Ok(None),
        Some(s) => {
            s.parse::<u64>().map(Some).map_err(|_| format!("invalid --seed {s:?} (expected a u64)"))
        }
    }
}

/// `--seed`, REQUIRED — the genetic arm's disposition of an absent seed.
///
/// ⚠ **Silently defaulting it is the one thing the searcher's author ruled out.**
/// `harness::genetic::GeneticConfig::new` takes the seed as a required PARAMETER, and its doc says
/// why in as many words: "a defaulted seed is a hidden constant that changes results silently".
/// Writing `GeneticConfig::new(0)` at a call site would re-introduce exactly what that signature
/// refuses — the type would still look strict while nothing in the tree expressed the requirement
/// any more.
///
/// **Why this differs from tpe, which defaults `--seed` to 0 one arm up.** Not because one search
/// is more stochastic than the other — they are equally seeded. Because tpe's default is a SHIPPED
/// WIRE: `crates/vike-cli/src/cmd/backtest.rs` forwards `--seed` only when the operator wrote one,
/// so a tpe run with no seed must keep running. Genetic has no such caller, so it is the one method
/// that can still afford to require the input.
fn require_seed(value: Option<&str>) -> Result<u64, String> {
    parse_seed(value)?.ok_or_else(|| {
        "--optimizer genetic requires --seed <u64>. It is not defaulted, deliberately: a genetic \
         search reports ONE sample of a distribution, and a seed nobody typed is a constant the \
         result silently depends on — write `--seed 7` (any u64) and the run is reproducible from \
         your own shell history"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel<'a>(
        optimizer: Option<&'a str>,
        knob: Option<(&'a str, &'a str)>,
    ) -> SearchSelection<'a> {
        let mut s = SearchSelection { optimizer, ..SearchSelection::default() };
        match knob {
            Some(("--euler-depth", v)) => s.euler_depth = Some(v),
            Some(("--trials", v)) => s.trials = Some(v),
            Some(("--seed", v)) => s.seed = Some(v),
            Some((other, _)) => panic!("unknown knob in this fixture: {other}"),
            None => {}
        }
        s
    }

    /// ⚠ THE ONE-ROSTER PROPERTY, on the side that IMPLEMENTS it: every name the protocol says it
    /// can carry builds a method whose `Optimizer::name` is that name. A fifth entry in
    /// `SEARCH_METHODS` with no arm here reddens this — the §15.1 gate's engine leg.
    #[test]
    fn every_protocol_method_builds_the_method_it_names() {
        for name in SEARCH_METHODS {
            // genetic REQUIRES a seed by design, so the fixture supplies one; nothing else does.
            let knob = (name == "genetic").then_some(("--seed", "7"));
            let method = resolve(&sel(Some(name), knob))
                .unwrap_or_else(|e| panic!("{name} must resolve: {e}"));
            assert_eq!(
                optimizer_for(method).name(),
                name,
                "--optimizer {name} must build {name:?}"
            );
            assert_eq!(identity_parts(&method).0, name, "the artifact identity names it too");
        }
    }

    /// No selector at all is the exhaustive grid — what this verb has always run, and what an
    /// omitted wire field must mean.
    #[test]
    fn an_empty_selection_is_the_default_method() {
        let method = resolve(&SearchSelection::default()).expect("an empty selection resolves");
        assert_eq!(optimizer_for(method).name(), DEFAULT_SEARCH_METHOD);
    }

    /// Every knob is refused by every method that does not own it, and the refusal names EVERY
    /// owner rather than the first — an operator who wrote `--optimizer grid --seed 7` must be told
    /// about genetic as well as tpe.
    #[test]
    fn a_knob_is_refused_by_every_method_that_does_not_own_it() {
        for (flag, owners) in METHOD_KNOBS {
            for name in SEARCH_METHODS {
                if owners.iter().any(|o| name.eq_ignore_ascii_case(o)) {
                    continue;
                }
                let err = resolve(&sel(Some(name), Some((flag, "1"))))
                    .expect_err("a knob under a non-owner must be refused");
                assert!(err.contains(flag), "the refusal names the knob: {err}");
                assert!(err.contains(name), "the refusal names the method selected: {err}");
                for owner in *owners {
                    assert!(err.contains(owner), "the refusal names EVERY owner ({owner}): {err}");
                }
            }
        }
    }

    /// ⚠ The seam's whole point: a VALUE is judged once, by this parser, so the message an operator
    /// reads is the same whether they typed `--local` or `--addr`.
    #[test]
    fn a_bad_value_is_refused_by_the_flag_that_owns_it() {
        for (optimizer, flag, value, needle) in [
            ("tpe", "--trials", "abc", "invalid --trials"),
            ("tpe", "--trials", "0", "invalid --trials"),
            ("tpe", "--seed", "-1", "invalid --seed"),
            ("euler", "--euler-depth", "abc", "invalid --euler-depth"),
        ] {
            let err = resolve(&sel(Some(optimizer), Some((flag, value))))
                .expect_err("a bad value must be refused");
            assert!(err.contains(needle), "{flag} {value}: {err}");
        }
    }

    /// The euler cap is a RANGE CHECK, not a clamp — `EulerConfig::with_depth` clamps silently and
    /// the budget line would then report a depth nobody typed. Moved verbatim with the parser.
    #[test]
    fn a_depth_past_the_cap_is_refused_rather_than_clamped() {
        let over = (EulerConfig::MAX_DEPTH_CAP + 1).to_string();
        let err = resolve(&sel(Some("euler"), Some(("--euler-depth", &over))))
            .expect_err("past the cap must refuse");
        assert!(err.contains("cap"), "{err}");
    }

    /// genetic refuses an absent seed and tpe does not, and the asymmetry is a property of the
    /// METHOD rather than of the flag — `GeneticConfig::new` takes the seed as a required parameter
    /// and its doc says why.
    #[test]
    fn genetic_requires_a_seed_and_tpe_does_not() {
        let err = resolve(&sel(Some("genetic"), None)).expect_err("genetic needs a seed");
        assert!(err.contains("--seed"), "{err}");
        assert!(resolve(&sel(Some("tpe"), None)).is_ok(), "tpe's absent seed is its documented 0");
    }

    /// An unknown method names the roster it is not in, rendered from the const so the message and
    /// the check cannot disagree.
    #[test]
    fn an_unknown_method_names_the_roster() {
        let err = resolve(&sel(Some("bogus"), None)).expect_err("bogus is not a method");
        for name in SEARCH_METHODS {
            assert!(err.contains(name), "the refusal lists {name}: {err}");
        }
    }

    /// The five ranking names, including the composite the WIRE could not carry before stage 7.
    #[test]
    fn every_rank_name_resolves_and_a_typo_does_not() {
        assert_eq!(resolve_rank(None).unwrap(), RankChoice::Metric(RankMetric::default()));
        assert_eq!(resolve_rank(Some("MULTI")).unwrap(), RankChoice::Multi);
        for name in ["sharpe", "return", "max_dd", "equity"] {
            assert!(matches!(resolve_rank(Some(name)).unwrap(), RankChoice::Metric(_)), "{name}");
        }
        let err = resolve_rank(Some("bogus")).expect_err("a typo is refused");
        assert!(err.contains("multi"), "the refusal advertises the composite too: {err}");
    }

    /// ⚠ THE PRESERVATION RULE, asserted rather than left in a comment: grid + a classic metric is
    /// the ONE combination that keeps `RankBy::Metric` (which clears every row's `score`), and
    /// everything else stamps an objective label. Getting this wrong changes a shipped JSON
    /// document — `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
    /// `profile_sweep_is_byte_identical_local_and_remote` is what goes red.
    #[test]
    fn only_grid_with_a_classic_metric_takes_the_classic_evaluator() {
        assert!(uses_classic_evaluator(SearchMethod::Grid, RankChoice::Metric(RankMetric::Sharpe)));
        assert!(!uses_classic_evaluator(SearchMethod::Grid, RankChoice::Multi));
        let tpe = resolve(&sel(Some("tpe"), None)).expect("tpe resolves");
        assert!(!uses_classic_evaluator(tpe, RankChoice::Metric(RankMetric::Sharpe)));
    }
}
