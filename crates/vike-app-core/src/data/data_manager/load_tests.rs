use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use vike_data::{HistoryStoreError, SeriesCoverage, SeriesId};
use vike_data_manager::DataManagerInventorySeriesKey;
use vike_model::StoreKindId;

/// The deadline every test that must SUCCEED runs under. Deliberately generous: these stores
/// answer from memory, so the only thing between the call and the answer is a thread spawn and
/// a channel hop — but the CI runners are shared boxes that go CPU-starved, and a bound tight
/// enough to be "realistic" here would turn every test in this file into a load-dependent
/// flake. Nothing in the file asserts that a HEALTHY call is fast; the timing assertions all
/// live on the timeout side, where the margin is three orders of magnitude.
const TEST_DEADLINE: Duration = Duration::from_secs(5);

/// The deadline the timeout tests run under, against a store that parks for [`PARK`]. The 300x
/// gap between them is what makes "the bound fired" a fact rather than a race.
const SHORT_DEADLINE: Duration = Duration::from_millis(100);

/// How long a planted hang parks for — the stand-in for the real failure, a peer that stops
/// answering. Long enough that a test observing a RETURN has observed the bound and not the store;
/// finite rather than infinite only so the parked helper threads are reclaimed if the harness
/// outlives them.
const PARK: Duration = Duration::from_secs(30);

/// Wrap a double in the shape the walk takes. The walk needs `Arc<dyn HistoryStore + Send + Sync>`
/// rather than a `&dyn HistoryStore` because each RPC is handed to a helper thread that outlives
/// the call (see [`bounded`]) — a borrow could not be, which is the one API consequence of
/// bounding the walk at all.
fn arc(store: FakeCatalogStore) -> Arc<dyn HistoryStore + Send + Sync> {
    Arc::new(store)
}

/// [`load_stored`] over one double, which answers the walk and the coverage report alike.
fn load(store: FakeCatalogStore) -> DataManagerInventoryLoadOutcome {
    let store = arc(store);
    load_stored(&store, &[], &store, "test", TEST_DEADLINE, &|_| {})
}

/// A seeded catalog-only `HistoryStore` double: answers the two verbs the walk uses
/// (`inventory`, `series_gaps`); every other required verb is unreachable and says so.
struct FakeCatalogStore {
    inventory: Result<Vec<(SeriesId, SeriesCoverage)>, String>,
    gaps: Vec<(SeriesId, Vec<(i64, i64)>)>,
    /// A planted per-series `series_gaps` failure (the degrade-contract probe).
    failing_gap_probe: Option<SeriesId>,
    /// The §6-Q2 cross-kind report this store answers with. `Err` stands in for BOTH ways a
    /// store cannot answer: a `HistoryStoreDatahub` whose peer predates the verb (refused
    /// client-side by the capability check) and a read that failed outright.
    coverage: Result<Vec<vike_data::InstrumentCoverage>, String>,
    /// Answer `coverage_report` the way a server OLDER than the verb does over the wire: the
    /// capability refusal `HistoryStoreDatahub` gives before sending anything.
    coverage_older: bool,
    /// Park in every `series_gaps` probe instead of answering — the HANG the probes' bound exists
    /// for, and the failure a planted `Err` cannot stand in for: an `Err` is an answer. (The
    /// inventory has no such flag: its bound is the socket's, proven over a scripted datahub below.)
    hang_gaps: bool,
    /// REFUSE every probe — a store that is talking and saying no, which is a different fact
    /// from one that has gone quiet and is the premise of
    /// `a_refusal_is_an_answer_and_does_not_count_towards_the_streak`. `failing_gap_probe`
    /// cannot express it: it plants ONE failure, and one is below [`DEAD_PEER_STREAK`], so a
    /// test built on it would pass whether or not refusals counted towards the streak.
    fail_all_gaps: bool,
    /// How many probes actually REACHED this store. The streak test reads it because "the walk
    /// stopped early" is a claim about calls made, and asserting it by elapsed time instead
    /// would be a timing race dressed up as a behaviour check.
    probes: AtomicUsize,
    /// How long every probe takes to answer — the stand-in for a round trip, which is what the walk
    /// overlaps. `ZERO` for every test that is not about overlap.
    probe_delay: Duration,
    /// Probes in flight right now and the most there ever were: "the walk overlaps its probes" is a
    /// claim about concurrency, so it is asserted on this peak and not on elapsed time.
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    /// Park every probe from the `n`th on — a peer that ANSWERS for a while and then goes away,
    /// which `hang_gaps` (every probe, from the first) cannot express.
    hang_gaps_from: Option<usize>,
}

impl Default for FakeCatalogStore {
    fn default() -> Self {
        Self {
            inventory: Ok(Vec::new()),
            gaps: Vec::new(),
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
            coverage_older: false,
            hang_gaps: false,
            fail_all_gaps: false,
            probes: AtomicUsize::new(0),
            probe_delay: Duration::ZERO,
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            hang_gaps_from: None,
        }
    }
}

/// Counts a probe out of flight when it returns, however it returns.
struct InFlight<'a>(&'a AtomicUsize);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn scopeguard_in_flight(counter: &AtomicUsize) -> InFlight<'_> {
    InFlight(counter)
}

fn off_walk(verb: &str) -> HistoryStoreError {
    HistoryStoreError::Query(format!("FakeCatalogStore: {verb} is not part of the stored walk"))
}

