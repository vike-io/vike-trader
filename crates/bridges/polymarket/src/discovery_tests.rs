use super::*;

fn mk(slug: &str, question: &str, liq: f64, vol: f64, end: &str, toks: &[&str]) -> GammaMarket {
    GammaMarket {
        id: slug.into(),
        question: question.into(),
        condition_id: format!("0x{slug}"),
        slug: slug.into(),
        end_date: end.into(),
        volume: vol,
        liquidity: liq,
        active: true,
        closed: false,
        neg_risk: false,
        tick_size: 0.01,
        outcomes: vec!["Yes".into(), "No".into()],
        token_ids: toks.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// A fixture [`GammaSource`] over a fixed list, paged exactly like the real client.
struct FixtureSource {
    markets: Vec<GammaMarket>,
    calls: std::cell::Cell<usize>,
}

impl FixtureSource {
    fn new(markets: Vec<GammaMarket>) -> Self {
        Self { markets, calls: std::cell::Cell::new(0) }
    }
}

impl GammaSource for FixtureSource {
    fn list(
        &self,
        active_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GammaMarket>, String> {
        self.calls.set(self.calls.get() + 1);
        Ok(self
            .markets
            .iter()
            .filter(|m| !active_only || (m.active && !m.closed))
            .skip(offset)
            .take(limit)
            .cloned()
            .collect())
    }
}

struct FailingSource;
impl GammaSource for FailingSource {
    fn list(&self, _: bool, _: usize, _: usize) -> Result<Vec<GammaMarket>, String> {
        Err("gamma 503".into())
    }
}

/// A fixture source shaped like the REAL Gamma directory: `list` pages a `volumeNum`-DESC
/// ordering (so a fresh ~$0-volume window sorts last, below any bounded crawl), while `by_slug`
/// is the O(1) exact lookup `GammaClient::by_slug` performs. This is the fixture the original
/// suite lacked — with insertion-ordered 4-row fixtures the volume blind spot is invisible.
struct RankedSource {
    markets: Vec<GammaMarket>,
    pages: std::cell::Cell<usize>,
    slug_lookups: std::cell::Cell<usize>,
}

impl RankedSource {
    fn new(mut markets: Vec<GammaMarket>) -> Self {
        markets.sort_by(|a, b| b.volume.partial_cmp(&a.volume).unwrap());
        Self { markets, pages: std::cell::Cell::new(0), slug_lookups: std::cell::Cell::new(0) }
    }
}

impl GammaSource for RankedSource {
    fn list(
        &self,
        _active_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GammaMarket>, String> {
        self.pages.set(self.pages.get() + 1);
        Ok(self.markets.iter().skip(offset).take(limit).cloned().collect())
    }

    fn by_slug(&self, slug: &str, _spec: FetchSpec) -> Result<Option<GammaMarket>, String> {
        self.slug_lookups.set(self.slug_lookups.get() + 1);
        let want = slug.to_lowercase();
        Ok(self.markets.iter().find(|m| m.slug.to_lowercase() == want).cloned())
    }
}

/// A big directory: `n` high-volume markets plus one fresh zero-volume window at the bottom.
fn ranked_directory(n: usize, fresh_slug: &str) -> Vec<GammaMarket> {
    let mut v: Vec<GammaMarket> = (0..n)
        .map(|i| {
            mk(
                &format!("popular-{i}"),
                "popular?",
                10_000.0,
                1_000_000.0 - i as f64,
                "2026-12-31T00:00:00Z",
                &["p"],
            )
        })
        .collect();
    v.push(mk(fresh_slug, "BTC up or down?", 0.0, 0.0, "2026-07-18T12:10:00Z", &["f1", "f2"]));
    v
}

fn sample() -> Vec<GammaMarket> {
    vec![
        mk(
            "btc-up-or-down-2026-07-18-1200",
            "BTC up or down?",
            5_000.0,
            900.0,
            "2026-07-18T12:05:00Z",
            &["b1", "b2"],
        ),
        mk(
            "btc-up-or-down-2026-07-18-1205",
            "BTC up or down?",
            100.0,
            0.0,
            "2026-07-18T12:10:00Z",
            &["c1", "c2"],
        ),
        mk(
            "nba-lakers-win-tonight",
            "Will the Lakers win?",
            20_000.0,
            50_000.0,
            "2026-07-19T00:00:00Z",
            &["n1", "n2"],
        ),
        mk("us-election-2028-winner", "Who wins in 2028?", 1.0, 2.0, "", &["e1"]),
    ]
}

// ---- filters ------------------------------------------------------------------------------

#[test]
fn slug_prefix_filter_matches_prefix_case_insensitively() {
    let f = SlugPrefixFilter::new("BTC-UP-OR-DOWN-");
    let src = FixtureSource::new(sample());
    let got = f.fetch(&src).unwrap();
    assert_eq!(
        got.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
        vec!["btc-up-or-down-2026-07-18-1200", "btc-up-or-down-2026-07-18-1205"]
    );
}

#[test]
fn slug_prefix_filter_applies_liquidity_and_volume_floors() {
    let f = SlugPrefixFilter::new("btc-up-or-down-").with_floors(1_000.0, 500.0);
    let src = FixtureSource::new(sample());
    let got = f.fetch(&src).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].slug, "btc-up-or-down-2026-07-18-1200");
}

#[test]
fn not_expired_gate_drops_past_and_unparseable_end_dates() {
    // 2026-07-18T12:06:00Z — the first window (ends 12:05) is expired, the second (12:10) is not.
    let now = mk("s", "q", 0.0, 0.0, "2026-07-18T12:06:00Z", &[]).resolution_ts_ms().unwrap();
    let f = SlugPrefixFilter::new("btc-up-or-down-").not_expired_at(now);
    let src = FixtureSource::new(sample());
    let got = f.fetch(&src).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].slug, "btc-up-or-down-2026-07-18-1205");

    // an absent/malformed end_date can't be proven unexpired -> rejected under the gate,
    // accepted without it.
    let gate = SlugPrefixFilter::new("us-election").not_expired_at(now);
    assert!(!gate.accept(&sample()[3]));
    assert!(SlugPrefixFilter::new("us-election").accept(&sample()[3]));
}

