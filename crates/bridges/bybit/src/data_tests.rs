use super::*;

use crate::instruments::PerpBook;
use vike_model::AssetClass;

/// What the venue's listings said, spelled at the call site so each routing test declares the
/// FACTS it is routing on rather than inheriting a default.
fn listed(bare_is_ambiguous: bool, perp_book: PerpBook) -> Listed {
    Listed { bare_is_ambiguous, perp_book }
}

/// The FETCHER's own routing decision — the gate the URL-shaping tests cannot provide, because
/// they build a URL from `rest_category` directly and that helper was already perp-capable
/// before this fix. Re-pinning `fetch_klines_range` to `CATEGORY` fails HERE.
///
/// ⚠ The `.P` suffix says PERPETUAL. WHICH perpetual book is the venue's answer, injected here
/// as [`PerpBook`] — see [`route_target`]'s ⚠.
#[test]
fn range_target_routes_a_perp_symbol_to_linear_with_the_suffix_stripped() {
    assert_eq!(
        route_target("BTCUSDT.P", None, listed(false, PerpBook::Linear)).unwrap(),
        (Category::Linear, "BTCUSDT")
    );
}

/// **THE PHASE-4 ROUTE, and the one assertion the whole branch exists for.** A symbol the venue
/// lists on `category=inverse` reaches `category=inverse` — through a CLAIM, through the `.P`
/// suffix, and through both together. Nothing here reads the symbol's shape: flip the injected
/// [`PerpBook`] and the same three calls answer `linear`.
///
/// ⚠ This is the test the mutation proof targets. `rest_category` falling back to `"linear"`
/// for [`Category::Inverse`] reddens it, and so does `route_target` dropping arm 5.
#[test]
fn an_inverse_listed_perpetual_routes_to_the_inverse_category() {
    let inverse = listed(true, PerpBook::Inverse);
    assert_eq!(
        route_target("BTCUSD", Some(AssetClass::CryptoPerp), inverse).unwrap(),
        (Category::Inverse, "BTCUSD"),
        "a CryptoPerp claim on a symbol the venue lists inverse must reach the inverse book"
    );
    assert_eq!(
        route_target("BTCUSD.P", None, inverse).unwrap(),
        (Category::Inverse, "BTCUSD"),
        "the suffix says PERPETUAL and the VENUE says which perpetual book"
    );
    assert_eq!(
        route_target("BTCUSD.P", Some(AssetClass::CryptoPerp), inverse).unwrap(),
        (Category::Inverse, "BTCUSD")
    );
    // ...and the category the URL is actually built from.
    assert_eq!(Category::Inverse.wire(), "inverse");

    // The SAME symbol with the SAME claim, routed by a DIFFERENT venue listing. This is what
    // makes the assertion above about the listing rather than about `BTCUSD`.
    assert_eq!(
        route_target("BTCUSD", Some(AssetClass::CryptoPerp), listed(true, PerpBook::Linear))
            .unwrap(),
        (Category::Linear, "BTCUSD")
    );
}

/// ⚠ **26 of the venue's 28 inverse listings have no spot twin at all** (MEASURED 2026-09-16:
/// `spot ∩ inverse` = `{BTCUSD, ETHUSD}`), so for them there is nothing ambiguous and a caller
/// must not be asked to name anything. A bare, unclaimed, UNAMBIGUOUS symbol still routes to
/// spot — bybit lists no bare `XRPUSD` spot pair, so that is a venue-side rejection by name —
/// while the perpetual spellings reach the inverse book with no ceremony.
#[test]
fn an_inverse_only_symbol_needs_no_claim_to_reach_its_book() {
    let inverse_only = listed(false, PerpBook::Inverse);
    assert_eq!(
        route_target("XRPUSD.P", None, inverse_only).unwrap(),
        (Category::Inverse, "XRPUSD")
    );
    assert_eq!(
        route_target("XRPUSD", Some(AssetClass::CryptoPerp), inverse_only).unwrap(),
        (Category::Inverse, "XRPUSD")
    );
    assert_eq!(
        route_target("XRPUSD", None, inverse_only).unwrap(),
        (Category::Spot, "XRPUSD"),
        "a BARE symbol is still a spot claim: the perp book is only consulted for a perp"
    );
}

