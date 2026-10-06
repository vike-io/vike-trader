//! The `hist` group (WHEN = a past window): its grammar, its argument and row types, its dispatch.
use std::path::Path;

#[cfg(test)]
use vike_model::parse_date_label;
use vike_node_proto::auth::NodeKeys;

use self::coverage::execute_coverage;
#[cfg(test)]
use self::coverage::{MAX_PARTIAL_DAYS_SHOWN, coverage_json, coverage_lines};
use self::engine::execute_engine;
#[cfg(test)]
use self::engine::{engine_argv, report_json};
use self::export_remote::execute_export_remote;
#[cfg(test)]
use self::fetch::is_missing_backfill_feature;
use self::fetch::{execute_cancel, execute_fetch, execute_running};
use self::gate::execute_gate;
use self::get::execute_get;
use self::import_exec::execute_import;
#[cfg(test)]
use self::import_exec::import_request;
#[cfg(test)]
use self::list::{account_exclusion_note, class_cell, class_error_line, gap_lines};
use self::list::{execute_list, refuse_an_account_kind_on_a_read};
#[cfg(test)]
use self::list::{list_json, list_lines, refuse_an_account_kind_filter};
#[cfg(test)]
use self::parse::parse;
use self::refuse::{
    check_spec, membership_window, refuse_a_blank_produced_by,
    refuse_a_producer_path_on_the_remote_route, refuse_a_window, refuse_an_account_kind,
    refuse_foreign_flags, window_from,
};
use self::repair::execute_repair;
#[cfg(test)]
use self::repair::repair_engine_argv;
use self::rm::execute_rm;
#[cfg(test)]
use self::rm::rm_engine_argv;
use self::tape_health::execute_tape_health;
use self::universe::execute_universe;
#[cfg(test)]
use super::shared::{DEFAULT_ADDR, ROW_VERB, UNBUILT_FORMATS};
use super::shared::{FILE_VERB, Source, col, empty_note, scope_cell};
#[cfg(test)]
use super::usage::USAGE;
#[cfg(test)]
use super::{KEY_EXPLAIN, KEY_MAX_GAP, KEY_ON_GAP, KEY_REQUIRE_COVERAGE, KEY_UNIVERSE};
#[cfg(test)]
use super::{explain_data_override, max_gap_override, on_gap_override};
#[cfg(test)]
use super::{require_coverage_override, universe_override};
use crate::exit::{CmdResult, Exit};

/// THE OPERATOR'S DOOR ONTO RUNNING FETCHES — `running` and `cancel`, rendered. A module of its own
/// for [`get`]'s reason: every line is a pure function over the wire's rows and an address, so the
/// wording is tested without a socket.
pub(super) mod backfills;
pub(super) mod coverage;
pub(super) mod engine;
/// ROWS OUT OF A **REMOTE** STORE — `export`'s second route, which walks the wire in fixed
/// wall-clock windows and writes `jsonl`/`csv` to `--out`. A module of its own for [`get`]'s
/// reason and for one of its own: the walk is pure arithmetic over two bounds and a step, the
/// CSV grammar is three decisions that have to be stated beside the code that makes them, and
/// neither can be tested through a socket.
mod export;
mod export_remote;
mod fetch;
/// ONE REQUEST PER CALENDAR YEAR for a long `fetch` — and the rule for which venues may be cut at
/// all, which is the half that matters: the store dedups by commit key, so a cut is invisible only
/// where the venue's lane keys what it stores by the UTC-day grid rather than by the request. A
/// module of its own for [`get`]'s reason: the cut is pure arithmetic over two bounds, and the stop
/// rule must be testable without a socket.
mod fetch_split;
/// DATA READINESS AS AN EXIT CODE: the one verb here whose product is a NUMBER a CI step branches
/// on rather than a table a person reads. A module of its own for [`tape_health`]'s reason and for
/// one of its own — the judging is a pure fold over plain numbers, and a verdict whose rung is
/// decided anywhere but beside the words that render it is how a table comes to say `pass` while
/// the process exits on a breach.
pub(super) mod gate;
/// THE ROWS THEMSELVES — the one verb of this plane whose product is a PRICE rather than a fact
/// about a store. A module of its own for [`gate`]'s reason and for one of its own: §8.2's two
/// rules (a window is REQUIRED, a row ceiling is REPORTED rather than silently applied) are pure
/// arithmetic over plain numbers, and a cost guard whose thresholds live anywhere but beside the
/// words that disclose them is how an answer comes to be cut without saying so.
pub(super) mod get;
/// A VENDOR ARCHIVE, READ ON THE DATAHUB'S BOX — `import`'s plan, month cut, progress, summary and
/// `--json` document. A module of its own for [`get`]'s reason: every line is a pure function over
/// the wire's own plan and outcome, so the wording and the month arithmetic are tested without a
/// socket, and `execute_import` is a dial, the requests and the prints.
pub(super) mod import;
mod import_exec;
pub(super) mod list;
pub(super) mod parse;
pub(super) mod refuse;
mod repair;
pub(super) mod rm;
pub(super) mod tape_health;
/// Point-in-time MEMBERSHIP: what the store CONTAINED over a window. Split out for
/// [`tape_health`]'s reason, and for one of its own — its verdicts are arithmetic over two
/// timestamps and a frame, which is exactly the shape that has to be testable without a store.
pub(super) mod universe;

// The per-subcommand half of `parse` (code-layout phase 2, task 11): one module per `hist` verb,
// each holding the arm of the second `match` in `parse` that used to hold its grammar. `parse`
// keeps the verb and flag loop and every refusal the verbs share, and calls these once the verb is
// known; their bodies moved verbatim, so each parameter is a local `parse` held under that name.
mod parse_cancel;
mod parse_coverage;
mod parse_export;
mod parse_fetch;
mod parse_gaps;
mod parse_gate;
mod parse_get;
mod parse_health;
mod parse_import;
mod parse_list;
mod parse_repair;
mod parse_rm;
mod parse_running;
mod parse_universe;