impl HistoryStore for FakeCatalogStore {
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        self.inventory.clone().map_err(HistoryStoreError::Query)
    }
    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        // Counted BEFORE any park or refusal, because the claim it backs is "how many probes
        // did the walk ATTEMPT" — a probe the walk started and abandoned still reached here.
        let nth = self.probes.fetch_add(1, Ordering::Relaxed);
        let now = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(now, Ordering::AcqRel);
        let _out = scopeguard_in_flight(&self.in_flight);
        std::thread::sleep(self.probe_delay);
        if self.hang_gaps || self.hang_gaps_from.is_some_and(|from| nth >= from) {
            std::thread::sleep(PARK);
        }
        if self.fail_all_gaps || self.failing_gap_probe.as_ref() == Some(id) {
            return Err(HistoryStoreError::Query("planted gap-probe failure".into()));
        }
        Ok(self
            .gaps
            .iter()
            .find(|(gid, _)| gid == id)
            .map(|(_, ranges)| ranges.clone())
            .unwrap_or_default())
    }
    fn coverage_report(&self) -> Result<Vec<vike_data::InstrumentCoverage>, HistoryStoreError> {
        if self.coverage_older {
            return Err(HistoryStoreError::Unsupported("predates the coverage verb".into()));
        }
        self.coverage.clone().map_err(HistoryStoreError::Query)
    }

    // ---- everything below is unreachable for the walk ----
    vike_data::hist_store_stubs!(refuse(off_walk): all);
}

fn sid(kind: &str, venue: &str, symbol: &str, iv: Option<&str>) -> SeriesId {
    SeriesId::per_symbol(kind.parse().unwrap(), venue, symbol, iv.map(Into::into))
}
/// `dates: 0`: these fakes count no days, so the walk probes every series ([`may_have_gaps`]).
fn coverage(rows: u64, bytes: u64, a: i64, b: i64) -> SeriesCoverage {
    SeriesCoverage { first_timestamp: a, last_timestamp: b, rows, bytes, parts: 1, dates: 0 }
}
fn key(
    kind: &str,
    venue: &str,
    symbol_name: &str,
    iv: Option<&str>,
) -> DataManagerInventorySeriesKey {
    DataManagerInventorySeriesKey {
        venue: venue.into(),
        symbol: symbol_name.into(),
        kind: kind.parse().unwrap(),
        interval: iv.map(Into::into),
        grouped: false,
        source: None,
    }
}

fn small_fixture() -> Vec<(SeriesId, SeriesCoverage)> {
    vec![
        (sid("bar", "binance", "BTCUSDT", Some("1m")), coverage(10, 100, 1_000, 2_000)),
        (sid("trade", "binance", "BTCUSDT", None), coverage(5, 50, 1_000, 1_500)),
        (sid("bar", "okx", "ETH-USDT", Some("5m")), coverage(7, 70, 1_100, 2_100)),
    ]
}

/// A catalog with more series than [`DEAD_PEER_STREAK`], so "the walk stopped probing" is a
/// claim that CAN fail. `small_fixture` is exactly the streak long, which makes it useless for
/// that one test and is why this second fixture exists rather than the first one growing — the
/// tests above pin counts against its length.
fn wide_fixture() -> Vec<(SeriesId, SeriesCoverage)> {
    (0..DEAD_PEER_STREAK * 2 + 1)
        .map(|n| (sid("bar", "binance", &format!("SYM{n}USDT"), Some("1m")), coverage(1, 1, 0, 1)))
        .collect()
}

/// A catalog far wider than [`PROBE_WORKERS`] and [`DEAD_PEER_STREAK`], for the walk's overlap.
fn very_wide_fixture() -> Vec<(SeriesId, SeriesCoverage)> {
    (0..PROBE_WORKERS * 4)
        .map(|n| (sid("bar", "binance", &format!("SYM{n}USDT"), Some("1m")), coverage(1, 1, 0, 1)))
        .collect()
}

/// The measured defect: the walk probed one series at a time, 135 ms each through a tunnel, so a
/// 36-series catalog spent 4.8 s of a 5.9 s load waiting on round trips that do not depend on each
/// other. With a healthy peer the probes now overlap — more than one in flight, never more than
/// [`PROBE_WORKERS`] — and the answer is the same one the one-at-a-time walk gave: every series
/// probed, every gap in its place.
#[test]
fn a_healthy_walk_overlaps_its_probes_and_still_probes_every_series() {
    let inventory = very_wide_fixture();
    let target = inventory[PROBE_WORKERS * 2].0.clone();
    let store = Arc::new(FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        gaps: vec![(target.clone(), vec![(1_200, 1_300)])],
        probe_delay: Duration::from_millis(40),
        ..Default::default()
    });
    let dyn_store: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    let lanes = vec![dyn_store.clone(); PROBE_WORKERS];
    let (tree, gaps) =
        load_stored_tree(&dyn_store, &lanes, TEST_DEADLINE).expect("a healthy store loads");
    assert_eq!(store.probes.load(Ordering::Relaxed), inventory.len(), "every series was probed");
    let peak = store.peak.load(Ordering::Relaxed);
    assert!(peak > 1, "the probes overlapped: at most {peak} were ever in flight");
    assert!(peak <= PROBE_WORKERS, "…but never more than the {PROBE_WORKERS} workers: {peak}");
    assert_eq!(tree, build_inventory_tree(inventory));
    let key = DataManagerInventorySeriesKey {
        venue: target.venue.clone(),
        symbol: target.label().to_string(),
        kind: target.kind,
        interval: target.interval.clone(),
        grouped: target.group.is_some(),
        source: target.source.clone(),
    };
    assert_eq!(gaps.get(&key), Some(&vec![(1_200, 1_300)]), "the one gap landed under its series");
    assert_eq!(gaps.len(), 1, "and no other series made an entry");
}