/// A symbol the venue lists under BOTH derivative categories is REFUSED, not guessed. Empty on
/// the live venue today — which is what makes this a gate on the measurement rather than a
/// restatement of it.
#[test]
fn a_symbol_in_both_derivative_books_is_refused() {
    let err = route_target("BOTHUSD.P", None, listed(false, PerpBook::Both))
        .expect_err("two derivative books, no claim that separates them");
    assert!(err.contains("BOTHUSD"), "the message must name the symbol: {err}");
    assert!(err.contains("category=linear") && err.contains("category=inverse"), "{err}");
    // A CLAIM does not rescue it either: `CryptoPerp` is what is already ambiguous here.
    assert!(
        route_target("BOTHUSD", Some(AssetClass::CryptoPerp), listed(false, PerpBook::Both))
            .is_err()
    );
}

/// A symbol the venue lists in NEITHER derivative category routes to `linear` — byte-identical
/// to this crate's behaviour before the lookup existed, so a typo keeps failing at the venue by
/// NAME rather than acquiring a new local failure mode. The variant is separate from
/// `PerpBook::Linear` so "never measured" is legible in a stack trace.
#[test]
fn an_unlisted_perpetual_keeps_todays_linear_route() {
    assert_eq!(
        route_target("NOSUCHUSDT.P", None, Listed::unconsulted()).unwrap(),
        (Category::Linear, "NOSUCHUSDT")
    );
}

/// A bare symbol the venue does NOT list under inverse still resolves the spot category and is
/// passed through untouched.
///
/// ⚠ **This is the 290-symbol case and it must never be refused.** MEASURED 2026-09-16: bybit
/// lists 290 symbols under both spot and LINEAR, and bare→spot is correct for every one of them
/// — the linear perp already has its own `.P` spelling. `crates/bridges/bybit/src/instruments.rs`
/// carries the predicate that keeps them out of the refusal.
#[test]
fn range_target_routes_a_bare_symbol_to_spot_unchanged() {
    assert_eq!(
        route_target("BTCUSDT", None, listed(false, PerpBook::Unlisted)).unwrap(),
        (Category::Spot, "BTCUSDT")
    );
}

/// **THE PHASE-0 REFUSAL, asserted with no network I/O at all** — `bare_is_ambiguous` is the
/// injected half, which is exactly what `range_target`'s extraction exists for.
///
/// The MEASURED case: bybit lists `BTCUSD` as a Trading spot pair and a Trading inverse
/// perpetual, the spot tape is vestigial, and before this the guess was silent.
#[test]
fn an_unclaimed_ambiguous_bare_symbol_is_refused_rather_than_guessed() {
    let err = route_target("BTCUSD", None, listed(true, PerpBook::Inverse))
        .expect_err("an ambiguous bare symbol with no claim must not route");
    assert!(err.contains("BTCUSD"), "the message must name the symbol: {err}");
    assert!(err.contains("INVERSE PERPETUAL"), "{err}");
    assert!(err.contains("CryptoSpot") && err.contains("CryptoPerp"), "{err}");
}

/// ⚠ **The refusal must not recommend the suffix as the way to reach an inverse book.** That
/// spelling works today by the venue's leniency plus `USD` rather than `USDT` being the quote
/// asset — the implicit encoding 0061 exists to remove — so the message says what `.P` actually
/// means and points at the CLASS instead. This test is what stops a later "helpful" edit turning
/// the signpost back into the bug.
#[test]
fn the_refusal_does_not_sell_the_perp_suffix_as_an_inverse_selector() {
    let err = route_target("BTCUSD", None, listed(true, PerpBook::Inverse)).expect_err("refused");
    assert!(
        err.contains("says PERPETUAL, not inverse"),
        "the message must correct the suffix's meaning rather than recommend it: {err}"
    );
    assert!(
        !err.contains("BTCUSD.P"),
        "naming the suffixed spelling as the answer recommends relying on quote-asset shape \
             plus venue leniency: {err}"
    );
}