/// Which subcommand ran.
///
/// Adding one is FIVE edits, and they are listed because the last two are the ones a compiler does
/// not ask for: an arm here, a row in [`SUBCOMMANDS`], an arm in [`Sub::as_str`], an arm in
/// [`execute`] — and a row in [`USAGE`], which nothing forces and which
/// `the_usage_names_every_subcommand_and_flag_this_parser_accepts` is the only thing standing
/// between an operator and a verb they cannot discover. A READ subcommand owes three answers as
/// well: [`Sub::is_read`], [`Sub::refuses_a_window`] and [`Sub::takes_a_spec`].
///
/// ⚠ The two halves the module doc opens with are this enum's two halves: [`Sub::is_read`] is the
/// split, and it is what decides which flags a given line may carry. Read it as the answer to
/// "does this subcommand talk to a server or spawn a child", because that is the only question the
/// flag refusals below ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sub {
    /// `fetch SPEC` — real bars, over a window. Public market data for most venues; a venue whose
    /// history needs a credential is marked by `data source show`, which is where its channels are.
    Fetch,
    /// `running` — every `fetch` the datahub is serving right now, from its registry
    /// (`Request::ListBackfills`, an Observe-scope verb). [`backfills`]' module doc carries what the
    /// rows mean.
    ///
    /// ⚠ **Neither half's, and so NOT [`Sub::is_read`]**, though it only asks a datahub. That
    /// predicate decides which GENERIC refusals a verb meets, and every one of them is worded for a
    /// verb over the STORE — a spec is refused with "narrow the listing with --kind/--venue/--name",
    /// a window with "this verb folds each series' whole recorded span". Neither is true here: this
    /// verb reads no store and takes no filter. So it meets its own refusals instead — [`parse`]'s
    /// running-door arm — each saying what IS true.
    Running,
    /// `cancel SPEC` — stop every running `fetch` on one series at its next chunk boundary
    /// (`Request::CancelBackfill`, a Control-scope verb a key-less loopback datahub serves as it
    /// serves `Backfill` — `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`).
    /// The spec is `fetch`'s own, three parts, matched EXACTLY on the far side.
    ///
    /// ⚠ No typed confirmation, unlike [`Sub::Rm`]: nothing is removed — every chunk stored before
    /// the boundary stays, and repeating the fetch resumes (the design's §4). Not [`Sub::is_read`],
    /// for [`Sub::Running`]'s reason.
    Cancel,
    /// `import FORMAT DATASET` — read ONE dataset of a vendor archive from the DATAHUB's own imports
    /// directory into its store (`Request::ImportArchive`, a Control-scope verb a key-less loopback
    /// datahub serves as it serves `Backfill` —
    /// `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`).
    /// [`import`]'s module doc carries the flow.
    ///
    /// ⚠ **Neither half's, and so NOT [`Sub::is_read`]**: it writes, through a datahub, from files
    /// on THAT box — so it takes `--addr` and refuses every flag naming a local store, in [`parse`]'s
    /// import arm, each with a reason true of it. Its two positionals are a FORMAT and a DATASET
    /// rather than a spec, which is why it is not [`Sub::takes_a_spec`] either.
    Import,
    /// `export SPEC --out FILE` — one series OUT of the store, as a standalone Parquet file.
    Export,
    /// `get SPEC (--days N | --from/--to) [--limit N]` — BOUNDED ROWS TO STDOUT, the surface
    /// design's §8.2. The only verb of this plane whose answer is a PRICE: every sibling here
    /// renders a fact ABOUT a store — its catalog, its holes, its readiness — and until this one
    /// there was no way to get rows out of a REMOTE store at all, in any quantity or format.
    ///
    /// ⚠ It is a READ verb that REQUIRES a window, which makes it the one exception to both halves
    /// of [`Sub::refuses_a_window`]'s rule — see that predicate. [`get`]'s module doc carries §8.2's
    /// two rules, the residual between them (the window bounds the WIRE, the limit bounds STDOUT),
    /// and why this verb reads bars and takes no `--kind`.
    Get,
    /// `list` — every stored series with its coverage, from a datahub's `inventory()`.
    List,
    /// `gaps` — the same enumeration as `ls`, answering the OTHER question: what is MISSING inside
    /// each matched series' recorded span. One `inventory()` round trip, then one `series_gaps`
    /// probe per MATCHED series.
    ///
    /// ⚠ **This was a FLAG on `ls` and the promotion keeps the whole of the argument that made it
    /// one.** That argument was never "gaps is a lesser question" — it was that a gap query names
    /// ONE series, a series is four dimensions with a grouped/per-symbol alternative inside them,
    /// and a verb that built that identity out of flags would have to GUESS whether the operator
    /// meant a symbol or a group and hand the server an id matching nothing when it guessed wrong.
    /// That cost is still refused: this verb takes the same SUBSTRING FILTERS `ls` takes, the ids
    /// come back from `inventory()`, and the ones the filter selected are handed straight back to
    /// `series_gaps` — so this side still never CONSTRUCTS a `SeriesId`. What changed is only where
    /// the question is spelled, which the surface design's §7 tree rules: a verb per question.
    ///
    /// ⚠ **It renders every matched series, clean ones included.** Dropping the clean rows would
    /// make an empty output mean two different things — "nothing matched your filter" and
    /// "everything you matched is whole" — and those are opposite answers. [`empty_note`] exists
    /// one level up for exactly that reason.
    Gaps,
    /// `coverage` — the cross-kind report, from a datahub's `coverage_report()`.
    Coverage,
    /// `tape-health` — the series whose own catalog CONTRADICTS itself, from the same
    /// `inventory()` the listing reads. The inverse question from every sibling: not what is
    /// missing, but whether what is here is even possible. [`tape_health`]'s module doc carries the
    /// argument, and the declared bound — it is not a ROW scan, and why it cannot be one here.
    TapeHealth,
    /// `universe` — point-in-time MEMBERSHIP over a window, the survivorship-bias defence. Also
    /// one `inventory()` round trip: every instrument's first and last RECORDED row, judged against
    /// the store's own span. [`universe`]'s module doc carries what that evidence is and — more
    /// importantly — what it is not.
    Universe,
    /// `gate SPEC --require-days N …` — DATA READINESS AS AN EXIT CODE, the surface design's §8.4.
    /// The only verb of this group whose PRODUCT is the rung rather than the output, and the only
    /// READ verb that takes a spec — a SELECTOR over what `inventory()` returned, never an
    /// identity this side built. [`gate`]'s module doc carries every criterion and the two
    /// measurements that bound them.
    Gate,
    /// `rm` — DELETE series, irreversibly. The one subcommand that reaches EITHER store: `--addr`
    /// is the datahub route, its absence the engine route. See [`Sub::is_read`].
    Rm,
    /// `repair` — rebuild ONE series' manifest from its parts, the repair
    /// `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s `read_manifest` names in its own error
    /// text. ENGINE-ONLY: `--addr` is refused by name, and [`refuse_the_remote_route_on_repair`]
    /// carries the three-part argument for why the datahub serves no such verb.
    ///
    /// It shares `rm`'s SELECTOR flags (`--kind`/`--venue`/`--symbol`/`--group`/`--interval`) and
    /// its `--dry-run`/`--yes` pair, and it is deliberately not a sibling of `rm` in anything else:
    /// it wildcards NOTHING, it rehearses by DEFAULT, and it has no `--produced-by` because it
    /// asserts no provenance — a rebuild reads what is on disk and touches no row.
    Repair,
}