/// **A series whose parts cover every day of its span is not probed**: the store's answer would
/// be no gap (the CI box, 2026-11-17: 632 of 820 series, each a round trip through the owner's tunnel).
/// One that misses a day, and one whose store counts no days, still are, and the missing day's gap
/// lands under its series.
#[test]
fn a_series_holding_every_day_of_its_span_is_not_probed() {
    let day = vike_model::MS_PER_DAY;
    let whole = sid("bar", "binance", "BTCUSDT", Some("1m"));
    let holed = sid("bar", "okx", "ETH-USDT", Some("1m"));
    let uncounted = sid("trade", "binance", "BTCUSDT", None);
    let days = |dates, last| SeriesCoverage { dates, ..coverage(3, 3, 0, last) };
    let store = Arc::new(FakeCatalogStore {
        inventory: Ok(vec![
            (whole.clone(), days(3, 2 * day + 5)),
            (holed.clone(), days(2, 2 * day + 5)),
            (uncounted.clone(), days(0, 2 * day + 5)),
        ]),
        gaps: vec![(holed.clone(), vec![(day, 2 * day - 1)])],
        ..Default::default()
    });
    let dyn_store: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    let (_, gaps) =
        load_stored_tree(&dyn_store, &[], TEST_DEADLINE).expect("a healthy store loads");
    assert_eq!(store.probes.load(Ordering::Relaxed), 2, "the whole series was not probed");
    assert_eq!(
        gaps.get(&key("bar", "okx", "ETH-USDT", Some("1m"))),
        Some(&vec![(day, 2 * day - 1)])
    );
    assert_eq!(gaps.len(), 1, "and the whole series has no entry: it reads gap-free");
}

/// A peer that ANSWERED and then went away mid-walk — the tunnel dropping, the ordinary failure.
/// The first [`DEAD_PEER_STREAK`] probes answer (so the overlap starts), every later one parks.
/// The walk must stop on the streak rather than pay a deadline per series: probes already in flight
/// when it is reached finish (each bounded by its own deadline), so the cost is bounded by the
/// workers and not by the catalog. The claim is asserted on probes MADE, as its sequential twin is.
#[test]
fn a_peer_that_goes_away_after_answering_costs_the_workers_not_the_catalog() {
    let inventory = very_wide_fixture();
    let store = Arc::new(FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        hang_gaps_from: Some(DEAD_PEER_STREAK),
        ..Default::default()
    });
    let dyn_store: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    let lanes = vec![dyn_store.clone(); PROBE_WORKERS];
    let (tree, gaps) = load_stored_tree(&dyn_store, &lanes, SHORT_DEADLINE)
        .expect("a dead peer mid-walk is not fatal");
    let probes = store.probes.load(Ordering::Relaxed);
    assert!(
        probes < inventory.len(),
        "the walk must stop probing, not wait out every series: {probes} of {}",
        inventory.len()
    );
    assert!(
        probes <= DEAD_PEER_STREAK + 2 * PROBE_WORKERS,
        "the cost is the warm-up plus the workers in flight, never the catalog: {probes}"
    );
    assert_eq!(
        vike_data_manager::gaps_unknown(&gaps),
        inventory.len() - DEAD_PEER_STREAK,
        "every series after the peer went away reads not known, asked or not; the ones it answered whole \
         make no entry"
    );
    assert_eq!(
        tree,
        build_inventory_tree(inventory),
        "and the TREE is still complete — that is what renders"
    );
}

/// THE equality pin of the seam close: walking a seeded TRAIT store yields byte-identically
/// the `DataManagerInventoryVenue` tree the local arm's fold (`build_inventory_tree` over the same inventory) yields —
/// so a remote grid renders exactly what a local grid over the same data renders.
#[test]
fn the_trait_walk_yields_the_tree_the_local_fold_yields_for_identical_data() {
    let inventory = small_fixture();
    let store = FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        gaps: vec![(sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)])],
        failing_gap_probe: None,
        coverage: Ok(Vec::new()),
        ..Default::default()
    };
    let (tree, gaps) = load_stored_tree(&arc(store), &[], TEST_DEADLINE).expect("seeded walk");
    assert_eq!(
        tree,
        build_inventory_tree(inventory),
        "remote walk and local fold must agree on the tree"
    );
    assert_eq!(gaps.len(), 1, "exactly the one gappy series gets an entry");
    assert_eq!(gaps[&key("bar", "okx", "ETH-USDT", Some("5m"))], vec![(1_200, 1_300)]);
}

