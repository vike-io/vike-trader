use super::*;

fn bar(ts: i64) -> Bar {
    vike_marketdata::test_support::distinct_ohlc_bar(ts, 10.0)
}

fn spec() -> Spec {
    Spec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: Some("1h".into()) }
}

/// A bar plan over `[10, 20]` in 5ms steps, with the step DEFAULTED — the shape the notes and
/// the document are exercised against.
fn plan(wire: Wire) -> Plan {
    Plan {
        kind: Kind::Bar,
        spec: spec(),
        wire,
        bounds: (10, 20),
        step_ms: 5,
        step_defaulted: true,
        source: None,
        levels: None,
    }
}

/// `--levels` is a book lane's, a positive whole number, and absent means every event as recorded.
#[test]
fn levels_is_a_positive_count_on_a_book_lane_only() {
    assert_eq!(parse_levels(None, Kind::Depth), Ok(None));
    assert_eq!(parse_levels(Some("10"), Kind::Depth), Ok(Some(10)));
    assert_eq!(parse_levels(Some(" 1 "), Kind::Book), Ok(Some(1)));
    for bad in ["0", "-3", "ten", ""] {
        let err = parse_levels(Some(bad), Kind::Depth).unwrap_err();
        assert!(err.contains(LEVELS_FLAG), "{bad}: {err}");
    }
    let bars = parse_levels(Some("10"), Kind::Bar).unwrap_err();
    assert!(bars.contains("--kind book or --kind depth"), "{bars}");
}

/// A seeded recorded chain on a 0.5 grid: a snapshot of eight levels a side, deltas that add, move
/// and remove levels at every depth (so levels enter and leave the top), two sequence holes, a
/// duplicate, a `GapStart` and a `LiveResume` marker, and re-anchoring snapshots.
fn recorded_chain() -> Vec<L2BookUpdate> {
    use vike_marketdata::L2BookLevel;
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |modulo: u64| {
        state =
            state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % modulo
    };
    let event = |timestamp: i64, sequence: u64, kind, bids, asks| L2BookUpdate {
        timestamp,
        local_timestamp: timestamp + 3,
        sequence,
        kind,
        tick_size: 0.5,
        bids,
        asks,
        symbol: String::new(),
    };
    let side = |from: f64, step: f64| -> Vec<L2BookLevel> {
        (0..8).map(|i| L2BookLevel::new(from + step * f64::from(i), 1.0 + f64::from(i))).collect()
    };
    let anchor = |timestamp, sequence| {
        event(timestamp, sequence, L2BookUpdateKind::Snapshot, side(99.5, -0.5), side(100.5, 0.5))
    };
    let mut chain = vec![anchor(0, 1)];
    let mut sequence = 1u64;
    for i in 1..400i64 {
        let timestamp = i / 2;
        match i {
            100 | 260 => sequence += 2, // a hole: the delta below lands on no book
            150 => {
                chain.push(event(timestamp, sequence, L2BookUpdateKind::Delta, vec![], vec![]));
                continue; // a duplicate of the last sequence: the chain refuses it
            }
            200 => {
                chain.push(event(timestamp, 0, L2BookUpdateKind::GapStart, vec![], vec![]));
                continue;
            }
            300 => {
                chain.push(event(timestamp, 0, L2BookUpdateKind::LiveResume, vec![], vec![]));
                continue;
            }
            105 | 155 | 205 | 265 => {
                sequence += 1;
                chain.push(anchor(timestamp, sequence));
                continue;
            }
            _ => sequence += 1,
        }
        // A level at a random depth; a quantity under 2 is a removal (`0`).
        let mut level = |base: f64, sign: f64| {
            let price = base + sign * 0.5 * next(12) as f64;
            let raw = next(6);
            L2BookLevel::new(price, if raw < 2 { 0.0 } else { raw as f64 })
        };
        let bids = vec![level(99.5, -1.0)];
        let asks = vec![level(100.5, 1.0), level(100.5, 1.0)];
        chain.push(event(timestamp, sequence, L2BookUpdateKind::Delta, bids, asks));
    }
    chain
}

