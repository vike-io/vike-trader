use super::*;
use vike_model::rate_limits::DEFAULT_UTILIZATION;

// A generic base for the pure-URL/parse tests. Venue-specific host-resolution tests live in
// each venue's own `data.rs` (proven per-venue), since the resolved host IS the per-venue delta.
const BASE: &str = "https://host/api/v3/klines";

/// A trimmed `https://api.binance.com/api/v3/exchangeInfo` body — the `rateLimits` array
/// verbatim, plus enough envelope to prove the parse reads a real response and not a hand-shaped
/// fragment. Same capture (2026-08-04, from the CI box) as
/// `vike_bridge_core::rate_discovery`'s fixtures; duplicated as a literal here because those are
/// `#[cfg(test)]` consts private to that crate, and this rung must be provable on its own.
const SPOT_EXCHANGE_INFO: &str = r#"{
      "timezone": "UTC",
      "rateLimits": [
        {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":6000},
        {"rateLimitType":"ORDERS","interval":"SECOND","intervalNum":10,"limit":100},
        {"rateLimitType":"RAW_REQUESTS","interval":"MINUTE","intervalNum":5,"limit":300000}
      ],
      "symbols": [{"symbol":"BTCUSDT","status":"TRADING"}]
    }"#;

/// The `https://fapi.binance.com/fapi/v1/exchangeInfo` twin — `REQUEST_WEIGHT` 2400, NOT spot's
/// 6000. Aster's `fapi` serves a byte-identical array.
const FAPI_EXCHANGE_INFO: &str = r#"{
      "timezone": "UTC",
      "rateLimits": [
        {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":2400},
        {"rateLimitType":"ORDERS","interval":"MINUTE","intervalNum":1,"limit":1200}
      ],
      "symbols": [{"symbol":"BTCUSDT","status":"TRADING","contractType":"PERPETUAL"}]
    }"#;

fn spec(page_delay_ms: u64, exchange_info_url: Option<&'static str>) -> KlineSpec {
    KlineSpec {
        venue: "testvenue",
        rate_limit: KlineRateLimit {
            page_delay: Duration::from_millis(page_delay_ms),
            weight_soft_limit: 2000,
            weight_cooldown: Duration::from_secs(10),
            max_rate_limit_retries: 6,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
        },
        exchange_info_url: exchange_info_url.map(Cow::Borrowed),
        utilization: DEFAULT_UTILIZATION,
        volume_column: VolumeColumn::Index5,
    }
}

/// Weight actually spent per minute at a given inter-request `delay` and per-request weight —
/// the quantity the pacer steers, and the only honest thing to assert against a venue ceiling.
fn weight_per_minute(delay: Duration, per_request: f64) -> f64 {
    (60.0 / delay.as_secs_f64()) * per_request
}

/// The byte-identical-behaviour contract for every venue that publishes nothing
/// (bybit/okx/deribit, and aster while its host stays env-resolved): the pager sleeps exactly
/// `page_delay`, forever, and never cools down on the pacer's account.
#[test]
fn a_spec_without_discovery_paces_on_the_fixed_page_delay() {
    let s = spec(150, None);
    let mut p = pacer_for(&s, None);
    assert_eq!(p.next_delay(), s.rate_limit.page_delay);
    assert!(!p.should_cool_down(), "no budget ⇒ no target ⇒ the second guard stays silent");
    // Observations are still fed in by the loop; in fallback mode they must change nothing.
    p.observe(100);
    p.observe(2_500);
    assert_eq!(p.next_delay(), s.rate_limit.page_delay);
    assert!(!p.should_cool_down());
}

/// A `None` URL makes NO request — the early `?` in `fetch_exchange_info` is the whole reason an
/// undiscoverable venue is byte-identical rather than merely equivalent. (A live agent is built
/// here precisely to show it is never used.)
///
/// The MISPAIRED case takes the identical exit and is asserted the same way: a spec naming a
/// budget on some other host issues nothing either, so the refusal costs a backfill one fixed
/// `page_delay` and never a request against a host it was not paging.
#[test]
fn a_none_url_never_reaches_the_network() {
    let agent = vike_bridge_core::http::blocking_agent();
    assert_eq!(fetch_exchange_info(&agent, &spec(150, None), BASE), None);
    assert_eq!(
        fetch_exchange_info(
            &agent,
            &spec(150, Some("https://elsewhere.example/api/v3/exchangeInfo")),
            BASE
        ),
        None
    );
}

/// The pairing gate itself, over the shapes a venue face can hand in. It is `pub` and both
/// venues' `data.rs` tests drive it against their OWN hosts (binance: spot vs fapi; aster:
/// `Environment` x market), so this proves the RULE and they prove their hosts obey it.
#[test]
fn discovery_is_refused_unless_it_names_the_host_being_paged() {
    // Same host, different path ⇒ accepted, and the URL comes back verbatim.
    let same = spec(150, Some("https://host/api/v3/exchangeInfo"));
    assert_eq!(discovery_url(&same, BASE), Some("https://host/api/v3/exchangeInfo"));

    // Another host ⇒ refused. This is the binance spot-vs-fapi and the aster
    // testnet-vs-mainnet defect, in the one place that can see both halves.
    for other in [
        "https://otherhost/api/v3/exchangeInfo",
        "https://host.evil/api/v3/exchangeInfo",
        "https://host:8443/api/v3/exchangeInfo", // a port IS part of the endpoint's identity
        // ...and the scheme is NOT: this gate answers "same host", not "same scheme". No venue
        // face composes an http:// discovery URL, and inventing a scheme rule here would be an
        // untested policy on a shape nothing serves.
        "http://host/api/v3/exchangeInfo",
    ] {
        let s = spec(150, Some(other));
        let got = discovery_url(&s, BASE);
        let expected = if url_host(other) == url_host(BASE) { Some(other) } else { None };
        assert_eq!(got, expected, "{other} against {BASE}");
    }

    // An unreadable URL on either side is a REFUSAL, never an assumption.
    assert_eq!(discovery_url(&spec(150, Some("no-scheme/exchangeInfo")), BASE), None);
    assert_eq!(discovery_url(&same, "no-scheme/klines"), None);
    // ...and `None` in stays `None` out, with nothing to mispair.
    assert_eq!(discovery_url(&spec(150, None), BASE), None);
}

