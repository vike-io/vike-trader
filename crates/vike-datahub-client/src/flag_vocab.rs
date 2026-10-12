//! `flag_vocab` — the flag VOCABULARY the two argv parsers on the `backtest` RUN surface share:
//! one row per flag spelling, its ARITY, its VALUE ROSTER, and which of the two routes accepts it.
//! Data plus ONE acceptance predicate. Every parsing MECHANIC stays on the side that owns it.
//!
//! # The disagreement class this exists to end
//!
//! One verb has two routes to the same work and a DIFFERENT parser on each. `--addr` ships the
//! profile to a daemon; `--local` spawns the standalone engine, whose argv is built by
//! `crates/vike-cli/src/cmd/backtest/execute.rs`'s `execute_local` and then parsed a second time by
//! `crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_search_flags` and its
//! neighbours. A third caller — a human on a box that has an engine and no `vike-cli` — types
//! the same flags straight at `backtest`, which that binary's own `USAGE` advertises as the
//! supported spelling. So one flag meets `crates/vike-cli/src/cmd/args.rs`'s `Flags` on one route
//! and `vike_analytics::binutil`'s positional scanners on the other, and nothing held the two
//! answers equal.
//!
//! **Both measured disagreements were VALUE spellings rather than flag names** — which is why a
//! shared list of NAMES would have caught neither. What the sides disagreed about is the PREDICATE
//! that reads a roster:
//!
//! * **`--rank-by SHARPE` and `--optimizer TPE` were ACCEPTED by the engine and REFUSED by the
//!   client.** `crates/vike-backtest/src/harness/sweep/rank.rs`'s `RankMetric::from_str_ci`
//!   lowercases before it matches, and `crates/vike-backtest/src/search/select.rs`'s `resolve`
//!   compares with `eq_ignore_ascii_case`; the client asked `roster.contains(&value)`, an EXACT
//!   match. Closed: `crates/vike-cli/src/cmd/backtest/run_args.rs`'s `parse_run_args` calls
//!   [`accept_value`] on both selector arms. (A module that ends a disagreement states the closure
//!   in the present tense only in the change that writes the CALLER; until then it writes the
//!   residual, as the next bullet does.)
//! * **`--json=1` is REFUSED on two of three doors and SILENTLY IGNORED on the third.**
//!   `crates/vike-cli/src/cmd/args.rs`'s `no_value` refuses it, and so does the engine's `data`
//!   verb (`crates/vike-backtest/src/backtest_cli/data_cmd.rs`'s `triage_data_argv`). The engine's
//!   RUN verb still ignores it: `vike_analytics::binutil`'s `has_flag` is exact-token and answers a
//!   `bool`, so `--json=1` reads as "no `--json`" and prints the human table to a script that
//!   asked for JSON. [`Arity::Bare`] is the fact a triage there needs and
//!   `vike_analytics::binutil`'s `inline_value` the primitive; the wiring is not this module's, and
//!   the residual stays DECLARED rather than silently closed.
//!
//! # Why the shared thing is a vocabulary and not one parser
//!
//! The two parsers are not the same SHAPE, and neither shape is wrong. `Flags` is a CONSUMING
//! iterator adapter: it walks argv once, in order, the only way to express the rules it owns — a
//! valued flag may not eat a next token that is itself a flag (`is_flag_token`), a bare boolean
//! rejects an inline value (`no_value`), `-h` short-circuits through the `Err` channel. `binutil`'s
//! `arg` and `has_flag` are POSITION-INDEPENDENT SCANS that consume nothing, which lets the engine
//! read one argv from a dozen unrelated functions (`profile_from_args`, `parse_addr_flag`,
//! `parse_search_flags`, `parse_keep_trials`, `store_root` each re-scan the slice).
//!
//! Collapsing them is a behaviour change either way. A scan CANNOT implement `is_flag_token`
//! (`--optimizer --json` would have to refuse AND leave JSON output on), and a consuming walk cannot
//! serve the engine's independent readers without threading one parsed struct through the engine
//! and five sibling bins. What both shapes read and neither owns is SHARED: which flags exist, what
//! shape of argument each takes, which VALUES each admits. That is this module — the shape of
//! [`crate::proto::SEARCH_METHODS`] and of `crates/vike-backtest/src/backtest_cli/data_cmd.rs`'s
//! `DATA_FLAGS` (a table that DRIVES a triage rather than being restated inside it).
//!
//! # Why THIS crate
//!
//! The [`crate::proto::SEARCH_METHODS`] argument: this crate is a normal dependency of both
//! `vike-cli` (which spelling-checks before a dial or a spawn) and `vike-backtest` (which
//! implements the flags), and the cure for two sides that must not disagree is a shared crate BELOW
//! both. `const` data and pure functions over `&str`, no dependency, no environment, so it can run
//! before any I/O.