#[test]
fn tag_filter_any_and_all() {
    let any = TagFilter::any(["nba", "election"]);
    let all = TagFilter::all(["btc", "down"]);
    let ms = sample();
    assert!(any.accept(&ms[2]) && any.accept(&ms[3]));
    assert!(!any.accept(&ms[0]));
    assert!(all.accept(&ms[0]));
    assert!(!TagFilter::all(["btc", "lakers"]).accept(&ms[0]));
    // empty tag list matches nothing (a visible mistake, not a full-directory subscribe)
    assert!(!TagFilter::any(Vec::<String>::new()).accept(&ms[0]));
}

#[test]
fn tag_filter_matches_question_text_too() {
    // "lakers" is in the slug; "Will the" only in the question — both hit the same haystack.
    let ms = sample();
    assert!(TagFilter::any(["will the"]).accept(&ms[2]));
}

#[test]
fn search_filter_matches_question_or_slug_case_insensitively() {
    let ms = sample();
    assert!(SearchFilter::new("LAKERS").accept(&ms[2]));
    assert!(SearchFilter::new("who wins").accept(&ms[3]));
    assert!(!SearchFilter::new("nothing here").accept(&ms[2]));
    // empty query matches nothing
    assert!(!SearchFilter::new("").accept(&ms[2]));
}

#[test]
fn predicate_filter_runs_the_closure() {
    let f = PredicateFilter::new(|m: &GammaMarket| m.liquidity > 10_000.0);
    let src = FixtureSource::new(sample());
    let got = f.fetch(&src).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].slug, "nba-lakers-win-tonight");
}

