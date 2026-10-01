use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(&argv(args), None)
}

fn row(symbol: &str, class: AssetClass) -> InstrumentRow {
    InstrumentRow {
        symbol: symbol.to_string(),
        class,
        base: "BTC".to_string(),
        quote: "USDT".to_string(),
        tick: 0.01,
        lot: 0.001,
        description: "Bitcoin".to_string(),
    }
}

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

/// ⚠ **The two groups spell one rule and there is no shared const to import**, so this is what
/// holds them equal: `crate::cmd::data`'s `parse` refuses the identical contradiction for the
/// `hist` group, and a reader who meets both must not learn that one is a different KIND of
/// no. It fails when either side is reworded, which is the moment to reword the other.
///
/// ⚠ **The comparison is over WORDS, not bytes, and that is a measurement rather than a
/// loosening.** The sibling's literal carries an eighteen-space run where a `\` line
/// continuation was meant — measured on this branch — so a byte comparison would demand that
/// this file reproduce that spacing in order to pass, which is copying a typographic defect
/// into a second place under the guise of agreement. Splitting on whitespace compares the
/// sentence both operators actually read, and still reddens on any rewording of either side.
#[test]
fn the_two_groups_refuse_the_json_format_contradiction_in_the_same_words() {
    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }
    let mine = parse_of(&["venues", "--json", "--format", "table"])
        .expect_err("the contradiction is refused");
    let hist = super::super::parse(
        ["hist", "ls", "--json", "--format", "table"].into_iter().map(String::from),
        None,
    )
    .expect_err("the sibling group refuses it too");
    assert_eq!(words(&mine), words(&hist), "one group reworded the shared refusal");
    // Anti-vacuity: `words` on two empty or two generic strings would also compare equal, so
    // the sentence has to be the real one — and it has to be more than a couple of tokens.
    assert!(mine.contains("--json") && mine.contains("--format table"), "{mine}");
    assert!(words(&mine).len() > 8, "a near-empty message would compare equal too: {mine}");
}

/// **…and the `jsonl` spelling, which is the one that actually parted.**
///
/// ⚠ `crate::cmd::data`'s `parse` grew a `--json --format jsonl` arm with `get`, and applied
/// it ABOVE the verb dispatch — so `data hist ls --json --format jsonl` answered with a
/// sentence about GET's document, ending "Pass one", while `data catalog ls --json --format
/// jsonl` answered with `ROW_VERB`, because THIS parser reads `--format` eagerly and its
/// contradiction check never sees a `jsonl` at all. One question, two answers, on the same
/// plane. The arm is `Sub::Get`'s alone now and this is what holds the two groups equal — the
/// case the `table` twin above could never have covered, since `table` is valid on both.
#[test]
fn the_two_groups_refuse_the_jsonl_format_contradiction_in_the_same_words() {
    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }
    let mine = parse_of(&["venues", "--json", "--format", "jsonl"])
        .expect_err("a catalog verb emits no rows");
    let hist = super::super::parse(
        ["hist", "ls", "--json", "--format", "jsonl"].into_iter().map(String::from),
        None,
    )
    .expect_err("the sibling group refuses it too");
    assert_eq!(words(&mine), words(&hist), "one group reworded the shared refusal");
    // Anti-vacuity, the same two rungs the twin above uses: the sentence has to be the real
    // one, and it has to name where `jsonl` DOES work rather than merely be long.
    assert!(mine.contains(super::super::ROW_VERB), "{mine}");
    assert!(words(&mine).len() > 8, "a near-empty message would compare equal too: {mine}");
    // THE CONTROL: the verb that SERVES `jsonl` answers differently, so the equality above is
    // about these two groups agreeing rather than about one sentence for every line.
    let get = super::super::parse(
        ["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"]
            .into_iter()
            .map(String::from),
        None,
    )
    .expect_err("a sequence is not one document");
    assert_ne!(words(&get), words(&mine), "`get` has a contradiction of its own: {get}");
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

/// The search is a case-insensitive substring over every field an operator can SEE in the
/// table, and the control is what makes the claim mean something: a term matching none of them
/// keeps nothing.
#[test]
fn the_search_reaches_every_visible_field_and_a_miss_keeps_nothing() {
    let r = row("BTCUSDT", AssetClass::CryptoSpot);
    for hit in ["btcusd", "BTC", "usdt", "bitcoin", "BITCOIN"] {
        assert!(keeps(&r, None, Some(hit)), "`{hit}` must match");
    }
    assert!(!keeps(&r, None, Some("ethereum")), "a miss must keep nothing");
    assert!(keeps(&r, None, None), "an absent filter matches everything");
}

