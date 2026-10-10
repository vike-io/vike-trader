use super::*;

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn spec() -> Spec {
    Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: Some("1h".into()) }
}

/// A bar plan over `[10, 20]` in 5ms steps, with the step DEFAULTED — the shape the notes and
/// the document are exercised against.
fn plan(wire: Wire) -> Plan {
    Plan { kind: Kind::Bar, spec: spec(), wire, bounds: (10, 20), step_ms: 5, step_defaulted: true }
}

/// The three kinds are the wire's three row verbs, and the roster is the authority for every
/// message that lists them.
#[test]
fn the_kind_roster_is_the_three_the_wire_can_read() {
    assert_eq!(Kind::ALL.map(Kind::as_str), ["bar", "quote", "trade"]);
    assert_eq!(served_kinds(), "bar | quote | trade");
    // ANTI-VACUITY: the roster is not empty and the rendering is not the empty string, so a
    // future refactor that emptied `ALL` would fail here rather than render blank messages.
    assert_eq!(Kind::ALL.len(), 3);
    assert!(!served_kinds().is_empty());
}

/// §9.3.2 outranks this verb's own shape, and a market kind THIS ROUTE cannot read is refused
/// with a DIFFERENT sentence — the two must not blur.
///
/// ⚠ Renamed from `..._a_book_meets_the_wire_one`: the sentence a book meets is no longer
/// about the WIRE. `docs/decisions/0084-only-the-datahub-touches-the-store.md` put book,
/// depth, cohort, perp-metric, equity and exec-fill reads ON the wire, so the bound this
/// refusal reports moved to this route — and the assertions below moved with it, because a
/// pinned sentence that has become false is worse than an unpinned one.
#[test]
fn an_account_kind_meets_the_plane_sentence_and_a_book_meets_the_route_one() {
    let account = parse_kind(Some("exec_fill")).unwrap_err();
    assert!(account.contains("ACCOUNT data"), "{account}");
    assert!(account.contains("vike-cli account"), "{account}");
    // ...and it does NOT get the wire sentence, which would be a fact about this verb standing
    // in front of a fact about the plane.
    assert!(!account.contains("THIS ROUTE"), "{account}");

    let book = parse_kind(Some("book")).unwrap_err();
    assert!(book.contains("THIS ROUTE"), "{book}");
    // ⚠ ...and it must NOT tell the operator the wire lacks the verb, which is the false
    // statement this change removed and the one that would send them around the server.
    assert!(
        !book.contains("reachable by no read verb"),
        "the refusal must not deny a verb the datahub serves: {book}"
    );
    assert!(book.contains("data hist ls --kind book"), "{book}");
    assert!(!book.contains("ACCOUNT data"), "{book}");

    // The market funding RATE is not an account kind and is not this verb's kind either — it
    // is `bar`, so it is refused as an unserved kind rather than as account data.
    // ⚠ ...and it lands on the OTHER side of the split below: `funding` is not a store kind at
    // all, so no verb reads it anywhere and the refusal must not promise that one exists.
    let funding = parse_kind(Some("funding")).unwrap_err();
    assert!(!funding.contains("ACCOUNT data"), "{funding}");
    assert!(funding.contains("no read verb for it at all"), "{funding}");
}

