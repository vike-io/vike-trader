//! Public spot + linear-perp + INVERSE-perp instrument catalog — the Bybit twin of binance's
//! `catalog` module, exposed as the venue's [`CatalogProvider`] contribution to the chart's
//! cross-venue symbol search. Reads the KEYLESS public `/v5/market/instruments-info` (`category=`
//! `spot` + `linear` + `inverse`, UNSIGNED GETs — no credentials, no signer), mapped to universal
//! [`Instrument`]s by [`parse_spot`] and the two perpetual parsers. The chart's live
//! bar feed ([`crate::feed`]) resolves the same symbol through the same routing decision this
//! catalog's rows satisfy, so a searched-and-selected Bybit symbol subscribes cleanly.
//!
//! Pure parse (`parse_spot`/`parse_perp`/`parse_inverse`, fixture-tested) is split from the blocking
//! `fetch_json` so the mapping stays testable without network — mirrors binance/catalog.rs's shape.
//!
//! ## What a row IS is decided once, here, from the row's own fields
//!
//! `docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`:
//! a spot row is a [`Contract::CryptoSpot`]; a derivative row's [`Contract`] is
//! [`derivative_contract`]'s answer — margining from `contractType`, the settle asset from
//! `settleCoin`, the funding interval from `fundingInterval`, an expiry from `deliveryTime` — and its
//! grid is [`derivative_properties`]'s. Both are ALSO what the exec plane's grid parser
//! (`crates/bridges/bybit/src/execution/rest/mod.rs`'s `parse_bybit_perp_instruments`) calls, so the picker row and
//! the grid the RiskGate rounds on cannot disagree about what one `instruments-info` row is. They
//! live in this module rather than in `execution/rest/mod.rs` because `execution/rest/mod.rs` rides the `execution` feature and this
//! catalog does not.
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
//! `crates/bridges/polymarket/src/catalog/mod.rs`'s `paged_markets`. Termination is triple: an absent,
//! null or EMPTY cursor (the venue's own end-of-listing signal — linear's last page answers `""`),
//! a cursor IDENTICAL to the one just spent, and the [`MAX_PAGES`] ceiling. The middle one is not
//! decoration: `list_instruments` runs on the picker's thread, and a venue echoing one cursor
//! forever is the shape that hangs a UI rather than the shape that returns wrong data.
//!
//! ## ⚠ `category=inverse` IS fetched now, and the order the work landed in is the load-bearing part
//!
//! It was not, until this. The block that stood here said so and said why: the only marker the core
//! vocabulary has for a bybit perp is [`vike_venues::PERP_SUFFIX`], and for THIS venue that suffix
//! MEANT `category=linear` at every wire-facing site — `history.rs`'s `rest_category`,
//! `feed/mod.rs`'s `perp_split` (which routed to the linear socket), and every
//! signed call in `execution/rest/mod.rs`/`execution/mod.rs`. An inverse perp surfaced as `BTCUSD.P` would have subscribed
//! a socket that answers `error:handler not found` and — if traded — signed an order carrying
//! `category: "linear"` for a symbol that does not exist in that category.
//!
//! **So the routing landed FIRST and the fetch second**, which is what
//! `docs/decisions/0061-an-instrument-names-its-kind.md` phase 4 asks for. `.P` no longer decides a
//! category anywhere in this crate: `crates/bridges/bybit/src/instrument/listing.rs`'s `perp_book` asks the
//! VENUE which derivative book carries a symbol, `history.rs`'s `Category` carries the answer to the
//! REST URL, and `feed/mod.rs`'s `websocket_host` carries it to one of three STRICT sockets. Only then
//! does the `inverse` [`listing_url`] exist. The test that used to pin the fetch as absent
//! (`catalog_tests::an_inverse_row_reaches_the_picker_only_if_it_routes`) was that ORDER written as
//! a gate, and it is rewritten rather than deleted — it now pins the invariant it was protecting: a
//! fetched inverse row must carry a class that routes.
//!
//! The EXEC plane signs an inverse row in its own book too (decision 0231 for an admitted symbol, the bybit
//! inverse amendment of decision 0258 for a mount: `crates/bridges/bybit/src/execution/mod.rs`'s `mount_money`), so an
//! inverse instrument the picker offers charts, backfills and trades.
//!
//! ## ⚠ Dated futures are LISTED under their own name, never under `.P`, and the picker shows none
//!
//! MEASURED 2026-09-16: `category=inverse` serves 28 Trading rows — 22 `InversePerpetual` and 6
//! `InverseFutures` (`BTCUSDZ26`, `ETHUSDH27`, …); 2026-10-11 it served 22 and 4; `category=linear` serves
//! dated `LinearFutures` (`ETHUSDT-27NOV26`) beside its perpetuals. The pages this catalog already fetches
//! carry them, and [`parse_dated`] mints each one keyed by the venue's OWN name, typed a [`Contract::Future`]
//! with its `deliveryTime` as the expiry: the listing ([`list_tolerant`], what the datahub KEEPS as
//! `instruments/bybit.json`) holds every book the exec plane signs in (2026-11-13, the owner's "remove all
//! hardcoded symbols"). [`parse_inverse`] and [`parse_perp`] still admit only the perpetuals, because `.P`
//! says PERPETUAL of a contract that does not expire (`catalog_tests::a_dated_future_never_wears_the_perpetual_suffix`).
//! The Symbol picker offers none of them: `vike_catalog::kind_of` gives a `CryptoFuture` row no picker kind,
//! so a row that the data plane cannot chart unclaimed (a bare symbol routes to SPOT) is never offered there.
//!
//! ## Every row carries the BOOK it lives in, as the venue's own category
//!
//! `spot`, `linear` or `inverse` (`crate::history::Category::wire`, what every V5 request is keyed on) is the
//! row's [`InstrumentVenueIds::group`]: a spot row's from the spot listing, a derivative's from its typed
//! margining (read off its own `contractType`, never the page's `category`). The keys stay what they were:
//! a spot pair is its bare name (`BTCUSDT`), a perpetual the bare name plus `.P` (`BTCUSDT.P`), a dated
//! future its bare name (`ETHUSDT-27NOV26`, `BTCUSDZ26`), so a spot pair and a perpetual of one name never
//! share a key, and a reader that holds an exec name (`DOGEUSDT.S` is the spot pair there, a bare
//! `BTCUSDT` the perpetual) finds its row with the lane suffix grammar alone and reads the book off the row.