/// The FLAT spellings that shipped before the group split, and what each one is now.
///
/// ⚠ Every verb of this plane used to sit directly under `data`. The surface design's §7 puts them
/// in groups, which is a BREAKING change to a shipped CLI — so each old spelling is refused BY NAME
/// with its replacement rather than removed. A deprecation that keeps working is one nobody
/// migrates off; a deprecation that fails without naming its replacement is one that costs a
/// support round trip.
///
/// ⚠ Two of these are RENAMES as well as moves — `list` -> `ls` and `tape-health` -> `health` — and
/// `data hist list` is refused separately with its own message, because a reader who typed the
/// group correctly and the verb by its old name should not be handed the whole roster to diff.
///
/// ⚠ `fetch-starter` and `seed-demo` point at `fetch --source starter`/`--source demo`, NOT at
/// themselves under `hist`. This paragraph said the opposite — that the `--source` axis "does not
/// exist yet" — while the rows below already pointed at it; the rows were right and the prose was
/// not. The rule it stated survives unchanged: a refusal that
/// names a spelling which then errors is worse than the one it replaced, which is why
/// `data hist seed-demo` is refused by [`parse`]'s verb match with the same `--source` answer.
const RETIRED_SPELLINGS: &[(&str, &str)] = &[
    ("fetch", "data hist fetch"),
    ("fetch-starter", "data hist fetch --source starter"),
    ("seed-demo", "data hist fetch --source demo"),
    ("export", "data hist export"),
    ("list", "data hist ls"),
    ("coverage", "data hist coverage"),
    ("tape-health", "data hist health"),
    ("universe", "data hist universe"),
    ("rm", "data hist rm"),
    ("repair", "data hist repair"),
];

const SUBCOMMANDS: &[Sub] = &[
    Sub::Fetch,
    Sub::Running,
    Sub::Cancel,
    Sub::Import,
    Sub::Export,
    Sub::Get,
    Sub::List,
    Sub::Gaps,
    Sub::Coverage,
    Sub::TapeHealth,
    Sub::Universe,
    Sub::Gate,
    Sub::Rm,
    Sub::Repair,
];

impl Sub {
    /// The name the operator typed, which is also what every refusal message names it by.
    fn as_str(self) -> &'static str {
        match self {
            Sub::Fetch => "fetch",
            Sub::Running => "running",
            Sub::Cancel => "cancel",
            Sub::Import => "import",
            Sub::Export => "export",
            Sub::Get => "get",
            Sub::List => "ls",
            Sub::Gaps => "gaps",
            Sub::Coverage => "coverage",
            Sub::TapeHealth => "health",
            Sub::Universe => "universe",
            Sub::Gate => "gate",
            Sub::Rm => "rm",
            Sub::Repair => "repair",
        }
    }

    /// `true` for the subcommands that ONLY ask a datahub, `false` for the ones that spawn the
    /// engine.
    ///
    /// ⚠ `rm` is `false` here and is NOT purely an engine verb: this predicate answers "may this
    /// subcommand carry a store-side flag", which for `rm` is yes, and the `--addr`-vs-`--store`
    /// contradiction is checked separately in [`parse`]. Widening this to a three-way enum was
    /// tried and abandoned — every existing caller asks the binary question, and a third state
    /// would have made two of them silently wrong for the new arm.
    fn is_read(self) -> bool {
        matches!(
            self,
            Sub::Get
                | Sub::List
                | Sub::Gaps
                | Sub::Coverage
                | Sub::TapeHealth
                | Sub::Universe
                | Sub::Gate
        )
    }

    /// `true` for the read subcommands whose answer is a WHOLE-SERIES fold of a manifest, and which
    /// therefore refuse a time bound.
    ///
    /// ⚠ This predicate exists because [`Sub::Universe`] broke the rule the read half used to hold
    /// unanimously. `ls`, `gaps`, `coverage` and `health` each answer about a series' entire
    /// recorded span — `--from`/`--to` could only narrow the RENDERING, never the question, so a
    /// bound that appeared to have worked would be the worst of both. `universe`'s question IS a
    /// window ("what was in it between these dates"), so the same two flags are load-bearing there.
    /// One predicate rather than an inline `matches!` at the refusal site, so a future read verb
    /// has to answer this question rather than inherit whichever side it was written next to.
    ///
    /// ⚠ [`Sub::Gate`] is on the REFUSING side, which is worth stating because it takes a spec and
    /// therefore looks like the window-shaped verbs: it judges each selected series' whole
    /// recorded span, so a bound could only narrow what was rendered, never what was asserted —
    /// and a gate whose subject a flag had quietly narrowed is a green over less than it claims.
    ///
    /// ⚠ **[`Sub::Get`] is the one read verb that REQUIRES a window rather than merely accepting
    /// one**, which is why this predicate could not have been widened into "does a bound mean
    /// anything here". `universe` takes a window and defaults to the store's own span; `get`
    /// REFUSES the line that names none, because §8.2 makes the bound the cost guard rather than a
    /// narrowing. The requirement therefore lives in [`get::parse_window`], where the message can
    /// say what it is for, and this predicate only has to keep `get` off the refusing side.
    fn refuses_a_window(self) -> bool {
        matches!(self, Sub::List | Sub::Gaps | Sub::Coverage | Sub::TapeHealth | Sub::Gate)
    }

    /// `true` for the subcommands that take the POSITIONAL spec, whichever half they belong to.
    ///
    /// ⚠ This predicate exists because [`Sub::Gate`] broke the rule the read half used to hold
    /// unanimously — *a stored series is four dimensions, which no colon-string can spell, so no
    /// read verb takes one*. That argument is about naming an IDENTITY, and it survives: `gate`'s
    /// spec SELECTS among the ids `inventory()` returned and builds none, which is exactly what
    /// `gaps` does with its substring filters one degree looser. So the refusal sites ask this
    /// question rather than `is_read`, and a future verb has to answer it rather than inherit
    /// whichever side it was written next to. Two grammars sit behind it — [`check_spec`]'s three
    /// mandatory parts for `fetch`/`export`/`get`, [`gate::parse_spec`]'s §7.1 selector — and each
    /// says at its own site why one parser could not serve both. ⚠ `get` is on the THREE-PART side
    /// though it is a read verb, and [`get::parse_spec`] argues why: its spec is an ADDRESS handed
    /// to `load_bars_ms`, not a selector over an enumeration.
    ///
    /// ⚠ [`Sub::Cancel`] takes `fetch`'s THREE-PART spec ([`check_spec`]) because it names what a
    /// `fetch` was asked for, which the datahub matches exactly — an address, like `get`'s, never a
    /// selector.
    fn takes_a_spec(self) -> bool {
        matches!(self, Sub::Fetch | Sub::Cancel | Sub::Export | Sub::Get | Sub::Gate)
    }
}