/// ⚠ **The refusal must tell a MISSING ARM from a MISSING VERB**, because the two name
/// different work and sending a reader at the wrong one is how somebody goes around the server
/// — the behaviour `docs/decisions/0084-only-the-datahub-touches-the-store.md` exists to stop.
///
/// The completeness half is the part that earns this test: [`Kind::ALL`] plus
/// [`WIRE_READS_THIS_ROUTE_LACKS`] plus `chain`/`properties` must be exactly the MARKET half of
/// `vike_data::store::store_kind::STORE_KINDS`. A new store kind therefore reddens this test until
/// somebody classifies it, which is the only thing standing in for the shared map that does not
/// exist.
#[test]
fn the_refusal_distinguishes_a_missing_arm_from_a_missing_verb() {
    for kind in WIRE_READS_THIS_ROUTE_LACKS {
        let msg = parse_kind(Some(kind)).unwrap_err();
        assert!(msg.contains("THIS ROUTE"), "{kind}: {msg}");
        assert!(
            msg.contains("DOES answer this kind"),
            "a kind the wire serves must not be told the wire lacks a verb — {kind}: {msg}"
        );
    }
    for kind in ["chain", "properties"] {
        let msg = parse_kind(Some(kind)).unwrap_err();
        assert!(
            msg.contains("no read verb for it at all"),
            "a kind the wire does NOT serve must say so — {kind}: {msg}"
        );
        assert!(!msg.contains("THIS ROUTE"), "{kind}: {msg}");
    }

    // COMPLETENESS: every market store kind is classified by exactly one of the three groups.
    let mut classified: Vec<&str> = Kind::ALL
        .iter()
        .map(|k| k.as_str())
        .chain(WIRE_READS_THIS_ROUTE_LACKS.iter().copied())
        .chain(["chain", "properties"])
        .collect();
    classified.sort_unstable();
    let mut market: Vec<&str> = vike_data::store::store_kind::STORE_KINDS
        .iter()
        .map(|k| k.kind)
        .filter(|k| !vike_model::is_account_kind(k))
        .collect();
    market.sort_unstable();
    market.dedup();
    assert_eq!(
        classified, market,
        "every MARKET store kind must be in exactly one group: this route reads it, the wire \
             reads it and this route does not, or nothing reads it"
    );
}

#[test]
fn a_blank_kind_is_its_own_refusal_and_the_default_is_bar() {
    assert_eq!(parse_kind(None), Ok(Kind::Bar));
    let err = parse_kind(Some("")).unwrap_err();
    assert!(err.contains("EMPTY"), "{err}");
    // ANTI-VACUITY: a NON-blank unknown value does NOT get the empty sentence.
    assert!(!parse_kind(Some("zzz")).unwrap_err().contains("EMPTY"));
}

/// The arity is a function of the kind, and a dropped interval is refused rather than ignored.
#[test]
fn the_spec_arity_follows_the_kind_and_a_surplus_interval_is_named() {
    assert_eq!(
        parse_spec("binance:BTCUSDT:1h", Kind::Bar),
        Ok(Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: Some("1h".into()) })
    );
    assert_eq!(
        parse_spec("binance:BTCUSDT", Kind::Trade),
        Ok(Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: None })
    );

    let surplus = parse_spec("binance:BTCUSDT:1h", Kind::Trade).unwrap_err();
    assert!(surplus.contains("no INTERVAL"), "{surplus}");
    assert!(surplus.contains("'1h'"), "{surplus}");
    assert!(surplus.contains("dropped in silence"), "{surplus}");

    let missing = parse_spec("binance:BTCUSDT", Kind::Bar).unwrap_err();
    assert!(missing.contains("needs an INTERVAL"), "{missing}");

    let blank = parse_spec("binance::1h", Kind::Bar).unwrap_err();
    assert!(blank.contains("non-empty"), "{blank}");
}

/// `--format` is REQUIRED on this route, and both of the shapes an operator is likeliest to
/// carry over are refused by NAME rather than as unknown words.
#[test]
fn the_file_format_is_required_and_the_old_axis_is_corrected_by_name() {
    let absent = parse_wire(None).unwrap_err();
    assert!(absent.contains("needs --format"), "{absent}");
    assert!(absent.contains("jsonl | csv"), "{absent}");

    assert_eq!(parse_wire(Some("jsonl")), Ok(Wire::Jsonl));
    assert_eq!(parse_wire(Some("csv")), Ok(Wire::Csv));

    for old in ["table", "json"] {
        let err = parse_wire(Some(old)).unwrap_err();
        assert!(err.contains("TERMINAL rendering"), "{old}: {err}");
        assert!(err.contains("`--json`"), "{old}: {err}");
    }

    let parquet = parse_wire(Some("parquet")).unwrap_err();
    assert!(parquet.contains("backend-agnostic"), "{parquet}");
    assert!(parquet.contains("drop --addr"), "{parquet}");
    // ANTI-VACUITY: the parquet refusal is not the generic unknown-word one.
    assert!(!parse_wire(Some("zzz")).unwrap_err().contains("backend-agnostic"));
}

