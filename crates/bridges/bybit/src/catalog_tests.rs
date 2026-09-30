use super::*;
use vike_model::AssetClass;

#[test]
fn spot_maps_to_cryptospot() {
    let payload = serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT", "status": "Trading" }
    ]}});
    let out = parse_spot(&payload);
    assert_eq!(out[0].asset_class, AssetClass::CryptoSpot);
    assert_eq!(out[0].raw_symbol, "BTCUSDT");
}

#[test]
fn perp_linear_category_maps_to_cryptoperp() {
    let payload = serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT",
          "status": "Trading", "contractType": "LinearPerpetual" },
        { "symbol": "BTCUSDT-25JUL26", "baseCoin": "BTC", "quoteCoin": "USDT",
          "status": "Trading", "contractType": "LinearFutures" }
    ]}});
    let out = parse_perp(&payload);
    assert_eq!(out.len(), 1, "only the LinearPerpetual row survives");
    assert_eq!(out[0].asset_class, AssetClass::CryptoPerp);
    assert_eq!(out[0].raw_symbol, "BTCUSDT.P", "perp gets the .P distinct-symbol suffix");
}

/// ⚠ **The venue's own contract words are CARRIED, not discarded.** The payload shape is the
/// live one (MEASURED 2026-09-16 against the keyless `instruments-info` endpoint): the inverse
/// perpetual `BTCUSD` reports `contractType: "InversePerpetual"` and `settleCoin: "BTC"`, and
/// before this the only thing separating it from a linear perp anywhere in the tree was the
/// tail of the symbol string.
///
/// The row is fed through the parser DIRECTLY rather than through the `LinearPerpetual` gate,
/// because that gate is what makes the field constant today — see the comment at the call site.
/// This test is what fails if a later change drops the carry while widening the gate.
#[test]
fn the_venues_own_contract_words_are_carried_verbatim() {
    let payload = serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT", "status": "Trading",
          "contractType": "LinearPerpetual", "settleCoin": "USDT" }
    ]}});
    let out = parse_perp(&payload);
    assert_eq!(out[0].contract_type.as_deref(), Some("LinearPerpetual"));
    assert_eq!(out[0].settle_asset.as_deref(), Some("USDT"));
}

/// A venue that publishes neither field leaves both ABSENT — the ordinary state, and the one
/// that must stay byte-identical to a world without the fields. Spot rows are exactly that case.
#[test]
fn a_row_without_the_fields_leaves_them_absent() {
    let payload = serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT", "status": "Trading" }
    ]}});
    let spot = parse_spot(&payload);
    assert_eq!(spot[0].contract_type, None);
    assert_eq!(spot[0].settle_asset, None);
}

/// The (g) robustness contract, mirroring okx's per-instType accumulate: the spot endpoint
/// failing must not wipe the venue — the linear perps still come back (and vice versa).
#[test]
fn one_failed_endpoint_still_yields_the_other_endpoints_instruments() {
    let perp_payload = serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT",
          "status": "Trading", "contractType": "LinearPerpetual" }
    ]}});
    let out = list_tolerant(|url| {
        if url == SPOT_URL {
            Err(CatalogError("HTTP 500: simulated".into()))
        } else {
            Ok(perp_payload.clone())
        }
    });
    assert_eq!(out.len(), 1, "the linear page must survive the spot failure");
    assert_eq!(out[0].raw_symbol, "BTCUSDT.P");
    // both failing yields an EMPTY catalog (warned), never an error
    assert!(list_tolerant(|_| Err(CatalogError("down".into()))).is_empty());
}

// ---- cursor paging (the truncation fix) ----------------------------------------------------