use crate::proto::SEARCH_METHODS;

/// What shape of argument a flag takes — the fact a boolean-versus-valued triage needs and the one
/// neither parser could look up before.
///
/// ⚠ [`Arity::OptionalValue`] is declared although no row uses it: `--addr` is that shape on the
/// engine (a bare `--addr` serves on the configured address), which is why it is an [`EXCLUDED`]
/// row. A missing variant is how an excluded flag gets "completed" into the table with the wrong
/// arity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// A bare boolean. An inline `=value` is a usage error, never a silent `true` — the rule
    /// `crates/vike-cli/src/cmd/args.rs`'s `no_value` states and the engine's RUN verb still does
    /// not apply.
    Bare,
    /// Takes a value, in either spelling (`--flag value` or `--flag=value`); both parsers accept
    /// both.
    Valued,
    /// Takes a value OPTIONALLY: the bare token means something on its own. See the ⚠ above.
    OptionalValue,
}

/// Which of the two routes accepts a spelling.
///
/// [`Route::EngineOnly`] records a live roster DIFFERENCE (the client refuses the spelling as an
/// unknown option), and recording it is the point: an unexamined difference and a decided one look
/// identical from inside a parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Parsed by `vike-cli backtest run` AND by the engine — the flags whose two answers must
    /// match, and the whole reason this module exists.
    Both,
    /// Parsed by the engine only. `vike-cli backtest run` refuses it as an unknown option, so an
    /// operator cannot reach it through the client at all; the engine's `USAGE` advertises it.
    EngineOnly,
}

/// One flag's vocabulary row.
pub struct FlagSpec {
    /// The spelling, dashes included. Long form only — see [`BACKTEST_FLAGS`].
    pub flag: &'static str,
    /// See [`Arity`].
    pub arity: Arity,
    /// The values this flag admits, or EMPTY for a free-form value (a path, a number, a run id).
    /// An empty roster means "nothing declared here refuses anything": the side that RUNS owns what
    /// a value means, and this module owns only which spellings of it are admissible.
    pub values: &'static [&'static str],
    /// See [`Route`].
    pub route: Route,
    /// Why this row reads the way it does — the ownership fact a reader needs before changing it.
    pub why: &'static str,
}

/// The `--rank-by` names — **the ONE roster**. The parse is
/// `crates/vike-backtest/src/search/select.rs`'s `resolve_rank` (its `multi` arm) plus
/// `crates/vike-backtest/src/harness/sweep/rank.rs`'s `RankMetric::from_str_ci`.
/// `crates/vike-backtest/src/harness/profile/walkforward_cfg.rs`'s `rank_metric` refuses with a
/// FOUR-name list because the walk-forward door has no `multi`: a DIFFERENT roster, which is why
/// this const is named for the flag and not for "the rank metrics".
///
/// ⚠ **THE ORDER IS LOAD-BEARING.** `resolve_rank`'s refusal renders this joined by `|`, so
/// reordering it is a visible change to a shipped message, not a tidy-up.
pub const RANK_METRICS: [&str; 5] = ["sharpe", "return", "max_dd", "equity", "multi"];

/// The `--progress` mode spellings, in the order a usage line should print them.
///
/// ⚠ **A COPY**: `vike_backtest::harness::optimize::ProgressMode::NAMES` is not nameable from this
/// crate (it sits BELOW `vike-backtest`), so the crate that CAN see both holds them equal —
/// `crates/vike-backtest/src/search/select_tests.rs`'s
/// `the_progress_roster_is_exactly_what_resolve_progress_accepts` compares the arrays, ORDER
/// included, and drives every member through the real resolver.
pub const PROGRESS_MODES: [&str; 3] = ["auto", "none", "json"];