/// The client-side row filter the read verbs apply to what the server already sent.
///
/// Case-insensitive SUBSTRING on each dimension, ANDed, and an absent field matches everything —
/// see the module doc for why this is a browse aid rather than a validated roster lookup. Nothing
/// here reaches the wire: both RPCs answer with the whole catalog and the filter is applied to the
/// answer, so a filter can never make the server do less work (which is exactly why `gaps` is the
/// verb worth pairing one with — THAT does).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Filter {
    /// `--kind` — matched against `SeriesId::kind`. `coverage` refuses it (see [`parse`]).
    kind: Option<String>,
    /// `--venue` — matched against the venue slug.
    venue: Option<String>,
    /// `--name` — matched against the series LABEL: the symbol of a per-symbol series, the group of
    /// a grouped one. Never against the raw `symbol`, which is EMPTY for every grouped series.
    name: Option<String>,
}

impl Filter {
    /// `true` when every named dimension matches. `kind` is passed as `None` by the `coverage`
    /// path, whose rows are the join ACROSS kinds and so have no single kind to test.
    fn matches(&self, kind: Option<&str>, venue: &str, name: &str) -> bool {
        fn contains(needle: &Option<String>, haystack: &str) -> bool {
            match needle {
                None => true,
                Some(n) => haystack.to_ascii_lowercase().contains(&n.to_ascii_lowercase()),
            }
        }
        // A row with NO kind dimension passes the kind test unconditionally. That is not a
        // silently-dropped filter: `coverage` is the only caller that passes `None`, and `parse`
        // refuses `--kind` there outright — so a discarded needle cannot reach this arm.
        let kind_ok = match kind {
            None => true,
            Some(k) => contains(&self.kind, k),
        };
        kind_ok && contains(&self.venue, venue) && contains(&self.name, name)
    }

    /// `true` when nothing was filtered — used only to decide whether an EMPTY result reads as
    /// "this store holds nothing" or "your filter matched nothing", which are different problems.
    fn is_empty(&self) -> bool {
        self.kind.is_none() && self.venue.is_none() && self.name.is_none()
    }
}

/// The window a `fetch` covers. Exactly one form, chosen by the operator; there is no default,
/// because "fetch everything" is not a thing any venue serves and a silent default would decide how
/// much of somebody's rate limit to spend.
#[derive(Debug, PartialEq, Eq)]
enum Window {
    /// `--days N`, counting back from now.
    Days(String),
    /// `--from LABEL --to LABEL`, an explicit range.
    Range { from: String, to: String },
}

/// `export`'s bounds — BOTH OPTIONAL, BOTH INDEPENDENT, and that is the one place this verb's
/// grammar deliberately diverges from `fetch`'s [`Window`].
///
/// ⚠ **The divergence is about what the flag decides, not about tidiness.** [`window_from`] refuses
/// `--from` without `--to` and refuses the neither-form, because a FETCH with no window would
/// decide how much of somebody's venue rate limit to spend — there is no meaningful default. An
/// EXPORT bounds a slice that is already on disk: every one of the four combinations is a
/// well-formed request, "the whole series" included, and refusing three of them would be a rule
/// with nothing behind it. So `export` does not go through `window_from` at all, and `--days` — a
/// count back from NOW, which bounds a fetch and says nothing about what a store holds — is
/// refused on it BY NAME rather than quietly accepted into a shape it cannot fill.
#[derive(Debug, Default, PartialEq, Eq)]
struct ExportRange {
    /// `--from LABEL` — epoch-ms or `YYYY-MM-DDTHH`, parsed by the ENGINE (one timestamp parser in
    /// the workspace; see this module's doc on what is validated here and what is not).
    from: Option<String>,
    /// `--to LABEL`, same spellings, and it needs no `from`.
    to: Option<String>,
}