#[test]
fn the_class_filter_keeps_only_that_class_and_is_anded_with_the_search() {
    let spot = row("BTCUSDT", AssetClass::CryptoSpot);
    let perp = row("BTCUSDT", AssetClass::CryptoPerp);
    assert!(keeps(&spot, Some(AssetClass::CryptoSpot), None));
    assert!(!keeps(&perp, Some(AssetClass::CryptoSpot), None));
    // ANDed: the right class with the wrong search keeps nothing.
    assert!(!keeps(&spot, Some(AssetClass::CryptoSpot), Some("ethereum")));
    assert!(keeps(&spot, Some(AssetClass::CryptoSpot), Some("btc")));
}

/// **The property this whole group's shape exists for.** A venue that listed NOTHING and a
/// venue that CANNOT be listed must not render alike — in the table or in the document. It is
/// `vike_datahub_client::catalog`'s decision 5, carried across the last hop.
#[test]
fn an_empty_listing_and_an_unlistable_venue_never_render_alike() {
    let empty = CatalogListing {
        venue: "binance".to_string(),
        outcome: CatalogOutcome::Listed {
            instruments: Vec::new(),
            truncated: false,
            cached: false,
        },
    };
    let none = CatalogListing {
        venue: "ig".to_string(),
        outcome: CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
            why: "searched live per query".to_string(),
        }),
    };
    // The DOCUMENT: a count against a null, under two different tokens.
    let empty_doc = outcome_json(&empty.outcome);
    let none_doc = outcome_json(&none.outcome);
    assert_eq!(empty_doc["outcome"], "listed");
    assert_eq!(empty_doc["listed"], 0);
    assert_eq!(none_doc["outcome"], "refused");
    assert_eq!(none_doc["refusal"], "no_bulk_list");
    assert!(none_doc["listed"].is_null(), "a refusal counts nothing: {none_doc}");
    assert_ne!(empty_doc, none_doc);
    // The TABLE: the empty listing renders an empty-note and the venue's own sentence; the
    // refusal never reaches this renderer at all (it is the EMPTY rung), and its sentence is
    // the wire's.
    let lines = ls_lines(&[], 0, false, false, &empty.describe());
    assert!(lines.iter().any(|l| l.contains("no instruments")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("0 instruments")), "{lines:?}");
    assert!(none.describe().contains("publishes no bulk instrument list"), "{}", none.describe());
    assert!(!none.describe().contains("0 instruments"), "{}", none.describe());
}

/// Which VARIANT of `CatalogRefusal` a sample is, as an index into the sample list below.
///
/// ⚠ **This match is the completeness gate, and it is the COMPILER's.** It carries no `_` arm,
/// so a variant added to `vike_datahub_client::catalog::CatalogRefusal` stops this file
/// compiling until it is given an index — and `every_refusal_carries_its_own_token_and_none_
/// of_them_counts_anything` then demands a sample for that index. It is the same property
/// [`live_lanes`]' destructure buys, bought for an enum rather than a struct.
fn refusal_slot(refusal: &CatalogRefusal) -> usize {
    // ⚠ IF YOU ARE HERE BECAUSE THE COMPILER DEMANDED AN ARM: give it the next free slot AND
    // bump [`REFUSAL_VARIANTS`] below, then add a sample to the test. Without the bump the
    // completeness check silently stops covering your variant — which is the exact hole this
    // pair was rewritten to close.
    match refusal {
        CatalogRefusal::NoBulkList { why: _ } => 0,
        CatalogRefusal::NeedsCredentials => 1,
        CatalogRefusal::NotServed { supported: _ } => 2,
    }
}

/// How many variants [`refusal_slot`] assigns a slot to.
///
/// ⚠ **This exists because the completeness check compared a derivation against its own
/// input.** It read `slots == (0..refusals.len())`, and BOTH sides came from the same
/// hand-typed three-element sample array — so a fourth variant given slot 3 and no sample left
/// `slots == [0, 1, 2]` and `0..3`, equal, green, with two distinct refusals free to render as
/// one token in every `--json` document. Against a count that the samples cannot move, the same
/// mistake is `[0, 1, 2] != 0..4` and fails by name.
///
/// It is a hand-maintained number and that is the residual, declared rather than hidden: the
/// compiler forces the author to the match above, and the note there is what carries them here.
const REFUSAL_VARIANTS: usize = 3;