/// The `backtest` RUN surface, one row per long flag.
///
/// # What this table is scoped to, and what it deliberately is not
///
/// The RUN verb. The `data` verb is a SECOND shared surface with an engine-side vocabulary of its
/// own (`crates/vike-backtest/src/backtest_cli/data_cmd.rs`'s `DATA_FLAGS`); folding it in here
/// would move which door owns a refusal SENTENCE on a verb that deletes stored series irreversibly,
/// which deserves its own argument.
///
/// # Long flags only
///
/// `-h` has no row, deliberately: `crates/vike-cli/src/cmd/args.rs`'s `is_flag_token` keys on `--`
/// so a NEGATIVE NUMBER stays a value, and a short-flag row would invite a `-5` row beside it.
pub const BACKTEST_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        flag: "--profile",
        arity: Arity::Valued,
        values: &[],
        route: Route::Both,
        why: "the profile PATH. Positional on the engine since ruling 14, with this older spelling \
              KEPT because `execute_local` spawns the engine with it",
    },
    FlagSpec {
        flag: "--json",
        arity: Arity::Bare,
        values: &[],
        route: Route::Both,
        why: "print the report as JSON. The `--json=1` residual named in this module's doc is THIS \
              row's arity going unread on the engine's run verb",
    },
    FlagSpec {
        flag: "--rank-by",
        arity: Arity::Valued,
        values: &RANK_METRICS,
        route: Route::Both,
        why: "how to ORDER a search's results, not what work to do — so a VALID value is ignored \
              rather than refused on a profile with no [paramscan] table",
    },
    FlagSpec {
        flag: "--optimizer",
        arity: Arity::Valued,
        values: &SEARCH_METHODS,
        route: Route::Both,
        why: "the search METHOD, and the ONE selector (ruling 13). The roster is the protocol const \
              both sides already read",
    },
    FlagSpec {
        flag: "--euler-depth",
        arity: Arity::Valued,
        values: &[],
        route: Route::Both,
        why: "euler's halving depth. Forwarded UNVALIDATED by the client on purpose: the engine \
              owns the ownership rule, the range and the cap, and a second copy of that table \
              could only ever produce a worse message",
    },
    FlagSpec {
        flag: "--trials",
        arity: Arity::Valued,
        values: &[],
        route: Route::Both,
        why: "tpe's trial budget. Forwarded unvalidated, same argument as --euler-depth",
    },
    FlagSpec {
        flag: "--seed",
        arity: Arity::Valued,
        values: &[],
        route: Route::Both,
        why: "the reproducibility seed, owned by tpe AND genetic. Forwarded unvalidated, same \
              argument as --euler-depth",
    },
    FlagSpec {
        flag: "--help",
        arity: Arity::Bare,
        values: &[],
        route: Route::Both,
        why: "a SUCCESS on both routes, with the usage on stdout — a non-zero help breaks every \
              `set -e` caller",
    },
    FlagSpec {
        flag: "--keep-trials",
        arity: Arity::Valued,
        values: &["none", "scalars", "returns"],
        route: Route::EngineOnly,
        why: "what a search leaves behind. `series` is REFUSED BY NAME with its own reason \
              (`crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_keep_trials`), \
              so a client adopting this row must RENDER that refusal rather than fall back to \
              the generic one. \
              ⚠ This row read `none|scalars` until 2026-09-16 and was SHORT BY ONE: `returns` \
              shipped with the anti-overfitting statistics and nothing held the roster against the \
              parser, so `accepts_value` refused a spelling the engine accepts. The gate it was \
              missing is `the_keep_trials_roster_is_exactly_what_the_parser_accepts`, in the file \
              that owns the parse",
    },
    FlagSpec {
        flag: "--resume",
        arity: Arity::Valued,
        values: &[],
        route: Route::EngineOnly,
        why: "continue an interrupted SEARCH by run id. Unreachable through the client, which \
              refuses it as an unknown option — a live roster difference, recorded here rather \
              than resolved",
    },
    FlagSpec {
        flag: "--list",
        arity: Arity::Bare,
        values: &[],
        route: Route::EngineOnly,
        why: "print every registered strategy name. The client answers the same question with a \
              sub-verb, so the two surfaces differ by SHAPE rather than by capability and neither \
              is missing anything",
    },
    FlagSpec {
        flag: "--version",
        arity: Arity::Bare,
        values: &[],
        route: Route::EngineOnly,
        why: "the engine binary's own version. The client's `--version` is a TOP-LEVEL flag rather \
              than a flag of this verb, which is why this row is EngineOnly and not Both",
    },
    FlagSpec {
        flag: "--min-trades",
        arity: Arity::Valued,
        values: &[],
        route: Route::EngineOnly,
        why: "the statistical-significance FLOOR a search's candidates must clear. EngineOnly \
              because the WIRE cannot carry it: `crate::proto`'s `WireSearch` has four fields and \
              a fifth is a protocol change plus a capability negotiation (the shape \
              `crate::FEATURE_SEARCH_METHOD` already has). A client that accepted it would have to \
              forward it on `--local` and DROP it on `--addr`, which is the silent downgrade that \
              feature gate exists to refuse. No roster: a non-negative integer, `0` disarming, \
              parsed by `crates/vike-backtest/src/search/select.rs`'s `resolve_min_trades`",
    },
    FlagSpec {
        flag: "--progress",
        arity: Arity::Valued,
        values: &PROGRESS_MODES,
        route: Route::EngineOnly,
        why: "which progress stream a search emits. EngineOnly BY NATURE rather than by deferral — \
              the sink writes to the process's own stderr \
              (`crates/vike-backtest/src/harness/optimize/progress.rs`'s `StderrProgress`, \
              which holds no \
              redirectable handle), and over a socket that stream is the DAEMON's terminal rather \
              than the operator's. There is no route for a client to adopt, so this row will not \
              become `Both` the way --min-trades' could",
    },
    FlagSpec {
        flag: "--list-optimizers",
        arity: Arity::Bare,
        values: &[],
        route: Route::EngineOnly,
        why: "print the search-method roster and exit — the `--optimizer` twin of `--list`, and \
              EngineOnly for the same reason that row is: the client answers the same question \
              through a PUBLISHED ASSET (`crates/vike-cli/src/surface.rs`'s `ROSTERS` row \
              `optimizers`, rendered into cli.json) rather than through a flag, so the two \
              surfaces differ by SHAPE and neither is missing anything",
    },
];