/// The walk covers `[start, end]` exactly once, with no shared instant between windows.
#[test]
fn the_walk_tiles_the_range_with_no_overlap_and_no_hole() {
    let w = windows(0, 9, 4);
    assert_eq!(w, vec![(0, 3), (4, 7), (8, 9)]);

    // The tiling property, asserted rather than read off the literal above: every window
    // begins one ms after the previous one ends, the first begins at `start`, the last ends at
    // `end`.
    for (lo, hi) in windows(1_000, 10_000, 1_500) {
        assert!(lo <= hi, "{lo} > {hi}");
    }
    let walk = windows(1_000, 10_000, 1_500);
    assert_eq!(walk.first().map(|w| w.0), Some(1_000));
    assert_eq!(walk.last().map(|w| w.1), Some(10_000));
    for pair in walk.windows(2) {
        assert_eq!(pair[1].0, pair[0].1 + 1, "{pair:?}");
    }

    // A step at least as wide as the range is ONE window, not two.
    assert_eq!(windows(5, 10, 1_000), vec![(5, 10)]);
    // A single instant is one window of width one.
    assert_eq!(windows(7, 7, 4), vec![(7, 7)]);
    // ANTI-VACUITY for the two guards: an inverted range and a non-positive step are empty,
    // and the healthy case above is NOT.
    assert!(windows(10, 5, 4).is_empty());
    assert!(windows(0, 9, 0).is_empty());
    assert!(!windows(0, 9, 4).is_empty());
}

/// The step grammar is the workspace's, narrowed — and the two span shapes that are not a fixed
/// number of milliseconds are refused with what is wrong with each.
#[test]
fn the_window_step_reuses_the_workspace_span_grammar() {
    assert_eq!(parse_window_step("4h", Kind::Bar), Ok(4 * 3_600_000));
    assert_eq!(parse_window_step("1d", Kind::Trade), Ok(MS_PER_DAY));

    let bars = parse_window_step("500bars", Kind::Trade).unwrap_err();
    assert!(bars.contains("BAR COUNT"), "{bars}");
    let months = parse_window_step("3mo", Kind::Bar).unwrap_err();
    assert!(months.contains("CALENDAR"), "{months}");
    let junk = parse_window_step("soon", Kind::Bar).unwrap_err();
    assert!(junk.contains("--window"), "{junk}");
}

/// The per-kind defaults differ, and the tick lanes are the narrower ones.
#[test]
fn the_default_window_is_wider_for_bars_than_for_ticks() {
    assert_eq!(Kind::Bar.default_window_ms(), 30 * MS_PER_DAY);
    assert_eq!(Kind::Quote.default_window_ms(), MS_PER_DAY);
    assert_eq!(Kind::Trade.default_window_ms(), MS_PER_DAY);
    assert!(Kind::Bar.default_window_ms() > Kind::Trade.default_window_ms());
}

