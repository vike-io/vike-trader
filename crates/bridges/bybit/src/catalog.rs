//! Public spot + linear-perp + INVERSE-perp instrument catalog — the Bybit twin of binance's
//! `catalog` module, exposed as the venue's [`CatalogProvider`] contribution to the chart's
//! cross-venue symbol search. Reads the KEYLESS public `/v5/market/instruments-info` (`category=`
//! `spot` + `linear` + `inverse`, UNSIGNED GETs — no credentials, no signer), mapped to universal
//! [`Instrument`]s via the declarative [`FieldMap`] and the two perpetual parsers. The chart's live
//! bar feed ([`crate::market_feed`]) resolves the same symbol through the same routing decision this
//! catalog's rows satisfy, so a searched-and-selected Bybit symbol subscribes cleanly.
//!
//! Pure parse (`parse_spot`/`parse_perp`/`parse_inverse`, fixture-tested) is split from the blocking
//! `fetch_json` so the mapping stays testable without network — mirrors binance/catalog.rs's shape.
//!
//! ## ⚠ The endpoint is CURSOR-PAGED, and ignoring that TRUNCATED the picker
//!
//! Both URLs passed no `limit` and followed no cursor, so each category took Bybit's DEFAULT page
//! of 500 rows and stopped. MEASURED live 2026-09-16 against the keyless endpoint: `category=spot`
//! serves **538** Trading rows and carries no `nextPageCursor` key at all (spot does not page —
//! that category was never truncated), while `category=linear` serves **870** Trading rows across
//! pages and returned a cursor after its first 500. Of that full linear list 830 are
//! `LinearPerpetual`, so the Symbol picker was offering **472 of 830 bybit perps** — it had never
//! seen the other 358.
//!
//! So a `limit` ALONE is not the fix: it would move the cliff rather than remove it. Bybit caps
//! `limit` at 1000 and linear is at 870 today — 130 contracts of headroom — so [`paged`] follows
//! `result.nextPageCursor` to exhaustion, the same warn-and-accumulate shape as
//! `crates/bridges/polymarket/src/catalog.rs`'s `paged_markets`. Termination is triple: an absent,
//! null or EMPTY cursor (the venue's own end-of-listing signal — linear's last page answers `""`),
//! a cursor IDENTICAL to the one just spent, and the [`MAX_PAGES`] ceiling. The middle one is not
//! decoration: `list_instruments` runs on the picker's thread, and a venue echoing one cursor
//! forever is the shape that hangs a UI rather than the shape that returns wrong data.
//!
//! ## ⚠ `category=inverse` IS fetched now, and the order the work landed in is the load-bearing part
//!
//! It was not, until this. The block that stood here said so and said why: the only marker the core
//! vocabulary has for a bybit perp is [`vike_catalog::PERP_SUFFIX`], and for THIS venue that suffix
//! MEANT `category=linear` at every wire-facing site — `data.rs`'s `rest_category`,
//! `market_feed.rs`'s `perp_split` (which routed to `market_feed::PUBLIC_WS_LINEAR`), and every
//! signed call in `perp.rs`/`exec.rs`. An inverse perp surfaced as `BTCUSD.P` would have subscribed
//! a socket that answers `error:handler not found` and — if traded — signed an order carrying
//! `category: "linear"` for a symbol that does not exist in that category.
//!
//! **So the routing landed FIRST and the fetch second**, which is what
//! `docs/decisions/0061-an-instrument-names-its-kind.md` phase 4 asks for. `.P` no longer decides a
//! category anywhere in this crate: `crates/bridges/bybit/src/instruments.rs`'s `perp_book` asks the
//! VENUE which derivative book carries a symbol, `data.rs`'s `Category` carries the answer to the
//! REST URL, and `market_feed.rs`'s `ws_host` carries it to one of three STRICT sockets. Only then
//! does [`INVERSE_URL`] exist. The test that used to pin the fetch as absent
//! ([`catalog_tests::an_inverse_row_reaches_the_picker_only_if_it_routes`]) was that ORDER written as
//! a gate, and it is rewritten rather than deleted — it now pins the invariant it was protecting: a
//! fetched inverse row must carry a class that routes.
//!
//! ⚠ **The EXEC plane did not move, and an inverse row is offered anyway.** `perp.rs`/`recon_client`/
//! `funding` still spell `category: "linear"` as a literal at every signed site.
//! `crates/bridges/bybit/src/exec.rs`'s `non_linear_perpetual_refusal` is what makes that safe: the
//! venue's own `contractType` for the mounted symbol — read off the `instruments-info` row the exec
//! thread ALREADY fetches — turns every order into a terminal `OrderRejected` naming the limitation
//! instead of a signed order in the wrong book. So the picker offers an inverse instrument that
//! CHARTS and BACKFILLS and cannot be traded, and the operator is told which it is at the moment
//! they try.
//!
//! ## ⚠ `InverseFutures` stays unfetched, and that pin is NARROWED rather than dropped
//!
//! MEASURED 2026-09-16: `category=inverse` serves 28 Trading rows — 22 `InversePerpetual` and 6
//! `InverseFutures` (`BTCUSDZ26`, `ETHUSDH27`, …). [`parse_inverse`] admits only the first kind.
//! Dated futures are `AssetClass::CryptoFuture`, the `.F` sibling suffix is mapped by no adapter in
//! this tree, and minting them under `.P` would say PERPETUAL of a contract that expires. The gate
//! for that shrunken hole is [`catalog_tests::inverse_dated_futures_stay_unfetched`] — the same
//! shape as the old whole-category pin, scoped to what is actually still unreachable.

