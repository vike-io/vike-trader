//! `vike-cli data source` — WHERE ROWS MAY COME FROM, the surface design's §7 fourth group.
//!
//! ⚠ **This group exists because `--source` is an AXIS and an axis needs a roster an operator can
//! read.** `crate::cmd::data`'s `Source` enum carries the three values that WORK and
//! `UNBUILT_SOURCES` the six that are designed and refused by name — between them they are the
//! whole answer to "what may I pass to `--source`", and until this group there was no way to see
//! it but to type a wrong value and read the refusal.
//!
//! # The two verbs
//!
//! | verb | reaches | answers |
//! |---|---|---|
//! | `ls` | NOTHING | every source, its state, and what it costs |
//! | `show NAME` | NOTHING | what that source holds, and what would fetch it if you used it — and, for a roster venue, how far back each door to its history goes |
//!
//! ⚠ **That third column used to read "what THIS box reaches" for `show`, and it was false.** The
//! `reaches` column had already been corrected from the stub's "local, plus a manifest read for
//! `vike`" to NOTHING; the `answers` column was carried over from the stub VERBATIM, where `show`
//! was going to perform that read. So the table promised a probe one column after denying it, and
//! [`USAGE`]'s own `show` row and [`LS_NOTES`]' third note repeated the promise twice more — while
//! [`NOT_VERIFIED`], which every answer ends on, says that no line here is a probe of what your
//! box, your network or your key actually reaches. All four spellings now say the same thing.
//!
//! ⚠ **`ls` must not print a roster this module TYPES.** `Source` and `UNBUILT_SOURCES` are the
//! declarations; a third copy here is the copy-drift disease the `data` plane has already paid for
//! twice (the bare-`data` refusal that omitted `rm`, and the `--json` roster sentence that was two
//! short within a release). Derive it — [`rows`] does, and [`reaches`] goes one further and reads
//! the TRANSPORT column straight out of `Source::engine_verb`, which is the declaration of that
//! same split.
//!
//! # ⚠ THE HONEST BOUNDARY: this group reaches nothing, and every answer SAYS SO
//!
//! The stub this module replaced promised `show` would perform "a manifest read for `vike`", and
//! §11's phase table says the same. **That read is not available here and this phase does not add
//! one.** `crates/vike-cli/Cargo.toml` links no HTTP client — no `ureq`, and `vike-bridge-core` is
//! taken with `default-features = false` — and adding one to print a description would put a second
//! transport stack into the binary that signs orders, for a verb whose whole value is that it works
//! before you have a subscription.
//!
//! So both verbs are LOCAL, and the consequence is carried in the OUTPUT rather than in this
//! comment: every answer ends on [`NOT_VERIFIED`] and every `--json` document carries
//! `verified_against_the_vendor: false`. **Naming the limit is the deliverable.** A local
//! description that read like a probe would tell an operator their key reaches something it may
//! not — positive confirmation of something false, which is the defect shape this repository
//! deleted `Policy::max_total_exposure` for.
//!
//! ⚠ **"every answer" is a CORRECTION, not a restatement.** Both of those were `show`'s alone until
//! it was fixed: `ls` printed a column headed REACHES — `a datahub`, `the engine, on this box` —
//! with no disclaimer in either rendering and no honesty field in its document, while this very
//! paragraph claimed every answer said so. A wrapper folding `ls --json` could read
//! `"reaches": "the engine, on this box"` as a fact about the box it was running on. It is a fact
//! about the BUILD.
//!
//! §9.1 is why the limit is worth naming rather than hiding: data.vike.io's ARCHIVE is keyed while
//! its MANIFEST is not, so *what exists* and *what I may fetch* are separately answerable there —
//! the one source in the roster with that property, and the reason this verb sits in P2 at all.
//! The half that needs no network ships now; the half that needs one is named, not faked.
//!
//! # ⚠ TWO CLASSES, never one
//!
//! §9.2 corrects §9.1's own table: data.vike.io serves **market history** (an instrument's own
//! tape) and **derived positioning analytics** (metrics ABOUT traders, not about a price), and an
//! earlier draft listed them as if they were one kind of thing. [`VIKE_CLASSES`] keeps them apart,
//! because the second is what makes the source unlike every other row in the listing.
//!
//! ⚠ **It names no `kind=` and may not.** Those descriptions used to end on `Lands as kind=book /
//! trade / quote` and `kind=cohort / perp_metrics` — five names copied out of
//! `crates/vike-data/src/store/store_kind.rs`'s `STORE_KINDS`, the declared authority for that roster.
//! This crate cannot link `vike-data`, so the copy was not derivable and nothing compared the two;
//! `crate::cmd::data`'s own `rm` and `repair` arms state the rule for exactly this reason — an
//! unknown kind is refused on the FAR side against that table, "because a roster copied into this
//! crate would be a second list to keep in step". Being unable to derive a roster is an argument
//! for not PRINTING it, never for typing it.
//!
//! ⚠ **And it serves no CEX market data, by the owner's ruling of 2026-09-21** — the Polymarket
//! archive, the event API and the Hyperliquid panels, and nothing else. Nothing rendered here may
//! describe, advertise or imply otherwise; [`VIKE_LICENCE`] states it outright so a reader cannot
//! infer a wider offer from a table that lists a venue axis beside an exchange.
//!
//! # A roster venue also prints its HISTORY CHANNELS
//!
//! `show binance` used to say a venue token is "public market data" and stop, which was false for
//! a venue whose history needs a credential and silent about the question an end user actually has:
//! *how far back can I go, through which door, and what does it cost?* The answer is a table in
//! `vike-catalog` (`vike_catalog::history_channels_for`), one row per (venue, channel), each citing
//! where its limits were read — and [`show_lines`] / [`show_json`] print a roster venue's rows under
//! the four cells, a rolling window resolved to a DATE from the box's clock at render time.
//!
//! ⚠ **That table is compiled-in data, so this group still reaches nothing** — and it says so
//! twice: [`HISTORY_NOTE`] rides every channel block, and [`NOT_VERIFIED`] still closes every
//! answer. An evidence URL in that block is a CITATION to the vendor's documentation dated by the
//! day a maintainer read it; it is not a base this binary resolved or a page it fetched, which is
//! the property `VIKE_BASE` keeps for the archive's endpoints and the reason a documentation URL
//! may print where an API base may not.
//!
//! A name that is not on the roster is still taken as a venue token and gets no channels at all —
//! and [`undeclared_note`] says that "no rows" is not "none exist".
//!
//! # ⚠ ONE opt-in reach: `show VENUE --addr A`
//!
//! "This group reaches nothing" above is the rule for every line WITHOUT `--addr`, and it stays the
//! rule: no configured address is ever used, so a plain `show` still opens no socket. With
//! `--addr`, a ROSTER venue's `show` asks that one datahub for `Request::HistoryChannels` on the
//! Observe scope (the owner's Q3 answer of 2026-10-02,
//! `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §7) and prints that
//! SERVER's rows with what only it can say — whether each lane is mounted there, whether a
//! credentialed lane's key is stored there (a word, never the key) and what its store holds. A
//! datahub older than the read is answered from this binary's own table under
//! `vike_datahub_client::history::COMPILED_TABLE_CAPTION`. Neither is a vendor read, so
//! `verified_against_the_vendor` stays `false`; [`NOT_VERIFIED`]'s "nothing above was read from a
//! server" would be false there, so an `--addr` answer ends on [`asked_closer`] instead.