#[test]
fn fetch_pages_until_short_page_and_respects_max_pages() {
    let mut f = SlugPrefixFilter::new("");
    f.fetch = FetchSpec { active_only: true, page_limit: 2, max_pages: 5 };
    let src = FixtureSource::new(sample());
    let got = f.fetch(&src).unwrap();
    assert_eq!(got.len(), 4);
    // 4 markets / page_limit 2 -> pages of 2,2,0: stops on the first short (empty) page.
    assert_eq!(src.calls.get(), 3);

    // max_pages caps the crawl
    let mut capped = SlugPrefixFilter::new("");
    capped.fetch = FetchSpec { active_only: true, page_limit: 2, max_pages: 1 };
    let src2 = FixtureSource::new(sample());
    assert_eq!(capped.fetch(&src2).unwrap().len(), 2);
    assert_eq!(src2.calls.get(), 1);
}

#[test]
fn fetch_error_aborts_the_pass() {
    let f = SlugPrefixFilter::new("btc");
    assert_eq!(f.fetch(&FailingSource).unwrap_err(), "gamma 503");
}

// ---- window slug computation --------------------------------------------------------------

const TPL: &str = "bitcoin-up-or-down-{yyyy}-{mm}-{dd}-{HH}{MM}";

#[test]
fn window_starts_floor_to_the_bucket_and_look_ahead() {
    let spec = WindowSpec::every_minutes(5, TPL);
    // 12:03:30 -> current window starts 12:00, next 12:05.
    let base = 1_784_376_000_000; // 2026-07-18T12:00:00Z (cross-checked below)
    let starts = spec.window_starts(base + 3 * 60_000 + 30_000);
    assert_eq!(starts, vec![base, base + 300_000]);
    // windows_ahead=2 -> three windows
    let s3 = spec.clone().with_windows_ahead(2).window_starts(base);
    assert_eq!(s3, vec![base, base + 300_000, base + 600_000]);
    // exactly on a boundary: that instant IS the new window's start
    assert_eq!(spec.window_starts(base + 300_000), vec![base + 300_000, base + 600_000]);
    // one ms before the boundary still belongs to the previous window
    assert_eq!(spec.window_starts(base + 299_999), vec![base, base + 300_000]);
}

#[test]
fn window_slugs_render_utc_calendar_fields() {
    let spec = WindowSpec::every_minutes(5, TPL);
    let base = 1_784_376_000_000; // 2026-07-18T12:00:00Z
    assert_eq!(
        spec.window_slugs(base + 1),
        vec![
            "bitcoin-up-or-down-2026-07-18-1200".to_string(),
            "bitcoin-up-or-down-2026-07-18-1205".to_string(),
        ]
    );
    // the anchor itself, verified against gamma.rs's independently-tested date parser
    assert_eq!(mk("s", "q", 0.0, 0.0, "2026-07-18T12:00:00Z", &[]).resolution_ts_ms(), Some(base));
}

#[test]
fn window_slugs_cross_utc_midnight_and_month_end() {
    let spec = WindowSpec::every_minutes(5, TPL);
    // 2026-07-31T23:57:00Z -> current 23:55 (Jul 31), next 00:00 (Aug 1): day, month AND the
    // 23->00 hour all roll in one step.
    let t = mk("s", "q", 0.0, 0.0, "2026-07-31T23:57:00Z", &[]).resolution_ts_ms().unwrap();
    assert_eq!(
        spec.window_slugs(t),
        vec![
            "bitcoin-up-or-down-2026-07-31-2355".to_string(),
            "bitcoin-up-or-down-2026-08-01-0000".to_string(),
        ]
    );
    // and a year boundary
    let ny = mk("s", "q", 0.0, 0.0, "2026-12-31T23:58:00Z", &[]).resolution_ts_ms().unwrap();
    assert_eq!(
        spec.window_slugs(ny),
        vec![
            "bitcoin-up-or-down-2026-12-31-2355".to_string(),
            "bitcoin-up-or-down-2027-01-01-0000".to_string(),
        ]
    );
}

#[test]
fn window_starts_floor_downward_before_the_epoch() {
    let spec = WindowSpec::every_minutes(5, TPL);
    // -1 ms is 1969-12-31T23:59:59.999Z: euclidean flooring puts it in the 23:55 window, NOT
    // in 00:00 (which truncating division would have given).
    assert_eq!(spec.window_starts(-1), vec![-300_000, 0]);
    assert_eq!(
        spec.window_slugs(-1),
        vec![
            "bitcoin-up-or-down-1969-12-31-2355".to_string(),
            "bitcoin-up-or-down-1970-01-01-0000".to_string(),
        ]
    );
}

