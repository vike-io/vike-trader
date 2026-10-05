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

use vike_catalog::{ChannelState, HistoryChannel, history_channels_for};
use vike_datahub_client::history::{
    COMPILED_TABLE_CAPTION, ChannelReport, CredentialPresence, HeldKind, HistoryChannelsReport,
    compiled_report,
};
use vike_model::{VENUES, time::epoch_ms_to_utc_date};
use vike_node_proto::auth::{NodeKeys, Scope};

use super::{Format, Source, UNBUILT_SOURCES, col, parse_format};
use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::exit::CmdResult;

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

/// The TRANSPORT cell — DERIVED from `Source::engine_verb`, never restated.
///
/// ⚠ That function IS the datahub-or-engine split: `None` means the source asks a datahub
/// (`Request::Backfill`), `Some` means it spawns the standalone engine, and its own doc calls
/// itself the seam. Reading the cell out of it means a source that CHANGES transport changes this
/// column with no edit here — which is exactly what a hand-written third column would not do.
fn reaches(source: Source) -> &'static str {
    match source.engine_verb() {
        None => "a datahub",
        Some(_) => "the engine, on this box",
    }
}

/// One built source's row. The `match` is what the compiler holds exhaustive — see [`SOURCES`].
fn built_row(source: Source) -> Row {
    // The COST cell is the credential-and-network story §11.2 says differs per lane, and it is the
    // whole reason `show` ships before the vendor dispatch does: you should be able to ask what a
    // source needs before you try to use it.
    //
    // ⚠ IF YOU ARE HERE BECAUSE THE COMPILER DEMANDED AN ARM: add the variant to [`SOURCES`] too.
    // Nothing else will ask you for it — that const's doc carries why no test can, and an arm
    // without a row is a source the axis accepts and the roster verb says does not exist.
    let (name, cost, venue_token) = match source {
        // ⚠ This cell said "no credentials — public market data" and that is FALSE for a venue
        // whose history needs a credential (OANDA's token, IBKR's gateway session). The row is a
        // CLASS, so the sentence must hold for every venue it stands for: most are public, one
        // that is not is MARKED, and `show <venue>` is where its own door is named.
        Source::Venue => (
            VENUE_TOKEN,
            "pulled by the datahub you point at — public market data for most venues; a venue \
             that needs a credential is marked, and `data source show <venue>` is where to look",
            true,
        ),
        Source::Starter => (
            "starter",
            "no credentials — plain HTTPS to the public mirror; a fixed span, so it takes no window",
            false,
        ),
        Source::Demo => {
            ("demo", "nothing at all — a closed-form curve, no network and no venue", false)
        }
    };
    Row {
        name: name.to_string(),
        state: State::Built,
        reaches: Some(reaches(source)),
        cost,
        venue_token,
        channels: &[],
        declared: false,
    }
}

/// The whole listing, BUILT half then DESIGNED half — derived from both declarations and typed by
/// neither. See this module's doc.
fn rows() -> Vec<Row> {
    let mut rows: Vec<Row> = SOURCES.iter().copied().map(built_row).collect();
    rows.extend(UNBUILT_SOURCES.iter().map(|(name, why)| Row {
        name: (*name).to_string(),
        state: State::Designed,
        reaches: None,
        cost: why,
        venue_token: false,
        channels: &[],
        declared: false,
    }));
    rows
}

/// What `--source NAME` would resolve to, as a row — or the AXIS's own refusal for a value the
/// axis refuses.
///
/// ⚠ **The fallback is not spelled here, it is ASKED of `super::parse_source`, and that is a
/// CORRECTION.** This doc used to claim the rule was "`super::parse_source`'s rule, spelled the
/// same way on purpose". It was not: that function carries an explicit empty-value refusal this
/// one did not mirror, and [`parse`] accepted an empty positional — so `data source show ""` exited
/// 0, printed a working venue row, and ended on the sentence "the same answer `--source ` gets",
/// which was false, because `data hist fetch --source ""` is refused on the usage rung. The group
/// whose whole job is that the two agree was the LOOSER grammar, and then said they agreed.
/// Deriving the answer from that function rather than restating its rule is what makes the claim
/// true instead of merely repeated.
///
/// A name that IS a declared row — built or designed — never reaches the axis: [`rows`] answers
/// first, deliberately, because a designed source is a value the axis REFUSES and a row this group
/// DESCRIBES, and describing it is this verb's entire job.
///
/// ⚠ **The EMPTY value no longer arrives here, and the reason is what the fix above got wrong.**
/// `super::parse_source`'s empty-value sentence is written for a FLAG — it says *"Omit the flag to
/// use a venue"* — and [`parse`] was printing it verbatim on a POSITIONAL rung, where `show` takes
/// no flag to omit and omitting the argument yields [`SHOW_NEEDS_A_NAME`] instead: two refusals for
/// one mistake, the first of which is wrong about what the operator typed. [`parse`] answers an
/// empty positional with [`SHOW_NEEDS_A_NAME`] now, so following the instruction reproduces the
/// SAME sentence rather than a second one. This function still refuses the empty value — it is the
/// backstop that keeps the group from being a looser grammar than the axis if a future caller
/// reaches it another way — and `an_empty_name_is_one_refusal_written_for_the_rung_that_prints_it`
/// holds both halves.
fn resolve(name: &str) -> Result<Row, String> {
    if let Some(row) = rows().into_iter().find(|r| r.name == name) {
        return Ok(row);
    }
    // Every NAMED source is a row above, so what reaches here is `parse_source`'s catch-all: the
    // VENUE TOKEN class, carrying whatever the operator typed — or its refusal, which is the only
    // other answer it has for a name no row claimed.
    let source = super::parse_source(name)?;
    // The channels are asked for by NAME, and only a roster venue is a declaration: an unknown
    // venue token gets the empty slice `history_channels_for` answers it, and `declared: false` is
    // what stops that emptiness reading as "no channel exists".
    Ok(Row {
        name: name.to_string(),
        channels: history_channels_for(name),
        declared: VENUES.contains(&name),
        ..built_row(source)
    })
}

