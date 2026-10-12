//! The two ignored measurements over real archive files: derived-vs-recorded L1, live smoke.

use super::*;

// ---- derived-L1 vs recorded-l1_quotes divergence report (never run in CI) --------------------
//
// The backtest over 580 BTC-5m markets agreed to within ~0.26pp between this store's DERIVED L1
// and a `DataFusionHist` whose quote lane came from the recorder's own `l1_quotes` (via
// ClickHouse). Close — but not zero, and "close" is not an explanation. This walks the SAME
// universe market by market and localises the residual: which markets diverge, by how much, and
// then event by event inside the worst one.
//
// The two sources are deliberately different, so SOME divergence is expected; the question is
// only whether it is the expected kind:
//   - derived L1 = fold this file's own book depth, emit on top-of-book change
//   - recorded l1_quotes = the live recorder's own L1 capture, its own timestamps, its own dedup
//
// Self-skips when any input is absent — same idiom as the live measure below. Run manually:
// `cargo test -p vike-backfill --features vike-archive --lib archive_store::tests::derived_l1_divergence_report -- --ignored --nocapture`
#[test]
#[ignore]
fn derived_l1_divergence_report() {
    const ARCHIVE: &str = "/var/lib/vike/dl/btc5m_2026-07-26.parquet";
    const STORE: &str = "/var/lib/vike/btc_store";
    const UNIVERSE: &str = "/tmp/uni_final.tsv";
    const TENOR_MS: i64 = 300_000;
    // How many markets to walk. ONE by default: the point is to LOCALISE a divergence, and one
    // market answers "is there one, and what shape is it" in ~14s where the full 580 costs ~20
    // minutes. Widen by writing a count into `<UNIVERSE>.markets` — libtest REJECTS unknown CLI
    // flags ("Unrecognized option"), and an env var would need a settings-registry row for what
    // is a test-only knob.
    let max_markets: usize = std::fs::read_to_string(format!("{UNIVERSE}.markets"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1);
    for p in [ARCHIVE, STORE, UNIVERSE] {
        if !Path::new(p).exists() {
            eprintln!("{p} not present — skipping derived-L1 divergence report");
            return;
        }
    }
    let universe = std::fs::read_to_string(UNIVERSE).unwrap();
    let markets: Vec<(String, i64)> = universe
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let _family = f.next()?;
            let token = f.next()?.to_string();
            let end: i64 = f.next()?.trim().parse().ok()?;
            Some((token, end))
        })
        .collect();
    assert!(!markets.is_empty(), "universe parsed empty");

    let archive = ArchiveParquetHistStore::from_files([PathBuf::from(ARCHIVE)]);
    let store = crate::DataFusionHist::open(STORE).expect("open reference store");

    // (token, derived_count, recorded_count, market_end, first disagreeing ts)
    let mut rows: Vec<(String, usize, usize, i64, Option<i64>)> = Vec::new();
    for (token, end) in markets.iter() {
        if rows.len() >= max_markets {
            break;
        }
        let range = TsRange::of(end - TENOR_MS, *end);
        let d = archive.scan_quotes(VENUE, token, range).unwrap();
        let r = store.scan_quotes(VENUE, token, range).unwrap();
        if d.is_empty() && r.is_empty() {
            continue; // market outside this file's day — not a divergence
        }
        let first_bad = first_l1_disagreement(&d, &r);
        rows.push((token.clone(), d.len(), r.len(), *end, first_bad));
    }
    assert!(!rows.is_empty(), "no market had quotes on either side");

    let with_data = rows.len();
    let agreeing = rows.iter().filter(|r| r.4.is_none()).count();
    let total_d: usize = rows.iter().map(|r| r.1).sum();
    let total_r: usize = rows.iter().map(|r| r.2).sum();
    let mut by_gap: Vec<&(String, usize, usize, i64, Option<i64>)> = rows.iter().collect();
    by_gap.sort_by_key(|r| -((r.1 as i64 - r.2 as i64).abs()));

    eprintln!("\n=== derived-L1 vs recorded-l1_quotes, {with_data} markets with data ===");
    eprintln!("markets whose L1 step function NEVER disagrees: {agreeing}/{with_data}");
    eprintln!("total quotes: derived={total_d} recorded={total_r}");
    eprintln!("\nworst 10 by |count gap|:");
    eprintln!(
        "{:<22} {:>9} {:>9} {:>8}  first-disagreement-ts",
        "token", "derived", "recorded", "gap"
    );
    for r in by_gap.iter().take(10) {
        eprintln!(
            "{:<22} {:>9} {:>9} {:>8}  {}",
            &r.0[..22.min(r.0.len())],
            r.1,
            r.2,
            r.1 as i64 - r.2 as i64,
            r.4.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
        );
    }

    // Event-by-event inside the worst market, so the SHAPE of the divergence is visible rather
    // than inferred from counts.
    // Dump the first market that actually DISAGREES, falling back to the worst count gap when
    // none does. A count gap alone is usually just the anchor gate suppressing the pre-anchor
    // head start — benign and already understood — whereas a step-function disagreement is the
    // thing still unexplained, so that is what deserves the event-by-event look.
    if let Some(worst) = by_gap.iter().find(|r| r.4.is_some()).or_else(|| by_gap.first()) {
        let range = TsRange::of(worst.3 - TENOR_MS, worst.3);
        let d = archive.scan_quotes(VENUE, &worst.0, range).unwrap();
        let r = store.scan_quotes(VENUE, &worst.0, range).unwrap();
        if let Some(bad) = worst.4 {
            eprintln!("\nfirst disagreement at ts={bad}; window opens at {}", worst.3 - TENOR_MS);
            // STRADDLE the disagreement: the last 3 events at or before it and the first 5 at
            // or after, per side. "First N within a window" truncates before the interesting
            // moment whenever the stream is dense — which is exactly when it matters.
            let straddle = |v: &[QuoteTick], label: &str| {
                let split = v.partition_point(|q| q.ts < bad);
                eprintln!("--- {label}, straddling the disagreement ---");
                for q in v[split.saturating_sub(3)..(split + 5).min(v.len())].iter() {
                    eprintln!(
                        "  {}ts={} bid={} x{} ask={} x{}",
                        if q.ts >= bad { ">" } else { " " },
                        q.ts,
                        q.bid,
                        q.bid_size,
                        q.ask,
                        q.ask_size
                    );
                }
            };
            straddle(&d, "derived");
            straddle(&r, "recorded");
        }
        eprintln!("\n=== event-by-event, worst market {} ===", &worst.0[..22.min(worst.0.len())]);
        eprintln!("--- derived (first 20) ---");
        for q in d.iter().take(20) {
            eprintln!("  ts={} bid={} x{} ask={} x{}", q.ts, q.bid, q.bid_size, q.ask, q.ask_size);
        }
        eprintln!("--- recorded (first 20) ---");
        for q in r.iter().take(20) {
            eprintln!("  ts={} bid={} x{} ask={} x{}", q.ts, q.bid, q.bid_size, q.ask, q.ask_size);
        }
    }
}

