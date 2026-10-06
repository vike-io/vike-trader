use std::net::TcpListener;

use vike_datahub_client::proto::write_frame;
use vike_model::{BookLevel, TradeTick};

use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(), None)
}

fn watch(extra: &[&str]) -> Vec<String> {
    let mut v = vec!["watch".to_string(), "binance:BTCUSDT".to_string()];
    v.extend(extra.iter().map(|s| (*s).to_string()));
    v
}

fn spec(lane: MdLane, depth: Option<u16>) -> MdSpec {
    MdSpec { venue: "binance".into(), symbol: "BTCUSDT".into(), lane, depth_levels: depth }
}

fn snapshot() -> BookSnapshot {
    BookSnapshot {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        tick_size: 0.1,
        bids: vec![BookLevel::new(100.2, 3.0), BookLevel::new(100.1, 5.0)],
        asks: vec![BookLevel::new(100.3, 2.0)],
        venue_ts: 1_700_000_000_000,
        venue_seq: 42,
        seq: 7,
    }
}

fn row(frame: &MdFrame) -> serde_json::Value {
    serde_json::from_str(&jsonl_row(frame)).expect("every row is one JSON document")
}

// ── the grammar ──────────────────────────────────────────────────────────────────────────

/// EVERY lane is reachable by the word this group advertises, and the no-`_` match is the
/// load-bearing half: a new [`MdLane`] variant must fail to COMPILE here rather than silently
/// being a lane no operator can type. (Stable Rust cannot enumerate variants, so this is the
/// only available backstop for [`LANES`]' completeness — the same one
/// `vike_datahub_client::market`'s own suite uses.)
#[test]
fn every_lane_is_reachable_by_the_word_it_advertises() {
    for lane in LANES.iter().copied() {
        match lane {
            MdLane::Depth | MdLane::Book | MdLane::Trades => {}
        }
        let word = lane.feed_stream_label();
        assert_eq!(parse_lane(word), Ok(lane), "`--lane {word}` must resolve to {lane:?}");
        assert!(lane_roster().contains(word), "the roster must name {word}: {}", lane_roster());
    }
    assert_eq!(LANES.len(), 3, "three lanes, and the match above is what proves it is all of them");
}

/// The CLI word IS the wire's own lane label, pinned in both directions.
///
/// ⚠ `MdLane::feed_stream_label`'s doc warns that it is string-keyed between two INDEPENDENT
/// venue producers, so a producer that renames a label would silently rename an operator's flag
/// value. This is what makes that visible: if it reddens, the decision is whether the word an
/// operator types moves with the feed's label — not whether to re-spell it here.
#[test]
fn the_lane_words_are_the_wires_own_labels() {
    assert_eq!(MdLane::Depth.feed_stream_label(), "depth");
    assert_eq!(MdLane::Book.feed_stream_label(), "book");
    assert_eq!(MdLane::Trades.feed_stream_label(), "trades");
    // ...and the parse round-trips through the wire's own reader, so this verb cannot accept a
    // word the wire would not.
    for lane in LANES {
        assert_eq!(MdLane::from_feed_stream_label(lane.feed_stream_label()), Some(*lane));
    }
}

/// §8.3: a quotes lane is refused BY NAME with the CONTRACT, and never mapped onto depth.
///
/// The anti-vacuity control is the third assertion: an unknown word gets a DIFFERENT answer, so
/// this cannot be passing because every value is refused alike.
#[test]
fn the_quotes_lane_is_refused_by_name_with_its_reason() {
    let why = parse_lane("quotes").expect_err("there is no quotes lane");
    assert!(why.contains("no quotes lane"), "{why}");
    assert!(why.contains("conflates"), "it must give the loss contract, not just a no: {why}");
    assert!(why.contains("tape gap"), "...and what the wire discloses instead: {why}");
    assert!(why.contains("depth"), "...and that depth is not a substitute: {why}");

    let bars = parse_lane("bars").expect_err("there is no bar lane either");
    assert!(bars.contains("data hist fetch"), "a bar is history, and it says where: {bars}");

    let unknown = parse_lane("frobnicate").expect_err("not a lane");
    assert!(unknown.contains("unknown"), "an unknown word is not a designed one: {unknown}");
    assert!(!unknown.contains("conflates"), "{unknown}");
}

/// §8.3's headline: a `watch` with no bound is a USAGE error naming all three ways to give one.
#[test]
fn an_unbounded_stream_must_be_asked_for() {
    let err = parse(&watch(&["--lane", "trades"]), None).expect_err("no bound");
    assert!(err.contains("BOUNDED by default"), "{err}");
    for way in ["--for", "--events", "--unbounded"] {
        assert!(err.contains(way), "the refusal must name {way}: {err}");
    }
    // ...and each of the three IS accepted, which is what stops this passing because `watch`
    // refuses everything.
    for bound in [vec!["--for", "30s"], vec!["--events", "5"], vec!["--unbounded"]] {
        let mut argv = watch(&["--lane", "trades"]);
        argv.extend(bound.iter().map(|s| (*s).to_string()));
        assert!(parse(&argv, None).is_ok(), "{bound:?} is a bound: {argv:?}");
    }
    // BOTH together is one bound, not a contradiction — whichever lands first.
    let both = parse(&watch(&["--lane", "trades", "--for", "30s", "--events", "5"]), None)
        .expect("both is one bound");
    assert_eq!(
        both.bound,
        Bound::First { events: Some(5), duration: Some(Duration::from_secs(30)) }
    );
    // ...while --unbounded WITH one is the contradiction.
    let clash = parse(&watch(&["--lane", "trades", "--for", "30s", "--unbounded"]), None)
        .expect_err("a stop and a never");
    assert!(clash.contains("contradicts"), "{clash}");
}