/// ⚠ THE CROSS-VERB PIN: `export --format jsonl` and `get --format jsonl` must describe one bar
/// identically, or a file concatenated from both is two schemas wearing one name.
#[test]
fn the_bar_row_is_the_same_shape_get_emits() {
    let s = spec();
    let b = bar(1_700_000_000_000);
    let mine: Value = serde_json::from_str(&jsonl_line(&bar_cells(&s, &b))).expect("json");
    let theirs = crate::cmd::data::get::json_row(
        &crate::cmd::data::get::Spec {
            venue: s.venue.clone(),
            symbol: s.symbol.clone(),
            interval: s.interval.clone().expect("a bar spec has one"),
        },
        &b,
    );
    assert_eq!(mine, theirs);

    // ANTI-VACUITY: the comparison is over a non-trivial object, and the optional columns are
    // genuinely ABSENT here rather than present-and-null — which is the property being pinned.
    assert_eq!(mine.as_object().map(|m| m.len()), Some(10));
    assert!(mine.get("bid").is_none(), "{mine}");

    // ...and a bar that HAS them carries all three, in both verbs.
    let mut rich = bar(1);
    rich.funding = Some(0.0001);
    rich.bid = Some(9.0);
    rich.ask = Some(11.0);
    let rich_mine: Value = serde_json::from_str(&jsonl_line(&bar_cells(&s, &rich))).expect("json");
    assert_eq!(rich_mine.as_object().map(|m| m.len()), Some(13));
    assert_eq!(
        rich_mine,
        crate::cmd::data::get::json_row(
            &crate::cmd::data::get::Spec {
                venue: s.venue.clone(),
                symbol: s.symbol.clone(),
                interval: s.interval.clone().expect("a bar spec has one"),
            },
            &rich,
        )
    );
}

/// The header comes from the roster, and the row comes from the same roster — so the two have
/// the same width for every kind.
#[test]
fn every_kinds_header_and_row_are_the_same_width() {
    let s = Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: None };
    let bar_spec = spec();
    let rows: [(Kind, Cells); 3] = [
        (Kind::Bar, bar_cells(&bar_spec, &bar(1))),
        (
            Kind::Quote,
            quote_cells(
                &s,
                &QuoteTick {
                    ts: 1,
                    local_ts: 2,
                    bid: 1.0,
                    ask: 2.0,
                    bid_size: 3.0,
                    ask_size: 4.0,
                    symbol: String::new(),
                },
            ),
        ),
        (
            Kind::Trade,
            trade_cells(
                &s,
                &TradeTick {
                    ts: 1,
                    local_ts: 2,
                    price: 5.0,
                    size: 6.0,
                    is_buyer_maker: true,
                    symbol: String::new(),
                },
            ),
        ),
    ];
    for (kind, cells) in rows {
        let header = csv_header(kind);
        let line = csv_line(&cells);
        assert_eq!(
            header.split(',').count(),
            line.split(',').count(),
            "{}: header {header:?} vs row {line:?}",
            kind.as_str()
        );
        // ANTI-VACUITY: the widths are not both zero, and the roster names match the cells'.
        assert!(header.split(',').count() >= 7, "{}", kind.as_str());
        assert_eq!(
            kind.columns(),
            cells.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            "{}",
            kind.as_str()
        );
    }
}

/// The three CSV decisions, each asserted where it bites.
#[test]
fn the_csv_rules_are_minimal_quoting_an_empty_null_and_a_header_that_always_lands() {
    // QUOTING — minimal: a plain value is bare, and only the four characters that break a
    // parser are quoted.
    assert_eq!(csv_field("BTCUSDT"), "BTCUSDT");
    assert_eq!(csv_field("1.5"), "1.5");
    assert_eq!(csv_field("a,b"), "\"a,b\"");
    assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");

    // NULL — an absent cell and a non-finite number are BOTH the empty field, which is the
    // cost this rule's doc states out loud.
    let cells =
        vec![("a", Some(json!(1))), ("b", None), ("c", Some(Value::Null)), ("d", Some(json!("x")))];
    assert_eq!(csv_line(&cells), "1,,,x");
    // ANTI-VACUITY: the same cells under `jsonl` keep `b` and `c` APART — `b` is absent, `c` is
    // null — which is the reason to prefer that form and would be invisible if both were empty.
    let line = jsonl_line(&cells);
    assert!(!line.contains("\"b\""), "{line}");
    assert!(line.contains("\"c\":null"), "{line}");

    // HEADER — present for every kind, even one whose export has no rows.
    for kind in Kind::ALL {
        let h = csv_header(kind);
        assert!(h.starts_with("venue,symbol,"), "{}: {h}", kind.as_str());
    }
}

