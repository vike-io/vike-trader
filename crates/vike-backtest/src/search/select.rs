//! WHICH SEARCH a run performs: the method, the knob each method owns, the ranking, and the
//! evaluator the two together imply. **One implementation, called by both surfaces that select a
//! search**, so a remote run and a `--local` run cannot answer differently.
//!
//! # Why this is a module and not two copies
//!
//! It was one copy until stage 7 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`, and that copy was PRIVATE in
//! `crates/vike-backtest/src/backtest_cli.rs` — behind `datafusion-store`, which
//! `crate::compute_server` (a DEFAULT build since the 2026-09-27 feature collapse — narrower than
//! `datafusion-store` either way) cannot enable. So when the wire
//! learned to carry a method, the server had no way to NAME the rule it had to apply, and the
//! obvious move — writing the ownership table again server-side — is the two-rosters defect the
//! stage exists to kill. The rule lives here instead, in `harness/`, below both callers.
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
// ⚠ `flag_vocab` joins the two consts from the SAME crate, and for the same reason they are there:
// it is the flag VOCABULARY below both argv parsers on this surface, so a refusal here can RENDER
// `--rank-by`'s roster instead of typing it again. See that module's doc for the two measured
// disagreements it exists to end, and this file's `the_rank_by_roster_is_exactly_what_resolve_rank_accepts`
// for the gate holding its rosters equal to what this module actually accepts.
use vike_datahub_client::{DEFAULT_SEARCH_METHOD, SEARCH_METHODS, flag_vocab};

use crate::harness::optimize::{ProgressMode, StderrProgress, TradeFloor};
use crate::harness::{
    BacktestProfile, HarnessError, Optimizer, ParamscanExec, RankMetric, StoreEvaluator,
};
use crate::search::EulerConfig;
use crate::search::objective::{self, Objective};
use vike_ml::tpe::TpeConfig;

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
///
/// ⚠ **The refusal RENDERS the roster rather than restating it**, and the roster is
/// `vike_datahub_client::flag_vocab`'s `RANK_METRICS` — the crate below both argv parsers, chosen
/// for the same reason [`SEARCH_METHODS`] lives there. The literal it replaced was the second of
/// FOUR hand copies of these five names (that const's doc names all four), and the copies had
/// already diverged in a way an operator could feel: the client refused `--rank-by SHARPE`, which
/// [`RankMetric::from_str_ci`] accepts and pins. The rendered bytes are IDENTICAL to the literal,
/// so this changes no message — it removes the second place a sixth metric could be added and
/// missed.
///
/// ⚠ The PARSE deliberately stays here rather than moving into the vocabulary. What a ranking name
/// MEANS is this crate's (a `RankMetric`, an objective, an evaluator); which spellings exist is the
/// vocabulary's. Holding the two equal is a test's job — this file's
/// `the_rank_by_roster_is_exactly_what_resolve_rank_accepts` — not a merge's.
pub fn resolve_rank(name: Option<&str>) -> Result<RankChoice, String> {
    match name {
        None => Ok(RankChoice::Metric(RankMetric::default())),
        Some(s) if s.eq_ignore_ascii_case("multi") => Ok(RankChoice::Multi),
        Some(s) => RankMetric::from_str_ci(s)
            .map(RankChoice::Metric)
            .ok_or_else(|| flag_vocab::refuse_value(RANK_BY_FLAG, s)),
    }
}

/// The `--rank-by` spelling, as a const so the refusal above and the vocabulary lookup that feeds it
/// cannot be typed two ways. It is the flag NAME only — the value roster is
/// `vike_datahub_client::flag_vocab`'s `RANK_METRICS`.
const RANK_BY_FLAG: &str = "--rank-by";

/// The `--min-trades` spelling, on [`RANK_BY_FLAG`]'s terms: the refusal below renders it and the
/// gates that hold the shared vocabulary against this module look it up, so it is typed once.
const MIN_TRADES_FLAG: &str = "--min-trades";

