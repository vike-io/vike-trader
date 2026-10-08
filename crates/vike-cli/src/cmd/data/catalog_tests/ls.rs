//! `ls`, `refresh` and `show`: the filters, the refusal tokens, and each table and document.
use super::*;

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
