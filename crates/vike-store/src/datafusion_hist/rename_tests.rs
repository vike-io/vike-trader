//! The rename's contract on a real store: a move keeps every row, part and key and renames the
//! row's symbol; a merge drops the batches the target already holds; a re-run after a crash or
//! after success finishes the job and adds nothing.

use vike_data::store::hist::{HistoryStore, TimestampRange};
use vike_data::{ExecFillRow, ExecOrderRow};
use vike_marketdata::{Bar, TickQuote, TickTrade};

use super::super::codec::{
    BarCodec, BookCodec, ChainCodec, CohortCodec, EquityCodec, ExecFillCodec, ExecOrderCodec,
    FinancialFactCodec, ForecastCodec, FundingCodec, InstitutionalHoldingCodec, OptionTradeCodec,
    PerpMetricsCodec, PropertiesCodec, QuoteCodec, SeriesCodec, TextCodec, TextScoreCodec,
    TradeCodec,
};
use super::super::manifest::rebuild_manifest;
use super::*;

const DAY: i64 = 86_400_000;
const T0: i64 = 1_700_000_000_000;

fn store() -> (tempfile::TempDir, HistoryStoreDataFusion) {
    let dir = tempfile::tempdir().expect("a scratch store");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open");
    (dir, store)
}

fn quote(ts: i64, symbol: &str) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid: ts as f64,
        ask: ts as f64 + 1.0,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: symbol.into(),
    }
}

fn trade(ts: i64, symbol: &str) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price: ts as f64,
        size: 1.0,
        is_buyer_maker: false,
        symbol: symbol.into(),
    }
}

fn fill(ts: i64, symbol: &str) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: format!("t{ts}"),
        order_client_id: "c1".into(),
        venue: "v".into(),
        symbol: symbol.into(),
        mount_id: None,
        side: 1,
        qty: 0.5,
        px: 100.0,
        commission: 0.1,
        mark_price: None,
        liquidity_side: "maker".into(),
        commission_asset: "USD".into(),
        venue_order_id: None,
        account: None,
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    }
}

