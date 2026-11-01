//! The grammar: every verb, flag, refusal and address rung the group takes, and what its help says.
use super::*;

/// Every verb [`VERBS`] names is one [`parse`] accepts, under the grammar its own help page
/// states — so a roster row the parser refuses is caught here.
///
/// ⚠ **This carried a second loop claiming "and every parsed verb is in the roster", and that
/// loop could not fail.** It asserted `verb_roster().contains(v.as_str())` for `v in VERBS`,
/// and [`verb_roster`] IS `VERBS` joined — a derivation compared against its own input. The
/// direction it claimed is the one that matters (a verb [`parse`] accepts that the roster does
/// not name is undiscoverable), and it is the one no test in this file can have: the parser's
/// accepted set is a `match` over literals and nothing here can enumerate it. Deleted rather
/// than left as a doc promising what it does not check. What narrows the gap instead is the
/// same ratchet the flags have — every accepted verb reaches [`Verb`], and the unknown-verb
/// refusal renders [`VERBS`], so a variant missing from the roster is named in no refusal at all
/// rather than merely untested.
#[test]
fn every_verb_in_the_roster_parses_under_its_own_grammar() {
    for v in VERBS {
        let line: Vec<&str> = match v {
            Verb::Ls | Verb::Refresh => vec![v.as_str(), "--venue", "binance"],
            Verb::Show => vec![v.as_str(), "binance:BTCUSDT"],
            Verb::Venues => vec![v.as_str()],
        };
        let got = parse_of(&line).unwrap_or_else(|e| panic!("{} must parse: {e}", v.as_str()));
        assert_eq!(got.verb, *v);
    }
}

#[test]
fn a_missing_verb_names_every_verb_that_exists() {
    let err = parse_of(&[]).expect_err("a verb is required");
    for v in VERBS {
        assert!(err.contains(v.as_str()), "the refusal must name `{}`: {err}", v.as_str());
    }
    // Anti-vacuity: the message is not merely long enough to contain any short word.
    assert!(!err.contains("frobnicate"), "{err}");
}

/// The rename an operator arrives with, because the sibling group made the same one.
#[test]
fn the_renamed_listing_verb_names_its_replacement_rather_than_the_roster() {
    let err = parse_of(&["list", "--venue", "binance"]).expect_err("`list` is not a verb");
    assert!(err.contains("`data catalog ls`"), "{err}");
    // ...and a genuinely unknown verb gets the OTHER message, so the assertion above is not
    // passing because everything says the same thing.
    let other = parse_of(&["frobnicate"]).expect_err("unknown");
    assert!(other.contains("unknown `data catalog` verb"), "{other}");
    assert!(!other.contains("`data catalog ls`"), "{other}");
}

/// `--venue` is the SUBJECT of two verbs and meaningless on the other two, and each refusal
/// says which — never "unknown option", which would tell an operator the flag does not exist.
#[test]
fn venue_is_required_where_it_is_the_subject_and_refused_where_it_is_not() {
    for v in ["ls", "refresh"] {
        let err = parse_of(&[v]).expect_err("--venue is required");
        assert!(err.contains("--venue"), "{v}: {err}");
        assert!(err.contains("data catalog venues"), "{v} must name the roster verb: {err}");
    }
    let show = parse_of(&["show", "binance:BTCUSDT", "--venue", "binance"])
        .expect_err("--venue is refused on show");
    assert!(show.contains("VENUE:SYMBOL"), "{show}");
    let venues =
        parse_of(&["venues", "--venue", "binance"]).expect_err("--venue is refused on venues");
    assert!(venues.contains("roster"), "{venues}");
}

/// The non-`show` lines every refusal below is applied to, spelled once.
fn other_verb_lines() -> Vec<Vec<&'static str>> {
    vec![vec!["ls", "--venue", "binance"], vec!["refresh", "--venue", "binance"], vec!["venues"]]
}

#[test]
fn the_instrument_positional_belongs_to_show_alone_and_every_refusal_says_why() {
    for base in other_verb_lines() {
        let mut line = base.clone();
        line.push("binance:BTCUSDT");
        let err = parse_of(&line).expect_err("a positional is refused");
        assert!(err.contains("data catalog show"), "{base:?}: {err}");
    }
    let missing = parse_of(&["show"]).expect_err("show needs one");
    assert!(missing.contains("VENUE:SYMBOL"), "{missing}");
    let two = parse_of(&["show", "binance:BTCUSDT", "okx:BTC-USDT"])
        .expect_err("two positionals are refused");
    assert!(two.contains("binance:BTCUSDT") && two.contains("okx:BTC-USDT"), "{two}");
}