/// **A CLAIM ends the ambiguity — there is nothing left to refuse.** The same symbol, the same
/// venue answer, routed both ways by what the caller named.
///
/// ⚠ The perp half now lands on `inverse` rather than `linear`, and that is the WHOLE phase-4
/// change in one line: before, a `CryptoPerp` claim on this symbol was served the right tape
/// only because bybit resolves a `category=linear` request for it leniently.
#[test]
fn a_claim_resolves_the_ambiguous_symbol_both_ways() {
    assert_eq!(
        route_target("BTCUSD", Some(AssetClass::CryptoPerp), listed(true, PerpBook::Inverse))
            .unwrap(),
        (Category::Inverse, "BTCUSD")
    );
    assert_eq!(
        route_target("BTCUSD", Some(AssetClass::CryptoSpot), listed(true, PerpBook::Inverse))
            .unwrap(),
        (Category::Spot, "BTCUSD")
    );
}

/// A class this venue's kline path cannot address is REFUSED rather than coerced onto a
/// category. `vike_catalog::addressing_for` is the authority, so this fails if that row shrinks.
#[test]
fn a_class_this_venue_cannot_address_is_refused() {
    for class in [AssetClass::Equity, AssetClass::Fx, AssetClass::Option] {
        let err = route_target("BTCUSDT", Some(class), Listed::unconsulted())
            .expect_err("a class this venue cannot address must be refused, not coerced");
        assert!(err.contains("addresses"), "{err}");
    }
    // ...and the two it CAN address are not refused.
    for class in [AssetClass::CryptoSpot, AssetClass::CryptoPerp] {
        assert!(route_target("BTCUSDT", Some(class), Listed::unconsulted()).is_ok(), "{class:?}");
    }
}

/// Two claims that disagree — a `.P` symbol claimed as spot — is refused rather than one of them
/// silently winning.
#[test]
fn a_claim_contradicting_the_suffix_is_refused() {
    let err = route_target("BTCUSDT.P", Some(AssetClass::CryptoSpot), Listed::unconsulted())
        .expect_err("a suffix and a spot claim disagree");
    assert!(err.contains("disagree"), "{err}");
    // ...and the agreeing combination routes.
    assert_eq!(
        route_target("BTCUSDT.P", Some(AssetClass::CryptoPerp), listed(false, PerpBook::Linear))
            .unwrap(),
        (Category::Linear, "BTCUSDT")
    );
}

/// The venue id this module looks its addressing row up by must be the one the catalog provider
/// answers with — two literals, pinned equal, because the row this routes on is keyed on it and
/// a typo would silently reach `VenueAddressing::UNCLASSIFIED`.
#[test]
fn the_venue_id_is_the_one_the_catalog_answers_with() {
    use vike_catalog::CatalogProvider;
    assert_eq!(VENUE, crate::catalog::BybitCatalog.venue());
    assert_ne!(
        vike_catalog::addressing_for(VENUE),
        vike_catalog::VenueAddressing::UNCLASSIFIED,
        "this venue must have a NAMED addressing row, or `must_claim` answers from the \
             refusing fallback rather than from a measurement"
    );
    assert!(
        vike_catalog::addressing_for(VENUE).must_claim(),
        "bybit's row is the measured ambiguity — if this flips, the instrument-list read below \
             stops happening and the refusal is dead code"
    );
}

