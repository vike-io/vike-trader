//! `show`'s source rows, the parser's refusals, and the usage page each verb is found by.
use super::*;

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
/// out of `crates/vike-data/src/store/store_kind.rs`'s `STORE_KINDS`, the declared authority, in the
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

/// `--addr` is refused BY NAME on `ls`, with the reason and with the verbs that do take one: `ls`
/// opens no socket, so accepting it would advertise a reach that verb does not have.
///
/// ⚠ It was refused on `show` too until the history-channels read (the owner's Q3): `show VENUE`
/// now takes it for a ROSTER venue, and every other row is refused by name — a source name or an
/// unclassified venue token has no channels to ask a datahub about.
#[test]
fn the_addr_flag_is_refused_by_name() {
    let err = parse_of(&["ls", "--addr", "1.2.3.4:9"]).unwrap_err();
    assert!(err.contains("--addr"), "{err}");
    assert!(err.contains("no server"), "the refusal must say WHY: {err}");
    assert!(err.contains("data hist"), "…and what does take one: {err}");
    assert!(err.contains("show VENUE --addr"), "…including this group's own: {err}");
    for name in ["vike", "demo", "notavenue"] {
        let err = parse_of(&["show", name, "--addr=1.2.3.4:9"]).unwrap_err();
        assert!(err.contains("ROSTER VENUE") && err.contains(name), "{name}: {err}");
    }
    let err = parse_of(&["show", "oanda", "--addr", ""]).unwrap_err();
    assert!(err.contains("EMPTY"), "an empty address names no datahub: {err}");
    // A roster venue takes it — and only when asked: a plain `show` carries no address at all.
    let asked = parse_of(&["show", "oanda", "--addr", "127.0.0.1:7878"]).unwrap();
    assert_eq!(asked.addr.as_deref(), Some("127.0.0.1:7878"));
    assert_eq!(
        parse_of(&["show", "--addr=127.0.0.1:7878", "oanda"]).unwrap().addr.as_deref(),
        Some("127.0.0.1:7878")
    );
    assert_eq!(parse_of(&["show", "oanda"]).unwrap().addr, None, "the reach is opt-in");
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
    for option in ["--format", "--json", "--addr", "-h"] {
        assert!(usage_row(option).is_some(), "USAGE must give `{option}` a row");
    }
    // `--addr` has a row since `show VENUE` takes it, and the row must say which verb, and that
    // it is opt-in — `ls` still refuses it.
    let addr_row = usage_row("--addr").expect("the --addr row");
    assert!(addr_row.contains("show VENUE"), "{addr_row}");
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