/// What the COST cell MEANS on a `designed` row — the one cell whose meaning depends on STATE.
///
/// ⚠ It is a const of its own rather than a line of [`LS_NOTES`] because BOTH verbs owe it: `ls`
/// prints the column and `show` prints the cell. Until it was lifted out, only `ls` explained it,
/// so `show tardis` printed a cost for something nobody can buy with no word about why.
const DESIGNED_COST: &str = "COST on a `designed` row is what that source is WAITING ON — nothing \
                             is spent on a source no verb reaches.";

/// What `ls` prints under the table. Each line is a fact the columns alone would misreport.
const LS_NOTES: &[&str] = &[
    DESIGNED_COST,
    "`<venue>` is a CLASS, not a name: any --source value this side does not recognise is taken as \
     a venue. The reachable venue set belongs to the datahub, and a roster here would be this \
     binary claiming to know it.",
    // ⚠ This note used to read "…says what one holds and what this box reaches", which was the
    // third spelling of a promise the output denies — see this module's doc.
    "`vike-cli data source show NAME` expands ONE row of this table: the same cells, what that \
     source holds, and what its value does at the axis. It reaches nothing either, unless a \
     roster venue is given --addr: then it asks that one datahub for the venue's history \
     channels.",
];

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

/// The two classes §9.2 separates — the correction that section exists to make.
///
/// ⚠ Class 2 is not an instrument's tape, and rendering it as one is the mistake: `venue` on the
/// positioning series is the exchange whose POSITIONS were graded, never the metrics service that
/// graded them.
///
/// ⚠ **No `kind=` is named here, and the module doc carries why**: that roster is
/// `crates/vike-data/src/store/store_kind.rs`'s `STORE_KINDS`, a table this crate cannot link and
/// therefore cannot derive — which makes printing it a second list with nothing holding it in step,
/// exactly what `crate::cmd::data`'s `rm` and `repair` arms refuse to do.
const VIKE_CLASSES: &[(&str, &str)] = &[
    (
        "market history",
        "an instrument's own tape — the Polymarket L2 archive (book_events, trades, l1_quotes) \
         and the paged event API.",
    ),
    (
        "positioning analytics",
        "metrics ABOUT traders and positioning rather than about a price — the Hyperliquid cohort \
         ladder and the hourly asset panel, whose `venue` is the exchange whose positions were \
         graded rather than the service that graded them.",
    ),
];

/// The owner's ruling of 2026-09-21, rendered so it cannot be inferred away.
///
/// ⚠ Class 1 above names a venue axis and class 2 names an exchange, which between them could be
/// read as an offer of exchange candles. It is not one, and this line is where that reading stops.
const VIKE_LICENCE: &str = "⚠ it serves NO CEX market data at all: the Polymarket archive, the \
                            event API and the Hyperliquid panels, and nothing else.";

/// The keyed/keyless split — the property that makes an honest `show vike` possible at all, and
/// the sentence saying this build does not yet exploit it. See this module's doc.
const VIKE_KEYS: &str = "keys: the ARCHIVE is keyed and its MANIFEST is not, so `what exists` and \
                         `what I may fetch` are separately answerable on this source — the only \
                         row in the listing with that property. ⚠ This verb answers NEITHER from \
                         the vendor: the unauthenticated manifest read lands with the vendor \
                         dispatch (P4), and until it does, the lines above describe the lane rather \
                         than read your subscription.";

/// Why `show vike` prints no URL.
///
/// ⚠ §9.1's third property: all three bases are overridable where their lane is configured
/// (`crates/vike-backfill/src/vike_archive.rs`'s `DEFAULT_BASE`,
/// `crates/vike-backfill/src/events_api.rs`'s `DEFAULT_BASE` and
/// `crates/vike-backfill/src/vikedata/client.rs`'s `VIKEDATA_BASE`), so a verb that names one must
/// report the base it RESOLVED. This side resolves none of them — it links that crate not at all
/// and reads no environment — so it prints the rule instead of a constant that would name a base
/// this box may not be using.
const VIKE_BASE: &str = "base: one per lane, each overridable where that lane is configured — so no \
                         URL is printed here. This side resolves none of them, and a constant would \
                         name a base this box may not be using.";