/// The parsed `data` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Args {
    sub: Sub,
    /// WHERE `fetch` gets its rows — the §6 coordinate. `Source::Venue` unless `--source` named
    /// otherwise, and meaningless for every other subcommand (the parser refuses the flag there).
    source: Source,
    /// `VENUE:SYMBOL:INTERVAL`, shape-checked. `None` for every subcommand but `fetch`/`export`.
    ///
    /// ⚠ `gate` takes a positional spec too and does NOT leave it here: it is PARSED into
    /// [`GateArgs::spec`], the way `rm` moves `--kind`/`--venue` out of [`Filter`]. One home per
    /// fact, and a raw string beside a parsed one is how the two come to disagree about a trimmed
    /// part or a stripped `@`.
    spec: Option<String>,
    /// `Some` only for `fetch` (the parser refuses one without, and refuses one anywhere else).
    ///
    /// ⚠ `export` carries NO `Window`, and that is deliberate rather than an omission — see
    /// [`ExportRange`], which is the shape a bound-what-is-already-on-disk range needs.
    window: Option<Window>,
    /// `export`'s two INDEPENDENT bounds. `None` for every other subcommand.
    export_range: Option<ExportRange>,
    /// `universe`'s membership window, ALREADY PARSED to epoch-ms. `None` for every other
    /// subcommand.
    ///
    /// ⚠ A SECOND range field rather than a reuse of [`ExportRange`], and the divergence is the
    /// whole reason: that one carries STRINGS because the engine parses them, while nothing here is
    /// forwarded — the comparison runs in this process, so this side must parse and must refuse an
    /// unreadable bound as a usage error. Sharing one field would have meant one of the two
    /// subcommands holding a shape it cannot use.
    universe_window: Option<universe::MembershipWindow>,
    /// `--out FILE` — `export`'s destination. Required there, refused everywhere else.
    out: Option<String>,
    store: Option<String>,
    engine: Option<String>,
    /// `--addr`, resolved to [`DEFAULT_ADDR`] when absent — and resolved for EVERY subcommand, not
    /// just the read ones. The write half never looks at it (the parser has already refused an
    /// explicit `--addr` there), and carrying one unconditional field is cheaper than a second
    /// `Option` whose `None` would mean two different things.
    addr: String,
    /// The read verbs' client-side row filter. Always present; a defaulted [`Filter`] matches
    /// everything.
    filter: Filter,
    /// Ask `series_gaps` for each MATCHED series after the inventory lands. DERIVED from the verb
    /// — `true` for [`Sub::Gaps`] and false for every other — never from a flag.
    gaps: bool,
    /// `--class`: on `ls`, ask `properties_as_of` for each MATCHED series' instrument — once per
    /// distinct `(venue, symbol)`, see [`execute_list`] — and render the recorded asset class.
    class: bool,
    /// `--partial-only`: on `coverage`, keep only instruments with at least one partial day.
    partial_only: bool,
    /// `--json`: emit this subcommand's document instead of the human rendering. Applies to EVERY
    /// subcommand in [`SUBCOMMANDS`] — the write half because "what was written and where" is the
    /// question a caller driving it has, the read half because the answer IS data and a table is
    /// the lossy form of it, and `rm` because its plan carries the one fact a human reads and a
    /// machine must be able to compare: the store that answered. (⚠ This said "ALL FIVE" and was
    /// two short within a release; the roster is the authority, not a number written here.)
    json: bool,
    /// `rm`'s selector. `None` for every other subcommand — the parser builds it only where it
    /// means something, so no other arm can read a half-filled one.
    rm: Option<RmArgs>,
    /// `repair`'s selector. `None` everywhere else, for [`Args::rm`]'s reason. A SECOND struct
    /// rather than a shared one: the two overlap in five fields and differ in the two that matter
    /// — `repair` has no `produced_by` (it asserts nothing) and cannot wildcard (its `symbol`/
    /// `group` alternative is REQUIRED), and a shared type would make both of those representable.
    repair: Option<RepairArgs>,
    /// `gate`'s spec and its criteria. `None` everywhere else, for [`Args::rm`]'s reason: no other
    /// arm can then read a half-filled one, and no other arm can be handed a `require_days` that
    /// defaulted to something.
    gate: Option<GateArgs>,
    /// `get`'s spec, window, ceiling and rendering. `None` everywhere else, for [`Args::gate`]'s
    /// reason — and one more that is this verb's own: [`Args::json`] is a TWO-state axis and `get`
    /// has THREE renderings, so a `jsonl` run would be indistinguishable from a `table` one if the
    /// answer lived there. [`GetArgs::render`] is the authority for this verb and `json` is its
    /// projection, computed once in [`parse`].
    get: Option<GetArgs>,
    /// `export`'s REMOTE route — its kind, spec, file format, bounds and walk step, all resolved.
    ///
    /// ⚠ **`Some` is what SELECTS the route**, which is why this field exists rather than a `bool`
    /// beside [`Args::export_range`]: the two routes take different grammars (`--kind`/`--window`/
    /// `--format` belong to one, `--engine` to the other, and only the remote one
    /// requires BOTH bounds), and a single struct holding both would be one a future arm could
    /// read a half-filled version of — [`Args::rm`]'s reason. [`Args::export_range`] stays the
    /// ENGINE route's, carrying strings because the engine parses them.
    /// ⚠ **The resolved request lives in [`export::Plan`] rather than in a sibling of [`GateArgs`]
    /// here**, which is the one place this verb diverges from the file's habit. That module's
    /// renderers each need four or five of its fields at once, and passing them individually put
    /// its `--json` document at nine parameters — past `clippy::too_many_arguments`, a merge gate.
    /// A type the pure half owns is also what lets the walk and both renderings be tested without
    /// building an [`Args`].
    export: Option<export::Plan>,
    /// `import`'s format, dataset, days, bars and confirmation flags. `None` everywhere else, for
    /// [`Args::gate`]'s reason.
    import: Option<ImportArgs>,
    /// `--addr` was given EXPLICITLY. `addr` above is always resolved, so it cannot answer "did the
    /// operator ask for the remote route" — which is the question `rm` and now `export` turn on.
    addr_given: bool,
}