fn bar(ts: i64) -> Bar {
    Bar {
        timestamp: ts,
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

/// A series' manifest with the part NAMES left out — what a move must carry across unchanged.
fn parts_of(
    store: &HistoryStoreDataFusion,
    id: &SeriesId,
) -> Vec<(String, i64, i64, usize, Vec<String>)> {
    let m = read_manifest(&store.series_dir_of(id)).expect("a manifest");
    let mut parts: Vec<_> = m
        .files
        .iter()
        .map(|f| (f.date.clone(), f.ts_min, f.ts_max, f.rows, f.commit_keys.clone()))
        .collect();
    parts.sort();
    parts
}

fn renamed(id: &SeriesId, to: &str) -> SeriesId {
    SeriesId { symbol: to.to_string(), ..id.clone() }
}

/// ⚠ **The headline.** Every kind the migration moves, in one store: each target holds exactly the
/// source's parts (date, ts bounds, rows) and commit keys — rewritten to the new name where they
/// spell the old one, as is where they do not, in the manifest AND in each part's footer — the
/// key-only marker included; each row that carries its own symbol reads the NEW one; the old leaves
/// are gone; and a re-run is a no-op.
#[test]
fn a_move_keeps_every_row_part_and_key_and_renames_the_row_symbol() {
    let (_dir, s) = store();
    let q1 = format!("v:OLD:{T0}-{}", T0 + 1);
    s.append_quotes("v", "OLD", &[quote(T0, "OLD"), quote(T0 + 1, "OLD")], Some(&q1)).unwrap();
    // An EMPTY row symbol is the per-symbol path's legal shape; it must read NEW too.
    s.append_quotes("v", "OLD", &[quote(T0 + DAY, "")], Some("live-v-OLD-quote-1-2-3")).unwrap();
    // A key that does not name the instrument is kept as it was.
    s.append_trades("v", "OLD", &[trade(T0, "OLD")], Some("t1")).unwrap();
    let f1 = "mat-fill-v-OLD-1-2";
    s.append_exec_fills("v", "OLD", &[fill(T0, "OLD"), fill(T0 + DAY, "OLD")], Some(f1)).unwrap();
    s.append_bars("v", "OLD", "1m", &[bar(T0), bar(T0 + 60_000)], Some("v:OLD:1m:1-2")).unwrap();
    s.append_bars("v", "OLD", "1h", &[bar(T0)], Some("v:OLD:1h:1-2")).unwrap();
    let quotes = SeriesId::per_symbol(StoreKindId::Quote, "v", "OLD", None);
    s.spend_keys_without_rows(&quotes, &["v:OLD:3-4:empty"]).unwrap();
    // A neighbour that must not move.
    s.append_quotes("v", "OTHER", &[quote(T0, "OTHER")], Some("o1")).unwrap();

    let plan = s.plan_rename("v", "OLD", "NEW", None, None, None).unwrap();
    assert_eq!(plan.len(), 5, "quote, trade, exec_fill and two bar intervals: {plan:?}");
    assert!(plan.iter().all(|p| !p.merge), "nothing is filed under NEW yet: {plan:?}");
    let renamed_keys = |parts: Vec<(String, i64, i64, usize, Vec<String>)>| {
        let mut parts: Vec<_> = parts
            .into_iter()
            .map(|(d, lo, hi, n, keys)| {
                let keys: Vec<String> =
                    keys.iter().map(|k| rename_in_commit_key(k, "OLD", "NEW")).collect();
                (d, lo, hi, n, keys)
            })
            .collect();
        parts.sort();
        parts
    };
    let before: Vec<_> = plan.iter().map(|p| renamed_keys(parts_of(&s, &p.source))).collect();
    let rows_before: u64 = plan.iter().map(|p| p.rows).sum();
    assert_eq!(rows_before, 3 + 1 + 2 + 2 + 1);

    for p in &plan {
        s.rename_series(&p.source, "NEW").unwrap();
    }
    for (p, parts) in plan.iter().zip(&before) {
        assert_eq!(&parts_of(&s, &p.target), parts, "{}: every part and key", p.line());
        let leaf = s.series_dir_of(&p.source);
        assert!(!leaf.exists(), "{}: the old leaf is gone", p.line());
        if p.source.interval.is_some() {
            assert!(!leaf.parent().unwrap().exists(), "{}: and its emptied `symbol=`", p.line());
        }
        // The footers say what the manifest says: a rebuild from the parts finds the same keys.
        let (rebuilt, _) = rebuild_manifest(&s.series_dir_of(&p.target)).unwrap();
        let mut footers: Vec<_> = rebuilt.files.iter().map(|f| f.commit_keys.clone()).collect();
        footers.sort();
        let mut listed: Vec<_> = parts.iter().map(|part| part.4.clone()).collect();
        listed.sort();
        assert_eq!(footers, listed, "{}: the footers carry the rewritten keys", p.line());
    }
    let mut keys = s.series_commits(&renamed(&quotes, "NEW")).unwrap();
    keys.sort();
    assert_eq!(
        keys,
        [
            "live-v-NEW-quote-1-2-3".to_string(),
            format!("v:NEW:{T0}-{}", T0 + 1),
            "v:NEW:3-4:empty".into()
        ]
    );
    let trades = SeriesId::per_symbol(StoreKindId::Trade, "v", "NEW", None);
    assert_eq!(s.series_commits(&trades).unwrap(), ["t1"], "a key without the name is kept");

    let got = s.scan_quotes("v", "NEW", TimestampRange::all()).unwrap();
    assert_eq!(got.iter().map(|q| q.timestamp).collect::<Vec<_>>(), [T0, T0 + 1, T0 + DAY]);
    assert!(got.iter().all(|q| q.symbol == "NEW"), "{got:?}");
    assert!(got.iter().all(|q| q.bid == q.timestamp as f64), "the other columns are untouched");
    assert!(s.scan_quotes("v", "OLD", TimestampRange::all()).unwrap().is_empty());
    let trades = s.scan_trades("v", "NEW", TimestampRange::all()).unwrap();
    assert!(trades.len() == 1 && trades[0].symbol == "NEW", "{trades:?}");
    let fills = s.scan_exec_fills("v", "NEW").unwrap();
    assert_eq!(fills.len(), 2);
    assert!(fills.iter().all(|f| f.symbol == "NEW" && f.venue == "v"), "{fills:?}");
    assert_eq!(
        s.load_bars("v", "NEW", "1m", TimestampRange::all()).unwrap(),
        [bar(T0), bar(T0 + 60_000)]
    );
    assert_eq!(s.scan_quotes("v", "OTHER", TimestampRange::all()).unwrap().len(), 1);

    assert!(s.plan_rename("v", "OLD", "NEW", None, None, None).unwrap().is_empty());
    assert_eq!(s.rename_series(&plan[0].source, "NEW").unwrap(), RenameOutcome::default());
}

/// A MERGE: the target already holds one of the source's batches — the key it was minted under for
/// the NEW name is the old key rewritten — so that part is dropped from the source with no second
/// copy, and the rest join it.
#[test]
fn a_merge_drops_the_batches_the_target_already_holds() {
    let (_dir, s) = store();
    s.append_quotes("v", "NEW", &[quote(T0, "NEW")], Some("v:NEW:1-2")).unwrap();
    s.append_quotes("v", "OLD", &[quote(T0, "OLD")], Some("v:OLD:1-2")).unwrap();
    s.append_quotes("v", "OLD", &[quote(T0 + DAY, "OLD")], Some("v:OLD:3-4")).unwrap();

    let plan = s.plan_rename("v", "OLD", "NEW", Some(StoreKindId::Quote), None, None).unwrap();
    assert_eq!(plan.len(), 1);
    assert!(plan[0].merge, "{}", plan[0].line());
    assert!(plan[0].line().starts_with("MERGE quote v OLD -> NEW"), "{}", plan[0].line());
    let outcome = s.rename_series(&plan[0].source, "NEW").unwrap();
    assert_eq!(
        outcome,
        RenameOutcome { parts_moved: 1, rows_moved: 1, parts_already_in_target: 1 }
    );
    let got = s.scan_quotes("v", "NEW", TimestampRange::all()).unwrap();
    assert_eq!(
        got.iter().map(|q| q.timestamp).collect::<Vec<_>>(),
        [T0, T0 + DAY],
        "no second copy"
    );
    let mut keys = s.series_commits(&plan[0].target).unwrap();
    keys.sort();
    assert_eq!(keys, ["v:NEW:1-2", "v:NEW:3-4"]);
}

/// The key rule: the old name is replaced where it is a WHOLE field — bounded by the key's start or
/// end, `:` or `-` — and nowhere else.
#[test]
fn a_key_renames_where_the_name_is_a_whole_field() {
    for (key, from, to, want) in [
        (
            "dukascopy-archive:AAPLUSUSD:1-2",
            "AAPLUSUSD",
            "AAPL.CFD",
            "dukascopy-archive:AAPL.CFD:1-2",
        ),
        ("oanda:EUR_USD:5s:1-2", "EUR_USD", "EURUSD", "oanda:EURUSD:5s:1-2"),
        ("mat-fill-oanda-EUR_USD-1-2", "EUR_USD", "EURUSD", "mat-fill-oanda-EURUSD-1-2"),
        // The execution stream's three key shapes: the journal id is a field of its own, so only the instrument
        // field is rewritten, never a journal id or a sequence.
        (
            "stream-fill-0123456789abcdef0123456789abcdef-oanda-EUR_USD-1-2",
            "EUR_USD",
            "EURUSD",
            "stream-fill-0123456789abcdef0123456789abcdef-oanda-EURUSD-1-2",
        ),
        (
            "stream-order-0123456789abcdef0123456789abcdef-hyperliquid-PURR/USDC-1-2",
            "PURR/USDC",
            "PURRUSDC",
            "stream-order-0123456789abcdef0123456789abcdef-hyperliquid-PURRUSDC-1-2",
        ),
        (
            "stream-forecast-0123456789abcdef0123456789abcdef-binance-BTC-7-9",
            "BTC",
            "BTCUSDC.P",
            "stream-forecast-0123456789abcdef0123456789abcdef-binance-BTCUSDC.P-7-9",
        ),
        ("EUR_USD", "EUR_USD", "EURUSD", "EURUSD"),
        ("oanda:EUR_USDX:5s:1-2", "EUR_USD", "EURUSD", "oanda:EUR_USDX:5s:1-2"),
        ("oanda:XEUR_USD:5s:1-2", "EUR_USD", "EURUSD", "oanda:XEUR_USD:5s:1-2"),
        (
            "live-binance-USDT-USD-bar-1-2-3",
            "USDT-USD",
            "USDTUSD",
            "live-binance-USDTUSD-bar-1-2-3",
        ),
        (
            "mat-order-hyperliquid-PURR/USDC-1-2",
            "PURR/USDC",
            "PURRUSDC",
            "mat-order-hyperliquid-PURRUSDC-1-2",
        ),
        ("t1", "EUR_USD", "EURUSD", "t1"),
    ] {
        assert_eq!(rename_in_commit_key(key, from, to), want, "{key}");
    }
}

/// Every commit-key template the store declares (`STORE_KINDS`, each held verbatim to its producer
/// by the store-kind gate) puts its instrument in a whole field, so the old name's key, rewritten,
/// is EXACTLY the key the producer mints for the new name — what lets a later backfill, import or
/// recording of a renamed instrument find the windows it already holds.
#[test]
fn every_declared_key_template_renames_to_the_key_minted_for_the_new_name() {
    // The template with its instrument placeholder set to `name` and every other one to `7`.
    fn mint(template: &str, name: &str) -> String {
        let mut out = String::new();
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let close = open + rest[open..].find('}').expect("a closed placeholder");
            let field = &rest[open + 1..close];
            out.push_str(if matches!(field, "symbol" | "asset" | "underlying") {
                name
            } else {
                "7"
            });
            rest = &rest[close + 1..];
        }
        out.push_str(rest);
        out
    }
    let mut checked = 0;
    for kind in vike_data::store::store_kind::STORE_KINDS {
        for key in kind.commit_keys {
            for (from, to) in [
                ("EUR_USD", "EURUSD"),
                ("AAPLUSUSD", "AAPL.CFD"),
                ("USDT-USD", "USDTUSD"),
                ("BTC-PERPETUAL", "BTCUSD.P"),
                ("BTC", "BTCUSDC.P"),
                ("PURR/USDC", "PURRUSDC"),
            ] {
                let old = mint(key.template, from);
                assert_eq!(
                    rename_in_commit_key(&old, from, to),
                    mint(key.template, to),
                    "{} ({})",
                    key.template,
                    key.producer
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "the table was read: {checked}");
}

/// ⚠ **A nested old name** — hyperliquid's spot `PURR/USDC`, filed INSIDE `symbol=PURR` by an older
/// build and reached by no listing — moves when spelled exactly, and the series filed at
/// `symbol=PURR` is untouched; renaming THAT series, in turn, leaves a nested leaf inside it alone.
#[test]
fn a_nested_old_name_moves_and_the_series_around_it_stay() {
    let (_dir, s) = store();
    let order = |ts: i64, symbol: &str| ExecOrderRow {
        ts,
        order_client_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: symbol.into(),
        mount_id: None,
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        status: "filled".into(),
        price: Some(1.0),
        trigger_price: None,
        venue_order_id: None,
        filled_qty: 1.0,
        avg_fill_px: 1.0,
        account: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    };
    let perp_key = "mat-order-hyperliquid-PURR-1-2";
    s.append_exec_orders("hyperliquid", "PURR", &[order(T0, "PURR")], Some(perp_key)).unwrap();
    let nested = SeriesId::per_symbol(StoreKindId::ExecOrder, "hyperliquid", "PURR/USDC", None);
    // An older build's nested leaf: written under a plain name, then moved where it sits on the CI box.
    let plant_nested = || {
        let key = "mat-order-hyperliquid-PURR/USDC-1-2";
        s.append_exec_orders("hyperliquid", "SPOT", &[order(T0 + 1, "PURR/USDC")], Some(key))
            .unwrap();
        let spot = SeriesId::per_symbol(StoreKindId::ExecOrder, "hyperliquid", "SPOT", None);
        std::fs::rename(s.series_dir_of(&spot), s.series_dir_of(&nested)).unwrap();
    };
    plant_nested();
    assert!(s.list_series().unwrap().iter().all(|id| id.symbol != "PURR/USDC"), "unlisted");

    let plan = s.plan_rename("hyperliquid", "PURR/USDC", "PURRUSDC", None, None, None).unwrap();
    assert_eq!(plan.len(), 1, "{plan:?}");
    assert_eq!(plan[0].source.kind, StoreKindId::ExecOrder);
    s.rename_series(&plan[0].source, "PURRUSDC").unwrap();
    let spot = s.scan_exec_orders("hyperliquid", "PURRUSDC").unwrap();
    assert!(spot.len() == 1 && spot[0].symbol == "PURRUSDC", "{spot:?}");
    assert_eq!(
        s.series_commits(&renamed(&nested, "PURRUSDC")).unwrap(),
        ["mat-order-hyperliquid-PURRUSDC-1-2"]
    );
    assert!(!s.series_dir_of(&nested).exists());
    let perp = s.scan_exec_orders("hyperliquid", "PURR").unwrap();
    assert!(perp.len() == 1 && perp[0].symbol == "PURR", "the series beside it stays: {perp:?}");
    let perp_id = SeriesId::per_symbol(StoreKindId::ExecOrder, "hyperliquid", "PURR", None);
    assert_eq!(s.series_commits(&perp_id).unwrap(), [perp_key]);

    // The other way round: the series at `symbol=PURR` renames, the nested leaf inside it stays.
    plant_nested();
    s.rename_series(&perp_id, "PURRUSDC.P").unwrap();
    assert_eq!(s.scan_exec_orders("hyperliquid", "PURRUSDC.P").unwrap().len(), 1);
    assert_eq!(s.scan_exec_orders("hyperliquid", "PURR/USDC").unwrap().len(), 1, "it stays");
    assert!(s.plan_rename("hyperliquid", "PURR", "X", None, None, None).unwrap().is_empty());
}

/// ⚠ **A crash between the target's publish and the source's** — the one window where a part is
/// in both series — plus a stale unpublished file for the next part: the re-run copies nothing
/// twice, a KEYLESS part included (its moved name is what finds it), and finishes the job.
#[test]
fn a_rerun_after_a_crash_mid_way_finishes_without_a_second_copy() {
    let (_dir, s) = store();
    s.append_quotes("v", "OLD", &[quote(T0, "OLD")], None).unwrap();
    s.append_quotes("v", "OLD", &[quote(T0 + 1, "OLD")], Some("k2")).unwrap();
    s.append_quotes("v", "OLD", &[quote(T0 + 2, "OLD")], Some("k3")).unwrap();
    let source = SeriesId::per_symbol(StoreKindId::Quote, "v", "OLD", None);
    let target = renamed(&source, "NEW");
    let parts = read_manifest(&s.series_dir_of(&source)).unwrap().files;
    assert_eq!(parts.len(), 3);

    // The crashed run: the first two parts reached the target and the source still lists them; the
    // third left a torn unpublished file behind.
    let moving = Move {
        store: &s,
        source: &source,
        from_dir: s.series_dir_of(&source),
        to_dir: s.series_dir_of(&target),
        column: symbol_column(StoreKindId::Quote),
        to: "NEW",
    };
    let mut seen = read_manifest(&moving.to_dir).unwrap();
    assert!(moving.part(&parts[0], &mut seen).unwrap().is_some());
    assert!(moving.part(&parts[1], &mut seen).unwrap().is_some());
    let date_dir = part_dir(&moving.to_dir, &parts[2].date);
    std::fs::create_dir_all(&date_dir).unwrap();
    std::fs::write(date_dir.join(merge_tmp_name(&moved_part_name(&source, &parts[2]))), b"torn")
        .unwrap();

    let outcome = s.rename_series(&source, "NEW").unwrap();
    assert_eq!(
        outcome,
        RenameOutcome { parts_moved: 1, rows_moved: 1, parts_already_in_target: 2 }
    );
    let got = s.scan_quotes("v", "NEW", TimestampRange::all()).unwrap();
    assert_eq!(got.iter().map(|q| q.timestamp).collect::<Vec<_>>(), [T0, T0 + 1, T0 + 2]);
    assert!(got.iter().all(|q| q.symbol == "NEW"));
    assert!(s.plan_rename("v", "OLD", "NEW", None, None, None).unwrap().is_empty());
}

/// A crash after the source was emptied and folded, before its leaf went: a re-run removes it.
#[test]
fn a_rerun_after_a_crash_in_the_removal_removes_the_emptied_source() {
    let (_dir, s) = store();
    s.append_trades("v", "OLD", &[trade(T0, "OLD")], Some("t1")).unwrap();
    let source = SeriesId::per_symbol(StoreKindId::Trade, "v", "OLD", None);
    s.rename_series(&source, "NEW").unwrap();
    // Re-create what the crash left: an empty, folded source still listed.
    let dir = s.series_dir_of(&source);
    std::fs::create_dir_all(&dir).unwrap();
    fold_base(&dir, &Manifest::empty()).unwrap();
    let plan = s.plan_rename("v", "OLD", "NEW", None, None, None).unwrap();
    assert_eq!(plan.len(), 1, "the emptied source is still listed");
    assert_eq!(plan[0].rows, 0);
    s.rename_series(&source, "NEW").unwrap();
    assert!(!dir.exists());
    assert_eq!(s.scan_trades("v", "NEW", TimestampRange::all()).unwrap().len(), 1);
}

/// What the target holds of a part, by its keys on its date: nothing, all, or some (which). A part
/// holding some is refused for every kind no row split is defined on, in words that say why.
#[test]
fn a_part_sharing_only_some_keys_with_the_target_is_split_or_refused() {
    let mut m = Manifest::empty();
    let entry = |name: &str, keys: &[&str]| FileEntry {
        name: name.to_string(),
        date: "2023-11-14".to_string(),
        ts_min: T0,
        ts_max: T0,
        rows: 1,
        commit_keys: keys.iter().map(|k| k.to_string()).collect(),
    };
    m.files.push(entry("part-00001.parquet", &["a"]));
    assert_eq!(target_holds(&m, &entry("x", &["b"])), TargetHolds::Nothing, "no key held: copy it");
    assert_eq!(target_holds(&m, &entry("x", &[])), TargetHolds::Nothing, "keyless: copy it");
    assert_eq!(target_holds(&m, &entry("x", &["a"])), TargetHolds::All, "every key held: drop it");
    // The same key on ANOTHER date is the second day of a batch that straddled midnight: copy it.
    let next_day = FileEntry { date: "2023-11-15".to_string(), ..entry("x", &["a"]) };
    assert_eq!(target_holds(&m, &next_day), TargetHolds::Nothing, "another date: copy it");
    assert_eq!(target_holds(&m, &entry("part-00001.parquet", &["b"])), TargetHolds::All, "listed");
    let some = target_holds(&m, &entry("x", &["a", "b"]));
    assert_eq!(some, TargetHolds::Some(vec!["a".to_string()]));
    assert!(HeldRows::splits(StoreKindId::ExecFill) && HeldRows::splits(StoreKindId::ExecOrder));
    assert!(!HeldRows::splits(StoreKindId::Quote) && !HeldRows::splits(StoreKindId::Bar));
    let err = partly_held(&entry("x", &["a", "b"]), 1).to_string();
    assert!(err.contains("1 of them") && err.contains("nothing lost"), "{err}");

    // A quote part sharing one of two keys is refused, and nothing moves.
    let (_dir, s) = store();
    s.append_quotes("v", "NEW", &[quote(T0, "NEW")], Some("v:NEW:1-2")).unwrap();
    s.append_quotes("v", "OLD", &[quote(T0, "OLD")], Some("v:OLD:1-2")).unwrap();
    s.append_quotes("v", "OLD", &[quote(T0 + 1, "OLD")], Some("v:OLD:3-4")).unwrap();
    let compact = crate::CompactionConfig { min_parts: 2, ..Default::default() };
    s.compact_series(StoreKindId::Quote, "v", "OLD", None, &compact).unwrap();
    let source = SeriesId::per_symbol(StoreKindId::Quote, "v", "OLD", None);
    let err = s.rename_series(&source, "NEW").unwrap_err().to_string();
    assert!(err.contains("already holds 1 of them"), "{err}");
    assert_eq!(s.scan_quotes("v", "OLD", TimestampRange::all()).unwrap().len(), 2, "nothing lost");
}

/// **the CI box's oanda `EUR_USD` fills of 2026-10-26** (the deploy-2 rename, 2026-11-17): an older build
/// materialized one journal window under both spellings of the instrument, so after the rename's key
/// rewrite a compacted part of the old name carries four keys, two of them the target's, and five
/// rows, two of them rows the target holds. The split keeps the target's copy of those two batches
/// as it is (its rows `t81` twice and `t99` included), moves the other three rows under the part's
/// other two keys with the new symbol, and loses nothing; a re-run does nothing.
#[test]
fn a_fill_part_sharing_some_batches_with_the_target_moves_only_the_rows_it_does_not_hold() {
    let (_dir, s) = store();
    let fills =
        |symbol: &str, ids: &[i64]| ids.iter().map(|i| fill(T0 + i, symbol)).collect::<Vec<_>>();
    s.append_exec_fills(
        "v",
        "NEW",
        &fills("NEW", &[79, 81, 81]),
        Some("mat-fill-v-NEW-398408-398435"),
    )
    .unwrap();
    s.append_exec_fills("v", "NEW", &fills("NEW", &[94, 96]), Some("mat-fill-v-NEW-413139-413151"))
        .unwrap();
    s.append_exec_fills(
        "v",
        "NEW",
        &fills("NEW", &[99, 102]),
        Some("mat-fill-v-NEW-413198-413210"),
    )
    .unwrap();
    s.append_exec_fills("v", "OLD", &fills("OLD", &[79]), Some("mat-fill-v-OLD-398408-398435"))
        .unwrap();
    s.append_exec_fills("v", "OLD", &fills("OLD", &[85]), Some("mat-fill-v-OLD-398588-398594"))
        .unwrap();
    s.append_exec_fills("v", "OLD", &fills("OLD", &[102]), Some("mat-fill-v-OLD-413198-413210"))
        .unwrap();
    s.append_exec_fills(
        "v",
        "OLD",
        &fills("OLD", &[106, 108]),
        Some("mat-fill-v-OLD-414234-414238"),
    )
    .unwrap();
    let compact = crate::CompactionConfig { min_parts: 2, ..Default::default() };
    s.compact_series(StoreKindId::ExecFill, "v", "OLD", None, &compact).unwrap();
    let source = SeriesId::per_symbol(StoreKindId::ExecFill, "v", "OLD", None);
    let target = renamed(&source, "NEW");
    let parts = read_manifest(&s.series_dir_of(&source)).unwrap().files;
    assert_eq!(
        (parts.len(), parts[0].commit_keys.len(), parts[0].rows),
        (1, 4, 5),
        "one compacted part"
    );

    let outcome = s.rename_series(&source, "NEW").unwrap();
    assert_eq!(
        outcome,
        RenameOutcome { parts_moved: 1, rows_moved: 3, parts_already_in_target: 0 }
    );
    let mut got: Vec<(i64, String)> =
        s.scan_exec_fills("v", "NEW").unwrap().into_iter().map(|f| (f.ts - T0, f.symbol)).collect();
    got.sort();
    let want: Vec<(i64, String)> = [79, 81, 81, 85, 94, 96, 99, 102, 106, 108]
        .iter()
        .map(|i| (*i, "NEW".to_string()))
        .collect();
    assert_eq!(got, want, "each held row once, as the target held it, and every other row moved");
    let mut keys = s.series_commits(&target).unwrap();
    keys.sort();
    assert_eq!(
        keys,
        [
            "mat-fill-v-NEW-398408-398435",
            "mat-fill-v-NEW-398588-398594",
            "mat-fill-v-NEW-413139-413151",
            "mat-fill-v-NEW-413198-413210",
            "mat-fill-v-NEW-414234-414238",
        ]
    );
    let moved = read_manifest(&s.series_dir_of(&target)).unwrap();
    let split =
        moved.files.iter().find(|f| f.name.starts_with("part-moved-")).expect("the moved part");
    assert_eq!(
        (split.rows, split.ts_min, split.ts_max),
        (3, T0 + 85, T0 + 108),
        "its own rows and bounds"
    );
    assert!(
        s.plan_rename("v", "OLD", "NEW", None, None, None).unwrap().is_empty(),
        "the old name is gone"
    );
    assert_eq!(
        s.rename_series(&source, "NEW").unwrap(),
        RenameOutcome::default(),
        "a re-run does nothing"
    );
}

/// The refusals: a grouped series by name, a name the store cannot file (new or old), one name twice.
#[test]
fn a_grouped_series_a_hostile_name_and_one_name_twice_are_refused() {
    let (_dir, s) = store();
    s.append_quotes_grouped("v", "OLD", &[quote(T0, "A"), quote(T0, "B")], Some("g1")).unwrap();
    let err = s.plan_rename("v", "OLD", "NEW", None, None, None).unwrap_err().to_string();
    assert!(err.contains("GROUPED") && err.contains("group=OLD"), "{err}");
    let err = s.plan_rename("v", "X", "NEW/1", None, None, None).unwrap_err().to_string();
    assert!(err.contains("cannot be a store key"), "{err}");
    // One `/` between two names is a nested leaf (none here, so nothing to rename); any other is refused.
    assert!(s.plan_rename("v", "PURR/USDC", "PURRUSDC", None, None, None).unwrap().is_empty());
    for old in ["a/b/c", "/x", "x/", "../x", "x/..", "a\\b"] {
        let err = s.plan_rename("v", old, "NEW", None, None, None).unwrap_err().to_string();
        assert!(err.contains("ONE `/`") || err.contains("cannot be a store key"), "{old}: {err}");
    }
    let err = s.plan_rename("v", "X", "X", None, None, None).unwrap_err().to_string();
    assert!(err.contains("already the name"), "{err}");
    let grouped = SeriesId::grouped(StoreKindId::Quote, "v", "OLD");
    assert!(s.rename_series(&grouped, "NEW").unwrap_err().to_string().contains("GROUP"));
}

/// Each column [`symbol_column`] names is a string column its kind's codec writes, and a kind with
/// none writes no column by any of those names.
#[test]
fn every_named_symbol_column_is_one_its_codec_writes() {
    for kind in StoreKindId::ALL {
        let schema = match kind {
            StoreKindId::Bar => BarCodec::schema(),
            StoreKindId::Book | StoreKindId::Depth => BookCodec::schema(),
            StoreKindId::Chain => ChainCodec::schema(),
            StoreKindId::Cohort => CohortCodec::schema(),
            StoreKindId::Equity => EquityCodec::schema(),
            StoreKindId::ExecFill => ExecFillCodec::schema(),
            StoreKindId::ExecForecast => ForecastCodec::schema(),
            StoreKindId::ExecFunding => FundingCodec::schema(),
            StoreKindId::ExecOrder => ExecOrderCodec::schema(),
            StoreKindId::FinancialFact => FinancialFactCodec::schema(),
            StoreKindId::InstitutionalHolding => InstitutionalHoldingCodec::schema(),
            StoreKindId::OptionTrade => OptionTradeCodec::schema(),
            StoreKindId::PerpMetrics => PerpMetricsCodec::schema(),
            StoreKindId::Properties => PropertiesCodec::schema(),
            StoreKindId::Quote => QuoteCodec::schema(),
            StoreKindId::Text => TextCodec::schema(),
            StoreKindId::TextScore => TextScoreCodec::schema(),
            StoreKindId::Trade => TradeCodec::schema(),
        };
        match symbol_column(kind) {
            Some(column) => {
                let field =
                    schema.field_with_name(column).unwrap_or_else(|e| panic!("{kind}: {e}"));
                assert_eq!(field.data_type(), &datafusion::arrow::datatypes::DataType::Utf8);
            }
            None => {
                // An option-trade log's `underlying` is each trade's own field (a currency's log holds several).
                let own = |c: &str| kind == StoreKindId::OptionTrade && c == "underlying";
                for column in
                    ["symbol", "symbol_col", "underlying", "asset"].into_iter().filter(|c| !own(c))
                {
                    assert!(schema.field_with_name(column).is_err(), "{kind} writes {column}");
                }
            }
        }
    }
}
