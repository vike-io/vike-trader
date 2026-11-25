//! Offline tests of the search-row mapping (`parse_markets`) and the injected-credential config.

use super::*;

/// A hand-written `/markets?searchTerm=` body (its `_provenance` says so) with one row per
/// mapped class and both payload expiry words plus dated rows.
const SEARCH: &str = include_str!("../tests/fixtures/markets_search.json");

fn cfd(
    underlying_kind: ContractCfdUnderlying,
    product: ContractCfdProduct,
    expiry: ContractCfdExpiry,
) -> Contract {
    Contract::Cfd(ContractCfd {
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
            Contract::Fx(ContractFx {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None,
            }),
        ),
        (
            "IX.D.FTSE.CFD.IP",
            cfd(ContractCfdUnderlying::Index, ContractCfdProduct::Cfd, ContractCfdExpiry::Rolling),
        ),
        (
            "IX.D.FTSE.DAILY.IP",
            cfd(
                ContractCfdUnderlying::Index,
                ContractCfdProduct::SpreadBet,
                ContractCfdExpiry::Dfb,
            ),
        ),
        // A share CFD, not a cash equity.
        (
            "UA.D.AAPL.CASH.IP",
            cfd(ContractCfdUnderlying::Share, ContractCfdProduct::Cfd, ContractCfdExpiry::Rolling),
        ),
        // Dated: no month-to-instant convention exists, so Unknown rather than a guess.
        (
            "EN.D.LCO.Month1.IP",
            cfd(
                ContractCfdUnderlying::Commodity,
                ContractCfdProduct::Cfd,
                ContractCfdExpiry::Unknown,
            ),
        ),
        (
            "CS.D.BITCOIN.CFD.IP",
            cfd(ContractCfdUnderlying::Crypto, ContractCfdProduct::Cfd, ContractCfdExpiry::Rolling),
        ),
        (
            "IR.D.FGBL.Month2.IP",
            cfd(ContractCfdUnderlying::Other, ContractCfdProduct::Cfd, ContractCfdExpiry::Unknown),
        ),
        // IG options are not modelled as `Contract::Option`.
        (
            "OP.D.FTSE2.8300C.IP",
            cfd(ContractCfdUnderlying::Other, ContractCfdProduct::Cfd, ContractCfdExpiry::Unknown),
        ),
    ];
    assert_eq!(out.len(), expected.len());
    for (epic, contract) in expected {
        let instrument = by_epic(&out, epic);
        assert_eq!(instrument.contract, contract, "{epic}");
        assert_eq!(instrument.venue, "ig");
        // Only the FX form IG names itself states its pair (`pair_of`); the payload carries no currency.
        let pair = if epic == "CS.D.EURUSD.MINI.IP" { ("EUR", "USD") } else { ("", "") };
        assert_eq!((instrument.base.as_str(), instrument.quote.as_str()), pair, "{epic}");
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
    assert_eq!(
        out[0].contract,
        cfd(ContractCfdUnderlying::Bond, ContractCfdProduct::Cfd, ContractCfdExpiry::Rolling)
    );
}

/// The payload's `expiry` word wins; the epic's exact segments decide only when it is absent.
#[test]
fn product_and_expiry_payload_first_then_epic_segments() {
    use ContractCfdExpiry::{Dfb, Rolling, Unknown};
    use ContractCfdProduct::{Cfd, SpreadBet};
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
    assert_eq!(
        parse_markets(&payload)[0].contract,
        cfd(ContractCfdUnderlying::Index, SpreadBet, Dfb)
    );
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
        CatalogProviderIg::new(&creds, &VenueUrls::seed("ig"))
            .expect("the seed's rows")
            .source
            .is_some(),
        "injected credentials must resolve without touching process env"
    );
}

/// An account holding nothing means no credentials — never a panic, and never a silent fallback to
/// process env.
#[test]
fn catalog_with_no_credentials_is_configless() {
    let catalog = CatalogProviderIg::new(&AccountCredentials::none(), &VenueUrls::default());
    assert!(catalog.expect("absent creds look nothing up").source.is_none());
}

/// F-8: `vike_catalog::addressing_for("ig")` may name FEWER classes than this catalog mints, never
/// more (`crates/vike-catalog/src/addressing.rs`'s rule): a class the row names and no ig row can
/// be lets the datahub admit a seed of it. Held from the bridge side, which sees both. Red while
/// the row named `Equity` and `Index`.
#[test]
fn the_addressing_row_names_no_class_this_catalog_does_not_mint() {
    let catalog =
        CatalogProviderIg::new(&AccountCredentials::none(), &VenueUrls::default()).unwrap();
    for class in vike_catalog::addressing_for("ig").classes {
        assert!(
            catalog.asset_classes().contains(class),
            "ig addresses {class:?}, mints no row of it"
        );
    }
}