/// The line above a roster venue's channels.
const HISTORY_HEADING: &str =
    "history — through which doors this venue's history comes, and how far back each goes:";

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

/// What a venue token that is NOT on the roster is told about its channels: that there are none
/// DECLARED, which is a statement about this table and not about the venue.
///
/// ⚠ The empty answer is the dangerous one — an operator who typed a venue this build has never
/// classified would read a blank as "no history channel exists", and the only thing this build
/// knows about that venue is its name.
fn undeclared_note(name: &str) -> String {
    format!(
        "this build declares no history channels for `{name}` — that is a statement about this \
         table, not about the venue: nothing here says what it offers or how far back it goes."
    )
}

/// The footnotes BOTH `ls` renderings carry: [`LS_NOTES`] plus the limit every answer in this group
/// ends on.
///
/// ⚠ [`NOT_VERIFIED`] was pushed by [`show_lines`] alone until this existed, so the one verb that
/// prints a column headed REACHES was the one verb that never said nothing had been asked. It is
/// derived here rather than appended at each call site so the table and the document cannot carry
/// different footnotes — which is the defect `notes` had on the other verb.
///
/// ⚠ **That sentence was a CLAIM before it was a wiring, and the gap is worth carrying.** Only
/// [`ls_json`] called this; [`ls_lines`] re-spelled the same chain inline — `LS_NOTES` mapped to
/// `note: {n}`, then [`NOT_VERIFIED`] pushed after it — which is precisely "appended at the call
/// site", one paragraph under a doc denying it. Nothing could see the difference: the tests
/// asserted the DOCUMENT's notes are printed by the table, so a footnote added to the table alone
/// satisfied them and was silently absent from `ls --json`. Both renderings now RENDER this
/// function ([`note_lines`] is the per-note shape), and
/// `the_table_prints_no_footnote_the_document_omits` gates the direction that was open.
fn ls_notes() -> Vec<&'static str> {
    LS_NOTES.iter().copied().chain(std::iter::once(NOT_VERIFIED)).collect()
}

/// ONE footnote, as the TABLE prints it — a `note:` row, or, for [`NOT_VERIFIED`], a blank line
/// and the sentence itself.
///
/// ⚠ The closer is set apart rather than filed as one `note:` among several because it is not one:
/// it is the limit EVERY answer in this group ends on, and prefixing it would rank it beside a
/// column footnote. A function rather than two pushes in [`ls_lines`] so that the table renders
/// [`ls_notes`] WHOLE — the shape a footnote cannot escape by being appended somewhere else.
fn note_lines(note: &str) -> Vec<String> {
    if note == NOT_VERIFIED {
        vec![String::new(), note.to_string()]
    } else {
        vec![format!("note: {note}")]
    }
}

/// The table plus its footer. Pure, so the shape is unit-tested without a process.
fn ls_lines(rows: &[Row]) -> Vec<String> {
    let name_w = col("SOURCE", rows.iter().map(|r| r.name.len()));
    let state_w = col("STATE", rows.iter().map(|r| r.state.as_str().len()));
    let reach_w = col("REACHES", rows.iter().map(|r| reach_cell(r).len()));

    let mut lines = vec![format!(
        "{:<name_w$}  {:<state_w$}  {:<reach_w$}  {}",
        "SOURCE", "STATE", "REACHES", "COST"
    )];
    for r in rows {
        lines.push(format!(
            "{:<name_w$}  {:<state_w$}  {:<reach_w$}  {}",
            r.name,
            r.state.as_str(),
            reach_cell(r),
            r.cost
        ));
    }
    lines.push(String::new());
    // ⚠ RENDERED from [`ls_notes`], never re-spelled: the footer is that derivation and nothing
    // else, which is what makes the table and the document one set of footnotes rather than two
    // lists that happen to agree today. See [`ls_notes`] for what the re-spelling cost.
    lines.extend(ls_notes().into_iter().flat_map(note_lines));
    lines
}

/// The REACHES cell for a row that reaches nothing. A dash would read as "not applicable"; this
/// reads as the answer it actually is.
fn reach_cell(row: &Row) -> &'static str {
    row.reaches.unwrap_or("nothing yet")
}

/// What a `designed` row's `show` must not leave the operator to find out by typing the value.
///
/// ⚠ A designed row used to print `state: designed` and its cost cell and stop — no statement that
/// `--source NAME` is refused today, and no "Built today: …" the way `super::parse_source`'s own
/// refusal carries one. So learning what IS usable cost a round trip through a refusal, which is
/// the round trip this group was built to remove.
///
/// The built half is RENDERED from [`SOURCES`], never typed: that refusal and this note are two
/// sentences nothing holds equal, so this one derives its list rather than restating theirs.
fn designed_note(name: &str) -> String {
    let built: Vec<String> = SOURCES.iter().copied().map(|s| built_row(s).name).collect();
    format!(
        "⚠ `--source {name}` is REFUSED today — by name, with what it is waiting on, which is the \
         COST cell above. Built today: {}. `{VENUE_TOKEN}` is the default and stands for any venue \
         token, so it needs no --source at all.",
        built.join(", ")
    )
}