/// First ts at which the two quote streams disagree about the L1 **in force at that instant**.
///
/// Compared as STEP FUNCTIONS, not element-wise: the two sources are allowed to emit at
/// different moments (different dedup, different capture), so zipping them by index would
/// report every stream as totally divergent and explain nothing. Instead, at each event ts in
/// either stream, ask both "what is your latest quote at or before this ts?" and compare those.
/// Prices compare within half a tick, for the same reason [`near`] does. `None` = the two never
/// disagree anywhere in the window.
fn first_l1_disagreement(a: &[QuoteTick], b: &[QuoteTick]) -> Option<i64> {
    // TIE-TOLERANT: when several events share a timestamp the feed defines NO order among
    // them, and the two pipelines sort independently, so "the state after ts" can legitimately
    // differ by which same-ts event happens to be last. Measured (token 1151341668…, ts=364):
    // both streams carried the same two events, `bid=0.41 x13.3` and `bid=0.4 x208.58`, in
    // opposite order — identical before, identical after. Comparing the state-after-ts alone
    // reports that as a divergence, which is an artifact of the comparator, not of the data.
    //
    // So a ts agrees if the two sides carry the same SET of L1 states at it, or if the state
    // after it matches. Only a genuine difference in what was seen survives both.
    let states_at = |v: &[QuoteTick], ts: i64| -> Vec<(u64, u64)> {
        let mut s: Vec<(u64, u64)> = v
            .iter()
            .filter(|q| q.ts == ts)
            .map(|q| ((q.bid * 1000.0).round() as u64, (q.ask * 1000.0).round() as u64))
            .collect();
        s.sort_unstable();
        s
    };
    let latest_at = |v: &[QuoteTick], ts: i64| -> Option<(f64, f64)> {
        v.iter().rev().find(|q| q.ts <= ts).map(|q| (q.bid, q.ask))
    };
    let mut times: Vec<i64> = a.iter().map(|q| q.ts).chain(b.iter().map(|q| q.ts)).collect();
    times.sort_unstable();
    times.dedup();
    // Only compare once BOTH streams have started; a pure head-start is a count difference,
    // reported by the count columns, not a mid-stream divergence.
    let both_live = match (a.first(), b.first()) {
        (Some(x), Some(y)) => x.ts.max(y.ts),
        _ => return None,
    };
    for ts in times {
        if ts < both_live {
            continue;
        }
        if let (Some((ab, aa)), Some((bb, ba))) = (latest_at(a, ts), latest_at(b, ts)) {
            let after_matches = (ab - bb).abs() < 0.005 && (aa - ba).abs() < 0.005;
            if !after_matches && states_at(a, ts) != states_at(b, ts) {
                return Some(ts);
            }
        }
    }
    None
}