/// A `.P` symbol and a claimed symbol are never refused for AMBIGUITY, which is how
/// `range_target` skips the spot-collision lookup entirely for them.
///
/// ⚠ **This test's scope shrank, and that is worth saying rather than quietly editing.** It used
/// to prove a `.P` symbol needed NO listing read at all. Phase 4 made a perpetual claim consult
/// the venue for its BOOK, so what survives is the narrower and still load-bearing claim: the
/// `bare_is_ambiguous` half changes nothing for a suffix or a claim. The cost of the other half
/// is argued at `crates/bridges/bybit/src/instruments.rs`'s "What it costs".
#[test]
fn a_suffix_or_a_claim_needs_no_ambiguity_answer() {
    // ⚠ **THE NO-REGRESSION ASSERTION for the one spelling that already worked.** `BTCUSD.P`
    // reached the perpetual's tape before any of this landed and must keep doing so — the
    // refusal above is scoped to the caller who named NOTHING, and a suffix is a name.
    //
    // What CHANGED under it is the reason: this used to resolve `category=linear` and be rescued
    // by the venue's leniency, and now it resolves `category=inverse` because the venue lists
    // the symbol there. The spelling is still not RECOMMENDED anywhere an operator reads — `.P`
    // says PERPETUAL, and the class (or the picker's perp row) is the way to mean one.
    let inverse = listed(true, PerpBook::Inverse);
    assert_eq!(route_target("BTCUSD.P", None, inverse).unwrap(), (Category::Inverse, "BTCUSD"));
    assert!(route_target("BTCUSD", Some(AssetClass::CryptoSpot), inverse).is_ok());
    // ...and the ambiguity flag genuinely changes nothing for either.
    assert_eq!(
        route_target("BTCUSD.P", None, listed(true, PerpBook::Inverse)).unwrap(),
        route_target("BTCUSD.P", None, listed(false, PerpBook::Inverse)).unwrap()
    );
    assert_eq!(
        route_target("BTCUSD", Some(AssetClass::CryptoSpot), listed(true, PerpBook::Inverse))
            .unwrap(),
        route_target("BTCUSD", Some(AssetClass::CryptoSpot), listed(false, PerpBook::Inverse))
            .unwrap()
    );
}

/// A previous run's record, shaped like the MEASURED sibling page (the CI box, 2026-08-04, okx:
/// ~480 ms). `budget_per_min: None` because this venue publishes none — which is also what
/// confines the record to another fallback pacer.
fn bybit_record(request_ms: u64) -> PaceSample {
    PaceSample { request_ms, per_request_weight: 1.0, budget_per_min: None, samples: 20 }
}

/// ⚠ THE load-bearing test of this change: the pager MEASURES and the delay does not move.
/// [`PAGE_DELAY`] is what the walk slept before the pacer existed, and it is bit-for-bit what
/// `next_delay()` answers after — unseeded, seeded, and after observations at every scale.
///
/// Asserted against the CONST rather than a literal `200`, so retuning the constant (a separate
/// change, with its own measurement) moves this test with it instead of leaving it stale.
#[test]
fn the_pager_measures_but_never_changes_the_page_delay() {
    let mut p = page_pacer(None);
    assert!(!p.is_discovered(), "bybit publishes no budget to discover");
    assert_eq!(p.next_delay(), PAGE_DELAY);
    assert_eq!(p.eta(1_000), None, "nothing timed ⇒ no ETA, not one guessed from the sleep");

    for _ in 0..4 {
        p.observe_request(None, Duration::from_millis(480));
    }
    assert_eq!(p.next_delay(), PAGE_DELAY, "the measured 480ms must not move the sleep");
    assert!(!p.should_cool_down(), "and no budget ⇒ no target to cross");

    // A seed is likewise ETA-only: it can never make this pager faster.
    let mut seeded = page_pacer(Some(&bybit_record(480)));
    assert_eq!(seeded.next_delay(), PAGE_DELAY);
    seeded.observe_request(Some(9_999), Duration::from_secs(30));
    assert_eq!(seeded.next_delay(), PAGE_DELAY, "nor can any observation, at any scale");
}