/// `n` linear-perp rows whose symbols encode their GLOBAL index, wrapped in the venue's real
/// `{result:{list,nextPageCursor}}` envelope — so a test can prove WHICH pages were fetched and
/// that ordering and accumulation survive the crawl. `cursor` is the page's terminal spelling:
/// `Some(c)` writes `nextPageCursor: c` (an empty `c` being the venue's own last-page answer on
/// `category=linear`), `None` omits the key entirely — what `category=spot` actually does.
fn perp_page(start: usize, n: usize, cursor: Option<&str>) -> Value {
    let list: Vec<Value> = (start..start + n)
        .map(|i| {
            serde_json::json!({
                "symbol": format!("P{i}USDT"), "baseCoin": format!("P{i}"),
                "quoteCoin": "USDT", "status": "Trading", "contractType": "LinearPerpetual"
            })
        })
        .collect();
    match cursor {
        Some(c) => serde_json::json!({ "result": { "list": list, "nextPageCursor": c } }),
        None => serde_json::json!({ "result": { "list": list } }),
    }
}

/// The defect this module's ⚠ block is about: a paged category must be followed to EXHAUSTION,
/// and the parsed count must be the fixture's FULL content rather than its first page. Driven
/// from a scripted response, never the live network.
#[test]
fn a_paged_category_is_followed_to_exhaustion_not_read_once() {
    use std::cell::RefCell;
    let urls: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let out = paged(PERP_URL, parse_perp, &|url: &str| {
        urls.borrow_mut().push(url.to_string());
        Ok(if url == PERP_URL {
            perp_page(0, 3, Some("cur1"))
        } else if url.ends_with("&cursor=cur1") {
            perp_page(3, 3, Some("cur2"))
        } else if url.ends_with("&cursor=cur2") {
            perp_page(6, 2, Some("")) // the venue's real last-page answer
        } else {
            panic!("unexpected url {url}")
        })
    })
    .unwrap();

    assert_eq!(out.len(), 8, "the FULL fixture (3+3+2), not its first page of 3");
    assert_eq!(out[0].raw_symbol, "P0USDT.P", "page 0 leads, in order");
    assert_eq!(out[7].raw_symbol, "P7USDT.P", "the final page's last row is present");
    assert_eq!(
        *urls.borrow(),
        vec![
            PERP_URL.to_string(),
            format!("{PERP_URL}&cursor=cur1"),
            format!("{PERP_URL}&cursor=cur2"),
        ],
        "the first page is the bare const; each later page echoes the cursor it was handed"
    );
}

/// All THREE end-of-listing spellings terminate after exactly one request — the key absent
/// (`category=spot`, MEASURED: it carries no `nextPageCursor` at all), the empty string
/// (`category=linear`'s final page) and a JSON `null`. A venue answering any of them must not
/// be asked for a second page.
#[test]
fn every_end_of_listing_cursor_spelling_terminates_after_one_request() {
    use std::cell::Cell;
    for (label, terminal) in [("absent key", None), ("empty string", Some(""))] {
        let calls = Cell::new(0usize);
        let out = paged(SPOT_URL, parse_perp, &|_: &str| {
            calls.set(calls.get() + 1);
            Ok(perp_page(0, 4, terminal))
        })
        .unwrap();
        assert_eq!(out.len(), 4, "{label}: the one page is parsed");
        assert_eq!(calls.get(), 1, "{label}: must not trigger a second fetch");
    }

    let calls = Cell::new(0usize);
    let out = paged(SPOT_URL, parse_spot, &|_: &str| {
        calls.set(calls.get() + 1);
        Ok(serde_json::json!({ "result": {
            "list": [{ "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT",
                       "status": "Trading" }],
            "nextPageCursor": Value::Null
        }}))
    })
    .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(calls.get(), 1, "a null cursor must not trigger a second fetch");
}

/// ⚠ The failure that would HANG the picker's thread rather than merely truncate it: a venue
/// echoing one cursor forever. The bound asserted here is the REPEAT guard, which must bite
/// strictly before [`MAX_PAGES`] — the ceiling is the backstop, not the mechanism.
#[test]
fn a_repeating_cursor_stops_rather_than_spinning_to_the_page_ceiling() {
    use std::cell::Cell;
    let calls = Cell::new(0usize);
    let out = paged(PERP_URL, parse_perp, &|_: &str| {
        calls.set(calls.get() + 1);
        Ok(perp_page(0, 2, Some("stuck"))) // every page hands back the SAME cursor
    })
    .unwrap();
    assert_eq!(calls.get(), 2, "page 0 spends the cursor; page 1 sees it repeat and stops");
    assert_eq!(out.len(), 4, "only the two pages actually fetched are accumulated");
    assert!(calls.get() < MAX_PAGES, "the repeat guard must bite BEFORE the page ceiling");
}