#[test]
fn non_positive_bucket_selects_nothing_instead_of_dividing_by_zero() {
    let spec = WindowSpec { slug_template: TPL.into(), bucket_ms: 0, windows_ahead: 1 };
    assert!(spec.window_starts(1_784_376_000_000).is_empty());
    assert!(spec.window_slugs(1_784_376_000_000).is_empty());
}

#[test]
fn render_slug_supports_unix_and_leaves_unknown_placeholders() {
    let base = 1_784_376_000_000;
    assert_eq!(render_slug("w-{unix}", base), "w-1784376000");
    assert_eq!(render_slug("w-{nope}-{HH}", base), "w-{nope}-12");
}

#[test]
fn interval_label_maps_the_documented_buckets() {
    assert_eq!(interval_label(300_000), "5m");
    assert_eq!(interval_label(900_000), "15m");
    assert_eq!(interval_label(60_000), "1m");
}

/// The current Polymarket up/down series: `{asset}-updown-{interval}-{unix}`, `{unix}` = the
/// window START in UTC epoch seconds, bucket-aligned.
#[test]
fn updown_series_renders_the_epoch_stamped_slug_and_floors() {
    // 2026-07-21T12:00:00Z = epoch 1_784_635_200 s, already 300s-aligned.
    const T: i64 = 1_784_635_200_000;
    let spec = WindowSpec::updown("btc", 300_000);

    // aligned start -> the exact epoch-stamped slug; plus the next (current + windows_ahead=1).
    assert_eq!(
        spec.window_slugs(T),
        vec!["btc-updown-5m-1784635200".to_string(), "btc-updown-5m-1784635500".to_string()]
    );
    // a non-aligned instant (137s into the window) floors to the 300s boundary.
    assert_eq!(spec.window_slugs(T + 137_000)[0], "btc-updown-5m-1784635200");
    // one ms before the boundary still belongs to the previous window.
    assert_eq!(spec.window_slugs(T + 299_999)[0], "btc-updown-5m-1784635200");

    // eth asset label + the 15m / 1m interval buckets.
    assert_eq!(WindowSpec::updown("eth", 300_000).window_slugs(T)[0], "eth-updown-5m-1784635200");
    assert_eq!(WindowSpec::updown("btc", 900_000).window_slugs(T)[0], "btc-updown-15m-1784635200");
    assert_eq!(WindowSpec::updown("btc", 60_000).window_slugs(T)[0], "btc-updown-1m-1784635200");
}

// ---- planner ------------------------------------------------------------------------------

/// A [`WindowResolver`] over a fixed slug→tokens map, counting lookups.
struct MockResolver {
    map: BTreeMap<String, Vec<String>>,
    calls: std::cell::RefCell<Vec<String>>,
}

impl MockResolver {
    fn new(pairs: &[(&str, &[&str])]) -> Self {
        Self {
            map: pairs
                .iter()
                .map(|(s, t)| ((*s).to_string(), t.iter().map(|x| x.to_string()).collect()))
                .collect(),
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl WindowResolver for MockResolver {
    fn resolve(&self, slug: &str) -> Result<Vec<String>, String> {
        self.calls.borrow_mut().push(slug.to_string());
        Ok(self.map.get(slug).cloned().unwrap_or_default())
    }
}

struct FailingResolver;
impl WindowResolver for FailingResolver {
    fn resolve(&self, _: &str) -> Result<Vec<String>, String> {
        Err("gamma 503".into())
    }
}

const BASE: i64 = 1_784_376_000_000; // 2026-07-18T12:00:00Z
const W1200: &str = "bitcoin-up-or-down-2026-07-18-1200";
const W1205: &str = "bitcoin-up-or-down-2026-07-18-1205";
const W1210: &str = "bitcoin-up-or-down-2026-07-18-1210";

fn planner() -> RollingWindowPlanner {
    RollingWindowPlanner::new(WindowSpec::every_minutes(5, TPL))
}

#[test]
fn planner_first_tick_subscribes_current_and_next_windows() {
    let mut p = planner();
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1", "a2"]), (W1205, &["b1"])]);

    let t = p.tick(BASE, &r, &mut u).unwrap();
    assert_eq!(t.slugs, vec![W1200, W1205]);
    assert_eq!(t.resolved, vec![W1200, W1205]);
    assert!(t.missing.is_empty());
    assert_eq!(t.diff.to_add, vec!["a1", "a2", "b1"]); // sorted-set order
    assert!(t.diff.to_remove.is_empty());
    assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["a1", "a2", "b1"]);
}

#[test]
fn planner_re_tick_in_the_same_window_is_an_empty_diff_and_no_refetch() {
    let mut p = planner();
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);

    p.tick(BASE, &r, &mut u).unwrap();
    let before = r.calls.borrow().len();
    // later in the SAME bucket -> identical slugs, cached resolutions, no change.
    let t2 = p.tick(BASE + 299_999, &r, &mut u).unwrap();
    assert!(t2.diff.is_empty(), "idempotent re-evaluation must not churn the feeds");
    assert_eq!(r.calls.borrow().len(), before, "cache hit: no resolver traffic");
    assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["a1", "b1"]);
}

