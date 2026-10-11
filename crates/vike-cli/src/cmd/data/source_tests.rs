use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
}

fn row_of(name: &str) -> Row {
    resolve(name).unwrap_or_else(|e| panic!("`{name}` must resolve: {e}"))
}

/// The instant every date-dependent assertion here is written against: 2026-09-30 UTC, a FIXED
/// point and never the clock, so these tests read the same tomorrow. `run` reads the real clock
/// once and hands it down; nothing below it does.
fn today() -> i64 {
    vike_model::time::days_from_civil(2026, 9, 30) * vike_model::MS_PER_DAY
}

fn show_text(name: &str) -> String {
    show_lines(&row_of(name), today()).join("\n")
}

fn show_doc(name: &str) -> serde_json::Value {
    serde_json::from_str(&show_json(&row_of(name), today())).expect("show --json is one document")
}

/// Collapse every run of whitespace to one space. Used where a message's EXACT spacing is not
/// the property under test — see
/// `the_output_axis_refuses_the_same_pair_the_hist_group_refuses`.
fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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

/// **THE SECOND: the `fetch` help.** It said "No credentials — this is public market data" over
/// EVERY venue token, the same falsehood one page over. It now says most venues need none, that a
/// venue that does is marked, and names the verb that answers — spelt with the placeholder a reader
/// substitutes, and asserted after whitespace is squeezed because the help wraps mid-command.
#[test]
fn the_fetch_help_no_longer_claims_every_venue_is_public() {
    let usage = squeeze(super::super::USAGE);
    assert!(
        !usage.contains("No credentials — this is public market data"),
        "the fetch help still says every venue is public"
    );
    assert!(usage.contains("Most venues need no credentials"), "{usage}");
    assert!(usage.contains("`vike-cli data source show VENUE` says what a venue needs"), "{usage}");
}

#[path = "source_tests/listing.rs"]
#[cfg(test)]
mod listing;

#[path = "source_tests/show_and_help.rs"]
#[cfg(test)]
mod show_and_help;

#[path = "source_tests/channels.rs"]
#[cfg(test)]
mod channels;

#[path = "source_tests/asked.rs"]
#[cfg(test)]
mod asked;