/// What the measurement BUYS: 30 days of 1m klines is 43 of Bybit's 1000-row pages, whose real
/// cost is ~20s of request time on top of 8.6s of sleep — a number the 200ms constant alone
/// could never have produced, and the one the pager's ETA line now prints after page one.
#[test]
fn a_measured_page_yields_the_eta_the_fixed_delay_could_not() {
    let mut p = page_pacer(None);
    p.observe_request(None, Duration::from_millis(480));
    let month = 30 * 86_400_000i64;
    let pages = remaining_pages(month, "1m", MAX_LIMIT).expect("1m parses");
    assert_eq!(pages, 43, "30 days of 1m klines at 1000 rows/page");
    let eta = p.eta(pages).expect("a timed page yields an ETA");
    // 43 x (480ms + 200ms) = 29.24s. The sleep-only view would have claimed 8.6s.
    assert!(
        eta > Duration::from_millis(29_000) && eta < Duration::from_millis(29_500),
        "expected ~29s for the measured shape, got {eta:?}"
    );
    // A SEEDED pacer answers the same before page one has even been fetched.
    assert_eq!(page_pacer(Some(&bybit_record(480))).eta(pages), Some(eta));
    // ...and this run, having measured, hands its own observation back out to be persisted.
    let m = p.measured().expect("a fixed pager still times its pages");
    assert_eq!(m.request_ms, 480);
    assert_eq!(m.budget_per_min, None, "nothing discovered ⇒ nothing claimed");
    assert!(m.is_usable());
}

/// A minimal newest-first V5 envelope with two klines (so the reversal is exercised).
const TWO_ROWS: &str = r#"{"retCode":0,"retMsg":"OK","result":{"category":"spot","symbol":"BTCUSDT","list":[
        ["1700000060000","27010.25","27100.00","27000.00","27080.10","8.10000000","219000.00"],
        ["1700000000000","27000.10","27050.50","26980.00","27010.25","12.34567800","333012.50"]
    ]},"retExtInfo":{},"time":1700000200000}"#;

#[test]
fn parse_reverses_to_ascending_and_maps_exactly() {
    let bars = parse_bybit_klines(TWO_ROWS).unwrap();
    assert_eq!(bars.len(), 2);
    // newest-first list → ascending bars: the LAST list row is bars[0].
    assert_eq!(bars[0].ts, 1_700_000_000_000);
    assert_eq!(bars[1].ts, 1_700_000_060_000);
    assert!(bars[0].ts < bars[1].ts, "ascending by startTime after reversal");
    assert_eq!(bars[0].open.to_bits(), 27000.10_f64.to_bits());
    assert_eq!(bars[0].high.to_bits(), 27050.50_f64.to_bits());
    assert_eq!(bars[0].low.to_bits(), 26980.00_f64.to_bits());
    assert_eq!(bars[0].close.to_bits(), 27010.25_f64.to_bits());
    assert_eq!(bars[0].volume.to_bits(), 12.345678_f64.to_bits());
    assert!(bars[0].symbol.is_none() && bars[0].funding.is_none());
}

#[test]
fn parse_empty_list_is_empty() {
    let body = r#"{"retCode":0,"retMsg":"OK","result":{"category":"spot","symbol":"BTCUSDT","list":[]},"retExtInfo":{},"time":1}"#;
    assert!(parse_bybit_klines(body).unwrap().is_empty());
}

#[test]
fn parse_nonzero_retcode_errors() {
    let body = r#"{"retCode":10001,"retMsg":"params error","result":{},"retExtInfo":{},"time":1}"#;
    assert!(parse_bybit_klines(body).is_err());
}

#[test]
fn parse_rejects_short_row() {
    let body =
        r#"{"retCode":0,"retMsg":"OK","result":{"list":[["1700000000000","1","2","3"]]},"time":1}"#;
    assert!(parse_bybit_klines(body).is_err());
}

#[test]
fn interval_code_maps_or_errors() {
    assert_eq!(interval_code("1m").unwrap(), "1");
    assert_eq!(interval_code("1h").unwrap(), "60");
    assert_eq!(interval_code("1d").unwrap(), "D");
    assert_eq!(interval_code("1M").unwrap(), "M");
    assert!(interval_code("7s").is_err());
}

#[test]
fn klines_url_has_category_and_bounds() {
    let url = klines_url(CATEGORY, "BTCUSDT", "1", 10, 20, 1000);
    assert_eq!(
        url,
        "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&start=10&end=20&limit=1000"
    );
}