#[test]
fn planner_rolls_forward_dropping_the_expired_window() {
    let mut p = planner();
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"]), (W1210, &["c1"])]);

    p.tick(BASE, &r, &mut u).unwrap();
    // cross into the next bucket: 12:00 expires, 12:10 joins.
    let t = p.tick(BASE + 300_000, &r, &mut u).unwrap();
    assert_eq!(t.slugs, vec![W1205, W1210]);
    assert_eq!(t.diff.to_add, vec!["c1"]);
    assert_eq!(t.diff.to_remove, vec!["a1"], "the expired window leaves the streamed set");
    assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["b1", "c1"]);
    // the rolled-off window is dropped from the cache too (bounded memory)
    assert_eq!(p.cached_slugs().collect::<Vec<_>>(), vec![W1205, W1210]);
}

#[test]
fn planner_reports_an_unlisted_window_as_missing_and_retries_it() {
    let mut p = planner();
    let mut u = UniverseManager::default();
    // 12:05 is not listed yet.
    let r1 = MockResolver::new(&[(W1200, &["a1"])]);
    let t1 = p.tick(BASE, &r1, &mut u).unwrap();
    assert_eq!(t1.resolved, vec![W1200]);
    assert_eq!(t1.missing, vec![W1205]);
    assert_eq!(t1.diff.to_add, vec!["a1"]);

    // it appears on a later tick within the same window -> picked up, no unsubscribe churn.
    let r2 = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
    let t2 = p.tick(BASE + 60_000, &r2, &mut u).unwrap();
    assert!(t2.missing.is_empty());
    assert_eq!(t2.diff.to_add, vec!["b1"]);
    assert!(t2.diff.to_remove.is_empty());
    assert_eq!(r2.calls.borrow().as_slice(), [W1205], "only the uncached slug is looked up");
}

#[test]
fn planner_plan_does_not_commit() {
    let mut p = planner();
    let u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
    let t = p.plan(BASE, &r, &u).unwrap();
    assert_eq!(t.diff.to_add, vec!["a1", "b1"]);
    assert_eq!(u.subscribed().count(), 0, "plan must leave the universe untouched");
}

#[test]
fn planner_resolver_failure_leaves_the_universe_untouched() {
    let mut p = planner();
    let mut u = UniverseManager::default();
    let ok = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
    p.tick(BASE, &ok, &mut u).unwrap();

    // next bucket, but Gamma is down: the tick fails and NOTHING is unsubscribed.
    assert_eq!(p.tick(BASE + 300_000, &FailingResolver, &mut u).unwrap_err(), "gamma 503");
    assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["a1", "b1"]);
    assert_eq!(p.cached_slugs().collect::<Vec<_>>(), vec![W1200, W1205], "cache unchanged");
}

