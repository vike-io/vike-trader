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
//! | `show NAME` | NOTHING | what that source holds, and what would fetch it if you used it |
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
//! `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`, the declared authority for that roster.
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

use std::process::ExitCode;

use vike_node_proto::auth::NodeKeys;

use super::{Format, Source, UNBUILT_SOURCES, col, parse_format};
use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};

/// This group's own usage roster, in `crate::cmd::data`'s `USAGE` style — a group owns its own
/// grammar, so it owns the page that documents it.
const USAGE: &str = "\
usage: vike-cli data source <verb> [options]

WHERE rows may come from — the `--source` axis `data hist fetch` takes, as a roster you can
read instead of a value you have to guess. BOTH verbs are LOCAL: they open no socket, ask no
datahub and read nothing from any vendor, so --addr is refused by name rather than ignored.

  ls           every source, its STATE (built | designed), what it is REACHED BY, and what it
               COSTS. The `designed` rows are the ones `--source` refuses by name today, and
               their COST cell is what each is WAITING ON
  show NAME    what that source holds, and what would fetch it — read off the axis this BINARY
               was built with, never probed. NAME is any `--source` value: a built one, a
               designed one, or a venue token — an unrecognised NAME is taken as a VENUE, which
               is exactly what `--source` itself does with it

options:
  --format F   HOW the answer is rendered: `table` (the default) or `json`. The same axis and
               the same refusals the `data hist` CATALOG verbs carry, reached through the same
               parser — `jsonl` is served by `data hist get`, the verb that emits ROWS, and is
               refused here because a roster is not rows; `csv`/`parquet` are FILE formats,
               written by `data hist export --out FILE`. Each is refused by name with its
               own reason
  --json       shorthand for --format json. For the roster: one object per source. For one row:
               the same fields plus what it holds. BOTH documents carry
               verified_against_the_vendor, which is FALSE and stays false until a vendor read
               exists — a machine reader must never take either for a probe
  -h, --help   this message

⚠ NOTHING here is verified against a server or a vendor. data.vike.io's archive is keyed and
  its manifest is not, so `what exists` is answerable without a subscription — but this binary
  links no HTTP client and does not ask. Every line these verbs print is a LOCAL description of
  a lane, never a probe of what your key reaches. See
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
        Source::Venue => (
            VENUE_TOKEN,
            "no credentials — public market data, pulled by the datahub you point at",
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
    Ok(Row { name: name.to_string(), ..built_row(source) })
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
     source holds, and what its value does at the axis. It reaches nothing either — neither verb \
     does.",
];

/// The line EVERY answer ends on, and the point of this phase's shape.
///
/// ⚠ It is unconditional rather than reserved for `vike`, and that is the stronger claim: a reader
/// who met it only on the row that HAS a live manifest would reasonably read its absence elsewhere
/// as "this one was checked". Nothing here is checked.
const NOT_VERIFIED: &str = "⚠ nothing above was read from a server or from a vendor. These verbs \
                            reach nothing: they describe the sources this BINARY was built with, \
                            so no line here is a probe of what your box, your network or your key \
                            actually reaches.";

/// The two classes §9.2 separates — the correction that section exists to make.
///
/// ⚠ Class 2 is not an instrument's tape, and rendering it as one is the mistake: `venue` on the
/// positioning series is the exchange whose POSITIONS were graded, never the metrics service that
/// graded them.
///
/// ⚠ **No `kind=` is named here, and the module doc carries why**: that roster is
/// `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`, a table this crate cannot link and
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
    notes.push(NOT_VERIFIED.to_string());
    notes
}