/// `url_host` reads the authority and nothing else — the unit a published budget belongs to.
#[test]
fn url_host_reads_the_authority_only() {
    assert_eq!(url_host("https://api.binance.com/api/v3/klines"), Some("api.binance.com"));
    assert_eq!(url_host("https://fapi.asterdex-testnet.com"), Some("fapi.asterdex-testnet.com"));
    assert_eq!(url_host("wss://host:9443/ws"), Some("host:9443"));
    assert_eq!(url_host("https:///path"), None); // empty authority
    assert_eq!(url_host("api.binance.com/klines"), None); // no scheme ⇒ unreadable
    assert_eq!(url_host(""), None);
}

/// Discovery on the real SPOT body: 6000/min at the 40 % default is 2400 weight/min, and once
/// the measured weight-2 cost of `/api/v3/klines?limit=1000` is observed that lands on a 50 ms
/// gap — INDEPENDENTLY reproducing the number `BINANCE_KLINE_SPOT.page_delay` was hand-measured
/// to. The point is not the coincidence: it is that the venue now supplies it.
#[test]
fn the_spot_budget_reproduces_the_hand_measured_pace() {
    let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
    let mut p = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
    p.observe(10);
    p.observe(12); // `/klines?limit=1000` costs weight 2 on spot
    let d = p.next_delay();
    assert!((d.as_secs_f64() - 0.05).abs() < 1e-6, "expected ~50ms, got {d:?}");
    assert!(weight_per_minute(d, 2.0) <= 6000.0, "must pace UNDER the venue's published ceiling");
}

/// The same on FAPI, where the ceiling is 2400 and a page costs weight 5.
///
/// ⚠ This asserts the UNMEASURED pace — the whole target gap slept, which is what the pacer
/// answers until a caller reports a request duration. Read end-to-end the two differ a lot: the
/// hand-set 150 ms delay looks like 2000 weight/min (83 % of the ceiling) by this maths, but a
/// MEASURED 24-month backfill at that delay left `x-mbx-used-weight-1m` at **556** (~23 %),
/// because each request's ~280 ms round trip dominates the sleep. So "discovery derives
/// ~312 ms, therefore 150 ms is 2x too fast" is a false conclusion — both are safe, and a
/// sleep-only pacer systematically UNDER-shoots its utilization target. That gap is what
/// [`Pacer::observe_request`] closes; see the sibling test below for the pace this loop
/// actually runs at once the first page has been timed.
#[test]
fn a_discovered_budget_paces_below_the_venue_ceiling() {
    let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
    let mut p = pacer_for(&s, Some(FAPI_EXCHANGE_INFO));
    p.observe(100);
    p.observe(105); // weight 5
    let d = p.next_delay();
    let spent = weight_per_minute(d, 5.0);
    assert!(spent <= 2400.0, "paced {spent} weight/min against a 2400 ceiling");
    assert!((spent - 960.0).abs() < 1.0, "40% of 2400 is 960 weight/min, got {spent}");
    assert!(
        d > s.rate_limit.page_delay,
        "the venue's own budget is STRICTER than the hand-set 150ms: {d:?}"
    );
}

/// Every unusable body degrades to the fallback — a discovery miss must never fail, or slow, a
/// backfill in some third way.
#[test]
fn an_unusable_exchange_info_body_falls_back_to_page_delay() {
    let s = spec(150, Some("https://host/exchangeInfo"));
    for body in [
        "",                                   // empty
        "not json",                           // garbage
        "<html>502 Bad Gateway</html>",       // a proxy error page
        r#"{"timezone":"UTC","symbols":[]}"#, // a venue that publishes nothing
        r#"{"rateLimits":[]}"#,               // an empty array
        // rows that exist but meter something else — ORDERS is a different gate entirely
        r#"{"rateLimits":[{"rateLimitType":"ORDERS","interval":"SECOND","limit":100}]}"#,
    ] {
        let p = pacer_for(&s, Some(body));
        assert_eq!(
            p.next_delay(),
            s.rate_limit.page_delay,
            "body {body:?} must fall back, not invent a pace"
        );
        assert!(!p.should_cool_down());
    }
}

/// The pace this loop ACTUALLY runs at, on the shape that was measured: fapi's 2400/min at the
/// 40 % default, weight-5 pages, and the ~280 ms round trip a pooled-agent request costs from
/// the CI box. The gap the venue sees is `sleep + request`, so the sleep must shrink to 32 ms for the
/// pair to land on the 312 ms that spends 960 weight/min.
///
/// (⚠ the 280 ms is the POOLED-agent number. A hand probe with ten separate `curl` calls read
/// 417 ms; that figure includes per-process DNS + TLS setup this agent pays once, and using it
/// here would over-subtract and pace ABOVE target.)
#[test]
fn a_timed_page_paces_to_the_target_end_to_end() {
    let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
    let mut p = pacer_for(&s, Some(FAPI_EXCHANGE_INFO));
    let rtt = Duration::from_millis(280);
    p.observe_request(Some(100), rtt);
    p.observe_request(Some(105), rtt); // weight 5

    let d = p.next_delay();
    let cycle = d + rtt;
    let spent = weight_per_minute(cycle, 5.0);
    assert!((spent - 960.0).abs() < 1.0, "40% of 2400 is 960 weight/min end-to-end, got {spent}");
    assert!(spent <= 2400.0, "and still under the venue's published ceiling");
    // The sleep-only pace would have spent ~506 — the ~23% the live run actually metered.
    assert!(
        weight_per_minute(Duration::from_millis(312) + rtt, 5.0) < 550.0,
        "the un-corrected pace must be the slow one, or this test proves nothing"
    );
}