/// Every non-`Listed` outcome gets its OWN token, and none of them counts anything.
///
/// ⚠ **The doc used to claim "an exhaustive check rather than a spot one" over a HAND-TYPED
/// three-element array, and it was neither.** Nothing forced a new `CatalogRefusal` variant
/// into that array — the compiler forces an ARM in [`outcome_json`], never a ROW here — so
/// adding one and giving it `("not_served", Null)` would have shipped two distinct refusals
/// rendering as one token in every `--json` document, green. It also omitted
/// `CatalogRefusal::NoBulkList` entirely, checking that variant's token only incidentally in
/// `an_empty_listing_and_an_unlistable_venue_never_render_alike`.
///
/// What makes it exhaustive now is [`refusal_slot`]: its match has no `_`, and the two
/// assertions below turn a missing sample into a failure rather than a silent shortfall.
#[test]
fn every_refusal_carries_its_own_token_and_none_of_them_counts_anything() {
    let refusals = [
        CatalogRefusal::NoBulkList { why: "searched live per query".to_string() },
        CatalogRefusal::NeedsCredentials,
        CatalogRefusal::NotServed { supported: vec!["binance".to_string(), "okx".to_string()] },
    ];
    // THE COMPLETENESS HALF, against [`REFUSAL_VARIANTS`] and NOT against `refusals.len()`.
    // ⚠ It compared the slots to `0..refusals.len()` — a derivation against its own input, so
    // a fourth variant with no sample here compared `[0,1,2]` to `0..3` and passed.
    let mut slots: Vec<usize> = refusals.iter().map(refusal_slot).collect();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(
        slots,
        (0..REFUSAL_VARIANTS).collect::<Vec<_>>(),
        "every `CatalogRefusal` variant needs exactly one sample above: {slots:?}"
    );
    assert_eq!(
        refusals.len(),
        REFUSAL_VARIANTS,
        "one sample per variant, no duplicates — {} samples for {REFUSAL_VARIANTS} variants",
        refusals.len()
    );

    let expected_supported =
        [serde_json::Value::Null, serde_json::Value::Null, serde_json::json!(["binance", "okx"])];
    // `NotArmed` is not a refusal and carries its own outcome token, so it is checked beside
    // them rather than through the slot machinery.
    let mut tokens: Vec<(String, String)> = vec![{
        let doc = outcome_json(&CatalogOutcome::NotArmed);
        assert_eq!(doc["outcome"], "not_armed");
        assert!(doc["listed"].is_null(), "{doc}");
        (doc["outcome"].to_string(), doc["refusal"].to_string())
    }];
    for (refusal, supported) in refusals.iter().zip(expected_supported) {
        let doc = outcome_json(&CatalogOutcome::Refused(refusal.clone()));
        assert_eq!(doc["outcome"], "refused");
        assert!(doc["listed"].is_null(), "{doc}");
        assert!(doc["truncated"].is_null(), "{doc}");
        assert!(doc["cached"].is_null(), "{doc}");
        assert_eq!(doc["supported"], supported);
        assert!(!doc["refusal"].is_null(), "a refusal must carry a token of its own: {doc}");
        tokens.push((doc["outcome"].to_string(), doc["refusal"].to_string()));
    }
    tokens.sort();
    let before = tokens.len();
    tokens.dedup();
    assert_eq!(before, tokens.len(), "two outcomes render as one token: {tokens:?}");
}

/// A truncated listing under a FILTER owes the reader a sentence the wire cannot give — and
/// the control is the same listing with no filter, where the wire's own sentence is enough.
#[test]
fn a_truncated_listing_warns_only_where_the_filter_could_hide_the_tail() {
    assert!(truncation_warning(true, true)[0].contains("TRUNCATED"));
    assert!(truncation_warning(true, false).is_empty(), "the wire already said so");
    assert!(truncation_warning(false, true).is_empty(), "nothing was truncated");
    assert!(truncation_warning(false, false).is_empty());
}

/// `refresh` must SAY it re-asked nothing when the server answered from its memo, because its
/// own name promises the opposite. The control is the fresh arm, which says the venue was
/// called.
#[test]
fn a_cached_refresh_says_it_re_asked_nothing_and_a_fresh_one_says_it_called_the_venue() {
    let cached = refresh_lines(12, true, "`okx`: 12 instruments (from this server's cache).");
    assert!(cached.iter().any(|l| l.contains("NOTHING was re-asked")), "{cached:?}");
    assert!(cached.iter().any(|l| l.contains("TTL")), "{cached:?}");
    let fresh = refresh_lines(12, false, "`okx`: 12 instruments.");
    assert!(fresh.iter().any(|l| l.contains("called the venue")), "{fresh:?}");
    assert!(!fresh.iter().any(|l| l.contains("NOTHING was re-asked")), "{fresh:?}");
}