use serde_json::Value;
use vike_catalog::{
    AssetClass, CatalogError, CatalogMode, CatalogProvider, FieldMap, Instrument, parse_with,
};

const SPOT_MAP: FieldMap = FieldMap {
    list_path: &["result", "list"],
    symbol: "symbol",
    base: "baseCoin",
    quote: "quoteCoin",
    active: Some(("status", "Trading")),
};

/// Bybit's documented `limit` ceiling for `instruments-info` (the venue accepts `[1, 1000]`).
///
/// ⚠ **`#[cfg(test)]` on purpose — this constant's whole job is to PIN a literal.** The two URL
/// consts below must stay `const` (a first-page URL is compared byte-for-byte against them, both by
/// [`page_url`]'s contract and by `list_tolerant`'s existing per-category failure test), and a
/// `const &str` cannot be `format!`ed, so the 1000 is spelled into each URL by hand.
/// `the_urls_request_the_venue_maximum_page` holds this number and those two spellings equal — the
/// same load-bearing-duplication shape the settings registry uses. Production code reads the URLs,
/// never this, so a non-test constant here would be dead code under `-D warnings`.
#[cfg(test)]
const PAGE_LIMIT: usize = 1000;

/// Defensive ceiling on pages fetched per category — 20 pages × the 1000-row `limit` the URLs ask
/// for = 20k rows, far above any category's live size (the largest, `linear`, is 870), so the crawl
/// can never run unbounded if Bybit stops sending an empty terminal cursor. The twin of
/// polymarket's `GAMMA_MAX_PAGES`.
const MAX_PAGES: usize = 20;

const SPOT_URL: &str = "https://api.bybit.com/v5/market/instruments-info?category=spot&limit=1000";
const PERP_URL: &str =
    "https://api.bybit.com/v5/market/instruments-info?category=linear&limit=1000";
/// The COIN-SETTLED derivative listing — 28 Trading rows on 2026-09-16, of which
/// [`parse_inverse`] mints the 22 perpetuals. Unreachable until the routing landed; see the module
/// doc for why that order was not optional.
const INVERSE_URL: &str =
    "https://api.bybit.com/v5/market/instruments-info?category=inverse&limit=1000";