/// `--for` admits SECONDS — which `vike_model::time::parse_span` deliberately does not — and
/// refuses the spans that mean something else here, each by name.
#[test]
fn the_for_grammar_is_seconds_minutes_hours_and_says_why_it_stops_there() {
    assert_eq!(parse_for("30s"), Ok(Duration::from_secs(30)));
    assert_eq!(parse_for("5m"), Ok(Duration::from_secs(300)));
    assert_eq!(parse_for("2h"), Ok(Duration::from_secs(7_200)));
    // The workspace grammar's own refusals, kept: a bare number and a zero count.
    assert!(parse_for("90").expect_err("no unit").contains("no unit"));
    assert!(parse_for("0s").expect_err("zero").contains("zero-length"));
    // The units that are refused BY NAME, with what they mean here.
    for long in ["1d", "2w", "3mo", "1y"] {
        let why = parse_for(long).expect_err("longer than a watch");
        assert!(why.contains("record"), "{long}: it must name the verb that is for it: {why}");
        assert!(why.contains("--unbounded"), "{long}: ...and what exists today: {why}");
    }
    let bars = parse_for("500bars").expect_err("no bar lane");
    assert!(bars.contains("no bar lane"), "{bars}");
    // The `M`/`m` trap this workspace's own duration grammar refuses for the same reason.
    let upper = parse_for("1M").expect_err("M is not a unit here");
    assert!(upper.contains("minutes"), "{upper}");
}

/// The SPEC grammar: two parts, the venue unvalidated, the symbol validated by the WIRE's own
/// rule — and a three-part series spelling refused with where an interval belongs.
#[test]
fn the_spec_is_a_live_key_and_a_series_spelling_is_refused_by_name() {
    assert_eq!(
        parse_key("binance:BTCUSDT"),
        Ok(Key { venue: "binance".into(), symbol: "BTCUSDT".into() })
    );
    // ⚠ `@` is NOT a group marker here: hyperliquid spells real instruments that way, and the
    // symbol is handed to the venue verbatim.
    assert_eq!(
        parse_key("hyperliquid:@107"),
        Ok(Key { venue: "hyperliquid".into(), symbol: "@107".into() })
    );
    // A venue nobody has heard of is NOT refused here — the reachable set belongs to the server.
    assert!(parse_key("frobnicate:X").is_ok(), "no venue roster lives in this crate");

    let series = parse_key("binance:BTCUSDT:1h").expect_err("a series, not a key");
    assert!(series.contains("INTERVAL"), "{series}");
    assert!(series.contains("data hist"), "it must say where an interval belongs: {series}");
    for bad in ["binance", "binance:", ":BTCUSDT", "", "a:b:c:d"] {
        assert!(parse_key(bad).is_err(), "{bad:?} is not VENUE:SYMBOL");
    }
    // The SYMBOL rule is the wire's own, reached before a socket opens — a blank one and an
    // over-long one are both the validator's words, forwarded.
    let long =
        parse_key(&format!("binance:{}", "A".repeat(97))).expect_err("over MD_MAX_SYMBOL_BYTES");
    assert!(long.contains("97"), "the validator names the length: {long}");
}

/// A positional carrying an `=` survives the flag splitter WHOLE.
///
/// ⚠ `Flags::next_flag` splits every token on its first `=`, which is right for a flag and
/// wrong for a positional — `crate::cmd::data::source` shipped the truncating version and its
/// own comment records what it cost. The symbol is handed to the venue VERBATIM, so a parser
/// that kept the head and dropped the tail would subscribe to something nobody typed.
#[test]
fn a_spec_carrying_an_equals_is_reassembled_rather_than_truncated() {
    let args = parse_of(&["watch", "binance:A=B", "--lane", "trades", "--events", "1"])
        .expect("`A=B` is part of a symbol, not a flag and its value");
    assert_eq!(args.key.expect("a key").symbol, "A=B", "the tail may not be silently dropped");
}

/// `--depth` belongs to the two BOOK lanes, is refused on `trades` by name, and a value above
/// the wire's ceiling is CARRIED rather than clamped here — the server owns that decision.
///
/// ⚠ **The REFUSED range and the ENFORCED one are now the same range, and they were not.**
/// Every unparseable value used to be refused naming `(1..=MD_DEPTH_LEVELS_CEILING)`, a bound
/// [`parse_depth`] does not apply and [`usage`] says is never applied — so `--depth 5000` was
/// accepted in silence (the first case below) while `--depth 70000` was refused for being "not
/// a whole number ... (1..=200)". Both halves of that sentence were false. The boundary cases
/// are what hold the one rule that replaced it: this side refuses only what cannot be SENT.
#[test]
fn depth_is_a_book_flag_and_the_client_clamps_nothing() {
    let args = parse(&watch(&["--lane", "depth", "--depth", "5000", "--events", "1"]), None)
        .expect("a depth above the ceiling is the server's to clamp");
    assert_eq!(args.depth, Some(5000), "the RAW request rides to the wire");

    let trades = parse(&watch(&["--lane", "trades", "--depth", "10", "--events", "1"]), None)
        .expect_err("a print has no levels");
    assert!(trades.contains("--depth does not apply"), "{trades}");
    assert!(trades.contains("IGNORES"), "it must say why silence would be worse: {trades}");

    assert!(parse_depth("0").expect_err("empty ladder").contains("empty ladder"));

    // THE BOUNDARY, both sides of it. The largest sendable number is accepted...
    assert_eq!(parse_depth(&u16::MAX.to_string()), Ok(u16::MAX));
    // ...and one past it is the ONE refusal, which names the field rather than a ceiling.
    let over = parse_depth("70000").expect_err("a u16 field cannot carry 70000");
    assert!(over.contains("u16"), "the refusal names the bound it actually applies: {over}");
    assert!(over.contains(&u16::MAX.to_string()), "...and its size: {over}");
    assert!(
        !over.contains(&format!("1..={MD_DEPTH_LEVELS_CEILING}")),
        "it may not advertise a range nothing enforces: {over}"
    );
    // ...and a value that is not a number at all gets the OTHER answer, so neither is passing
    // because everything is refused alike.
    let typo = parse_depth("x").expect_err("not a number");
    assert!(typo.contains("not a whole number"), "{typo}");
    assert!(!typo.contains("u16"), "a typo is a spelling question, not a transport one: {typo}");

    // ⚠ The USAGE page states the same rule, and states it as the wire's own number — the
    // three spellings (this parser, its message and the page) agree or this reddens.
    let page = usage();
    assert!(page.contains(&u16::MAX.to_string()), "the page names the sendable maximum: {page}");
    assert!(page.contains("CLAMPED AND ACCEPTED"), "{page}");
}

