//! `flag_vocab` — the flag VOCABULARY the two argv parsers on the `backtest` RUN surface share:
//! one row per flag spelling, its ARITY, its VALUE ROSTER, and which of the two routes accepts it.
//! Data plus ONE acceptance predicate. Every parsing MECHANIC stays on the side that owns it.
//!
//! # The disagreement class this exists to end
//!
//! One verb has two routes to the same work and a DIFFERENT parser on each. `--addr` ships the
//! profile to a daemon; `--local` spawns the standalone engine, whose argv is built by
//! `crates/vike-cli/src/cmd/backtest.rs`'s `execute_local` and then parsed a second time by
//! `crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags` and its neighbours. A third
//! caller — a human on a box that has an engine and no `vike-cli` — types the same flags straight
//! at `backtest`, which that binary's own `USAGE` advertises as the supported spelling. So one flag
//! meets `crates/vike-cli/src/cmd/args.rs`'s `Flags` on one route and `vike_analytics::binutil`'s
//! positional scanners on the other, and nothing held the two answers equal.
//!
//! **Two disagreements were MEASURED before this module existed, and BOTH are VALUE spellings
//! rather than flag names** — which is exactly why a shared list of NAMES would have caught
//! neither. The two sides already agree on every name, and `--optimizer`'s roster is already one
//! const ([`crate::proto::SEARCH_METHODS`], read by both). What they disagreed about is the
//! PREDICATE that reads a roster:
//!
//! * **`--rank-by SHARPE` and `--optimizer TPE` were ACCEPTED by the engine and REFUSED by the
//!   client.** `crates/vike-backtest/src/harness/sweep.rs`'s `RankMetric::from_str_ci` lowercases
//!   before it matches, and `crates/vike-backtest/src/harness/search_select.rs`'s `resolve`
//!   compares every method name with `eq_ignore_ascii_case`; the client's spelling check asked
//!   `roster.contains(&value)`, an EXACT match. The two sides' TESTS contradicted each other in one
//!   workspace: `from_str_ci("SHARPE")` is pinned as `Some(RankMetric::Sharpe)` in
//!   `crates/vike-backtest/src/harness/sweep.rs` while the client's parser refused the identical
//!   token by name. An operator who learned the flag from `backtest --help` and then reached for
//!   `vike-cli` got a usage error for a spelling the engine documents and accepts.
//!
//!   ⚠ **The past tense above was written before the caller existed, and is a measurement now.**
//!   This module landed in the same dedup wave as this note, and for the length of that wave
//!   nothing in `vike-cli` referenced it at all: the client still owned a five-element `--rank-by`
//!   literal and an exact `roster.contains(&value)`, so "REFUSED by the client" described a
//!   refusal that was still live, and [`accept_value`]'s "the function a client-side spelling
//!   check calls" named a caller that did not exist.
//!   `crates/vike-cli/src/cmd/backtest.rs`'s `parse_run_args` calls it on both selector arms now
//!   and that file carries the deletion of its copy. The rule this leaves, which is the half worth
//!   keeping: a module that ENDS a disagreement may only state the closure in the present tense in
//!   the change that writes the CALLER — otherwise write the residual the way the `--json=1`
//!   bullet below does, and a reader can tell an examined difference from a closed one.
//! * **`--json=1` is REFUSED on two of three doors and SILENTLY IGNORED on the third.**
//!   `crates/vike-cli/src/cmd/args.rs`'s `no_value` refuses it; the engine's own `data` verb
//!   refuses it through `crates/vike-backtest/src/backtest_cli.rs`'s `triage_data_argv`, whose
//!   `DATA_FLAGS` table says in its own doc what the hole cost on `rm` — a run written as a
//!   rehearsal performed the deletion. The engine's RUN verb is the door that still ignores it,
//!   because `vike_analytics::binutil`'s `has_flag` is exact-token and answers a `bool`, so it
//!   cannot refuse anything: `--json=1` reads as "no `--json`" and prints the human table to a
//!   script that asked for JSON. [`Arity::Bare`] is the fact a triage on that verb needs and
//!   `vike_analytics::binutil`'s `inline_value` is the primitive it needs; the wiring is not this
//!   module's, and the residual stays DECLARED rather than silently closed.
//!
//! # Why the shared thing is a vocabulary and not one parser
//!
//! The two parsers are not the same SHAPE, and neither shape is wrong. `Flags` is a CONSUMING
//! iterator adapter: it walks argv once, in order, which is the only way to express the rules it
//! owns — a valued flag may not eat the next token when that token is itself a flag
//! (`is_flag_token`), a bare boolean rejects an inline value (`no_value`), `-h` short-circuits out
//! through the `Err` channel, and a parse error becomes an exit rung. `binutil`'s `arg` and
//! `has_flag` are POSITION-INDEPENDENT SCANS over a `&[String]` that consume nothing, which is what
//! lets the engine read one argv from a dozen unrelated functions at a dozen different times:
//! `profile_from_args`, `parse_addr_flag`, `parse_search_flags`, `parse_keep_trials` and
//! `store_root` each re-scan the whole slice, and `flag_given` is only expressible as a scan
//! (`has_flag(args, flag) || arg(args, flag).is_some()`).
//!
//! Collapsing them would have to pick one shape and rewrite the other side around it, and both
//! directions are a behaviour change rather than a refactor. A scan CANNOT implement
//! `is_flag_token`: nothing is consumed, so a token refused as one flag's value is still visible to
//! every other scan, and `--optimizer --json` would have to refuse AND leave JSON output on. A
//! consuming walk cannot serve the engine's dozen independent readers without threading one parsed
//! struct through 5,000 lines of one file and five sibling bins. So the merge buys a name and pays
//! in rewrites of two working parsers, on the surface an operator drives a live trading box from.
//!
//! What was genuinely SHARED is the thing both shapes read and neither owns: which flags exist,
//! what shape of argument each takes, and which VALUES each admits. That is this module, and it is
//! the shape this tree has already chosen twice for the same problem —
//! [`crate::proto::SEARCH_METHODS`] (the method roster, in this crate for this reason, argued in its
//! own doc) and `crates/vike-backtest/src/backtest_cli.rs`'s `DATA_FLAGS` (the `data` verb's
//! `(flag, takes_value)` table, which DRIVES a triage rather than being restated inside it).
//!
//! # Why THIS crate
//!
//! [`crate::proto::SEARCH_METHODS`]'s own doc settled this placement question for half of this
//! vocabulary already, and the argument is unchanged: `vike-datahub-client` is a normal dependency
//! of both `vike-cli` (which spelling-checks before a dial or a spawn) and `vike-backtest` (which
//! implements the methods), and this workspace's cure for two sides that must not disagree is a
//! shared crate BELOW both. Putting `--rank-by`'s roster anywhere else would give the two halves of
//! one vocabulary two homes, which is the same defect wearing a smaller size.
//!
//! This module adds no dependency and reads no environment — `const` data and pure functions over
//! `&str` — so it costs the light client nothing and can be called from a parser that runs before
//! any I/O.

