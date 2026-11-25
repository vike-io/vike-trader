//! **IG's market LIST**: every epic the account's categories list, for the datahub's kept list
//! (`<project>/instruments/ig.json`; `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`,
//! amended 2026-11-16). `GET /categories` (v1), then every page of `GET /categories/{code}/instruments` (v1,
//! [`PAGE_SIZE`] rows a page, the most IG serves), over the facts session of a demo account that RUNS in this gatewayhub
//! (`crate::operator_row::hold_source`, kept from the account's start), never a second login beside it: a session
//! that has not logged in yet (no order look so far) makes its ONE login for the refresh, the same one the account's
//! first order would make (2026-11-16, the coordinator relaying the owner's rule). [`CatalogProviderIgList`] is the
//! gatewayhub's ig entry (`crates/vike-gatewayhub/src/rest/catalog.rs`'s `CatalogProviders::real`).
//!
//! # Read only on the operator's command
//!
//! The owner's rulings of 2026-11-16: no timer asks for it (the datahub refreshes no kept list on a timer, and its
//! properties tape never asks ig); the list is read when the operator refreshes it
//! (`vike-cli data catalog refresh --venue ig`, the Data Manager's Refresh), and the datahub writes `ig.json` then. It is about 17 reads (measured on the demo 2026-11-16: `/categories`, one page for each of eight
//! categories, eight pages of EQUITIES), each one a LISTING read of the source's per-minute cap
//! (`crate::instrument`'s `LISTING_SHARE_DIVISOR`: a quarter of the cap), so the walk takes about three minutes and
//! leaves the account's first looks and its refresh reserve their share.
//!
//! # A short page is not the end (measured 2026-11-16)
//!
//! IG pages BEFORE it filters for the account: INDICES answered 168 rows on its only page for a `totalResults` of
//! 376, EQUITIES 948 rows on page 0 of 8. The rows it leaves out are not visible to this CFD account by search
//! either: every index epic IG's own search names for `FTSE` is on the page, and the page carries no daily-funded-bet
//! (`DAILY`/`TODAY`) form. So the end of a category is its `metadata.totalPages`, never a page shorter than asked, and
//! a page that states no `totalPages` fails the walk (the kept list stays as it was).
//!
//! # What a row says (`page_rows`)
//!
//! `crate::catalog`'s row (`row_of`): the epic, `instrumentName`, the pair an FX form's epic states as its base and
//! quote (`pair_of`: the page carries no currency), and the `Contract` of `instrumentType` and `expiry`
//! (a row that states no type, as CRYPTOCURRENCY's and KNOCKOUTS' rows do, is typed by its category's code), and IG's
//! own `lotSize` and `scalingFactor` verbatim where given (`vike_model::Instrument::venue_figures`, informational).
//! No grid: an epic's tick, contract size and minimum deal are its facts (`GET /markets/{epic}`, `crate::instrument`'s
//! decision-0227 path), read the first time it is picked or traded.
//!
//! One row per epic: an OPTIONS row (an underlying and a maturity, measured: its `epic` is the underlying's, with
//! `referenceEpic` beside it) is not a market of its own and is left out, and an epic two categories list keeps its
//! first row.

use std::collections::HashSet;

use serde_json::Value;
use vike_bridge_core::json::json_number;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{AssetClass, Instrument};

use crate::operator_row::held_source;
use crate::refusals::IgSymbolRefusal;

/// Rows asked for per page: the most `GET /categories/{id}/instruments` serves (IG's reference: 1 to 1000).
pub const PAGE_SIZE: u32 = 1000;

/// The most pages one category is read for, a bound on a `totalPages` that runs away (EQUITIES stated 8).
pub(crate) const MAX_PAGES: u64 = 50;

/// IG's own figures a list row carries verbatim where given.
const FIGURES: [&str; 2] = ["lotSize", "scalingFactor"];

/// The category codes of a `GET /categories` body, in IG's order. A code is put in a path, so one that is not a
/// plain word refuses the walk.
///
/// # Errors
/// No `categories` list, an empty one, or a code that is not letters, digits and underscores.
pub(crate) fn category_codes(body: &Value) -> Result<Vec<String>, IgSymbolRefusal> {
    let list = body
        .get("categories")
        .and_then(Value::as_array)
        .ok_or(IgSymbolRefusal::ListNoCategories)?;
    let mut codes = Vec::new();
    for code in list.iter().map(|c| c.get("code").and_then(Value::as_str).unwrap_or("")) {
        if code.is_empty() || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(IgSymbolRefusal::ListCategoryNotAWord { code: code.to_string() });
        }
        codes.push(code.to_string());
    }
    if codes.is_empty() {
        return Err(IgSymbolRefusal::ListNoCategory);
    }
    Ok(codes)
}