/// The two verbs take DIFFERENT halves of the output axis, and each refusal names the form that
/// verb actually has.
#[test]
fn the_output_axis_splits_by_the_shape_of_the_answer() {
    // `watch` is a sequence: jsonl yes, json no, --json no.
    assert_eq!(
        parse(&watch(&["--lane", "trades", "--events", "1", "--format", "jsonl"]), None)
            .expect("jsonl is watch's machine form")
            .render,
        Some(Render::Jsonl)
    );
    let doc = parse(&watch(&["--lane", "trades", "--events", "1", "--format", "json"]), None)
        .expect_err("a stream is not one document");
    assert!(doc.contains("jsonl"), "the refusal must name the form that exists: {doc}");
    let short = parse(&watch(&["--lane", "trades", "--events", "1", "--json"]), None)
        .expect_err("--json IS --format json");
    assert!(short.contains("jsonl"), "{short}");

    // `status` is one document: json yes, jsonl no.
    assert_eq!(
        parse_of(&["status", "--json"]).expect("the workspace shorthand").render,
        Some(Render::Json)
    );
    let rows = parse_of(&["status", "--format", "jsonl"]).expect_err("status is not a stream");
    assert!(rows.contains("ONE question about ONE server"), "{rows}");

    // The contradiction is refused rather than resolved, on the verb where both are reachable.
    let clash = parse_of(&["status", "--json", "--format", "table"]).expect_err("two answers");
    assert!(clash.contains("pass one"), "{clash}");

    // ...and the formats this group does not serve are refused by NAME, not as spelling.
    for (name, needle) in [("csv", "data hist get"), ("parquet", "no schema")] {
        let why = parse_render(name).expect_err("not served here");
        assert!(why.contains(needle), "`{name}` must say what it is waiting on: {why}");
    }
}

/// **ONE FACT, ONE SPELLING**: which verb emits ROWS is `crate::cmd::data`'s
/// [`super::super::ROW_VERB`], rendered by BOTH `--format` rosters.
///
/// ⚠ **The two copies had drifted and this is what stops them doing it again.** This group's
/// roster said `(P4)` — the surface design's §11 phase table — while the sibling one module over
/// said `(P2)` three times, about the same verb, on the same plane: `data hist ls --format csv`
/// answered "same verb, same phase (P2)" and `data realtime watch … --format csv` answered
/// "`data hist get` (P4)", and nothing compared them.
///
/// ⚠ **The PHASE left the fact when the verb SHIPPED, and this test's control changed with
/// it.** It used to assert the spelling contained `"(P"`, so that a const which lost its phase
/// could not leave the case passing on the verb name alone. That control is now the OPPOSITE:
/// a refusal naming a phase would send an operator to a plan instead of to a command line they
/// can run, which is the same defect the `(P2)` spelling had in the other direction.
#[test]
fn the_row_verbs_name_is_one_spelling_on_both_planes() {
    let here = parse_render("csv").expect_err("a frame is not a row");
    assert!(
        here.contains(super::super::ROW_VERB),
        "this group must RENDER the plane's spelling rather than type one: {here}"
    );
    // ...and the sibling group's refusal for the same value, reached through ITS own parser, so
    // a verb name typed into either message reddens here.
    let there = super::super::parse_format("csv").expect_err("designed, not built");
    assert!(there.contains(super::super::ROW_VERB), "{there}");
    // The anti-vacuity control, both directions: the spelling is a COMMAND LINE — it names the
    // verb, and it names no phase.
    assert!(
        super::super::ROW_VERB.contains("data hist get"),
        "the one spelling names the verb: {}",
        super::super::ROW_VERB
    );
    assert!(
        !super::super::ROW_VERB.contains("(P"),
        "…and no longer a PHASE, because the verb ships: {}",
        super::super::ROW_VERB
    );
}

/// The STREAM follows its destination — which is what makes `| jq` and `--out FILE` agree
/// without being told twice — and the DOCUMENT verb follows the plane.
///
/// ⚠ The `status` rows are the ones that matter: with the destination rule applied to both, a
/// piped `status` answered in JSON, and the table assertions written against it passed on the
/// document's own keys. See [`default_render`] for the incident.
#[test]
fn the_stream_follows_its_destination_and_the_document_verb_follows_the_plane() {
    assert_eq!(default_render(Verb::Watch, false, true), Render::Table);
    assert_eq!(default_render(Verb::Watch, false, false), Render::Jsonl);
    // A file is not a terminal, even when stdout is one.
    assert_eq!(default_render(Verb::Watch, true, true), Render::Jsonl);
    // ...and `status` answers `table` from either side of a pipe, like every sibling verb on
    // this plane. `--json` is one word away and is how a consumer asks.
    assert_eq!(default_render(Verb::Status, false, true), Render::Table);
    assert_eq!(default_render(Verb::Status, false, false), Render::Table);
}