use crate::proto::SEARCH_METHODS;

/// What shape of argument a flag takes — the fact a boolean-versus-valued triage needs and the one
/// neither parser could look up before.
///
/// ⚠ [`Arity::OptionalValue`] is declared although no row currently uses it, and that is a
/// statement about the surface rather than dead code: `--addr` is exactly that shape on the engine
/// (a bare `--addr` serves on the configured address), and it is why that flag is an [`EXCLUDED`]
/// row rather than a [`BACKTEST_FLAGS`] one. A variant missing here is how an excluded flag gets
/// "completed" into the table later with the wrong arity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// A bare boolean. An inline `=value` is a usage error, never a silent `true` — the rule
    /// `crates/vike-cli/src/cmd/args.rs`'s `no_value` states and the engine's RUN verb still does
    /// not apply.
    Bare,
    /// Takes a value, in either spelling (`--flag value` or `--flag=value`). Both parsers accept
    /// both spellings today; `vike_analytics::binutil`'s `arg` carries what the inline half cost
    /// while only one of them did.
    Valued,
    /// Takes a value OPTIONALLY: the bare token means something on its own. See the ⚠ above.
    OptionalValue,
}

/// Which of the two routes accepts a spelling.
///
/// A row is not an aspiration: [`Route::EngineOnly`] records a spelling the engine accepts and the
/// client refuses as an unknown option, which is a live roster DIFFERENCE and somebody's decision to
/// make rather than this module's. Recording it is the point — an unexamined difference and a
/// decided one look identical from inside a parser.
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