/// The `--progress` spelling, same terms. The VALUE roster is [`ProgressMode::NAMES`] — this is the
/// flag NAME only, exactly as [`RANK_BY_FLAG`] is.
const PROGRESS_FLAG: &str = "--progress";

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
/// rows have their `score` CLEARED (`crate::harness::report_from_outcome`'s `RankBy::Metric` arm),
/// and it is EXACTLY the invocation `vike-cli backtest --local` spawns, `scripts/cli_mcp_smoke.sh`
/// runs, and `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
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
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` does not move and this module adds
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
/// `search::genetic::GeneticConfig::new` takes the seed as a required PARAMETER, and its doc says
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

/// `--min-trades` — the statistical-significance FLOOR, resolved.
///
/// ⚠ **Deliberately NOT a [`METHOD_KNOBS`] row, and that is the whole design of the flag.** Every
/// entry in that table is a knob one or two METHODS own, refused under the rest, because a knob
/// configuring a search you did not select cannot do what it says. This one configures the
/// EVALUATOR, which every method drives — `crate::harness::optimize::apply_trade_floor` sits in the
/// one post-evaluation site both shipped evaluators fold through — so there is no method under
/// which it is meaningless and nothing for an ownership check to refuse. Adding a row would refuse
/// it under three of the four methods for no reason at all. [`resolve_rank`] is the existing
/// precedent for a method-agnostic search knob resolved through its own public function rather than
/// through [`resolve`].
///
/// ⚠ `0` is ACCEPTED and means disarmed, rather than being refused as a pointless value. An
/// operator scripting a sweep matrix wants to write `--min-trades $FLOOR` with `FLOOR=0` for the
/// control arm; refusing it would make them build the argv conditionally, and a conditional argv is
/// where a flag gets dropped from the arm that needed it.
pub fn resolve_min_trades(value: Option<&str>) -> Result<TradeFloor, String> {
    match value {
        None => Ok(TradeFloor::DISARMED),
        Some(s) => match s.parse::<usize>() {
            Ok(n) => Ok(TradeFloor::new(n)),
            // ⚠ The refusal names what `0` MEANS, because `usize` already rejects a negative and
            // the operator who typed `-1` needs to be told which end of the range they wanted
            // rather than merely that their value is not a number.
            // The message is byte-identical to the literal it replaced; what moved is that the
            // flag NAME is now the one const the gates look up.
            Err(_) => Err(format!(
                "invalid {MIN_TRADES_FLAG} {s:?} (expected a non-negative integer; 0 disarms the \
                 floor)"
            )),
        },
    }
}

/// `--progress` — which progress stream, resolved. Unset is
/// `crate::harness::optimize::ProgressMode::Auto`, which emits only when stderr is a terminal.
///
/// ⚠ The refusal RENDERS `ProgressMode::NAMES` rather than restating the three spellings, so a
/// fourth mode cannot be accepted by the parser and missing from the message — the same property
/// `resolve`'s `invalid --optimizer` refusal buys from `SEARCH_METHODS`.
pub fn resolve_progress(value: Option<&str>) -> Result<ProgressMode, String> {
    match value {
        None => Ok(ProgressMode::default()),
        Some(s) => ProgressMode::from_str_ci(s).ok_or_else(|| {
            format!("invalid {PROGRESS_FLAG} {s:?} (expected {})", ProgressMode::NAMES.join("|"))
        }),
    }
}

/// How many evaluations `method` will spend on `base`, or `None` when the method cannot promise a
/// number — `Optimizer::budget_hint` through [`optimizer_for`], so the answer comes from the METHOD
/// rather than from a second table here that could disagree with it.
///
/// It builds a `Box<dyn Optimizer>` to ask, which is the same allocation the caller is about to make
/// anyway; the alternative is a `match` on [`SearchMethod`] duplicating what each impl already
/// states.
pub fn budget_hint_for(method: SearchMethod, base: &BacktestProfile) -> Option<u64> {
    optimizer_for(method).budget_hint(base)
}

/// **The ONE door that arms both observers on a store-driven evaluator** — the floor and the
/// progress stream — so a surface arms them by calling this rather than by remembering two builders
/// and a budget lookup.
///
/// ⚠ **This doc said "so the engine binary and the wire arm cannot arm them differently", and the
/// WIRE ARM DOES NOT CALL IT.** That was false when written (nothing called this at all) and it is
/// still false of one of the two surfaces it named, so here is what is true of the tree:
///
/// * `crates/vike-backtest/src/backtest_cli.rs`'s `run` calls it on every search, unconditionally,
///   with whatever `parse_search_flags` resolved from `--min-trades` and `--progress`.
/// * `crate::compute_server`'s `run_paramscan_profile` deliberately does NOT, and cannot usefully:
///   `vike_datahub_client::proto`'s `WireSearch` carries four fields and neither observer is one of
///   them, so there is nothing for a remote caller to have asked for. Adding the call with
///   defaults would not be a no-op either — `crate::harness::optimize::StderrProgress::for_mode`
///   probes `is_terminal()` on THIS process's stderr, so a daemon started from a terminal would
///   begin writing one progress line per grid point into its own log for runs nobody asked to
///   watch. That is the failure `vike_log`'s file-level knob exists for, bought with no caller's
///   consent.
///
/// So the "cannot arm them differently" property is real but currently vacuous on one side: there
/// is ONE arming surface. The door is what makes a SECOND one cheap and consistent, which is worth
/// having before the second exists — and a `WireSearch` field is what would make the wire the
/// second.
///
/// ⚠ It takes `method` only to ask it for a TOTAL. The floor is method-agnostic (see
/// [`resolve_min_trades`]) and progress is armed on the evaluator for the reason
/// `crate::harness::optimize::ProgressTracker`'s doc gives — the evaluator is the one funnel every
/// candidate of every method passes through.
///
/// ⚠ Both halves are NO-OPS at their defaults: a `TradeFloor::DISARMED` plus a
/// `ProgressMode::Auto` on a non-terminal stderr returns the evaluator with neither field set, so a
/// caller that threads the resolved values unconditionally still produces the byte-identical runs
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs` compares. That is why this is safe to
/// call on every path rather than behind a condition the two surfaces could spell differently.
pub fn arm_observers<'a>(
    eval: StoreEvaluator<'a>,
    method: SearchMethod,
    base: &BacktestProfile,
    floor: TradeFloor,
    progress: ProgressMode,
) -> StoreEvaluator<'a> {
    let eval = eval.with_min_trades(floor);
    match StderrProgress::for_mode(progress) {
        Some(sink) => eval.with_progress(Box::new(sink), budget_hint_for(method, base)),
        None => eval,
    }
}