/// The ETA the first page reports, on the run that motivated all of this: 24 months of 1m bars
/// is ~1051 pages of 1000, and at the corrected pace that is ~5.5 minutes — against the 8m00s
/// the same backfill MEASURED at the hand-set 150 ms delay (1051 * (0.150 + 0.280) = 452s, which
/// is the wall clock that run actually took, to within its 429-free noise).
#[test]
fn the_reported_eta_is_the_full_cycle_not_the_sleep() {
    let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
    let mut p = pacer_for(&s, Some(FAPI_EXCHANGE_INFO));
    assert_eq!(p.eta(1_051), None, "nothing timed yet ⇒ no ETA rather than a wrong one");

    p.observe_request(Some(100), Duration::from_millis(280));
    p.observe_request(Some(105), Duration::from_millis(280));
    let eta = p.eta(1_051).expect("a timed page yields an ETA");
    assert!(
        eta > Duration::from_secs(300) && eta < Duration::from_secs(360),
        "expected ~5m28s for 1051 pages at 312ms, got {eta:?}"
    );
}

/// The ETA's page count: window width over one page's span, `None` when the interval is not a
/// vike interval string at all (an ETA is a courtesy — a wrong one is worse than none).
#[test]
fn remaining_pages_divides_the_window_by_one_pages_span() {
    // 24 months of 1m bars — the measured shape. 730d = 63_072_000_000ms, a page = 60_000_000ms.
    let two_years = 730 * 86_400_000i64;
    assert_eq!(remaining_pages(0, two_years, "1m"), Some(1_051));
    // Coarser bars ⇒ far fewer pages for the same window.
    assert_eq!(remaining_pages(0, two_years, "1h"), Some(17));
    assert_eq!(remaining_pages(0, two_years, "1d"), Some(0), "730 days fits in one 1000-row page");
    // A window narrower than one page has nothing left after the page just fetched.
    assert_eq!(remaining_pages(0, 1_000, "1m"), Some(0));
    // A cursor already past the end never goes negative.
    assert_eq!(remaining_pages(two_years, 0, "1m"), Some(0));
    // Unparseable / absurd intervals decline to guess instead of dividing by zero.
    assert_eq!(remaining_pages(0, two_years, ""), None);
    assert_eq!(remaining_pages(0, two_years, "1y"), None);
    assert_eq!(remaining_pages(0, two_years, "0m"), None);
    // A page span that overflows i64 (`200000000d` x 1000 rows) declines rather than wrapping
    // into a tiny span and reporting a preposterous page count.
    assert_eq!(remaining_pages(0, i64::MAX, "200000000d"), None);
}

/// `is_discovered` is what the pace line stamps as its `discovered` field (it used to be the
/// GATE on that line — see the loop's comment). Nothing here asserts on a log line; it asserts
/// on the flag the log site reads, which is the testable half.
#[test]
fn the_fallback_pacer_reports_no_discovered_pace() {
    assert!(!pacer_for(&spec(150, None), None).is_discovered());
    assert!(
        !pacer_for(&spec(150, Some("https://host/exchangeInfo")), Some("not json")).is_discovered()
    );
    assert!(
        pacer_for(&spec(150, Some("https://host/exchangeInfo")), Some(FAPI_EXCHANGE_INFO))
            .is_discovered()
    );
}

/// The fallback pacer now ANSWERS an ETA once a page has been timed — which is what makes the
/// un-gated report emit rather than fall through its `Option` zip. The sleep is untouched.
#[test]
fn a_fallback_pacer_still_reports_a_measured_eta_and_the_unchanged_delay() {
    let s = spec(150, None);
    let mut p = pacer_for(&s, None);
    assert_eq!(p.eta(1_051), None, "nothing timed yet ⇒ no line, not a fabricated one");
    p.observe_request(None, Duration::from_millis(280));
    assert_eq!(p.next_delay(), s.rate_limit.page_delay, "measuring must not move the sleep");
    let eta = p.eta(1_051).expect("a timed page yields an ETA even with no budget");
    // 1051 pages x (280ms request + the unchanged 150ms sleep) = ~452s — which is the wall
    // clock the hand-set delay MEASURED on the CI box, now reportable before the run finishes.
    assert!(eta > Duration::from_secs(440) && eta < Duration::from_secs(465), "{eta:?}");
    assert!(p.measured().is_some(), "and the measurement is persistable");
}

/// A venue that RE-PRICES the endpoint self-corrects without anyone editing a const — the whole
/// reason the delay is inferred from the counter rather than measured once by hand.
#[test]
fn a_repriced_endpoint_widens_the_gap_by_itself() {
    let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
    let mut p = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
    p.observe(10);
    p.observe(12); // weight 2
    let cheap = p.next_delay();
    p.observe(17); // the same endpoint now costs 5
    let dear = p.next_delay();
    assert!(dear > cheap, "a heavier page must slow the pager, not keep the old pace");
    assert!(weight_per_minute(dear, 5.0) <= 6000.0);
}

#[test]
fn parse_klines_maps_one_row_exactly() {
    // one 12-element kline row; o/h/l/c/v are decimal strings, openTime is ms.
    let body = r#"[[1700000000000,"27000.10","27050.50","26980.00","27010.25","12.34567800",1700000059999,"0",3,"0","0","0"]]"#;
    let bars = parse_klines(body).unwrap();
    assert_eq!(bars.len(), 1);
    let b = &bars[0];
    assert_eq!(b.ts, 1_700_000_000_000);
    assert_eq!(b.open.to_bits(), 27000.10_f64.to_bits());
    assert_eq!(b.high.to_bits(), 27050.50_f64.to_bits());
    assert_eq!(b.low.to_bits(), 26980.00_f64.to_bits());
    assert_eq!(b.close.to_bits(), 27010.25_f64.to_bits());
    assert_eq!(b.volume.to_bits(), 12.345678_f64.to_bits());
    assert!(b.funding.is_none() && b.bid.is_none() && b.ask.is_none() && b.symbol.is_none());
}