use std::process::ExitCode;

use vike_catalog::HistoryChannel;
use vike_node_proto::auth::NodeKeys;

use super::{Format, Source, UNBUILT_SOURCES, col, parse_format};
use crate::cmd::args::exit_for_parse_error;

mod asked;
mod grammar;
mod render;
mod roster;

use self::asked::{ask_the_datahub, asked_json, asked_lines};
use self::grammar::parse;
use self::render::{ls_json, ls_lines, show_json, show_lines};
use self::roster::rows;
use super::{connect, parse_source};

#[cfg(test)]
use self::asked::{Asked, asked_closer};
#[cfg(test)]
use self::render::channel_lines;
#[cfg(test)]
use self::roster::{HISTORY_HEADING, LS_NOTES, VIKE_CLASSES, VIKE_KEYS};
#[cfg(test)]
use self::roster::{built_row, ls_notes, reaches, resolve};
#[cfg(test)]
use vike_catalog::history_channels_for;
#[cfg(test)]
use vike_datahub_client::history::{COMPILED_TABLE_CAPTION, CredentialPresence};
#[cfg(test)]
use vike_datahub_client::history::{HeldKind, compiled_report};
#[cfg(test)]
use vike_model::VENUES;

/// This group's own usage roster, in `crate::cmd::data`'s `USAGE` style — a group owns its own
/// grammar, so it owns the page that documents it.
const USAGE: &str = "\
usage: vike-cli data source <verb> [options]

