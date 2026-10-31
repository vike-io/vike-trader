use super::*;

#[test]
fn setters_fill_the_matching_slot() {
    let mut b = PriceBoard::default();
    b.set_mark("bybit", "BTCUSDT", 50_000.0, 1_000);
    b.set_quote("bybit", "BTCUSDT", 49_999.0, 50_001.0, 1_001);
    b.set_last_trade("bybit", "BTCUSDT", 50_000.5, 1_002);
    b.set_bar_close("bybit", "BTCUSDT", 49_990.0, 1_003);
    let c = b.cell("bybit", "BTCUSDT").expect("cell exists");
    assert_eq!(c.mark, Some((50_000.0, 1_000)));
    assert_eq!(c.bid, Some((49_999.0, 1_001)));
    assert_eq!(c.ask, Some((50_001.0, 1_001)));
    assert_eq!(c.last_trade, Some((50_000.5, 1_002)));
    assert_eq!(c.bar_close, Some((49_990.0, 1_003)));
}

#[test]
fn later_write_overwrites_same_slot_only() {
    let mut b = PriceBoard::default();
    b.set_mark("okx", "ETHUSDT", 3_000.0, 1);
    b.set_mark("okx", "ETHUSDT", 3_001.0, 2);
    let c = b.cell("okx", "ETHUSDT").unwrap();
    assert_eq!(c.mark, Some((3_001.0, 2)));
    assert_eq!(c.bid, None);
}

#[test]
fn non_positive_and_nan_prices_are_ignored() {
    let mut b = PriceBoard::default();
    b.set_mark("bybit", "X", 0.0, 1);
    b.set_mark("bybit", "X", -1.0, 2);
    b.set_mark("bybit", "X", f64::NAN, 3);
    b.set_last_trade("bybit", "X", 0.0, 4);
    b.set_bar_close("bybit", "X", -5.0, 5);
    assert!(b.cell("bybit", "X").is_none(), "no slot may be created by dead prices");
    // a quote with ONE dead side stores only the live side
    b.set_quote("bybit", "X", 0.0, 101.0, 6);
    let c = b.cell("bybit", "X").unwrap();
    assert_eq!(c.bid, None);
    assert_eq!(c.ask, Some((101.0, 6)));
}

#[test]
fn cells_are_venue_scoped() {
    let mut b = PriceBoard::default();
    b.set_mark("bybit", "BTCUSDT", 1.0, 1);
    b.set_mark("okx", "BTCUSDT", 2.0, 1);
    assert_eq!(b.cell("bybit", "BTCUSDT").unwrap().mark, Some((1.0, 1)));
    assert_eq!(b.cell("okx", "BTCUSDT").unwrap().mark, Some((2.0, 1)));
}

fn full_cell() -> PriceBoard {
    let mut b = PriceBoard::default();
    b.set_mark("v", "S", 100.0, 1_000);
    b.set_quote("v", "S", 99.0, 101.0, 2_000);
    b.set_last_trade("v", "S", 100.5, 3_000);
    b.set_bar_close("v", "S", 98.0, 4_000);
    b
}