/// ⚠ **The two binance futures books put the base asset in DIFFERENT columns**, and this is the
/// row pair that says so. Both rows below are the SAME hour, MEASURED from the live keyless
/// endpoints 2026-09-16 and pasted verbatim.
///
/// The arithmetic is the proof that index 7 is the base asset on the COIN-M row: 595,752
/// contracts x $100 = ~$59.5M, and 776.29 BTC at that bar's 76,885.9 close = ~$59.7M. Parsing a
/// COIN-M bar at index 5 lands the contract COUNT in `Bar::volume` — a ~767x unit error inside
/// one `kind=bar` schema.
#[test]
fn the_volume_column_is_the_one_the_book_puts_the_base_asset_in() {
    // COIN-M `BTCUSD_PERP`, hour 1789491600000: index 5 = contracts, index 7 = BTC.
    let coin_m = r#"[[1789491600000,"76307.2","77257.1","76112.1","76885.9","595752",1789495199999,"776.29174326",14111,"296451","386.45008562","0"]]"#;
    // USDⓈ-M `BTCUSDT`, the same hour: index 5 = BTC, index 7 = USDT.
    let usd_m = r#"[[1789491600000,"76352.30","77323.60","76151.30","76931.00","14205.232",1789495199999,"1090447633.34890",284720,"7502.716","575902349.00280","0"]]"#;

    assert_eq!(parse_klines_with(coin_m, VolumeColumn::Index7).unwrap()[0].volume, 776.291_743_26);
    assert_eq!(parse_klines_with(coin_m, VolumeColumn::Index5).unwrap()[0].volume, 595_752.0);
    let ratio = 595_752.0 / 776.291_743_26;
    assert!(ratio > 700.0 && ratio < 800.0, "the unit error this field exists to stop: {ratio}");

    // ...and the DEFAULT is unchanged for every book that is not COIN-M: `parse_klines` reads
    // index 5, which is where spot and USDⓈ-M put the base asset.
    assert_eq!(parse_klines(usd_m).unwrap()[0].volume, 14_205.232);
    assert_eq!(
        parse_klines(usd_m).unwrap()[0].volume,
        parse_klines_with(usd_m, VolumeColumn::Index5).unwrap()[0].volume,
        "`parse_klines` must be exactly the Index5 call — the public map is byte-identical"
    );
    assert_eq!((VolumeColumn::Index5.index(), VolumeColumn::Index7.index()), (5, 7));
    // Every other field is untouched by the column choice — only `volume` moves.
    let a = &parse_klines_with(coin_m, VolumeColumn::Index5).unwrap()[0];
    let b = &parse_klines_with(coin_m, VolumeColumn::Index7).unwrap()[0];
    assert_eq!(
        (a.ts, a.open.to_bits(), a.close.to_bits()),
        (b.ts, b.open.to_bits(), b.close.to_bits())
    );
}

#[test]
fn parse_klines_empty_array_is_empty() {
    assert!(parse_klines("[]").unwrap().is_empty());
}

#[test]
fn parse_klines_rejects_short_row() {
    // a row missing the close field must error, not panic.
    let body = r#"[[1700000000000,"1","2","3"]]"#;
    assert!(parse_klines(body).is_err());
}

/// The cross-run payoff, on the two real binance shapes: a stored sample seeds the pacer that
/// discovery just built, so the FIRST page is paced at last run's measurement instead of the
/// pessimistic constant — and a sample from the OTHER host is refused, which is what stops a
/// weight-2 spot record pacing the weight-5 fapi endpoint at 2.5x its target.
#[test]
fn a_persisted_sample_seeds_the_pacer_only_for_its_own_host() {
    let spot_spec = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
    let perp_spec = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
    let spot_record = PaceSample {
        request_ms: 120,
        per_request_weight: 2.0,
        budget_per_min: Some(6000),
        samples: 40,
    };
    let perp_record = PaceSample {
        request_ms: 280,
        per_request_weight: 5.0,
        budget_per_min: Some(2400),
        samples: 40,
    };

    // Matching host: applied, and the pace is immediately the corrected one.
    let mut perp = pacer_for(&perp_spec, Some(FAPI_EXCHANGE_INFO));
    assert!(perp.seed(&perp_record));
    assert!(
        (perp.next_delay().as_secs_f64() - 0.0325).abs() < 1e-6,
        "312.5ms target gap minus the 280ms the request is known to cost, on page ONE"
    );
    assert!(weight_per_minute(perp.next_delay() + Duration::from_millis(280), 5.0) <= 2400.0);

    // Cross-host: refused whole, and the pacer is left exactly as discovery built it.
    let mut cross = pacer_for(&perp_spec, Some(FAPI_EXCHANGE_INFO));
    assert!(!cross.seed(&spot_record), "the spot budget is not the fapi budget");
    assert_eq!(cross.next_delay(), pacer_for(&perp_spec, Some(FAPI_EXCHANGE_INFO)).next_delay());

    // The spot record does apply to the spot pacer — 6000/min at 40% is a 30ms gap at weight 2,
    // which the known 120ms request time floors, and the END-TO-END pace stays under 6000.
    let mut spot = pacer_for(&spot_spec, Some(SPOT_EXCHANGE_INFO));
    assert!(spot.seed(&spot_record));
    assert_eq!(spot.per_request_weight(), 2.0, "the measured cost, not the SEED_WEIGHT constant");
    assert!(
        weight_per_minute(spot.next_delay() + Duration::from_millis(120), 2.0) <= 6000.0,
        "a seeded pace must stay under the venue's published ceiling"
    );

    // And a DISCOVERY-LESS spec is unreachable by any discovered record, so the byte-identical
    // fallback path cannot be sped up by a stored file either.
    let mut none = pacer_for(&spec(150, None), None);
    assert!(!none.seed(&perp_record));
    assert_eq!(none.next_delay(), Duration::from_millis(150));
}

// ----- the walk, sequential vs concurrent ---------------------------------------------------

/// A synthetic binance klines endpoint over a KNOWN set of available bar timestamps.
///
/// It reproduces the wire semantics the walk depends on, and nothing else:
/// `startTime`/`endTime` are a FILTER (not a grid), the response is the first `rows_per_page`
/// available rows inside `[cursor, end]`, ascending. That filter semantics is why a short page
/// genuinely means "the window is exhausted" even across a gap in the venue's own history — the
/// property the whole sequential-vs-concurrent equivalence rests on.
struct SyntheticVenue {
    /// Every timestamp this "venue" has history for, ascending.
    ticks: Vec<i64>,
    rows_per_page: usize,
    /// Every `(cursor, end)` this venue was asked for — the request-sequence evidence.
    asked: std::sync::Mutex<Vec<(i64, i64)>>,
    /// Fail any page whose `[cursor, end]` CONTAINS this tick. Keyed on the tick rather than on
    /// the cursor so the same fixture fails deterministically in both paths (the sequential walk
    /// and whichever lane owns that tick), and in exactly ONE lane.
    fail_tick: Option<i64>,
    /// Per-page wall-clock cost, so an abort has something to land in. `ZERO` everywhere except
    /// the abort test, which is the only one that cares about wall-clock interleaving.
    page_cost: Duration,
}