/// A GROUPED series' gaps land under a key the grid can actually look up.
///
/// ⚠ This is a regression test for a bug that shipped, and the reason it survived is the shape
/// of every other test in this file: they all build fixtures with `SeriesId::per_symbol`, where
/// `symbol == label()` and the two spellings are indistinguishable. A grouped series carries an
/// EMPTY `symbol` and is identified by its GROUP NAME, which is what `build_inventory_tree` keys the
/// display tree on and what the grid looks this map up with — so keying here on `symbol`
/// produced an entry nothing could ever match. Every polymarket group rendered gap-free, and
/// `DataManagerInventoryFilter::HasGaps` could not list one.
#[test]
fn a_grouped_series_gap_is_keyed_by_its_group_name_not_an_empty_symbol() {
    let g = SeriesId::grouped(StoreKindId::Quote, "polymarket", "btc-5m");
    assert!(g.symbol.is_empty(), "the premise: a grouped series carries no symbol");
    let store = FakeCatalogStore {
        inventory: Ok(vec![(g.clone(), coverage(9, 90, 1_000, 3_000))]),
        gaps: vec![(g.clone(), vec![(1_500, 1_600)])],
        failing_gap_probe: None,
        coverage: Ok(Vec::new()),
        ..Default::default()
    };
    let (tree, gaps) = load_stored_tree(&arc(store), &[], TEST_DEADLINE).expect("seeded walk");

    let node = &tree[0].symbols[0];
    assert_eq!(node.symbol, "btc-5m", "the tree names the group");
    assert!(
        gaps.contains_key(&DataManagerInventorySeriesKey::of("polymarket", node, &node.series[0])),
        "the gap map must be reachable by the key the grid builds from the tree, not by an empty \
         symbol"
    );
    assert!(
        !gaps.contains_key(&key("quote", "polymarket", &node.symbol, None)),
        "nor under the per-symbol series of the same label"
    );
    assert!(
        !gaps.contains_key(&key("quote", "polymarket", "", None)),
        "no entry may hide under the empty symbol"
    );
}

/// An absent key already reads as "no known gaps" downstream, so a gap-free series must not
/// insert an empty entry (byte-preserves `refresh_stored`'s original omit-empty behavior).
#[test]
fn a_gap_free_series_gets_no_gap_map_entry() {
    let store = FakeCatalogStore {
        inventory: Ok(small_fixture()),
        gaps: Vec::new(),
        failing_gap_probe: None,
        coverage: Ok(Vec::new()),
        ..Default::default()
    };
    let (tree, gaps) = load_stored_tree(&arc(store), &[], TEST_DEADLINE).expect("seeded walk");
    assert_eq!(tree.len(), 2, "both venues present");
    assert!(gaps.is_empty(), "no gaps anywhere ⇒ an empty map, not empty entries");
}

/// The degrade contract: one series' failing gap probe costs THAT series its answer, never the
/// refresh — the tree stays complete and every other series' gaps still land. ⚠ And the series reads
/// "not known", an EMPTY list, because no entry at all is what a whole series gets.
#[test]
fn a_failing_gap_probe_reads_not_known_and_never_stops_the_walk() {
    let inventory = small_fixture();
    let store = FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        gaps: vec![
            (sid("bar", "binance", "BTCUSDT", Some("1m")), vec![(1_400, 1_600)]),
            (sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)]),
        ],
        failing_gap_probe: Some(sid("bar", "okx", "ETH-USDT", Some("5m"))),
        coverage: Ok(Vec::new()),
        ..Default::default()
    };
    let (tree, gaps) = load_stored_tree(&arc(store), &[], TEST_DEADLINE)
        .expect("a per-series failure is not fatal");
    assert_eq!(
        tree,
        build_inventory_tree(inventory),
        "the tree survives a gap-probe failure whole"
    );
    assert_eq!(gaps[&key("bar", "binance", "BTCUSDT", Some("1m"))], vec![(1_400, 1_600)]);
    assert_eq!(gaps[&key("bar", "okx", "ETH-USDT", Some("5m"))], Vec::new(), "not known");
    assert_eq!(gaps.len(), 2, "a series that answered whole makes no entry");
    assert_eq!(vike_data_manager::gaps_unknown(&gaps), 1);
}

/// An inventory failure IS fatal to the load (there is nothing to render) — surfaced as `Err`
/// naming the cause, which [`load_stored`] turns into `DataManagerInventoryLoadOutcome::error` and the caller
/// RENDERS (it used to render an empty tree, which is indistinguishable from an empty store —
/// see `an_unreadable_store_and_an_empty_one_are_different_outcomes`).
#[test]
fn an_inventory_failure_is_an_err_naming_the_cause() {
    let store = FakeCatalogStore {
        inventory: Err("planted inventory failure".into()),
        gaps: Vec::new(),
        failing_gap_probe: None,
        coverage: Ok(Vec::new()),
        ..Default::default()
    };
    let err =
        load_stored_tree(&arc(store), &[], TEST_DEADLINE).expect_err("no inventory ⇒ no load");
    assert!(err.contains("planted inventory failure"), "the cause must survive: {err}");
}

// ---- the §6-Q2 partial-day fold ------------------------------------------------------------

/// One instrument recording BOTH trade and quote, where day 2 has trades and no quotes — the
/// smallest shape that makes `partial_days` non-empty (a report with one recorded kind can
/// never disagree with itself, so a single-kind fixture would pass vacuously).
fn partial_fixture() -> Vec<vike_data::InstrumentCoverage> {
    vike_data::store::coverage::join_coverage(&[
        (sid("trade", "binance", "BTCUSDT", None), vec![0, 1, 2]),
        (sid("quote", "binance", "BTCUSDT", None), vec![0, 1]),
    ])
}

/// The fold a REMOTE grid runs is the fold a LOCAL grid runs: `load_partials` over a store that
/// answers the trait verb equals `partial_days_from_coverage` over the same report. This is the
/// app-core half of the wire-adds-nothing property the composed datahub test proves end to end.
#[test]
fn the_partial_fold_equals_the_direct_fold_over_the_same_report() {
    let report = partial_fixture();
    let store = FakeCatalogStore { coverage: Ok(report.clone()), ..Default::default() };
    let folded = load_partials(&arc(store)).expect("a store that answers coverage");
    assert_eq!(folded, partial_days_from_coverage(&report), "one fold, whichever store");
    assert!(!folded.is_empty(), "the fixture must actually produce a partial day");
}