/// ...and the backstop itself, for a listing whose cursor never repeats AND never terminates.
#[test]
fn the_page_ceiling_bounds_a_listing_whose_cursor_never_ends() {
    use std::cell::Cell;
    let calls = Cell::new(0usize);
    let out = paged(PERP_URL, parse_perp, &|_: &str| {
        let n = calls.get();
        calls.set(n + 1);
        Ok(perp_page(n, 1, Some(&format!("c{n}")))) // a FRESH cursor forever
    })
    .unwrap();
    assert_eq!(calls.get(), MAX_PAGES, "the defensive ceiling stops the crawl");
    assert_eq!(out.len(), MAX_PAGES);
}

/// The paging inherits polymarket's failure shape: a FIRST-page failure is the category being
/// unreachable (`Err`, which is what keeps `list_tolerant`'s per-category tolerance working);
/// a LATER page failing degrades to what was already fetched rather than losing the venue.
#[test]
fn a_first_page_failure_errors_but_a_later_one_keeps_what_it_has() {
    assert!(paged(PERP_URL, parse_perp, &|_: &str| Err(CatalogError("down".into()))).is_err());
    let out = paged(PERP_URL, parse_perp, &|url: &str| {
        if url == PERP_URL {
            Ok(perp_page(0, 5, Some("c1")))
        } else {
            Err(CatalogError("mid-crawl hiccup".into()))
        }
    })
    .unwrap();
    assert_eq!(out.len(), 5, "page 0 survives the page-1 failure");
}

/// Both URLs ask for the venue's MAXIMUM page, so the cursor loop is the durable half rather
/// than the only half — and the two spellings of that number cannot drift. Also pins the
/// first-page/later-page URL shapes the crawl depends on.
#[test]
fn the_urls_request_the_venue_maximum_page() {
    let want = format!("limit={PAGE_LIMIT}");
    assert!(SPOT_URL.ends_with(&want), "{SPOT_URL} must end with {want}");
    assert!(PERP_URL.ends_with(&want), "{PERP_URL} must end with {want}");
    assert_eq!(page_url(SPOT_URL, ""), SPOT_URL, "the first page is the bare const");
    assert_eq!(
        page_url(SPOT_URL, "first%3DA%26last%3DB"),
        format!("{SPOT_URL}&cursor=first%3DA%26last%3DB"),
        "a cursor Bybit already percent-encoded is echoed verbatim, never re-encoded"
    );
}

/// The live `category=inverse` row shape, MEASURED 2026-09-16 against the keyless endpoint
/// (trimmed to the keys these parsers read).
fn live_inverse_row() -> Value {
    serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSD", "baseCoin": "BTC", "quoteCoin": "USD", "status": "Trading",
          "contractType": "InversePerpetual", "settleCoin": "BTC" }
    ]}})
}

