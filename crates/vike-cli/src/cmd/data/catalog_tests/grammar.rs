//! The parser and the usage page: every verb, flag, refusal and address rung the group takes.
use super::*;

/// Every verb [`VERBS`] names is one [`parse`] accepts, under the grammar its own usage block
/// states — so a roster row the parser refuses is caught here.
///
/// ⚠ **This carried a second loop claiming "and every parsed verb is in the roster", and that
/// loop could not fail.** It asserted `verb_roster().contains(v.as_str())` for `v in VERBS`,
/// and [`verb_roster`] IS `VERBS` joined — a derivation compared against its own input. The
/// direction it claimed is the one that matters (a verb [`parse`] accepts that the roster does
/// not name is undiscoverable), and it is the one no test in this file can have: the parser's
/// accepted set is a `match` over literals and nothing here can enumerate it. Deleted rather
/// than left as a doc promising what it does not check. What narrows the gap instead is the
/// same ratchet the flags have — every accepted verb reaches [`Verb`], and [`usage`] and every
/// refusal render [`VERBS`], so a variant missing from the roster is documented and named
/// nowhere at all rather than merely untested.
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
    assert!(blank.contains("EMPTY"), "{blank}");
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

/// `--search ""` is refused by NAME, like its two sibling narrowing/rendering flags — see
/// [`parse_search`] for what an empty needle does to a listing that is not refused.
#[test]
fn an_empty_search_is_refused_the_way_an_empty_class_and_an_empty_format_are() {
    let err = parse_of(&["ls", "--venue", "binance", "--search", ""])
        .expect_err("an empty needle narrows nothing");
    assert!(err.contains("--search"), "{err}");
    assert!(err.contains("EMPTY"), "{err}");
    // The three flags that take a value all answer the same way, so an operator meets ONE
    // rule rather than three — and the control is that a real needle still parses.
    for flag in ["--class", "--format", "--search"] {
        let Err(e) = parse_of(&["ls", "--venue", "binance", flag, ""]) else {
            panic!("`{flag} \"\"` must be refused by name");
        };
        assert!(e.contains("EMPTY"), "{flag}: {e}");
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
    assert!(parse_of(&["venues", "--json=1"]).expect_err("boolean").contains("takes no value"));
}

/// The flag, then the configured address, then the default — the ladder
/// `crate::cmd::data`'s `parse` already climbs, with a BLANK configured rung skipped rather
/// than honoured.
#[test]
fn the_address_ladder_is_cli_then_configured_then_default() {
    assert_eq!(parse_of(&["venues"]).expect("default").addr, DEFAULT_ADDR);
    let configured = parse(&argv(&["venues"]), Some("<host>:9")).expect("configured");
    assert_eq!(configured.addr, "<host>:9");
    let flagged = parse(&argv(&["venues", "--addr", "127.0.0.1:1"]), Some("<host>:9"))
        .expect("the flag wins");
    assert_eq!(flagged.addr, "127.0.0.1:1");
    let blank = parse(&argv(&["venues"]), Some("   ")).expect("a blank rung is skipped");
    assert_eq!(blank.addr, DEFAULT_ADDR);
}

/// The usage page is the only place this group's verbs and flags are named, so one missing
/// from it is one an operator cannot discover. The assertion is over the ROW each one owns —
/// the label column plus its own first line — never over the page.
///
/// ⚠ **This shipped as `the_usage_names_every_verb_and_every_flag_this_parser_accepts` and it
/// could not fail for either reason it named.** The verb half asserted
/// `usage().contains(v.as_str())`, and every verb name occurs in the page's PROSE independently
/// of its own block: measured over the text this replaced, `ls` appeared 9 times, `venues` 5,
/// `show` 3 and `refresh` 2 (`--venue V   ls/refresh:`, "`ls` lists may have nothing here",
/// "`data catalog venues` is the roster"), so deleting a verb's whole paragraph left this test
/// AND `crates/vike-cli/tests/data_cli/catalog.rs`'s
/// `the_catalog_group_answers_and_its_help_names_every_verb` green with the verb
/// undiscoverable from `--help`. The flag half was a hand-typed array of seven
/// spellings under a name promising "every flag this parser accepts" — the subtract-only shape
/// that file's own `help_names_every_subcommand_and_exits_zero` warns about. Its doc's claim
/// that "both rosters are DERIVED here" was true of the classes and of nothing else.
///
/// What holds it now is structural first and assertive second: [`usage`] RENDERS one block per
/// [`VERBS`] row through [`Verb::usage_block`]'s exhaustive match and one row per [`FLAGS`]
/// entry, so a block cannot be deleted without deleting a declaration the compiler wants.
///
/// ⚠ **The residual, declared rather than implied:** a flag arm added to [`parse`] with a
/// fresh literal and no [`FLAGS`] row is invisible here. A `match` over literals cannot be
/// enumerated from inside the process and this file has no source reflection, so no assertion
/// can reach it. What narrows it is that every arm [`parse`] carries today names one of the
/// consts [`FLAGS`] is built from — a new arm spelled as a bare literal is the only one that
/// would not — and `every_declared_flag_is_accepted_by_the_parser` holds the other direction.
#[test]
fn the_usage_renders_a_block_for_every_verb_and_a_row_for_every_flag_it_declares() {
    let text = usage();
    let verb_head = |v: &Verb| format!("  {:<9}{}", v.as_str(), expand(v.usage_block()[0]));
    for v in VERBS {
        assert!(text.contains(&verb_head(v)), "`{}` has no block of its own: {text}", v.as_str());
    }
    for f in FLAGS {
        let head = format!("  {:<12}{}", f.label(), expand(f.help[0]));
        assert!(text.contains(&head), "`{}` has no row of its own: {text}", f.label());
    }
    for class in AssetClass::ALL {
        assert!(text.contains(class.sql_word()), "the class roster is derived and complete");
    }
    assert!(text.contains(DEFAULT_ADDR), "the default address is stated, not implied");

    // THE KILL PROOF, and it is what separates this spelling from the one it replaced: strip
    // `refresh`'s row out of the rendered page and the check above fails on it — WHILE the
    // word `refresh` still occurs elsewhere in the page, which is precisely why
    // `contains("refresh")` stayed green over the same mutilation.
    let head = verb_head(&Verb::Refresh);
    let mutilated: Vec<&str> = text.lines().filter(|l| !l.contains(&head)).collect();
    let mutilated = mutilated.join("\n");
    assert!(!mutilated.contains(&head), "the mutilation must remove the row: {mutilated}");
    assert!(
        mutilated.contains(Verb::Refresh.as_str()),
        "…and the verb's NAME must survive it, which is the whole measurement: {mutilated}"
    );
}

/// Every flag [`FLAGS`] declares is one [`parse`] actually accepts — the other direction of
/// the page, and the one that catches a row kept after its arm was deleted or renamed.
#[test]
fn every_declared_flag_is_accepted_by_the_parser() {
    // A value each flag will take. `ls` is the verb every one of them is legal on.
    fn probe(spelling: &str) -> &'static str {
        match spelling {
            FLAG_VENUE => "okx",
            FLAG_CLASS => AssetClass::ALL[0].sql_word(),
            FLAG_SEARCH => "BTC",
            FLAG_ADDR => "127.0.0.1:1",
            FLAG_FORMAT => "json",
            other => panic!("`{other}` declares a value placeholder and this probe has none"),
        }
    }
    for f in FLAGS {
        for spelling in f.spellings {
            let mut line = vec!["ls", FLAG_VENUE, "binance", *spelling];
            if !f.arg.is_empty() {
                line.push(probe(spelling));
            }
            // ⚠ `-h`/`--help` is an `Err` BY DESIGN — it is the help REQUEST sentinel — so the
            // assertion is not "this parses" but "this is not an unknown option", which is the
            // one answer a deleted arm produces.
            if let Err(e) = parse_of(&line) {
                assert!(
                    !e.contains("unknown option"),
                    "`{spelling}` has a usage row and no parser arm: {e}"
                );
            }
        }
    }
    // The control: a spelling [`FLAGS`] does not declare IS refused that way, so the loop above
    // is not passing because nothing in this parser ever says "unknown option".
    let err = parse_of(&["ls", FLAG_VENUE, "binance", "--limit", "5"])
        .expect_err("an undeclared flag is unknown");
    assert!(err.contains("unknown option"), "{err}");
}

/// Every token [`expand`] is asked to substitute is one it knows. A `{…}` surviving into the
/// rendered page is a block naming a fact nothing supplies — which reads to an operator as a
/// literal brace where a vocabulary or an address should be.
#[test]
fn the_usage_leaves_no_placeholder_unexpanded() {
    let text = usage();
    assert!(!text.contains('{'), "an unexpanded placeholder survived into the page: {text}");
    // The control: the tokens are real and [`expand`] is doing work, so the assertion above is
    // not passing because the page never carried one.
    assert_eq!(expand("({classes})"), format!("({})", class_roster()));
    assert_eq!(expand("default {default_addr}."), format!("default {DEFAULT_ADDR}."));
}
