use super::*;

/// The three leaf shapes `parse_series_id` must tell apart, each driven through the REAL encode
/// side (`ticks_dir` / `bars_dir` / `group_dir`) rather than a hand-built string, so the test
/// exercises the actual encode/parse pair the store round-trips through.
///
/// The empty one is the defect: `crates/vike-data/src/store/series.rs`'s `SeriesId::group` reserves an
/// empty `symbol` as the GROUPED-series sentinel, while this parser accepted `symbol=` and
/// handed back `SeriesId { symbol: "", group: None }` — a PER-SYMBOL id carrying the sentinel
/// value, addressable by no scan and recognisable as grouped by nothing. The sentinel and the
/// parser disagreed about the same value.
#[test]
fn an_empty_symbol_segment_is_refused_while_both_real_layouts_round_trip() {
    let d = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(d.path()).unwrap();

    // Control 1: an ordinary tick series still round-trips.
    assert_eq!(
        parse_series_id(d.path(), &df.ticks_dir("quote", "binance", "BTCUSDT")),
        Some(SeriesId::per_symbol("quote", "binance", "BTCUSDT", None)),
        "a per-symbol tick leaf must still parse"
    );

    // Control 2: so does a bar series, whose leaf carries the extra `interval=` segment.
    assert_eq!(
        parse_series_id(d.path(), &df.bars_dir("binance", "BTCUSDT", "1m")),
        Some(SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string()))),
        "a per-symbol bar leaf must still parse, interval included"
    );

    // Control 3: and a GROUPED leaf, which has no `symbol=` segment at all.
    assert_eq!(
        parse_series_id(d.path(), &df.group_dir("quote", "polymarket", "btc-5m", None)),
        Some(SeriesId::grouped("quote", "polymarket", "btc-5m")),
        "a grouped leaf must still parse as grouped"
    );

    // The finding: the same encoder, handed the sentinel value, produces a `symbol=` leaf.
    let sentinel = df.ticks_dir("exec_fill", "binance", "");
    assert!(
        sentinel.ends_with("symbol="),
        "the fixture really is the empty-`symbol=` shape: {}",
        sentinel.display()
    );
    assert_eq!(
        parse_series_id(d.path(), &sentinel),
        None,
        "`symbol=` is the grouped sentinel, not a symbol — it must not parse as a per-symbol id"
    );
}

/// An empty `group=` is deliberately NOT refused alongside it. It is degenerate-looking but
/// UNAMBIGUOUS: `group.is_some()` — the field every consumer tells the two layouts apart by —
/// still answers correctly, so it collides with no sentinel. Pinned so the narrower rule is a
/// decision on the record rather than an oversight someone "tidies up" later.
#[test]
fn an_empty_group_segment_is_not_refused() {
    let d = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(d.path()).unwrap();
    let id = parse_series_id(d.path(), &df.group_dir("quote", "binance", "", None))
        .expect("an empty group= still parses");
    assert!(id.group.is_some(), "it is recognisable as GROUPED, which is the whole test");
}