/// Parse the V5 `{result:{list}}` spot envelope into `CryptoSpot` [`Instrument`]s via the
/// declarative [`FieldMap`].
pub fn parse_spot(payload: &Value) -> Vec<Instrument> {
    parse_with(&SPOT_MAP, "bybit", AssetClass::CryptoSpot, payload)
}

/// Parse the V5 `{result:{list}}` linear-category envelope into `CryptoPerp` [`Instrument`]s
/// (`contractType == "LinearPerpetual"` — the `linear` category also lists dated `LinearFutures`).
pub fn parse_perp(payload: &Value) -> Vec<Instrument> {
    parse_perpetuals(payload, "LinearPerpetual")
}

/// Parse the V5 `{result:{list}}` INVERSE-category envelope into `CryptoPerp` [`Instrument`]s
/// (`contractType == "InversePerpetual"`; the 6 dated `InverseFutures` rows are deliberately
/// dropped — see the module doc).
///
/// ⚠ **The same `CryptoPerp` class and the same `.P` spelling as its linear twin, and that is the
/// point rather than an economy.** `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 1
/// keeps the perp variant WHOLE, and `.P` says PERPETUAL — which an inverse perpetual is. What
/// separates the two rows for a MACHINE is [`Instrument::contract_type`]/`settle_asset`, carried
/// verbatim below; what separates them for a HUMAN is [`contract_label`], painted in the picker's
/// description column. Neither is the symbol string, and neither is the suffix.
///
/// There is no id collision to design around: MEASURED 2026-09-16, `linear ∩ inverse` is EMPTY, so
/// no linear perp is named `BTCUSD` and `BTCUSD.P` names exactly one instrument.
/// `crates/bridges/bybit/src/instruments.rs` carries that measurement and the `PerpBook::Both` arm
/// that turns it into a gate.
pub fn parse_inverse(payload: &Value) -> Vec<Instrument> {
    parse_perpetuals(payload, "InversePerpetual")
}

/// The shared body of [`parse_perp`]/[`parse_inverse`]: every Trading row whose `contractType` is
/// exactly `want`, minted as a `.P`-suffixed `CryptoPerp`.
///
/// Gating on the EXACT contract type (rather than on the category the URL asked for) is what keeps
/// dated futures out of both: `category=linear` serves `LinearFutures` and `category=inverse` serves
/// `InverseFutures`, and neither is a perpetual. It is a small fn rather than a pure `FieldMap` —
/// the escape-hatch case for venues whose "active" test needs more than one field.
fn parse_perpetuals(payload: &Value, want: &str) -> Vec<Instrument> {
    let Some(list) = payload.get("result").and_then(|r| r.get("list")).and_then(|l| l.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        if e.get("status").and_then(|v| v.as_str()) != Some("Trading") {
            continue;
        }
        let contract_type = e.get("contractType").and_then(|v| v.as_str());
        if contract_type != Some(want) {
            continue;
        }
        let symbol = e.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        let settle = e.get("settleCoin").and_then(|v| v.as_str()).map(str::to_uppercase);
        out.push(Instrument {
            venue: "bybit".into(),
            // `.P` suffix (TradingView convention): a distinct vike symbol so a perpetual doesn't
            // collide with its spot twin (both are `BTCUSDT` on Bybit). The feed strips the `.P`
            // back to the exchange symbol at the WS/REST boundary, and `data.rs`'s `route_target`
            // asks the venue WHICH derivative book the stripped symbol lives in.
            raw_symbol: format!("{symbol}.P"),
            asset_class: AssetClass::CryptoPerp,
            base: e.get("baseCoin").and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            quote: e.get("quoteCoin").and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            // ⚠ **THE HUMAN HALF.** The owner's 2026-09-16 ruling: *"if we search by catalog and
            // enter BTCUSD — it has to show us TWO instruments, and the inverse one has to have a
            // label as inverse."* This is that label, and it is the FIRST thing in this tree to
            // render `contract_type` anywhere. See `contract_label`.
            description: contract_label(contract_type, settle.as_deref()),
            // ⚠ The venue's OWN words for what this contract IS, carried verbatim and
            // uninterpreted — `docs/decisions/0061-an-instrument-names-its-kind.md` plus the
            // owner's 2026-09-16 ruling that a `contractType` we already receive must be CARRIED
            // rather than discarded and reconstructed from the tail of a symbol string.
            //
            // ⚠ **Nothing ROUTES on either, and that is still true after phase 4.** The route is a
            // venue-LISTING membership (`crates/bridges/bybit/src/instruments.rs`), not a reading
            // of these fields — see `vike_catalog::Instrument::contract_type`'s doc for why a
            // router reading `settle_asset` would be 0061's split-the-perp-variant trigger firing.
            // What DID change is that `contract_type` is no longer CONSTANT here: this parser now
            // has two callers with two `want` values, which is the moment an instrument saying what
            // it is starts to matter, because two Trading instruments share the string `BTCUSD`.
            properties: Default::default(),
            contract_type: contract_type.map(str::to_string),
            settle_asset: settle,
        });
    }
    out
}