/// The `--rank-by` names. **The ONE roster; four hand copies preceded it.**
///
/// The copies, named so that a fifth is recognisable: `crates/vike-cli/src/cmd/backtest.rs` carried
/// a five-element array of its own; `crates/vike-backtest/src/harness/search_select.rs`'s
/// `resolve_rank` TYPED the same five names into its refusal sentence; that function's `multi` arm
/// plus `crates/vike-backtest/src/harness/sweep.rs`'s `RankMetric::from_str_ci` are the parse; and
/// `crates/vike-backtest/src/harness/profile.rs`'s `rank_metric` refuses with a FOUR-name list,
/// because the walk-forward door genuinely has no `multi`. That last one is a DIFFERENT roster
/// rather than a fifth copy of this one, which is why this const is named for the flag and not for
/// "the rank metrics".
///
/// ⚠ **THE ORDER IS LOAD-BEARING.** `resolve_rank`'s refusal renders this joined by `|`, and it
/// renders BYTE-IDENTICALLY to the literal it replaced only in this order. Reordering it is a
/// visible change to a shipped message, not a tidy-up.
pub const RANK_METRICS: [&str; 5] = ["sharpe", "return", "max_dd", "equity", "multi"];

/// The `--progress` mode spellings, in the order a usage line should print them.
///
/// ⚠ **A COPY, and the difference from the rest of this module is WHERE its gate lives.**
/// `vike-datahub-client` takes no dependency on `vike-backtest` — this crate sits BELOW it and
/// `crates/vike-ops/tests/layer_gate.rs` fails the edge — so
/// `vike_backtest::harness::optimize::ProgressMode::NAMES`, which is the ONE roster its resolver
/// walks and its refusal renders, is not nameable here at all. What makes the copy acceptable is
/// that the crate which CAN see both holds them equal:
/// `crates/vike-backtest/src/harness/search_select_tests.rs`'s
/// `the_progress_roster_is_exactly_what_resolve_progress_accepts` compares this array to that one
/// and then drives every member through the real resolver, both directions — the same shape, in the
/// same file, as [`RANK_METRICS`]' own gate.
///
/// ⚠ The ORDER matches `ProgressMode::NAMES` and that test asserts it, so [`refuse_value`] renders
/// the same sequence the engine's own `invalid --progress` sentence does.
pub const PROGRESS_MODES: [&str; 3] = ["auto", "none", "json"];