/// **IG names itself** (decision 0290 item 6, the owner's ruling of 2026-11-17): the Mini is the pair, the standard
/// `.CFD.IP` the pair + `.CFD`, and a share its code + `.CFD`; a knock-out states why it has no name; an epic of no such
/// form states nothing (the shared grammar spells it).
#[test]
fn a_row_states_igs_own_naming() {
    let user = vike_catalog::user_name_of_row;
    let mini = row_of("CS.D.EURUSD.MINI.IP", "CURRENCIES", Some("-"), "EUR/USD Mini");
    assert_eq!(user(&mini).as_deref(), Ok("EURUSD"));
    assert_eq!(vike_catalog::wire_name_of_row(&mini), "CS.D.EURUSD.MINI.IP");
    let standard = row_of("CS.D.EURUSD.CFD.IP", "CURRENCIES", Some("-"), "EUR/USD");
    assert_eq!(user(&standard).as_deref(), Ok("EURUSD.CFD"));
    assert_eq!(vike_catalog::wire_name_of_row(&standard), "CS.D.EURUSD.CFD.IP");
    assert_eq!(
        vike_catalog::kept_fiat_quote("ig", "EURUSD.CFD", Some(&standard)),
        Some(vike_model::FiatQuote::Code("USD")),
        "the `.CFD` name of a pair is quoted in the pair's quote"
    );
    assert_eq!(vike_catalog::fiat_quote_of("EURUSD.CFD"), Some("USD"), "and so is the name alone");
    for epic in ["CS.D.EURUSD.CFDBULL.IP", "IX.D.FTSE.CFDBEAR.IP"] {
        let why = user(&row_of(epic, "KNOCKOUTS", Some("-"), "KO")).expect_err("a knock-out");
        assert!(why.contains("knock-out") && why.contains(epic), "{why}");
    }
    let share = row_of("UA.D.AAPL.CASH.IP", "SHARES", Some("-"), "Apple Inc (24 Hours)");
    assert_eq!(user(&share).as_deref(), Ok("AAPL.CFD"));
    assert_eq!(row_of("CS.D.ZINC.CFD.IP", "CURRENCIES", Some("-"), "Zinc").naming, None);
}

/// **A pair's row states the currency its epic names** (the CI box's 2026-11-17 regression: IG's list rows stated no quote, so
/// a paper book refused `EURUSD` as quoted in a currency not known). The standard lot and the Mini state the pair; a
/// knock-out, a share, copper (typed CURRENCIES, its `PER` no currency) and a pair whose quote is no code this workspace
/// knows (`USDBRL`) state none, and a paper book refuses those in words.
#[test]
fn a_pairs_row_states_the_quote_its_epic_names() {
    let pair = |epic: &str, kind: &str| {
        let row = row_of(epic, kind, Some("-"), "");
        (row.base, row.quote)
    };
    let stated = |base: &str, quote: &str| (base.to_string(), quote.to_string());
    assert_eq!(pair("CS.D.EURUSD.CFD.IP", "CURRENCIES"), stated("EUR", "USD"));
    assert_eq!(pair("CS.D.USDJPY.MINI.IP", "CURRENCIES"), stated("USD", "JPY"));
    for (epic, kind) in [
        ("CS.D.EURUSD.CFDBULL.IP", "KNOCKOUTS"),
        ("UA.D.AAPL.CASH.IP", "SHARES"),
        ("CS.D.COPPER.CFD.IP", "CURRENCIES"),
        ("CS.D.USDBRL.CFD.IP", "CURRENCIES"),
        ("CS.D.GBPUSD.TODAY.IP", "CURRENCIES"),
        ("IX.D.DAX.IFD.IP", "INDICES"),
    ] {
        assert_eq!(pair(epic, kind), stated("", ""), "{epic}");
    }
    let row = row_of("CS.D.EURUSD.CFD.IP", "CURRENCIES", Some("-"), "EUR/USD");
    assert_eq!(
        vike_catalog::kept_fiat_quote("ig", "EURUSD", Some(&row)),
        Some(vike_model::FiatQuote::Code("USD"))
    );
}

/// **Copper is no currency pair** (the 2026-11-17 finding: the CI box's `ig.json` named `CS.D.COPPER.CFD.IP` `COPPER`, an FX
/// user name, because IG types copper CURRENCIES and `COPPER` is six letters). A pair is named only where IG's market
/// name states it (`EUR/USD`, an emerging-market `EMFX USD/INR (10 Contract)`); copper states nothing, so the shared
/// grammar refuses it a name, as zinc's (decision 0290's commodity CFD is its row's base + `.CFD`, and an IG row states no
/// base), and its Mini names no standard lot that does not exist.
#[test]
fn copper_typed_currencies_is_no_pair_and_has_no_name() {
    let user = vike_catalog::user_name_of_row;
    let copper = row_of("CS.D.COPPER.CFD.IP", "CURRENCIES", Some("-"), "Copper");
    assert_eq!(copper.naming, None, "IG states nothing: the list's writer spells it");
    let why = user(&vike_catalog::spelled(vec![copper])[0]).expect_err("no name");
    assert!(why.contains("CS.D.COPPER.CFD.IP") && why.contains("no base"), "{why}");
    let mini = row_of("CS.D.COPPER.MINI.IP", "CURRENCIES", Some("-"), "Copper ($5 Mini Contract)");
    assert_eq!(
        user(&mini),
        Err("IG's mini contract CS.D.COPPER.MINI.IP has no vike name (decision 0290)".to_string())
    );
    // the CI box's list of 2026-11-17 holds this pair with no Mini: it has only its `.CFD` name
    let emerging =
        row_of("CS.D.EURINR.CFD.IP", "CURRENCIES", Some("-"), "EMFX EUR/INR (10 Contract)");
    assert_eq!(user(&emerging).as_deref(), Ok("EURINR.CFD"));
}