WHERE rows may come from — the `--source` axis `data hist fetch` takes, as a roster you can
read instead of a value you have to guess. Both verbs are LOCAL — they open no socket, ask no
datahub and read nothing from any vendor — UNLESS `show VENUE` is given --addr, which asks that
one datahub for the venue's history channels. `ls` refuses --addr by name rather than ignoring it.

  ls           every source, its STATE (built | designed), what it is REACHED BY, and what it
               COSTS. The `designed` rows are the ones `--source` refuses by name today, and
               their COST cell is what each is WAITING ON
  show NAME    what that source holds, and what would fetch it — read off the axis this BINARY
               was built with, never probed. NAME is any `--source` value: a built one, a
               designed one, or a venue token — an unrecognised NAME is taken as a VENUE, which
               is exactly what `--source` itself does with it. A venue on the roster also lists
               its HISTORY CHANNELS: through which doors its history comes, how far back each
               goes, what each limits and costs, and where each answer was read from

options:
  --format F   HOW the answer is rendered: `table` (the default) or `json`. The same axis and
               the same refusals the `data hist` CATALOG verbs carry, reached through the same
               parser — `jsonl` is served by `data hist get`, the verb that emits ROWS, and is
               refused here because a roster is not rows; `csv`/`parquet` are FILE formats,
               written by `data hist export --out FILE`. Each is refused by name with its
               own reason
  --json       shorthand for --format json. For the roster: one object per source. For one row:
               the same fields plus what it holds, and `channels` for a roster venue. BOTH
               documents carry verified_against_the_vendor, which is FALSE and stays false
               until a vendor read exists — a machine reader must never take either for a probe
  --addr A     `show VENUE` only, VENUE on the roster: ask the datahub at A for the venue's
               history channels as THAT server's build declares them, plus what only it can say
               — whether each lane is mounted there, whether a credentialed lane's key is stored
               there (a word, never the key) and what its store holds. A datahub older than that
               read is answered from this binary's own table, which the output says. Opt-in: no
               configured address is used, and nothing is ever asked of a vendor
  -h, --help   this message

⚠ NOTHING here is verified against a vendor, and without --addr nothing asks a server either.
  data.vike.io's archive is keyed and its manifest is not, so `what exists` is answerable
  without a subscription — but this binary links no HTTP client and does not ask. Every line
  these verbs print is a description of a lane — this binary's, or with --addr that datahub's
  — never a probe of what your key reaches. See
  docs/superpowers/specs/2026-09-20-cli-data-surface-design.md §9.";

/// Every verb of this group, in the order [`USAGE`] lists them — and the roster the
/// "a verb is required" refusal RENDERS rather than restates. `crate::cmd::data`'s `SUBCOMMANDS`
/// exists for the same reason and the incident is recorded there: the one message whose whole job
/// is to name a roster named it short, on the verb that DELETES.
const VERBS: &[&str] = &["ls", "show"];