impl SyntheticVenue {
    fn new(ticks: Vec<i64>, rows_per_page: usize) -> Self {
        SyntheticVenue {
            ticks,
            rows_per_page,
            asked: std::sync::Mutex::new(Vec::new()),
            fail_tick: None,
            page_cost: Duration::ZERO,
        }
    }

    fn page(&self, cursor: i64, end: i64) -> Result<Vec<Bar>, String> {
        self.asked.lock().unwrap().push((cursor, end));
        if self.page_cost > Duration::ZERO {
            std::thread::sleep(self.page_cost);
        }
        if self.fail_tick.is_some_and(|t| cursor <= t && t <= end) {
            return Err(format!("synthetic venue failed over [{cursor}, {end}]"));
        }
        Ok(self
            .ticks
            .iter()
            .copied()
            .filter(|&t| cursor <= t && t <= end)
            .take(self.rows_per_page)
            .map(|t| kline_to_bar(t, 1.0, 2.0, 0.5, 1.5, 10.0))
            .collect())
    }

    fn requests(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

/// The four history SHAPES that could make a span split disagree with cursor chaining, plus the
/// dense one. Each is `(name, ticks)` over a 1m grid.
fn history_shapes() -> Vec<(&'static str, Vec<i64>)> {
    let step = 60_000i64;
    let dense: Vec<i64> = (0..250).map(|i| i * step).collect();
    // Listed late: nothing before minute 180 (a symbol whose history starts inside the window).
    let leading_gap: Vec<i64> = (180..250).map(|i| i * step).collect();
    // A maintenance/delisting hole in the middle, wider than several pages.
    let interior_gap: Vec<i64> = (0..60).chain(140..250).map(|i: i64| i * step).collect();
    // History runs out well before the requested end.
    let trailing_gap: Vec<i64> = (0..70).map(|i| i * step).collect();
    // Every third bar only — a venue that serves no rows for illiquid minutes.
    let sparse: Vec<i64> = (0..250).filter(|i| i % 3 == 0).map(|i| i * step).collect();
    vec![
        ("dense", dense),
        ("leading gap", leading_gap),
        ("interior gap", interior_gap),
        ("trailing gap", trailing_gap),
        ("sparse", sparse),
        ("empty", Vec::new()),
    ]
}

/// THE load-bearing test: a synthetic multi-window fetch returns the SAME `Vec<Bar>` as the
/// sequential path — same order, same dedup, same clipping — for every lane count and every
/// history shape that could make the two disagree.
///
/// It is what makes the concurrent pager a refactor rather than a second implementation, and it
/// is deliberately asserted on the BARS rather than on request counts: a lane split legitimately
/// costs a few extra boundary pages (each span's last page is short), so equality of requests
/// would be false while equality of RESULT is the contract.
#[test]
fn lanes_return_the_same_bars_as_the_sequential_walk() {
    let rows_per_page = 10usize;
    let (start, end) = (0i64, 250 * 60_000i64);
    for (name, ticks) in history_shapes() {
        let seq_venue = SyntheticVenue::new(ticks.clone(), rows_per_page);
        let sequential =
            walk_forward_pages(start, end, rows_per_page, |c, e| seq_venue.page(c, e)).unwrap();

        for lanes in 1..=8usize {
            let venue = SyntheticVenue::new(ticks.clone(), rows_per_page);
            let concurrent =
                walk_forward_spans(start, end, rows_per_page, lanes, |c, e| venue.page(c, e))
                    .unwrap();
            assert_eq!(
                concurrent.iter().map(|b| b.ts).collect::<Vec<_>>(),
                sequential.iter().map(|b| b.ts).collect::<Vec<_>>(),
                "{name} @ {lanes} lanes: the concurrent walk must return the sequential result"
            );
            // ...and every field, bit for bit — not just the timestamps.
            for (c, s) in concurrent.iter().zip(sequential.iter()) {
                assert_eq!(c.open.to_bits(), s.open.to_bits());
                assert_eq!(c.high.to_bits(), s.high.to_bits());
                assert_eq!(c.low.to_bits(), s.low.to_bits());
                assert_eq!(c.close.to_bits(), s.close.to_bits());
                assert_eq!(c.volume.to_bits(), s.volume.to_bits());
            }
            // The contract the store depends on: ascending, unique, inside the window.
            assert!(
                concurrent.windows(2).all(|w| w[0].ts < w[1].ts),
                "{name} @ {lanes}: ascending and deduped"
            );
            assert!(concurrent.iter().all(|b| start <= b.ts && b.ts <= end));
        }
    }
}

/// ONE lane is the sequential walk — not "equivalent to" it: `split_range(_, _, 1)` is one span,
/// which `run_lanes` runs INLINE, so the request sequence is identical too, not merely the rows.
#[test]
fn one_lane_issues_exactly_the_sequential_requests() {
    let rows_per_page = 10usize;
    let (start, end) = (0i64, 250 * 60_000i64);
    for (name, ticks) in history_shapes() {
        let seq = SyntheticVenue::new(ticks.clone(), rows_per_page);
        let sequential =
            walk_forward_pages(start, end, rows_per_page, |c, e| seq.page(c, e)).unwrap();
        let one = SyntheticVenue::new(ticks, rows_per_page);
        let lane = walk_forward_spans(start, end, rows_per_page, 1, |c, e| one.page(c, e)).unwrap();

        assert_eq!(
            lane.iter().map(|b| b.ts).collect::<Vec<_>>(),
            sequential.iter().map(|b| b.ts).collect::<Vec<_>>(),
            "{name}: one lane must be byte-identical in RESULT"
        );
        assert_eq!(
            *one.asked.lock().unwrap(),
            *seq.asked.lock().unwrap(),
            "{name}: ...and in the exact sequence of (cursor, end) pairs it asked the venue for"
        );
    }
}

/// A failed window must NOT silently shrink the result. The whole fetch fails, because these
/// bars are written under ONE commit key naming the whole window — a partial reported as success
/// marks that window permanently ingested and the missing rows are never fetched again.
#[test]
fn one_failed_window_fails_the_whole_fetch_rather_than_shrinking_it() {
    let rows_per_page = 10usize;
    let (start, end) = (0i64, 250 * 60_000i64);
    let ticks: Vec<i64> = (0..250).map(|i| i * 60_000).collect();

    // A tick in the middle of the window — inside the span lane 2 of 4 owns, and reached by the
    // sequential walk too, so both paths fail on the SAME fixture for the same reason.
    let broken = 130 * 60_000i64;

    let mut venue = SyntheticVenue::new(ticks.clone(), rows_per_page);
    venue.fail_tick = Some(broken);
    let err = walk_forward_spans(start, end, rows_per_page, 4, |c, e| venue.page(c, e))
        .expect_err("a failed window must fail the fetch, never return the other lanes' rows");
    assert!(err.contains("synthetic venue failed"), "{err}");

    // The failure policy is not concurrency-specific — it is the sequential loop's `?`.
    let mut seq = SyntheticVenue::new(ticks, rows_per_page);
    seq.fail_tick = Some(broken);
    assert!(walk_forward_pages(start, end, rows_per_page, |c, e| seq.page(c, e)).is_err());
}

/// A failure STOPS the sibling lanes instead of letting them page the window out. Bounded, not
/// instant: a blocking request cannot be cancelled, so each lane pays at most one more page.
#[test]
fn a_failed_window_stops_the_sibling_lanes() {
    let rows_per_page = 10usize;
    let (start, end) = (0i64, 5_000 * 60_000i64);
    let ticks: Vec<i64> = (0..5_000).map(|i| i * 60_000).collect();
    let mut venue = SyntheticVenue::new(ticks, rows_per_page);
    // Tick 0 lives only in lane 0's span, and only in its FIRST page — so exactly one lane
    // fails, immediately, and the other three are still paging when it does.
    venue.fail_tick = Some(0);
    venue.page_cost = Duration::from_micros(300);
    assert!(walk_forward_spans(start, end, rows_per_page, 4, |c, e| venue.page(c, e)).is_err());
    // Run to completion the four lanes would need ~500 pages between them. The abort check caps
    // it far below that; the exact count is scheduler-dependent, the order of magnitude is not.
    assert!(
        venue.requests() < 200,
        "the siblings must stop early, not page the window out: {} requests",
        venue.requests()
    );
}

/// The walk's three stop conditions, pinned directly — an EMPTY page, a SHORT page, and a cursor
/// that cannot move forward. The third is the infinite-loop guard: a venue answering with a bar
/// at or before the cursor must terminate the walk, not spin.
#[test]
fn the_forward_walk_stops_on_empty_short_and_non_advancing_pages() {
    // Empty: one request, no rows.
    let empty = SyntheticVenue::new(Vec::new(), 10);
    assert!(walk_forward_pages(0, 1_000_000, 10, |c, e| empty.page(c, e)).unwrap().is_empty());
    assert_eq!(empty.requests(), 1, "an empty page must not be retried");

    // Short: the page is smaller than the cap ⇒ the window is exhausted, one request.
    let short = SyntheticVenue::new(vec![0, 60_000, 120_000], 10);
    assert_eq!(walk_forward_pages(0, 1_000_000, 10, |c, e| short.page(c, e)).unwrap().len(), 3);
    assert_eq!(short.requests(), 1);

    // Non-advancing: a venue that keeps answering with the SAME full page must TERMINATE rather
    // than loop forever. The cap is 2, so every page is full and the short-page rule cannot
    // help — only the cursor guard can.
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let out = walk_forward_pages(0, 1_000_000, 2, |_, _| {
        calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(vec![kline_to_bar(0, 1.0, 1.0, 1.0, 1.0, 0.0), kline_to_bar(1, 1.0, 1.0, 1.0, 1.0, 0.0)])
    })
    .unwrap();
    // Page 1 advances the cursor 0 -> 2; page 2 answers the same rows, so `next` (2) is not
    // past the cursor (2) and the walk stops. TWO requests, and the repeated rows come back
    // duplicated — which the sequential path has always done and `walk_forward_spans` dedups.
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2, "must not spin");
    assert_eq!(out.len(), 4);
}

/// Bars OUTSIDE the requested window are clipped, in both paths — a venue that over-serves must
/// not widen the result, and the span split must not let a boundary bar in twice.
#[test]
fn both_paths_clip_to_the_requested_window_and_never_double_count_a_boundary() {
    let step = 60_000i64;
    let ticks: Vec<i64> = (0..250).map(|i| i * step).collect();
    // A window that starts and ends INSIDE the available history, on non-page boundaries.
    let (start, end) = (37 * step, 191 * step);
    let expected: Vec<i64> = ticks.iter().copied().filter(|&t| start <= t && t <= end).collect();

    let seq = SyntheticVenue::new(ticks.clone(), 10);
    let sequential = walk_forward_pages(start, end, 10, |c, e| seq.page(c, e)).unwrap();
    assert_eq!(sequential.iter().map(|b| b.ts).collect::<Vec<_>>(), expected);

    for lanes in 1..=8usize {
        let v = SyntheticVenue::new(ticks.clone(), 10);
        let got = walk_forward_spans(start, end, 10, lanes, |c, e| v.page(c, e)).unwrap();
        assert_eq!(
            got.iter().map(|b| b.ts).collect::<Vec<_>>(),
            expected,
            "{lanes} lanes must clip to [{start}, {end}] exactly"
        );
    }
}

/// The lane count this rung will actually derive on the two REAL binance shapes — the finding
/// the whole change rests on, asserted here (against the same fixtures the pacing tests use)
/// rather than left to the pacer's own unit tests.
#[test]
fn the_derived_lane_count_splits_binances_two_hosts() {
    let rtt = Duration::from_millis(280); // MEASURED, pooled agent, the CI box

    // fapi: a 312ms target gap is WIDER than the round trip ⇒ one lane, nothing changes.
    let perp = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
    let mut p = pacer_for(&perp, Some(FAPI_EXCHANGE_INFO));
    p.observe_request(Some(100), rtt);
    p.observe_request(Some(105), rtt); // weight 5
    assert_eq!(p.suggested_lanes(8), 1, "perp is budget-bound, not latency-bound");

    // spot: a 50ms target gap against the same round trip ⇒ 6 lanes, and the sequential sleep is
    // already floored, which is why no delay tuning could have closed this.
    let spot = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
    let mut s = pacer_for(&spot, Some(SPOT_EXCHANGE_INFO));
    s.observe_request(Some(10), rtt);
    s.observe_request(Some(12), rtt); // weight 2
    assert_eq!(s.suggested_lanes(8), 6);
    assert!(s.next_delay() <= Duration::from_millis(1), "the sleep had nothing left to give");
    // And the aggregate the gate will pace at is still 40% of the published 6000, not 6x it.
    assert!(
        weight_per_minute(s.target_gap(), 2.0) <= 6000.0 * 0.4 + 1.0,
        "the target rate is unchanged by concurrency: {:?}",
        s.target_gap()
    );

    // A venue that publishes NO budget gets one lane whatever is measured — aster's shape.
    let none = spec(150, None);
    let mut n = pacer_for(&none, None);
    n.observe_request(None, Duration::from_millis(480));
    assert_eq!(n.suggested_lanes(8), 1, "no discovered ceiling ⇒ no aggregate ⇒ no lanes");
}

/// THE cold-run gate for [`PROBE_PAGES`]. One probe page records the counter but yields no
/// DELTA — there is nothing to subtract from — so `per_request` is still the pacer's pessimistic
/// seed of 5, and spot's own weight-2 endpoint is paced as though every page cost 5.
///
/// FAILS at `PROBE_PAGES = 1`: the derived count is 3 on a 125 ms gap, not 6 on 50 ms. That is
/// not merely three fewer lanes — the gap is twice too wide, so the run also spends 40 % of the
/// 40 % of the venue's budget that was actually asked for.
#[test]
fn a_cold_run_needs_two_probe_pages_to_derive_the_venues_own_weight() {
    let rtt = Duration::from_millis(280); // MEASURED, pooled agent, the CI box
    let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));

    // ONE probe page — what a single-probe implementation would hand `suggested_lanes`.
    let mut one = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
    one.observe_request(Some(10), rtt);
    assert_eq!(one.per_request_weight(), 5.0, "one reading is a baseline, not a delta");
    assert!((one.target_gap().as_secs_f64() - 0.125).abs() < 1e-6, "{:?}", one.target_gap());
    assert_eq!(one.suggested_lanes(8), 3, "ceil(280/125) — the seed's lane count, not the venue's");

    // TWO — what `PROBE_PAGES` actually walks. The second reading is the measurement.
    let mut two = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
    two.observe_request(Some(10), rtt);
    two.observe_request(Some(12), rtt); // `/api/v3/klines?limit=1000` costs weight 2
    assert_eq!(two.per_request_weight(), 2.0, "the venue's own cost");
    assert!((two.target_gap().as_secs_f64() - 0.050).abs() < 1e-6, "{:?}", two.target_gap());
    assert_eq!(two.suggested_lanes(8), 6, "ceil(280/50)");
    // ...and the aggregate the gate will run at is still 40% of the published 6000, not 6x it.
    assert!(weight_per_minute(two.target_gap(), 2.0) <= 6000.0 * DEFAULT_UTILIZATION + 1.0);

    // (`PROBE_PAGES >= 2` is enforced beside the const itself, as a `const _: ()` — a compile
    // error rather than a test failure, because a build that got it wrong should not link.)

    // fapi is 1 either way, so the second page costs the perp path nothing but a page it was
    // going to fetch regardless.
    let f = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
    let mut one_perp = pacer_for(&f, Some(FAPI_EXCHANGE_INFO));
    one_perp.observe_request(Some(100), rtt);
    assert_eq!(one_perp.suggested_lanes(8), 1);
    let mut two_perp = pacer_for(&f, Some(FAPI_EXCHANGE_INFO));
    two_perp.observe_request(Some(100), rtt);
    two_perp.observe_request(Some(105), rtt);
    assert_eq!(two_perp.suggested_lanes(8), 1);
}

