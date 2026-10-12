//! The listing: `ls` derived from both declarations, its footnotes, and the not-verified limit.
use super::*;

fn ls_text() -> String {
    ls_lines(&rows()).join("\n")
}

fn ls_doc() -> serde_json::Value {
    serde_json::from_str(&ls_json(&rows())).expect("ls --json is one document")
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