/// The perp suffix selects a derivative category AND is stripped from the wire symbol. The
/// BOOK comes from the venue's listings (`route_target` arm 5), so both are asserted here.
#[test]
fn perp_suffix_selects_a_derivative_category_and_strips_the_wire_symbol() {
    let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
    assert_eq!(wire, "BTCUSDT");
    assert!(perp);
    let url = klines_url(Category::Linear.wire(), wire, "1", 10, 20, 1000);
    assert!(url.contains("category=linear"), "a linear-listed perp must use linear: {url}");
    assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
    assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
}

/// ⚠ **The third category, spelled all the way to the URL.** This is the assertion that fails if
/// [`rest_category`]'s `Inverse` arm is re-pinned to `"linear"` — the URL builder and the
/// routing decision share one enum, so neither can drift from the other.
#[test]
fn an_inverse_route_builds_an_inverse_url() {
    let (wire, perp) = vike_catalog::split_perp("BTCUSD.P");
    assert!(perp);
    let url = klines_url(Category::Inverse.wire(), wire, "1", 10, 20, 1000);
    assert_eq!(
        url,
        "https://api.bybit.com/v5/market/kline?category=inverse&symbol=BTCUSD&interval=1&start=10&end=20&limit=1000"
    );
    assert_eq!(Category::Inverse.wire(), "inverse");
    assert_ne!(
        Category::Inverse.wire(),
        Category::Linear.wire(),
        "the two derivative books must not collapse to one category string"
    );
}

/// A bare symbol is unchanged — spot URLs stay byte-identical to the pre-change form.
#[test]
fn a_bare_symbol_still_builds_the_spot_url() {
    let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
    assert!(!perp);
    assert_eq!(
        klines_url(Category::Spot.wire(), wire, "1", 10, 20, 1000),
        "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&start=10&end=20&limit=1000"
    );
}

// ---- the end-anchored truncation trap (module doc, point 4) --------------------------------

const MIN: i64 = 60_000;
const T0: i64 = 1_700_000_000_000;
/// 2.5 pages at the 1000-row cap — enough that a single-page test would pass while the window
/// came back 60% empty.
const BARS: i64 = 2_500;

fn synthetic_bar(ts: i64) -> Bar {
    kline_to_bar(ts, 100.0, 101.0, 99.0, 100.5, 1.0)
}

/// One page from a fake venue that behaves EXACTLY like Bybit's `/v5/market/kline`: it **ignores
/// `start` entirely** and answers the newest [`MAX_LIMIT`] klines at or below `page_end`, out of
/// a synthetic series of [`BARS`] one-minute klines beginning at [`T0`].
///
/// Returned ASCENDING — i.e. already reversed, exactly what [`bars_from_envelope`] hands the
/// walk — so these tests cannot accidentally paper over a double-reverse. The newest-first RAW
/// order is covered separately by `the_walk_is_independent_of_wire_order_within_a_page`.
fn end_anchored_page(page_end: i64) -> Vec<Bar> {
    if page_end < T0 {
        return Vec::new(); // older than the series' first kline
    }
    let newest_i = ((page_end - T0) / MIN).min(BARS - 1);
    let oldest_i = (newest_i - MAX_LIMIT as i64 + 1).max(0);
    (oldest_i..=newest_i).map(|i| synthetic_bar(T0 + i * MIN)).collect()
}