/// The cell that stands for the VENUE TOKEN CLASS, which has no single name.
///
/// ⚠ `Source::Venue` is not a `--source` spelling — it is what `super::parse_source` answers for
/// every value it does not recognise, deliberately, because the reachable venue set is a property
/// of a datahub this crate cannot see at parse time (§7.1). A listing that printed `binance` here
/// would be this binary claiming a venue roster it has refused to hold.
const VENUE_TOKEN: &str = "<venue>";

/// The ONE source name this module special-cases, and it is one row rather than a roster.
///
/// ⚠ §9.1 and §9.2 are written ABOUT this row — the keyed-archive / keyless-manifest split and the
/// two classes are facts about no other source — so [`show_lines`] expands it and nothing else. A
/// second special case would be the start of a hand-typed roster and belongs in a table instead.
/// `the_special_cased_name_is_still_a_declared_row` holds it to `UNBUILT_SOURCES`, so renaming that
/// row reddens this module rather than silently turning the expansion off.
const VIKE: &str = "vike";

/// Whether a source is REACHABLE from a `vike-cli` verb today.
///
/// ⚠ The two words answer the operator's question, not a build one: `designed` means `--source
/// NAME` is refused by name, whatever compiles. Two of the unbuilt lanes are compiled into every
/// `vike-backfill` build (§9's corrected table) and that changes nothing anybody can type, so it is
/// not what this column reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// `--source NAME` works today.
    Built,
    /// `--source NAME` is refused BY NAME, with what it is waiting on.
    Designed,
}

impl State {
    /// The token both renderings use, and the string a `--json` consumer branches on.
    fn as_str(self) -> &'static str {
        match self {
            State::Built => "built",
            State::Designed => "designed",
        }
    }
}

/// One source, as BOTH verbs see it — so `show` can never describe a source differently from the
/// listing it came out of.
///
/// `cost` is the one cell whose MEANING depends on `state`, and [`DESIGNED_COST`] says so: on a
/// `built` row it is what using the source costs you, and on a `designed` row it is what that
/// source is waiting on. They share a column because nothing is spent on a source no verb reaches,
/// so a second column would be empty on every row that has an answer in the first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    name: String,
    state: State,
    /// What performs the fetch, or `None` for a source nothing reaches yet.
    reaches: Option<&'static str>,
    cost: &'static str,
    /// Whether this row is the VENUE TOKEN CLASS rather than a named source — see [`VENUE_TOKEN`].
    venue_token: bool,
    /// The venue's history channels — `vike_catalog::history_channels_for`'s rows for a name ON
    /// THE ROSTER, and empty for every other name. Empty is not "none exist": [`Row::declared`]
    /// says whether the table was consulted for this name at all.
    channels: &'static [HistoryChannel],
    /// Whether `name` is a roster venue, i.e. whether `channels` is a DECLARATION about it. A venue
    /// token that is not on the roster gets `false`, and [`undeclared_note`] says what that means.
    declared: bool,
}

/// Every value of `super::Source`, in §9's order — the BUILT half of the listing.
///
/// ⚠ **The facts are not here, and that is the point.** [`built_row`]'s `match` is exhaustive, so a
/// variant added to `Source` does not COMPILE until it has been described; this array carries only
/// the order.
///
/// ⚠ **The residual this used to declare was understated, and the correction is worth carrying.**
/// It said the gap was "the same residual `crate::cmd::data`'s `SUBCOMMANDS` carries". It is
/// strictly WEAKER than that one. `SUBCOMMANDS` is held against `parse`'s match in BOTH directions
/// by `all_subcommands_are_reachable_by_the_name_they_advertise`, because an unknown subcommand is
/// an ERROR there; `super::parse_source`'s catch-all answers `Ok(Source::Venue)` for every
/// unrecognised string, so a round trip through the axis can never detect an omission here, and
/// every test that would notice one iterates this array itself. Stable Rust cannot enumerate an
/// enum's variants, so there is no gate to write: a variant with a [`built_row`] arm and no row in
/// this array would be absent from `ls`, absent from `ls --json` and green everywhere. The one
/// backstop is the instruction written where the compiler actually stops you — on that match.
const SOURCES: &[Source] = &[Source::Venue, Source::Starter, Source::Demo];