#[test]
fn gamma_slug_resolver_finds_the_market_by_exact_slug() {
    let src = FixtureSource::new(sample());
    let r =
        GammaSlugResolver::new(&src, FetchSpec { active_only: true, page_limit: 2, max_pages: 5 });
    assert_eq!(r.resolve("BTC-UP-OR-DOWN-2026-07-18-1205").unwrap(), vec!["c1", "c2"]);
    assert!(r.resolve("no-such-window").unwrap().is_empty());
    // and an error propagates rather than reading as "unlisted"
    let bad = GammaSlugResolver::new(&FailingSource, FetchSpec::default());
    assert_eq!(bad.resolve("x").unwrap_err(), "gamma 503");
}

/// THE regression for the volume-ordering blind spot: a fresh window sits below a crawl ceiling
/// of 500 in a volume-DESC directory, so only a by-slug lookup can find it. Before the fix
/// `GammaSlugResolver` scanned pages and returned empty here — i.e. it could never resolve the
/// exact case it exists for.
#[test]
fn gamma_slug_resolver_finds_a_fresh_low_volume_window_below_the_crawl_ceiling() {
    let src = RankedSource::new(ranked_directory(900, W1210));
    let spec = FetchSpec::default(); // 100 * 5 = 500 markets scanned; the window is #901.
    let r = GammaSlugResolver::new(&src, spec);

    assert_eq!(r.resolve(W1210).unwrap(), vec!["f1", "f2"]);
    assert_eq!(src.slug_lookups.get(), 1, "one by-slug request, not a crawl");
    assert_eq!(src.pages.get(), 0, "the volume-ordered browse must not be paged at all");

    // sanity: the market really is unreachable by scanning under this spec.
    let scanned: Vec<String> = (0..spec.max_pages)
        .flat_map(|p| src.list(true, spec.page_limit, p * spec.page_limit).unwrap().into_iter())
        .map(|m| m.slug)
        .collect();
    assert_eq!(scanned.len(), 500);
    assert!(!scanned.iter().any(|s| s == W1210), "fresh window sorts below the ceiling");
}

/// The default `by_slug` (the scan fallback for a source with no by-slug route) still works for
/// a market that IS inside the ceiling, and honours `page_limit == 0` without spinning.
#[test]
fn default_by_slug_scan_fallback_and_zero_page_limit() {
    let src = FixtureSource::new(sample());
    let spec = FetchSpec { active_only: true, page_limit: 2, max_pages: 5 };
    assert_eq!(
        src.by_slug("BTC-UP-OR-DOWN-2026-07-18-1205", spec).unwrap().unwrap().token_ids,
        vec!["c1", "c2"]
    );
    assert!(src.by_slug("no-such-window", spec).unwrap().is_none());

    let zero = FetchSpec { active_only: true, page_limit: 0, max_pages: 5 };
    let src2 = FixtureSource::new(sample());
    assert!(src2.by_slug("btc-up-or-down-2026-07-18-1205", zero).unwrap().is_none());
    assert_eq!(src2.calls.get(), 0, "page_limit 0 must not re-request offset 0 max_pages times");
}

/// `MarketFilter::fetch` with `page_limit == 0` returns empty instead of looping to `max_pages`
/// re-requesting a zero-size page (a zero-size page is never "short").
#[test]
fn fetch_with_zero_page_limit_is_an_immediate_empty() {
    let mut f = SlugPrefixFilter::new("");
    f.fetch = FetchSpec { active_only: true, page_limit: 0, max_pages: 5 };
    let src = FixtureSource::new(sample());
    assert!(f.fetch(&src).unwrap().is_empty());
    assert_eq!(src.calls.get(), 0);
}