/// The footnotes a `show` carries — the facts the four cells alone would misreport, in the order
/// both of its renderings emit them.
///
/// ⚠ **This is what `notes` MEANS in this group's every document, and pinning it down is a
/// correction.** `show --json` used to set `notes` to [`show_lines`] — the ENTIRE human rendering,
/// `source:   vike` and `state:    designed` and the blank lines included — while `ls --json` set
/// the same field to three footnotes. One field name, two categorically different documents, from
/// two verbs of one group: a consumer that rendered `notes` as a bullet list printed `state:
/// designed` twice under `show` and three footnotes under `ls`, and a consumer that grepped `notes`
/// for the unverified warning found it under one verb and not the other. Every structured cell the
/// document already carries as a FIELD is now carried once.
fn show_notes(row: &Row) -> Vec<String> {
    let mut notes = Vec::new();
    // The row an operator reached by typing something this side does not recognise. Saying so is
    // the difference between "your venue is fine" and "nothing here judged your venue", and only
    // the second is true.
    if row.venue_token && row.name != VENUE_TOKEN {
        notes.push(format!(
            "`{name}` is not a source NAME this side knows, so it is taken as a VENUE token — the \
             same answer `--source {name}` gets. Whether that venue is one a datahub can reach is \
             a property of that process, and nothing here asked it.",
            name = row.name
        ));
    }
    if row.state == State::Designed {
        notes.push(designed_note(&row.name));
        notes.push(DESIGNED_COST.to_string());
    }
    if row.name == VIKE {
        notes.push(VIKE_LICENCE.to_string());
        notes.push(VIKE_KEYS.to_string());
        notes.push(VIKE_BASE.to_string());
    }
    if row.declared {
        notes.push(HISTORY_NOTE.to_string());
    } else if row.venue_token && row.name != VENUE_TOKEN {
        notes.push(undeclared_note(&row.name));
    }
    notes.push(NOT_VERIFIED.to_string());
    notes
}

/// The columns before a channel's name — two spaces, the class word padded to the longest
/// (`request`), and one more — which is also how far a channel's detail lines are indented, so the
/// cells hang under the name.
const CLASS_W: usize = 7;
const DETAIL_INDENT: &str = "          ";

/// The width of a detail line's label, wide enough for the longest one (`per request:`).
const LABEL_W: usize = 13;

/// One detail line of a channel: the label, padded, then the text. An empty label continues the
/// line above it, which is how a second evidence source hangs under the first.
fn detail(label: &str, text: &str) -> String {
    format!("{DETAIL_INDENT}{label:<w$}{text}", w = LABEL_W)
}

/// One channel as terminal lines: its class and name, then one line per cell.
///
/// A channel that serves something prints every cell; a FINDING that no channel exists prints the
/// finding, its scope and its source and nothing that would describe a channel; and a row nobody
/// classified says so and stops. Which of the three it is comes from the row itself, so this
/// function cannot disagree with the JSON's `presence`.
fn channel_lines(ch: &HistoryChannel, today_ms: i64) -> Vec<String> {
    let mut lines = vec![format!("  {:<w$} {}", ch.class.word(), ch.name, w = CLASS_W)];
    if ch.is_unclassified() {
        let reason = match ch.state {
            ChannelState::Designed(reason) => reason,
            ChannelState::Built(_) => "built",
        };
        lines.push(detail("state:", &format!("not classified — {reason}")));
        return lines;
    }
    if ch.is_absent() {
        lines.push(detail("result:", "none found in what was read"));
    } else {
        lines.push(detail("serves:", &ch.kinds_text()));
        lines.push(detail("depth:", &ch.depth_text(Some(today_ms))));
        lines.push(detail("per request:", &ch.per_request_text()));
        lines.push(detail("pace:", &ch.pace_text()));
        lines.push(detail("access:", &ch.access_text()));
        lines.push(detail("state:", &ch.state_text()));
    }
    for (i, source) in ch.evidence_lines().iter().enumerate() {
        lines.push(detail(if i == 0 { "evidence:" } else { "" }, source));
    }
    if !ch.note.is_empty() {
        lines.push(detail("note:", ch.note));
    }
    lines
}

/// A roster venue's whole history block: the heading, then each channel, blank-line separated.
fn history_lines(channels: &[HistoryChannel], today_ms: i64) -> Vec<String> {
    let mut lines = vec![HISTORY_HEADING.to_string()];
    for ch in channels {
        lines.push(String::new());
        lines.extend(channel_lines(ch, today_ms));
    }
    lines
}