/// ⚠ **THE EXACTNESS PROPERTY of `--levels N`**: replaying the written rows through the recorded
/// chain's one rule gives, after EVERY row, the top N of the recorded book after the same event: the
/// same levels on both sides, and no book exactly where the recorder had none.
#[test]
fn replaying_the_top_levels_gives_the_top_of_the_recorded_book() {
    let chain = recorded_chain();
    for levels in [1, 3, 20] {
        let mut top = BookTopLevels::new(levels);
        let (mut recorded, mut replayed): (Option<L2Book>, Option<L2Book>) = (None, None);
        let (mut books, mut holes) = (0, 0);
        for event in &chain {
            let row = top.row(event);
            apply_book_event(&mut recorded, event, event.tick_size, |book| book);
            apply_book_event(&mut replayed, &row, row.tick_size, |book| book);
            match (&recorded, &replayed) {
                (Some(a), Some(b)) => {
                    assert_eq!(a.top_n(levels), b.top_n(levels), "seq {}", event.sequence);
                    books += 1;
                }
                (None, None) => holes += 1,
                _ => panic!("seq {}: one side holds a book, the other none", event.sequence),
            }
            if row.kind == L2BookUpdateKind::Snapshot {
                assert!(row.bids.len() <= levels && row.asks.len() <= levels, "{row:?}");
            }
        }
        // ANTI-VACUITY: the chain had books and holes, and the top really is narrower than it.
        assert!(books > 300 && holes > 0, "{levels}: {books} books, {holes} holes");
        assert!(recorded.as_ref().is_some_and(|b| b.bid_levels() > 3), "the book is deeper than 3");
    }
}

/// Without `--levels` nothing changes; with it, a recorded snapshot keeps its time and sequence and
/// only its best levels.
#[test]
fn the_top_levels_of_a_snapshot_keep_its_time_and_sequence() {
    let chain = recorded_chain();
    let row = BookTopLevels::new(2).row(&chain[0]);
    assert_eq!((row.timestamp, row.local_timestamp, row.sequence), (0, 3, 1));
    assert_eq!(row.bids.iter().map(|l| l.price).collect::<Vec<_>>(), [99.5, 99.0]);
    assert_eq!(row.asks.iter().map(|l| l.price).collect::<Vec<_>>(), [100.5, 101.0]);
}

/// The five kinds are the row verbs this route walks, and the roster is the authority for every
/// message that lists them.
#[test]
fn the_kind_roster_is_the_five_this_route_reads() {
    assert_eq!(Kind::ALL.map(Kind::as_str), ["bar", "quote", "trade", "book", "depth"]);
    assert_eq!(served_kinds(), "bar | quote | trade | book | depth");
    // ANTI-VACUITY: the roster is not empty and the rendering is not the empty string, so a
    // future refactor that emptied `ALL` would fail here rather than render blank messages.
    assert_eq!(Kind::ALL.len(), 5);
    assert!(!served_kinds().is_empty());
    // The book lanes parse, have no interval, and walk the narrowest default window.
    for kind in [Kind::Book, Kind::Depth] {
        assert_eq!(parse_kind(Some(kind.as_str())), Ok(kind));
        assert!(!kind.takes_an_interval());
        assert_eq!(kind.default_window_ms(), 10 * 60_000);
        assert_eq!(kind.columns(), Kind::Book.columns());
    }
}