/// A backwards wall-clock step (NTP correction) across a bucket boundary must NOT roll the
/// universe back: no re-subscribe of the expired window, no unsubscribe of the future one, and
/// no re-resolution traffic.
#[test]
fn backwards_clock_step_does_not_roll_the_universe_back() {
    let mut p = planner();
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"]), (W1210, &["c1"])]);

    p.tick(BASE, &r, &mut u).unwrap();
    // cross the 12:05 boundary: 12:00 rolls off, 12:10 joins.
    p.tick(BASE + 300_000, &r, &mut u).unwrap();
    assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["b1", "c1"]);
    let calls_before = r.calls.borrow().len();

    // clock steps back 200ms, to just BEFORE the boundary we already crossed.
    let t = p.tick(BASE + 299_800, &r, &mut u).unwrap();
    assert!(t.diff.is_empty(), "a backwards clock step must not churn the feeds");
    assert_eq!(t.slugs, vec![W1205, W1210], "windows stay clamped forward");
    assert_eq!(u.subscribed().collect::<Vec<_>>(), vec!["b1", "c1"]);
    assert_eq!(r.calls.borrow().len(), calls_before, "no re-resolution of the expired window");
    assert_eq!(p.last_now_ms(), Some(BASE + 300_000));

    // and the clock catching back up past the clamp still rolls forward normally.
    let t2 = p.tick(BASE + 600_000, &r, &mut u).unwrap();
    assert_eq!(t2.slugs[0], W1210);
    assert_eq!(t2.diff.to_remove, vec!["b1"]);
}

/// The miss-retry throttle: an unlisted window is re-asked at most once per interval, and is
/// still picked up as soon as the throttle lets a retry through.
#[test]
fn miss_retry_throttle_bounds_resolver_traffic_for_an_unlisted_window() {
    let mut p =
        RollingWindowPlanner::new(WindowSpec::every_minutes(5, TPL)).with_miss_retry_ms(120_000);
    let mut u = UniverseManager::default();
    let r1 = MockResolver::new(&[(W1200, &["a1"])]); // 12:05 not listed yet

    p.tick(BASE, &r1, &mut u).unwrap();
    assert_eq!(r1.calls.borrow().len(), 2); // both slugs asked once
    // 30s later: throttled, no new lookup, still reported missing.
    let t = p.tick(BASE + 30_000, &r1, &mut u).unwrap();
    assert_eq!(t.missing, vec![W1205]);
    assert_eq!(r1.calls.borrow().len(), 2, "throttled: no re-ask inside the interval");
    // 2min later: retried.
    p.tick(BASE + 120_000, &r1, &mut u).unwrap();
    assert_eq!(r1.calls.borrow().as_slice()[2], W1205);

    // default (no throttle) re-asks every tick — byte-identical to the pre-fix behavior.
    let mut p2 = planner();
    let mut u2 = UniverseManager::default();
    let r2 = MockResolver::new(&[(W1200, &["a1"])]);
    p2.tick(BASE, &r2, &mut u2).unwrap();
    p2.tick(BASE + 30_000, &r2, &mut u2).unwrap();
    assert_eq!(r2.calls.borrow().len(), 3, "unthrottled default retries every tick");
}

/// `windows_ahead = 0` is the degenerate current-window-only case.
#[test]
fn windows_ahead_zero_holds_only_the_current_window() {
    let spec = WindowSpec::every_minutes(5, TPL).with_windows_ahead(0);
    assert_eq!(spec.window_starts(BASE + 1), vec![BASE]);
    let mut p = RollingWindowPlanner::new(spec);
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"])]);
    let t = p.tick(BASE, &r, &mut u).unwrap();
    assert_eq!(t.slugs, vec![W1200]);
    assert_eq!(t.diff.to_add, vec!["a1"], "the next window is NOT pre-subscribed");
}

/// A large look-ahead whose future windows are all unlisted: only the current window is
/// subscribed, the rest are reported missing (never an error, never a partial universe).
#[test]
fn large_look_ahead_with_every_future_window_unlisted() {
    let spec = WindowSpec::every_minutes(5, TPL).with_windows_ahead(4);
    let mut p = RollingWindowPlanner::new(spec);
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1"])]);
    let t = p.tick(BASE, &r, &mut u).unwrap();
    assert_eq!(t.resolved, vec![W1200]);
    assert_eq!(t.missing.len(), 4);
    assert_eq!(t.diff.to_add, vec!["a1"]);
    assert_eq!(t.target, ["a1".to_string()].into_iter().collect::<BTreeSet<_>>());
}