/// The probe phase's PURPOSE, driven end to end without a socket: after [`PROBE_PAGES`] pages
/// fed through a pacer exactly the way `fetch_klines_range_lanes` feeds them, the pacer knows the
/// VENUE's per-request weight and therefore derives the venue's lane count.
///
/// This is the behavioural half of the `PROBE_PAGES >= 2` gate (the arithmetic half is the
/// `const _: ()` beside the const). FAILS at `PROBE_PAGES = 1`: one page leaves `per_request` at
/// the pacer's seed of 5, the gap at 125 ms and the derivation at 3 lanes.
#[test]
fn the_probe_phase_teaches_the_pacer_the_venues_own_weight() {
    let s = spec(50, Some("https://api.binance.com/api/v3/exchangeInfo"));
    let mut pacer = pacer_for(&s, Some(SPOT_EXCHANGE_INFO));
    let ticks: Vec<i64> = (0..250).map(|i| i * 60_000).collect();
    let venue = SyntheticVenue::new(ticks, 10);
    // The venue's counter advancing by the REAL weight-2 page cost, with exactly one request
    // outstanding — the only condition under which a delta is a per-request cost at all.
    let mut counter = 100u64;
    let (_bars, resume) = probe_pages(0, 250 * 60_000, 10, PROBE_PAGES, |c, e| {
        let page = venue.page(c, e)?;
        counter += 2;
        pacer.observe_request(Some(counter), Duration::from_millis(280));
        Ok(page)
    })
    .unwrap();

    assert!(resume.is_some(), "the window is longer than the probe budget");
    assert_eq!(venue.requests(), PROBE_PAGES, "the probe stops at its budget");
    assert_eq!(pacer.per_request_weight(), 2.0, "the VENUE's cost, not the pacer's seed of 5");
    assert!((pacer.target_gap().as_secs_f64() - 0.050).abs() < 1e-6);
    assert_eq!(pacer.suggested_lanes(8), 6, "which is what makes six lanes the derived answer");
}