/// **The picker's description for a derivative row** — the venue's own `contractType` and
/// `settleCoin`, rendered for a person.
///
/// `crates/vike-app-core/src/symbol_row.rs`'s `search_result_row` paints `raw_symbol | description |
/// VENUE`, and its own comment records that the middle column is empty for every crypto venue —
/// i.e. it paints nothing today. This fills it, which is why the owner's "the inverse one has to
/// have a label as inverse" needs no UI change at all.
///
/// ⚠ **BOTH perpetual kinds get a label, not just the inverse one**, and the asymmetry would be a
/// bug rather than an economy: if only inverse rows were labelled, an UNLABELLED `.P` row would mean
/// "linear" BY ABSENCE — a second implicit encoding, in the exact place 0061 is trying to remove the
/// first. A reader seeing `Perp · settles USDT` next to `Inverse perp · settles BTC` needs nothing
/// but English; a reader seeing one label and one blank needs to know this codebase.
///
/// The venue's word is TRANSLATED here rather than printed raw (`InversePerpetual` is not a phrase),
/// but only for the two kinds this catalog mints — anything else falls through to the venue's own
/// string, because inventing prose for a contract type we have never seen is how a label starts
/// lying. An absent `contractType` yields an EMPTY description, which is byte-identical to every
/// pre-existing row and paints nothing.
fn contract_label(contract_type: Option<&str>, settle: Option<&str>) -> String {
    let kind = match contract_type {
        Some("LinearPerpetual") => "Perp".to_string(),
        Some("InversePerpetual") => "Inverse perp".to_string(),
        Some(other) => other.to_string(),
        None => return String::new(),
    };
    match settle.filter(|s| !s.is_empty()) {
        Some(coin) => format!("{kind} · settles {coin}"),
        None => kind,
    }
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — both instruments-info endpoints are keyless public reads); called once per
/// PAGE by [`paged`] (it was once per `AssetClass`, which is what truncated the picker). Only the
/// `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The venue's end-of-listing signal, read off one page. `Some(cursor)` only for a NON-EMPTY string
/// — an absent key (what `category=spot` answers: it does not page at all), a `null`, and the empty
/// string (what `category=linear`'s final page answers) are the three spellings of "that was the
/// last page", and all three must terminate the crawl identically.
fn next_cursor(payload: &Value) -> Option<&str> {
    payload
        .get("result")
        .and_then(|r| r.get("nextPageCursor"))
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
}

/// One page's URL: the category's base const for the FIRST page (so a first-page URL is
/// byte-identical to `SPOT_URL`/`PERP_URL`), `&cursor=…` appended for every page after it. The
/// cursor is echoed back exactly as Bybit sent it — the venue already percent-encodes the
/// `first=…&last=…` payload it hands out, so re-encoding here would produce a cursor it rejects.
fn page_url(base: &str, cursor: &str) -> String {
    if cursor.is_empty() { base.to_string() } else { format!("{base}&cursor={cursor}") }
}