/// ⚠ **THE ORDER-OF-WORK GATE, re-pointed rather than deleted.**
///
/// Its predecessor (`inverse_is_deliberately_unfetched_and_the_parse_gate_makes_that_structural`)
/// asserted two pieces of NEGATIVE SPACE — no URL says `inverse`, and `parse_perp` drops an
/// `InversePerpetual` row — so that widening the fetch could not leak un-routable symbols into
/// the picker by accident. Both became false by construction the moment the fetch landed, which
/// is exactly what that test was built to force somebody to think about. **A test that has done
/// its job is re-pointed at the invariant it was protecting, never removed**: an inverse row may
/// reach the picker only if it carries what the wire needs to ROUTE it.
///
/// The load-bearing half is the last assertion. Checking that a FIELD is populated is something
/// a compile-time constant would pass; this feeds the minted symbol to the REAL routing decision
/// and to the REAL host chooser, so it reddens if somebody widens the fetch while `data.rs` or
/// `market_feed.rs` still answers "linear" for this symbol — the state the old test existed to
/// prevent, and the one the WS measurements make worse than its author knew.
#[test]
fn an_inverse_row_reaches_the_picker_only_if_it_routes() {
    let out = parse_inverse(&live_inverse_row());
    assert_eq!(out.len(), 1, "the live InversePerpetual row must now be minted");
    let inst = &out[0];
    assert_eq!(inst.raw_symbol, "BTCUSD.P");
    assert_eq!(inst.asset_class, AssetClass::CryptoPerp);

    // (1) It carries the venue's own words — the machine-readable half of the class.
    assert_eq!(inst.contract_type.as_deref(), Some("InversePerpetual"));
    assert_eq!(
        inst.settle_asset.as_deref(),
        Some("BTC"),
        "coin-settled: the settle asset IS the base asset — 0061's cross-venue-comparable half"
    );
    assert_eq!(inst.settle_asset.as_deref(), Some(inst.base.as_str()));

    // (2) It carries a label a HUMAN can tell from the spot row's — the owner's ruling.
    assert_eq!(inst.description, "Inverse perp · settles BTC");

    // (3) ...and THE ROUTE. Fed to the real decisions, not to a re-implementation of them.
    let (wire, book) =
        crate::data::route_for_test(&inst.raw_symbol, crate::instruments::PerpBook::Inverse)
            .expect("a minted inverse row must route");
    assert_eq!(wire, "BTCUSD", "the .P must not reach the wire");
    assert_eq!(
        book,
        crate::data::Category::Inverse,
        "a fetched inverse row that routes to `linear` is the state the predecessor test \
             existed to prevent"
    );
    assert_eq!(book.wire(), "inverse");
    assert_eq!(
        crate::market_feed::ws_host_for_test(book),
        crate::market_feed::PUBLIC_WS_INVERSE,
        "the linear socket answers `error:handler not found` for this symbol (MEASURED \
             2026-09-16) — silently, on the depth lane"
    );
}

/// ⚠ **The NARROWED negative pin.** 6 of the 28 Trading inverse rows are DATED futures
/// (`BTCUSDZ26`, `ETHUSDH27`, …), and they stay unreachable: they are `AssetClass::CryptoFuture`
/// rather than perpetuals, the `.F` sibling suffix is mapped by no adapter in this tree, and
/// minting them under `.P` would say PERPETUAL of a contract that expires. So the hole shrank
/// from a whole category to six symbols and is still GATED, rather than becoming an unexamined
/// remainder — the same reason its predecessor existed.
///
/// The linear half is asserted beside it: `category=linear` serves dated `LinearFutures` too,
/// and has always dropped them for the identical reason.
#[test]
fn inverse_dated_futures_stay_unfetched() {
    let payload = serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDZ26", "baseCoin": "BTC", "quoteCoin": "USD", "status": "Trading",
          "contractType": "InverseFutures", "settleCoin": "BTC" },
        { "symbol": "BTCUSD", "baseCoin": "BTC", "quoteCoin": "USD", "status": "Trading",
          "contractType": "InversePerpetual", "settleCoin": "BTC" }
    ]}});
    let out = parse_inverse(&payload);
    assert_eq!(out.len(), 1, "only the perpetual survives");
    assert_eq!(out[0].raw_symbol, "BTCUSD.P");
    assert!(
        !out.iter().any(|i| i.raw_symbol.starts_with("BTCUSDZ26")),
        "a dated future must not reach the picker wearing the PERPETUAL suffix"
    );
    // ...and the linear twin of the same rule, which predates this change.
    let linear = serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDT-25JUL26", "baseCoin": "BTC", "quoteCoin": "USDT",
          "status": "Trading", "contractType": "LinearFutures" }
    ]}});
    assert!(parse_perp(&linear).is_empty());
}

