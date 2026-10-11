//! The roster: every source as a row, the one resolver, and the notes its renderings carry.

use vike_catalog::history_channels_for;
use vike_model::VENUES;

use super::{
    DESIGNED_COST, NOT_VERIFIED, Row, SOURCES, Source, State, UNBUILT_SOURCES, VENUE_TOKEN,
};

/// The TRANSPORT cell — DERIVED from `Source::engine_verb`, never restated.
///
/// ⚠ That function IS the datahub-or-engine split: `None` means the source asks a datahub
/// (`Request::Backfill`), `Some` means it spawns the standalone engine, and its own doc calls
/// itself the seam. Reading the cell out of it means a source that CHANGES transport changes this
/// column with no edit here — which is exactly what a hand-written third column would not do.
pub(super) fn reaches(source: Source) -> &'static str {
    match source.engine_verb() {
        None => "a datahub",
        Some(_) => "the engine, on this box",
    }
}

/// One built source's row. The `match` is what the compiler holds exhaustive — see [`SOURCES`].
pub(super) fn built_row(source: Source) -> Row {
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
pub(super) fn rows() -> Vec<Row> {
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
pub(super) fn resolve(name: &str) -> Result<Row, String> {
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

/// What `ls` prints under the table. Each line is a fact the columns alone would misreport.
pub(super) const LS_NOTES: &[&str] = &[
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
pub(super) const VIKE_CLASSES: &[(&str, &str)] = &[
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
pub(super) const VIKE_LICENCE: &str = "⚠ it serves NO CEX market data at all: the Polymarket archive, the \
                            event API and the Hyperliquid panels, and nothing else.";

/// The keyed/keyless split — the property that makes an honest `show vike` possible at all, and
/// the sentence saying this build does not yet exploit it. See this module's doc.
pub(super) const VIKE_KEYS: &str = "keys: the ARCHIVE is keyed and its MANIFEST is not, so `what exists` and \
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
pub(super) const VIKE_BASE: &str = "base: one per lane, each overridable where that lane is configured — so no \
                         URL is printed here. This side resolves none of them, and a constant would \
                         name a base this box may not be using.";

/// The line above a roster venue's channels.
pub(super) const HISTORY_HEADING: &str =
    "history — through which doors this venue's history comes, and how far back each goes:";

/// What a venue token that is NOT on the roster is told about its channels: that there are none
/// DECLARED, which is a statement about this table and not about the venue.
///
/// ⚠ The empty answer is the dangerous one — an operator who typed a venue this build has never
/// classified would read a blank as "no history channel exists", and the only thing this build
/// knows about that venue is its name.
pub(super) fn undeclared_note(name: &str) -> String {
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
pub(super) fn ls_notes() -> Vec<&'static str> {
    LS_NOTES.iter().copied().chain(std::iter::once(NOT_VERIFIED)).collect()
}

/// ONE footnote, as the TABLE prints it — a `note:` row, or, for [`NOT_VERIFIED`], a blank line
/// and the sentence itself.
///
/// ⚠ The closer is set apart rather than filed as one `note:` among several because it is not one:
/// it is the limit EVERY answer in this group ends on, and prefixing it would rank it beside a
/// column footnote. A function rather than two pushes in [`ls_lines`] so that the table renders
/// [`ls_notes`] WHOLE — the shape a footnote cannot escape by being appended somewhere else.
pub(super) fn note_lines(note: &str) -> Vec<String> {
    if note == NOT_VERIFIED {
        vec![String::new(), note.to_string()]
    } else {
        vec![format!("note: {note}")]
    }
}
