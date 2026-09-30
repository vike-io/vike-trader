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
    assert!(!text.contains("https://"), "no base is resolved here, so none may be printed: {text}");
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
    assert_eq!(empty, missing, "obeying the refusal must not produce a SECOND, different refusal");
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
        assert!(!note.contains("this box reaches"), "a footnote may not promise it either: {note}");
    }
    // The control: the phrase is not simply absent from the whole surface — NOT_VERIFIED uses
    // it, to DENY it, which is the one place it belongs.
    assert!(NOT_VERIFIED.contains("your box"), "{NOT_VERIFIED}");
}