/// `rm`'s own arguments — the selector, the assertion, and the two confirmation flags.
///
/// A struct of its own rather than five more `Option`s on [`Args`], because every field here is
/// meaningless for the other four subcommands and an `Args` that could hold a selector for
/// `seed-demo` is an `Args` some future arm will read one from.
#[derive(Debug, PartialEq, Eq)]
struct RmArgs {
    /// The four identity dimensions. Held as the store's own type so this side never re-implements
    /// what a series IS — ⚠ and it is reached through a DEV-dependency (see the module doc's note
    /// on `SeriesRow`), so it may be NAMED only under `#[cfg(test)]`. Hence the plain fields.
    kind: String,
    venue: String,
    symbol: Option<String>,
    group: Option<String>,
    interval: Option<String>,
    /// `--produced-by`, verbatim as typed.
    ///
    /// ⚠ **A producer PATH resolves to its prefix on BOTH far sides since 2026-09-11, and this CLI
    /// still refuses to SEND one.** This doc said "on the far side" unqualified, which was true of
    /// one of the two: `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` called
    /// `vike_data::store::store_kind::resolve_produced_by` and
    /// `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` called nothing of the kind. The
    /// server now calls it too — through the `vike_datahub_client::proto` re-export, so one
    /// definition answers both ends — but a DEPLOYED datahub may predate that and the protocol
    /// carries no capability string to tell the two apart. See
    /// [`refuse_a_producer_path_on_the_remote_route`], which is now a compatibility guard and
    /// carries what the unresolved spelling used to report instead.
    produced_by: Option<String>,
    dry_run: bool,
    yes: bool,
}

/// `repair`'s own arguments — the identity of ONE series, and the two confirmation flags.
///
/// ⚠ **Every dimension but `interval` is REQUIRED, and that is the whole difference from
/// [`RmArgs`].** `rm` wildcards an omitted dimension because a cleanup is a set; `repair` rebuilds
/// exactly one series' index, so an omitted `--symbol`/`--group` names nothing and is refused. The
/// residual `Option`s here are therefore the store's own ALTERNATIVE (`symbol` xor `group`) and
/// bars' `interval=` segment, never a wildcard.
#[derive(Debug, PartialEq, Eq)]
struct RepairArgs {
    kind: String,
    venue: String,
    /// Exactly one of these two is `Some` — [`parse`] refuses both and refuses neither.
    symbol: Option<String>,
    group: Option<String>,
    /// `Some` for a bar series. Its REQUIREMENT on `bar` is the engine's call, not this crate's —
    /// see [`parse_repair::parse`], the `Sub::Repair` arm of [`parse`].
    interval: Option<String>,
    /// `--dry-run`: the DEFAULT spelled explicitly, and it WINS over `--yes`.
    dry_run: bool,
    /// `--yes`: perform the rebuild. Without it this verb rehearses — see [`Sub::Repair`].
    yes: bool,
}

/// `gate`'s own arguments — what the gate is ABOUT, and what it ASSERTS.
///
/// A struct of its own for [`RmArgs`]'s reason, and one more that matters here: every field below
/// is already RESOLVED. `require_days` is a number rather than the string an operator typed,
/// `max_gap_ms` is milliseconds rather than `"4h"`, and `kinds` is never empty because [`parse`]
/// folds in [`gate::DEFAULT_KIND`] when nobody named one. So the judging in [`gate`] is a pure fold
/// over plain data with no parse left in it, and no site downstream can re-decide a default.
#[derive(Debug, PartialEq, Eq)]
struct GateArgs {
    /// The SELECTOR the positional spec parsed to — never a `vike_data::SeriesId`; see
    /// [`gate::Spec`].
    spec: gate::Spec,
    /// `--require-days N`, a whole positive count. REQUIRED, because a gate with no criterion
    /// exits 0 having checked nothing.
    require_days: i64,
    /// `--max-gap D` in milliseconds, or `None` when the holes were not asked about — which the
    /// verdict DISCLOSES rather than leaving to be noticed.
    max_gap_ms: Option<i64>,
    /// `--require-kind K`, repeatable, defaulted to `[bar]`. Never empty: it is what the gate is
    /// about, and an empty one would judge nothing while reading like a gate.
    kinds: Vec<String>,
}

/// `get`'s own arguments — WHICH series, over WHAT window, at most HOW MANY rows, rendered HOW.
///
/// A struct of its own for [`GateArgs`]'s reason, and RESOLVED for its reason too: the spec is
/// parsed, the bounds are epoch-ms, the ceiling is folded in and the rendering is decided. So
/// [`execute_get`] is a read and a render with no parse left in it, and no site downstream can
/// re-decide a default.
///
/// ⚠ **The one thing deliberately NOT resolved here is the CLOCK.** `--days N` stays a count
/// because [`parse`] is PURE — it reads no environment and no clock — so
/// [`get::Window::bounds`] takes the instant as a parameter and `execute_get` supplies it. That is
/// the same split `fetch` makes: its `Window` is carried raw and `fetch_window_ms` resolves it at
/// execute time.
#[derive(Debug, PartialEq, Eq)]
struct GetArgs {
    /// The ADDRESS of one bar series — `VENUE:SYMBOL:INTERVAL`, all three parts. Not a selector
    /// over an enumeration the way [`GateArgs::spec`] is: it goes straight to `load_bars_ms`.
    spec: get::Spec,
    /// The REQUIRED bound, in whichever of the two forms was typed. §8.2's first rule.
    window: get::Window,
    /// The effective row ceiling — `--limit` when it lowered [`get::ROW_CEILING`], that constant
    /// otherwise. Never above it: [`get::parse_limit`] refuses rather than clamping.
    limit: usize,
    /// `true` when the ceiling was DEFAULTED rather than named, so the disclosure can say which
    /// one cut the answer. A message that said "--limit 1000" to somebody who typed no `--limit`
    /// would be telling them about a flag they did not use.
    limit_defaulted: bool,
    /// `table` | `json` | `jsonl` — this verb's three, and the authority for how it renders. See
    /// [`Args::get`] for why [`Args::json`] cannot be that authority here.
    render: get::Render,
}

