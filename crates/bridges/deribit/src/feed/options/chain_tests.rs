//! Ports `tests/unit/data/test_options_deribit.py` (captured payloads inline).

use super::*;

/// 2026-06-02 08:00 UTC — the oracle tests' `_ms(2026, 6, 2)`.
const NOW: i64 = 1_780_387_200_000;

fn summary() -> Vec<Value> {
    // two expiries (27 Jun + 25 Sep 2026); within 27 Jun: two strikes, call+put on the first
    serde_json::json!([
        {"instrument_name": "BTC-27JUN26-100000-C", "bid_price": 0.05, "ask_price": 0.06,
         "mark_price": 0.055, "mark_iv": 62.5, "open_interest": 120.0, "volume": 8.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-27JUN26-100000-P", "bid_price": 0.04, "ask_price": 0.05,
         "mark_price": 0.045, "mark_iv": 61.0, "open_interest": 90.0, "volume": 3.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-27JUN26-110000-C", "bid_price": 0.02, "ask_price": 0.03,
         "mark_price": 0.025, "mark_iv": 64.0, "open_interest": 50.0, "volume": 1.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-25SEP26-120000-C", "bid_price": 0.01, "ask_price": 0.02,
         "mark_price": 0.015, "mark_iv": 70.0, "open_interest": 10.0, "volume": 1.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-PERPETUAL", "mark_price": 104000.0}
    ])
    .as_array()
    .unwrap()
    .clone()
}

fn usdc_summary() -> Vec<Value> {
    // The shared USDC book mixes coins; SOL's chain must pick out only SOL_USDC rows.
    serde_json::json!([
        {"instrument_name": "SOL_USDC-26JUN26-90-P", "bid_price": 17.5, "ask_price": 18.0,
         "mark_price": 17.75, "mark_iv": 60.0, "open_interest": 40.0, "volume": 5.0,
         "underlying_price": 74.5},
        {"instrument_name": "SOL_USDC-26JUN26-90-C", "bid_price": 1.0, "ask_price": 1.2,
         "mark_price": 1.1, "mark_iv": 61.0, "open_interest": 30.0, "volume": 2.0,
         "underlying_price": 74.5},
        {"instrument_name": "BTC_USDC-31JUL26-115000-P", "bid_price": 5000.0, "ask_price": 5100.0,
         "mark_price": 5050.0, "mark_iv": 55.0, "underlying_price": 104000.0},
        {"instrument_name": "XRP_USDC-26JUN26-3-C", "bid_price": 0.1, "ask_price": 0.12,
         "mark_price": 0.11, "mark_iv": 70.0, "underlying_price": 2.4}
    ])
    .as_array()
    .unwrap()
    .clone()
}

#[test]
fn parses_instrument_names() {
    assert_eq!(
        parse_instrument_name("BTC-27JUN26-100000-C"),
        Some(("BTC".into(), "2026-06-27".into(), 100000.0, OptionKind::Call))
    );
    assert_eq!(parse_instrument_name("BTC-PERPETUAL"), None);
    assert_eq!(parse_instrument_name("BTC-27JUN26-100000-Z"), None);
    assert_eq!(parse_instrument_name("BTC-27XXX26-100000-C"), None); // non-month token
    // USDC-margined altcoin form: base carries a _USDC suffix, dropped -> "SOL"
    assert_eq!(
        parse_instrument_name("SOL_USDC-26JUN26-90-P"),
        Some(("SOL".into(), "2026-06-26".into(), 90.0, OptionKind::Put))
    );
    assert_eq!(
        parse_instrument_name("SOL_USDC-5JUN26-80-C"),
        Some(("SOL".into(), "2026-06-05".into(), 80.0, OptionKind::Call))
    );
}

/// A fractional strike in the venue's own form, a lower-case `d` as the decimal mark (`XRP_USDC-26SEP25-1d6-P`,
/// the trade log's name, measured 2026-11-16), is the strike it says; so is the `.` form. A malformed fraction
/// is no option name, and an upper-case `D` is not the venue's mark.
#[test]
fn a_fractional_strike_reads_in_the_venues_d_form() {
    assert_eq!(
        parse_instrument_name("XRP_USDC-26SEP25-1d6-P"),
        Some(("XRP".into(), "2025-09-26".into(), 1.6, OptionKind::Put))
    );
    assert_eq!(parse_instrument_name("XRP_USDC-27MAR26-2d55-C").map(|p| p.2), Some(2.55));
    assert_eq!(parse_instrument_name("XRP_USDC-27MAR26-2.5-C").map(|p| p.2), Some(2.5));
    for bad in [
        "XRP_USDC-27MAR26-2d-C",
        "XRP_USDC-27MAR26-d5-C",
        "XRP_USDC-27MAR26-2d5d1-C",
        "XRP_USDC-27MAR26-2D5-C",
    ] {
        assert_eq!(parse_instrument_name(bad), None, "{bad}");
    }
}

/// The LIVE chain keeps an option whose strike has a fraction: its book-summary row groups under its
/// expiry with the strike it names, where the parser used to drop it.
#[test]
fn the_live_chain_keeps_a_fractional_strike() {
    let rows = serde_json::json!([
        {"instrument_name": "XRP_USDC-26JUN26-2d5-C", "bid_price": 0.1, "ask_price": 0.12,
         "mark_price": 0.11, "mark_iv": 70.0, "underlying_price": 2.4},
        {"instrument_name": "XRP_USDC-26JUN26-3-C", "bid_price": 0.05, "ask_price": 0.06,
         "mark_price": 0.055, "mark_iv": 72.0, "underlying_price": 2.4}
    ]);
    let rows = rows.as_array().unwrap();
    let expiries = list_expiries_from_summary(rows, 1_780_000_000_000, Some("XRP"));
    assert_eq!(expiries.len(), 1);
    let chain = build_chain_from_summary("XRP", rows, "2026-06-26", 1_780_000_000_000, true, 0.0);
    let strikes: Vec<f64> = chain.rows.iter().map(|s| s.strike).collect();
    assert_eq!(strikes, [2.5, 3.0]);
}

/// A day the calendar does not have is no option name; a leap day is one (2000 is a leap year, 2026 is
/// not), and so is each month's last day.
#[test]
fn an_expiry_the_calendar_does_not_have_is_refused() {
    for (name, date) in [
        ("BTC-29FEB28-100000-C", Some("2028-02-29")),
        ("BTC-29FEB00-100000-C", Some("2000-02-29")),
        ("BTC-28FEB26-100000-C", Some("2026-02-28")),
        ("BTC-30APR26-100000-C", Some("2026-04-30")),
        ("BTC-31DEC26-100000-C", Some("2026-12-31")),
        ("BTC-29FEB26-100000-C", None),
        ("BTC-31FEB26-100000-C", None),
        ("BTC-31APR26-100000-C", None),
        ("BTC-31NOV26-100000-C", None),
        ("BTC-00JAN26-100000-C", None),
        ("BTC-32JAN26-100000-C", None),
    ] {
        assert_eq!(parse_instrument_name(name).map(|p| p.1), date.map(String::from), "{name}");
    }
}

#[test]
fn lists_expiries_ascending_distinct() {
    let exps = list_expiries_from_summary(&summary(), NOW, None);
    let dates: Vec<&str> = exps.iter().map(|e| e.date.as_str()).collect();
    assert_eq!(dates, ["2026-06-27", "2026-09-25"]);
    assert_eq!(exps[0].dte, 25);
}

#[test]
fn lists_expiries_filters_to_requested_coin_in_shared_book() {
    let exps = list_expiries_from_summary(&usdc_summary(), NOW, Some("SOL"));
    let dates: Vec<&str> = exps.iter().map(|e| e.date.as_str()).collect();
    assert_eq!(dates, ["2026-06-26"]); // only SOL, not the BTC 31 Jul row
}

#[test]
fn builds_chain_groups_scales_and_enriches() {
    let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
    assert_eq!(chain.source, "deribit");
    assert_eq!(chain.underlying_kind, OptionUnderlyingKind::Crypto);
    assert_eq!(chain.underlying_price, Some(104000.0));
    // the 25 Sep / 120000 row is filtered out
    let strikes: Vec<f64> = chain.rows.iter().map(|r| r.strike).collect();
    assert_eq!(strikes, [100000.0, 110000.0]);
    let row = &chain.rows[0];
    let (call, put) = (row.call.as_ref().unwrap(), row.put.as_ref().unwrap());
    assert_eq!(call.iv, Some(0.625)); // mark_iv % -> decimal
    assert_eq!(put.iv, Some(0.61));
    // Deribit premiums are coin units, scaled to USD by underlying_price (exact arithmetic)
    assert_eq!(call.bid, Some(0.05 * 104000.0));
    assert_eq!(call.mark, Some(0.055 * 104000.0));
    assert!(call.delta.is_some(), "greeks enriched from IV");
    assert!(chain.rows[1].put.is_none(), "only a call at 110000");
}

#[test]
fn builds_sol_chain_from_usdc_book_unscaled_premiums() {
    // usd_quoted=true: USDC premiums are already USD -> NOT scaled by the ~74.5 underlying
    let chain = build_chain_from_summary("SOL", &usdc_summary(), "2026-06-26", NOW, true, 0.0);
    assert_eq!(chain.underlying, "SOL");
    assert_eq!(chain.underlying_price, Some(74.5));
    let strikes: Vec<f64> = chain.rows.iter().map(|r| r.strike).collect();
    assert_eq!(strikes, [90.0]); // BTC/XRP rows excluded
    assert_eq!(chain.rows[0].put.as_ref().unwrap().bid, Some(17.5)); // not 17.5 * 74.5
    assert_eq!(chain.rows[0].call.as_ref().unwrap().mark, Some(1.1));
}

#[test]
fn chain_carries_exact_instrument_name() {
    // the arm path needs the exact venue id; the _USDC suffix must survive verbatim
    let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
    let row = &chain.rows[0];
    assert_eq!(row.call.as_ref().unwrap().instrument_name.as_deref(), Some("BTC-27JUN26-100000-C"));
    assert_eq!(row.put.as_ref().unwrap().instrument_name.as_deref(), Some("BTC-27JUN26-100000-P"));
    assert_eq!(
        chain.rows[1].call.as_ref().unwrap().instrument_name.as_deref(),
        Some("BTC-27JUN26-110000-C")
    );
    let sol = build_chain_from_summary("SOL", &usdc_summary(), "2026-06-26", NOW, true, 0.0);
    assert_eq!(
        sol.rows[0].put.as_ref().unwrap().instrument_name.as_deref(),
        Some("SOL_USDC-26JUN26-90-P")
    );
}

/// The keyless book-summary read follows redirects ON PURPOSE and to the shared stated limit, not
/// ureq's default of ten (A50): the provider's agent carries `PUBLIC_GET_MAX_REDIRECTS`.
#[test]
fn the_book_summary_read_states_its_redirect_limit() {
    let provider =
        DeribitOptionsProvider::from_urls(&vike_venues::venue_urls::VenueUrls::seed("deribit"))
            .expect("the seed holds deribit's live rest row");
    assert_eq!(
        provider.agent.config().max_redirects(),
        vike_transport::http::PUBLIC_GET_MAX_REDIRECTS
    );
    assert!(provider.agent.config().http_status_as_error(), "a 429 or 5xx stays an Err");
    assert_eq!(provider.agent.config().timeouts().global, Some(std::time::Duration::from_secs(10)));
}

/// On the release's seed the book summary is read where it always was, byte for byte; with no row
/// the provider is refused, naming it.
#[test]
fn the_book_summary_reads_the_seeds_live_row() {
    let seed = vike_venues::venue_urls::VenueUrls::seed("deribit");
    let provider = DeribitOptionsProvider::from_urls(&seed).expect("the seed");
    assert_eq!(
        provider.summary_url,
        "https://www.deribit.com/api/v2/public/get_book_summary_by_currency"
    );
    let none = vike_venues::venue_urls::VenueUrls::from_rows("deribit", std::iter::empty());
    assert!(DeribitOptionsProvider::from_urls(&none).is_err());
}

// ── the listing: the venue's own rows, held and read again ───────────────────────────────────────

/// A `currency=any&kind=option` answer in the venue's documented row shape.
fn instruments_answer(bases: &[(&str, &str, &str)]) -> Value {
    let rows: Vec<Value> = bases
        .iter()
        .map(|(base, settle, index)| {
            serde_json::json!({
                "kind": "option", "is_active": true, "instrument_name": format!("{base}-X"),
                "base_currency": base, "settlement_currency": settle, "price_index": index
            })
        })
        .collect();
    serde_json::json!({ "jsonrpc": "2.0", "result": rows })
}

type Script = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Result<Value, String>>>>;

/// A provider whose listing read answers `script` in order (and fails once it runs out), counting the
/// reads; its book summary points nowhere, and no test here asks it.
fn scripted(script: Vec<Result<Value, String>>) -> (DeribitOptionsProvider, Script) {
    let script: Script = std::sync::Arc::new(std::sync::Mutex::new(script.into()));
    let feed = std::sync::Arc::clone(&script);
    let provider = DeribitOptionsProvider::at(
        "http://127.0.0.1:1/never",
        Box::new(move || {
            feed.lock().expect("script").pop_front().unwrap_or_else(|| Err("ran out".into()))
        }),
    );
    (provider, script)
}

fn names(listed: &[OptionUnderlying]) -> Vec<&str> {
    listed.iter().map(|u| u.name.as_str()).collect()
}

/// **The underlyings are the venue's, not a table**: what the instrument rows list, books and indexes
/// included, whatever coins those are, in the listing's order (one option each here: a tie, by name).
#[test]
fn the_provider_lists_what_the_venues_instrument_rows_list() {
    let (mut provider, _) = scripted(vec![Ok(instruments_answer(&[
        ("SOL", "USDC", "sol_usdc"),
        ("BTC", "BTC", "btc_usd"),
        ("AVAX", "USDC", "avax_usdc"),
    ]))]);
    let listed = provider.list_underlyings().expect("listed");
    assert_eq!(names(&listed), ["AVAX", "BTC", "SOL"]);
    assert_eq!(
        listed.iter().map(|u| (u.book.as_str(), u.index.as_str())).collect::<Vec<_>>(),
        [("USDC", "avax_usdc"), ("BTC", "btc_usd"), ("USDC", "sol_usdc")]
    );
    assert_eq!(provider.listed().map(names), Some(vec!["AVAX", "BTC", "SOL"]));
}

/// The listing is HELD for `LISTING_TTL_MS` and read again after it, so a new underlying and a
/// delisted one show without a restart; a read that fails keeps the held listing, and is retried no
/// sooner than `LISTING_MIN_GAP_MS`.
#[test]
fn the_listing_is_read_again_after_its_ttl_and_a_failed_read_keeps_the_last_one() {
    let (mut provider, script) = scripted(vec![
        Ok(instruments_answer(&[("BTC", "BTC", "btc_usd"), ("ETH", "ETH", "eth_usd")])),
        Err("deribit option instruments: HTTP 503".into()),
        Ok(instruments_answer(&[("BTC", "BTC", "btc_usd"), ("XRP", "USDC", "xrp_usdc")])),
    ]);
    let t0 = 1_000_000;
    assert_eq!(names(provider.listing_at(t0, false).expect("read")), ["BTC", "ETH"]);
    // Held: no read inside the TTL.
    assert_eq!(
        names(provider.listing_at(t0 + LISTING_TTL_MS - 1, false).expect("held")),
        ["BTC", "ETH"]
    );
    assert_eq!(script.lock().expect("script").len(), 2, "a held listing read nothing");
    // Stale: read again; the read fails and the held listing stays.
    let stale = t0 + LISTING_TTL_MS;
    assert_eq!(names(provider.listing_at(stale, false).expect("kept")), ["BTC", "ETH"]);
    // The retry waits out the gap, then lists what the venue lists now: ETH went, XRP came.
    assert_eq!(
        names(provider.listing_at(stale + LISTING_MIN_GAP_MS - 1, false).expect("kept")),
        ["BTC", "ETH"]
    );
    assert_eq!(script.lock().expect("script").len(), 1, "a retry inside the gap read nothing");
    assert_eq!(
        names(provider.listing_at(stale + LISTING_MIN_GAP_MS, false).expect("read")),
        ["BTC", "XRP"]
    );
}

/// With no listing ever read, a failed read is an `Err` naming why, never an empty list; an answer
/// that lists no option underlying is a failure too.
#[test]
fn no_listing_yet_is_an_error_naming_why_never_an_empty_list() {
    let (mut provider, _) = scripted(vec![Err("deribit option instruments: HTTP 503".into())]);
    let why = provider.list_underlyings().expect_err("nothing listed yet");
    assert_eq!(
        why,
        DeribitOptionsRefusal::ListingRead {
            why: "deribit option instruments: HTTP 503".to_string()
        }
    );
    let (mut provider, _) = scripted(vec![Ok(serde_json::json!({ "result": [] }))]);
    let why = provider.list_underlyings().expect_err("an empty answer");
    assert_eq!(why, DeribitOptionsRefusal::NoUnderlying { rows: 0 });
    let (mut provider, _) = scripted(vec![Ok(serde_json::json!({ "error": "x" }))]);
    assert_eq!(
        provider.list_underlyings().expect_err("no result"),
        DeribitOptionsRefusal::NoResultList
    );
}

/// A refresh reads NOW (the held listing's age does not matter), but never within
/// `LISTING_MIN_GAP_MS` of the last read, which answers the listing held; a refresh whose read fails
/// is an `Err` and keeps the held listing.
#[test]
fn a_refresh_reads_now_but_never_inside_the_gap() {
    let (mut provider, script) = scripted(vec![
        Ok(instruments_answer(&[("BTC", "BTC", "btc_usd")])),
        Ok(instruments_answer(&[("BTC", "BTC", "btc_usd"), ("SOL", "USDC", "sol_usdc")])),
        Err("deribit option instruments: HTTP 429".into()),
    ]);
    assert_eq!(names(&provider.list_underlyings().expect("read")), ["BTC"]);
    // Inside the gap: the held listing, nothing read.
    assert_eq!(names(&provider.refresh_underlyings().expect("held")), ["BTC"]);
    assert_eq!(script.lock().expect("script").len(), 2);
    // Past the gap (moved back in time rather than waited for): read now.
    provider.listing_asked_ms = Some(now_ms() - LISTING_MIN_GAP_MS);
    assert_eq!(names(&provider.refresh_underlyings().expect("read")), ["BTC", "SOL"]);
    provider.listing_asked_ms = Some(now_ms() - LISTING_MIN_GAP_MS);
    let why = provider.refresh_underlyings().expect_err("the read failed");
    assert_eq!(
        why,
        DeribitOptionsRefusal::ListingRead {
            why: "deribit option instruments: HTTP 429".to_string()
        }
    );
    assert_eq!(provider.listed().map(names), Some(vec!["BTC", "SOL"]), "the held listing stays");
}

/// An underlying the listing does not name is refused naming the listing, before any book is read.
#[test]
fn an_underlying_the_venue_does_not_list_is_refused_naming_the_listing() {
    let (mut provider, _) = scripted(vec![Ok(instruments_answer(&[
        ("BTC", "BTC", "btc_usd"),
        ("SOL", "USDC", "sol_usdc"),
    ]))]);
    let why = provider.list_expiries("DOGE").expect_err("not listed");
    assert_eq!(
        why,
        DeribitOptionsRefusal::NotListed {
            underlying: "DOGE".to_string(),
            listed: "BTC, SOL".to_string()
        }
    );
}