/// `probe_pages` is [`walk_forward_pages`] with a page budget and a resume cursor — pinned
/// against it directly, so the probe phase cannot drift from the walk it hands off to.
#[test]
fn an_unbounded_probe_is_the_sequential_walk() {
    let rows = 10usize;
    let (start, end) = (0i64, 250 * 60_000i64);
    for (name, ticks) in history_shapes() {
        let seq = SyntheticVenue::new(ticks.clone(), rows);
        let walked = walk_forward_pages(start, end, rows, |c, e| seq.page(c, e)).unwrap();
        let probed_venue = SyntheticVenue::new(ticks, rows);
        let (probed, resume) =
            probe_pages(start, end, rows, usize::MAX, |c, e| probed_venue.page(c, e)).unwrap();
        assert_eq!(
            probed.iter().map(|b| b.ts).collect::<Vec<_>>(),
            walked.iter().map(|b| b.ts).collect::<Vec<_>>(),
            "{name}: an unbounded probe must be the sequential walk"
        );
        assert_eq!(resume, None, "{name}: ...which always exhausts the window");
        assert_eq!(*probed_venue.asked.lock().unwrap(), *seq.asked.lock().unwrap());
    }
}

/// The hand-off: after `PROBE_PAGES` the probe stops and names the cursor the LANES resume from,
/// and probe+lanes together are the sequential walk — the same equality the one-lane test makes,
/// now across the real two-phase shape the network function runs.
#[test]
fn the_probe_hands_the_lanes_a_cursor_that_reproduces_the_sequential_walk() {
    let rows = 10usize;
    let (start, end) = (0i64, 250 * 60_000i64);
    for (name, ticks) in history_shapes() {
        let seq = SyntheticVenue::new(ticks.clone(), rows);
        let sequential = walk_forward_pages(start, end, rows, |c, e| seq.page(c, e)).unwrap();

        for lanes in 1..=8usize {
            let venue = SyntheticVenue::new(ticks.clone(), rows);
            let (mut out, resume) =
                probe_pages(start, end, rows, PROBE_PAGES, |c, e| venue.page(c, e)).unwrap();
            assert!(venue.requests() <= PROBE_PAGES, "{name}: the probe must stop at its budget");
            if let Some(cursor) = resume {
                assert!(cursor > start && cursor <= end, "{name}: a usable resume cursor");
                let rest =
                    walk_forward_spans(cursor, end, rows, lanes, |c, e| venue.page(c, e)).unwrap();
                out.extend(rest);
                out.sort_by_key(|b| b.ts);
                out.dedup_by_key(|b| b.ts);
            }
            assert_eq!(
                out.iter().map(|b| b.ts).collect::<Vec<_>>(),
                sequential.iter().map(|b| b.ts).collect::<Vec<_>>(),
                "{name} @ {lanes} lanes: probe + lanes must equal the sequential walk"
            );
        }
    }
}