/// `import`'s own arguments — WHICH dataset of WHICH format, over WHICH days, deriving WHICH bars,
/// and how far to go. A struct of its own for [`GateArgs`]'s reason, and RESOLVED for it too: the
/// dataset has met the wire's shared validator, the days are epoch-ms UTC midnights, the bars are
/// the list the request carries — so [`execute_import`] has no parse left in it.
#[derive(Debug, PartialEq, Eq)]
struct ImportArgs {
    /// The archive FORMAT — a registry id the datahub advertises as `import_format=<id>`. Matched
    /// exactly by the client against that advertisement, never against a list on this side.
    format: String,
    /// ONE dataset directory NAME under the datahub's `imports/<format>/` — held to
    /// `vike_datahub_client::archive::validate_import_dataset` at parse time, the same function the
    /// client and the server's door call.
    dataset: String,
    /// `--from`, the first day, INCLUSIVE; `None` is the dataset's own first day.
    from_day: Option<i64>,
    /// `--to`, the last day, INCLUSIVE; `None` is the dataset's own last day.
    to_day: Option<i64>,
    /// The bar intervals derived per imported day — [`import::DEFAULT_BARS`] when `--bars` was not
    /// given, empty under `--bars none`.
    bars: Vec<String>,
    /// `--dry-run`, `--verify` — [`import::Mode::of`] turns the pair into what the run does.
    dry_run: bool,
    verify: bool,
    /// `--yes`: confirm without asking. Loses to `--dry-run`, as on `rm`.
    yes: bool,
}

/// One series' cheap coverage, flattened out of the wire type on arrival — see the module doc for
/// why the read half carries its own row types.
///
/// `bytes`/`parts`/`dates` are ON-DISK facts (summed part-file size, part count, `date=` partition
/// count), so a store that has no files answers 0 for all three while still reporting real rows.
/// They are carried into [`list_json`] and left out of the human table, which has room for the
/// three numbers a person browsing actually asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Coverage {
    first_ts: i64,
    last_ts: i64,
    rows: u64,
    bytes: u64,
    parts: usize,
    dates: usize,
}

/// What the store's `kind=properties` tape says about ONE instrument's asset class — the READER
/// `vike_model::SymbolProperties::asset_class` did not have until this flag.
///
/// ⚠ **Five outcomes, and every one of them is SAID rather than left to an empty cell.** The field
/// is an `Option` in the model precisely so that "the venue told us" and "nobody said" stay
/// distinguishable (that field's doc argues it at length), and a renderer that folded both into a
/// blank would undo the distinction on the way to the terminal — which is the same defect
/// `vike_data::KindDays::absent` names for a kind that was never recorded. Here the absence splits
/// THREE ways, not two, and the three call for different actions: fix the producer, run the
/// recorder, or nothing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassProbe {
    /// A properties row exists and NAMES a class. Carried as the variant's own
    /// `vike_model::AssetClass::sql_word`, which is also its serde word — never a second spelling
    /// minted here.
    Classified(&'static str),
    /// A properties row exists and its `asset_class` is `None`. **This is the wiring signal**: the
    /// venue producer recorded an instrument grid and named no class for it.
    Unclassified,
    /// No properties row at all for this `(venue, symbol)` — nothing has ever recorded a grid for
    /// this instrument, which is a RECORDER gap rather than a producer one.
    Unrecorded,
    /// A GROUPED series: its name is a group, not a symbol, so no lookup was made. Asking
    /// `properties_as_of(venue, group)` would answer `None` for a question that was malformed, and
    /// that `None` is indistinguishable from [`ClassProbe::Unrecorded`] — a wrong answer dressed as
    /// a real one, plus a round trip spent to get it.
    Grouped,
    /// The probe failed for this instrument. Degrades the ROW, never the run — see [`execute_list`].
    Failed(String),
}

impl ClassProbe {
    /// The probe's verdict as the `--json` document spells it. A machine reader gets WHICH answer
    /// this was, so an absent class can never be read as a present-but-empty one.
    fn status(&self) -> &'static str {
        match self {
            ClassProbe::Classified(_) => "classified",
            ClassProbe::Unclassified => "unclassified",
            ClassProbe::Unrecorded => "unrecorded",
            ClassProbe::Grouped => "grouped",
            ClassProbe::Failed(_) => "error",
        }
    }

    /// The class word itself — `Some` for exactly one variant, which is what makes
    /// `asset_class != null` mean `status == "classified"` and nothing else.
    fn word(&self) -> Option<&'static str> {
        match self {
            ClassProbe::Classified(w) => Some(w),
            _ => None,
        }
    }
}

/// One row of `ls` or `gaps`: a series' identity, its coverage, and — only under `gaps` — its holes.
///
/// ⚠ `name`/`grouped` are DERIVED from `symbol`/`group` at the one conversion site, and all four
/// are kept. The pair is what the renderers use (exactly one of symbol/group is meaningful, and
/// resolving that once beats resolving it at every render site); the raw fields are what
/// [`list_json`] emits, so a machine reader gets the identity rather than this side's reading of
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SeriesRow {
    kind: String,
    venue: String,
    /// The symbol for a per-symbol series, the group for a grouped one — `SeriesId::label()`.
    name: String,
    /// `true` when this series is a `group=` directory holding many symbols in one part.
    grouped: bool,
    /// The RAW symbol: EMPTY for every grouped series, which is why it is not what `name` reads.
    symbol: String,
    /// The RAW group: `Some` exactly when `grouped`.
    group: Option<String>,
    /// `Some` for bars (which sub-partition by bar step), `None` for every tick-shaped kind.
    pub(super) interval: Option<String>,
    coverage: Coverage,
    /// Inclusive epoch-ms ranges MISSING inside the recorded span. `None` means the question was
    /// not asked (this is `ls`) or could not be answered — `gaps_error` tells those apart, and an
    /// EMPTY `Some` is the real "this series has no holes".
    gaps: Option<Vec<(i64, i64)>>,
    /// Why this one series' gap probe failed, when it did. See [`execute_list`] for the degrade.
    gaps_error: Option<String>,
    /// The recorded asset class for this row's INSTRUMENT. `None` means the question was not asked
    /// (no `--class`) — every way of it having been asked and answered, failure included, is a
    /// [`ClassProbe`] variant, so this side needs no sibling `class_error` field the way `gaps`
    /// does.
    class: Option<ClassProbe>,
}