/// The matrix is the canonical roster, every row of it, and nothing typed here. A venue added
/// to `vike_model::VENUES` appears with no edit to this file — and a row this file invented
/// would fail the second half.
#[test]
fn every_roster_venue_has_a_row_and_no_row_names_a_venue_off_the_roster() {
    let rows = matrix();
    assert_eq!(rows.len(), vike_model::VENUES.len());
    for (row, venue) in rows.iter().zip(vike_model::VENUES) {
        assert_eq!(&row.venue, venue, "the matrix must be the roster, in its order");
    }
    // Anti-vacuity: the roster is not empty, and the matrix is not uniformly blank — at least
    // one venue declares a live lane and at least one declares a backfill kind, so the
    // renderers below are exercised on real values rather than on a table of dashes.
    assert!(!rows.is_empty());
    assert!(rows.iter().any(|r| !r.live.is_empty()), "no venue declares a live lane");
    assert!(rows.iter().any(|r| r.backfill_bars), "no venue declares a bar backfill");
}

/// Every lane the model declares is NAMED here. The completeness half is the compiler's — see
/// [`live_lanes`] — and this pins the half it cannot check: that the names are distinct and
/// that an all-on row yields all of them.
#[test]
fn every_live_lane_the_model_declares_has_a_distinct_name() {
    let all = LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: true };
    let mut names = live_lanes(&all);
    assert_eq!(names.len(), 5, "an all-on row must name every lane: {names:?}");
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "two lanes share a name: {names:?}");
    // The control: nothing on names nothing, so the assertion above is not passing because
    // this function returns a constant.
    assert!(live_lanes(&LiveDataCaps::NONE).is_empty());
}

fn venue_row(venue: &'static str, live: &[&'static str], bars: bool, ticks: bool) -> VenueRow {
    VenueRow { venue, live: live.to_vec(), backfill_bars: bars, backfill_ticks: ticks }
}

/// **§8.1's demand, as a test.** The build's columns and the server's column are three
/// independent statements, and the rendering must let a reader see WHICH side said no. So a
/// venue this build declares four live lanes for, against a server that advertises none, still
/// shows those four lanes AND a `not served` cell — and the skew line names it.
#[test]
fn the_build_column_and_the_server_column_are_never_merged_into_one_verdict() {
    let rows = vec![venue_row("binance", &["bars", "trades"], true, false)];
    let view = ServerView::Answered(vec!["inventory".to_string()]);
    let lines = venues_lines(&rows, &view, "127.0.0.1:7878");
    let body = lines.join("\n");
    assert!(body.contains("bars,trades"), "the build's lanes survive: {body}");
    assert!(body.contains("not served"), "the server's answer is its own cell: {body}");
    assert!(body.contains("SKEW"), "the difference is named: {body}");
    assert!(body.contains("that datahub advertises none"), "{body}");
    // ⚠ ...and it names NO ROUTE. The sentence this replaced told the operator that a
    // `data realtime watch` on a skewed venue "is refused by the server, not by this binary".
    // When it was struck, BOTH halves were false because the route did not exist — that group
    // was refused on this binary's own usage rung. `crate::cmd::data::realtime`'s `watch` ships
    // now, so the first half is true; the SECOND half is still false, and this fixture is
    // exactly the case that shows it. `ServerView::Answered(vec!["inventory"])` advertises no
    // market-data plane, so `vike_datahub_client::DatahubClient::md_subscribe` refuses on the
    // capability LOCALLY — "nothing was sent" — and the refusal that sentence attributed to the
    // server never reaches it. Which side said no is the one thing this verb keeps legible.
    assert!(
        !body.contains("data realtime"),
        "the skew line may not attribute a refusal to a side that did not make it: {body}"
    );
    // The control: a server that DOES advertise it renders the same build columns with a
    // different server cell and NO skew line.
    let served = ServerView::Answered(vec![vike_datahub_client::md_venue_feature("binance")]);
    let ok = venues_lines(&rows, &served, "127.0.0.1:7878").join("\n");
    assert!(ok.contains("bars,trades"), "{ok}");
    // ⚠ `contains("served")` would also match `not served`, so the control asserts the
    // NEGATIVE cell is absent — an assertion that cannot pass for the wrong reason.
    assert!(!ok.contains("not served"), "{ok}");
    assert!(ok.contains("served"), "{ok}");
    assert!(!ok.contains("SKEW"), "nothing differs, so nothing is reported: {ok}");
}

/// A server NEWER than this binary — it serves a venue this roster does not carry — is the
/// other direction of the same skew, and it is the one an operator can otherwise diagnose only
/// by reading two build logs.
#[test]
fn a_venue_the_server_serves_and_this_roster_does_not_name_is_reported_as_a_version_skew() {
    let rows = matrix();
    let mut features: Vec<String> =
        rows.iter().map(|r| vike_datahub_client::md_venue_feature(r.venue)).collect();
    features.push(vike_datahub_client::md_venue_feature("nextvenue"));
    let view = ServerView::Answered(features);
    let s = skew(&rows, &view).expect("the server answered");
    assert_eq!(s.served_there_unknown_here, vec!["nextvenue".to_string()]);
    assert!(
        s.declared_here_unserved_there.is_empty(),
        "every roster venue is served in this fixture: {s:?}"
    );
    let body = venues_lines(&rows, &view, "127.0.0.1:7878").join("\n");
    assert!(body.contains("OLDER than that server"), "{body}");
    assert!(body.contains("nextvenue"), "{body}");
}