/// The probe's own stop conditions are the walk's three, and it declines to hand over a cursor
/// for a window it has already exhausted — the guard that stops a lane being spawned for nothing.
#[test]
fn the_probe_reports_no_resume_cursor_for_an_exhausted_window() {
    let rows = 10usize;
    // Empty page.
    let empty = SyntheticVenue::new(Vec::new(), rows);
    assert_eq!(probe_pages(0, 1_000_000, rows, 2, |c, e| empty.page(c, e)).unwrap().1, None);
    assert_eq!(empty.requests(), 1, "and it stops asking");

    // Short page — the window is exhausted inside the budget.
    let short = SyntheticVenue::new(vec![0, 60_000], rows);
    let (bars, resume) = probe_pages(0, 1_000_000, rows, 2, |c, e| short.page(c, e)).unwrap();
    assert_eq!(bars.len(), 2);
    assert_eq!(resume, None);
    assert_eq!(short.requests(), 1);

    // Exactly PROBE_PAGES full pages with more to come ⇒ a resume cursor for the lanes.
    let ticks: Vec<i64> = (0..250).map(|i| i * 60_000).collect();
    let more = SyntheticVenue::new(ticks, rows);
    let (bars, resume) = probe_pages(0, 250 * 60_000, rows, 2, |c, e| more.page(c, e)).unwrap();
    assert_eq!(bars.len(), 20, "two full pages");
    assert_eq!(resume, Some(19 * 60_000 + 1), "one ms past the second page's last openTime");
    assert_eq!(more.requests(), 2);

    // An empty window is never even asked about.
    let never = SyntheticVenue::new(vec![0], rows);
    assert_eq!(probe_pages(100, 0, rows, 2, |c, e| never.page(c, e)).unwrap(), (vec![], None));
    assert_eq!(never.requests(), 0);
}

/// The RUNTIME discovery miss — a spec that names an `exchangeInfo` URL but does not get a usable
/// body this run. `fetch_klines_range_lanes` tests `is_discovered()` and hands the fetch to the
/// sequential `paged_walk` before a gate exists, because a `Fixed` pacer's `target_gap` IS the
/// hand-set `page_delay` and pacing a shared gate by it would space requests ~1.5x closer than
/// the sequential pager does — against a venue whose ceiling we just failed to read.
#[test]
fn a_runtime_discovery_miss_is_not_a_one_lane_gate() {
    let s = spec(150, Some("https://fapi.binance.com/fapi/v1/exchangeInfo"));
    for body in [None, Some(""), Some("not json"), Some(r#"{"rateLimits":[]}"#)] {
        let mut p = pacer_for(&s, body);
        assert!(!p.is_discovered(), "body {body:?} must not look discovered");
        p.observe_request(None, Duration::from_millis(280));
        // The gate WOULD have paced at the bare page_delay (150ms) where the sequential pager
        // paces at page_delay + request (430ms) — which is why the branch is on `is_discovered`
        // and not on the lane count.
        assert_eq!(p.target_gap(), s.rate_limit.page_delay);
        assert_eq!(p.next_delay(), s.rate_limit.page_delay);
        assert_eq!(p.suggested_lanes(8), 1);
    }
}

#[test]
fn klines_url_includes_optional_bounds() {
    let latest = klines_url(BASE, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(latest, "https://host/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000");
    let ranged = klines_url(BASE, "BTCUSDT", "1m", Some(10), Some(20), 1000);
    assert!(ranged.ends_with("&startTime=10&endTime=20"));
}