/// `show NAME`. Pure, for [`ls_lines`]'s reason — and it takes the RESOLVED row rather than a
/// string, so there is no name left for this function to invent a default for. `today_ms` is the
/// caller's clock: a rolling window resolves against it, and a test passes a fixed instant.
fn show_lines(row: &Row, today_ms: i64) -> Vec<String> {
    let mut lines = vec![
        format!("source:   {}", row.name),
        format!("state:    {}", row.state.as_str()),
        format!("reaches:  {}", reach_cell(row)),
        format!("cost:     {}", row.cost),
    ];
    if row.name == VIKE {
        lines.push(String::new());
        lines.push("holds TWO CLASSES, and they are different kinds of thing:".to_string());
        for (class, what) in VIKE_CLASSES {
            lines.push(format!("  {class}: {what}"));
        }
    }
    if row.declared {
        lines.push(String::new());
        lines.extend(history_lines(row.channels, today_ms));
    }
    for note in show_notes(row) {
        lines.push(String::new());
        lines.push(note);
    }
    lines
}

/// `ls --json`: one object per source, carrying the same four cells the table carries — and the
/// same footnotes, under the same field name the other verb uses for the same kind of thing.
///
/// `reaches` is `null` rather than a sentence on a designed row, because a machine reader asking
/// "can I use this" wants a value it can test rather than prose it has to match on.
fn ls_json(rows: &[Row]) -> String {
    let sources: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "state": r.state.as_str(),
                "reaches": r.reaches,
                "cost": r.cost,
                "venue_token": r.venue_token,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "count": sources.len(),
        "sources": sources,
        "verified_against_the_vendor": false,
        "notes": ls_notes(),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, bools and nulls; serialization is total")
}

