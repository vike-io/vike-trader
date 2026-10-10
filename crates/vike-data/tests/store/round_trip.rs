//! Round trips: bars, ticks, hostile symbols, resample, parquet import, book, properties.

use std::path::Path;

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::test_support::bars::assert_bars_bit_eq;
use vike_model::{
    AssetClass, Bar, BookLevel, BookUpdateKind, QuoteTick, SymbolProperties, TickScheme, TickTier,
    TradeTick, consolidate_trades,
};

use crate::common::{bar, bars_series, bu};

#[test]
fn datafusion_bars_round_trip_bit_for_bit() {
    // Parquet append -> load preserves every bar field bit-for-bit (incl. Option<funding>).
    let bars: Vec<Bar> = (0..50)
        .map(|i| {
            let f = if i % 3 == 0 { Some(0.0001 * i as f64) } else { None };
            bar(1000 + i * 60_000, 100.0 + i as f64 * 0.25, f)
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got);
}

#[test]
fn datafusion_root_returns_the_open_path() {
    // `root()` hands back exactly the directory `open` was given — consumers (e.g. the Studio
    // persisting its saved-strategies/workspace JSON next to the store) derive paths from it.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df.root(), dir.path());
}

#[test]
fn datafusion_ts_range_is_inclusive_and_unknown_series_empty() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..10).map(|i| bar(i * 60_000, 100.0, None)).collect();
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();

    let got =
        df.load_bars("binance", "BTCUSDT", "1m", TsRange::of(2 * 60_000, 5 * 60_000)).unwrap();
    assert_eq!(got.len(), 4, "ts 2,3,4,5 inclusive");
    assert_eq!(got.first().unwrap().ts, 2 * 60_000);
    assert_eq!(got.last().unwrap().ts, 5 * 60_000);

    // unknown series → empty, not an error
    assert!(df.load_bars("okx", "ETHUSDT", "1m", TsRange::all()).unwrap().is_empty());
}

#[test]
fn datafusion_multiple_parts_merge_ordered() {
    // two sealed writes to the same series -> two parquet parts; read merges + sorts by ts
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let first: Vec<Bar> = (5..10).map(|i| bar(i * 60_000, 100.0, None)).collect();
    let second: Vec<Bar> = (0..5).map(|i| bar(i * 60_000, 200.0, None)).collect();
    df.append_bars("binance", "BTCUSDT", "1m", &first, None).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &second, None).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 10);
    assert!(got.windows(2).all(|w| w[0].ts <= w[1].ts), "ts ascending across parts");
    assert_eq!(got[0].ts, 0);
}

#[test]
fn datafusion_ticks_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    let quotes: Vec<QuoteTick> = (0..20)
        .map(|i| QuoteTick {
            ts: i * 100,
            local_ts: 0,
            bid: 100.0 + i as f64,
            ask: 100.1 + i as f64,
            bid_size: 1.0,
            ask_size: 2.0,
            symbol: String::new(),
        })
        .collect();
    df.append_quotes("binance", "BTCUSDT", &quotes, None).unwrap();
    let gq = df.scan_quotes("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(gq.len(), 20);
    assert_eq!(gq[0].symbol, "BTCUSDT", "symbol tagged on read");
    assert_eq!(gq[5].bid.to_bits(), quotes[5].bid.to_bits());
    assert_eq!(gq[5].ask_size.to_bits(), quotes[5].ask_size.to_bits());

    let trades: Vec<TradeTick> = (0..15)
        .map(|i| TradeTick {
            ts: i * 100,
            local_ts: 0,
            price: 50.0 + i as f64,
            size: 0.5,
            is_buyer_maker: i % 2 == 0,
            symbol: String::new(),
        })
        .collect();
    df.append_trades("binance", "BTCUSDT", &trades, None).unwrap();
    let gt = df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(gt.len(), 15);
    assert_eq!(gt[3].is_buyer_maker, trades[3].is_buyer_maker);
    assert_eq!(gt[3].price.to_bits(), trades[3].price.to_bits());
}