/// What the COST cell MEANS on a `designed` row — the one cell whose meaning depends on STATE.
///
/// ⚠ It is a const of its own rather than a line of [`LS_NOTES`] because BOTH verbs owe it: `ls`
/// prints the column and `show` prints the cell. Until it was lifted out, only `ls` explained it,
/// so `show tardis` printed a cost for something nobody can buy with no word about why.
const DESIGNED_COST: &str = "COST on a `designed` row is what that source is WAITING ON — nothing \
                             is spent on a source no verb reaches.";

/// The line EVERY answer WITHOUT `--addr` ends on, and the point of this phase's shape. An
/// `--addr` answer ends on [`asked_closer`] instead, because a server WAS asked.
///
/// ⚠ It is unconditional rather than reserved for `vike`, and that is the stronger claim: a reader
/// who met it only on the row that HAS a live manifest would reasonably read its absence elsewhere
/// as "this one was checked". Nothing here is checked.
const NOT_VERIFIED: &str = "⚠ nothing above was read from a server or from a vendor. This answer \
                            reached nothing: it describes the sources this BINARY was built with, \
                            so no line here is a probe of what your box, your network or your key \
                            actually reaches.";

/// The footnote every roster venue's channels carry, and the sentence that keeps them from being
/// read as a probe.
///
/// ⚠ It says three things the block above cannot say for itself. The rows are a TABLE COMPILED
/// INTO THIS BINARY — each carries the date a maintainer read a vendor page or measured, and none
/// of it was checked by this command. A rolling window is resolved to a date from THIS box's clock,
/// so the date moves with the day you ask. And the two blanks mean different things: `not known`
/// says nobody has read a source or measured, `not stated` says one was read and is silent — and
/// neither is a promise about how far back the data goes.
const HISTORY_NOTE: &str = "⚠ the channels above are a table compiled into this binary, not a \
                            probe: each row's evidence carries the date a maintainer read a vendor \
                            page or measured, and none of it was checked by this command. A rolling \
                            window is resolved to a date from this box's clock. `not known` means \
                            nobody has read a source or measured anything, `not stated` means one \
                            was read and is silent — and neither is a promise about how far back \
                            the data goes.";

/// The columns before a channel's name — two spaces, the class word padded to the longest
/// (`request`), and one more — which is also how far a channel's detail lines are indented, so the
/// cells hang under the name.
const CLASS_W: usize = 7;
const DETAIL_INDENT: &str = "          ";

/// The width of a detail line's label, wide enough for the longest one (`per request:`).
const LABEL_W: usize = 13;

/// Which verb ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `ls` — the whole axis, as a table or a document.
    Ls,
    /// `show NAME` — one row of it, expanded.
    Show,
}

/// A parsed `data source …` line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    verb: Verb,
    /// `show`'s one positional, ALREADY RESOLVED. Always `Some` on [`Verb::Show`] and always `None`
    /// on [`Verb::Ls`] — [`parse`] refuses both other shapes rather than defaulting either way.
    ///
    /// ⚠ It carries the ROW rather than the string deliberately: resolving at parse time is what
    /// puts a value this group cannot describe on the USAGE rung — where `data hist fetch --source
    /// ""` already puts the empty one — instead of rendering it as a working venue. The empty value
    /// itself never reaches [`resolve`] any more; see [`SHOW_NEEDS_A_NAME`] for why the rung owes
    /// its own sentence rather than the axis's.
    row: Option<Row>,
    json: bool,
    /// `show VENUE --addr A`'s datahub — `Some` only on a `show` of a ROSTER venue, which
    /// [`parse`] holds. Never filled from a configured address: the reach is opt-in.
    addr: Option<String>,
}