/// The `backtest` RUN surface, one row per long flag.
///
/// # What this table is scoped to, and what it deliberately is not
///
/// The RUN verb. The `data` verb is a SECOND shared surface — `vike-cli data <sub>` SPAWNS
/// `backtest data <sub>` — and it already has an engine-side vocabulary of its own
/// (`crates/vike-backtest/src/backtest_cli.rs`'s `DATA_FLAGS`) driving a triage that reimplements
/// the client's three shape rules. Folding that table in here is the obvious next step and is NOT
/// done in this pass: it would move which door owns a refusal SENTENCE on a verb that deletes
/// stored series irreversibly, and that deserves its own argument rather than riding in on this one.
///
/// # Long flags only
///
/// `-h` is accepted on both routes and has no row, deliberately. `crates/vike-cli/src/cmd/args.rs`'s
/// `is_flag_token` keys on `--` precisely so a NEGATIVE NUMBER stays a value, so a short-flag row
/// here would invite a `-5` row beside it and hand the vocabulary a question it can only answer
/// wrong. Short spellings are each parser's own business.
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
              (`crates/vike-backtest/src/backtest_cli.rs`'s `parse_keep_trials`), so a client \
              adopting this row must RENDER that refusal rather than fall back to the generic one. \
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
              parsed by `crates/vike-backtest/src/harness/search_select.rs`'s `resolve_min_trades`",
    },
    FlagSpec {
        flag: "--progress",
        arity: Arity::Valued,
        values: &PROGRESS_MODES,
        route: Route::EngineOnly,
        why: "which progress stream a search emits. EngineOnly BY NATURE rather than by deferral — \
              the sink writes to the process's own stderr \
              (`crates/vike-backtest/src/harness/optimize.rs`'s `StderrProgress`, which holds no \
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
/// const, so "complete the table" cannot quietly add one of these rows — which matters more than it
/// looks, because the exclusions are exactly the spellings where the two sides MUST differ and a
/// naive equality gate would force one of them to change.
///
/// ⚠ **`--min-trades` and `--progress` LEFT this const on 2026-09-16, exactly as their own rows
/// said they would.** Both read "a resolver with no argv door yet … no parser calls it", and both
/// promised to "join the table in the PR that gives it a door, which is also the PR that can say
/// which routes accept it". That PR is the one that deleted these two rows:
/// `crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags` parses both now, and both
/// joined [`BACKTEST_FLAGS`] as [`Route::EngineOnly`] — each row carrying the reason its route is
/// that and not `Both`. The two remaining rows are exclusions for a different reason entirely (a
/// spelling that means two things, and a spelling both sides refuse), so nothing here is waiting
/// for a door any more.
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
         `VIKE_DATAHUB_STORE=DIR vike-backend datahub` beside it. It sat in the shared table as a \
         `Both` row until then, because `execute_local` forwarded it to the engine. Both parsers \
         now answer it with [`store_flag_removed`], so there is no disagreement left for a row to \
         close — the `--search` shape, which is why it lives beside that row.",
    ),
];

/// The ONE sentence every history reader prints when it refuses `--store`, so the replacement is
/// spelled the same everywhere it is offered.
///
/// ⚠ **It lives HERE, in the ungated vocabulary, rather than in `crate::route`.** It was written
/// there first and moved within the day: `route` sits behind `hist-route`, which `vike-cli` does not
/// enable, and `vike-cli backtest run` is exactly the parser that must refuse the flag BEFORE it
/// spawns an engine. A sentence one of its two callers cannot reach would have become two sentences.
///
/// ⚠ It names the decision by NUMBER, never by its `docs/` path: `docs/` is withheld from the
/// public mirror, and `vike-cli`'s published surface carries this sentence verbatim, where
/// `crates/vike-cli/src/surface_tests.rs`'s `no_row_cites_a_path_the_mirror_withholds` refuses the path.
///
/// ⚠ A REFUSAL, never a silent ignore and never an "unknown flag": somebody who passed `--store DIR`
/// believes a directory will be read, and the honest answer names both what changed and the one
/// command that gives them that run now.
///
/// ⚠ **That command is `vike-backend datahub`, and for its first day this sentence said
/// `vike datahub` — a spelling no release has ever run.** The shipped multicall is `vike-backend`
/// (`crates/vike/Cargo.toml`'s `name`; there is no `[[bin]]`), and it matches a tool by EXACT
/// string with no aliases (`crates/vike/src/lib.rs`'s `resolve`). `vike` is the PRE-RENAME binary,
/// and it called this tool `vike-datahub`: the verb lost its prefix in the same cut that renamed
/// the binary, so the mixed spelling is "command not found" on a current install and a tool-list
/// exit 2 on an old one. Every reader that refuses `--store` printed it, and it was the one line in
/// the refusal the operator was meant to paste. `the_store_refusal_names_the_shipped_binary` holds
/// it.
pub fn store_flag_removed(verb: &str) -> String {
    format!(
        "{verb}: `--store DIR` no longer reads history locally — every history READ goes through a \
         datahub since 2026-09-25 (decision 0084). \
         For a local run, start one beside it with `VIKE_DATAHUB_STORE=DIR vike-backend datahub`: \
         with no node keys it authenticates nothing and binds loopback only, and this verb dials \
         loopback by default."
    )
}