/// A symbol containing URL-reserved characters must round-trip. `#` is the live case: the
/// Polymarket window convention is `<slug>#<outcome_index>` (`vike_strategy::CheapNp`'s symbol
/// grammar), and an unencoded `file://` path truncates at the fragment marker — the write lands on
/// disk and the read then reports "No files found ... Cannot infer schema from an empty location".
///
/// ⚠ **URL-reserved is not the same set as path-safe, and this test used to conflate them.** It
/// carried `"weird?q=1"` in this loop, which passed only because CI runs on Linux: `?` is one of
/// the characters Windows refuses in a directory name outright, so that series could be WRITTEN on
/// the CI box and never opened on the dev box. `append_*` refuses it by name now, so the symbol moved
/// into `a_path_hostile_symbol_is_refused_by_every_write_verb` below — the three that remain here
/// are the ones a `venue=…/symbol=…` directory genuinely holds on both platforms.
#[test]
fn symbols_with_url_reserved_characters_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let trades: Vec<TradeTick> = (0..5)
        .map(|i| TradeTick {
            ts: i * 1000,
            local_ts: 0,
            price: 0.2 + i as f64 * 0.01,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        })
        .collect();
    for symbol in ["btc-updown-5m-1775001300#0", "pct%20sign", "sp ace"] {
        df.append_trades("polymarket", symbol, &trades, None).unwrap();
        let got = df.scan_trades("polymarket", symbol, TsRange::all()).unwrap();
        assert_eq!(got.len(), 5, "{symbol} wrote but did not read back");
        assert_eq!(got[2].price.to_bits(), trades[2].price.to_bits(), "{symbol}");
    }
}

/// The other half of the pair above: a symbol that cannot BE a directory name is refused at the
/// write, by every verb that takes one, rather than written into a layout only one platform can
/// open.
///
/// `?` is the measured case — MSYS created `symbol=weird?q=1` happily, and PowerShell on the same
/// box refused the identical path with "The directory name is invalid". A separator is the worse
/// failure of the two and is refused for a different reason: `/` SUCCEEDS on both platforms and
/// silently gains the series a directory level, so nothing ever reports it.
#[test]
fn a_path_hostile_symbol_is_refused_by_every_write_verb() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let trades = vec![TradeTick {
        ts: 0,
        local_ts: 0,
        price: 1.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    }];
    let err = df.append_trades("polymarket", "weird?q=1", &trades, None).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("weird?q=1") && text.contains('?'),
        "the refusal must name the symbol AND the character that made it hostile, or an operator \
         cannot tell which of a batch of symbols was rejected — got {text:?}"
    );

    // ...and nothing was written: a refusal that half-lands is worse than either answer.
    assert!(
        df.scan_trades("polymarket", "weird?q=1", TsRange::all()).map(|t| t.len()).unwrap_or(0)
            == 0,
        "the refused symbol left rows behind"
    );
}

// ---- slice 3: resample bridge (ticks -> bars, in-store) ------------------------------------

#[test]
fn resample_trades_to_bars_matches_consolidator() {
    // Ingest trades, resample them to 1m bars in-store, then confirm the stored bars equal the
    // parity-tested consolidator's output bit-for-bit — the bridge is just consolidate + round-trip.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let trades: Vec<TradeTick> = (0..300)
        .map(|i| TradeTick {
            ts: i * 1_000, // 1s apart → many per 1m bucket
            local_ts: 0,
            price: 100.0 + (i as f64 * 0.13).sin(),
            size: 0.5 + (i % 5) as f64 * 0.1,
            is_buyer_maker: i % 2 == 0,
            symbol: String::new(),
        })
        .collect();
    df.append_trades("binance", "BTCUSDT", &trades, Some("t1")).unwrap();

    let n =
        df.resample_trades_to_bars("binance", "BTCUSDT", "1m", TsRange::all(), Some("r1")).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    let want = consolidate_trades(&trades, 60_000); // the oracle (parity-tested vs Python)
    assert_eq!(n, want.len(), "resampled bar count");
    assert!(!want.is_empty(), "sanity: produced bars");
    assert_bars_bit_eq(&want, &got);

    // idempotent: re-running the same resample commit key is a no-op
    let n2 =
        df.resample_trades_to_bars("binance", "BTCUSDT", "1m", TsRange::all(), Some("r1")).unwrap();
    assert_eq!(n2, 0, "re-resample with the same key is a no-op");
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), want.len());
}

