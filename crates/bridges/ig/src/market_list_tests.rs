//! The walk and the rows over the pages the demo gateway answered on 2026-11-16
//! (`tests/fixtures/categories/pages.json`; EQUITIES pages 1 to 7 are hand-written, the fixture says so), with no
//! socket: `walk`'s `get` is answered from the fixture and every ask is recorded.

use serde_json::{Value, json};
use vike_catalog::{CatalogMode, CatalogProvider};
use vike_model::{
    Contract, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying, SymbolProperties,
};

use super::*;

const PAGES: &str = include_str!("../tests/fixtures/categories/pages.json");

fn fixture() -> Value {
    serde_json::from_str(PAGES).expect("the fixture parses")
}

/// `walk` over `doc`, recording each ask as `path?query`.
fn walk_over(doc: &Value) -> (Result<Vec<Instrument>, IgSymbolRefusal>, Vec<String>) {
    let mut asked = Vec::new();
    let mut get = |path: &str, query: &str| -> Result<Value, IgSymbolRefusal> {
        asked.push(format!("{path}?{query}"));
        if path == "/categories" {
            return Ok(doc["categories"].clone());
        }
        let code = path.trim_start_matches("/categories/").trim_end_matches("/instruments");
        let page: usize = query
            .strip_prefix("pageNumber=")
            .and_then(|q| q.split('&').next())
            .and_then(|n| n.parse().ok())
            .expect("a page number");
        doc["pages"][code].get(page).cloned().ok_or_else(|| IgSymbolRefusal::ListReadFailed {
            path: path.to_string(),
            why: format!("page {page} was never served"),
        })
    };
    let rows = walk(&mut get);
    (rows, asked)
}

fn row<'a>(rows: &'a [Instrument], epic: &str) -> &'a Instrument {
    rows.iter().find(|r| r.raw_symbol == epic).unwrap_or_else(|| panic!("{epic} is listed"))
}

/// **The whole list is every page of every category up to its `totalPages`, and a short page is not the end**: the
/// nine categories the demo gateway named, page 0 of each (each far shorter than the 1000 asked, INDICES 3 rows of a
/// `totalResults` 376 here and 168 there) and EQUITIES' pages 1 to 7 because it states 8: 17 reads, none past a
/// category's last page.
///
/// MUTATION PROOF: end a category on a page shorter than `PAGE_SIZE` and EQUITIES' pages 1 to 7 are never asked (10
/// reads, 24 rows).
#[test]
fn the_whole_list_is_every_page_of_every_category_to_its_total_pages() {
    let (rows, asked) = walk_over(&fixture());
    let rows = rows.expect("the list");
    assert_eq!(asked.len(), 17, "{asked:#?}");
    assert_eq!(asked[0], "/categories?");
    let equities: Vec<&String> = asked.iter().filter(|a| a.contains("/EQUITIES/")).collect();
    assert_eq!(equities.len(), 8, "{equities:#?}");
    assert!(equities[7].ends_with("pageNumber=7&pageSize=1000"), "{equities:#?}");
    assert!(asked.iter().all(|a| a == "/categories?" || a.ends_with("&pageSize=1000")));
    // 8 categories x 3 captured rows + 7 hand-written EQUITIES rows; OPTIONS' 3 rows name an underlying, not a market
    assert_eq!(rows.len(), 31, "{:?}", rows.iter().map(|r| &r.raw_symbol).collect::<Vec<_>>());
    assert!(
        rows.iter().any(|r| r.raw_symbol == "EF.D.HANDWRITTEN7.CASH.IP"),
        "the last page is read"
    );
}

/// **A row carries the epic, the name, the type or expiry, and IG's own `lotSize` and `scalingFactor` verbatim, and no
/// grid** (the facts are read for one epic when it is picked). A row that states no type is typed by its category.
#[test]
fn a_row_carries_epic_name_type_expiry_and_igs_figures_and_no_grid() {
    let rows = walk_over(&fixture()).0.expect("the list");
    let index = row(&rows, "IX.D.ASX.IFMOZ.IP");
    assert_eq!(index.description, "Australia 200 Cash (A$5)");
    assert_eq!(index.contract_type.as_deref(), Some("INDICES"));
    assert!(matches!(
        index.contract,
        Contract::Cfd(ref c) if c.underlying_kind == ContractCfdUnderlying::Index
            && c.expiry == ContractCfdExpiry::Rolling && c.product == ContractCfdProduct::Cfd
    ));
    assert_eq!(index.venue_figures.get("lotSize"), Some(&5.0));
    assert_eq!(index.venue_figures.get("scalingFactor"), Some(&1.0));

    let mini = row(&rows, "CS.D.EURUSD.MINI.IP");
    assert!(matches!(mini.contract, Contract::Fx(_)), "{:?}", mini.contract);
    assert_eq!(mini.venue_figures.get("lotSize"), Some(&1.0), "IG's lot, not the contract size");
    assert_eq!(mini.venue_figures.get("scalingFactor"), Some(&10_000.0));

    let bond = row(&rows, "IR.D.05YEAR100.FWM2.IP");
    assert_eq!(bond.contract_type.as_deref(), Some("RATES"));
    assert!(
        matches!(bond.contract, Contract::Cfd(ref c) if c.expiry == ContractCfdExpiry::Unknown),
        "IG's padded `   DEC-26` is a dated contract: {:?}",
        bond.contract
    );

    let coin = row(&rows, "CS.D.BITCOIN.CFM.IP");
    assert_eq!(coin.contract_type.as_deref(), Some("CRYPTOCURRENCY"), "typed by its category");
    assert!(matches!(
        coin.contract,
        Contract::Cfd(ref c) if c.underlying_kind == ContractCfdUnderlying::Crypto
    ));
    assert!(coin.venue_figures.is_empty(), "the crypto rows state no figures");
    assert_eq!(row(&rows, "IX.D.OBX.CFDBEAR.IP").contract_type.as_deref(), Some("KNOCKOUTS"));

    assert!(rows.iter().all(|r| r.properties == SymbolProperties::default() && r.venue == "ig"));
}