/// **§8.1's other demand.** With no datahub the local columns are the whole answer and the
/// verb still answers: every roster venue is rendered, the server column says UNASKED rather
/// than unserved, and the document carries `null` rather than an empty list.
///
/// ⚠ **The fixture is an `io::Error`'s OWN text, and it is built that way because the hand-typed
/// one drifted.** It read `cannot connect to datahub at 127.0.0.1:1` — [`connect`]'s prefix,
/// which [`ask_the_server`] deliberately does NOT add (the address is already on that line, and
/// the prefix printed it twice). So this test showed a reader the doubled shape the fix had
/// removed, and stayed green because it only looked for `NOT REACHED`. The message now comes
/// from the same place production's does — an `io::Error`, stringified — and the count
/// assertion below is what would fail if the prefix ever came back.
#[test]
fn an_unreachable_datahub_still_renders_every_local_row_and_says_the_column_is_unasked() {
    let rows = matrix();
    let view =
        ServerView::Unreachable(io::Error::from(io::ErrorKind::ConnectionRefused).to_string());
    let body = venues_lines(&rows, &view, "127.0.0.1:1").join("\n");
    for r in &rows {
        assert!(body.contains(r.venue), "{} is missing: {body}", r.venue);
    }
    assert!(body.contains("NOT REACHED"), "{body}");
    assert_eq!(
        body.matches("127.0.0.1:1").count(),
        1,
        "the address is named ONCE — this view carries the client's own sentence, without the \
             `cannot connect to datahub at {{addr}}` prefix `connect` adds: {body}"
    );
    assert!(body.contains("unasked, which is not the same as unserved"), "{body}");
    assert!(!body.contains("not served"), "nothing may claim the server refused: {body}");
    assert!(skew(&rows, &view).is_none(), "there is no difference to state");
    // ...and `?` is not `false` in the document either.
    assert_eq!(view.serves(FEATURE_BACKFILL), None);
    assert_eq!(ServerView::Answered(Vec::new()).serves(FEATURE_BACKFILL), Some(false));
}

/// **A server that ANSWERED and said no is not an absent server**, and this is the case the
/// two-variant [`ServerView`] could not express.
///
/// ⚠ The defect it pins is measured rather than hypothetical: `crate::cmd::data`'s [`connect`]
/// folds a denied mac, a keyed server with no keys in the store, and a PROTO_VERSION skew into
/// the same `CliError::connect` sentence, so `execute_venues` rendered every one of them as
/// `NOT REACHED` with `"reachable": false` — in the verb whose own module doc says it exists
/// to surface a version skew. [`ask_the_server`] reads the `io::ErrorKind` instead, and this
/// holds the three renderings apart.
#[test]
fn a_server_that_answered_and_refused_is_not_rendered_as_an_absent_one() {
    let rows = matrix();
    let refused = ServerView::Refused(
        "datahub protocol version mismatch: client speaks 9, server speaks 10".to_string(),
    );
    let body = venues_lines(&rows, &refused, "127.0.0.1:7878").join("\n");
    assert!(body.contains("REACHED, and it REFUSED"), "{body}");
    assert!(
        !body.contains("NOT REACHED"),
        "a server that answered may not be reported as absent: {body}"
    );
    assert!(body.contains("PROTOCOL VERSION SKEW"), "the cause an operator acts on: {body}");
    assert_eq!(
        body.matches("127.0.0.1:7878").count(),
        1,
        "the address is named ONCE here too — same view, same client sentence: {body}"
    );
    // ⚠ …and the `?` column is attributed to THIS side. The sentence this replaced said the
    // server "was reached and never asked", which is false for the commonest served refusal:
    // a keyed datahub met with no keys advertised its venues in the `Welcome` and this binary
    // discarded them. See the arm in `venues_lines`.
    assert!(
        !body.contains("never asked"),
        "this side's own discard may not be reported as the server saying nothing: {body}"
    );
    assert!(body.contains("not the server's silence"), "…and it says whose choice it is: {body}");
    // The build's own columns are untouched — the whole verb still answers.
    for r in &rows {
        assert!(body.contains(r.venue), "{} is missing: {body}", r.venue);
    }
    // ⚠ [`SERVER_VERBS`] is the ANSWERED arm's alone, and this is the assertion that holds it
    // there: with no advertisement to read, [`ServerView::serves`] is `None` for every one of
    // them and the loop renders `was not asked for` — the same false claim about the far side
    // wearing a spelling the ban above does not match.
    //
    // ⚠ It replaces `!body.contains("not served")`, a string `venues_lines` renders in NO arm
    // (the answered one says `does NOT serve`), so that assertion could not fail for its stated
    // reason — and its message, "nothing was asked, so nothing was refused", was the deleted
    // claim itself, three lines under the assertion that forbids it.
    assert!(
        !body.contains("was not asked for"),
        "a refused server's capability rows may not be rendered, least of all as unasked: \
             {body}"
    );

    // The DOCUMENT: three tokens, because `reachable` was a boolean answering a three-state
    // question — and for this state it answered `false` about a server that was reached.
    assert_eq!(refused.state(), "refused");
    assert_eq!(ServerView::Unreachable("closed".to_string()).state(), "unreachable");
    assert_eq!(ServerView::Answered(Vec::new()).state(), "answered");
    let args = parse_of(&["venues", "--json"]).expect("parses");
    let doc: serde_json::Value =
        serde_json::from_str(&venues_json(&args, &rows, &refused)).expect("one document");
    assert_eq!(doc["server"]["state"], "refused");
    assert!(doc["server"]["features"].is_null(), "a discarded handshake advertised nothing");
    assert!(doc["skew"].is_null(), "there is no difference to state: {doc}");
}