/// Two sources over ONE manager: diffing each target separately churns (which is why the docs
/// no longer suggest it), while `plan_union` over both targets is stable.
#[test]
fn two_sources_share_one_manager_only_via_plan_union() {
    let mut p = planner();
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["w1"]), (W1205, &["w2"])]);

    // the liquidity path's target, and the planner's.
    let liquidity: BTreeSet<String> = ["l1".to_string(), "l2".to_string()].into();
    let window_target = p.plan(BASE, &r, &u).unwrap().target;

    // union: one commit, both sources' tokens subscribed.
    let d = u.plan_union([&liquidity, &window_target]);
    assert_eq!(d.to_add, vec!["l1", "l2", "w1", "w2"]);
    assert!(d.to_remove.is_empty());
    u.commit(&d);

    // re-planning both against the union is a clean no-op — no churn.
    let d2 = u.plan_union([&liquidity, &window_target]);
    assert!(d2.is_empty(), "a shared manager must not churn when both sources are unchanged");

    // whereas diffing ONE source alone against the shared manager would evict the other's
    // tokens — pinned here so the anti-pattern stays visibly wrong.
    let solo = u.plan_tokens(window_target.clone());
    assert_eq!(solo.to_remove, vec!["l1", "l2"]);
}

/// The pm-resolution hand-off, pinned rather than asserted in prose: when a window expires and
/// its token is unsubscribed, the settlement watchlist must STILL be watching that token — the
/// position is settling and its payout has not been emitted yet. (`polymarket`-gated with the
/// settlement cluster it drives — the ONE exec-plane reference in this feed-plane module's
/// tests; a `--features feeds` `cargo test` compiles this file without it.)
#[cfg(feature = "polymarket")]
#[test]
fn expired_window_unsubscribe_leaves_a_settling_position_watched() {
    use crate::exec_plane::settlement::positions::Position;
    use crate::exec_plane::settlement::resolve::ResolveWatchlist;

    let mut p = planner();
    let mut u = UniverseManager::default();
    let r = MockResolver::new(&[(W1200, &["a1"]), (W1205, &["b1"]), (W1210, &["c1"])]);
    p.tick(BASE, &r, &mut u).unwrap();

    // we hold the 12:00 window's outcome token, awaiting settlement.
    let mut watch = ResolveWatchlist::new();
    let held = Position {
        condition_id: "0xw1200".into(),
        asset: "a1".into(),
        size: 25.0,
        redeemable: false, // not resolved yet — still settling
        neg_risk: false,
        outcome_index: Some(0),
        title: "BTC up or down?".into(),
        cur_price: None,
    };
    watch.upsert_from_positions(std::slice::from_ref(&held));

    // the window expires and the planner unsubscribes its token from the streamed universe...
    let t = p.tick(BASE + 300_000, &r, &mut u).unwrap();
    assert_eq!(t.diff.to_remove, vec!["a1"]);
    assert!(!u.subscribed().any(|s| s == "a1"), "market data for a1 is gone");

    // ...and the settlement watchlist is UNAFFECTED: it is fed from /positions, not from the
    // subscribed set, so the payout leg is still armed.
    assert_eq!(watch.len(), 1);
    assert_eq!(watch.get("a1").map(|e| e.qty), Some(25.0));
    // it only leaves the watchlist when /positions reports it flat (redeemed/sold).
    watch.upsert_from_positions(std::slice::from_ref(&held));
    assert_eq!(watch.len(), 1, "an unsubscribe is not a position event");
    assert_eq!(watch.prune_flat(&[]), vec!["a1"]);
}

#[test]
fn planner_end_to_end_over_the_gamma_resolver() {
    // the two btc windows in `sample()` are exactly the 12:00 and 12:05 windows of a
    // "btc-up-or-down-{yyyy}-{mm}-{dd}-{HH}{MM}" series.
    let spec = WindowSpec::every_minutes(5, "btc-up-or-down-{yyyy}-{mm}-{dd}-{HH}{MM}");
    let mut p = RollingWindowPlanner::new(spec);
    let mut u = UniverseManager::default();
    let src = FixtureSource::new(sample());
    let r = GammaSlugResolver::new(&src, FetchSpec::default());

    let t = p.tick(BASE, &r, &mut u).unwrap();
    assert_eq!(t.diff.to_add, vec!["b1", "b2", "c1", "c2"]);
    assert!(t.missing.is_empty());
}