/// Follow one category's cursor to exhaustion, parsing each page as it arrives.
///
/// Failure shape mirrors polymarket's `paged_markets`: a FIRST-page failure is the category being
/// unreachable → `Err` (exactly the pre-paging behaviour, which is what keeps [`list_tolerant`]'s
/// per-category tolerance contract intact); a LATER page failing degrades to the pages already
/// fetched (warned, never silent) — a mid-crawl hiccup must not wipe the venue from the picker.
///
/// Three independent terminations, because only the first is the venue behaving: an end-of-listing
/// cursor ([`next_cursor`]), a cursor IDENTICAL to the one just spent, and [`MAX_PAGES`]. The
/// repeat guard is the one that matters for a UI: this runs on the picker's thread, so a venue
/// echoing one cursor forever would otherwise spin here until the page ceiling, re-parsing and
/// re-appending the same page every time.
fn paged(
    base: &str,
    parse: fn(&Value) -> Vec<Instrument>,
    fetch: &impl Fn(&str) -> Result<Value, CatalogError>,
) -> Result<Vec<Instrument>, CatalogError> {
    let mut out = Vec::new();
    let mut cursor = String::new();
    for page in 0..MAX_PAGES {
        let payload = match fetch(&page_url(base, &cursor)) {
            Ok(v) => v,
            Err(e) if page == 0 => return Err(e),
            Err(e) => {
                tracing::warn!(
                    target: "vike_bybit::catalog",
                    "page {page} of {base} failed (kept {} instruments from earlier pages): {e}",
                    out.len()
                );
                break;
            }
        };
        out.extend(parse(&payload));
        let Some(next) = next_cursor(&payload) else { break };
        if next == cursor {
            tracing::warn!(
                target: "vike_bybit::catalog",
                "{base} repeated its page cursor at page {page}; stopping with {} instruments",
                out.len()
            );
            break;
        }
        cursor = next.to_string();
    }
    Ok(out)
}

/// The body of `list_instruments` over an injectable fetch — per-category TOLERANT: one endpoint
/// failing must NOT drop the other (okx's catalog documents the bug shape this prevents: a `?` on
/// one endpoint wiped the whole venue from the Symbol picker). Accumulate what succeeds; warn (not
/// fail) on each miss. Split from the provider so the tolerance is testable without network.
///
/// Each category is crawled to exhaustion by [`paged`] rather than read once — see the module doc
/// for the truncation that cost, and for the order in which `category=inverse` became a third row.
fn list_tolerant(fetch: impl Fn(&str) -> Result<Value, CatalogError>) -> Vec<Instrument> {
    let mut out = Vec::new();
    for (label, url, parse) in [
        ("spot", SPOT_URL, parse_spot as fn(&Value) -> Vec<Instrument>),
        ("linear", PERP_URL, parse_perp as fn(&Value) -> Vec<Instrument>),
        ("inverse", INVERSE_URL, parse_inverse as fn(&Value) -> Vec<Instrument>),
    ] {
        match paged(url, parse, &fetch) {
            Ok(v) => out.extend(v),
            Err(e) => {
                tracing::warn!(target: "vike_bybit::catalog", "{label} fetch failed (skipped): {e}")
            }
        }
    }
    out
}

/// The bybit venue's `CatalogProvider` contribution: spot (`CryptoSpot`) + linear perp
/// (`CryptoPerp`), both bulk-enumerable off the keyless public `instruments-info` endpoint.
/// Per-category tolerant (see [`list_tolerant`]) — mirrors okx's per-instType accumulate.
pub struct BybitCatalog;

impl CatalogProvider for BybitCatalog {
    fn venue(&self) -> &str {
        "bybit"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        Ok(list_tolerant(fetch_json))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use vike_catalog::AssetClass;

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
    /// VENUE`, `crates/vike-app-core/src/symbol_row.rs`'s `search_result_row`).
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
}