use serde_json::Value;
use vike_bridge_core::catalog_read::{
    CatalogPage, CatalogPageWalk, CatalogPageWalkEnd, catalog_read_tolerant,
};
use vike_bridge_core::json::{get_f64, get_upper, json_int};
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AssetClass, Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily,
    ContractPerp, Instrument, InstrumentVenueIds, Margining, SymbolProperties,
};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::history::Category;

/// The venue name every row of this catalog carries.
const VENUE: &str = "bybit";

/// The row's BOOK as the venue's own category word (the module doc's last section), as its wire ids.
fn book_ids(book: Category) -> InstrumentVenueIds {
    InstrumentVenueIds { group: Some(book.wire().to_string()), ..InstrumentVenueIds::default() }
}

/// A derivative row's book, from its typed margining ([`derivative_contract`] types only these two).
fn derivative_ids(contract: &Contract) -> InstrumentVenueIds {
    match contract.margining() {
        Some(Margining::Linear) => book_ids(Category::Linear),
        Some(Margining::Inverse) => book_ids(Category::Inverse),
        _ => InstrumentVenueIds::default(),
    }
}

/// Bybit's documented `limit` ceiling for `instruments-info` (the venue accepts `[1, 1000]`): every
/// listing URL asks the venue maximum page ([`listing_url`]).
const PAGE_LIMIT: usize = 1000;

/// Defensive ceiling on pages fetched per category — 20 pages × the 1000-row `limit` the URLs ask
/// for = 20k rows, far above any category's live size (the largest, `linear`, is 870), so the crawl
/// can never run unbounded if Bybit stops sending an empty terminal cursor. The twin of
/// polymarket's `GAMMA_MAX_PAGES`.
const MAX_PAGES: usize = 20;