/// Flags that shape a STREAM are refused on `status` by name, with the verb they belong to.
#[test]
fn a_stream_flag_on_status_names_the_verb_it_belongs_to() {
    for flag in [
        vec!["--lane", "trades"],
        vec!["--depth", "10"],
        vec!["--for", "30s"],
        vec!["--events", "5"],
        vec!["--unbounded"],
        vec!["--out", "x.jsonl"],
    ] {
        let mut argv = vec!["status".to_string()];
        argv.extend(flag.iter().map(|s| (*s).to_string()));
        let err = parse(&argv, None).expect_err("a stream flag on a handshake read");
        assert!(err.contains("does not apply to `status`"), "{flag:?}: {err}");
        assert!(err.contains("data realtime watch"), "{flag:?}: {err}");
    }
    // ...and a positional is refused with what the operator probably meant.
    let pos = parse_of(&["status", "binance:BTCUSDT"]).expect_err("status takes no key");
    assert!(pos.contains("data realtime watch binance:BTCUSDT"), "{pos}");
}

/// **A SUB-GROUP is in the ROSTER even though it is not a [`Verb`]**, and an unknown word is a
/// different answer — so neither passes because everything is refused alike.
///
/// ⚠ This test used to assert that `record` was refused as "designed and not built". The verb
/// SHIPPED on 2026-09-22 and is routed by [`run`] above [`parse`], so this parser never sees
/// the word at all; what has to hold instead is that the refusals an operator DOES reach still
/// name it, which is [`SUBGROUPS`]' whole job. The old assertion is replaced rather than
/// deleted, because a roster that silently stopped naming a reachable sub-group is exactly the
/// undiscoverable-verb failure [`VERBS`]' doc records.
#[test]
fn the_roster_names_every_verb_and_every_sub_group() {
    let none = parse(&[], None).expect_err("a verb is required");
    for v in VERBS {
        assert!(none.contains(v.as_str()), "the roster must name {}: {none}", v.as_str());
    }
    for g in SUBGROUPS {
        assert!(none.contains(g), "the roster must name the sub-group {g}: {none}");
    }
    let unknown = parse_of(&["frobnicate"]).expect_err("not a verb");
    assert!(unknown.contains("unknown"), "{unknown}");
    for g in SUBGROUPS {
        assert!(unknown.contains(g), "…and it renders the same roster: {unknown}");
    }
    // Anti-vacuity: the two rosters must not be the same set, or the `SUBGROUPS` half above
    // could be passing on a `VERBS` row that happens to share the word.
    assert!(
        SUBGROUPS.iter().all(|g| VERBS.iter().all(|v| v.as_str() != *g)),
        "a sub-group that is also a verb would make this test measure nothing"
    );
}

/// The usage page is rendered from the declarations, so no placeholder survives and every verb
/// is documented.
///
/// ⚠ The verb check asserts on the LABEL COLUMN rather than on the whole page: every verb name
/// also occurs in the surrounding prose, so a `contains` over the page would pass with a verb's
/// block deleted — the exact mistake `crate::cmd::data::catalog`'s own usage test records.
#[test]
fn the_usage_documents_every_verb_and_leaves_no_placeholder() {
    let page = usage();
    assert!(!page.contains('{'), "an unexpanded token survived: {page}");
    for v in VERBS {
        let labelled = page.lines().any(|l| l.starts_with(&format!("  {}", v.as_str())));
        assert!(labelled, "`{}` has no block of its own on the page", v.as_str());
    }
    // ...and so does every SUB-GROUP, which is reachable from this page or from nowhere.
    for g in SUBGROUPS {
        let labelled = page.lines().any(|l| l.starts_with(&format!("  {g}")));
        assert!(labelled, "the sub-group `{g}` has no block of its own on the page");
    }
    for lane in LANES {
        assert!(page.contains(lane.feed_stream_label()), "the lanes are named on the page");
    }
    assert!(
        page.contains(&MD_DEPTH_LEVELS_CEILING.to_string()),
        "the ceiling is the wire's number and is expanded, never typed: {page}"
    );
}

// ── the disclosures ──────────────────────────────────────────────────────────────────────

/// **A CLAMP IS AN ACCEPTANCE**, and the note carries the number the SERVER served rather than
/// the one the operator typed. The pairing is the point: an unclamped subscription must NOT
/// produce the warning, or the warning stops being read.
#[test]
fn a_clamped_depth_is_disclosed_with_the_number_that_was_served() {
    let asked = spec(MdLane::Depth, Some(200));
    let served = spec(MdLane::Depth, Some(50));
    let note = depth_note(&asked, &served).expect("the book lanes have levels");
    assert!(note.contains("CLAMPED"), "{note}");
    assert!(
        note.contains("200") && note.contains("50"),
        "both numbers, so it can be acted on: {note}"
    );
    assert!(
        note.contains("ACCEPTANCE"),
        "a clamp is not a refusal and must not read as one: {note}"
    );

    // The control: served AS ASKED says so and warns about nothing.
    let same = depth_note(&spec(MdLane::Depth, Some(20)), &spec(MdLane::Depth, Some(20)))
        .expect("still a book lane");
    assert!(!same.contains("CLAMPED"), "{same}");
    assert!(same.contains("as asked"), "{same}");

    // ...and a request ABOVE the wire's own ceiling reports what the server served, never the
    // client's own clamp of the request: `resolved_depth()` on the REQUEST would say 200.
    let over = depth_note(&spec(MdLane::Depth, Some(5_000)), &spec(MdLane::Depth, Some(50)))
        .expect("a book lane");
    assert!(over.contains("asked for 5000"), "the number they TYPED: {over}");
    // ⚠ The SERVED SLOT, not a bare `contains("50")` — which is what this line was and which
    // could not fail: "50" is a substring of "5000", already asserted present one line up, so a
    // note that rendered the ASKED-FOR number in the served slot would have passed it.
    assert!(over.contains("serves 50."), "...and the number they GOT, where it belongs: {over}");

    // A lane with no levels gets no depth line at all.
    assert_eq!(depth_note(&spec(MdLane::Trades, None), &spec(MdLane::Trades, None)), None);

    // The DEFAULT case names the wire's default and how to ask for more.
    let default =
        depth_note(&spec(MdLane::Book, None), &spec(MdLane::Book, None)).expect("a book lane");
    assert!(default.contains(&MD_DEPTH_LEVELS_DEFAULT.to_string()), "{default}");
}