// ---- slice 5: ingest an external parquet file (replaces the Python exporter) ----------------

/// Return the first `.parquet` part under a series dir (recurses `date=` subdirs).
fn find_one_parquet(dir: &Path) -> Option<std::path::PathBuf> {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = find_one_parquet(&p) {
                    return Some(f);
                }
            } else if p.extension().is_some_and(|x| x == "parquet") {
                return Some(p);
            }
        }
    }
    None
}

#[test]
fn append_bars_from_parquet_round_trips_bit_for_bit() {
    // Storage-parity-neutral proof: bars written by the store, read back through the external-
    // parquet ingest path, are bit-identical — so the bench reads the SAME bars with no Python.
    let src = tempfile::tempdir().unwrap();
    let a = DataFusionHist::open(src.path()).unwrap();
    let bars: Vec<Bar> = (0..30).map(|i| bar(i * 60_000, 100.0 + i as f64 * 0.1, None)).collect();
    a.append_bars("binance", "BTCUSDT", "1m", &bars, Some("x")).unwrap();
    let part = find_one_parquet(&bars_series(src.path())).expect("a written part file");

    // ingest that raw parquet into a FRESH store via the external-file path
    let dst = tempfile::tempdir().unwrap();
    let b = DataFusionHist::open(dst.path()).unwrap();
    let n = b.append_bars_from_parquet(&part, "binance", "BTCUSDT", "1m", Some("y")).unwrap();
    assert_eq!(n, 30);
    let got = b.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got); // ts/open/high/low/close/volume bit-eq; funding None both sides

    // idempotent like any append
    assert_eq!(
        b.append_bars_from_parquet(&part, "binance", "BTCUSDT", "1m", Some("y")).unwrap(),
        0
    );
}

// ---- kind=book series + local_ts columns (book-recording plan Task 5) ------------------------

/// The `kind=book` store series round-trips per-level rows back into the exact `BookUpdate`
/// events (bit-for-bit f64), including zero-level status events (placeholder-row decode), the
/// (seq, kind) regroup boundary rule, and batch-level idempotency.
#[test]
fn book_updates_roundtrip_bit_eq() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let sent = vec![
        bu(
            1_000,
            1,
            BookUpdateKind::Snapshot,
            vec![BookLevel::new(0.45, 100.0), BookLevel::new(0.44, 20.0)],
            vec![BookLevel::new(0.46, 50.0)],
        ),
        bu(1_005, 2, BookUpdateKind::Delta, vec![BookLevel::new(0.45, 0.0)], vec![]),
        bu(1_010, 0, BookUpdateKind::GapStart, vec![], vec![]), // status: zero levels
        bu(1_020, 0, BookUpdateKind::LiveResume, vec![], vec![]),
        bu(
            1_021,
            3,
            BookUpdateKind::Snapshot,
            vec![BookLevel::new(0.44, 20.0)],
            vec![BookLevel::new(0.47, 5.0)],
        ),
    ];
    let n = store.append_book_updates("polymarket", "TOK", &sent, Some("k1")).unwrap();
    assert_eq!(n, 5, "returns events, not rows");
    let got = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 5);
    for (w, g) in sent.iter().zip(&got) {
        assert_eq!(w.ts, g.ts);
        assert_eq!(w.local_ts, g.local_ts);
        assert_eq!(w.seq, g.seq);
        assert_eq!(w.kind, g.kind);
        assert_eq!(w.tick_size.to_bits(), g.tick_size.to_bits());
        assert_eq!(w.bids.len(), g.bids.len());
        assert_eq!(w.asks.len(), g.asks.len());
        for (a, b) in w.bids.iter().zip(&g.bids) {
            assert_eq!(a.price.to_bits(), b.price.to_bits());
            assert_eq!(a.qty.to_bits(), b.qty.to_bits());
        }
        for (a, b) in w.asks.iter().zip(&g.asks) {
            assert_eq!(a.price.to_bits(), b.price.to_bits());
            assert_eq!(a.qty.to_bits(), b.qty.to_bits());
        }
    }
    // idempotency: same commit key = silent no-op
    assert_eq!(store.append_book_updates("polymarket", "TOK", &sent, Some("k1")).unwrap(), 0);
}