/// Spellings deliberately kept OUT of [`BACKTEST_FLAGS`], each with the reason.
///
/// ⚠ **A pinned exclusion, not a gap.** `every_excluded_flag_stays_out_of_the_table` iterates this
/// const, so "complete the table" cannot quietly add one of these rows: they are the spellings
/// where the two sides MUST differ (or both refuse), and a naive equality gate would force one side
/// to change.
pub const EXCLUDED: &[(&str, &str)] = &[
    (
        "--addr",
        "the two sides of ONE socket, so the same spelling is a different flag on each. On the \
         engine it means BECOME a daemon and its value is OPTIONAL (a bare --addr serves on the \
         configured address); on the client it means DIAL a daemon and a value is required. One \
         `arity` field cannot hold both, and a row asserting either would make the other side's \
         parser look wrong when it is right.",
    ),
    (
        "--search",
        "RETIRED on the engine and refused there BY NAME, naming --optimizer as its replacement; \
         on the client it never existed and falls into the unknown-argument arm. Both routes \
         refuse it, so there is no disagreement for a row to close — only two messages, and the \
         engine's is the one worth reaching.",
    ),
    (
        "--store",
        "REFUSED on both routes since 2026-09-25, BY NAME, with its replacement named — the owner \
         closed the local READ door, so a run reads history from a datahub and a local one is \
         `vike-backend datahub --store DIR` beside it. It sat in the shared table as a \
         `Both` row until then, because `execute_local` forwarded it to the engine. Both parsers \
         now answer it with [`store_flag_removed`], so there is no disagreement left for a row to \
         close — the `--search` shape, which is why it lives beside that row.",
    ),
];

/// The ONE sentence every history reader prints when it refuses `--store`, so the replacement is
/// spelled the same everywhere it is offered.
///
/// ⚠ **It lives HERE, in the ungated vocabulary, rather than in `crate::route`**: `route` sits
/// behind `hist-route`, which `vike-cli` does not enable, and `vike-cli backtest run` must refuse
/// the flag BEFORE it spawns an engine.
///
/// ⚠ It names the decision by NUMBER, never by its `docs/` path: `docs/` is withheld from the
/// public mirror, and `vike-cli`'s published surface carries this sentence verbatim, where
/// `crates/vike-cli/src/surface_tests.rs`'s `no_row_cites_a_path_the_mirror_withholds` refuses
/// the path.
///
/// ⚠ A REFUSAL, never a silent ignore and never an "unknown flag": somebody who passed `--store DIR`
/// believes a directory will be read, and the honest answer names what changed and the one command
/// that gives them that run now.
///
/// ⚠ **That command is `vike-backend datahub`, never `vike datahub`.** The shipped multicall is
/// `vike-backend` (`crates/vike/Cargo.toml`'s `name`) and matches a tool by EXACT string with no
/// aliases (`crates/vike/src/lib.rs`'s `resolve`); the mixed spelling is "command not found".
/// `the_store_refusal_names_the_shipped_binary` holds it.
pub fn store_flag_removed(verb: &str) -> String {
    format!(
        "{verb}: `--store DIR` no longer reads history locally — every history READ goes through a \
         datahub since 2026-09-25 (decision 0084). \
         For a local run, start one beside it with `vike-backend datahub --store DIR`: \
         with no node keys it authenticates nothing and binds loopback only, and this verb dials \
         loopback by default."
    )
}