/// Every refusal is a sentence that names the key, the reason and whether retrying could ever
/// help — and the far side's own words are forwarded rather than re-written.
#[test]
fn every_refusal_says_what_was_refused_and_whether_it_can_ever_work() {
    let asked = spec(MdLane::Book, None);
    let permanent = refusal_sentence(&asked, &MdRefusal::UnknownVenue);
    assert!(permanent.contains("binance:BTCUSDT"), "{permanent}");
    assert!(permanent.contains("book"), "the LANE is part of what was refused: {permanent}");
    assert!(permanent.contains("Retrying cannot"), "{permanent}");

    // A CAP is the other class, and it must not read as a permanent no.
    let cap = refusal_sentence(&asked, &MdRefusal::KeyCapTotal { held: 64, cap: 64 });
    assert!(cap.contains("can free up"), "{cap}");
    assert!(!cap.contains("Retrying cannot"), "{cap}");

    // The far side's own text rides verbatim — this side re-words neither validator.
    let wire = "the venue's declared VenueCaps.live_data serves no such lane";
    let forwarded = refusal_sentence(&asked, &MdRefusal::LaneUnsupported(wire.to_string()));
    assert!(forwarded.contains(wire), "{forwarded}");

    // ...and the served set is named on the one refusal that carries it, because that is the
    // answer to "then what CAN I watch".
    let unserved = refusal_sentence(&asked, &MdRefusal::VenueNotServed("okx, polymarket".into()));
    assert!(unserved.contains("okx, polymarket"), "{unserved}");
    assert!(unserved.contains("data realtime status"), "{unserved}");
}

// ── rendering ────────────────────────────────────────────────────────────────────────────

/// EVERY frame renders under BOTH forms, every jsonl row is one JSON object carrying a `type`,
/// and no two frame classes share a type token — which is what a consumer filters on.
#[test]
fn every_frame_renders_under_both_forms_with_a_type_of_its_own() {
    let frames = [
        MdFrame::Depth(snapshot()),
        MdFrame::Book(snapshot()),
        MdFrame::Trades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            ticks: vec![TradeTick {
                ts: 1_700_000_000_000,
                local_ts: 1_700_000_000_005,
                price: 100.25,
                size: 0.5,
                is_buyer_maker: true,
                symbol: String::new(),
            }],
            seq: 9,
        },
        MdFrame::Status {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            lane: MdLane::Depth,
            status: WireStreamStatus::Live { gap_started_ts_ms: None },
        },
        MdFrame::TapeGap {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            dropped: 12,
            from_seq: 4,
            to_seq: 9,
        },
        MdFrame::Heartbeat,
        MdFrame::Bye(MdBye::ServerStopping),
    ];
    let mut types = std::collections::BTreeSet::new();
    for frame in &frames {
        // The compile-time completeness guard, no `_` arm: a new `MdFrame` variant must be given
        // a rendering rather than inheriting one.
        match frame {
            MdFrame::Depth(_)
            | MdFrame::Book(_)
            | MdFrame::Trades { .. }
            | MdFrame::Status { .. }
            | MdFrame::TapeGap { .. }
            | MdFrame::Heartbeat
            | MdFrame::Bye(_) => {}
        }
        let doc = row(frame);
        let kind = doc["type"].as_str().unwrap_or_default().to_string();
        assert!(!kind.is_empty(), "every row carries a type: {doc}");
        assert!(types.insert(kind.clone()), "two frame classes share `{kind}`");
        assert!(!table_line(frame).is_empty(), "every frame has a human line too: {frame:?}");
        assert!(!table_line(frame).contains('\n'), "a table frame is ONE line: {frame:?}");
    }
    // The two BOOK lanes are DISTINCT on the wire even though the payload is identical — that
    // separation is the whole disclosure, so it must survive rendering.
    assert_eq!(types.len(), frames.len(), "each frame class renders as its own type: {types:?}");
}

/// ⚠ **A trade row is stamped from the ENVELOPE.** Every tick on this wire carries an EMPTY
/// `symbol` by design, so a row built by serializing `TradeTick` would publish `"symbol": ""` on
/// every print — and anything grouping by it folds every venue's tape into one bucket.
#[test]
fn a_trade_row_is_stamped_from_the_envelope_and_never_from_the_tick() {
    let frame = MdFrame::Trades {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        ticks: vec![TradeTick {
            ts: 1_700_000_000_000,
            local_ts: 0,
            price: 100.25,
            size: 0.5,
            // The buyer was RESTING, so the aggressor was the seller.
            is_buyer_maker: true,
            // ...as the hub leaves it: blanked on the way in, to keep the tape's memory budget.
            symbol: String::new(),
        }],
        seq: 9,
    };
    let doc = row(&frame);
    assert_eq!(doc["symbol"], "BTCUSDT", "the envelope is authoritative: {doc}");
    assert_eq!(doc["prints"][0]["price"], 100.25);
    assert_eq!(doc["prints"][0]["is_buyer_maker"], true, "the model's own flag, raw: {doc}");
    assert!(
        doc["prints"][0].get("symbol").is_none(),
        "an empty per-tick symbol may not ride as though it were an answer: {doc}"
    );
    // The human line reads the aggressor rather than the flag, and derives it at one site.
    assert!(table_line(&frame).contains("sell"), "a buyer-maker print was SELL-aggressed");
    assert_eq!(aggressor(true), "sell");
    assert_eq!(aggressor(false), "buy");
}