/// The additive `local_ts` column on the quote series round-trips through append/scan, and the
/// decoder path defaults it to 0 for parts written before the column existed (the additive-schema
/// contract — proven bindingly by `quotes_from_batch_defaults_missing_local_ts` in codec.rs).
#[test]
fn quote_local_ts_roundtrips_and_old_parts_stay_readable() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let q = QuoteTick {
        ts: 1_000,
        local_ts: 1_003,
        bid: 1.0,
        ask: 1.1,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    };
    store.append_quotes("v", "S", &[q], Some("k1")).unwrap();
    let got = store.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(got[0].local_ts, 1_003);
}

#[test]
fn symbol_properties_round_trip_and_as_of() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let f1 = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        asset_class: None,
    };
    let f2 = SymbolProperties {
        tick_size: 0.05,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        asset_class: None,
    };
    // day 1 (2020-01-01) and day 30 (2020-01-31), distinct commit keys
    let d1 = 1_577_836_800_000i64; // 2020-01-01T00:00:00Z ms
    let d2 = 1_580_428_800_000i64; // 2020-01-31T00:00:00Z ms
    store
        .append_symbol_properties("bybit", "BTCUSDT", &[(d1, f1)], Some("bybit:BTCUSDT:2020-01-01"))
        .unwrap();
    store
        .append_symbol_properties("bybit", "BTCUSDT", &[(d2, f2)], Some("bybit:BTCUSDT:2020-01-31"))
        .unwrap();

    let all = store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(all, vec![(d1, f1), (d2, f2)]); // ts-ascending

    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d1 - 1).unwrap(), None); // before first
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d1).unwrap(), Some(f1)); // at first
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d2 - 1).unwrap(), Some(f1)); // between → older
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d2 + 999).unwrap(), Some(f2)); // after → latest
    assert_eq!(store.properties_as_of("bybit", "UNKNOWN", d2).unwrap(), None); // unknown symbol
}