/// **One row per epic**: an OPTIONS row is an underlying and a maturity (its `epic` is the underlying's, measured), so
/// none is listed, and an epic two categories list keeps its first row.
#[test]
fn an_options_row_and_a_second_listing_of_an_epic_are_left_out() {
    let rows = walk_over(&fixture()).0.expect("the list");
    assert!(
        !rows.iter().any(|r| r.raw_symbol == "IX.D.NASDAQ.IFD.IP"),
        "the OPTIONS rows' underlying is not listed in their name"
    );
    let page = |rows: Value| json!({ "instruments": rows, "metadata": { "totalPages": 1 } });
    let doc = json!({
        "categories": { "categories": [{ "code": "INDICES" }, { "code": "COMMODITIES" }] },
        "pages": {
            "INDICES": [page(json!([{ "epic": "CC.D.X.UMEB.IP", "instrumentType": "COMMODITIES" }]))],
            "COMMODITIES": [page(json!([
                { "epic": "CC.D.X.UMEB.IP", "instrumentType": "INDICES" },
                { "epic": "CC.D.Y.UMEB.IP", "instrumentType": "COMMODITIES" },
            ]))],
        },
    });
    let rows = walk_over(&doc).0.expect("the list");
    let epics: Vec<(&str, Option<&str>)> =
        rows.iter().map(|r| (r.raw_symbol.as_str(), r.contract_type.as_deref())).collect();
    assert_eq!(
        epics,
        [("CC.D.X.UMEB.IP", Some("COMMODITIES")), ("CC.D.Y.UMEB.IP", Some("COMMODITIES"))]
    );
}

/// **A body whose end cannot be told, a code that is not a word, and a category that runs away each fail the whole
/// walk** (the kept list stays as it was), each in its own words.
#[test]
fn a_page_with_no_total_a_strange_code_and_a_runaway_category_fail_the_walk() {
    let one = |code: &str, page: Value| {
        let mut pages = serde_json::Map::new();
        pages.insert(code.to_string(), json!([page]));
        json!({ "categories": { "categories": [{ "code": code }] }, "pages": pages })
    };
    let no_total =
        one("FX", json!({ "instruments": [], "metadata": { "pageNumber": 0, "pageSize": 1000 } }));
    let why = walk_over(&no_total).0.expect_err("no totalPages");
    assert_eq!(why, IgSymbolRefusal::ListPageStatesNoTotal { code: "FX".to_string() });

    let strange = one("FX/../x", json!({ "instruments": [], "metadata": { "totalPages": 1 } }));
    let (out, asked) = walk_over(&strange);
    assert_eq!(
        out.expect_err("a path word"),
        IgSymbolRefusal::ListCategoryNotAWord { code: "FX/../x".to_string() }
    );
    assert_eq!(asked, ["/categories?"], "nothing is put in a path");

    let pages: Vec<Value> = (0..MAX_PAGES)
        .map(|_| json!({ "instruments": [], "metadata": { "totalPages": 1000 } }))
        .collect();
    let runaway = json!({ "categories": { "categories": [{ "code": "EQUITIES" }] }, "pages": { "EQUITIES": pages } });
    let (out, asked) = walk_over(&runaway);
    assert_eq!(
        out.expect_err("runaway"),
        IgSymbolRefusal::ListTooManyPages { code: "EQUITIES".to_string(), total_pages: 1000 }
    );
    assert_eq!(asked.len() as u64, 1 + MAX_PAGES);

    let none = json!({ "categories": { "categories": [] }, "pages": {} });
    assert_eq!(walk_over(&none).0.expect_err("no category"), IgSymbolRefusal::ListNoCategory);
}

/// The gatewayhub's entry: an enumerable FX and CFD list of ig.
#[test]
fn the_entry_is_an_enumerable_list_of_ig() {
    let entry = CatalogProviderIgList;
    assert_eq!(entry.venue(), "ig");
    assert_eq!(entry.mode(), CatalogMode::Enumerable);
    assert_eq!(entry.asset_classes(), [AssetClass::Fx, AssetClass::Cfd]);
}
