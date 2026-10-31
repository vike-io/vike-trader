//! Offline tests of the search-row mapping (`parse_markets`) and the injected-credential config.

use super::*;

/// A hand-written `/markets?searchTerm=` body (its `_provenance` says so) with one row per
/// mapped class and both payload expiry words plus dated rows.
const SEARCH: &str = include_str!("../tests/fixtures/markets_search.json");

fn cfd(underlying_kind: CfdUnderlying, product: CfdProduct, expiry: CfdExpiry) -> Contract {
    Contract::Cfd(CfdSpec {
        underlying_kind,
        underlying: None,
        product,
        expiry,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    })
}

fn by_epic<'a>(out: &'a [Instrument], epic: &str) -> &'a Instrument {
    out.iter().find(|i| i.raw_symbol == epic).unwrap_or_else(|| panic!("{epic} present"))
}

#[test]
fn search_fixture_maps_every_row_to_its_exact_contract() {
    let payload: Value = serde_json::from_str(SEARCH).expect("fixture is valid JSON");
    let out = parse_markets(&payload);
    let expected = [
        (
            "CS.D.EURUSD.MINI.IP",
            Contract::Fx(FxSpec {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None,
            }),
        ),
        ("IX.D.FTSE.CFD.IP", cfd(CfdUnderlying::Index, CfdProduct::Cfd, CfdExpiry::Rolling)),
        ("IX.D.FTSE.DAILY.IP", cfd(CfdUnderlying::Index, CfdProduct::SpreadBet, CfdExpiry::Dfb)),
        // A share CFD, not a cash equity.
        ("UA.D.AAPL.CASH.IP", cfd(CfdUnderlying::Share, CfdProduct::Cfd, CfdExpiry::Rolling)),
        // Dated: no month-to-instant convention exists, so Unknown rather than a guess.
        ("EN.D.LCO.Month1.IP", cfd(CfdUnderlying::Commodity, CfdProduct::Cfd, CfdExpiry::Unknown)),
        ("CS.D.BITCOIN.CFD.IP", cfd(CfdUnderlying::Crypto, CfdProduct::Cfd, CfdExpiry::Rolling)),
        ("IR.D.FGBL.Month2.IP", cfd(CfdUnderlying::Other, CfdProduct::Cfd, CfdExpiry::Unknown)),
        // IG options are not modelled as `Contract::Option`.
        ("OP.D.FTSE2.8300C.IP", cfd(CfdUnderlying::Other, CfdProduct::Cfd, CfdExpiry::Unknown)),
    ];
    assert_eq!(out.len(), expected.len());
    for (epic, contract) in expected {
        let inst = by_epic(&out, epic);
        assert_eq!(inst.contract, contract, "{epic}");
        assert_eq!(inst.venue, "ig");
        assert_eq!(inst.base, "", "{epic}: the search row carries no pair");
        assert_eq!(inst.quote, "", "{epic}: the search row carries no pair");
    }
    let eurusd = by_epic(&out, "CS.D.EURUSD.MINI.IP");
    assert_eq!(eurusd.description, "EUR/USD Mini");
    assert_eq!(eurusd.asset_class(), AssetClass::Fx);
    assert_eq!(eurusd.contract_type.as_deref(), Some("CURRENCIES"));
    let opt = by_epic(&out, "OP.D.FTSE2.8300C.IP");
    assert_eq!(opt.asset_class(), AssetClass::Cfd);
    assert_eq!(opt.contract_type.as_deref(), Some("OPT_INDICES"), "venue word kept verbatim");
}

#[test]
fn bonds_map_to_a_bond_cfd_and_an_empty_epic_is_skipped() {
    let payload = serde_json::json!({
        "markets": [
            { "epic": "XB.D.GILT.CFD.IP", "instrumentName": "Gilt", "instrumentType": "BONDS", "expiry": "-" },
            { "epic": "", "instrumentName": "Nameless", "instrumentType": "CURRENCIES", "expiry": "-" }
        ]
    });
    let out = parse_markets(&payload);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].contract, cfd(CfdUnderlying::Bond, CfdProduct::Cfd, CfdExpiry::Rolling));
}

/// The payload's `expiry` word wins; the epic's exact segments decide only when it is absent.
#[test]
fn product_and_expiry_payload_first_then_epic_segments() {
    use CfdExpiry::{Dfb, Rolling, Unknown};
    use CfdProduct::{Cfd, SpreadBet};
    let cases = [
        ("IX.D.FTSE.DAILY.IP", Some("-"), (Cfd, Rolling)),
        ("IX.D.FTSE.CFD.IP", Some("DFB"), (SpreadBet, Dfb)),
        ("IX.D.FTSE.CFD.IP", Some("JUN-26"), (Cfd, Unknown)),
        ("IX.D.DAX.DAILY.IP", None, (SpreadBet, Dfb)),
        ("CS.D.GBPUSD.TODAY.IP", None, (SpreadBet, Dfb)),
        ("IX.D.DOW.CFD.IP", None, (Cfd, Rolling)),
        ("CS.D.EURUSD.MINI.IP", None, (Cfd, Rolling)),
        // A substring is not a segment.
        ("IX.D.MINIDAX.IFD.IP", None, (Cfd, Unknown)),
        ("UA.D.AAPL.CASH.IP", None, (Cfd, Unknown)),
    ];
    for (epic, expiry, want) in cases {
        assert_eq!(product_and_expiry(epic, expiry), want, "{epic} / {expiry:?}");
    }
    // An empty payload word counts as absent.
    let payload = serde_json::json!({ "markets": [
        { "epic": "IX.D.DAX.DAILY.IP", "instrumentType": "INDICES", "expiry": "" }
    ]});
    assert_eq!(parse_markets(&payload)[0].contract, cfd(CfdUnderlying::Index, SpreadBet, Dfb));
}

#[test]
fn empty_payload_is_empty() {
    assert!(parse_markets(&serde_json::json!({})).is_empty());
    assert!(parse_markets(&serde_json::json!({ "markets": [] })).is_empty());
}

#[test]
fn missing_fields_never_panic() {
    let payload = serde_json::json!({ "markets": [ {} ] });
    assert!(parse_markets(&payload).is_empty());
}

/// The catalog must read the credentials it is HANDED, not the process environment.
/// Before this fix `new()` swept `std::env::vars()`, so a `.env`-only credential set — which
/// is the ONLY place this repo stores them — resolved to `None` and the venue silently
/// vanished from the Symbol picker.
#[test]
fn catalog_resolves_credentials_from_the_injected_account() {
    let fields = vike_secrets::account_credentials::CredentialFields::from_pairs([
        ("API_KEY", "key-xyz"),
        ("IDENTIFIER", "myuser"),
        ("PASSWORD", "s3cr3t"),
    ]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    assert!(
        IgCatalog::new(&creds).source.is_some(),
        "injected credentials must resolve without touching process env"
    );
}

/// An account holding nothing means no credentials — never a panic, and never a silent fallback to
/// process env.
#[test]
fn catalog_with_no_credentials_is_configless() {
    assert!(IgCatalog::new(&AccountCredentials::none()).source.is_none());
}