/// The operator-facing method roster, ONE NAME PER LINE — the body an optimizer LISTING prints on
/// stdout.
///
/// ⚠ **THE DOOR IS `backtest --list-optimizers`, and this doc carried its DEFERRAL until
/// 2026-09-16.** The deferral read "BUILT AND DEFERRED: no door calls this", having itself
/// corrected an earlier claim that a door existed — so this function has now been described three
/// ways, and the third is the one with an arm behind it:
/// `crates/vike-backtest/src/backtest_cli.rs`'s `run` prints this beside its `--list` over
/// `harness::STRATEGIES`, and `vike_datahub_client::flag_vocab`'s `--list-optimizers` row (which
/// left `EXCLUDED` in the same change) is where the route that flag is reachable on is recorded.
///
/// ⚠ **What did NOT change is the CLIENT plane, and the deferral's second half is still true of
/// it.** `crates/vike-cli/src/surface.rs` carries no `FLAGS` row for a listing and
/// `crates/vike-cli/tests/fixtures/cli.json` no entry, deliberately: that row is `EngineOnly` for
/// the reason `--list`'s is, because the client answers the same question through the PUBLISHED
/// ASSET (`ROSTERS`' `optimizers` row) rather than through a flag. So an operator with `vike-cli`
/// and no engine reads the roster out of `cli.json`; an operator with an engine types this.
///
/// The renderers were kept rather than deleted while the door was missing, because THIS is the side
/// of it that must not name methods a fifth time — which is why landing the arm was one `if` block
/// and no new roster.
///
/// ⚠ **The SHAPE is `crate::cmd::strategies`' shape in `vike-cli`, and matching it is the point.**
/// That verb's plain mode prints one name per line "and nothing else — the same shape the engine's
/// own `--list` prints, so a script that piped one can pipe the other". A discovery listing that
/// decorates its output with a header, a count or a default marker is a listing a shell pipeline has
/// to strip; the count and the default belong in [`optimizer_roster_json`], where a machine can read
/// them as fields.
///
/// ⚠ **This renders `vike_datahub_client::SEARCH_METHODS` and cannot render anything else**, which
/// is the entire content of the "one roster, every door" claim. That const already IS the single
/// source — the engine's `--optimizer` spelling check, `vike-cli`'s `OPTIMIZERS`, the wire's
/// `WireSearch::optimizer` and the MCP tool schema all read it, and three gates hold them equal
/// (`crates/vike-backtest/tests/compute_plane.rs`'s roster walk, `crates/vike-cli/tests/backtest_cli/local.rs`'s
/// `every_roster_method_is_accepted_on_both_routes`, and this module's own exhaustiveness test). So
/// a door that lands on top of this renders the existing const rather than becoming a fifth place
/// that names methods — which is the whole reason this half is worth having before its caller does.
pub fn optimizer_roster_lines() -> String {
    SEARCH_METHODS.join("\n")
}

/// The roster as ONE JSON object, for the `--json` half of the listing
/// [`optimizer_roster_lines`] renders in plain text — reached by `backtest --list-optimizers
/// --json`, the same arm, since 2026-09-16. ⚠ This doc read "DEFERRED on exactly the same terms,
/// with no caller of its own"; that door's argument is on the sibling above and this one still does
/// not restate it.
///
/// Carries the two facts the plain listing deliberately omits: how many methods there are, and
/// which one runs when `--optimizer` is absent. Hand-assembled for
/// `crate::harness::optimize::ProgressEvent`'s reason — this crate's harness tree carries no
/// `serde_json` edge, and a method name is a lowercase ASCII identifier with nothing in it for a
/// serializer to escape.
pub fn optimizer_roster_json() -> String {
    let names = SEARCH_METHODS.map(|n| format!("\"{n}\"")).join(",");
    format!(
        "{{\"count\":{},\"default\":\"{DEFAULT_SEARCH_METHOD}\",\"optimizers\":[{names}]}}",
        SEARCH_METHODS.len()
    )
}

#[path = "select_tests.rs"]
#[cfg(test)]
mod select_tests;