/// The three-part spelling is the one an operator arrives with from `data hist fetch`, so it
/// is refused by NAME rather than as a malformed line.
#[test]
fn a_series_spec_is_refused_as_a_series_rather_than_as_a_typo() {
    let err = parse_of(&["show", "binance:BTCUSDT:1h"]).expect_err("a series is not one");
    assert!(err.contains("INTERVAL"), "{err}");
    assert!(err.contains("data hist"), "the refusal must name the verbs that take one: {err}");
    // A genuinely malformed spelling gets the SHAPE message instead, so the assertion above
    // is not passing because every bad spec says the same thing.
    for bad in ["binance", "binance:", ":BTCUSDT", "binance: "] {
        let e = parse_of(&["show", bad]).expect_err("malformed");
        assert!(e.contains("two non-empty parts"), "{bad}: {e}");
        assert!(!e.contains("INTERVAL"), "{bad}: {e}");
    }
}

/// The class vocabulary is the MODEL's, matched case-insensitively, and the refusal renders
/// the whole roster rather than a guess.
#[test]
fn the_class_filter_is_the_models_own_word_and_an_unknown_one_names_the_roster() {
    for class in AssetClass::ALL {
        let word = class.sql_word();
        let got = parse_of(&["ls", "--venue", "binance", "--class", word])
            .unwrap_or_else(|e| panic!("{word}: {e}"));
        assert_eq!(got.class, Some(*class));
        // ...and the same word shouted, because an operator types `CryptoPerp` and `cryptoperp`
        // interchangeably and neither is a different class.
        let shouted = parse_of(&["ls", "--venue", "binance", "--class", &word.to_uppercase()])
            .unwrap_or_else(|e| panic!("{word} upper: {e}"));
        assert_eq!(shouted.class, Some(*class));
    }
    let err = parse_of(&["ls", "--venue", "binance", "--class", "perp"])
        .expect_err("`perp` is not the vocabulary");
    for class in AssetClass::ALL {
        assert!(err.contains(class.sql_word()), "the refusal must name {}", class.sql_word());
    }
    let blank = parse_of(&["ls", "--venue", "binance", "--class", ""])
        .expect_err("an empty class names nothing");
    assert!(blank.contains("--class"), "{blank}");
}

#[test]
fn the_narrowing_flags_belong_to_the_listing_and_every_refusal_names_it() {
    let mut verbs = other_verb_lines();
    verbs.retain(|v| v[0] != "ls");
    verbs.push(vec!["show", "binance:BTCUSDT"]);
    for verb in verbs {
        for flag in [["--class", "CryptoSpot"], ["--search", "BTC"]] {
            let mut line = verb.clone();
            line.extend(flag.iter().copied());
            let err = parse_of(&line).expect_err("refused off `ls`");
            assert!(err.contains(flag[0]), "{verb:?} {flag:?}: {err}");
            assert!(err.contains("data catalog ls"), "{verb:?} {flag:?}: {err}");
        }
    }
    // The control: both flags are ACCEPTED on `ls`, so the loop above is not passing because
    // this parser refuses them everywhere.
    let ok = parse_of(&["ls", "--venue", "binance", "--class", "CryptoSpot", "--search", "BTC"])
        .expect("`ls` takes both");
    assert_eq!(ok.class, Some(AssetClass::CryptoSpot));
    assert_eq!(ok.search.as_deref(), Some("BTC"));
}

/// **A `--venue` this binary can refuse, refused by this binary.** The shape rules are
/// `vike_datahub_client::catalog::validate_catalog_venue`'s — the SAME function the server's
/// door calls — so the refusal an operator reads locally is the refusal the server would have
/// given, and it arrives on the USAGE rung with no socket opened.
///
/// ⚠ Until [`parse`] called it, each of these opened a connection and the operator's diagnosis
/// depended on who was on the port: `cannot connect to datahub at …` (the CONNECT rung, which
/// a wrapper retries) with no datahub up, or the `does not advertise venue_catalog` message
/// against a server with no catalog lane. The slug itself was named on neither path.
#[test]
fn a_venue_this_binary_can_refuse_is_refused_before_a_socket_is_opened() {
    for verb in ["ls", "refresh"] {
        for bad in ["", "BINANCE", "bin@nce", "bi nance", "averyveryverylongvenueslug"] {
            let Err(err) = parse_of(&[verb, "--venue", bad]) else {
                panic!("`{verb} --venue {bad}` must be refused before a socket is opened");
            };
            assert!(err.contains("--venue"), "the refusal names the flag: {err}");
            // ...and the sentence is the CLIENT's own, so one refusal is worded one way
            // wherever it is reached from. This is the exact text the server would return.
            // (The EMPTY slug never reaches the validator any more: the command-line layer
            // refuses a blank value for every flag, in its own words, before the verb's rules.)
            if bad.is_empty() {
                continue;
            }
            let wire = validate_catalog_venue(bad).expect_err("the client refuses it too");
            assert!(err.contains(&wire), "the wording must be the wire's: {err}");
        }
    }
    // The control, in both directions: a real roster slug parses, and the refusal above is not
    // passing because every `--venue` is refused.
    for venue in vike_model::VENUES {
        let got = parse_of(&["ls", "--venue", *venue])
            .unwrap_or_else(|e| panic!("`{venue}` is on the roster and must parse: {e}"));
        assert_eq!(got.venue.as_deref(), Some(*venue));
    }
    // ⚠ ...and `show`'s venue is deliberately NOT put through this validator: that half of the
    // positional reaches `properties_as_of`, a different verb with its own door.
    let shouted = parse_of(&["show", "BINANCE:BTCUSDT"]).expect("`show` validates no venue");
    assert_eq!(shouted.instrument.expect("an instrument").venue, "BINANCE");
}