/// A store that cannot answer is an `Err`, NOT an empty map. The distinction is the whole
/// honesty of the column: an empty map renders as "nothing is partial", which is a claim, while
/// the `Err` is what the caller turns into `mode::PARTIALS_UNSERVED`.
#[test]
fn an_unanswerable_coverage_report_is_an_err_not_an_empty_map() {
    let store = FakeCatalogStore {
        coverage: Err("does not advertise `coverage`".into()),
        ..Default::default()
    };
    let err =
        load_partials(&arc(store)).expect_err("an unanswerable report must not fold to empty");
    assert!(err.to_string().contains("coverage"), "the cause must survive for the log line: {err}");
}

/// A store that answers with an EMPTY report is a different fact from one that cannot answer,
/// and must stay one: it folds to an empty map through `Ok`, so the column renders blank
/// (correctly — nothing is partial) instead of showing the unserved note.
#[test]
fn an_empty_report_folds_to_an_empty_map_through_ok() {
    let store = FakeCatalogStore::default();
    assert!(load_partials(&arc(store)).expect("an empty report is still an answer").is_empty());
}

// ---- the BOUND (see the module doc's "THE WALK IS BOUNDED HERE") ---------------------------

/// The idle window of the scripted-datahub tests: [`load_remote_within`]'s bound, in milliseconds.
const WINDOW: Duration = Duration::from_millis(300);

/// A scripted key-less datahub that advertises nothing (so the coverage report and the history read
/// are refused before anything is sent) and answers each connection's frames: `Inventory` with
/// `inventory`, written in `chunks` pieces `gap` apart (a thin link), or never when `inventory` is
/// `None` (a peer gone silent); a gap probe with no gaps. Nothing else is asked of it.
fn scripted_datahub(
    inventory: Option<Vec<(SeriesId, SeriesCoverage)>>,
    chunks: usize,
    gap: Duration,
) -> String {
    let probes = ScriptedProbes { released: true.into(), ..Default::default() };
    scripted_datahub_probing(inventory, chunks, gap, Arc::new(probes))
}

/// How [`scripted_datahub_probing`] answers a gap probe: it holds it until `released`, then answers
/// `gaps` (a series not listed has none), or refuses it when `refuse`. `answered` counts the answers.
#[derive(Default)]
struct ScriptedProbes {
    released: std::sync::atomic::AtomicBool,
    gaps: Vec<(SeriesId, Vec<(i64, i64)>)>,
    refuse: bool,
    answered: AtomicUsize,
}