/// The row for `flag`, or `None` if this vocabulary does not describe that spelling.
///
/// ⚠ **EXACT match, never a prefix**: `--seed` sits beside `--seed-demo`, `--list` beside
/// `--list-optimizers` (both rows here), so a prefix lookup would answer the wrong row.
pub fn spec(flag: &str) -> Option<&'static FlagSpec> {
    BACKTEST_FLAGS.iter().find(|s| s.flag == flag)
}

/// The values `flag` admits — EMPTY for a free-form value, and empty for a flag this vocabulary
/// does not describe.
///
/// A refusal should RENDER this rather than restate it, so a sixth value cannot be accepted by the
/// parser and missing from the message.
pub fn value_roster(flag: &str) -> &'static [&'static str] {
    spec(flag).map_or(&[], |s| s.values)
}

/// The CANONICAL spelling of `value` for `flag`, matched ASCII-CASE-INSENSITIVELY, or `None` when
/// `flag` declares no roster or `value` is not one of its members.
///
/// ⚠ **Case-insensitive because that is what the side that RUNS already does** (the module doc's
/// first bullet): adopting the client's exact match instead would have NARROWED the engine, and
/// widening the client is the direction in which nothing that works stops working.
///
/// ⚠ ASCII case, not Unicode: every roster member is ASCII, and `to_lowercase` would make a Turkish
/// dotless i a live question for no reason. Same rule as the two engine parsers.
pub fn canonical_value(flag: &str, value: &str) -> Option<&'static str> {
    value_roster(flag).iter().copied().find(|m| m.eq_ignore_ascii_case(value))
}

/// Whether `flag` admits `value`. A flag with no declared roster admits everything — this module
/// owns which SPELLINGS are admissible, never what a value means.
pub fn accepts_value(flag: &str, value: &str) -> bool {
    value_roster(flag).is_empty() || canonical_value(flag, value).is_some()
}

/// The ONE refusal sentence for a value outside a flag's roster.
///
/// Byte-identical in shape to the message `crates/vike-backtest/src/search/select.rs`'s
/// `resolve` already produces for `--optimizer`, which is what lets both routes reach one sentence
/// with no operator-visible change: `invalid --optimizer "TPE" (expected grid|euler|tpe|genetic)`.
pub fn refuse_value(flag: &str, value: &str) -> String {
    format!("invalid {flag} {value:?} (expected {})", value_roster(flag).join("|"))
}

/// **THE acceptance decision, for both routes.** Returns the CANONICAL spelling of `value`, or the
/// one refusal sentence.
///
/// The function a client-side spelling check calls instead of owning a roster and a match rule —
/// `crates/vike-cli/src/cmd/backtest/run_args.rs`'s `parse_run_args`, on its `--rank-by` and
/// `--optimizer` arms. It answers `Ok(value)` unchanged for a flag with no declared roster.
///
/// ⚠ **It returns the CANONICAL spelling, and forwarding that rather than the operator's typing is
/// the half that finishes the fix.** The client forwards these flags to a spawned engine
/// (`--local`) or puts them in a [`crate::proto::WireSearch`] (`--addr`), whose fields are
/// `String`s; forwarding `tpe` rather than `TPE` means every downstream reader — the engine's
/// resolver, the run ARTIFACT's identity, a possibly older daemon — sees one spelling, which keeps
/// `--local` a byte-identical rehearsal of `--addr`.
pub fn accept_value(flag: &str, value: &str) -> Result<String, String> {
    if value_roster(flag).is_empty() {
        return Ok(value.to_string());
    }
    match canonical_value(flag, value) {
        Some(canonical) => Ok(canonical.to_string()),
        None => Err(refuse_value(flag, value)),
    }
}

#[path = "flag_vocab_tests.rs"]
#[cfg(test)]
mod flag_vocab_tests;