/// A book row carries EVERY level in the model's own two-element shape, and best-first survives
/// the rendering — bids descend, asks ascend, nothing re-sorts.
#[test]
fn a_book_row_keeps_every_level_and_its_best_first_order() {
    let doc = row(&MdFrame::Depth(snapshot()));
    assert_eq!(doc["type"], "depth");
    assert_eq!(doc["bids"].as_array().expect("bids").len(), 2);
    // `BookLevel` serializes as `[price, qty]` through its own `#[serde(into)]` — the shape the
    // journal has always written, not one invented here.
    assert_eq!(doc["bids"][0][0], 100.2, "best bid FIRST: {doc}");
    assert_eq!(doc["bids"][1][0], 100.1, "...and the next one is lower: {doc}");
    assert_eq!(doc["asks"][0][0], 100.3);
    assert_eq!(doc["seq"], 7, "the WIRE sequence, which is what a contiguity check reads");
    assert_eq!(doc["venue_seq"], 42, "...beside the venue's own, which is diagnostic only");
    // The human line summarises instead, and says how deep the frame actually was.
    let line = table_line(&MdFrame::Depth(snapshot()));
    assert!(line.contains("2x1 levels"), "the depth of the FRAME is part of the line: {line}");
}

/// The three stream-status states render as distinct machine tokens and carry the numbers each
/// verdict rests on — flattened, because the wire's mirror enum is externally tagged and hostile
/// to `jq`.
#[test]
fn every_stream_status_flattens_to_its_own_token_and_keeps_its_numbers() {
    let of = |status| {
        row(&MdFrame::Status {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            lane: MdLane::Depth,
            status,
        })
    };
    let gap = of(WireStreamStatus::GapStart { at_ts_ms: 7 });
    assert_eq!(gap["state"], "gap_start");
    assert_eq!(gap["episode_ts"], 7);
    let live = of(WireStreamStatus::Live { gap_started_ts_ms: Some(7) });
    assert_eq!(live["state"], "live");
    assert_eq!(live["episode_ts"], 7, "a recovery echoes the episode it closes");
    assert!(of(WireStreamStatus::Live { gap_started_ts_ms: None })["episode_ts"].is_null());
    let stale = of(WireStreamStatus::Stale { newest_data_ts_ms: 5, now_ms: 9 });
    assert_eq!(stale["state"], "stale");
    assert_eq!(stale["newest_data_ts"], 5);
    assert_eq!(stale["judged_at_ts"], 9);
    // ...and the human sentence keeps the two apart, since they are opposite diagnoses.
    assert!(
        status_sentence(&WireStreamStatus::Stale { newest_data_ts_ms: 5, now_ms: 9 })
            .contains("STALE")
    );
    assert!(
        !status_sentence(&WireStreamStatus::Live { gap_started_ts_ms: None }).contains("STALE")
    );
}

/// A tape gap is LOUD in both forms, and `dropped` is authoritative for how much was lost.
#[test]
fn a_tape_gap_is_loud_and_carries_the_count_that_is_authoritative() {
    let frame = MdFrame::TapeGap {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        dropped: 12,
        from_seq: 4,
        to_seq: 9,
    };
    let doc = row(&frame);
    assert_eq!(doc["type"], "tape_gap");
    assert_eq!(doc["dropped"], 12);
    let line = table_line(&frame);
    assert!(line.contains("TAPE GAP"), "{line}");
    assert!(line.contains("LOST"), "a dropped print is a LOSS and the line says so: {line}");
}

/// Every goodbye has its own token and its own sentence. ⚠ `too_slow` and `session_idle` are
/// OPPOSITE diagnoses — too much to read versus nothing asked for — and the wire's own doc
/// records what sending the wrong one cost, so this holds them apart.
#[test]
fn every_goodbye_has_its_own_token_and_sentence() {
    let all = [
        MdBye::TooSlow { lapses: 3 },
        MdBye::ServerStopping,
        MdBye::SessionIdle,
        MdBye::ControlLaneOverflow,
    ];
    let mut tokens = std::collections::BTreeSet::new();
    for why in all {
        match why {
            MdBye::TooSlow { .. }
            | MdBye::ServerStopping
            | MdBye::SessionIdle
            | MdBye::ControlLaneOverflow => {}
        }
        assert!(tokens.insert(bye_token(why)), "two goodbyes share `{}`", bye_token(why));
        assert!(!bye_sentence(why).is_empty());
    }
    assert_eq!(tokens.len(), 4);
    assert!(bye_sentence(MdBye::TooSlow { lapses: 3 }).contains("keep up"));
    assert!(bye_sentence(MdBye::SessionIdle).contains("no subscriptions"));
    // The lapse count rides the row, because it is the only number a slow reader can act on.
    assert_eq!(row(&MdFrame::Bye(MdBye::TooSlow { lapses: 3 }))["lapses"], 3);
    assert!(row(&MdFrame::Bye(MdBye::ServerStopping)).get("lapses").is_none());
}

// ── the bound, the tally and the exit rule ───────────────────────────────────────────────

/// Only the three DATA variants are events. A heartbeat is what the wire says ABOUT the stream,
/// and counting it would let a dead-quiet key satisfy `--events 500` by saying nothing.
#[test]
fn only_data_frames_count_towards_the_events_bound() {
    let mut tally = Tally::default();
    for frame in [
        MdFrame::Depth(snapshot()),
        MdFrame::Book(snapshot()),
        MdFrame::Heartbeat,
        MdFrame::Heartbeat,
        MdFrame::Status {
            venue: "b".into(),
            symbol: "S".into(),
            lane: MdLane::Depth,
            status: WireStreamStatus::Live { gap_started_ts_ms: None },
        },
        MdFrame::TapeGap {
            venue: "b".into(),
            symbol: "S".into(),
            dropped: 12,
            from_seq: 1,
            to_seq: 4,
        },
        MdFrame::Bye(MdBye::ServerStopping),
    ] {
        count_frame(&mut tally, &frame);
    }
    assert_eq!(
        tally,
        Tally { events: 2, statuses: 1, gaps: 1, heartbeats: 2, dropped: 12 },
        "each class counts in its own column"
    );
}