/// THE load-bearing test: a multi-page window over a venue that reproduces the live failure.
///
/// The pre-fix forward pager walked `start` upward, so its FIRST request came back at the far
/// END of the window, the cursor jumped past `end_ms`, and the loop exited after one page —
/// MEASURED on the CI box 2026-08-04 as 1,001 rows for a 43,200-minute request. A single-page test
/// cannot see any of that; this one fails outright against the old pager.
#[test]
fn the_walk_goes_backward_and_returns_a_multi_page_window_whole() {
    let start = T0;
    let end = T0 + (BARS - 1) * MIN;

    // First prove the FAKE really is faithful: one end-anchored request for the whole window
    // answers only the newest cap-many rows, and its oldest row is nowhere near `start`.
    let naive = end_anchored_page(end);
    assert_eq!(naive.len(), MAX_LIMIT, "the venue truncates to its cap");
    assert_eq!(
        naive[0].ts,
        T0 + (BARS - MAX_LIMIT as i64) * MIN,
        "the newest page only — this is the truncation signature"
    );
    assert!(naive[0].ts > start, "the requested start is ignored; the older tail is dropped");

    let mut pages = 0usize;
    let bars = walk_backward_pages(start, end, |page_end| {
        pages += 1;
        assert!(pages < 100, "runaway walk");
        Ok(end_anchored_page(page_end))
    })
    .expect("walk");

    assert_eq!(pages, 3, "2500 klines at a 1000-row cap is three pages");
    assert!(bars.len() > MAX_LIMIT, "strictly more than one end-anchored page");
    assert_eq!(bars.len(), BARS as usize, "the window must come back WHOLE");
    assert_eq!(bars.first().expect("non-empty").ts, start, "reaches the requested start");
    assert_eq!(bars.last().expect("non-empty").ts, end, "reaches the requested end");
    assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "ascending and de-duplicated");
    for (i, b) in bars.iter().enumerate() {
        assert_eq!(b.ts, T0 + i as i64 * MIN, "contiguous — no gap at the page seams (i={i})");
    }
}

/// The double-reverse guard: [`next_page_step`] keys off the page MINIMUM, not its first or last
/// element, so the same window comes back identically whether pages arrive ascending (what
/// [`bars_from_envelope`] produces) or newest-first (Bybit's raw `result.list` order). An
/// implementation that read `page[0].ts` or `page.last()` would walk the wrong direction on one
/// of the two and fail here.
#[test]
fn the_walk_is_independent_of_wire_order_within_a_page() {
    let start = T0;
    let end = T0 + (BARS - 1) * MIN;
    let ascending =
        walk_backward_pages(start, end, |pe| Ok(end_anchored_page(pe))).expect("asc walk");
    let newest_first = walk_backward_pages(start, end, |pe| {
        let mut p = end_anchored_page(pe);
        p.reverse(); // Bybit's RAW result.list order, before `bars_from_envelope` reverses it
        Ok(p)
    })
    .expect("desc walk");

    assert_eq!(ascending.len(), BARS as usize);
    assert_eq!(ascending.len(), newest_first.len(), "same window either way");
    assert!(
        ascending.iter().zip(&newest_first).all(|(a, b)| a.ts == b.ts),
        "identical ts sequence regardless of intra-page wire order"
    );
    assert_eq!(newest_first.first().expect("non-empty").ts, start);
}

/// A venue that keeps answering the SAME newest block regardless of `end` must stop, not spin.
/// This is the guard the task calls for: termination cannot depend on the cursor advancing.
#[test]
fn a_non_advancing_cursor_stops_instead_of_looping_forever() {
    let mut pages = 0usize;
    let bars = walk_backward_pages(T0, T0 + 10 * MIN, |_page_end| {
        pages += 1;
        assert!(pages < 50, "the walk must not loop forever on a stuck cursor");
        // Always the same block, ignoring `page_end` entirely.
        Ok(vec![synthetic_bar(T0 + 9 * MIN), synthetic_bar(T0 + 10 * MIN)])
    })
    .expect("walk");
    assert_eq!(pages, 2, "one page, then one whose oldest failed to move — then stop");
    assert_eq!(bars.len(), 2, "deduped to the two distinct klines");
}