/// The row for `flag`, or `None` if this vocabulary does not describe that spelling.
///
/// ⚠ **EXACT match, never a prefix**, and the adjacency landmine is live rather than theoretical:
/// `--seed` sits beside `--seed-demo` and `--fetch` beside `--fetch-starter` in the engine's own
/// retirement table, and `--size` beside `--sizes` across the `cheap_np_*` family. A prefix lookup
/// would answer `--seed`'s row for `--seed-demo`, which is the WORSE defect
/// `vike_analytics::binutil`'s `arg` is anchored on `=` to avoid.
pub fn spec(flag: &str) -> Option<&'static FlagSpec> {
    BACKTEST_FLAGS.iter().find(|s| s.flag == flag)
}

/// The values `flag` admits — EMPTY for a free-form value, and empty for a flag this vocabulary
/// does not describe.
///
/// A refusal should RENDER this rather than restate it, so a sixth value cannot be accepted by the
/// parser and missing from the message. That is the rule
/// `crates/vike-backtest/src/harness/optimize.rs`'s `ProgressMode` already states for its own
/// roster, applied across two crates instead of within one.
pub fn value_roster(flag: &str) -> &'static [&'static str] {
    spec(flag).map_or(&[], |s| s.values)
}

/// The CANONICAL spelling of `value` for `flag`, matched ASCII-CASE-INSENSITIVELY, or `None` when
/// `flag` declares no roster or `value` is not one of its members.
///
/// ⚠ **Case-insensitive because that is what the side that RUNS already does**, and the direction
/// of that choice is the whole fix. `crates/vike-backtest/src/harness/sweep.rs`'s
/// `RankMetric::from_str_ci` lowercases and `crates/vike-backtest/src/harness/search_select.rs`'s
/// `resolve` compares with `eq_ignore_ascii_case`, both pinned by their own tests. Adopting the
/// client's exact match instead would have NARROWED the engine: `--rank-by SHARPE` works on a box
/// today, and a workspace that took it away would be fixing a disagreement by breaking the side
/// that was already reachable. Widening the client is the direction in which nothing that works
/// stops working.
///
/// ⚠ ASCII case, not Unicode: every roster member is ASCII, and `to_lowercase` would make a Turkish
/// dotless i a live question on a flag value for no reason. Same rule as the two engine parsers.
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
/// Byte-identical in shape to the message `crates/vike-backtest/src/harness/search_select.rs`'s
/// `resolve` already produces for `--optimizer`, which is what lets both routes reach one sentence
/// with no operator-visible change: `invalid --optimizer "TPE" (expected grid|euler|tpe|genetic)`.
pub fn refuse_value(flag: &str, value: &str) -> String {
    format!("invalid {flag} {value:?} (expected {})", value_roster(flag).join("|"))
}

/// **THE acceptance decision, for both routes.** Returns the CANONICAL spelling of `value`, or the
/// one refusal sentence.
///
/// This is the function a client-side spelling check calls instead of owning a roster and a match
/// rule of its own — `crates/vike-cli/src/cmd/backtest.rs`'s `parse_run_args` is that caller, on
/// its `--rank-by` and `--optimizer` arms, and the module doc records that this sentence named no
/// caller for the length of the wave that wrote it. It answers `Ok(value)` unchanged for a flag
/// with no declared roster, so a caller that routes a free-form flag through it gets the value back
/// rather than a refusal nobody could act on.
///
/// ⚠ **It returns the CANONICAL spelling, and forwarding that rather than the operator's own typing
/// is the half that finishes the fix.** The client's `--local` arm forwards these flags to a spawned
/// engine and its `--addr` arm puts them on the wire in a [`crate::proto::WireSearch`], whose fields
/// are deliberately `String`s. Forwarding `TPE` verbatim would leave the canonicalisation to happen
/// twice, in two places, one of which is a daemon that may be older than this fix; forwarding `tpe`
/// means every downstream reader — the engine's resolver, the run ARTIFACT's identity, a remote
/// server — sees the one spelling, which is what keeps `--local` a byte-identical rehearsal of
/// `--addr` on a search.
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