/// A recorded book event is ONE row, exactly as recorded: its chain number, its event kind in the
/// journal's own spelling, its grid and its levels as `[price, quantity]` pairs, in both forms.
#[test]
fn a_book_row_is_one_recorded_event_in_both_forms() {
    use vike_marketdata::{L2BookLevel, L2BookUpdateKind};
    let s = Spec { venue: "binance".into(), symbol: "BTCUSDT.P".into(), interval: None };
    let event = L2BookUpdate {
        timestamp: 1_700_000_000_000,
        local_timestamp: 1_700_000_000_007,
        sequence: 42,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: 0.1,
        bids: vec![L2BookLevel::new(100.0, 2.0), L2BookLevel::new(99.9, 0.5)],
        asks: vec![L2BookLevel::new(100.1, 1.5)],
        symbol: String::new(),
    };
    let cells = book_cells(&s, &event);
    let row: Value = serde_json::from_str(&jsonl_line(&cells)).expect("one JSON object");
    assert_eq!(
        row,
        json!({
            "venue": "binance", "symbol": "BTCUSDT.P", "timestamp": 1_700_000_000_000_i64,
            "timestamp_utc": "2023-11-14T22:13:20Z", "local_timestamp": 1_700_000_000_007_i64,
            "sequence": 42, "event": "Snapshot", "tick_size": 0.1,
            "bids": [[100.0, 2.0], [99.9, 0.5]], "asks": [[100.1, 1.5]],
        })
    );
    assert_eq!(
        csv_line(&cells),
        "binance,BTCUSDT.P,1700000000000,2023-11-14T22:13:20Z,1700000000007,42,Snapshot,0.1,\
         \"[[100.0,2.0],[99.9,0.5]]\",\"[[100.1,1.5]]\""
    );
    // A stream-health marker carries no levels: two empty arrays, never a missing column.
    let gap =
        L2BookUpdate { kind: L2BookUpdateKind::GapStart, bids: vec![], asks: vec![], ..event };
    let line = csv_line(&book_cells(&s, &gap));
    assert!(line.ends_with(",GapStart,0.1,[],[]"), "{line}");
}