/// `--search ""` is refused by NAME, like its two sibling narrowing/rendering flags — an empty
/// needle keeps every row while claiming a filter ran.
///
/// ⚠ The refusal is the command-line layer's now, one rule for EVERY flag and positional
/// (`--search needs a non-empty value`), rather than three hand-written sentences that all said
/// EMPTY — which is also why a needle of nothing BUT whitespace is refused where it used to be
/// accepted. A needle with text in it still parses, spaces included.
#[test]
fn an_empty_search_is_refused_the_way_an_empty_class_and_an_empty_format_are() {
    let err = parse_of(&["ls", "--venue", "binance", "--search", ""])
        .expect_err("an empty needle narrows nothing");
    assert!(err.contains("--search"), "{err}");
    // The three flags that take a value all answer the same way, so an operator meets ONE
    // rule rather than three — and the control is that a real needle still parses.
    for flag in ["--class", "--format", "--search"] {
        let Err(e) = parse_of(&["ls", "--venue", "binance", flag, ""]) else {
            panic!("`{flag} \"\"` must be refused by name");
        };
        assert!(e.contains(flag), "{flag}: {e}");
    }
    assert_eq!(
        parse_of(&["ls", "--venue", "binance", "--search", " perp"])
            .expect("whitespace is a needle, not an accident")
            .search
            .as_deref(),
        Some(" perp")
    );
}

/// The format axis, and the refusals that are as much a part of it as the two values — the
/// same contract `crate::cmd::data`'s `parse_format` states, reached through that function so
/// there is no second roster here.
///
/// ⚠ **THIS TEST WAS CALLED `…refuses_the_unbuilt_ones_by_name` AND ASSERTED THE WORDS "not
/// built" FOR `csv`/`parquet`, AND BOTH HALVES ARE NOW FALSE.** `data hist export --out FILE`
/// writes Parquet (and always did — the old row's own text said so while filing it under
/// "nothing writes one") and `export --addr --format csv` writes CSV. So `crate::cmd::data`'s
/// `UNBUILT_FORMATS` is EMPTY and neither value is waiting on a phase: they are refused HERE
/// because a catalog is printed and these are FILE formats, which is a fact about this verb
/// rather than about the workspace. A test still demanding the old words would have kept a
/// message pointing operators at a plan for something they can run today.
#[test]
fn the_format_axis_carries_json_and_refuses_the_file_formats_by_name() {
    assert!(!parse_of(&["venues"]).expect("default").json);
    assert!(parse_of(&["venues", "--json"]).expect("--json").json);
    assert!(parse_of(&["venues", "--format", "json"]).expect("--format json").json);
    assert!(!parse_of(&["venues", "--format", "table"]).expect("--format table").json);
    for file_format in ["csv", "parquet"] {
        let err = parse_of(&["venues", "--format", file_format]).expect_err("a FILE format");
        assert!(err.contains(file_format), "{file_format}: {err}");
        assert!(err.contains("FILE format"), "{file_format}: {err}");
        // ...and it names the verb that WRITES one, so an operator who wanted a file has a
        // command line rather than a diagnosis.
        assert!(err.contains("data hist export"), "{file_format}: {err}");
        // THE ANTI-VACUITY CONTROL, and the reason this test was renamed: a shipped format may
        // not be described as waiting on a phase.
        assert!(!err.contains("not built"), "{file_format} SHIPS: {err}");
    }
    // ⚠ **`jsonl` LEFT that loop when `data hist get` shipped, and the split is the point.**
    // It is refused here for a different reason from its two former neighbours — not "a FILE
    // format" but "built, on the verb that emits ROWS to stdout" — so a message conflating the
    // two would send an operator who typed the right format on the wrong verb to `--out`
    // instead of to a pipe. Reached through `crate::cmd::data`'s `parse_format`, so this
    // asserts that function's arm rather than a second roster.
    let err = parse_of(&["venues", "--format", "jsonl"]).expect_err("a catalog is not rows");
    assert!(err.contains("data hist get"), "the refusal names the ROW verb: {err}");
    assert!(!err.contains("not built"), "…and does not call a shipped format unbuilt: {err}");
    // A switch with a value is refused, naming the switch (the wording is `clap`'s).
    assert!(parse_of(&["venues", "--json=1"]).expect_err("boolean").contains("--json"));
}