/// [`scripted_datahub`] whose gap probes answer as `probes` says.
fn scripted_datahub_probing(
    inventory: Option<Vec<(SeriesId, SeriesCoverage)>>,
    chunks: usize,
    gap: Duration,
    probes: Arc<ScriptedProbes>,
) -> String {
    use std::io::Write;
    use vike_datahub_client::proto::{Request, Response};
    use vike_hub_protocol::frame::{read_frame, write_frame};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let address = listener.local_addr().expect("assigned port").to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let (Ok(mut stream), inventory, probes) = (stream, inventory.clone(), probes.clone())
            else {
                return;
            };
            std::thread::spawn(move || {
                if !vike_datahub_client::test_support::accept_keyless(&mut stream, Vec::new()) {
                    return;
                }
                while let Ok(request) = read_frame::<_, Request>(&mut stream) {
                    let rows = match (request, &inventory) {
                        (Request::Inventory, Some(rows)) => rows.clone(),
                        (Request::Inventory, None) => return std::thread::sleep(PARK),
                        (Request::SeriesGaps { id }, _) => {
                            while !probes.released.load(Ordering::Acquire) {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            let reply = if probes.refuse {
                                Response::Error("planted probe refusal".into())
                            } else {
                                let found = probes.gaps.iter().find(|(g, _)| *g == id);
                                Response::SeriesGaps(
                                    found.map(|(_, r)| r.clone()).unwrap_or_default(),
                                )
                            };
                            probes.answered.fetch_add(1, Ordering::AcqRel);
                            if write_frame(&mut stream, &reply).is_err() {
                                return;
                            }
                            continue;
                        }
                        (other, _) => panic!("the script expected no {other:?}"),
                    };
                    let mut frame = Vec::new();
                    write_frame(&mut frame, &Response::Inventory(rows)).unwrap();
                    for piece in frame.chunks(frame.len().div_ceil(chunks)) {
                        std::thread::sleep(gap);
                        if stream.write_all(piece).and_then(|()| stream.flush()).is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    address
}

/// [`load_remote_within`] against the scripted datahub at `address`, under [`WINDOW`].
fn remote(address: &str) -> DataManagerInventoryLoadOutcome {
    let address = address.to_string();
    load_remote_within(
        || vike_datahub_client::HistoryStoreDatahub::new(address.clone()),
        WINDOW,
        &|_| {},
    )
}

/// Fold what `catalog`'s load has sent until `done` holds, failing after [`TEST_DEADLINE`].
fn until(catalog: &mut DataManagerInventory, done: impl Fn(&DataManagerInventory) -> bool) {
    let deadline = Instant::now() + TEST_DEADLINE;
    loop {
        catalog.drain();
        if done(catalog) {
            return;
        }
        assert!(Instant::now() < deadline, "the load never got there");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **The tree lands before any gap probe answers**, over the real socket path the desktop runs
/// ([`DataManagerInventory::start`]): a datahub that answers the inventory and holds every probe gives the
/// shell every series at once, each still `checking…`; released, the probes fill the gap column in
/// and the load ends with the same tree.
#[test]
fn the_tree_lands_before_any_gap_probe_answers() {
    let rows = small_fixture();
    let holed = sid("bar", "okx", "ETH-USDT", Some("5m"));
    let probes = ScriptedProbes { gaps: vec![(holed, vec![(1_200, 1_300)])], ..Default::default() };
    let probes = Arc::new(probes);
    let address = scripted_datahub_probing(Some(rows.clone()), 1, Duration::ZERO, probes.clone());
    let mut catalog = DataManagerInventory::default();
    let dial = move || vike_datahub_client::HistoryStoreDatahub::new(address.clone());
    catalog.start(dial, &egui::Context::default());

    until(&mut catalog, |c| !c.tree.is_empty());
    let listed: usize = catalog.tree.iter().map(|v| v.total.series).sum();
    assert_eq!(listed, rows.len(), "every series is listed");
    assert_eq!(probes.answered.load(Ordering::Acquire), 0, "the premise: no probe has answered");
    assert_eq!(catalog.progress.gaps_pending.len(), rows.len(), "each one says checking…");
    assert!(catalog.loading && catalog.gaps.is_empty(), "and the load is still out");

    probes.released.store(true, Ordering::Release);
    until(&mut catalog, |c| !c.loading);
    assert!(catalog.progress.gaps_pending.is_empty(), "every probe answered");
    assert_eq!(
        catalog.gaps.get(&key("bar", "okx", "ETH-USDT", Some("5m"))),
        Some(&vec![(1_200, 1_300)])
    );
    assert_eq!(catalog.gaps.len(), 1, "the whole series make no entry");
    assert_eq!(
        catalog.tree,
        build_inventory_tree(rows),
        "the list it drew first is the list it ends with"
    );
    assert_eq!(catalog.error, None);
    assert_eq!(
        catalog.coverage,
        DatahubCoverage::Older,
        "a datahub that does not serve the report"
    );
    assert!(catalog.progress.coverage_cause.is_some(), "and the column says why");
}

/// **A refused probe costs its series' gap cell, never the list**: over the same path, a datahub
/// that refuses every probe still lists every series, each reads not known, and the cause is kept
/// for the column.
#[test]
fn a_refused_probe_leaves_the_list_in_place() {
    let rows = small_fixture();
    let probes = ScriptedProbes { released: true.into(), refuse: true, ..Default::default() };
    let address = scripted_datahub_probing(Some(rows.clone()), 1, Duration::ZERO, Arc::new(probes));
    let mut catalog = DataManagerInventory::default();
    let dial = move || vike_datahub_client::HistoryStoreDatahub::new(address.clone());
    catalog.start(dial, &egui::Context::default());
    until(&mut catalog, |c| !c.loading);
    assert_eq!(catalog.tree, build_inventory_tree(rows.clone()), "the list stays whole");
    assert_eq!(vike_data_manager::gaps_unknown(&catalog.gaps), rows.len(), "each reads not known");
    let cause = catalog.progress.gaps_cause.as_deref().unwrap_or_default();
    assert!(cause.contains("planted probe refusal"), "the server's words are the cause: {cause}");
    assert_eq!(catalog.error, None, "a refused probe is not an unreadable store");
}

/// **Every series the walk lists is answered exactly once**, the ones a dead peer left unasked
/// included, so no row is left saying `checking…`; the steps add up to the outcome the load returns.
#[test]
fn every_listed_series_is_answered_once_even_when_the_peer_goes_away() {
    let inventory = wide_fixture();
    let store = FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        hang_gaps: true,
        ..Default::default()
    };
    let store = arc(store);
    let events = std::sync::Mutex::new(Vec::new());
    let out = load_stored(&store, &[], &store, "test", SHORT_DEADLINE, &|e| {
        events.lock().unwrap().push(e);
    });
    let mut catalog = DataManagerInventory::default();
    for event in events.into_inner().unwrap() {
        catalog.apply(event);
    }
    assert!(catalog.progress.gaps_pending.is_empty(), "no row is left checking");
    assert!(!catalog.progress.coverage_pending, "and the report answered");
    assert_eq!((&catalog.tree, &catalog.gaps), (&out.tree, &out.gaps), "the steps are the whole");
    assert_eq!(vike_data_manager::gaps_unknown(&catalog.gaps), inventory.len());
    let cause = catalog.progress.gaps_cause.as_deref().unwrap_or_default();
    assert!(cause.contains("unanswered"), "the streak names itself: {cause}");
}

/// **A slow but steady answer COMPLETES.** The owner's desktop, behind a 13 to 40 KB/s tunnel, gave up
/// on a live inventory after a hard 10 s. Here the inventory arrives in ten pieces a third of the idle
/// window apart, so the whole answer takes three windows and no single silence reaches one: a hard
/// deadline of one window cuts it, the progress bound does not.
#[test]
fn a_slow_but_steady_inventory_completes() {
    let rows = small_fixture();
    let address = scripted_datahub(Some(rows.clone()), 10, WINDOW / 3);
    let began = Instant::now();
    let out = remote(&address);
    let took = began.elapsed();
    assert!(
        out.error.is_none(),
        "a link that keeps delivering is not a dead store: {:?}",
        out.error
    );
    assert_eq!(out.tree, build_inventory_tree(rows), "the whole answer arrived");
    assert!(took > WINDOW * 2, "the premise: the answer took several windows ({took:?})");
    assert_eq!(out.coverage, DatahubCoverage::Older, "a server that does not advertise the verb");
}

/// **A silent answer FAILS, within about one window**, as the honest "could not be read".
#[test]
fn a_silent_inventory_fails_within_the_window() {
    let address = scripted_datahub(None, 1, Duration::ZERO);
    let began = Instant::now();
    let out = remote(&address);
    let took = began.elapsed();
    let why = out.error.expect("a peer that sends nothing is unreadable");
    assert!(why.contains("no reply byte"), "the reason says the link went quiet: {why}");
    assert!(took < PARK / 2, "the window returned, not the planted park ({took:?})");
    assert!(out.tree.is_empty());
    assert_eq!(out.coverage, DatahubCoverage::Unknown, "the report was never asked");
}

/// **A store nobody answers for is "could not be read" at once**: nothing listens on the port.
#[test]
fn a_dead_datahub_is_unreadable() {
    let address = {
        let gone = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        gone.local_addr().expect("assigned port").to_string()
    };
    let out = remote(&address);
    assert!(out.error.is_some(), "a refused dial is an unreadable store, not an empty one");
}

/// A gap probe that never answers is treated exactly as one that FAILS: that series reads not known
/// and the walk finishes whole. `small_fixture` is deliberately exactly
/// [`DEAD_PEER_STREAK`] long, so every probe is attempted and the streak break below is a
/// SEPARATE claim rather than something this test could accidentally be proving.
#[test]
fn a_gap_probe_that_never_answers_skips_that_series_never_the_walk() {
    let inventory = small_fixture();
    assert_eq!(inventory.len(), DEAD_PEER_STREAK, "the premise: no probe is skipped by the streak");
    let store = Arc::new(FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        hang_gaps: true,
        ..Default::default()
    });
    let dyn_store: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    let (tree, gaps) = load_stored_tree(&dyn_store, &[], SHORT_DEADLINE)
        .expect("a probe that never answers is not fatal, any more than one that fails is");
    assert_eq!(tree, build_inventory_tree(inventory), "the tree survives every probe going quiet");
    assert_eq!(
        vike_data_manager::gaps_unknown(&gaps),
        gaps.len(),
        "an unanswered probe reads not known, and no made-up range"
    );
    assert_eq!(gaps.len(), DEAD_PEER_STREAK, "every series of this catalog");
    assert_eq!(
        store.probes.load(Ordering::Relaxed),
        DEAD_PEER_STREAK,
        "every series was still attempted"
    );
}

/// The streak: a peer that goes away MID-walk (a tunnel dropping is the ordinary way) stops
/// being probed, so the walk costs [`DEAD_PEER_STREAK`] deadlines rather than one per series.
///
/// Asserted on the probe COUNT rather than on elapsed time: "the walk stopped early" is a claim
/// about calls made, and an elapsed-time assertion would be a race dressed as a behaviour check
/// — it would also pass on a box slow enough to make the arithmetic ambiguous.
#[test]
fn the_walk_stops_probing_a_peer_that_went_away_mid_walk() {
    let inventory = wide_fixture();
    assert!(
        inventory.len() > DEAD_PEER_STREAK,
        "the premise: there must be series LEFT to skip, or this test cannot fail"
    );
    let store = Arc::new(FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        hang_gaps: true,
        ..Default::default()
    });
    let dyn_store: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    let (tree, gaps) = load_stored_tree(&dyn_store, &[], SHORT_DEADLINE)
        .expect("a dead peer mid-walk is not fatal");
    assert_eq!(
        store.probes.load(Ordering::Relaxed),
        DEAD_PEER_STREAK,
        "the walk must stop after {DEAD_PEER_STREAK} unanswered probes, not pay one deadline \
             per series for the rest of the catalog"
    );
    assert_eq!(
        vike_data_manager::gaps_unknown(&gaps),
        inventory.len(),
        "the series it never asked about read not known, like the ones that went unanswered"
    );
    assert_eq!(
        tree,
        build_inventory_tree(inventory),
        "and the TREE is still complete — that is what renders"
    );
}

/// An ANSWER resets the streak, refusal included: a store that says "I cannot read that
/// series' manifest" is a store that is talking, and unreadable manifests must not end a walk
/// over a catalog of thousands. Without the reset, `DEAD_PEER_STREAK` `Err`s in a row anywhere
/// in a large catalog would silently truncate the gap map.
///
/// ⚠ **Every probe refuses, and that is load-bearing.** A first draft planted ONE failure and
/// asserted the whole catalog was still probed — which would have passed whether or not
/// refusals counted towards the streak, since one is below [`DEAD_PEER_STREAK`]. With all of
/// them refusing, a streak that counted refusals would stop after three, so the probe count
/// below is a claim that can actually fail.
#[test]
fn a_refusal_is_an_answer_and_does_not_count_towards_the_streak() {
    let inventory = wide_fixture();
    let store = Arc::new(FakeCatalogStore {
        inventory: Ok(inventory.clone()),
        fail_all_gaps: true,
        ..Default::default()
    });
    let dyn_store: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    let (tree, _gaps) =
        load_stored_tree(&dyn_store, &[], TEST_DEADLINE).expect("a refusal is not a dead peer");
    assert_eq!(
        store.probes.load(Ordering::Relaxed),
        inventory.len(),
        "every series must still be probed — refusals are answers"
    );
    assert_eq!(tree, build_inventory_tree(inventory));
}

/// THE render honesty, and the half of the defect the bound alone does not fix: an EMPTY store
/// and an UNREADABLE one both produce an empty tree, and before `DataManagerInventoryLoadOutcome::error`
/// existed they were the same value — so both drew the same empty grid and an operator could
/// not tell "there is nothing recorded" from "nothing could be reached".
#[test]
fn an_unreadable_store_and_an_empty_one_are_different_outcomes() {
    let empty = load(FakeCatalogStore::default());
    assert!(empty.tree.is_empty(), "the premise: an empty store draws an empty tree");
    assert!(
        empty.error.is_none(),
        "a store that answered 'nothing' has no error to show — that IS the answer"
    );

    let unreadable = load(FakeCatalogStore {
        inventory: Err("planted inventory failure".into()),
        ..Default::default()
    });
    assert!(unreadable.tree.is_empty(), "the premise: it draws the SAME empty tree");
    let why = unreadable.error.expect("an unreadable store must carry its reason to the render");
    assert!(why.contains("planted inventory failure"), "the cause must survive: {why}");
}

/// A load whose walk failed does not then spend a SECOND deadline asking the same dead peer for
/// a coverage report — and the column reads `Unknown`, so no note claims the server did not answer a
/// verb nobody asked (the owner's screen showed exactly that claim under "could not be read"). The
/// fixture answers coverage happily, so a load that DID ask would come back `Served` with a non-empty
/// map and fail this.
#[test]
fn a_failed_walk_skips_the_coverage_fold_and_reads_unknown() {
    let out = load(FakeCatalogStore {
        inventory: Err("planted inventory failure".into()),
        coverage: Ok(partial_fixture()),
        ..Default::default()
    });
    assert_eq!(out.coverage, DatahubCoverage::Unknown);
    assert!(out.partials.is_empty(), "no fold may run against a store that could not be read");
}

/// The coverage report's two failures stay apart: a server older than the verb reads `Older` (its
/// note says so and guesses nothing), a read that failed reads `Unserved`.
#[test]
fn an_older_server_and_a_failed_report_are_told_apart() {
    let walked = || FakeCatalogStore { inventory: Ok(small_fixture()), ..Default::default() };
    let older = load(FakeCatalogStore { coverage_older: true, ..walked() });
    assert_eq!(older.coverage, DatahubCoverage::Older);
    let failed = load(FakeCatalogStore { coverage: Err("read failed".into()), ..walked() });
    assert_eq!(failed.coverage, DatahubCoverage::Unserved);
    for out in [older, failed] {
        assert!(out.error.is_none() && out.partials.is_empty(), "the tree still renders");
    }
}

/// The healthy path through [`load_stored`]: the walk, the fold and the negotiated answer all
/// land, and nothing reports an error. The counterweight to every degrade test above — without
/// it they would all still pass over a function that failed unconditionally.
#[test]
fn a_healthy_store_loads_with_no_error_and_a_served_coverage_answer() {
    let out = load(FakeCatalogStore {
        inventory: Ok(small_fixture()),
        gaps: vec![(sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)])],
        coverage: Ok(partial_fixture()),
        ..Default::default()
    });
    assert!(out.error.is_none(), "a store that answered has nothing to report: {:?}", out.error);
    assert_eq!(out.coverage, DatahubCoverage::Served);
    assert_eq!(out.tree.len(), 2, "both venues present");
    assert_eq!(out.gaps.len(), 1, "the one gappy series");
    assert!(!out.partials.is_empty(), "the §6-Q2 fold ran");
}

/// **The HISTORY column's three answers**, through the same bounded path the walk uses: a served
/// report is adopted as served; a datahub OLDER than the read (`Ok(None)`, nothing sent) gets this
/// build's own table under its caption; a failure — or a read that never answers — leaves the
/// column unloaded rather than inventing rows. And `load_stored` itself never fills it.
#[test]
fn the_history_read_has_three_answers_and_a_hang_is_one_of_them() {
    let now = 1_790_899_200_000;
    let report = vike_datahub_client::history::compiled_report(now);
    let served =
        load_history_channel_column(move || Ok(Some(report)), now, TEST_DEADLINE).expect("served");
    assert!(served.served && served.caption().is_none());

    let older = load_history_channel_column(|| Ok(None), now, TEST_DEADLINE).expect("the fallback");
    assert!(!older.served);
    assert_eq!(older.caption(), Some(vike_datahub_client::history::COMPILED_TABLE_CAPTION));

    assert_eq!(
        load_history_channel_column(|| Err("refused".to_string()), now, TEST_DEADLINE),
        None
    );
    let hang = || {
        std::thread::sleep(PARK);
        Ok(None)
    };
    let started = Instant::now();
    assert_eq!(
        load_history_channel_column(hang, now, SHORT_DEADLINE),
        None,
        "a hang is not an answer"
    );
    assert!(started.elapsed() < Duration::from_secs(2), "bounded, not waited out");

    let out = load(FakeCatalogStore::default());
    assert_eq!(out.history, None, "the walk does not ask the history-channels read");
}

/// **The grid is the MARKET plane: account series never reach it.** A catalog holding every
/// account kind beside market series builds the tree of the market series alone, and no gap
/// probe is spent on an account series — they are the Account window's History tab's (`crates/vike-app-core/src/data/execution_history.rs`).
#[test]
fn account_series_are_hidden_from_the_stored_grid() {
    let market = small_fixture();
    let mut inventory = market.clone();
    let account =
        StoreKindId::ALL.into_iter().filter(|k| k.plane() == vike_model::StorePlane::Account);
    for kind in account {
        inventory
            .push((sid(kind.word(), "binance", "BTCUSDT", None), coverage(3, 30, 1_000, 1_200)));
    }
    let store = FakeCatalogStore { inventory: Ok(inventory), ..Default::default() };
    let store = arc(store);
    let (tree, _) = load_stored_tree(&store, &[], TEST_DEADLINE).expect("seeded walk");
    assert_eq!(tree, build_inventory_tree(market), "only the market series are in the tree");
}