/// The `io::ErrorKind` split [`ask_the_server`] turns on, as a table — the kinds the client
/// produces when the far side SPOKE, against the ones that mean nothing answered.
///
/// It is a unit test of the CLASSIFIER rather than of a connection, deliberately: the three
/// served refusals need a server that denies a mac, one that speaks another PROTO_VERSION and
/// one that is not a datahub at all, and `DatahubClient`'s own tests are where those live. What
/// this file owns is the decision made on the kind they each arrive as.
#[test]
fn the_answered_and_refused_kinds_are_the_ones_the_far_side_spoke_on() {
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData] {
        assert!(answered_and_refused(kind), "{kind:?} is a served refusal");
    }
    for kind in [
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionReset,
    ] {
        assert!(!answered_and_refused(kind), "{kind:?} is a socket, not an answer");
    }
}

/// Both server-wide rows are rendered with their own verdict and their own cost, and neither
/// is a per-venue column — see [`SERVER_VERBS`].
#[test]
fn the_server_wide_capabilities_are_reported_with_what_their_absence_costs() {
    let rows = matrix();
    let view = ServerView::Answered(vec![FEATURE_BACKFILL.to_string()]);
    let body = venues_lines(&rows, &view, "127.0.0.1:7878").join("\n");
    assert!(body.contains(&format!("serves `{FEATURE_BACKFILL}`")), "{body}");
    assert!(body.contains(&format!("does NOT serve `{FEATURE_VENUE_CATALOG}`")), "{body}");
    for (_, why) in SERVER_VERBS {
        assert!(body.contains(*why), "the cost must be stated: {body}");
    }
}

/// The documents name the group and the verb from the declarations rather than as literals,
/// and the `venues` document keeps the two sources APART — which is the shape a consumer
/// branches on.
#[test]
fn the_documents_derive_their_verb_and_keep_the_two_sources_apart() {
    let args = parse_of(&["venues", "--json"]).expect("parses");
    let rows = matrix();
    let doc: serde_json::Value = serde_json::from_str(&venues_json(
        &args,
        &rows,
        &ServerView::Unreachable("closed".to_string()),
    ))
    .expect("one JSON document");
    assert_eq!(doc["group"], "catalog");
    assert_eq!(doc["verb"], Verb::Venues.as_str());
    assert_eq!(doc["build"]["venues"].as_array().expect("rows").len(), rows.len());
    assert_eq!(doc["server"]["state"], "unreachable");
    assert!(doc["server"]["features"].is_null(), "unasked is null, never []: {doc}");
    assert!(doc["skew"].is_null());
    // There is deliberately NO merged per-venue verdict anywhere in the document.
    assert!(doc["venues"].is_null(), "the two sources must not be flattened: {doc}");
}