/// The refusal a missing NAME gets, worded once — [`parse`] returns it and [`run`]'s
/// otherwise-unreachable arm answers with it rather than inventing a default.
///
/// ⚠ **An EMPTY positional gets it too, and that is the point of the const being one sentence.**
/// `show ""` and `show` are the same mistake — no name was given — so the operator who reads this
/// and drops the empty argument reads the SAME sentence rather than a second, different one. The
/// alternative shipped first: the empty value fell through to `super::parse_source`, whose refusal
/// is written for the `--source` FLAG and instructs the reader to omit it. There is no flag on this
/// rung to omit.
const SHOW_NEEDS_A_NAME: &str = "`data source show` needs a source NAME. `data source ls` lists \
                                 them all — and any value that is not one of them is taken as a \
                                 venue, the same way `--source` takes it";

/// Does `data hist` take this flag?
///
/// ⚠ **DERIVED from `crate::cmd::data::USAGE`'s options block, never typed here.** That page is
/// already held against the parser it documents — `the_usage_names_every_subcommand_and_flag_this_
/// parser_accepts` fails when a flag the parser accepts is missing from it — so reading it is
/// reading a declaration, while a list in this module would be a second roster with nothing holding
/// it in step. That is the copy-drift disease this module's own doc opens on, and it is why the
/// first version of the refusal above named the set in PROSE rather than enumerating it: prose
/// cannot go stale into a false claim about a specific flag, but it also cannot answer about one,
/// which is exactly what the refusal needed to do.
///
/// An options row is `  --flag …` at the left margin of that page, which is the one shape every
/// row shares and no prose line does.
fn is_a_hist_flag(flag: &str) -> bool {
    super::USAGE
        .lines()
        .filter(|l| l.starts_with("  --"))
        .any(|l| l.split_whitespace().next() == Some(flag))
}

/// Run a `data source …` line. `argv` is everything AFTER the group word.
///
/// `keys` signs the ONE socket this group can open — `show VENUE --addr A`'s, at the Observe scope
/// the history-channels read requires. ⚠ `configured_addr` is taken and IGNORED, and the underscore
/// is the whole story: the group layer hands every group the same three arguments so one dispatch
/// arm serves all of them, and this group's one reach is OPT-IN — a `show` without `--addr` must
/// still reach nothing, which a fallback to the configured datahub would break.
pub(super) fn run(
    argv: &[String],
    keys: Option<&NodeKeys>,
    _configured_addr: Option<&str>,
) -> ExitCode {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("data source", USAGE, &msg),
    };
    // The one clock read in this group, taken here and handed down so every renderer below stays
    // pure: a rolling window in a channel resolves to a date against it.
    let today_ms = vike_model::now_ms();
    let text = match (args.verb, args.row.as_ref()) {
        (Verb::Ls, _) if args.json => ls_json(&rows()),
        (Verb::Ls, _) => ls_lines(&rows()).join("\n"),
        (Verb::Show, Some(row)) if args.addr.is_some() => {
            let addr = args.addr.as_deref().unwrap_or_default();
            match ask_the_datahub(addr, keys, today_ms) {
                Ok(asked) if args.json => asked_json(row, &asked),
                Ok(asked) => asked_lines(row, &asked).join("\n"),
                Err(e) => {
                    eprintln!("vike-cli data source: {}", e.msg);
                    return e.exit.into();
                }
            }
        }
        (Verb::Show, Some(row)) if args.json => show_json(row, today_ms),
        (Verb::Show, Some(row)) => show_lines(row, today_ms).join("\n"),
        // Unreachable by construction — [`parse`] refuses a `show` with no NAME — and it REFUSES
        // rather than defaulting, with the same sentence, because the `unwrap_or_default()` this
        // replaced is exactly how `show ""` came to render a working venue row.
        (Verb::Show, None) => return exit_for_parse_error("data source", USAGE, SHOW_NEEDS_A_NAME),
    };
    println!("{text}");
    ExitCode::SUCCESS
}

#[path = "source_tests.rs"]
#[cfg(test)]
mod source_tests;