#[test]
fn chain_prefers_fresh_mark_then_side_quote_then_trade_then_bar() {
    let b = full_cell();
    let cfg = PriceCfg::default();
    // 1. mark wins when enabled
    assert_eq!(
        b.resolve("v", "S", true, 5_000, &cfg),
        Resolution::Priced { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
    );
    // 2. mark disabled -> side-appropriate quote: Bid for long, Ask for short
    let cfg_nm = PriceCfg { use_mark: false, ..PriceCfg::default() };
    assert_eq!(
        b.resolve("v", "S", true, 5_000, &cfg_nm),
        Resolution::Priced { px: 99.0, source: PriceSource::Bid, ts: 2_000 }
    );
    assert_eq!(
        b.resolve("v", "S", false, 5_000, &cfg_nm),
        Resolution::Priced { px: 101.0, source: PriceSource::Ask, ts: 2_000 }
    );
}

#[test]
fn stale_sources_fall_through_the_chain() {
    let b = full_cell();
    // everything but bar_close aged out at now=10_000
    let cfg = PriceCfg {
        use_mark: true,
        mark_max_age_ms: Some(1_000), // mark ts 1_000, age 9_000 -> stale
        quote_max_age_ms: Some(1_000), // quote ts 2_000, age 8_000 -> stale
        trade_max_age_ms: Some(1_000), // trade ts 3_000, age 7_000 -> stale
        bar_max_age_ms: None,         // no limit -> fresh
        stale_fallback: false,        // bar is fresh here; fallback irrelevant (default off)
        last_known_by_recency: false,
    };
    assert_eq!(
        b.resolve("v", "S", true, 10_000, &cfg),
        Resolution::Priced { px: 98.0, source: PriceSource::BarClose, ts: 4_000 }
    );
}

#[test]
fn freshness_boundary_is_inclusive_at_exactly_max_age() {
    let mut b = PriceBoard::default();
    b.set_mark("v", "S", 100.0, 1_000);
    let cfg = PriceCfg { use_mark: true, mark_max_age_ms: Some(500), ..PriceCfg::default() };
    // age == max_age (500) -> still FRESH (the impl's `fresh` uses strict `>`)
    assert_eq!(
        b.resolve("v", "S", true, 1_500, &cfg),
        Resolution::Priced { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
    );
    // age == max_age + 1 (501) -> stale; only mark is set, so falls through to Missing
    assert_eq!(b.resolve("v", "S", true, 1_501, &cfg), Resolution::Missing);
}

#[test]
fn one_sided_quote_falls_to_next_source_for_the_missing_side() {
    let mut b = PriceBoard::default();
    b.set_quote("v", "S", 99.0, 0.0, 1_000); // bid only
    b.set_last_trade("v", "S", 100.5, 2_000);
    let cfg = PriceCfg { use_mark: false, ..PriceCfg::default() };
    // long -> bid exists
    assert_eq!(
        b.resolve("v", "S", true, 3_000, &cfg),
        Resolution::Priced { px: 99.0, source: PriceSource::Bid, ts: 1_000 }
    );
    // short -> no ask -> falls to last trade
    assert_eq!(
        b.resolve("v", "S", false, 3_000, &cfg),
        Resolution::Priced { px: 100.5, source: PriceSource::LastTrade, ts: 2_000 }
    );
}

#[test]
fn unknown_symbol_and_empty_cell_resolve_missing() {
    let b = PriceBoard::default();
    assert_eq!(b.resolve("v", "NOPE", true, 1, &PriceCfg::default()), Resolution::Missing);
}

#[test]
fn note_tracks_missing_and_rearms_on_recovery() {
    let mut b = PriceBoard::default();
    let miss = Resolution::Missing;
    b.note("v", "S", &miss);
    b.note("v", "S", &miss); // second miss of the same episode: still tracked once
    assert_eq!(
        b.missing_price_instruments("v").map(|s| s.iter().cloned().collect::<Vec<_>>()),
        Some(vec!["S".to_string()])
    );
    // recovery clears the set (and re-arms the warn for a future episode)
    let ok = Resolution::Priced { px: 1.0, source: PriceSource::Mark, ts: 1 };
    b.note("v", "S", &ok);
    assert!(b.missing_price_instruments("v").is_none_or(|s| s.is_empty()));
    // a new episode is tracked again
    b.note("v", "S", &miss);
    assert!(b.missing_price_instruments("v").is_some_and(|s| s.contains("S")));
}

// --- last-known floor (rung 5) + classify (Ext 1) -----------------------------------------

/// Aged-out windows on every rung (all `Some(1)`), so at a late `now` the fresh chain finds
/// nothing and the last-known floor is what's under test.
fn all_aged() -> PriceCfg {
    PriceCfg {
        mark_max_age_ms: Some(1),
        quote_max_age_ms: Some(1),
        trade_max_age_ms: Some(1),
        bar_max_age_ms: Some(1),
        ..PriceCfg::default()
    }
}

/// THE inert guarantee: a FRESH venue mark early-returns `Priced { Mark }` no matter how
/// `stale_fallback` is set — the common case is byte-identical, the floor only ever helps a
/// stale-everywhere cell.
#[test]
fn fresh_mark_is_byte_identical_regardless_of_stale_fallback() {
    let b = full_cell();
    let want = Resolution::Priced { px: 100.0, source: PriceSource::Mark, ts: 1_000 };
    assert_eq!(b.resolve("v", "S", true, 5_000, &PriceCfg::default()), want);
    assert_eq!(
        b.resolve("v", "S", true, 5_000, &PriceCfg { stale_fallback: true, ..Default::default() }),
        want
    );
}

/// `classify` reports Fresh / Stale / Missing independent of `stale_fallback`.
#[test]
fn classify_buckets_fresh_stale_and_missing() {
    let b = full_cell();
    assert_eq!(
        b.classify("v", "S", true, 5_000, &PriceCfg::default()),
        MarkStatus::Fresh { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
    );
    // every window aged out -> Stale, highest-priority present slot = mark
    assert_eq!(
        b.classify("v", "S", true, 100_000, &all_aged()),
        MarkStatus::Stale { px: 100.0, source: PriceSource::Mark, ts: 1_000, age_ms: 99_000 }
    );
    // unknown symbol -> Missing (classify is stale_fallback-independent)
    assert_eq!(b.classify("v", "NOPE", true, 1, &all_aged()), MarkStatus::Missing);
}

/// The behavior switch: the SAME aged-out cell resolves `Missing` with the floor OFF (today's
/// behavior) and `Stale` with it ON — carrying the highest-priority surviving slot.
#[test]
fn stale_fallback_flips_missing_to_last_known() {
    let b = full_cell();
    assert_eq!(b.resolve("v", "S", true, 100_000, &all_aged()), Resolution::Missing);
    assert_eq!(
        b.resolve("v", "S", true, 100_000, &PriceCfg { stale_fallback: true, ..all_aged() }),
        Resolution::Stale { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
    );
}

/// Last-known picks by CHAIN PRIORITY, not recency: with the mark rung disabled the floor
/// falls to the side quote (Bid for a long) even though last-trade / bar-close are NEWER.
#[test]
fn last_known_follows_chain_priority_not_recency() {
    let b = full_cell();
    let on = PriceCfg {
        use_mark: false,
        mark_max_age_ms: Some(1),
        quote_max_age_ms: Some(1),
        trade_max_age_ms: Some(1),
        bar_max_age_ms: Some(1),
        stale_fallback: true,
        last_known_by_recency: false,
    };
    assert_eq!(
        b.resolve("v", "S", true, 100_000, &on),
        Resolution::Stale { px: 99.0, source: PriceSource::Bid, ts: 2_000 }
    );
}

/// The OPT-IN twin of the test above: with `last_known_by_recency` the floor is the NEWEST slot, whatever its
/// rung. `full_cell` stamps the mark at 1_000, the quote at 2_000, the trade at 3_000 and the bar close at 4_000,
/// so the same aged-out cell that answered the bid by priority answers the bar close by recency; a tie goes to the
/// higher rung; the default (false) is the priority answer, and the fresh path never reads the flag.
#[test]
fn last_known_by_recency_takes_the_newest_slot_and_a_tie_keeps_the_higher_rung() {
    let b = full_cell();
    let aged = PriceCfg {
        use_mark: false,
        mark_max_age_ms: Some(1),
        quote_max_age_ms: Some(1),
        trade_max_age_ms: Some(1),
        bar_max_age_ms: Some(1),
        stale_fallback: true,
        last_known_by_recency: true,
    };
    assert_eq!(
        b.classify("v", "S", true, 100_000, &aged),
        MarkStatus::Stale { px: 98.0, source: PriceSource::BarClose, ts: 4_000, age_ms: 96_000 },
        "the newest slot, not the highest-priority one"
    );
    assert_eq!(
        b.resolve("v", "S", true, 100_000, &aged),
        Resolution::Stale { px: 98.0, source: PriceSource::BarClose, ts: 4_000 }
    );
    let by_priority = PriceCfg { last_known_by_recency: false, ..aged };
    assert_eq!(
        b.resolve("v", "S", true, 100_000, &by_priority),
        Resolution::Stale { px: 99.0, source: PriceSource::Bid, ts: 2_000 },
        "off, the answer is the priority one as before"
    );

    let mut tie = PriceBoard::default();
    tie.set_last_trade("v", "S", 50.0, 7_000);
    tie.set_bar_close("v", "S", 51.0, 7_000);
    assert_eq!(
        tie.classify("v", "S", true, 100_000, &aged),
        MarkStatus::Stale { px: 50.0, source: PriceSource::LastTrade, ts: 7_000, age_ms: 93_000 },
        "equal timestamps: the higher rung"
    );

    // a fresh rung wins over any recency: the flag is read by the relaxed walk only
    let fresh = PriceCfg { bar_max_age_ms: None, ..aged };
    assert_eq!(
        b.classify("v", "S", true, 100_000, &fresh),
        MarkStatus::Fresh { px: 98.0, source: PriceSource::BarClose, ts: 4_000 }
    );
}

/// The floor cannot invent a price: a cell that never held any value is Missing even ON.
#[test]
fn stale_fallback_never_prices_an_empty_cell() {
    let b = PriceBoard::default();
    let on = PriceCfg { stale_fallback: true, ..PriceCfg::default() };
    assert_eq!(b.resolve("v", "GHOST", true, 1, &on), Resolution::Missing);
    assert_eq!(b.classify("v", "GHOST", true, 1, &on), MarkStatus::Missing);
}

/// `note` treats a Stale resolution as a price (clears the missing set), like Priced.
#[test]
fn note_clears_missing_on_a_stale_resolution() {
    let mut b = PriceBoard::default();
    b.note("v", "S", &Resolution::Missing);
    assert!(b.missing_price_instruments("v").is_some_and(|s| s.contains("S")));
    b.note("v", "S", &Resolution::Stale { px: 1.0, source: PriceSource::BarClose, ts: 1 });
    assert!(b.missing_price_instruments("v").is_none_or(|s| s.is_empty()));
}

/// `cell` scans a small board and hashes a large one; both must answer the cell the plain scan
/// would, for a hit and for every kind of miss (the key halves swapped, one half a prefix of a
/// stored one, a venue that holds the symbol under another venue's name).
#[test]
fn a_hashed_lookup_answers_what_the_scan_answers() {
    for count in [1, LINEAR_SCAN_CELLS, LINEAR_SCAN_CELLS + 1, 40] {
        let mut b = PriceBoard::default();
        for i in 0..count {
            let venue = if i % 2 == 0 { "sim" } else { "okx" };
            b.set_mark(venue, &format!("SYM{i}USDT"), 100.0 + i as f64, i as i64);
        }
        let scan = |v: &str, s: &str| {
            b.cells.iter().find(|((cv, cs), _)| cv == v && cs == s).map(|(_, c)| c)
        };
        for i in 0..count {
            let venue = if i % 2 == 0 { "sim" } else { "okx" };
            let symbol = format!("SYM{i}USDT");
            let hit = b.cell(venue, &symbol).expect("a stored cell is found");
            assert_eq!(hit.mark, Some((100.0 + i as f64, i as i64)), "{count} cells, {symbol}");
            // The other venue never holds this symbol.
            let other = if venue == "sim" { "okx" } else { "sim" };
            assert!(b.cell(other, &symbol).is_none(), "{count} cells: {other}/{symbol}");
            assert!(b.cell(&symbol, venue).is_none(), "{count} cells: halves swapped");
            assert!(b.cell(venue, &symbol[..symbol.len() - 1]).is_none(), "prefix of {symbol}");
            assert!(b.cell(&format!("{venue}x"), &symbol).is_none(), "venue plus a letter");
        }
        for (v, s) in
            [("sim", "SYM0"), ("", ""), ("sim", ""), ("", "SYM0USDT"), ("sim", "SYM0USDTx")]
        {
            assert_eq!(b.cell(v, s), scan(v, s), "{count} cells: {v:?}/{s:?}");
        }
    }
}