// ---- live measurement smoke (never run in CI; needs the real the CI box archive file) ------------
//
// Reports the MEASURE deliverable numbers this module's report is built on: wall clock, bytes
// read (via `plan`'s row-group byte accounting), row groups touched vs total, rows returned, and
// peak RSS (`/proc/self/status` `VmHWM`, Linux-only, best-effort) for ONE token's own market
// window vs the same query repeated over ~20 tokens. Self-skips when the file is absent — same
// idiom as `vike_archive.rs`'s `#[ignore]`d live smokes. Run manually:
// `cargo test -p vike-backfill --features vike-archive --lib archive_store::tests::live_measure_against_real_archive_file -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_measure_against_real_archive_file() {
    const FILE: &str = "/var/lib/vike/dl/btc5m_2026-07-28.parquet";
    if !Path::new(FILE).exists() {
        eprintln!("{FILE} not present — skipping live archive-store measurement");
        return;
    }

    fn vm_hwm_kb() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status.lines().find_map(|l| {
            l.strip_prefix("VmHWM:")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kb| kb.parse().ok())
        })
    }

    // 25 real token ids from this file's own day, oldest-window-first (harvested via
    // `clickhouse-local`'s standalone `GROUP BY token_id` against the same local file — no
    // server, no network). The first is used for the single-token case; all 25 for the
    // "~20 tokens" case (a realistic per-series backtest replay loop: one `scan_book_updates`
    // call per token, exactly like `hist_replay::replay_ticks` issues per series).
    const TOKENS: &[&str] = &[
        "23152407599655538885623805493900043467958601874029530629927915276594719410439",
        "104571443997234897304141858465981034372030063880357705082249379826847038542378",
        "14301790185379892771571254247303545757717650772133892621663602964384542523992",
        "78321747154810254051861975218789122051845823043795362938649577164074888086089",
        "97671438448529185350787963197240804605805884897399485407503693494408417168470",
        "26256870810957662813774985645741265151480872553185993802730211703418628116159",
        "53027238618278186187577262403781830403994334114229466701325039465002184402157",
        "96667851774266765992556672614454228715564184201094718939745824592319584816112",
        "104086802706154418012875514448736113322113701846888813699955218638729179713845",
        "69050896154587792492371870680355670647404731895517712766508577896915780266670",
        "21413240782782670867191403316273233750669966867951507759590252952907554173144",
        "64363682592539029771920593705361530319135028911791177669001455328725856174386",
        "57997709997188651018796419454040666912401809435049910488400909521576768666389",
        "36325126872656430136186868653334900065037869194804379074055328330745125586278",
        "79393919153640374967889303608748517110647328768868919134062021800595655663540",
        "67708259947866665757301650057559387706903035949289005175172523672038772089859",
        "9783993234210266699347245365566496415323137082179260422643701204848379402442",
        "80887692812220726900805645119609289512621731695120160205175177755807711515699",
        "47151877687402178187507313687642483587217295197325736942711148947667700139749",
        "98251074153187170609910333754018623687412278037670001730907786212785897353274",
    ];

    let store = ArchiveParquetHistStore::from_files([PathBuf::from(FILE)]);

    // ---- single token, whole file's ts range (its own window is well inside it) -----------
    let one = TOKENS[0];
    let t0 = std::time::Instant::now();
    let plan = store.plan(one, TsRange::all()).unwrap();
    let plan_ms = t0.elapsed().as_millis();
    let t1 = std::time::Instant::now();
    let rows = store.scan_book_updates(VENUE, one, TsRange::all()).unwrap();
    let scan_ms = t1.elapsed().as_millis();
    let rss = vm_hwm_kb();
    println!(
        "SINGLE token={one} plan_ms={plan_ms} scan_ms={scan_ms} rows={} \
             row_groups={}/{} bytes={}/{} vm_hwm_kb={rss:?}",
        rows.len(),
        plan[0].selected_row_groups,
        plan[0].total_row_groups,
        plan[0].selected_compressed_bytes,
        plan[0].total_compressed_bytes,
    );

    // ---- ~20 tokens, one scan_book_updates call each (a realistic replay loop) -------------
    let t2 = std::time::Instant::now();
    let mut total_rows = 0usize;
    let mut total_selected_rg = 0usize;
    let mut total_rg = 0usize;
    let mut total_selected_bytes: i64 = 0;
    let mut total_bytes: i64 = 0;
    for &tok in TOKENS {
        let p = store.plan(tok, TsRange::all()).unwrap();
        total_selected_rg += p[0].selected_row_groups;
        total_rg += p[0].total_row_groups;
        total_selected_bytes += p[0].selected_compressed_bytes;
        total_bytes += p[0].total_compressed_bytes;
        total_rows += store.scan_book_updates(VENUE, tok, TsRange::all()).unwrap().len();
    }
    let many_ms = t2.elapsed().as_millis();
    let rss_many = vm_hwm_kb();
    println!(
        "MANY tokens={} total_ms={many_ms} avg_ms={:.1} total_rows={total_rows} \
             row_groups_selected_sum={total_selected_rg} row_groups_total_sum={total_rg} \
             bytes_selected_sum={total_selected_bytes} bytes_total_sum={total_bytes} \
             vm_hwm_kb={rss_many:?}",
        TOKENS.len(),
        many_ms as f64 / TOKENS.len() as f64,
    );
}