/// The flag, then the configured address, then the default — the ladder
/// `crate::cmd::data`'s `parse` already climbs, with a BLANK configured rung skipped rather
/// than honoured.
#[test]
fn the_address_ladder_is_cli_then_configured_then_default() {
    assert_eq!(parse_of(&["venues"]).expect("default").addr, vike_config::DEFAULT_DATAHUB_ADDR);
    let configured = parse(&argv(&["venues"]), Some("<host>:9")).expect("configured");
    assert_eq!(configured.addr, "<host>:9");
    let flagged = parse(&argv(&["venues", "--addr", "127.0.0.1:1"]), Some("<host>:9"))
        .expect("the flag wins");
    assert_eq!(flagged.addr, "127.0.0.1:1");
    let blank = parse(&argv(&["venues"]), Some("   ")).expect("a blank rung is skipped");
    assert_eq!(blank.addr, vike_config::DEFAULT_DATAHUB_ADDR);
}

/// `--help` is `clap`'s page, rendered from the declarations the parser reads — so there is ONE
/// description and nothing to hold equal to it. What is worth a test is the CONTENT facts the
/// page must carry: every verb listed as a command with a paragraph of its own, every flag named
/// on the verb that takes it, and the two runtime facts (the class vocabulary, the default
/// address) rendered rather than typed.
///
/// ⚠ **The verb half asserts the verb's OWN paragraph, not its name.** Every verb NAME occurs in
/// the page's prose independently of its own block (`ls` in `refresh`'s paragraph, `venues` in
/// `show`'s), so a name-only check stayed green over a deleted paragraph — see the measurement the
/// hand-rendered page's test carried. Each verb's marker below is a phrase only its paragraph has.
#[test]
fn the_help_gives_every_verb_a_row_and_a_paragraph_and_every_flag_its_vocabulary() {
    let group = help_of(&["--help"]);
    let listed =
        |v: &Verb| group.lines().any(|l| l.trim_start().starts_with(&format!("{} ", v.as_str())));
    for v in VERBS {
        assert!(listed(v), "`{}` has no row in the group's command list:\n{group}", v.as_str());
    }
    assert!(group.contains("WHAT IS ADDRESSABLE"), "the group's preamble moved: {group}");
    for (verb, marker) in [
        (Verb::Ls, "none of them renders as an empty list"),
        (Verb::Show, "kind=properties"),
        (Verb::Refresh, "force bit"),
        (Verb::Venues, "THE CAPABILITY MATRIX"),
    ] {
        let page = help_of(&[verb.as_str(), "--help"]);
        assert!(page.contains(marker), "`{}` lost its paragraph:\n{page}", verb.as_str());
    }

    // Every flag is declared on every verb (the group's one flag set), with the runtime facts.
    let ls = help_of(&["ls", "--help"]);
    for flag in ["--venue", "--class", "--search", "--addr", "--format", "--json"] {
        assert!(ls.contains(flag), "`ls --help` must name {flag}:\n{ls}");
    }
    for class in AssetClass::ALL {
        assert!(ls.contains(class.sql_word()), "the class roster is derived and complete");
    }
    assert!(
        ls.contains(vike_config::DEFAULT_DATAHUB_ADDR),
        "the default address is stated, not implied"
    );
    assert!(!ls.contains('{'), "an unexpanded placeholder survived into the page: {ls}");
    // The control: the vocabulary is the model's own, so the derived help and the parser agree.
    assert!(ls.contains(&class_roster()), "{ls}");
}

/// A flag the group does not declare is refused, naming it — so the flags the help lists are not
/// merely ones nothing ever refuses.
#[test]
fn an_undeclared_flag_is_refused_by_name() {
    let err = parse_of(&["ls", "--venue", "binance", "--limit", "5"])
        .expect_err("an undeclared flag is unknown");
    assert!(!err.is_help());
    assert!(err.contains("--limit"), "{err}");
}