/// **`series_dir_of` and `parse_series_id` are inverses, with a `source=` segment and without
/// one.** The private half of the `source=` dimension's pin — the public half lives in
/// `crates/vike-data/tests/hist_datafusion.rs`, which cannot reach either function.
///
/// The round trip is what makes every `SeriesId`-taking verb correct for a sourced leaf without
/// a signature change: `list_series` parses an id OUT of a path and `series_dir_of` builds the
/// same path back IN. If the two disagreed, `series_coverage` would read a phantom directory,
/// `read_manifest` would answer EMPTY for it rather than failing, and the verb would report
/// "no data" for a series holding rows — the exact five-bug class `series_dir_of`'s own doc
/// records from the grouping rollout.
#[test]
fn a_source_survives_the_path_round_trip_in_both_layouts() {
    let d = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(d.path()).unwrap();
    for id in [
        // The CONTROLS: sourceless, both layouts, unchanged by the dimension existing.
        SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string())),
        SeriesId::grouped("quote", "polymarket", "btc-5m"),
        // ...and the same four, sourced.
        SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string()))
            .with_source("venue"),
        SeriesId::per_symbol("quote", "polymarket", "TOK", None).with_source("pmxt"),
        SeriesId::grouped("quote", "polymarket", "btc-5m").with_source("recorder"),
        SeriesId::grouped("book", "polymarket", "btc-5m").with_source("vikearchive"),
    ] {
        let dir = df.series_dir_of(&id);
        assert_eq!(parse_series_id(d.path(), &dir), Some(id.clone()), "{}", dir.display());
    }

    // The ANTI-VACUITY control: the round trip is not a no-op that would pass for anything.
    // Two ids differing ONLY by lane must build two DIFFERENT directories, and the sourceless
    // one must not acquire a segment.
    let plain = SeriesId::per_symbol("quote", "polymarket", "TOK", None);
    let sourced = plain.clone().with_source("pmxt");
    assert_ne!(df.series_dir_of(&plain), df.series_dir_of(&sourced));
    assert!(!df.series_dir_of(&plain).to_string_lossy().contains("source="));
    assert!(df.series_dir_of(&sourced).to_string_lossy().contains("source=pmxt"));

    // ⚠ **And the POSITION, asserted against the BUILDER.** The round trip above cannot see
    // this: `parse_series_id` matches segment prefixes in whatever order the path yields them,
    // so `symbol=TOK/source=pmxt` parses to the same id and round-trips perfectly — while being
    // the one layout §4.1 rules out, because `find_manifest_series_dirs` stops at the first
    // `_manifest.json` and would never descend past the legacy leaf to reach it. MEASURED: with
    // the segment moved below the leaf, every other assertion in this function still passed.
    let segs = |id: &SeriesId| -> Vec<String> {
        df.series_dir_of(id)
            .strip_prefix(d.path())
            .expect("built under the root")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(segs(&sourced), ["kind=quote", "venue=polymarket", "source=pmxt", "symbol=TOK"]);
    assert_eq!(
        segs(&SeriesId::grouped("book", "polymarket", "btc-5m").with_source("recorder")),
        ["kind=book", "venue=polymarket", "source=recorder", "group=btc-5m"],
        "the grouped layout puts the segment in the same place — the two must not disagree, or \
             one of them ends up below a manifest-bearing directory"
    );
}

/// The WRITE side of the same law, and the half with live-money consequences.
///
/// A reconcile pre-seed scans every `kind=exec_fill` leaf by `id.symbol` and folds the trade_ids
/// into the SEEN-FILL dedup set (`vike-app`'s did until the GUI's local core went; the walk is
/// `vike_core::journal_view_from_store` today, reached by no production root yet). A fill missing
/// from that set reads as a
/// `MissingFill` — one of the two kinds `hybrid` auto-applies — so an unattributable fill row is
/// a double-book waiting for the next pass, not merely an unreadable one.
///
/// This must hold together with the parser above: refusing the leaf on READ while still
/// accepting it on WRITE would strand exactly those trade_ids.
#[test]
fn an_exec_fill_with_no_symbol_is_refused_and_a_normal_one_is_not() {
    let d = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(d.path()).unwrap();

    let err = df
        .append_exec_fills("binance", "", &[], None)
        .expect_err("an unattributable fill must never become durable");
    let msg = err.to_string();
    assert!(msg.contains("empty symbol"), "the refusal names the offence: {msg}");
    assert!(msg.contains("seen-fill"), "and why it matters, not just that it is unaddressable");

    // The control: a normal symbol still appends. Without it, a guard that refused EVERYTHING
    // would pass the assertion above.
    df.append_exec_fills("binance", "BTCUSDT", &[], None)
        .expect("an attributable fill still appends");
}