/// Cross-page overlap is deduped, out-of-window klines are clipped, and the result is sorted.
#[test]
fn pages_are_deduped_sorted_and_clipped_to_the_window() {
    let start = T0 + 2 * MIN;
    let end = T0 + 4 * MIN;
    let mut call = 0usize;
    let bars = walk_backward_pages(start, end, |_page_end| {
        call += 1;
        Ok(match call {
            // newest page: carries one kline ABOVE the window end
            1 => vec![
                synthetic_bar(T0 + 3 * MIN),
                synthetic_bar(T0 + 4 * MIN),
                synthetic_bar(T0 + 5 * MIN),
            ],
            // older page: overlaps T0+3MIN and reaches BELOW the window start
            2 => vec![
                synthetic_bar(T0 + MIN),
                synthetic_bar(T0 + 2 * MIN),
                synthetic_bar(T0 + 3 * MIN),
            ],
            _ => Vec::new(),
        })
    })
    .expect("walk");
    let ts: Vec<i64> = bars.iter().map(|b| b.ts).collect();
    assert_eq!(ts, vec![T0 + 2 * MIN, T0 + 3 * MIN, T0 + 4 * MIN]);
    assert_eq!(call, 2, "the second page reached the start and ended the walk");
}

#[test]
fn an_empty_page_terminates_the_walk_and_an_empty_window_never_asks() {
    let mut pages = 0usize;
    let bars = walk_backward_pages(T0, T0 + 10 * MIN, |_| {
        pages += 1;
        Ok(Vec::new())
    })
    .expect("walk");
    assert_eq!(pages, 1, "an empty page stops the walk immediately");
    assert!(bars.is_empty());

    let mut asked = false;
    let bars = walk_backward_pages(T0 + MIN, T0, |_| {
        asked = true;
        Ok(vec![synthetic_bar(T0)])
    })
    .expect("walk");
    assert!(!asked, "end < start must never touch the venue");
    assert!(bars.is_empty());
}

#[test]
fn a_page_error_aborts_the_walk() {
    let err = walk_backward_pages(T0, T0 + 10 * MIN, |_| Err("bybit klines HTTP 500".to_string()))
        .expect_err("the error must propagate, not silently truncate");
    assert!(err.contains("HTTP 500"), "{err}");
}

#[test]
fn next_page_step_decisions() {
    let end = T0 + 10 * MIN;
    // Normal: step to one ms below the page's OLDEST kline.
    assert_eq!(
        next_page_step(&[T0 + 5 * MIN, T0 + 6 * MIN], T0, end),
        PageStep::Next(T0 + 5 * MIN - 1)
    );
    // Wire order is irrelevant — the MINIMUM decides, not the first element.
    assert_eq!(
        next_page_step(&[T0 + 6 * MIN, T0 + 5 * MIN], T0, end),
        PageStep::Next(T0 + 5 * MIN - 1)
    );
    // The window start is covered (exactly, then overshot).
    assert_eq!(next_page_step(&[T0, T0 + MIN], T0, end), PageStep::Done);
    assert_eq!(next_page_step(&[T0 - MIN], T0, end), PageStep::Done);
    // Empty page.
    assert_eq!(next_page_step(&[], T0, end), PageStep::Done);
    // Klines NEWER than the `end` asked for cannot move the cursor backward.
    assert_eq!(next_page_step(&[end + MIN], T0, end), PageStep::Done);
    // A SHORT page is NOT a stop signal: termination is by observed ticks, never a page count.
    assert_eq!(next_page_step(&[T0 + 9 * MIN], T0, end), PageStep::Next(T0 + 9 * MIN - 1));
}

#[test]
fn latest_url_spells_every_category_the_live_feed_can_seed_from() {
    let perp = latest_url(Category::Linear.wire(), "BTCUSDT", "1", 1000);
    assert_eq!(
        perp,
        "https://api.bybit.com/v5/market/kline?category=linear&symbol=BTCUSDT&interval=1&limit=1000"
    );
    let spot = latest_url(Category::Spot.wire(), "BTCUSDT", "1", 1000);
    assert_eq!(
        spot,
        "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&limit=1000"
    );
    // ⚠ The seed the INVERSE live chart warms up from. It matters that this is the same
    // `Category` value `market_feed::ws_host` picks the socket with: a seed and a stream that
    // resolved the book independently could disagree, which is what the `bool` this parameter
    // used to be actually did.
    let inverse = latest_url(Category::Inverse.wire(), "BTCUSD", "1", 1000);
    assert_eq!(
        inverse,
        "https://api.bybit.com/v5/market/kline?category=inverse&symbol=BTCUSD&interval=1&limit=1000"
    );
}