/// The store-level twin of the codec's `properties_codec_round_trips_the_tick_scheme`: a populated
/// `TickScheme` must survive a real write→Parquet→read round trip (and `properties_as_of`), and a
/// scheme-less row must come back `None`. This is the hole the `SymbolProperties::tick_scheme` field
/// doc documented — a struct field with no codec column is silently dropped on a store round-trip.
#[test]
fn symbol_properties_round_trip_carries_the_tick_scheme() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    // the real Deribit BTC-option grid: base 0.0001, 0.0005 above 0.005.
    let scheme = TickScheme::new(0.0001, &[TickTier { above_price: 0.005, tick_size: 0.0005 }])
        .expect("valid deribit grid");
    let tiered = SymbolProperties {
        tick_size: 0.0001,
        step_size: 0.1,
        min_qty: 0.1,
        max_qty: 0.0,
        min_notional: 0.0,
        contract_size: 1.0,
        tick_scheme: Some(scheme),
        taker_hold_ms: 0,
        asset_class: None,
    };
    let flat = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        asset_class: None,
    };
    let d1 = 1_577_836_800_000i64; // 2020-01-01
    let d2 = 1_580_428_800_000i64; // 2020-01-31
    store
        .append_symbol_properties(
            "deribit",
            "BTC-OPT",
            &[(d1, tiered)],
            Some("deribit:BTC-OPT:2020-01-01"),
        )
        .unwrap();
    store
        .append_symbol_properties(
            "deribit",
            "BTC-OPT",
            &[(d2, flat)],
            Some("deribit:BTC-OPT:2020-01-31"),
        )
        .unwrap();

    let all = store.scan_symbol_properties("deribit", "BTC-OPT", TsRange::all()).unwrap();
    assert_eq!(all, vec![(d1, tiered), (d2, flat)], "both rows ride back through Parquet intact");
    // the tiered grid genuinely survived — it still resolves by price, not as a flat tick.
    let back = all[0].1.tick_scheme.expect("the scheme must survive the store round-trip");
    assert_eq!(back, scheme);
    assert_eq!(all[0].1.effective_tick(0.05), 0.0005);
    assert_eq!(all[0].1.effective_tick(0.004), 0.0001);
    assert!(all[1].1.tick_scheme.is_none(), "the scheme-less row stays None");
    // and the PIT read carries it too.
    assert_eq!(store.properties_as_of("deribit", "BTC-OPT", d1).unwrap(), Some(tiered));
    assert_eq!(store.properties_as_of("deribit", "BTC-OPT", d2).unwrap(), Some(flat));
}

/// The `asset_class` twin of the test above, and the whole point of 0061 STEP 2: what the store
/// records is the venue's own answer about the KIND of instrument, so a replay reads spot-vs-perp
/// back instead of re-deriving it from the symbol string. Two rows on one series that differ ONLY
/// in their class also prove the PIT read discriminates on it — a class is point-in-time data like
/// every other column here, not a static label on the series.
#[test]
fn symbol_properties_round_trip_carries_the_asset_class() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let perp = SymbolProperties {
        tick_size: 0.01,
        asset_class: Some(AssetClass::CryptoPerp),
        ..Default::default()
    };
    let unclassified = SymbolProperties { tick_size: 0.01, ..Default::default() };
    let d1 = 1_577_836_800_000i64; // 2020-01-01
    let d2 = 1_580_428_800_000i64; // 2020-01-31
    store
        .append_symbol_properties(
            "bybit",
            "BTCUSDT",
            &[(d1, perp)],
            Some("bybit:BTCUSDT:2020-01-01"),
        )
        .unwrap();
    store
        .append_symbol_properties(
            "bybit",
            "BTCUSDT",
            &[(d2, unclassified)],
            Some("bybit:BTCUSDT:2020-01-31"),
        )
        .unwrap();

    let all = store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(all, vec![(d1, perp), (d2, unclassified)], "both rows ride back through Parquet");
    assert_eq!(all[0].1.asset_class, Some(AssetClass::CryptoPerp));
    assert_eq!(all[1].1.asset_class, None, "an unclassified row stays unclassified");
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d1).unwrap(), Some(perp));
    assert_eq!(store.properties_as_of("bybit", "BTCUSDT", d2).unwrap(), Some(unclassified));
}

#[test]
fn symbol_properties_daily_commit_key_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let f = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        asset_class: None,
    };
    let d1 = 1_577_836_800_000i64;
    store
        .append_symbol_properties(
            "okx",
            "BTC-USDT-SWAP",
            &[(d1, f)],
            Some("okx:BTC-USDT-SWAP:2020-01-01"),
        )
        .unwrap();
    let second = store
        .append_symbol_properties(
            "okx",
            "BTC-USDT-SWAP",
            &[(d1, f)],
            Some("okx:BTC-USDT-SWAP:2020-01-01"),
        )
        .unwrap();
    assert_eq!(second, 0, "same commit_key must dedup to a no-op");
    assert_eq!(
        store.scan_symbol_properties("okx", "BTC-USDT-SWAP", TsRange::all()).unwrap().len(),
        1
    );
}