/// An empty answer says what it does and does not mean, and a non-empty one does not carry the
/// note at all.
#[test]
fn an_empty_export_states_both_readings_and_a_full_one_says_nothing_extra() {
    let empty = summary(&plan(Wire::Jsonl), "out.jsonl", &Written { rows: 0, windows: 3 });
    assert_eq!(empty.len(), 2);
    assert!(empty[0].contains("3 windows"), "{:?}", empty[0]);
    assert!(empty[0].contains("binance:BTCUSDT:1h"), "{:?}", empty[0]);
    assert!(empty[1].contains("ONE of two facts"), "{:?}", empty[1]);
    assert!(empty[1].contains("--venue binance --name BTCUSDT"), "{:?}", empty[1]);
    // ⚠ The two formats describe the empty FILE differently, because a jsonl export of nothing
    // really is zero bytes while a csv one still carries its header.
    assert!(empty[1].contains("that file is empty"), "{:?}", empty[1]);
    let empty_csv = summary(&plan(Wire::Csv), "out.csv", &Written { rows: 0, windows: 1 });
    assert!(empty_csv[1].contains("a header and no rows"), "{:?}", empty_csv[1]);

    let full = summary(&plan(Wire::Jsonl), "out.jsonl", &Written { rows: 9, windows: 1 });
    assert_eq!(full.len(), 1);
    assert!(full[0].contains("1 window)"), "{:?}", full[0]);

    // The step note fires only when the flag was DEFAULTED and there was more than one window
    // to tune — a note on every run stops being read.
    assert!(step_note(&plan(Wire::Jsonl), &Written { rows: 9, windows: 1 }).is_none());
    let mut named = plan(Wire::Jsonl);
    named.step_defaulted = false;
    assert!(step_note(&named, &Written { rows: 9, windows: 4 }).is_none());
    // ANTI-VACUITY: the case it IS for fires, and names the flag.
    let note = step_note(&plan(Wire::Jsonl), &Written { rows: 9, windows: 4 })
        .expect("a defaulted multi-window walk is exactly what this note is for");
    assert!(note.contains(WINDOW_FLAG), "{note}");
}

/// The failed-window note names the lever and where to resume, because a byte count does not.
#[test]
fn a_failed_window_names_the_flag_that_lowers_it_and_where_to_resume() {
    let note = read_failed_note("binance:BTCUSDT:1h", 100, 200, 101, "connection reset");
    assert!(note.contains("connection reset"), "{note}");
    assert!(note.contains("--window SPAN"), "{note}");
    assert!(note.contains("--from 100"), "{note}");
}

/// The `--json` document carries the route and the columns, and names no engine.
#[test]
fn the_document_describes_a_remote_route_and_quotes_no_engine() {
    let doc = json_doc(
        "export",
        "127.0.0.1:7878",
        "out.csv",
        &plan(Wire::Csv),
        &Written { rows: 2, windows: 3 },
    );
    let v: Value = serde_json::from_str(&doc).expect("json");
    assert_eq!(v["route"], json!("remote"));
    assert_eq!(v["subcommand"], json!("export"));
    assert_eq!(v["format"], json!("csv"));
    assert_eq!(v["window"]["step_ms"], json!(5));
    assert_eq!(v["rows"], json!(2));
    assert_eq!(v["columns"], json!(Kind::Bar.columns()));
    // ANTI-VACUITY: the fields the ENGINE route's document carries are absent here rather than
    // null, which is the divergence `json_doc`'s doc argues for.
    assert!(v.get("engine").is_none(), "{doc}");
    assert!(v.get("engine_argv").is_none(), "{doc}");
}
