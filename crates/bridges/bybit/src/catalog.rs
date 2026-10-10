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
//! (`catalog_tests::an_inverse_row_reaches_the_picker_only_if_it_routes`) was that ORDER written as
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
//! for that shrunken hole is `catalog_tests::inverse_dated_futures_stay_unfetched` — the same
//! shape as the old whole-category pin, scoped to what is actually still unreachable.

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, FieldMap, Instrument, parse_with};
use vike_model::AssetClass;

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
/// `crates/vike-app-core/src/ui/symbol_row.rs`'s `search_result_row` paints `raw_symbol | description |
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

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