/// The EXIT rule: under a bound, only that bound is a success; under `--unbounded`, every
/// clean ENDING is the end — but a FAULT is a failure under both. A reader that closed the pipe
/// is always a success — `| head -3` is a legitimate way to use a stream.
///
/// ⚠ **This test PINNED the defect it now guards against.** It asserted
/// `early.was_asked_for(Bound::Unbounded)` for all five non-bound ends, `Desync` and `Fault`
/// included — so a transport failure and a protocol desync exited 0 under `--unbounded`, in the
/// one channel a pipeline reads, and a wrapper piping `--out tape.jsonl` could not tell a
/// finished capture from a broken one. The split is [`End::is_fault`], and
/// `a_faulting_stream_is_never_asked_for_however_it_is_bounded` drives a real socket into both
/// halves of it rather than only asserting the helper.
#[test]
fn a_bound_that_was_not_reached_is_a_failure_and_unbounded_has_none_to_miss() {
    let bounded = Bound::First { events: Some(5), duration: None };
    assert!(End::Events(5).was_asked_for(bounded));
    assert!(End::Elapsed.was_asked_for(bounded));
    assert!(End::ReaderGone.was_asked_for(bounded), "`| head -3` is not a failure");
    // The ENDINGS: not the bound that was named, but nothing broke — so `--unbounded`, which
    // named no bound, is satisfied by them.
    for early in [End::Bye(MdBye::ServerStopping), End::Closed] {
        assert!(!early.was_asked_for(bounded), "{early:?} did not deliver the bound");
        assert!(early.was_asked_for(Bound::Unbounded), "{early:?} has no bound to miss");
        assert!(!early.is_fault(), "{early:?} is an ending rather than a break");
        assert!(!early.sentence().is_empty(), "{early:?} must say what happened");
    }
    // The FAULTS: a failure under EVERY bound, `--unbounded` included.
    for broken in [End::Silent, End::Desync("x".into()), End::Fault("x".into())] {
        assert!(broken.is_fault(), "{broken:?} is something breaking, not a stream ending");
        assert!(!broken.was_asked_for(bounded), "{broken:?} did not deliver the bound");
        assert!(
            !broken.was_asked_for(Bound::Unbounded),
            "{broken:?} exits 0 under --unbounded — a wrapper cannot tell it from a clean stop"
        );
        assert!(!broken.sentence().is_empty(), "{broken:?} must say what happened");
    }
    // ...and the two ends that ARE the bound name it, so a summary reads as an answer.
    assert!(End::Events(5).sentence().contains("--events"));
    assert!(End::Elapsed.sentence().contains("--for"));
    // A dead link is not a quiet market, and the sentence is what stops it being read as one.
    assert!(End::Silent.sentence().contains("dead link"));
}

/// One scripted server socket: a loopback listener that runs `script` against the accepted
/// connection and then hangs up.
///
/// ⚠ A REAL `TcpStream` rather than an in-memory double, and that is what earns the test below:
/// the classification under test is made from `io::ErrorKind`s, and the only thing that produces
/// the real ones is a socket. A double would be asserting the classifier against itself.
fn scripted_stream(script: fn(&mut TcpStream)) -> TcpStream {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("the assigned port");
    std::thread::spawn(move || {
        let (mut server, _) = listener.accept().expect("the client below dials at once");
        script(&mut server);
    });
    TcpStream::connect(addr).expect("dial the scripted server")
}

/// Drain a scripted socket the way `watch` does — through the real [`stream_frames`], into a
/// throwaway tape rather than this test binary's stdout.
fn drained(dir: &std::path::Path, name: &str, script: fn(&mut TcpStream)) -> (End, Tally) {
    let path = dir.join(name).to_string_lossy().into_owned();
    let mut sink = Sink::open(Some(path.as_str())).expect("a writable tape");
    let out = stream_frames(scripted_stream(script), Bound::Unbounded, Render::Jsonl, &mut sink);
    sink.finish().expect("the tape flushes");
    out
}

/// **A STREAM THAT BREAKS IS NEVER THE END THAT WAS ASKED FOR**, `--unbounded` included — and
/// this drives a real socket into each way of breaking rather than asserting the classifier
/// against itself.
///
/// ⚠ The two faults below are the ones that used to exit 0 under `--unbounded`: the socket
/// carries a valid response that is not an `Md` frame (the wire's own §0 invariant broken after
/// `MdSubscribed` — a protocol desync) or a body that does not decode (a transport fault). Both
/// were folded in with a clean stop, in the ONE channel a pipeline reads, so a wrapper running
/// `… --unbounded --out tape.jsonl` under `set -e` took a half-written tape for a complete one.
#[test]
fn a_faulting_stream_is_never_asked_for_however_it_is_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bounded = Bound::First { events: Some(5), duration: None };

    // A DESYNC. `Pong` stands for every non-`Md` variant: what makes it a desync is the KIND of
    // frame, not which one.
    let (end, tally) = drained(dir.path(), "desync.jsonl", |s| {
        write_frame(s, &Response::Pong).expect("the scripted frame is written");
    });
    assert!(matches!(end, End::Desync(_)), "a non-Md response is a desync: {end:?}");
    assert!(end.is_fault(), "{end:?} is something breaking, not a stream ending");
    assert!(
        !end.was_asked_for(Bound::Unbounded),
        "a desync exits 0 under --unbounded — a wrapper cannot tell it from a clean stop"
    );
    assert!(!end.was_asked_for(bounded), "{end:?}");
    assert_eq!(tally, Tally::default(), "nothing was streamed: {tally:?}");

    // A FAULT: a well-framed body that does not decode. `read_frame` fuses framing and decoding
    // into one `InvalidData`, which is how a transport failure reaches this verb.
    let (end, tally) = drained(dir.path(), "fault.jsonl", |s| {
        s.write_all(&3u32.to_be_bytes()).expect("a valid length prefix");
        s.write_all(b"{{{").expect("...and a body that is not JSON");
        s.flush().expect("the scripted bytes are on the wire");
    });
    assert!(matches!(end, End::Fault(_)), "an undecodable body is a fault: {end:?}");
    assert!(end.is_fault(), "{end:?}");
    assert!(
        !end.was_asked_for(Bound::Unbounded),
        "a transport fault exits 0 under --unbounded — the finished-vs-broken signal is gone"
    );
    assert_eq!(tally, Tally::default(), "nothing was streamed: {tally:?}");

    // THE CONTROL, and it is what stops the two above passing because every scripted socket
    // fails: a server that says GOODBYE and hangs up ENDED the stream, so `--unbounded` — which
    // named no bound to miss — got what it asked for.
    let (end, tally) = drained(dir.path(), "bye.jsonl", |s| {
        write_frame(s, &Response::Md(Box::new(MdFrame::Bye(MdBye::ServerStopping))))
            .expect("the goodbye is written");
    });
    assert_eq!(end, End::Bye(MdBye::ServerStopping));
    assert!(!end.is_fault(), "a goodbye is an ending, not a break");
    assert!(end.was_asked_for(Bound::Unbounded), "there was no bound to miss");
    assert!(!end.was_asked_for(bounded), "...but five frames were asked for and none arrived");
    assert_eq!(tally, Tally::default(), "a goodbye is not a DATA frame");
}