/// `show NAME`. Pure, for [`ls_lines`]'s reason — and it takes the RESOLVED row rather than a
/// string, so there is no name left for this function to invent a default for.
fn show_lines(row: &Row) -> Vec<String> {
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
fn show_json(row: &Row) -> String {
    let holds: Vec<serde_json::Value> = if row.name == VIKE {
        VIKE_CLASSES
            .iter()
            .map(|(class, what)| serde_json::json!({ "class": class, "what": what }))
            .collect()
    } else {
        Vec::new()
    };
    let doc = serde_json::json!({
        "source": row.name,
        "state": row.state.as_str(),
        "reaches": row.reaches,
        "cost": row.cost,
        "venue_token": row.venue_token,
        "verified_against_the_vendor": false,
        "holds": holds,
        "notes": show_notes(row),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, bools and nulls; serialization is total")
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

/// The refusal `--addr` gets here, worded once.
///
/// ⚠ It is a REFUSAL rather than a silent ignore, for `super::refuse_foreign_flags`'s reason one
/// level up: every flag that function guards is one an operator typed because a SIBLING verb takes
/// it, so "unknown option" would be a lie. `data hist ls --addr` is a real line; this one is not,
/// and the message names the reach this group does not have and the verbs that do.
const ADDR_REFUSAL: &str = "--addr does not apply to `data source` — this group reaches no server. \
                            `ls` renders the axis this binary was BUILT with and `show` describes \
                            one row of it; neither asks a datahub anything, so there is nothing \
                            for an address to select. The verbs that ask one are under `data hist`.";

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

    let mut flags = Flags::new(argv[1..].iter().cloned());
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" => return Err(ADDR_REFUSAL.to_string()),
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

    Ok(Args { verb, row, json })
}

/// Run a `data source …` line. `argv` is everything AFTER the group word.
///
/// ⚠ `keys` and `configured_addr` are taken and IGNORED, and the underscores are the whole story:
/// the group layer hands every group the same three arguments so one dispatch arm serves all of
/// them, and this group reaches no server — see [`ADDR_REFUSAL`]. A verb here that did open a
/// socket would take them; none does, and a signature that dropped them would make this module the
/// odd one out at a seam no reader of `crate::cmd::data`'s `run` could see into.
pub(super) fn run(
    argv: &[String],
    _keys: Option<&NodeKeys>,
    _configured_addr: Option<&str>,
) -> ExitCode {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("data source", USAGE, &msg),
    };
    let text = match (args.verb, args.row.as_ref()) {
        (Verb::Ls, _) if args.json => ls_json(&rows()),
        (Verb::Ls, _) => ls_lines(&rows()).join("\n"),
        (Verb::Show, Some(row)) if args.json => show_json(row),
        (Verb::Show, Some(row)) => show_lines(row).join("\n"),
        // Unreachable by construction — [`parse`] refuses a `show` with no NAME — and it REFUSES
        // rather than defaulting, with the same sentence, because the `unwrap_or_default()` this
        // replaced is exactly how `show ""` came to render a working venue row.
        (Verb::Show, None) => return exit_for_parse_error("data source", USAGE, SHOW_NEEDS_A_NAME),
    };
    println!("{text}");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    fn ls_text() -> String {
        ls_lines(&rows()).join("\n")
    }

    fn ls_doc() -> serde_json::Value {
        serde_json::from_str(&ls_json(&rows())).expect("ls --json is one document")
    }

    fn row_of(name: &str) -> Row {
        resolve(name).unwrap_or_else(|e| panic!("`{name}` must resolve: {e}"))
    }

    fn show_text(name: &str) -> String {
        show_lines(&row_of(name)).join("\n")
    }

    fn show_doc(name: &str) -> serde_json::Value {
        serde_json::from_str(&show_json(&row_of(name))).expect("show --json is one document")
    }

    /// Collapse every run of whitespace to one space. Used where a message's EXACT spacing is not
    /// the property under test — see
    /// `the_output_axis_refuses_the_same_pair_the_hist_group_refuses`.
    fn squeeze(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The [`USAGE`] ROW a token heads, or `None` — the smallest unit that carries "this token is
    /// DISCOVERABLE".
    ///
    /// ⚠ It exists because a `contains` over the whole page cannot fail for that reason: `ls` is a
    /// substring of `jsonl` in the `--format` row, of `` `ls` `` in the `--json` prose and of the
    /// word `false`, so the page satisfies `USAGE.contains("ls")` with the `ls` ROW deleted. A row
    /// is found by its HEAD — the token at the start of an indented line, followed by space or by
    /// the comma in `-h, --help` — which only that row can satisfy.
    fn usage_row(token: &str) -> Option<&'static str> {
        USAGE.lines().map(str::trim_start).find(|line| {
            line.strip_prefix(token)
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace() || c == ','))
        })
    }

    /// **THE DERIVATION.** Every row of `UNBUILT_SOURCES` reaches the listing carrying its own WHY,
    /// and every built source reaches it by the name `--source` takes. A row added to either
    /// declaration is rendered by [`rows`] with no edit here, so this test fails on the one thing
    /// that could go wrong: somebody re-typing the roster in this module and letting the copies
    /// drift.
    ///
    /// ⚠ Paired with an anti-vacuity control, because "every X appears in a long string" passes
    /// trivially when the string is long enough: a name in NEITHER declaration must be ABSENT,
    /// which also pins that the listing is not quietly printing a venue roster.
    #[test]
    fn the_listing_is_derived_from_both_declarations() {
        let text = ls_text();
        for (name, why) in UNBUILT_SOURCES {
            assert!(text.contains(name), "`ls` must name the designed source `{name}`: {text}");
            assert!(text.contains(why), "…with what `{name}` is waiting on: {text}");
        }
        for source in SOURCES {
            let row = built_row(*source);
            assert!(text.contains(&row.name), "`ls` must name `{}`: {text}", row.name);
            assert!(text.contains(row.cost), "…with what `{}` costs: {text}", row.name);
        }
        assert!(
            !text.contains("binance"),
            "a venue is not a source row — the token class is `{VENUE_TOKEN}`: {text}"
        );
    }

    /// **THE ROUND TRIP.** Every built row is named by the spelling `super::parse_source` actually
    /// accepts, so the listing cannot advertise a value the axis refuses.
    ///
    /// ⚠ The control matters more than the assertion: `parse_source` answers `Venue` for
    /// EVERYTHING it does not recognise, so a round trip alone would pass on a misspelt `startr`.
    /// The second half pins that the two named sources do NOT collapse into that fallback.
    #[test]
    fn every_built_source_round_trips_through_the_parser() {
        for source in SOURCES {
            let row = built_row(*source);
            assert_eq!(
                super::super::parse_source(&row.name),
                Ok(*source),
                "`{}` must be the spelling the axis takes",
                row.name
            );
        }
        assert_eq!(super::super::parse_source("starter"), Ok(Source::Starter));
        assert_eq!(super::super::parse_source("demo"), Ok(Source::Demo));
        assert_eq!(
            super::super::parse_source("startr"),
            Ok(Source::Venue),
            "an unrecognised value is a venue, which is why the control above is needed"
        );
    }

    /// The BUILT half lists no source twice — the one direction of [`SOURCES`]' residual that is
    /// checkable at all. That const's doc carries the direction that is not, and why stable Rust
    /// offers no gate for it.
    #[test]
    fn the_built_half_lists_no_source_twice() {
        let names: Vec<String> = SOURCES.iter().copied().map(|s| built_row(s).name).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "a source is listed twice: {names:?}");
    }

    /// **EVERY `Source` VARIANT IS IN [`SOURCES`]**, held by an EXHAUSTIVE MATCH rather than by a
    /// count.
    ///
    /// ⚠ This is the hole the review left open and the comment in [`built_row`] admitted: the
    /// compiler demands an arm there when a variant lands, but nothing demanded a `SOURCES` row —
    /// so a fourth source would be accepted by the axis, absent from `ls`, absent from `ls --json`,
    /// and green in every test here, because all of them iterate `SOURCES` and would simply never
    /// see it. `built_row`'s note asked the author to remember; this asks the COMPILER.
    ///
    /// The mechanism is the match below, which has no `_` arm. A new variant does not fail this
    /// test — it fails to BUILD it, at the line that names the roster, which is the one place the
    /// author can fix it. A length assertion would have been the wrong tool twice over: it passes
    /// while naming nothing, and it is the exact shape (`[T; N]` vs the rows it counts) that has
    /// merged wrong in this repository before.
    #[test]
    fn every_source_variant_is_in_the_roster() {
        fn token(s: Source) -> &'static str {
            match s {
                Source::Venue => "venue",
                Source::Starter => "starter",
                Source::Demo => "demo",
            }
        }
        for s in [Source::Venue, Source::Starter, Source::Demo] {
            assert!(
                SOURCES.contains(&s),
                "`Source::{:?}` has a `built_row` arm and no SOURCES row, so `data source ls` says \
                 it does not exist while `--source {}` accepts it",
                s,
                token(s)
            );
        }
        // ...and the control: the roster names nothing the enum does not, so the check above
        // cannot be passing because `SOURCES` simply holds everything.
        assert_eq!(SOURCES.len(), 3, "the roster grew without this test being told: {SOURCES:?}");
    }

    /// A designed source is never rendered as reachable, in either form: `reaches` is `null` in the
    /// document and the cell reads `nothing yet` in the table — and the STATE agrees with what the
    /// axis actually does, which is refuse the value by name.
    #[test]
    fn a_designed_source_is_never_rendered_as_reachable() {
        for (name, _) in UNBUILT_SOURCES {
            let row = row_of(name);
            assert_eq!(row.state, State::Designed, "{name}");
            assert_eq!(row.reaches, None, "{name}");
            assert!(
                super::super::parse_source(name).is_err(),
                "`ls` calls `{name}` designed, so the axis must refuse it"
            );
        }
        for source in SOURCES {
            let row = built_row(*source);
            assert_eq!(row.state, State::Built, "{}", row.name);
            assert!(row.reaches.is_some(), "{}", row.name);
        }
    }

    /// The TRANSPORT column is read out of `Source::engine_verb` rather than restated — so the two
    /// engine-verb sources and the one datahub source land in different cells, and a source that
    /// changed transport would move without an edit in this module.
    #[test]
    fn the_transport_cell_follows_the_engine_verb_seam() {
        assert_eq!(reaches(Source::Venue), "a datahub");
        assert_eq!(reaches(Source::Starter), reaches(Source::Demo));
        assert_ne!(
            reaches(Source::Venue),
            reaches(Source::Starter),
            "the split is the point: one asks a server, the other spawns a child"
        );
    }

    /// **THE BOUNDARY, ON BOTH VERBS.** Every `show` — built, designed and venue-token alike — and
    /// `ls` in both its renderings say that nothing was verified, and each document says it as a
    /// testable FIELD rather than as prose.
    ///
    /// ⚠ **`ls` was not covered and did not say it**, while this module's doc claimed every answer
    /// did. [`NOT_VERIFIED`] was pushed by [`show_lines`] alone and `verified_against_the_vendor`
    /// appeared in [`show_json`] alone, so the verb that prints a column headed REACHES — `a
    /// datahub`, `the engine, on this box` — carried no disclaimer anywhere, and a wrapper folding
    /// `ls --json` could read `"reaches": "the engine, on this box"` as a probe of the box it was
    /// running on.
    #[test]
    fn every_answer_in_this_group_says_that_nothing_was_verified() {
        let mut names: Vec<String> = SOURCES.iter().map(|s| built_row(*s).name).collect();
        names.extend(UNBUILT_SOURCES.iter().map(|(n, _)| (*n).to_string()));
        // ...and a name in neither declaration, which is the venue-token path.
        names.push("binance".to_string());
        for name in &names {
            let text = show_text(name);
            assert!(text.contains(NOT_VERIFIED), "`show {name}` must name the limit: {text}");
            assert_eq!(
                show_doc(name)["verified_against_the_vendor"],
                serde_json::Value::Bool(false),
                "`show {name} --json` must say so as a FIELD"
            );
        }
        assert!(ls_text().contains(NOT_VERIFIED), "`ls` must name it too: {}", ls_text());
        assert_eq!(
            ls_doc()["verified_against_the_vendor"],
            serde_json::Value::Bool(false),
            "`ls --json` must carry the same FIELD the other verb carries: {}",
            ls_doc()
        );
        // The control: the constant is not the empty string, which would make every assertion
        // above pass against any output at all.
        assert!(NOT_VERIFIED.len() > 40, "the note must actually say something");
    }

    /// **ONE FIELD NAME, ONE KIND OF DOCUMENT.** `notes` is a list of FOOTNOTE SENTENCES in both
    /// verbs, and every note a document carries is printed by the same verb's table.
    ///
    /// ⚠ **The correction.** `show --json` used to set `notes` to the whole human rendering —
    /// `source:   vike`, `state:    designed`, the empty strings where the table has blank lines —
    /// while `ls --json` set the same field to three footnotes. So the document re-encoded as prose
    /// every structured field it already carried, and a consumer that learned `notes` from one verb
    /// read something categorically different from the other. Nothing pinned it: neither the unit
    /// test nor the integration test asserted anything about `notes` on `show`.
    #[test]
    fn notes_means_footnotes_in_both_verbs_and_each_table_prints_its_own() {
        let ls_table = ls_text();
        let listing = ls_doc();
        let notes = listing["notes"].as_array().expect("`ls --json` carries notes");
        assert!(!notes.is_empty(), "…and they are not an empty array: {listing}");
        for note in notes {
            let note = note.as_str().expect("a note is a sentence");
            assert!(!note.is_empty(), "a blank line is not a note");
            assert!(ls_table.contains(note), "`ls` must print the note it documents: {note}");
        }

        for name in ["vike", "demo", "binance"] {
            let text = show_text(name);
            let doc = show_doc(name);
            let notes = doc["notes"].as_array().expect("`show --json` carries notes");
            assert!(!notes.is_empty(), "…and they are not an empty array: {doc}");
            for note in notes {
                let note = note.as_str().expect("a note is a sentence");
                assert!(!note.is_empty(), "a blank line is not a note: {doc}");
                assert!(text.contains(note), "`show {name}` must print it: {note}");
                // THE PROPERTY: a note is a footnote, never a re-encoding of a cell this document
                // already carries as a FIELD. These four prefixes are the rendering's own cells.
                for cell in ["source:", "state:", "reaches:", "cost:"] {
                    assert!(
                        !note.starts_with(cell),
                        "`{note}` is the RENDERING of `{cell}`, which the document carries as a \
                         field — see this test's doc for what that cost"
                    );
                }
            }
        }
    }

    /// **THE OTHER DIRECTION, WHICH WAS GATED BY NOTHING.**
    /// `notes_means_footnotes_in_both_verbs_and_each_table_prints_its_own` walks the DOCUMENT and
    /// asks the table to print each note, so it holds document ⊆ table. This one walks the TABLE's
    /// FOOTER and asks the document to carry each line, which is the containment that was open: a
    /// footnote appended to [`ls_lines`] alone — a per-row caveat under the table, say — left
    /// `ls --json`'s `notes` array short, a wrapper rendering the document showed an operator fewer
    /// footnotes than the table did, and both tests stayed green.
    ///
    /// ⚠ The wiring is the real fix and this is its gate: [`ls_lines`] now renders [`ls_notes`]
    /// whole rather than re-spelling the chain, so the two sets are one by construction. The test
    /// is what keeps that true after the next edit, because the re-spelling compiled perfectly and
    /// read as tidy code.
    #[test]
    fn the_table_prints_no_footnote_the_document_omits() {
        let derived = ls_notes();
        let table_rows = rows();
        let lines = ls_lines(&table_rows);

        // The document IS the derivation, in order.
        let documented: Vec<String> = ls_doc()["notes"]
            .as_array()
            .expect("`ls --json` carries notes")
            .iter()
            .map(|n| n.as_str().expect("a note is a sentence").to_string())
            .collect();
        assert_eq!(documented, derived, "the document must render `ls_notes` and nothing else");

        // …and so is the FOOTER: one header line, one line per row, a blank, then footnotes only.
        assert_eq!(lines[table_rows.len() + 1], "", "the blank that ends the table: {lines:?}");
        // ⚠ **A SEQUENCE, not a membership loop plus a COUNT.** This was
        // `assert!(derived.contains(&note))` per line followed by
        // `assert_eq!(printed, derived.len(), "…and every derived footnote is printed once")`, and
        // that pair does not check what the message says: a footer printing ONE note twice and
        // omitting another satisfies both halves, because every printed line is still in `derived`
        // and the tally still matches. Comparing the stripped footer to [`ls_notes`] directly earns
        // the sentence — containment BOTH ways, the multiplicity and the order, in one assertion.
        // (The omitted direction was covered next door by
        // `notes_means_footnotes_in_both_verbs_and_each_table_prints_its_own`, so nothing was
        // actually open; what was wrong was a message claiming more than its assertion, which is
        // how that neighbour gets deleted as redundant.)
        let printed: Vec<&str> = lines[table_rows.len() + 2..]
            .iter()
            .filter(|l| !l.is_empty())
            .map(|l| l.strip_prefix("note: ").unwrap_or(l.as_str()))
            .collect();
        assert_eq!(
            printed, derived,
            "the footer IS `ls_notes`, rendered whole and in order — every derived footnote \
             printed once, and nothing printed that `ls --json` omits: {lines:?}"
        );

        // The controls: the derivation is not empty, so the loop above is not passing vacuously,
        // and it is strictly WIDER than [`LS_NOTES`] — the closer is in it, which is the note that
        // was appended at the call site rather than derived.
        assert_eq!(derived.len(), LS_NOTES.len() + 1, "{derived:?}");
        assert!(derived.contains(&NOT_VERIFIED), "{derived:?}");
    }

    /// `show vike` keeps §9.2's two classes apart, states the keyed/keyless split, prints no URL,
    /// and states the licensing constraint outright.
    ///
    /// ⚠ The licence assertion is the one that is not decoration. Class 1 names a venue axis and
    /// class 2 an exchange, so a reader could infer an offer of exchange candles from the table
    /// alone; the ruling of 2026-09-21 is that there is none, and this pins the sentence that says
    /// so into the OUTPUT rather than into a comment.
    #[test]
    fn show_vike_carries_two_classes_the_keyed_split_and_the_licence() {
        let text = show_text(VIKE);
        for (class, what) in VIKE_CLASSES {
            assert!(text.contains(class), "`show vike` must name the `{class}` class: {text}");
            assert!(text.contains(what), "…and what it is: {text}");
        }
        assert!(text.contains("NO CEX market data"), "the ruling must be stated: {text}");
        assert!(text.contains(VIKE_KEYS), "the keyed/keyless split is this source's property");
        assert!(
            !text.contains("https://"),
            "no base is resolved here, so none may be printed: {text}"
        );
        assert_eq!(
            show_doc(VIKE)["holds"].as_array().map(Vec::len),
            Some(VIKE_CLASSES.len()),
            "the document carries the classes SEPARATELY, which is §9.2's whole correction"
        );

        // The control: no other source grows a `holds` array, so the assertion above is about this
        // row rather than about every row.
        assert_eq!(show_doc("demo")["holds"].as_array().map(Vec::len), Some(0));
    }

    /// **NO STORE-KIND ROSTER LIVES IN THIS CRATE.** [`VIKE_CLASSES`] used to end each class on
    /// `Lands as kind=book / trade / quote` and `kind=cohort / perp_metrics` — five names copied
    /// out of `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`, the declared authority, in the
    /// crate whose own `rm` and `repair` arms say a kind roster "copied into this crate would be a
    /// second list to keep in step". Nothing compared the two, so a rename on that side would have
    /// left this verb advertising a kind the far side refuses — and being unable to derive the list
    /// is an argument for not printing it, never for typing it.
    #[test]
    fn the_class_descriptions_name_no_store_kind() {
        for (class, what) in VIKE_CLASSES {
            assert!(
                !what.contains("kind="),
                "`{class}` names a store kind: {what} — that roster belongs to `vike-data`, which \
                 this crate cannot link, so it may not be copied here"
            );
        }
        // The control: the descriptions still SAY something, so the assertion above is not passing
        // on an empty table.
        assert!(VIKE_CLASSES.iter().all(|(_, what)| what.len() > 40), "{VIKE_CLASSES:?}");
    }

    /// The one special case is anchored to a DECLARED row, so renaming or BUILDING `vike` reddens
    /// this module instead of silently turning the expansion off.
    #[test]
    fn the_special_cased_name_is_still_a_declared_row() {
        assert!(
            UNBUILT_SOURCES.iter().any(|(name, _)| *name == VIKE),
            "`{VIKE}` must still be a row of UNBUILT_SOURCES — if it was BUILT, its expansion \
             belongs on the built row instead"
        );
    }

    /// An unknown name is a VENUE token, exactly as `--source` takes it — and `show` says that is
    /// what happened rather than implying the venue was judged.
    #[test]
    fn show_of_an_unknown_name_is_a_venue_token_and_says_so() {
        let row = row_of("binance");
        assert!(row.venue_token);
        assert_eq!(row.state, State::Built, "the venue lane works today");
        let text = show_text("binance");
        assert!(text.contains("taken as a VENUE token"), "{text}");
        assert!(text.contains("nothing here asked it"), "the limit, again: {text}");
        // The control: the token-class row itself does NOT carry that sentence — nothing was
        // substituted, because it IS the class.
        assert!(!show_text(VENUE_TOKEN).contains("taken as a VENUE token"));
    }

    /// **ONE MISTAKE, ONE REFUSAL, WORDED FOR THE RUNG THAT PRINTS IT.** `show ""` and `show` are
    /// both "no name was given", so they answer with the same sentence — and an operator who obeys
    /// it by dropping the empty argument meets that sentence again rather than a different one.
    ///
    /// ⚠ **Two corrections, in order.** `data source show ""` first exited 0 and printed `source:`
    /// blank, `state: built`, `reaches: a datahub` and "the same answer `--source ` gets", which
    /// was false — `data hist fetch --source ""` is refused on the usage rung. The fix made this
    /// rung print the AXIS's own empty-value sentence byte for byte, and that sentence is written
    /// for a FLAG: *"Omit the flag to use a venue"*. `show` takes no flag, and omitting the
    /// argument answered with [`SHOW_NEEDS_A_NAME`] — two refusals for one mistake, the first of
    /// them wrong about what the operator typed.
    ///
    /// What the byte-identity was BUYING is kept and asserted below: this group is still not a
    /// looser grammar than the axis it documents. The empty value is refused on the same USAGE
    /// rung, [`resolve`] still refuses it, and `super::parse_source` still refuses it — the two
    /// sides agree on the VERDICT, which is the property, and each states it in its own rung's
    /// terms, which is the correction.
    #[test]
    fn an_empty_name_is_one_refusal_written_for_the_rung_that_prints_it() {
        let empty = parse_of(&["show", ""]).unwrap_err();
        let missing = parse_of(&["show"]).unwrap_err();
        assert_eq!(
            empty, missing,
            "obeying the refusal must not produce a SECOND, different refusal"
        );
        assert_eq!(empty, SHOW_NEEDS_A_NAME, "…and it is the rung's own sentence");
        // THE PROPERTY the byte-identity broke: an instruction a reader can carry out HERE. The
        // axis's sentence names a flag this rung does not take.
        assert!(
            !empty.contains("Omit the flag"),
            "a positional rung may not tell an operator to omit a flag: {empty}"
        );

        // …and the VERDICT still matches the axis's, which is what the identity was for.
        assert!(super::super::parse_source("").is_err(), "the axis refuses an empty value");
        assert!(resolve("").is_err(), "…and so does the resolver, as the backstop below it");

        // THE CONTROLS, all three classes: the venue-token fallback, a built row and a DESIGNED
        // row — the last one because describing a value the axis refuses is this verb's whole job,
        // so `resolve` must refuse the empty value WITHOUT refusing the designed ones.
        assert!(resolve("binance").is_ok(), "an unrecognised name is a venue, not an error");
        assert!(resolve("demo").is_ok());
        for (name, _) in UNBUILT_SOURCES {
            assert!(resolve(name).is_ok(), "`{name}` is DESCRIBED here, never refused");
        }
        // …and `show NAME` still parses, so the assertions above are not passing because this verb
        // refuses everything.
        assert!(parse_of(&["show", "demo"]).is_ok());
    }

    /// A positional carrying an `=` reaches [`resolve`] WHOLE, so `show X` and `--source X`
    /// describe the same X.
    ///
    /// ⚠ `crate::cmd::args`'s `Flags::next_flag` splits every token on its first `=` — right for a
    /// FLAG, wrong for a positional. `show a=b` arrived as `("a", Some("b"))` and the positional
    /// arm bound `a`, discarding `b` with no error, so this group answered for `a` while the axis
    /// answered for `a=b`.
    #[test]
    fn a_positional_carrying_an_equals_sign_is_not_truncated() {
        let row = parse_of(&["show", "a=b"]).unwrap().row.expect("a resolved row");
        assert_eq!(row.name, "a=b", "the value was truncated at the `=`");
        // The shapes the split also manufactures: a trailing `=` and a leading one.
        assert_eq!(parse_of(&["show", "a="]).unwrap().row.expect("a row").name, "a=");
        assert_eq!(parse_of(&["show", "=b"]).unwrap().row.expect("a row").name, "=b");
        // ...and `ls`'s refusal echoes the whole token rather than half of it.
        assert!(parse_of(&["ls", "a=b"]).unwrap_err().contains("'a=b'"));
        // THE CONTROL: the FLAG form still splits, which is what `next_flag` is for.
        assert!(parse_of(&["ls", "--format=json"]).unwrap().json);
    }

    /// A `designed` row's `show` says what the axis DOES take, so learning it costs no round trip
    /// through a refusal — and it explains what its COST cell means, which only `ls` used to.
    ///
    /// ⚠ It used to print `state: designed` and the cost cell and stop. An operator reading
    /// `cost: the paid crypto L2 archive — feature-gated at module AND bin, and keyed (P4)` had to
    /// type `--source tardis` and read the refusal to learn what IS usable, which is exactly the
    /// round trip this group exists to remove.
    #[test]
    fn a_designed_row_says_what_the_axis_takes_instead() {
        for (name, _) in UNBUILT_SOURCES {
            let text = show_text(name);
            assert!(
                text.contains(&format!("`--source {name}` is REFUSED")),
                "`show {name}` must say the axis refuses it: {text}"
            );
            for source in SOURCES {
                let built = built_row(*source).name;
                assert!(text.contains(&built), "…and name `{built}`, which works: {text}");
            }
            assert!(text.contains(DESIGNED_COST), "…and what the COST cell means here: {text}");
        }
        // THE CONTROL: a BUILT row carries none of it — there is nothing to redirect from, and a
        // note that fired on every row would stop being read.
        let demo = show_text("demo");
        assert!(!demo.contains("is REFUSED"), "{demo}");
        assert!(!demo.contains(DESIGNED_COST), "{demo}");
    }

    /// `--addr` is refused BY NAME, with the reason and with the verbs that do take one. Nothing
    /// here opens a socket, so accepting it would advertise a reach this group does not have.
    #[test]
    fn the_addr_flag_is_refused_by_name() {
        for args in [vec!["ls", "--addr", "1.2.3.4:9"], vec!["show", "vike", "--addr=1.2.3.4:9"]] {
            let err = parse_of(&args).unwrap_err();
            assert!(err.contains("--addr"), "{err}");
            assert!(err.contains("no server"), "the refusal must say WHY: {err}");
            assert!(err.contains("data hist"), "…and what does take one: {err}");
        }
        // The control: another flag gets a DIFFERENT answer, so the assertions above are not
        // passing because every flag is refused identically.
        let err = parse_of(&["ls", "--nope"]).unwrap_err();
        assert!(err.contains("not a `data source` flag"), "{err}");
        assert!(!err.contains("no server"), "{err}");
    }

    /// **A SIBLING GROUP'S FLAG IS NOT "UNKNOWN".** [`ADDR_REFUSAL`] cites
    /// `super::refuse_foreign_flags`'s rule — a flag an operator typed because a SIBLING verb takes
    /// it is not unknown, so saying so would be a lie — and this module applied it to `--addr`
    /// alone. `--store`, `--engine`, `--days`, `--from`/`--to`, `--venue`, `--kind` and `--source`
    /// are every one a real `data hist` flag, and every one landed on `unknown option '--store'`,
    /// which sent an operator to check a spelling that was right.
    ///
    /// The flags below are EXAMPLES of that class rather than a roster: [`foreign_flag_refusal`]
    /// answers for every `--` token, which is why this file writes no list of another group's
    /// flags — see that function's doc.
    #[test]
    fn a_sibling_groups_flag_is_not_called_unknown() {
        for flag in ["--store", "--engine", "--days", "--source"] {
            let err = parse_of(&["ls", flag, "x"]).unwrap_err();
            assert!(err.contains(flag), "the refusal must name `{flag}`: {err}");
            assert!(!err.contains("unknown"), "`{flag}` is a real `data hist` flag: {err}");
            assert!(err.contains("data hist"), "…and must say where it belongs: {err}");
        }
        // THE CONTROL: a flag that is a real flag HERE is accepted, so the refusal above is about
        // foreign flags rather than about every `--` token.
        assert!(parse_of(&["ls", "--json"]).is_ok());
    }

    /// **THE OUTPUT DOOR.** The same axis, the same shorthand and the same by-name refusals the
    /// `hist` group carries, reached through the SAME `parse_format` rather than a second parser.
    ///
    /// ⚠ The disagreement message is a COPY — `crate::cmd::data`'s `parse` owns the other one —
    /// and this holds the two equal after whitespace normalisation. Exact bytes are deliberately
    /// not the property: that literal carries a run of spaces from an earlier edit, and pinning a
    /// typo would make the test about the typo instead of about the sentence.
    #[test]
    fn the_output_axis_refuses_the_same_pair_the_hist_group_refuses() {
        assert!(!parse_of(&["ls"]).unwrap().json, "table is the default");
        assert!(parse_of(&["ls", "--json"]).unwrap().json);
        assert!(parse_of(&["ls", "--format", "json"]).unwrap().json);
        assert!(!parse_of(&["ls", "--format", "table"]).unwrap().json);
        assert!(parse_of(&["ls", "--json", "--format", "json"]).unwrap().json);

        let mine = parse_of(&["ls", "--json", "--format", "table"]).unwrap_err();
        let hist = super::super::parse(
            ["hist", "ls", "--json", "--format", "table"].iter().map(|s| (*s).to_string()),
            None,
        )
        .unwrap_err();
        assert_eq!(
            squeeze(&mine),
            squeeze(&hist),
            "one axis, one sentence — the two copies have drifted"
        );

        // ⚠ **This looped over `super::super::UNBUILT_FORMATS` and became VACUOUS when that roster
        // emptied** — `csv` and `parquet` are WRITTEN now, by `data hist export`, so neither is
        // "designed but not built" and the loop had nothing to iterate. The claim it was making is
        // still worth holding, and it is about the SHARED PARSER: a format this verb does not
        // serve is refused here in the same words the `hist` group uses, because both reach
        // `crate::cmd::data::parse_format`. So the values are named and the two sides compared.
        for name in ["csv", "parquet", "jsonl"] {
            let mine = parse_of(&["ls", "--format", name]).unwrap_err();
            let hist = super::super::parse(
                ["hist", "ls", "--format", name].iter().map(|s| (*s).to_string()),
                None,
            )
            .unwrap_err();
            assert_eq!(squeeze(&mine), squeeze(&hist), "{name}: one parser, one sentence");
            // ANTI-VACUITY: the shared sentence is not empty and it names the value.
            assert!(mine.contains(name), "{name}: {mine}");
        }
    }

    /// A missing or misspelt verb RENDERS the roster rather than restating it, and `--help` is a
    /// success rather than a diagnostic (the shared `HELP_SENTINEL` path).
    ///
    /// ⚠ **The roster half could not fail and now can.** It asserted `err.contains("ls")` against
    /// ``unknown `data source` verb 'lsit' (ls | show)`` — and the ECHOED token `lsit` satisfies
    /// that on its own, so a message that named no roster at all still passed. What the claim is
    /// about is the RENDERED suffix, so that is what is compared: the exact parenthesis [`VERBS`]
    /// produces, which a hand-typed roster stops matching the moment that const moves.
    #[test]
    fn the_verb_roster_is_rendered_not_restated() {
        let roster = format!("({})", VERBS.join(" | "));
        assert!(VERBS.len() >= 2, "a one-verb roster would make the suffix trivially matchable");
        for verb in VERBS {
            assert!(
                parse_of(&[*verb]).is_ok() || parse_of(&[*verb, "vike"]).is_ok(),
                "`{verb}` must be reachable by the name the roster advertises"
            );
        }
        let err = parse_of(&[]).unwrap_err();
        assert!(err.ends_with(&roster), "the refusal must RENDER `{roster}`: {err}");

        let err = parse_of(&["lsit"]).unwrap_err();
        assert!(err.contains("unknown"), "{err}");
        assert!(err.contains("'lsit'"), "…echoing what was typed: {err}");
        assert!(err.ends_with(&roster), "…and still rendering `{roster}`: {err}");

        assert_eq!(
            parse_of(&["--help"]).unwrap_err(),
            crate::cmd::args::HELP_SENTINEL,
            "help is CONTROL FLOW, not a diagnostic"
        );
    }

    /// `ls` takes no positional and `show` requires one — both refused by name rather than
    /// defaulted, because either default would answer a question nobody asked.
    #[test]
    fn each_verb_refuses_the_argument_shape_that_is_not_its_own() {
        let err = parse_of(&["ls", "vike"]).unwrap_err();
        assert!(err.contains("takes no argument"), "{err}");
        assert!(err.contains("data source show vike"), "…and names the verb that does: {err}");

        let err = parse_of(&["show"]).unwrap_err();
        assert_eq!(err, SHOW_NEEDS_A_NAME, "worded once, so [`run`]'s arm cannot disagree");
        assert!(err.contains("data source ls"), "…and says where to find one: {err}");

        let err = parse_of(&["show", "vike", "demo"]).unwrap_err();
        assert!(err.contains("one source per `show`"), "{err}");

        assert_eq!(parse_of(&["show", "vike"]).unwrap().row.expect("a row").name, "vike");
        assert_eq!(parse_of(&["ls"]).unwrap().row, None);
    }

    /// The usage names every verb this parser accepts and every option it takes — each in a ROW of
    /// its own, which is the only form of that claim that can fail.
    ///
    /// ⚠ **This test could not fail for its stated reason.** It asserted `USAGE.contains(verb)`
    /// over the whole page while calling itself "the only thing standing between an operator and a
    /// verb they cannot discover" — and `ls` is a substring of `jsonl` in the `--format` row, of
    /// `` `ls` `` in the `--json` prose and of the word `false`, while `show` is a substring of
    /// "For `show`". Deleting either verb's ROW left the page advertising neither and every
    /// assertion green. [`usage_row`] asserts against the smallest unit that carries the claim, and
    /// the controls below prove it can answer `None`.
    #[test]
    fn the_usage_names_every_verb_and_flag_this_parser_accepts() {
        for verb in VERBS {
            let row = usage_row(verb).unwrap_or_else(|| panic!("USAGE must give `{verb}` a row"));
            assert!(row.len() > verb.len() + 8, "…that says what the verb does: {row}");
        }
        for option in ["--format", "--json", "-h"] {
            assert!(usage_row(option).is_some(), "USAGE must give `{option}` a row");
        }
        // `--addr` deliberately has NO row: it is refused, not accepted. The page owes it a
        // sentence instead, which is the one thing an operator who typed it needs.
        assert!(USAGE.contains("--addr"), "the refused flag must still be named in the prose");
        assert!(
            USAGE.contains("verified"),
            "…and the limit, which is the one thing this group must not leave to the code"
        );
        // THE CONTROLS: tokens this parser does not accept head no row, so the assertions above
        // are about the rows rather than about the page being long enough to contain anything.
        assert_eq!(usage_row("fetch"), None, "a `data hist` verb is not a row of this page");
        assert_eq!(usage_row("jsonl"), None, "a refused format is named in prose, not as a row");
    }

    /// **THE HELP MAY NOT PROMISE WHAT THE OUTPUT DENIES.** [`USAGE`]'s `show` row said `show NAME`
    /// reports "what THIS box reaches", the module doc's verb table said it a second time and
    /// [`LS_NOTES`]' third note a third — while [`NOT_VERIFIED`], which every answer ends on, says
    /// no line here is a probe of what your box, your network or your key reaches. An operator who
    /// read the help, ran `show vike` and saw `reaches: nothing yet` would read it as a fact about
    /// their box rather than about the build: positive confirmation of something false.
    #[test]
    fn nothing_rendered_promises_a_per_box_reach() {
        let show_row = usage_row("show").expect("the `show` row");
        assert!(
            !show_row.contains("THIS box"),
            "the help may not promise a probe the output denies: {show_row}"
        );
        for note in LS_NOTES {
            assert!(
                !note.contains("this box reaches"),
                "a footnote may not promise it either: {note}"
            );
        }
        // The control: the phrase is not simply absent from the whole surface — NOT_VERIFIED uses
        // it, to DENY it, which is the one place it belongs.
        assert!(NOT_VERIFIED.contains("your box"), "{NOT_VERIFIED}");
    }
}