/// **The rows of one page of category `code`, and the category's `totalPages`** (module doc, "What a row says").
///
/// # Errors
/// The page states no `metadata.totalPages` or carries no `instruments` list.
pub(crate) fn page_rows(
    code: &str,
    body: &Value,
) -> Result<(Vec<Instrument>, u64), IgSymbolRefusal> {
    let total_pages = body
        .pointer("/metadata/totalPages")
        .and_then(Value::as_u64)
        .ok_or_else(|| IgSymbolRefusal::ListPageStatesNoTotal { code: code.to_string() })?;
    let rows = body
        .get("instruments")
        .and_then(Value::as_array)
        .ok_or_else(|| IgSymbolRefusal::ListPageCarriesNoRows { code: code.to_string() })?;
    let text = |row: &Value, key: &str| {
        row.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
    };
    let rows = rows
        .iter()
        .filter(|row| row.get("referenceEpic").is_none())
        .filter_map(|row| {
            let epic = text(row, "epic");
            if epic.is_empty() {
                return None;
            }
            let kind = Some(text(row, "instrumentType")).filter(|k| !k.is_empty());
            let expiry = text(row, "expiry");
            let expiry = Some(expiry.as_str()).filter(|e| !e.is_empty());
            let name = text(row, "instrumentName");
            let mut out =
                crate::catalog::row_of(&epic, kind.as_deref().unwrap_or(code), expiry, &name);
            for figure in FIGURES {
                if let Some(value) = row.get(figure).and_then(json_number) {
                    out = out.with_venue_figure(figure, value);
                }
            }
            Some(out)
        })
        .collect();
    Ok((rows, total_pages))
}

/// **Walk IG's list** with `get(path, query)` answering each `GET` (v1): `/categories`, then pages 0 to
/// `totalPages - 1` of every category, one row per epic.
///
/// # Errors
/// `get`'s words, or a body [`category_codes`] or [`page_rows`] refuses, or a category of more than [`MAX_PAGES`]
/// pages. Nothing is returned half-read.
pub(crate) fn walk(
    get: &mut dyn FnMut(&str, &str) -> Result<Value, IgSymbolRefusal>,
) -> Result<Vec<Instrument>, IgSymbolRefusal> {
    let codes = category_codes(&get("/categories", "")?)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for code in &codes {
        let mut page = 0;
        loop {
            let body = get(
                &format!("/categories/{code}/instruments"),
                &format!("pageNumber={page}&pageSize={PAGE_SIZE}"),
            )?;
            let (rows, total_pages) = page_rows(code, &body)?;
            out.extend(rows.into_iter().filter(|row| seen.insert(row.raw_symbol.clone())));
            page += 1;
            if page >= total_pages {
                break;
            }
            if page >= MAX_PAGES {
                return Err(IgSymbolRefusal::ListTooManyPages { code: code.clone(), total_pages });
            }
        }
    }
    Ok(out)
}

/// **IG's entry in the gatewayhub's catalog table**: the list (the module doc's walk) over the session a running demo account's
/// facts hold in this process (`crate::operator_row::hold_source`); with none, or an account that no longer runs,
/// [`CatalogProvider::no_session`] answers in words and nothing is asked.
#[derive(Clone, Copy, Debug, Default)]
pub struct CatalogProviderIgList;

impl CatalogProvider for CatalogProviderIgList {
    fn venue(&self) -> &str {
        "ig"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx, AssetClass::Cfd]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn no_session(&self) -> Option<vike_utility::errors::CarriedRefusal> {
        held_source().err()
    }
    /// A walk that failed is recorded in its words (`vike_catalog::held_list::note_read`, which `vike-cli trade health`
    /// shows through the gatewayhub's status) and forgotten by the next walk that succeeds.
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        let source = held_source().map_err(CatalogError)?;
        let listed = source.list_markets().map_err(|e| CatalogError(e.carried()));
        vike_catalog::held_list::note_read("ig", listed.as_ref().err().map(|e| e.0.text()));
        listed
    }
}

#[cfg(test)]
#[path = "market_list_tests.rs"]
mod market_list_tests;