/// `show NAME --json`.
///
/// ⚠ `verified_against_the_vendor` is the field this whole phase turns on, and it is a hard `false`
/// rather than an omission: a consumer folding this document has no prose to read, so the ONE thing
/// it must not be able to assume is that the description was checked. It becomes a real answer the
/// day a vendor read exists; until then it says what happened, which is nothing.
///
/// `channels` carries a roster venue's history rows — each cell an object with a `kind` a consumer
/// branches on and a `text` the terminal prints — and is an EMPTY array for every other source.
/// `channels_declared` is what tells that emptiness apart: `false` says this build declares nothing
/// for the name, which is not the same as the venue having no channel. `channels_as_of` is the UTC
/// date every rolling window in `channels` was resolved against.
fn show_json(row: &Row, today_ms: i64) -> String {
    let holds: Vec<serde_json::Value> = if row.name == VIKE {
        VIKE_CLASSES
            .iter()
            .map(|(class, what)| serde_json::json!({ "class": class, "what": what }))
            .collect()
    } else {
        Vec::new()
    };
    let channels: Vec<serde_json::Value> =
        row.channels.iter().map(|ch| ch.as_json(today_ms)).collect();
    let doc = serde_json::json!({
        "source": row.name,
        "state": row.state.as_str(),
        "reaches": row.reaches,
        "cost": row.cost,
        "venue_token": row.venue_token,
        "verified_against_the_vendor": false,
        "holds": holds,
        "channels_declared": row.declared,
        "channels_as_of": epoch_ms_to_utc_date(today_ms),
        "channels": channels,
        "notes": show_notes(row),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, bools and nulls; serialization is total")
}

// ── `show VENUE --addr A`: the one reach this group has ──────────────────────────────────────────

/// What `show VENUE --addr A` got from the datahub at `addr`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    /// The address that was asked, as the operator typed it.
    addr: String,
    /// The rows — the SERVER's, with its overlay, when `served`; this binary's own compiled table
    /// with the overlay marked not known, when the server is older than the read.
    report: HistoryChannelsReport,
    /// Whether the server answered the history-channels read. `false` is a server OLDER than it:
    /// refused client-side with nothing sent, and answered from this binary's table — the design's
    /// §2.3 fallback, under [`COMPILED_TABLE_CAPTION`].
    served: bool,
}

/// Ask the datahub at `addr` for the history channels, on the Observe scope the read requires.
///
/// An unreachable datahub is an error on the connect rung, through `crate::cmd::data`'s `connect`
/// like every other verb that dials one. A datahub that does not advertise the read is NOT an
/// error: its absence means the server is older than the verb, and the honest answer is this
/// binary's own table, labelled — the rows are vendor facts compiled into both ends, and only the
/// overlay is the server's, which this never pretends to have.
fn ask_the_datahub(addr: &str, keys: Option<&NodeKeys>, today_ms: i64) -> CmdResult<Asked> {
    let mut client = super::connect(addr, keys, Scope::Read)?;
    if !client.serves_history_channels() {
        return Ok(Asked {
            addr: addr.to_string(),
            report: compiled_report(today_ms),
            served: false,
        });
    }
    let report = client.history_channels()?;
    Ok(Asked { addr: addr.to_string(), report, served: true })
}

/// The heading over the asked channels — which table the rows are, and who said so.
fn asked_heading(asked: &Asked) -> String {
    if asked.served {
        format!(
            "history — as the datahub at {} declares it, with what that server says about its own \
             lanes (its clock: {}):",
            asked.addr,
            epoch_ms_to_utc_date(asked.report.as_of_ms)
        )
    } else {
        format!("history — {COMPILED_TABLE_CAPTION} (the datahub at {} was asked):", asked.addr)
    }
}

/// The footnote the asked channels carry in place of [`HISTORY_NOTE`], which says the rows are
/// compiled into THIS binary — true of the fallback, and not of a served answer.
fn asked_history_note(asked: &Asked) -> String {
    if asked.served {
        format!(
            "⚠ the channels above are the table compiled into the datahub at {}, not a probe: each \
             row's evidence carries the date a maintainer read a vendor page or measured, and \
             nothing was checked against a vendor. `mounted`, `credential` and `held` are that \
             server's own answers; the credential line is a word, never the key. `not known` means \
             nobody has read a source or measured anything, `not stated` means one was read and is \
             silent — and neither is a promise about how far back the data goes.",
            asked.addr
        )
    } else {
        HISTORY_NOTE.to_string()
    }
}

/// The line an `--addr` answer ends on, in place of [`NOT_VERIFIED`], which says no server was
/// asked — false once one was.
fn asked_closer(asked: &Asked) -> String {
    format!(
        "⚠ the history block above came from the datahub at {} and nothing was read from a vendor. \
         No line here is a probe of what your box, your network or your key actually reaches.",
        asked.addr
    )
}

/// [`show_notes`] with its two server-denying footnotes replaced by their `--addr` twins.
fn asked_notes(row: &Row, asked: &Asked) -> Vec<String> {
    show_notes(row)
        .into_iter()
        .map(|n| {
            if n == HISTORY_NOTE {
                asked_history_note(asked)
            } else if n == NOT_VERIFIED {
                asked_closer(asked)
            } else {
                n
            }
        })
        .collect()
}

/// One served channel as terminal lines — [`channel_lines`]' layout over the wire's cells, plus the
/// server's overlay: `mounted:` on a built row, `credential:` wherever a lane reads one. Every word
/// is the cell's own `text`, so a depth form this binary has never heard of still prints.
fn report_channel_lines(ch: &ChannelReport) -> Vec<String> {
    let mut lines = vec![format!("  {:<w$} {}", ch.class, ch.name, w = CLASS_W)];
    match ch.presence.as_str() {
        "not_classified" => {
            lines.push(detail("state:", &format!("not classified — {}", ch.state.text)));
        }
        "none_found" => lines.push(detail("result:", "none found in what was read")),
        _ => {
            lines.push(detail("serves:", &ch.kinds_text));
            lines.push(detail("depth:", &ch.depth.text));
            lines.push(detail("per request:", &ch.per_request.text));
            lines.push(detail("pace:", &ch.pace.text));
            lines.push(detail("access:", &ch.access.text));
            lines.push(detail("state:", &ch.state.text));
        }
    }
    match ch.mounted {
        Some(true) => {
            lines.push(detail("mounted:", "yes — that datahub's collector table carries this lane"))
        }
        Some(false) => lines.push(detail(
            "mounted:",
            "no — that datahub mounts no lane for it, so a fetch through it is refused there",
        )),
        None => {}
    }
    if ch.credential != CredentialPresence::NotNeeded {
        lines.push(detail("credential:", ch.credential.phrase()));
    }
    for (i, source) in ch.evidence.iter().enumerate() {
        lines.push(detail(if i == 0 { "evidence:" } else { "" }, &source.text));
    }
    if !ch.note.is_empty() {
        lines.push(detail("note:", &ch.note));
    }
    lines
}

/// What the asked datahub's store holds for the venue, one line per kind — or why there is none.
fn held_lines(asked: &Asked, held: &[HeldKind]) -> Vec<String> {
    if !asked.served {
        return vec![format!(
            "held: not known — the datahub at {} is older than the history-channels read",
            asked.addr
        )];
    }
    if held.is_empty() {
        return vec![format!("held: nothing — the store at {} holds no series for it", asked.addr)];
    }
    let mut lines = vec![format!("held — what the store at {} holds for it:", asked.addr)];
    for h in held {
        lines.push(format!(
            "  {:<w$} {} series, {} rows, {} .. {}",
            h.kind,
            h.series,
            h.rows,
            epoch_ms_to_utc_date(h.first_ts),
            epoch_ms_to_utc_date(h.last_ts),
            w = CLASS_W
        ));
    }
    lines
}

/// `show VENUE --addr A`, as a table: the same four cells as [`show_lines`], then the venue's
/// channels as the asked datahub declares them with its overlay, what its store holds, and the
/// notes. Pure — the answer arrives as a value.
fn asked_lines(row: &Row, asked: &Asked) -> Vec<String> {
    let mut lines = vec![
        format!("source:   {}", row.name),
        format!("state:    {}", row.state.as_str()),
        format!("reaches:  {}", reach_cell(row)),
        format!("cost:     {}", row.cost),
        String::new(),
        asked_heading(asked),
    ];
    match asked.report.venue(&row.name) {
        Some(venue) => {
            for ch in &venue.channels {
                lines.push(String::new());
                lines.extend(report_channel_lines(ch));
            }
            lines.push(String::new());
            lines.extend(held_lines(asked, &venue.held));
        }
        None => {
            lines.push(String::new());
            lines.push(format!(
                "the datahub at {} declares no history channels for `{}` — its build's roster does \
                 not carry the venue, which says nothing about what the venue offers.",
                asked.addr, row.name
            ));
        }
    }
    for note in asked_notes(row, asked) {
        lines.push(String::new());
        lines.push(note);
    }
    lines
}

/// `show VENUE --addr A --json`: [`show_json`]'s document with `channels` and `held` from the asked
/// datahub, and a `datahub` object saying who answered and whether the server or this binary's
/// own table supplied the rows. `verified_against_the_vendor` stays a hard `false`: a datahub is
/// not a vendor.
fn asked_json(row: &Row, asked: &Asked) -> String {
    let venue = asked.report.venue(&row.name);
    let doc = serde_json::json!({
        "source": row.name,
        "state": row.state.as_str(),
        "reaches": row.reaches,
        "cost": row.cost,
        "venue_token": row.venue_token,
        "verified_against_the_vendor": false,
        "holds": Vec::<serde_json::Value>::new(),
        "channels_declared": venue.is_some(),
        "channels_as_of": epoch_ms_to_utc_date(asked.report.as_of_ms),
        "datahub": {
            "addr": asked.addr,
            "served": asked.served,
            "caption": (!asked.served).then_some(COMPILED_TABLE_CAPTION),
        },
        "channels": venue.map(|v| v.channels.clone()).unwrap_or_default(),
        "held": venue.filter(|_| asked.served).map(|v| v.held.clone()),
        "notes": asked_notes(row, asked),
    });
    serde_json::to_string_pretty(&doc).expect("a tree of strings, numbers and bools")
}

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

/// The refusal `--addr` gets on `ls`, worded once.
///
/// ⚠ It is a REFUSAL rather than a silent ignore, for `super::refuse_foreign_flags`'s reason one
/// level up: every flag that function guards is one an operator typed because a SIBLING verb takes
/// it, so "unknown option" would be a lie. `data hist ls --addr` is a real line; this one is not,
/// and the message names the reach `ls` does not have and the verbs that do.
///
/// ⚠ **It covered BOTH verbs until the history-channels read landed** — `show` reached no server
/// either. The owner's Q3 answer of 2026-10-02
/// (`docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §7) gave `show VENUE` an
/// opt-in `--addr`, so the refusal narrowed to the verb that still reaches nothing.
const ADDR_REFUSAL: &str = "--addr does not apply to `data source ls` — it renders the axis this \
                            binary was BUILT with and reaches no server, so there is nothing for \
                            an address to select. `data source show VENUE --addr A` asks one \
                            datahub for a venue's history channels; the other verbs that ask one \
                            are under `data hist`.";

/// What `show NAME --addr A` is told when NAME is not a ROSTER venue: the address selects a
/// datahub to ask about a venue's history channels, and a source name or an unclassified venue
/// token has none to ask about.
fn addr_needs_a_roster_venue(name: &str) -> String {
    format!(
        "--addr asks a datahub for a ROSTER VENUE's history channels, and `{name}` is not one — \
         a source name or a venue this build has never classified has no channels to ask about. \
         Drop --addr to describe `{name}` locally."
    )
}

/// The refusal any OTHER flag gets — and it deliberately does not say "unknown".
///
/// ⚠ **[`ADDR_REFUSAL`] applied its own stated rule to exactly one flag, and this is the fix.**
/// `--store`, `--engine`, `--days`, `--from`/`--to`, `--venue`, `--kind` and `--source` are all
/// real, documented `data hist` flags, and every one of them fell through to
/// `unknown option '--store'` — which is, by the standard that const cites, a lie, and one that
/// sends an operator to check the spelling of a flag they typed correctly for a sibling.
///
/// ⚠ **It answered EVERY `--` token, so a TYPO was told it was spelt correctly.** The first version
/// of this said, unconditionally, *"If you typed `{flag}` for `data hist`, it is spelt correctly and
/// belongs there"* — so `data source ls --stroe /srv/vike/data` replied that `--stroe` is a correct
/// `data hist` flag. It is not a flag anywhere. That is the same defect the const above was written
/// to fix, wearing the other face: the old code lied by calling a real flag unknown, and the fix
/// lied by calling an unknown flag real.
///
/// So the claim is now made only for a flag `data hist` ACTUALLY takes, and the set is DERIVED
/// rather than typed — see [`is_a_hist_flag`]. Anything else gets the plain refusal, with this
/// group's own options printed under it by `crate::cmd::args::exit_for_parse_error`.
fn foreign_flag_refusal(flag: &str) -> String {
    if is_a_hist_flag(flag) {
        return format!(
            "`{flag}` is not a `data source` flag — the options this group takes are listed \
             below. ⚠ That is not a misspelling: `{flag}` is a real `data hist` flag, and it \
             means nothing to a group whose two verbs open no socket and read no store. Typed \
             for `data hist`, it belongs there."
        );
    }
    format!(
        "`{flag}` is not a `data source` flag, and it is not a `data hist` flag either — the \
         options this group takes are listed below."
    )
}

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

/// Parse a `data source …` line. `argv` is everything AFTER the group word.
fn parse(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        // DERIVED from [`VERBS`], for the reason that const carries.
        return Err(format!("`data source` needs a verb ({})", VERBS.join(" | ")));
    };
    let verb = match first.as_str() {
        "ls" => Verb::Ls,
        "show" => Verb::Show,
        "-h" | "--help" | "help" => return help_requested(),
        other => {
            return Err(format!("unknown `data source` verb '{other}' ({})", VERBS.join(" | ")));
        }
    };

    let mut name: Option<String> = None;
    let mut json_flag = false;
    let mut format: Option<Format> = None;
    let mut addr: Option<String> = None;

    let mut flags = Flags::new(argv[1..].iter().cloned());
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" if verb == Verb::Ls => return Err(ADDR_REFUSAL.to_string()),
            "--addr" => {
                let value = flags.value(&flag, inline)?;
                if value.trim().is_empty() {
                    return Err("--addr was given an EMPTY value, so it names no datahub. Pass \
                                HOST:PORT, or drop --addr to describe the venue locally"
                        .to_string());
                }
                addr = Some(value);
            }
            "--json" => {
                no_value(&flag, inline)?;
                json_flag = true;
            }
            "--format" => format = Some(parse_format(&flags.value(&flag, inline)?)?),
            "-h" | "--help" => return help_requested(),
            // The `--` rule `crate::cmd::args`'s `is_flag_token` spells for this whole crate.
            other if other.starts_with("--") => return Err(foreign_flag_refusal(other)),
            positional => {
                // ⚠ `Flags::next_flag` splits EVERY token on its first `=`, which is right for a
                // FLAG and wrong for a positional: `show a=b` arrives as `("a", Some("b"))`, and
                // binding `a` discarded `b` in SILENCE — two answers for one value, from the group
                // whose stated job is that `show X` and `--source X` agree about X. Reassembling
                // is what makes that true for a value carrying an `=`.
                let token = match &inline {
                    Some(rest) => format!("{positional}={rest}"),
                    None => positional.to_string(),
                };
                match (&name, verb) {
                    (_, Verb::Ls) => {
                        return Err(format!(
                            "`data source ls` takes no argument, and got '{token}'. It lists \
                             every source; `data source show {token}` describes one"
                        ));
                    }
                    (None, Verb::Show) => name = Some(token),
                    (Some(already), Verb::Show) => {
                        return Err(format!(
                            "unexpected extra argument '{token}' (the source is already \
                             '{already}'); one source per `show`"
                        ));
                    }
                }
            }
        }
    }

    // ⚠ RESOLVED HERE, not at render time — so a value this group cannot describe is refused on the
    // USAGE rung rather than rendered as a working venue row. See [`resolve`].
    //
    // ⚠ An EMPTY positional is a MISSING name, not a source named "", and it gets
    // [`SHOW_NEEDS_A_NAME`] for that reason: `show ""` and `show` are one mistake, so they owe one
    // sentence. It used to fall through to [`resolve`] and print the AXIS's empty-value refusal,
    // which tells the operator to "omit the flag" — on the one rung that takes no flag, where
    // omitting the argument answers with this const instead. [`resolve`]'s doc carries the rest.
    let row = match (verb, name) {
        (Verb::Show, Some(n)) if !n.is_empty() => Some(resolve(&n)?),
        (Verb::Show, _) => return Err(SHOW_NEEDS_A_NAME.to_string()),
        (Verb::Ls, _) => None,
    };

    // ONE axis, two spellings, and the CONTRADICTION is refused rather than resolved — the rule
    // `crate::cmd::data`'s `parse` already follows.
    //
    // ⚠ The sentence below is a COPY of that function's, and
    // `the_output_axis_refuses_the_same_pair_the_hist_group_refuses` holds the two equal after
    // whitespace normalisation. Sharing it outright would mean editing that function, which two
    // sibling branches are inside; pinning two copies equal from a test is the seam
    // `crates/vike-bridge-core/tests/settings_dir_spellings.rs` already uses for a duplication that
    // could not be removed either.
    let json = match (format, json_flag) {
        (Some(Format::Table), true) => {
            return Err("--json and --format table ask for two different renderings. `--json` IS \
                        `--format json` — pass one"
                .to_string());
        }
        (Some(f), _) => f == Format::Json,
        (None, given) => given,
    };

    // `--addr` asks a datahub about a ROSTER venue's channels; any other row has none to ask about.
    if addr.is_some()
        && let Some(r) = row.as_ref().filter(|r| !r.declared)
    {
        return Err(addr_needs_a_roster_venue(&r.name));
    }

    Ok(Args { verb, row, json, addr })
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