/// The keyless `instruments-info` path, on the `live` `data` `rest` base.
const INFO_PATH: &str = "/v5/market/instruments-info";

/// **One category's full listing at `base`**, the FIRST page's URL (`?category=<c>&limit=1000`): the
/// picker's `spot`, `linear` and the COIN-SETTLED `inverse` listing (28 Trading rows on 2026-09-16, of
/// which [`parse_inverse`] mints the 22 perpetuals; unreachable until the routing landed, see the module
/// doc for why that order was not optional), and the three `crate::instrument::listing` reads. `base` is the
/// `live` `data` `rest` row: a demo account's facts and picker read MAINNET, as they always have.
pub(crate) fn listing_url(base: &str, category: &str) -> String {
    format!("{base}{INFO_PATH}?category={category}&limit={PAGE_LIMIT}")
}

/// Every `status: "Trading"` row of one `{result:{list}}` page that names a symbol, paired with that
/// symbol upper-cased, in listing order. The one row filter every parser here shares.
fn trading_rows(payload: &Value) -> impl Iterator<Item = (&Value, String)> {
    payload
        .get("result")
        .and_then(|r| r.get("list"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.get("status").and_then(Value::as_str) == Some("Trading"))
        .filter_map(|e| {
            let symbol = get_upper(e, "symbol").unwrap_or_default();
            (!symbol.is_empty()).then_some((e, symbol))
        })
}

/// **What one DERIVATIVE `instruments-info` row is**, from the row's own fields — the one place
/// this crate decides it (the catalog's perpetual rows and the exec plane's
/// `crates/bridges/bybit/src/execution/rest/mod.rs`'s `parse_bybit_perp_instruments` both call this).
///
/// * `contractType` names the margining and whether the contract is dated: `LinearPerpetual` /
///   `InversePerpetual` are a [`Contract::Perp`], `LinearFutures` / `InverseFutures` a
///   [`Contract::Future`]. bybit lists no quanto contract.
/// * `settleCoin` is the settle asset.
/// * The contract's VALUE currency follows the margining: a linear contract's quantity is the base
///   coin (`qtyStep` `0.001` BTC on `BTCUSDT`), an inverse contract's is the quote, USD
///   (`qtyStep` `1` on `BTCUSD`). Both coins are read off the row.
/// * `fundingInterval` is MINUTES (`480` = 8 h, `240` on `HYPEUSD`, `0` on a dated future);
///   absent or `0` is `None`.
/// * `deliveryTime` is epoch ms, `"0"` on a live perpetual: a dated future's expiry, and on a
///   perpetual a non-zero value is the venue scheduling its settlement (`settling_ms`).
///
/// ⚠ **`contractType` is the ROW's word; `category` is the REQUEST's.** `category` is the query
/// parameter the fetch sends and the venue echoes ONCE at `result.category`, beside the list — and
/// MEASURED 2026-09-16, `?category=linear&symbol=XRPUSD` ANSWERS with an `InversePerpetual` row. So
/// the margining is read off the row, never off the URL that fetched it.
///
/// `None` — no contract this row can honestly be — when `contractType` is absent, empty or a word
/// this parser has not read about (the four values are a closed set today, and a fifth means
/// something new), when `settleCoin` is absent (a perpetual whose settle asset is unknown is not a
/// contract anyone can value), or when a dated future carries no positive `deliveryTime`.
/// ⚠ **The payload carries no contract multiplier**, so nothing here or in [`derivative_properties`]
/// sets `contract_size`: one inverse contract is one USD of notional, which
/// `SymbolProperties::multiplier()` cannot express — the exec plane states it as the inverse contract
/// value instead (`crates/bridges/bybit/src/execution/mod.rs`'s `mount_money`, decision 0170).
pub fn derivative_contract(entry: &Value) -> Option<Contract> {
    let (margining, dated, value_key) = match entry.get("contractType").and_then(Value::as_str)? {
        "LinearPerpetual" => (Margining::Linear, false, "baseCoin"),
        "InversePerpetual" => (Margining::Inverse, false, "quoteCoin"),
        "LinearFutures" => (Margining::Linear, true, "baseCoin"),
        "InverseFutures" => (Margining::Inverse, true, "quoteCoin"),
        _ => return None,
    };
    let settle = get_upper(entry, "settleCoin").unwrap_or_default();
    if settle.is_empty() {
        return None;
    }
    let value_ccy = get_upper(entry, value_key);
    let delivery_ms = entry.get("deliveryTime").and_then(json_int).filter(|&t| t > 0);
    if dated {
        return Some(Contract::Future(ContractFuture {
            margining,
            settle,
            value_currency: value_ccy,
            expiry_ms: delivery_ms?,
            // bybit's row names no tenor and no delivery method (`displayName` `BTCUSD1225` is a
            // label, not a field) — unknown, never guessed from the symbol.
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family: ContractFutureFamily::Crypto,
        }));
    }
    let funding_interval_ms = entry
        .get("fundingInterval")
        .and_then(json_int)
        .and_then(|minutes| u64::try_from(minutes).ok())
        .filter(|&minutes| minutes > 0)
        .and_then(|minutes| minutes.checked_mul(60_000));
    Some(Contract::Perp(ContractPerp {
        margining,
        settle,
        value_currency: value_ccy,
        funding_interval_ms,
        settling_ms: delivery_ms,
    }))
}

/// **The grid of one DERIVATIVE `instruments-info` row**: `priceFilter.tickSize` and
/// `lotSizeFilter`'s `qtyStep` (NOT a spot row's `basePrecision`), `minOrderQty`, `maxOrderQty` and
/// `minNotionalValue` (NOT a spot row's `minOrderAmt`), plus the class of `contract` — the SAME
/// [`derivative_contract`] answer the caller holds, so the grid's class and the row's contract are
/// one decision. Everything else absent: `priceFilter` reports ONE `tickSize` (flat grid, no tiers),
/// there is no venue taker hold, and the payload carries no multiplier (see
/// [`derivative_contract`]). FRU rather than an exhaustive literal so a new [`SymbolProperties`]
/// field costs this parser nothing.
pub fn derivative_properties(entry: &Value, contract: Option<&Contract>) -> SymbolProperties {
    SymbolProperties {
        tick_size: get_f64(&entry["priceFilter"], "tickSize"),
        step_size: get_f64(&entry["lotSizeFilter"], "qtyStep"),
        min_qty: get_f64(&entry["lotSizeFilter"], "minOrderQty"),
        max_qty: get_f64(&entry["lotSizeFilter"], "maxOrderQty"),
        min_notional: get_f64(&entry["lotSizeFilter"], "minNotionalValue"),
        asset_class: contract.map(Contract::class),
        ..Default::default()
    }
}

/// The grid of one SPOT row: `priceFilter.tickSize`, `lotSizeFilter.basePrecision` (the base-coin
/// quantity step — a spot row has no `qtyStep`), `minOrderQty`, `maxOrderQty` and `minOrderAmt`
/// (the quote-coin minimum order value — a spot row has no `minNotionalValue`).
pub(crate) fn spot_properties(entry: &Value) -> SymbolProperties {
    SymbolProperties {
        tick_size: get_f64(&entry["priceFilter"], "tickSize"),
        step_size: get_f64(&entry["lotSizeFilter"], "basePrecision"),
        min_qty: get_f64(&entry["lotSizeFilter"], "minOrderQty"),
        max_qty: get_f64(&entry["lotSizeFilter"], "maxOrderQty"),
        min_notional: get_f64(&entry["lotSizeFilter"], "minOrderAmt"),
        asset_class: Some(Contract::CryptoSpot.class()),
        ..Default::default()
    }
}

/// Parse the V5 `{result:{list}}` spot envelope into [`Contract::CryptoSpot`] [`Instrument`]s,
/// each carrying its spot grid.
pub fn parse_spot(payload: &Value) -> Vec<Instrument> {
    trading_rows(payload)
        .map(|(e, symbol)| {
            Instrument::new(
                VENUE,
                symbol,
                get_upper(e, "baseCoin").unwrap_or_default(),
                get_upper(e, "quoteCoin").unwrap_or_default(),
                Contract::CryptoSpot,
            )
            .with_properties(spot_properties(e))
            .with_wire(book_ids(Category::Spot))
        })
        .collect()
}

/// Parse the V5 `{result:{list}}` linear-category envelope into linear [`Contract::Perp`]
/// [`Instrument`]s (`contractType == "LinearPerpetual"` — the `linear` category also lists dated
/// `LinearFutures`).
pub fn parse_perp(payload: &Value) -> Vec<Instrument> {
    parse_perpetuals(payload, "LinearPerpetual")
}

/// Parse the V5 `{result:{list}}` INVERSE-category envelope into inverse [`Contract::Perp`]
/// [`Instrument`]s (`contractType == "InversePerpetual"`; the dated `InverseFutures` rows are
/// deliberately dropped — see the module doc).
///
/// ⚠ **The same `Perp` variant and the same `.P` spelling as its linear twin, and that is the
/// point rather than an economy.** `.P` says PERPETUAL — which an inverse perpetual is. What
/// separates the two rows for a MACHINE is the contract's [`Margining`] and settle asset, read off
/// the row, beside the venue's verbatim [`Instrument::contract_type`]; what separates them for a
/// HUMAN is [`contract_label`], painted in the picker's description column. Neither is the symbol
/// string, and neither is the suffix.
///
/// There is no id collision to design around: MEASURED 2026-09-16, `linear ∩ inverse` is EMPTY, so
/// no linear perp is named `BTCUSD` and `BTCUSD.P` names exactly one instrument.
/// `crates/bridges/bybit/src/instrument/listing.rs` carries that measurement and the `PerpListing::Both` arm
/// that turns it into a gate.
pub fn parse_inverse(payload: &Value) -> Vec<Instrument> {
    parse_perpetuals(payload, "InversePerpetual")
}

/// Parse the V5 `{result:{list}}` envelope of EITHER derivative category into the DATED [`Contract::Future`]
/// [`Instrument`]s it carries (`contractType` `LinearFutures` or `InverseFutures`), each keyed by the venue's OWN
/// bare symbol (`ETHUSDT-27NOV26`, `BTCUSDZ26`): a dated contract's name is already unambiguous, so it takes no
/// `.P` (which says PERPETUAL) and collides with no spot pair.
///
/// Read by [`perp_symbol`] (decision 0231: the exec plane's facts need the row's expiry and grid) and by the
/// listing ([`list_tolerant`]), so the KEPT list holds every dated contract the exec plane signs; the Symbol
/// picker shows none of them (the module doc's dated-futures section).
pub fn parse_dated(payload: &Value) -> Vec<Instrument> {
    trading_rows(payload)
        .filter(|(e, _)| {
            matches!(
                e.get("contractType").and_then(Value::as_str),
                Some("LinearFutures" | "InverseFutures")
            )
        })
        .filter_map(|(e, symbol)| {
            let contract = derivative_contract(e)?;
            let word = e.get("contractType").and_then(Value::as_str)?;
            Some(
                Instrument::new(
                    VENUE,
                    symbol,
                    get_upper(e, "baseCoin").unwrap_or_default(),
                    get_upper(e, "quoteCoin").unwrap_or_default(),
                    contract.clone(),
                )
                .with_description(contract_label(
                    Some(word),
                    Some(get_upper(e, "settleCoin").unwrap_or_default().as_str()),
                ))
                .with_properties(derivative_properties(e, Some(&contract)))
                .with_wire(derivative_ids(&contract))
                .with_venue_word(word),
            )
        })
        .collect()
}

/// A `linear` page as the listing keeps it: its perpetuals (`.P`) and its dated futures (their own names).
fn parse_linear_page(payload: &Value) -> Vec<Instrument> {
    [parse_perp(payload), parse_dated(payload)].concat()
}

/// An `inverse` page as the listing keeps it: its perpetuals (`.P`) and its dated futures (their own names).
fn parse_inverse_page(payload: &Value) -> Vec<Instrument> {
    [parse_inverse(payload), parse_dated(payload)].concat()
}

/// The shared body of [`parse_perp`]/[`parse_inverse`]: every Trading row whose `contractType` is
/// exactly `want`, minted as a `.P`-suffixed [`Contract::Perp`] carrying its grid.
///
/// Gating on the EXACT contract type (rather than on the category the URL asked for) is what keeps
/// dated futures out of both: `category=linear` serves `LinearFutures` and `category=inverse` serves
/// `InverseFutures`, and neither is a perpetual.
///
/// A row [`derivative_contract`] cannot type (in practice: no `settleCoin`) is SKIPPED, never
/// minted with a guessed settle asset, and the skip is warned per page so a venue that stopped
/// sending the field cannot empty the picker silently.
fn parse_perpetuals(payload: &Value, want: &str) -> Vec<Instrument> {
    let mut untyped = 0usize;
    let out: Vec<Instrument> = trading_rows(payload)
        .filter(|(e, _)| e.get("contractType").and_then(Value::as_str) == Some(want))
        .filter_map(|(e, symbol)| {
            let Some(contract) = derivative_contract(e) else {
                untyped += 1;
                return None;
            };
            let properties = derivative_properties(e, Some(&contract));
            let book = derivative_ids(&contract);
            // ⚠ **THE HUMAN HALF.** The owner's 2026-09-16 ruling: *"if we search by catalog and
            // enter BTCUSD — it has to show us TWO instruments, and the inverse one has to have a
            // label as inverse."* This is that label. See `contract_label`.
            let label = contract_label(
                Some(want),
                Some(get_upper(e, "settleCoin").unwrap_or_default().as_str()),
            );
            // `.P` suffix (TradingView convention): a distinct vike symbol so a perpetual doesn't
            // collide with its spot twin (both are `BTCUSDT` on Bybit). The feed strips the `.P`
            // back to the exchange symbol at the WS/REST boundary, and `history.rs`'s `route_target`
            // asks the venue WHICH derivative book the stripped symbol lives in.
            let instrument = Instrument::new(
                VENUE,
                format!("{symbol}.P"),
                get_upper(e, "baseCoin").unwrap_or_default(),
                get_upper(e, "quoteCoin").unwrap_or_default(),
                contract,
            )
            .with_description(label)
            .with_properties(properties)
            .with_wire(book)
            // ⚠ The venue's OWN word for what this contract IS, carried verbatim beside the typed
            // contract — the owner's 2026-09-16 ruling that a `contractType` we already receive
            // must be CARRIED rather than discarded and reconstructed from a symbol string.
            //
            // ⚠ **Nothing ROUTES on it, nor on the contract's margining.** The route is a
            // venue-LISTING membership (`crates/bridges/bybit/src/instrument/listing.rs`), not a reading
            // of these fields.
            .with_venue_word(want);
            Some(instrument)
        })
        .collect();
    if untyped > 0 {
        tracing::warn!(
            target: "vike_bybit::catalog",
            "{untyped} Trading {want} row(s) carried no settleCoin and were skipped"
        );
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
        Some("LinearFutures") => "Future".to_string(),
        Some("InverseFutures") => "Inverse future".to_string(),
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

/// One page's URL: the category's [`listing_url`] for the FIRST page (so a first-page URL is
/// byte-identical to it), `&cursor=…` appended for every page after it. The
/// cursor is echoed back exactly as Bybit sent it — the venue already percent-encodes the
/// `first=…&last=…` payload it hands out, so re-encoding here would produce a cursor it rejects.
fn page_url(base: &str, cursor: &str) -> String {
    if cursor.is_empty() { base.to_string() } else { format!("{base}&cursor={cursor}") }
}

/// The picker's walk of one category: a later page failing keeps the pages already read (a mid-crawl hiccup must
/// not wipe the venue from the picker), a cursor IDENTICAL to the one just spent ends it, and [`MAX_PAGES`] bounds it
/// with a warn line (`vike_bridge_core::catalog_read`'s module doc lists the knobs per walk).
const PICKER_WALK: CatalogPageWalk = CatalogPageWalk { max_pages: MAX_PAGES, strict: false };

/// Follow one category's cursor to exhaustion ([`PICKER_WALK`]), parsing each page as it arrives.
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
    let (out, end) = PICKER_WALK.walk(String::new(), |cursor| {
        let payload = fetch(&page_url(base, cursor))?;
        Ok(CatalogPage { rows: parse(&payload), next: next_cursor(&payload).map(str::to_string) })
    })?;
    match end {
        CatalogPageWalkEnd::Failed { page, error: e } => tracing::warn!(
            target: "vike_bybit::catalog",
            "page {page} of {base} failed (kept {} instruments from earlier pages): {e}",
            out.len()
        ),
        CatalogPageWalkEnd::Repeated { page } => tracing::warn!(
            target: "vike_bybit::catalog",
            "{base} repeated its page cursor at page {page}; stopping with {} instruments",
            out.len()
        ),
        CatalogPageWalkEnd::Ceiling => tracing::warn!(
            target: "vike_bybit::catalog",
            "{base} still offered a page after {MAX_PAGES}; stopping with {} instruments",
            out.len()
        ),
        CatalogPageWalkEnd::Listed => {}
    }
    Ok(out)
}

/// The body of `list_instruments` over an injectable fetch — per-category TOLERANT
/// (`vike_bridge_core::catalog_read::catalog_read_tolerant`): one endpoint failing must NOT drop the other (okx's
/// catalog documents the bug shape this prevents: a `?` on one endpoint wiped the whole venue from the Symbol
/// picker). Accumulate what succeeds; warn (not fail) on each miss. Split from the provider so the tolerance is
/// testable without network.
///
/// Each category is crawled to exhaustion by [`paged`] rather than read once — see the module doc
/// for the truncation that cost, and for the order in which `category=inverse` became a third row.
/// The two derivative pages keep their dated futures beside their perpetuals (no extra request).
fn list_tolerant(
    base: &str,
    fetch: impl Fn(&str) -> Result<Value, CatalogError>,
) -> Vec<Instrument> {
    catalog_read_tolerant(
        [
            ("spot", parse_spot as fn(&Value) -> Vec<Instrument>),
            ("linear", parse_linear_page as fn(&Value) -> Vec<Instrument>),
            ("inverse", parse_inverse_page as fn(&Value) -> Vec<Instrument>),
        ],
        |(label, parse)| paged(&listing_url(base, label), *parse, &fetch),
        |(label, _), e| tracing::warn!(target: "vike_bybit::catalog", "{label} fetch failed (skipped): {e}"),
    )
}

/// The keyless `instruments-info` endpoint at `urls`' `live` `data` `rest` row, with no query: the
/// per-symbol reads ([`perp_symbol_at`]) add `?category=…&symbol=…` themselves. The symbol facts read
/// MAINNET for a demo account too, as they always have.
fn info_url(urls: &VenueUrls) -> Result<String, CatalogError> {
    let base = urls
        .url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")
        .map_err(|refusal| CatalogError(refusal.to_string()))?;
    Ok(format!("{base}{INFO_PATH}"))
}

/// Whether `symbol` can ride in a query string unencoded (a bybit symbol is letters and digits).
fn is_plain_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// One `instruments-info?category=<category>&symbol=<symbol>` page of `base`, checked: a non-2xx, an
/// unparseable body or a `retCode` other than 0 is an `Err`, so a failed read is never an empty list.
fn symbol_page(base: &str, category: &str, symbol: &str) -> Result<Value, CatalogError> {
    let url = format!("{base}?category={category}&symbol={symbol}");
    let agent = vike_bridge_core::http::blocking_agent();
    let response =
        vike_bridge_core::http::get_raw(&agent, &url, VENUE, &[]).map_err(CatalogError)?;
    if !(200..300).contains(&response.status) {
        return Err(CatalogError(format!(
            "{VENUE} {category}: HTTP {}: {}",
            response.status,
            vike_bridge_core::http::body_head(&response.body)
        )));
    }
    let page: Value = serde_json::from_str(&response.body)
        .map_err(|e| CatalogError(format!("{VENUE} {category}: JSON: {e}")))?;
    match page.get("retCode").and_then(Value::as_i64) {
        Some(0) => Ok(page),
        _ => Err(CatalogError(format!(
            "{VENUE} {category}: the venue refused the read: {}",
            page.get("retMsg").and_then(Value::as_str).unwrap_or("no retMsg")
        ))),
    }
}

/// **ONE derivative symbol's rows**, `instruments-info?symbol=` (no 1000-row pages), for the symbol-facts
/// lookup. `symbol` is the venue's own name (`BTCUSDT`, no `.P`; or a dated contract's, `ETHUSDT-27NOV26`);
/// a perpetual's row carries a `.P` id, a dated contract's its own name ([`parse_dated`]).
///
/// The `linear` read answers for an inverse symbol too (MEASURED 2026-09-16, see
/// `crates/bridges/bybit/src/execution/mod.rs`'s `fetch_bybit_instrument`), and the parsers run over it so the
/// facts can refuse an inverse contract by NAME; a symbol `linear` does not carry is then asked of
/// `inverse`. ⚠ STRICT across the two reads: the second failing after the first came back empty is an
/// `Err`, never "no such symbol" (a half-read is not an answer). `Ok(vec![])` = neither category
/// carries a Trading perpetual or dated future of that name.
///
/// `urls` are the venue's rows; the read is at the `live` `data` `rest` row (a refusal is an `Err`).
pub fn perp_symbol(urls: &VenueUrls, symbol: &str) -> Result<Vec<Instrument>, CatalogError> {
    perp_symbol_at(&info_url(urls)?, symbol)
}

/// **ONE spot pair's row**, `instruments-info?category=spot&symbol=` (decision 0231), for the symbol-facts lookup.
/// `symbol` is the venue's pair name (`DOGEUSDT`, no `.S`); the row is the picker's own ([`parse_spot`]: keyed by
/// the bare name, a Trading pair only). STRICT like [`perp_symbol`]: a failed read is an `Err`, and `Ok(vec![])`
/// is a pair the venue does not list as Trading. `urls` as for [`perp_symbol`].
pub fn spot_symbol(urls: &VenueUrls, symbol: &str) -> Result<Vec<Instrument>, CatalogError> {
    spot_symbol_at(&info_url(urls)?, symbol)
}

/// [`spot_symbol`] over an explicit endpoint (the tests' loopback double).
fn spot_symbol_at(base: &str, symbol: &str) -> Result<Vec<Instrument>, CatalogError> {
    if !is_plain_symbol(symbol) {
        return Err(CatalogError(format!("{VENUE}: `{symbol}` is not a spot pair spelling")));
    }
    Ok(parse_spot(&symbol_page(base, "spot", symbol)?))
}

/// [`perp_symbol`] over an explicit endpoint (the tests' loopback double).
fn perp_symbol_at(base: &str, symbol: &str) -> Result<Vec<Instrument>, CatalogError> {
    if !is_plain_symbol(symbol) {
        return Err(CatalogError(format!(
            "{VENUE}: `{symbol}` is not a perpetual symbol spelling"
        )));
    }
    let linear = symbol_page(base, "linear", symbol)?;
    let mut rows = parse_perp(&linear);
    rows.extend(parse_inverse(&linear));
    rows.extend(parse_dated(&linear));
    if !rows.is_empty() {
        return Ok(rows);
    }
    let inverse = symbol_page(base, "inverse", symbol)?;
    let mut rows = parse_inverse(&inverse);
    rows.extend(parse_dated(&inverse));
    Ok(rows)
}

/// The bybit venue's `CatalogProvider` contribution: spot (`CryptoSpot`) + linear and inverse
/// perps (`CryptoPerp`) and dated futures (`CryptoFuture`, kept but never offered by the picker), all
/// bulk-enumerable off the keyless public `instruments-info` endpoint.
/// Per-category tolerant (see [`list_tolerant`]) — mirrors okx's per-instType accumulate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProviderBybit {
    /// The `live` `data` `rest` base the listings are read at.
    base: String,
}

impl CatalogProviderBybit {
    /// The picker over the venue's rows, its base resolved ONCE here.
    ///
    /// # Errors
    /// The refusal naming the missing row.
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(CatalogProviderBybit {
            base: urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?.to_string(),
        })
    }
}

impl CatalogProvider for CatalogProviderBybit {
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
        Ok(list_tolerant(&self.base, fetch_json))
    }
}

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;

#[path = "catalog_symbol_tests.rs"]
#[cfg(test)]
mod catalog_symbol_tests;