/// The summary is the honest half of a stream that carried nothing: zero data frames on a quiet
/// key is the MARKET, and the heartbeat count is what says the link was alive.
#[test]
fn the_summary_counts_by_class_and_says_when_nothing_arrived() {
    let asked = spec(MdLane::Trades, None);
    let quiet = summary_lines(&asked, &End::Elapsed, &Tally { heartbeats: 2, ..Tally::default() });
    let text = quiet.join("\n");
    assert!(text.contains("binance:BTCUSDT"), "it names the key: {text}");
    assert!(text.contains("no DATA frame arrived"), "{text}");
    assert!(text.contains("2 heartbeat"), "...and what says the link was alive: {text}");

    // A stream that DID carry data says nothing of the kind — the anti-vacuity control for the
    // sentence above.
    let busy =
        summary_lines(&asked, &End::Events(3), &Tally { events: 3, ..Tally::default() }).join("\n");
    assert!(!busy.contains("no DATA frame arrived"), "{busy}");

    // A LOSS is reported once, loudly, with what it costs anything folded from the stream.
    let lossy = summary_lines(
        &asked,
        &End::Events(3),
        &Tally { events: 3, gaps: 1, dropped: 12, ..Tally::default() },
    )
    .join("\n");
    assert!(lossy.contains("12 PRINTS WERE LOST"), "{lossy}");
    assert!(lossy.contains("cannot be repaired"), "{lossy}");
}

// ── `status` ─────────────────────────────────────────────────────────────────────────────

/// **The advertisement is never rendered as a probe**, under either form, in any of the three
/// server shapes — and each shape gets its OWN note, because "no plane" and "a plane serving no
/// venue" are different things to fix.
#[test]
fn status_says_it_is_an_advertisement_in_every_shape_it_can_report() {
    let serving = ServerFeeds::of(&[
        FEATURE_MARKET_DATA.to_string(),
        "md_venue=binance".to_string(),
        "md_venue=polymarket".to_string(),
    ]);
    assert!(serving.plane);
    assert_eq!(serving.venues, vec!["binance".to_string(), "polymarket".to_string()]);

    let mounted_but_empty = ServerFeeds::of(&[FEATURE_MARKET_DATA.to_string()]);
    let no_plane = ServerFeeds::of(&["backfill".to_string()]);
    assert!(!no_plane.plane);
    assert!(no_plane.venues.is_empty());

    for view in [&serving, &mounted_but_empty, &no_plane] {
        let table = status_lines("127.0.0.1:7878", view).join("\n");
        let doc: serde_json::Value =
            serde_json::from_str(&status_json("127.0.0.1:7878", view)).expect("one document");
        assert!(table.contains("ADVERTISES"), "the table must say so: {table}");
        assert!(table.contains("not a liveness check"), "{table}");
        assert_eq!(doc["liveness_probed"], false, "and the document must say so too: {doc}");
        assert_eq!(doc["market_data_advertised"], view.plane);
        assert_eq!(doc["count"], view.venues.len());
        // The notes are the SAME notes, so a table reader and a document reader cannot be told
        // different things.
        let notes: Vec<String> = doc["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .map(|n| n.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(notes, status_notes(view));
    }

    // Each shape's own note, and the anti-vacuity control: they are DIFFERENT sentences.
    assert!(status_notes(&no_plane)[0].contains(FEATURE_MARKET_DATA));
    assert!(status_notes(&mounted_but_empty)[0].contains("NO venue"));
    assert!(status_notes(&serving)[0].contains("VenueNotServed"));
    assert_ne!(status_notes(&no_plane)[0], status_notes(&mounted_but_empty)[0]);

    // A served venue is a ROW, and an empty set says so rather than printing a headed table
    // with nothing under it.
    assert!(status_lines("a", &serving).iter().any(|l| l.starts_with("binance")));
    assert!(status_lines("a", &no_plane).iter().any(|l| l.contains("no venue advertises")));
}

/// The advertisement ORDER is the server's own statement about itself and is preserved, and a
/// blank `md_venue=` entry advertises nothing — both are `advertised_md_venues`' contract, read
/// through it rather than re-implemented.
#[test]
fn the_advertised_order_is_the_servers_own_and_a_blank_entry_is_not_a_venue() {
    let view = ServerFeeds::of(&[
        FEATURE_MARKET_DATA.to_string(),
        "md_venue=polymarket".to_string(),
        "md_venue=".to_string(),
        "md_venue=binance".to_string(),
    ]);
    assert_eq!(view.venues, vec!["polymarket".to_string(), "binance".to_string()]);
}