/// The `show` rendering names its SOURCE and says which absence it is looking at — the trap
/// the module doc opens with, at the one place a reader can act on it.
#[test]
fn show_names_the_store_as_its_source_and_says_when_a_grid_is_empty() {
    let target = InstrumentRef { venue: "okx".to_string(), symbol: "BTC-USDT".to_string() };
    let recorded = vike_model::SymbolProperties {
        tick_size: 0.1,
        step_size: 0.001,
        asset_class: Some(AssetClass::CryptoPerp),
        ..Default::default()
    };
    let good = show_lines(&target, &recorded).join("\n");
    assert!(good.contains("kind=properties"), "the source is named: {good}");
    assert!(good.contains(AssetClass::CryptoPerp.sql_word()), "{good}");
    assert!(good.contains("0.1"), "{good}");
    assert!(!good.contains("names no tick size"), "this grid has one: {good}");
    // A DEFAULT grid: a row exists, so something recorded it — and every number is the model's
    // absent-is-zero rather than a fact.
    let empty = show_lines(&target, &vike_model::SymbolProperties::default()).join("\n");
    assert!(empty.contains("unclassified"), "{empty}");
    assert!(empty.contains("names no tick size"), "{empty}");
    assert!(!empty.contains("  tick size      0"), "a zero must not render as a number: {empty}");
}

/// `ls`'s TABLE on real rows — the path
/// `an_empty_listing_and_an_unlistable_venue_never_render_alike` cannot reach, since that case
/// renders the EMPTY answer. A width bug or a lost column is invisible without this.
#[test]
fn the_listing_table_renders_every_column_and_says_what_a_filter_narrowed() {
    let mut bare = row("ETH-PERP", AssetClass::CryptoPerp);
    bare.base = "ETH".to_string();
    bare.quote = "USD".to_string();
    bare.description = String::new();
    bare.tick = 0.0;
    bare.lot = 0.0;
    let rows = vec![row("BTCUSDT", AssetClass::CryptoSpot), bare];

    let body = ls_lines(&rows, 400, true, false, "`binance`: 400 instruments.").join("\n");
    for header in ["SYMBOL", "CLASS", "BASE", "QUOTE", "TICK", "LOT", "DESCRIPTION"] {
        assert!(body.contains(header), "the {header} column is missing: {body}");
    }
    assert!(body.contains("BTCUSDT") && body.contains("ETH-PERP"), "{body}");
    assert!(body.contains(AssetClass::CryptoPerp.sql_word()), "{body}");
    assert!(body.contains("2 of 400 instruments"), "a filter states both sides: {body}");
    // ⚠ The absent-grid row carries NO DIGIT at all — every one of its cells is a word or a
    // dash. A `0` here would be the model's absent rendered as a fact, which is the whole of
    // [`grid_cell`]'s argument, checked on the real row rather than on the helper.
    let eth = body.lines().find(|l| l.starts_with("ETH-PERP")).expect("the bare row");
    assert!(!eth.contains('0'), "an absent grid must not render as a zero: {eth}");
    // ...and the control: the row that HAS a grid prints it.
    let btc = body.lines().find(|l| l.starts_with("BTCUSDT")).expect("the populated row");
    assert!(btc.contains("0.01") && btc.contains("0.001"), "{btc}");

    // An UNNARROWED listing states one number, not two: "2 of 2" would invent a filter.
    let whole = ls_lines(&rows, 2, false, false, "`binance`: 2 instruments.").join("\n");
    assert!(!whole.contains("2 of 2"), "nothing was narrowed: {whole}");
    assert!(whole.contains("`binance`: 2 instruments."), "{whole}");
}

/// The `ls` document carries the RAW grid and echoes what narrowed it — the two things a
/// machine reader cannot recover from the table.
///
/// ⚠ The listing's own `instruments` is empty here while the rows are passed apart, and that is
/// exactly the split [`execute_ls`] performs: `vike_catalog::Instrument` cannot be CONSTRUCTED
/// in this crate (see [`InstrumentRow`]), and by the time this renderer sees a row it is
/// already flattened. The outcome fields and the row fields therefore come from the two halves
/// independently, which is what this case checks.
#[test]
fn the_listing_document_carries_the_raw_grid_and_echoes_the_filter() {
    let args =
        parse_of(&["ls", "--venue", "binance", "--class", "CryptoPerp", "--json"]).expect("parses");
    let listing = CatalogListing {
        venue: "binance".to_string(),
        outcome: CatalogOutcome::Listed { instruments: Vec::new(), truncated: true, cached: true },
    };
    let mut r = row("ETHUSDT", AssetClass::CryptoPerp);
    r.tick = 0.0;
    let doc: serde_json::Value = serde_json::from_str(&ls_json(&args, &listing, &[r]))
        .expect("the document is one JSON object");
    assert_eq!(doc["verb"], Verb::Ls.as_str());
    assert_eq!(doc["venue"], "binance");
    assert_eq!(doc["shown"], 1);
    assert_eq!(doc["filter"]["class"], AssetClass::CryptoPerp.sql_word());
    assert!(doc["filter"]["search"].is_null(), "an unused filter is null: {doc}");
    assert_eq!(doc["outcome_detail"]["truncated"], true);
    assert_eq!(doc["outcome_detail"]["cached"], true);
    // ⚠ The RAW `0.0`, never the table's dash: a machine reader asked for the grid in order to
    // fold it, and a dash is this side's reading rather than the datum.
    assert_eq!(doc["instruments"][0]["tick_size"], 0.0);
    assert_eq!(doc["instruments"][0]["asset_class"], AssetClass::CryptoPerp.sql_word());
}