/// **The two rows a person actually sees when they search `BTCUSD`** — the owner's ruling,
/// asserted end to end over the SHARED widget's three columns (`raw_symbol | description |
/// VENUE`, `crates/vike-app-core/src/ui/symbol_row.rs`'s `search_result_row`).
///
/// ⚠ The distinguishing evidence is deliberately NOT the id. `BTCUSD` vs `BTCUSD.P` differ, but
/// `.P` says PERPETUAL and nothing else — a reader who does not know this codebase cannot tell
/// which of two perpetual books they are looking at from a suffix. The DESCRIPTION column is
/// what carries it, in English, and it is unique between the rows.
#[test]
fn a_spot_row_and_an_inverse_row_are_distinguishable_to_a_person() {
    let spot = parse_spot(&serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSD", "baseCoin": "BTC", "quoteCoin": "USD", "status": "Trading" }
    ]}}));
    let inverse = parse_inverse(&live_inverse_row());
    let linear = parse_perp(&serde_json::json!({ "result": { "list": [
        { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT", "status": "Trading",
          "contractType": "LinearPerpetual", "settleCoin": "USDT" }
    ]}}));

    assert_eq!(spot[0].raw_symbol, "BTCUSD");
    assert_eq!(inverse[0].raw_symbol, "BTCUSD.P");
    assert_ne!(spot[0].id(), inverse[0].id(), "two rows, two ids");

    // The column a person reads. Empty for spot (unchanged for every venue), and a sentence for
    // each perpetual kind.
    assert_eq!(spot[0].description, "");
    assert_eq!(inverse[0].description, "Inverse perp · settles BTC");
    assert_eq!(
        linear[0].description, "Perp · settles USDT",
        "⚠ the LINEAR row is labelled too: an unlabelled `.P` meaning `linear` by ABSENCE is \
             the implicit encoding 0061 exists to remove, wearing a description column"
    );
    assert_ne!(inverse[0].description, linear[0].description);
    assert!(
        inverse[0].description.to_lowercase().contains("inverse"),
        "the owner's ruling is literally that the inverse one is LABELLED inverse"
    );
}

/// The label is derived from the venue's words, and degrades rather than invents. An unknown
/// `contractType` prints the venue's own string (never a guess), and an absent one prints
/// NOTHING — byte-identical to every row minted before this column was filled.
#[test]
fn the_label_degrades_instead_of_inventing() {
    assert_eq!(contract_label(None, Some("BTC")), "");
    assert_eq!(contract_label(Some("InversePerpetual"), None), "Inverse perp");
    assert_eq!(contract_label(Some("InversePerpetual"), Some("")), "Inverse perp");
    assert_eq!(
        contract_label(Some("SomethingNew"), Some("XYZ")),
        "SomethingNew · settles XYZ",
        "a contract type nobody has seen prints the venue's own word rather than prose we made \
             up for it"
    );
}

/// All THREE categories are fetched, and one failing does not wipe the others — the per-category
/// tolerance contract, now with a third row to be tolerant about.
#[test]
fn every_category_is_fetched_and_the_crawl_stays_per_category_tolerant() {
    use std::cell::RefCell;
    let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let out = list_tolerant(|url| {
        seen.borrow_mut().push(url.to_string());
        if url == INVERSE_URL {
            Ok(live_inverse_row())
        } else if url == PERP_URL {
            Err(CatalogError("HTTP 500: simulated".into()))
        } else {
            Ok(serde_json::json!({ "result": { "list": [
                { "symbol": "BTCUSD", "baseCoin": "BTC", "quoteCoin": "USD",
                  "status": "Trading" }
            ]}}))
        }
    });
    let urls = seen.borrow().clone();
    for want in [SPOT_URL, PERP_URL, INVERSE_URL] {
        assert!(urls.iter().any(|u| u == want), "{want} never fetched: {urls:?}");
    }
    assert_eq!(out.len(), 2, "the linear failure must not take spot or inverse with it");
    assert!(out.iter().any(|i| i.raw_symbol == "BTCUSD"));
    assert!(out.iter().any(|i| i.raw_symbol == "BTCUSD.P"));
}

/// The third URL obeys the same page contract as its siblings — the crawl's whole correctness
/// rests on `limit=1000` plus the cursor, and a new category joining without both is how the
/// picker gets silently truncated again.
#[test]
fn the_inverse_url_obeys_the_same_page_contract() {
    assert!(INVERSE_URL.ends_with(&format!("limit={PAGE_LIMIT}")));
    assert!(INVERSE_URL.contains("category=inverse"));
    assert_eq!(page_url(INVERSE_URL, ""), INVERSE_URL, "the first page is the bare const");
}