/// §9.3.2 outranks this verb's own shape, and a market kind THIS ROUTE cannot read is refused
/// with a DIFFERENT sentence — the two must not blur.
///
/// ⚠ Renamed from `..._a_book_meets_the_wire_one` and then from `..._a_book_meets_the_route_one`
/// (this route reads a book since 2026-11-16): the sentence a cohort meets is not about the WIRE. `docs/decisions/0084-only-the-datahub-touches-the-store.md` put book,
/// depth, cohort, perp-metric, equity and exec-fill reads ON the wire, so the bound this
/// refusal reports moved to this route — and the assertions below moved with it, because a
/// pinned sentence that has become false is worse than an unpinned one.
#[test]
fn an_account_kind_meets_the_plane_sentence_and_a_cohort_meets_the_route_one() {
    let account = parse_kind(Some("exec_fill")).unwrap_err();
    assert!(account.contains("ACCOUNT data"), "{account}");
    assert!(account.contains("vike-cli account"), "{account}");
    // ...and it does NOT get the wire sentence, which would be a fact about this verb standing
    // in front of a fact about the plane.
    assert!(!account.contains("THIS ROUTE"), "{account}");
    // ...and its wire half is TRUE of the datahub this CLI talks to: equity, exec fills AND exec
    // orders are on the wire (`ScanExecOrders`), funding payments are not. It said "orders and
    // funding payments are on neither" until the order-history read shipped.
    let order = parse_kind(Some("exec_order")).unwrap_err();
    assert!(order.contains("exec_order — ARE on the"), "{order}");
    assert!(!order.contains("orders and funding"), "orders are on the wire now: {order}");
    assert!(order.contains("funding payments are on neither"), "{order}");

    // A book is READ since 2026-11-16; the route's own sentence now meets a cohort series.
    let cohort = parse_kind(Some("cohort")).unwrap_err();
    assert!(cohort.contains("THIS ROUTE"), "{cohort}");
    // ⚠ ...and it must NOT tell the operator the wire lacks the verb, which is the false
    // statement this change removed and the one that would send them around the server.
    assert!(
        !cohort.contains("reachable by no read verb"),
        "the refusal must not deny a verb the datahub serves: {cohort}"
    );
    assert!(cohort.contains("data history ls --kind cohort"), "{cohort}");
    assert!(!cohort.contains("ACCOUNT data"), "{cohort}");

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
/// [`WIRE_READS_THIS_ROUTE_LACKS`] plus `option_trade`/`properties` must be exactly the MARKET half of
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
    for kind in ["option_trade", "properties"] {
        let msg = parse_kind(Some(kind)).unwrap_err();
        assert!(
            msg.contains("no read verb for it at all"),
            "a kind the wire does NOT serve must say so — {kind}: {msg}"
        );
        assert_eq!(
            msg,
            format!(
                "`--kind {kind}` is not a row shape this route can read. A datahub export walks {} \
                 of the datahub's row verbs — {} — and has an arm for no others. Neither this \
                 route nor the datahub's wire reads this kind: there is no read verb for it at \
                 all, so the wire is what has to change first. `vike-cli data history ls --kind \
                 {kind}` shows what the store holds of it.",
                Kind::ALL.len(),
                served_kinds()
            ),
            "a missing VERB, never told it is THIS ROUTE's missing arm"
        );
    }

    // COMPLETENESS: every market store kind is classified by exactly one of the three groups.
    let mut classified: Vec<&str> = Kind::ALL
        .iter()
        .map(|k| k.as_str())
        .chain(WIRE_READS_THIS_ROUTE_LACKS.iter().copied())
        .chain(["option_trade", "properties"])
        .collect();
    classified.sort_unstable();
    let mut market: Vec<&str> = vike_data::store::store_kind::STORE_KINDS
        .iter()
        .filter(|k| k.kind.plane() == vike_model::StorePlane::Market)
        .map(|k| k.kind.word())
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
    // ANTI-VACUITY: a NON-blank unknown value gets the unserved-kind sentence, not the empty one.
    let unknown = parse_kind(Some("zzz")).unwrap_err();
    assert!(unknown.contains("`--kind zzz` is not a row shape this route can read"), "{unknown}");
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
    assert_eq!(
        parse_wire(Some("zzz")),
        Err(format!(
            "unknown `--format zzz` on a datahub export ({}). `parquet` is the ENGINE route's \
             format — drop --addr.",
            served_formats()
        ))
    );
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
    assert!(Kind::Trade.default_window_ms() > Kind::Depth.default_window_ms());
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
    // A stream-health marker: its two level arrays are `[]`, so no quoted comma splits the line.
    let marker = L2BookUpdate {
        timestamp: 1,
        local_timestamp: 2,
        sequence: 0,
        kind: vike_marketdata::L2BookUpdateKind::GapStart,
        tick_size: 0.1,
        bids: vec![],
        asks: vec![],
        symbol: String::new(),
    };
    let rows: [(Kind, Cells); 4] = [
        (Kind::Book, book_cells(&s, &marker)),
        (Kind::Bar, bar_cells(&bar_spec, &bar(1))),
        (
            Kind::Quote,
            quote_cells(
                &s,
                &TickQuote {
                    timestamp: 1,
                    local_timestamp: 2,
                    bid: 1.0,
                    ask: 2.0,
                    bid_size: 3.0,
                    ask_size: 4.0,
                    symbol: Default::default(),
                },
            ),
        ),
        (
            Kind::Trade,
            trade_cells(
                &s,
                &TickTrade {
                    timestamp: 1,
                    local_timestamp: 2,
                    price: 5.0,
                    size: 6.0,
                    is_buyer_maker: true,
                    symbol: Default::default(),
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
    let written = |rows, windows| Written { rows, windows, out_kept: false };
    let empty = summary(&plan(Wire::Jsonl), "out.jsonl", &written(0, 3));
    assert_eq!(empty.len(), 2);
    assert!(empty[0].contains("3 windows"), "{:?}", empty[0]);
    assert!(empty[0].contains("binance:BTCUSDT:1h"), "{:?}", empty[0]);
    assert!(empty[1].contains("ONE of two facts"), "{:?}", empty[1]);
    assert!(empty[1].contains("--venue binance --name BTCUSDT"), "{:?}", empty[1]);
    // ⚠ The two formats describe the empty FILE differently, because a jsonl export of nothing
    // really is zero bytes while a csv one still carries its header.
    assert!(empty[1].contains("that file is empty"), "{:?}", empty[1]);
    let empty_csv = summary(&plan(Wire::Csv), "out.csv", &written(0, 1));
    assert!(empty_csv[1].contains("a header and no rows"), "{:?}", empty_csv[1]);

    // A kept file is no empty file: neither line may read as if one had been written.
    let kept = Written { out_kept: true, ..written(0, 1) };
    let kept = summary(&plan(Wire::Csv), "out.csv", &kept);
    assert!(kept[0].contains("out.csv, which already exists, was LEFT AS IT WAS"), "{:?}", kept[0]);
    assert!(!kept[0].contains("wrote"), "{:?}", kept[0]);
    assert!(kept[1].contains("nothing came back"), "{:?}", kept[1]);
    assert!(kept[1].contains("ONE of two facts"), "{:?}", kept[1]);

    let full = summary(&plan(Wire::Jsonl), "out.jsonl", &written(9, 1));
    assert_eq!(full.len(), 1);
    assert!(full[0].starts_with("wrote 9 "), "{:?}", full[0]);
    assert!(full[0].contains("1 window)"), "{:?}", full[0]);

    // The step note fires only when the flag was DEFAULTED and there was more than one window
    // to tune — a note on every run stops being read.
    assert!(step_note(&plan(Wire::Jsonl), &written(9, 1)).is_none());
    let mut named = plan(Wire::Jsonl);
    named.step_defaulted = false;
    assert!(step_note(&named, &written(9, 4)).is_none());
    // ANTI-VACUITY: the case it IS for fires, and names the flag.
    let note = step_note(&plan(Wire::Jsonl), &written(9, 4))
        .expect("a defaulted multi-window walk is exactly what this note is for");
    assert!(note.contains(WINDOW_FLAG), "{note}");
}

/// The failed-window note names the lever and that `--out` was not touched, because a byte count
/// does not, and it offers no resume bound: nothing of the walk was kept to resume onto.
#[test]
fn a_failed_window_names_the_flag_that_lowers_it_and_that_out_was_not_touched() {
    let note = read_failed_note("binance:BTCUSDT:1h", 100, 200, 101, "connection reset");
    assert!(note.contains("connection reset"), "{note}");
    assert!(note.contains("--window SPAN"), "{note}");
    assert!(note.contains("--out was not touched"), "{note}");
    assert!(!note.contains("--from"), "{note}");
}

/// The `--json` document carries the route and the columns, and names no engine.
#[test]
fn the_document_describes_a_remote_route_and_quotes_no_engine() {
    let doc = json_doc(
        "export",
        "127.0.0.1:17878",
        "out.csv",
        &plan(Wire::Csv),
        &Written { rows: 2, windows: 3, out_kept: false },
    );
    let v: Value = serde_json::from_str(&doc).expect("json");
    assert_eq!(v["out_kept"], json!(false));
    assert_eq!(v["route"], json!("datahub"));
    assert_eq!(v["subcommand"], json!("export"));
    assert_eq!(v["format"], json!("csv"));
    assert_eq!(v["window"]["step_ms"], json!(5));
    assert_eq!(v["rows"], json!(2));
    assert_eq!(v["columns"], json!(Kind::Bar.columns()));
    // ANTI-VACUITY: the fields the ENGINE route's document carries are absent here rather than
    // null, which is the divergence `json_doc`'s doc argues for.
    assert!(v.get("engine").is_none(), "{doc}");
    assert!(v.get("engine_argv").is_none(), "{doc}");
    // `levels` is reported only when asked for, so a plain export's document is unchanged.
    assert!(v.get("levels").is_none(), "{doc}");
    let depth = Plan {
        kind: Kind::Depth,
        spec: Spec { venue: "binance".into(), symbol: "BTCUSDT.P".into(), interval: None },
        levels: Some(10),
        ..plan(Wire::Jsonl)
    };
    let written = Written { rows: 2, windows: 1, out_kept: false };
    let v: Value = serde_json::from_str(&json_doc("export", "a:1", "o.jsonl", &depth, &written))
        .expect("json");
    assert_eq!(v["levels"], json!(10));
    let line = &summary(&depth, "o.jsonl", &written)[0];
    assert!(
        line.contains("depth rows for binance:BTCUSDT.P (the best 10 levels a side)"),
        "{line}"
    );
}