/// One kind's presence for one instrument in the cross-kind report: the kind, and how many UTC days
/// of it are on disk. Only kinds the instrument ACTUALLY records appear — "never recorded" is a
/// different fact from "recorded with holes", and the wire type says so itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KindRow {
    pub(super) kind: String,
    pub(super) days: usize,
}

/// A day some of an instrument's recorded kinds cover and others do not — the report's whole point.
///
/// Both `day` (the UTC-day INDEX the wire carries) and `start_ms` (that day's epoch-ms midnight,
/// via the wire type's own converter) are kept: the index is what the store indexes by, and the ms
/// is what a date renders from. Deriving the second here rather than in the renderer keeps this
/// module free of a day-length constant it would otherwise have to spell for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PartialRow {
    day: i64,
    start_ms: i64,
    missing: Vec<String>,
}

/// One row of `coverage`: an instrument's kinds lined up, and the days on which they disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstrumentRow {
    venue: String,
    /// The symbol for a per-symbol instrument, the group for a grouped one — the same `label`
    /// rule [`SeriesRow::name`] follows, and the same reason.
    name: String,
    grouped: bool,
    pub(super) kinds: Vec<KindRow>,
    /// The union of days ANY kind has — this instrument's overall recorded span.
    spanned_days: usize,
    partial: Vec<PartialRow>,
}

/// Route the parsed line to whichever half it belongs to — see [`Sub::is_read`] and the module
/// doc's opening. Nothing is shared between the two arms but this dispatch and the exit ladder:
/// they reach different stores by different mechanisms, which is exactly the fact the flag
/// refusals in [`parse`] exist to keep visible.
///
/// ⚠ It returns a RUNG rather than `()`. Every arm but one answers [`Exit::Ok`], and the
/// conversion `.map(|()| Exit::Ok)` says so at each site rather than being hidden in a helper;
/// `gate` is the one verb whose answer is the number, and threading it through the shared type is
/// what keeps that number decided in the function that judged it rather than re-derived at this
/// boundary.
pub(super) fn execute(
    args: &Args,
    project_root: Option<&Path>,
    keys: Option<&NodeKeys>,
) -> CmdResult<Exit> {
    match args.sub {
        // The engine routes spawn a child and open no socket of THIS process's, so a datahub key
        // is not theirs to carry. ⚠ The export engine DOES dial a datahub since 2026-09-26
        // (decision 0084's amendment) — and resolves that hub's keys itself, out of the same
        // credential store, through `vike_datahub_client::route::open_routed_history`. Handing the
        // child this process's keys would be a second resolution of one fact, over argv or env.
        // ⚠ `Fetch` LEFT this arm. It is the one verb here that asks a DATAHUB rather than running
        // an engine against a local store — see [`execute_fetch`] for the whole argument.
        // ⚠ ONE verb, TWO ROUTES, chosen by whether `--addr` was TYPED — the second verb in this
        // plane to have that shape, after `rm`. [`Args::export`] is `Some` for exactly the remote
        // one, so the route is read off a field that could only have been built on that branch
        // rather than re-derived from a flag here.
        Sub::Export => match args.export.as_ref() {
            Some(e) => execute_export_remote(args, e, keys).map(|()| Exit::Ok),
            None => execute_engine(args, project_root).map(|()| Exit::Ok),
        },
        // ⚠ ONE verb, TWO transports, chosen by the SOURCE. §2 records that this plane had three
        // transport mechanisms and no rule predicting which verb used which; this line is where
        // that stops being true — the transport is a property of WHERE the rows come from.
        Sub::Fetch => match args.source {
            Source::Venue => execute_fetch(args, keys).map(|()| Exit::Ok),
            Source::Starter | Source::Demo => execute_engine(args, project_root).map(|()| Exit::Ok),
        },
        // The running-fetch door — a datahub's registry, and `Exit::Ok` for every answer it serves:
        // an empty list and a cancel that matched nothing are facts about the datahub, not failures
        // of the command (see [`execute_cancel`]).
        Sub::Running => execute_running(args, keys).map(|()| Exit::Ok),
        Sub::Cancel => execute_cancel(args, keys).map(|()| Exit::Ok),
        // A datahub's archive import — `Exit::Ok` unless a day was refused for what is in its file
        // or the directory could not be walked, which are run failures; see [`import`]'s module doc.
        Sub::Import => execute_import(args, keys).map(|()| Exit::Ok),
        // ⚠ ONE function for TWO verbs, and deliberately so: `gaps` IS `ls` with the probe armed,
        // and [`Args::gaps`] is already `true` here because [`parse`] derives it from the verb.
        // A second function would be the same enumeration, the same filter, the same account-kind
        // exclusion and the same disclosure, copied — which is how the two would come to disagree
        // about which series a filter matches.
        // The ROW verb. `Exit::Ok` even when the window held nothing: an empty answer is a fact
        // about the store rather than a failure of the command, the same call `execute_list` makes
        // for an empty listing — and [`get::empty_note`] is what stops it being read as "this
        // series does not exist".
        Sub::Get => execute_get(args, keys).map(|()| Exit::Ok),
        Sub::List | Sub::Gaps => execute_list(args, keys).map(|()| Exit::Ok),
        Sub::Coverage => execute_coverage(args, keys).map(|()| Exit::Ok),
        Sub::TapeHealth => execute_tape_health(args, keys).map(|()| Exit::Ok),
        Sub::Universe => execute_universe(args, keys).map(|()| Exit::Ok),
        // The ONE arm that decides its own rung — see [`execute_gate`].
        Sub::Gate => execute_gate(args, keys),
        Sub::Rm => execute_rm(args, project_root, keys).map(|()| Exit::Ok),
        // ENGINE-ONLY and carrying no datahub key: [`parse`] has already refused `--addr` here —
        // see [`refuse_the_remote_route_on_repair`].
        Sub::Repair => execute_repair(args, project_root).map(|()| Exit::Ok),
    }
}

// ─── `fetch`: the one verb that reaches ONLY a datahub ──────────────────────────────────────────

#[cfg(test)]
mod flag_tests;

#[cfg(test)]
mod tests;