/// The two documents a NON-listing answer produces. Both must be unmistakable for a listing:
/// the refusal carries no `instruments` key at all, and an unrecorded instrument carries no
/// zeroed grid — an absent number and a recorded zero are different facts about the world.
///
/// ⚠ **Each half is now checked against the renderer its own verb REACHES**, which it was not:
/// both halves ran on `refresh`'s `Args` while [`execute_refresh`] emitted
/// [`outcome_only_json`] on that path, so the `reasked: null` assertion described a document
/// this binary never produced. `ls` refuses through [`outcome_only_json`] and `refresh`
/// through [`refresh_json`], and that is how they are exercised here.
#[test]
fn a_refusal_and_an_unrecorded_instrument_carry_no_rows_and_no_zeroes() {
    let ls_args = parse_of(&["ls", "--venue", "ig", "--json"]).expect("parses");
    let refresh_args = parse_of(&["refresh", "--venue", "ig", "--json"]).expect("parses");
    let listing = CatalogListing {
        venue: "ig".to_string(),
        outcome: CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
            why: "searched live per query".to_string(),
        }),
    };
    let refused: serde_json::Value =
        serde_json::from_str(&outcome_only_json(&ls_args, &listing)).expect("a document");
    assert_eq!(refused["verb"], Verb::Ls.as_str());
    assert_eq!(refused["outcome_detail"]["refusal"], "no_bulk_list");
    assert!(refused["instruments"].is_null(), "a refusal lists nothing: {refused}");
    // ...and `refresh`'s own field is PRESENT and null rather than absent or a misleading
    // `false`: nothing was listed, so "did not re-ask" would invite a wrapper to retry
    // forever, while an absent key is the shape `ls` emits and tells this verb's consumer
    // nothing at all.
    let doc: serde_json::Value =
        serde_json::from_str(&refresh_json(&refresh_args, &listing)).expect("a document");
    assert_eq!(doc["verb"], Verb::Refresh.as_str());
    assert!(doc["reasked"].is_null(), "{doc}");
    assert!(
        doc.get("reasked").is_some(),
        "the key must be THERE — an absent one is `ls`'s document, not this verb's: {doc}"
    );
    // The control: a LISTED answer puts a real boolean in it, so the null above is the
    // refusal's own value rather than a field this renderer never fills.
    let listed = CatalogListing {
        venue: "ig".to_string(),
        outcome: CatalogOutcome::Listed { instruments: Vec::new(), truncated: false, cached: true },
    };
    let fresh: serde_json::Value =
        serde_json::from_str(&refresh_json(&refresh_args, &listed)).expect("a document");
    assert_eq!(fresh["reasked"], false, "a cached listing re-asked nothing: {fresh}");

    let show_args = parse_of(&["show", "okx:BTC-USDT", "--json"]).expect("parses");
    let target = InstrumentRef { venue: "okx".to_string(), symbol: "BTC-USDT".to_string() };
    let missing: serde_json::Value =
        serde_json::from_str(&show_missing_json(&show_args, &target)).expect("a document");
    assert_eq!(missing["recorded"], false);
    assert!(missing["tick_size"].is_null(), "no grid field may be zeroed: {missing}");
    assert!(missing["asset_class"].is_null(), "{missing}");
    // The control: a RECORDED default grid does carry the zeroes, because they were recorded.
    let recorded: serde_json::Value = serde_json::from_str(&show_json(
        &show_args,
        &target,
        &vike_model::SymbolProperties::default(),
    ))
    .expect("a document");
    assert_eq!(recorded["recorded"], true);
    assert_eq!(recorded["tick_size"], 0.0);
}

/// `-` is the model's absent-is-`0.0`, and a real number is a real number. Paired, because a
/// renderer that dashed everything would pass the first half alone.
#[test]
fn an_absent_grid_number_is_a_dash_and_a_present_one_is_itself() {
    assert_eq!(grid_cell(0.0), "-");
    assert_eq!(grid_cell(-1.0), "-");
    assert_eq!(grid_cell(0.001), "0.001");
    assert_eq!(grid_cell(100.0), "100");
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
/// AND `crates/vike-cli/tests/data_cli.rs`'s
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
