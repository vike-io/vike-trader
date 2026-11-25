//! The catalog parse over REAL `public/get_instruments` rows: the exact [`Contract`] each row
//! defines, the order grid it carries, the classifier, and the per-request tolerance of a full pass.

use super::*;
use crate::refusals::DeribitContractRefusalId;

/// **The delivery guard's day is this parser's expiry** (decision 0233 as amended on 2026-11-22): every dated future and
/// option of the kept list, read back from its USER name by `vike_venues::venue_delivery_guard`'s `expiry_of`, is the
/// instant this parser read off the row (`expiration_timestamp`, 08:00 UTC). A measurement pin of the venue's column,
/// not a copy weld.
#[test]
fn the_delivery_guard_reads_each_dated_rows_expiry_from_its_user_name() {
    crate::names::hold_deribit_kept_rows();
    let rows = vike_catalog::kept_list("deribit").expect("the list is held");
    let guard = vike_venues::venue_delivery_guard::delivery_guard_for("deribit");
    let (mut futures, mut options) = (0, 0);
    for row in rows.iter() {
        let (Some(expiry), Some(user)) = (
            row.contract.expiry_ms(),
            vike_catalog::kept_user_name_of_raw("deribit", &row.raw_symbol),
        ) else {
            continue;
        };
        match row.contract {
            Contract::Future(_) => futures += 1,
            Contract::Option(_) => options += 1,
            _ => continue,
        }
        assert_eq!(guard.expiry_of(&user), Some(expiry), "{} ({user})", row.raw_symbol);
    }
    assert!(futures > 0 && options > 0, "read {futures} dated futures and {options} options");
}

/// REAL `public/get_instruments` payloads, captured keyless from the live mainnet endpoint
/// 2026-10-04 and trimmed to a handful of rows each — every row is verbatim venue output, with
/// ONE exception: `BTC-26MAR27` in `future` carries `is_active: false` / `state: "closed"`
/// because the live book held no inactive row to capture. Keys: `future` is the
/// `currency=any&kind=future` envelope, `spot` is `currency=any&kind=spot`, `option_btc` and
/// `option_eth` are the two coin-settled option books, `option_usdc` is the USDC-settled book
/// (`currency=USDC&kind=option`, ONE `SOL_USDC` put, captured the same way on 2026-10-11 and
/// re-serialized by `jq -c` with its key order kept), and `unfiltered_btc` is what the OLD
/// `currency=BTC` request returned in shape — every kind mixed, combos included.
const FIXTURE: &str = include_str!("../tests/fixtures/deribit_get_instruments.json");

/// The release's public REST row (`VENUE_URL_SEED`, `live` `data`): the base the catalog reads.
fn base() -> String {
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};
    VenueUrls::seed("deribit")
        .url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")
        .expect("the seed holds deribit's live rest row")
        .to_string()
}

/// The catalog on the release's rows.
fn seeded() -> CatalogProviderDeribit {
    CatalogProviderDeribit::from_urls(&vike_venues::venue_urls::VenueUrls::seed("deribit"))
        .expect("the seed holds deribit's live rest row")
}

/// The perpetual/spot expiry SENTINEL the venue publishes (year 3000) — never an expiry.
const SENTINEL_MS: i64 = 32_503_708_800_000;

fn envelope(key: &str) -> Value {
    let all: Value = serde_json::from_str(FIXTURE).expect("the fixture is valid JSON");
    all.get(key).unwrap_or_else(|| panic!("fixture has no `{key}` envelope")).clone()
}

/// One REAL fixture row by instrument name (searched across every envelope).
fn real_row(name: &str) -> Value {
    for key in ["future", "spot", "option_btc", "option_eth", "option_usdc", "unfiltered_btc"] {
        if let Some(r) = envelope(key)["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["instrument_name"] == serde_json::json!(name))
        {
            return r.clone();
        }
    }
    panic!("{name} is in no fixture envelope")
}

/// Which fixture envelope answers a given request URL — the injectable fetch the tolerance
/// tests drive, keyed off the REAL request set so a changed request set cannot go unanswered.
/// `currency=any&kind=option` answers the three captured books' rows together, as the live request
/// does (module doc, MEASURED 2026-11-13: the union, name for name).
fn envelope_for(url: &str) -> Option<Value> {
    let key = match url.strip_prefix(format!("{}{PATH_INSTRUMENTS}", base()).as_str())? {
        "?currency=any&kind=future" => "future",
        "?currency=any&kind=spot" => "spot",
        "?currency=any&kind=option" => {
            let rows: Vec<Value> = ["option_btc", "option_eth", "option_usdc"]
                .iter()
                .flat_map(|book| envelope(book)["result"].as_array().cloned().unwrap_or_default())
                .collect();
            return Some(serde_json::json!({ "result": rows }));
        }
        _ => return None,
    };
    Some(envelope(key))
}

fn names(instruments: &[Instrument]) -> Vec<&str> {
    instruments.iter().map(|i| i.raw_symbol.as_str()).collect()
}

fn by_name<'a>(instruments: &'a [Instrument], name: &str) -> &'a Instrument {
    instruments
        .iter()
        .find(|i| i.raw_symbol == name)
        .unwrap_or_else(|| panic!("{name} is not in {:?}", names(instruments)))
}

fn perp(margining: Margining, settle: &str, value_ccy: &str) -> Contract {
    Contract::Perp(ContractPerp {
        margining,
        settle: settle.into(),
        value_currency: Some(value_ccy.into()),
        funding_interval_ms: None,
        settling_ms: None,
    })
}

fn future(
    margining: Margining,
    settle: &str,
    value_ccy: &str,
    expiry_ms: i64,
    tenor: ContractFutureTenor,
) -> Contract {
    Contract::Future(ContractFuture {
        margining,
        settle: settle.into(),
        value_currency: Some(value_ccy.into()),
        expiry_ms,
        tenor: Some(tenor),
        delivery: ContractFutureDelivery::Unknown,
        family: ContractFutureFamily::Crypto,
    })
}

fn option(right: OptionKind, strike: f64, expiry_ms: i64, coin: &str) -> Contract {
    Contract::Option(ContractOption {
        right,
        strike,
        expiry_ms,
        style: ContractOptionExerciseStyle::European,
        margining: Margining::Inverse,
        settle: coin.into(),
        value_currency: Some(coin.into()),
        underlying: Some(coin.into()),
    })
}

/// The `currency=any&kind=future` shape, row for row, as the EXACT contract each defines: a
/// coin-settled perpetual, a USDC-settled linear perpetual, a perpetual on an EQUITY, and a dated
/// future of each venue period — and the one inactive row skipped. Perpetual vs dated is the
/// venue's `settlement_period`, margining its `instrument_type`, never the name.
#[test]
fn a_real_future_payload_mints_the_exact_perp_and_future_contracts() {
    let out = parse_instruments(&envelope("future"));
    assert_eq!(
        names(&out),
        [
            "BTC-PERPETUAL",
            "BTC_USDC-PERPETUAL",
            "AAPL_USDC-PERPETUAL",
            "AVAX_USDC-5OCT26",
            "BTC-9OCT26",
            "BTC-30OCT26",
        ],
        "BTC-26MAR27 is inactive and must be skipped"
    );
    use Margining::{Inverse, Linear};
    // (name, contract, base, quote, the venue's own word)
    let expected = [
        ("BTC-PERPETUAL", perp(Inverse, "BTC", "USD"), "BTC", "USD", "reversed"),
        ("BTC_USDC-PERPETUAL", perp(Linear, "USDC", "BTC"), "BTC", "USDC", "linear"),
        ("AAPL_USDC-PERPETUAL", perp(Linear, "USDC", "AAPL"), "AAPL", "USDC", "linear"),
        (
            "AVAX_USDC-5OCT26",
            future(Linear, "USDC", "AVAX", 1_791_187_200_000, ContractFutureTenor::Day),
            "AVAX",
            "USDC",
            "linear",
        ),
        (
            "BTC-9OCT26",
            future(Inverse, "BTC", "USD", 1_791_532_800_000, ContractFutureTenor::Week),
            "BTC",
            "USD",
            "reversed",
        ),
        (
            "BTC-30OCT26",
            future(Inverse, "BTC", "USD", 1_793_347_200_000, ContractFutureTenor::Month),
            "BTC",
            "USD",
            "reversed",
        ),
    ];
    for (name, contract, base, quote, word) in expected {
        let i = by_name(&out, name);
        assert_eq!(i.contract, contract, "{name}");
        assert_eq!((i.base.as_str(), i.quote.as_str()), (base, quote), "{name}");
        assert_eq!(i.contract_type.as_deref(), Some(word), "{name}");
        assert_eq!(i.venue, "deribit");
    }
    assert_eq!(by_name(&out, "BTC-PERPETUAL").asset_class(), AssetClass::CryptoPerp);
    assert_eq!(by_name(&out, "BTC-9OCT26").asset_class(), AssetClass::CryptoFuture);
    assert_eq!(by_name(&out, "BTC_USDC-PERPETUAL").settle_asset(), Some("USDC"));
}

/// ⚠ A perpetual carries NO expiry, and the venue's `expiration_timestamp: 32503708800000` on
/// every perpetual row is never read: changing it — or removing it — leaves the contract
/// byte-identical, and nowhere in the minted contract does the sentinel appear.
#[test]
fn a_perpetual_never_reads_the_expiration_sentinel() {
    let row = real_row("BTC-PERPETUAL");
    assert_eq!(row["expiration_timestamp"], serde_json::json!(SENTINEL_MS), "fixture precondition");
    let as_published = contract_of(&row).expect("a real perpetual row");
    for replacement in [serde_json::json!(1), serde_json::json!("junk"), Value::Null] {
        let mut r = row.clone();
        r["expiration_timestamp"] = replacement.clone();
        assert_eq!(contract_of(&r).as_ref(), Some(&as_published), "{replacement}");
    }
    let mut absent = row.clone();
    absent.as_object_mut().unwrap().remove("expiration_timestamp");
    assert_eq!(contract_of(&absent).as_ref(), Some(&as_published));
    let debug = format!("{as_published:?}");
    assert!(!debug.contains(&SENTINEL_MS.to_string()), "the sentinel leaked: {debug}");
}

/// The order grid rides in the catalog row: `tick_size`, `min_trade_amount` as step and minimum,
/// `contract_size`, the class — the same [`properties_of`] the singular `get_instrument` read runs.
#[test]
fn a_real_future_row_carries_its_order_grid() {
    let out = parse_instruments(&envelope("future"));
    let p = by_name(&out, "BTC-PERPETUAL").properties;
    assert_eq!((p.tick_size, p.step_size, p.min_qty, p.contract_size), (0.5, 10.0, 10.0, 10.0));
    assert_eq!(p.asset_class, Some(AssetClass::CryptoPerp));
    assert!(p.tick_scheme.is_none(), "an empty `tick_size_steps` is a flat grid");
    let p = by_name(&out, "BTC_USDC-PERPETUAL").properties;
    assert_eq!((p.tick_size, p.min_qty, p.contract_size), (0.1, 0.0001, 0.0001));
    for i in &out {
        assert_eq!(Some(i.properties), properties_of(&real_row(&i.raw_symbol)), "{}", i.raw_symbol);
    }
}

/// The `currency=any&kind=spot` shape: a spot pair is [`Contract::CryptoSpot`] from the venue's
/// `kind` alone, it carries its own base and quote, no settlement asset (the venue publishes none
/// on a spot row), its grid, and the venue's `instrument_type: "linear"` verbatim.
#[test]
fn a_real_spot_payload_is_crypto_spot_with_no_settlement_asset() {
    let out = parse_instruments(&envelope("spot"));
    assert_eq!(names(&out), ["BTC_USDC", "ETH_USDC"]);
    for (name, base) in [("BTC_USDC", "BTC"), ("ETH_USDC", "ETH")] {
        let i = by_name(&out, name);
        assert_eq!(i.contract, Contract::CryptoSpot, "{name}");
        assert_eq!(i.asset_class(), AssetClass::CryptoSpot, "{name}");
        assert_eq!((i.base.as_str(), i.quote.as_str()), (base, "USDC"), "{name}");
        assert_eq!(i.settle_asset(), None, "{name}: a spot row publishes no settlement currency");
        assert_eq!(i.contract_type.as_deref(), Some("linear"), "{name}: the venue's own word");
        assert_eq!((i.properties.tick_size, i.properties.min_qty), (0.01, 1.0e-8), "{name}");
        assert_eq!(i.properties.asset_class, Some(AssetClass::CryptoSpot), "{name}");
    }
}

/// The two option books: the exact option contract (right, strike, expiry to the millisecond —
/// 08:00 UTC — European, coin-margined and coin-settled, underlying from `base_currency`) and the
/// TIERED tick grid the row publishes.
#[test]
fn a_real_option_payload_mints_the_exact_option_contract() {
    for (key, name, coin, strike, min_qty) in [
        ("option_btc", "BTC-5OCT26-74000-C", "BTC", 74_000.0, 0.1),
        ("option_eth", "ETH-5OCT26-2300-C", "ETH", 2_300.0, 1.0),
    ] {
        let out = parse_instruments(&envelope(key));
        assert_eq!(names(&out), [name]);
        let i = &out[0];
        assert_eq!(i.contract, option(OptionKind::Call, strike, 1_791_187_200_000, coin), "{name}");
        assert_eq!(i.asset_class(), AssetClass::Option);
        assert_eq!((i.base.as_str(), i.quote.as_str()), (coin, coin));
        assert_eq!(i.contract_type.as_deref(), Some("reversed"));
        assert_eq!(i.settle_asset(), Some(coin));
        let p = i.properties;
        assert_eq!((p.tick_size, p.min_qty, p.contract_size), (0.0001, min_qty, 1.0), "{name}");
        let want =
            GridTickScheme::new(0.0001, &[GridTickTier { above_price: 0.005, tick_size: 0.0005 }])
                .expect("the real BTC/ETH option grid");
        assert_eq!(p.tick_scheme, Some(want), "{name}: tick_size_steps reaches the catalog row");
    }
    // The expiry is the venue's exact millisecond, 08:00 UTC on the expiry day.
    assert_eq!(1_791_187_200_000_i64.rem_euclid(86_400_000), 8 * 3_600_000);
}

/// The USDC-settled book: a LINEAR, USDC-settled option on an altcoin with no coin-settled book
/// (SOL), its underlying and contract unit the row's `base_currency`, its name kept verbatim — the
/// row the Greeks tool's option index needs to price a SOL position.
#[test]
fn a_real_usdc_book_payload_mints_a_linear_usdc_settled_option() {
    let name = "SOL_USDC-12OCT26-96-P";
    let out = parse_instruments(&envelope("option_usdc"));
    assert_eq!(names(&out), [name]);
    let i = &out[0];
    assert_eq!(
        i.contract,
        Contract::Option(ContractOption {
            right: OptionKind::Put,
            strike: 96.0,
            expiry_ms: 1_791_792_000_000,
            style: ContractOptionExerciseStyle::European,
            margining: Margining::Linear,
            settle: "USDC".into(),
            value_currency: Some("SOL".into()),
            underlying: Some("SOL".into()),
        })
    );
    assert_eq!(i.asset_class(), AssetClass::Option);
    assert_eq!((i.base.as_str(), i.quote.as_str()), ("SOL", "USDC"));
    assert_eq!(i.contract_type.as_deref(), Some("linear"));
    assert_eq!(i.settle_asset(), Some("USDC"));
    let p = i.properties;
    assert_eq!((p.tick_size, p.min_qty, p.contract_size), (0.1, 10.0, 10.0), "{name}");
    // 12 Oct 2026, 08:00 UTC — the date in the name, at the venue's settlement hour.
    assert_eq!(1_791_792_000_000_i64.rem_euclid(86_400_000), 8 * 3_600_000);
}

/// A put is a put by the venue's `option_type`, never by the trailing `-C`/`-P` of its name.
#[test]
fn the_option_right_is_the_payloads_option_type() {
    let mut row = real_row("BTC-5OCT26-74000-C");
    row["option_type"] = serde_json::json!("put");
    let Some(Contract::Option(spec)) = contract_of(&row) else { panic!("an option") };
    assert_eq!(spec.right, OptionKind::Put, "the name still reads -C");
    row["option_type"] = serde_json::json!("straddle");
    assert_eq!(contract_of(&row), None, "an unknown right mints nothing");
}

/// What the OLD unfiltered `currency=BTC` request returned in shape — every kind mixed. The
/// option, the perpetual and the spot pair are kept; the `future_combo` and the `option_combo`
/// the venue also lists are skipped: their rows carry no legs, so a combo contract minted from
/// them would hold an empty leg list.
#[test]
fn a_mixed_payload_skips_both_combo_kinds_and_keeps_the_rest() {
    let payload = envelope("unfiltered_btc");
    let kinds: Vec<&str> =
        payload["result"].as_array().unwrap().iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        ["option", "future", "future_combo", "option_combo", "spot"],
        "the fixture must carry both combo kinds or this test proves nothing"
    );
    let out = parse_instruments(&payload);
    assert_eq!(names(&out), ["BTC-5OCT26-74000-C", "BTC-PERPETUAL", "BTC_USDC"]);
    assert_eq!(
        out.iter().map(Instrument::asset_class).collect::<Vec<_>>(),
        [AssetClass::Option, AssetClass::CryptoPerp, AssetClass::CryptoSpot]
    );
    for combo in ["BTC-FS-5OCT26_PERP", "BTC-CSR12-6OCT26-86000_86500"] {
        assert_eq!(contract_of(&real_row(combo)), None, "{combo}");
        assert!(properties_of(&real_row(combo)).is_some(), "{combo}: its grid still parses");
    }
}

/// The ONE kind decision, held both ways over every real fixture row: the minted contract's class
/// is exactly [`asset_class_of`]'s word, so the catalog and the grid parser cannot disagree.
#[test]
fn every_real_rows_contract_projects_to_the_classifiers_word() {
    let mut seen = 0;
    for key in ["future", "spot", "option_btc", "option_eth", "option_usdc"] {
        for row in envelope(key)["result"].as_array().unwrap() {
            let name = row["instrument_name"].as_str().unwrap();
            let contract = contract_of(row).unwrap_or_else(|| panic!("{name}: a real row mints"));
            assert_eq!(Some(contract.class()), asset_class_of(row), "{name}");
            seen += 1;
        }
    }
    assert_eq!(seen, 7 + 2 + 1 + 1 + 1, "every fixture row, the inactive one included");
}

/// The classifier on ITS OWN terms — `crates/bridges/deribit/src/execution/mod.rs`'s `parse_get_instrument`
/// reaches it through [`properties_of`], so its `None` arms are a contract two callers depend on.
///
/// Every product the venue sells is told apart here by `kind` + `settlement_period` alone: an
/// option, a perpetual, a dated future of each period the venue uses, and a spot pair.
#[test]
fn the_classifier_distinguishes_every_product_by_kind_and_settlement_period() {
    let class = |kind: &str, period: &str| {
        asset_class_of(&serde_json::json!({
            "instrument_name": "BTC-PERPETUAL", "kind": kind, "settlement_period": period
        }))
    };
    assert_eq!(class("option", "month"), Some(AssetClass::Option));
    assert_eq!(class("option", "perpetual"), Some(AssetClass::Option), "kind wins for options");
    assert_eq!(class("spot", "perpetual"), Some(AssetClass::CryptoSpot), "kind wins for spot");
    assert_eq!(class("future", "perpetual"), Some(AssetClass::CryptoPerp));
    for dated in ["day", "week", "month"] {
        assert_eq!(class("future", dated), Some(AssetClass::CryptoFuture), "{dated}");
    }
    // A future whose `settlement_period` the venue omitted is a DATED future, not a perp: only
    // the literal word `perpetual` buys `CryptoPerp`.
    assert_eq!(
        asset_class_of(&serde_json::json!({"kind": "future"})),
        Some(AssetClass::CryptoFuture)
    );
    // A real spot row carries no `settlement_period` key at all and is a spot pair regardless.
    assert_eq!(
        asset_class_of(&serde_json::json!({"kind": "spot", "instrument_name": "BTC_USDC"})),
        Some(AssetClass::CryptoSpot)
    );
    // The two combo kinds, and a row with no `kind`, are UNCLASSIFIED — never guessed.
    for kind in ["future_combo", "option_combo", ""] {
        assert_eq!(class(kind, "perpetual"), None, "{kind}");
    }
    assert_eq!(asset_class_of(&serde_json::json!({})), None);
    assert_eq!(asset_class_of(&serde_json::json!({"kind": 7})), None, "non-string kind");
}

/// ⚠ The name is NOT evidence (0126). A row NAMED like a perpetual whose venue `kind` is `option`
/// mints an option, one named like an option whose `kind` is `future` mints a future, and the
/// whole catalog parse follows the payload, not the name.
#[test]
fn the_instrument_name_is_never_consulted() {
    let class = |name: &str, kind: &str, period: Option<&str>| {
        let mut r = serde_json::json!({"instrument_name": name, "kind": kind});
        if let Some(p) = period {
            r["settlement_period"] = serde_json::json!(p);
        }
        asset_class_of(&r)
    };
    assert_eq!(class("BTC-PERPETUAL", "option", Some("perpetual")), Some(AssetClass::Option));
    assert_eq!(class("BTC-PERPETUAL", "option", None), Some(AssetClass::Option));
    assert_eq!(class("BTC-PERPETUAL", "spot", Some("perpetual")), Some(AssetClass::CryptoSpot));
    assert_eq!(
        class("BTC-5OCT26-74000-C", "future", Some("month")),
        Some(AssetClass::CryptoFuture)
    );
    assert_eq!(class("BTC_USDC", "future", Some("perpetual")), Some(AssetClass::CryptoPerp));
    assert_eq!(class("", "future", Some("perpetual")), Some(AssetClass::CryptoPerp));

    // Through the catalog parse: a REAL option row renamed `BTC-PERPETUAL` is still that option,
    // strike and expiry included — none of it was ever read from the name.
    let mut renamed = real_row("BTC-5OCT26-74000-C");
    renamed["instrument_name"] = serde_json::json!("BTC-PERPETUAL");
    let out = parse_instruments(&serde_json::json!({ "result": [renamed] }));
    assert_eq!(out[0].contract, option(OptionKind::Call, 74_000.0, 1_791_187_200_000, "BTC"));
}

/// Margining is the venue's `instrument_type`, the settlement asset its `settlement_currency`
/// (uppercased — the venue sends `"usdc"` on some rows), and the verbatim word rides along.
#[test]
fn margining_and_settlement_are_read_from_the_payload() {
    let mut linear = real_row("BTC_USDC-PERPETUAL");
    linear["settlement_currency"] = serde_json::json!("usdc");
    let out =
        parse_instruments(&serde_json::json!({ "result": [real_row("BTC-PERPETUAL"), linear] }));
    assert_eq!(out[0].contract, perp(Margining::Inverse, "BTC", "USD"));
    assert_eq!(out[0].contract_type.as_deref(), Some("reversed"));
    assert_eq!(out[1].contract, perp(Margining::Linear, "USDC", "BTC"));
    assert_eq!(out[1].settle_asset(), Some("USDC"));
    assert_eq!(out[1].contract_type.as_deref(), Some("linear"));
}

/// A derivative row WITHOUT the fields its contract requires mints NOTHING — there is no honest
/// margining, settlement asset, expiry, strike or right to put in its place — while a spot row
/// needs none of them.
#[test]
fn a_derivative_lacking_a_required_field_is_skipped_not_guessed() {
    let without = |name: &str, key: &str| {
        let mut r = real_row(name);
        r.as_object_mut().unwrap().remove(key);
        r
    };
    for (name, key) in [
        ("BTC-PERPETUAL", "instrument_type"),
        ("BTC-PERPETUAL", "settlement_currency"),
        ("BTC-9OCT26", "expiration_timestamp"),
        ("BTC-5OCT26-74000-C", "strike"),
        ("BTC-5OCT26-74000-C", "option_type"),
        ("BTC-5OCT26-74000-C", "expiration_timestamp"),
    ] {
        let row = without(name, key);
        assert_eq!(contract_of(&row), None, "{name} without {key}");
        assert!(parse_instruments(&serde_json::json!({ "result": [row] })).is_empty());
    }
    let mut odd = real_row("BTC-PERPETUAL");
    odd["instrument_type"] = serde_json::json!("quanto?");
    assert_eq!(contract_of(&odd), None, "an unknown margining word is not guessed");
    let spot = without("BTC_USDC", "instrument_type");
    assert_eq!(contract_of(&spot), Some(Contract::CryptoSpot));
}

/// **DER-3**: the row keeps the venue's own name, which is the door's spelling too: a decimal strike's
/// lower-case `d` stays, so a name picked from the list is one the door admits and the facts find.
#[test]
fn venue_and_verbatim_raw_symbol() {
    let mut row = real_row("BTC-PERPETUAL");
    row["instrument_name"] = serde_json::json!("XRP_USDC-27MAR26-2d5-C");
    let out = parse_instruments(&serde_json::json!({ "result": [row] }));
    assert_eq!(out[0].venue, "deribit");
    assert_eq!(out[0].raw_symbol, "XRP_USDC-27MAR26-2d5-C");
    assert_eq!(
        vike_catalog::door_symbol("deribit", "xrp_usdc-27mar26-2D5-c"),
        Some(Ok(out[0].raw_symbol.clone())),
        "one case rule: the door spells the typed name as the list keeps it"
    );
}

#[test]
fn empty_or_missing_result_yields_empty() {
    assert!(parse_instruments(&serde_json::json!({})).is_empty());
    assert!(parse_instruments(&serde_json::json!({"result": []})).is_empty());
}

/// The request set, spelled out: perpetuals, dated futures, spot and options, each across EVERY
/// currency, so no currency is named (owner, 2026-11-13). Every URL names its `kind` — an unfiltered
/// request would return every kind and overlap the others — and no two URLs are the same query.
#[test]
fn the_request_set_asks_every_currency_once_per_kind_and_names_no_currency() {
    let urls = request_urls(&base());
    let b = "https://www.deribit.com/api/v2/public/get_instruments";
    assert_eq!(
        urls,
        [
            format!("{b}?currency=any&kind=future"),
            format!("{b}?currency=any&kind=spot"),
            format!("{b}?currency=any&kind=option"),
        ]
    );
    for url in &urls {
        assert!(url.contains("&kind="), "{url} must name its kind");
    }
    let unique: std::collections::HashSet<_> = urls.iter().collect();
    assert_eq!(unique.len(), urls.len(), "a repeated request would duplicate its rows");
}

/// A full pass over the real fixture envelopes — one per request — holds every product kind
/// the venue sells and not one instrument twice (the three requests are disjoint).
#[test]
fn a_full_pass_yields_every_product_once() {
    let out =
        list_tolerant(&base(), |url| envelope_for(url).ok_or_else(|| CatalogError(url.into())));
    let mut sorted = names(&out);
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(sorted.len(), before, "an instrument arrived on two requests: {:?}", names(&out));
    assert_eq!(out.len(), 6 + 2 + 1 + 1 + 1, "futures + spot + one option per book");
    let count = |class| out.iter().filter(|i| i.asset_class() == class).count();
    assert_eq!(count(AssetClass::CryptoPerp), 3);
    assert_eq!(count(AssetClass::CryptoFuture), 3);
    assert_eq!(count(AssetClass::CryptoSpot), 2);
    assert_eq!(count(AssetClass::Option), 3);
}

/// `asset_classes()` is what the picker is told this venue sells, so it must be EXACTLY what a
/// pass can mint — in both directions. A class advertised and never minted is the okx bug (it
/// advertised `Option` while the picker received none); a class minted and never advertised is
/// invisible to anything that filters on the advertisement.
#[test]
fn a_full_pass_mints_exactly_the_advertised_classes() {
    let out =
        list_tolerant(&base(), |url| envelope_for(url).ok_or_else(|| CatalogError(url.into())));
    let minted: std::collections::HashSet<AssetClass> =
        out.iter().map(Instrument::asset_class).collect();
    let advertised: std::collections::HashSet<AssetClass> =
        seeded().asset_classes().iter().copied().collect();
    assert_eq!(minted, advertised);
    assert_eq!(seeded().asset_classes().len(), advertised.len(), "a class is advertised twice");
    assert!(advertised.contains(&AssetClass::CryptoSpot), "spot is advertised since 2026-10-04");
}

/// The (g) robustness contract, mirroring okx's per-instType accumulate: ONE request failing
/// must not wipe the venue. Each of the three requests is failed in turn and the OTHER two
/// still come back, row for row; every request failing yields an EMPTY catalog (warned), never
/// an error.
#[test]
fn one_failed_request_still_yields_the_other_requests_instruments() {
    let all =
        list_tolerant(&base(), |url| envelope_for(url).ok_or_else(|| CatalogError(url.into())));
    for failing in request_urls(&base()) {
        let out = list_tolerant(&base(), |url| {
            if url == failing {
                Err(CatalogError("HTTP 400: simulated".into()))
            } else {
                envelope_for(url).ok_or_else(|| CatalogError(url.into()))
            }
        });
        let lost = envelope_for(&failing).map(|e| parse_instruments(&e)).unwrap();
        assert!(!lost.is_empty(), "{failing}: the fixture envelope must hold rows");
        assert_eq!(
            out.len(),
            all.len() - lost.len(),
            "{failing} failing must cost exactly its own rows, no more"
        );
        for i in &lost {
            assert!(!names(&out).contains(&i.raw_symbol.as_str()), "{failing}");
        }
    }
    // all requests failing yields an EMPTY catalog (warned), never an error
    assert!(list_tolerant(&base(), |_| Err(CatalogError("down".into()))).is_empty());
}

/// The options request failing costs every option book (they are one request now) and nothing else.
#[test]
fn a_failed_option_request_still_yields_futures_and_spot() {
    let out = list_tolerant(&base(), |url| {
        if url.contains("kind=option") {
            Err(CatalogError("HTTP 400: simulated".into()))
        } else {
            envelope_for(url).ok_or_else(|| CatalogError(url.into()))
        }
    });
    assert!(out.iter().all(|i| i.asset_class() != AssetClass::Option), "{:?}", names(&out));
    assert!(names(&out).contains(&"BTC_USDC"), "spot must survive an option failure");
    assert!(names(&out).contains(&"BTC-PERPETUAL"), "futures must survive an option failure");
}

// ---- the contract value (decision 0170) --------------------------------------------------------

/// THE mapping table over REAL rows: the KIND is the row's own `instrument_type` on a perpetual or
/// dated future, `per_unit` is 1.0 everywhere (the `amount` is dollars on an inverse contract and the
/// base coin on a linear one and on an option), and a USDC OPTION is `Linear` (it is priced
/// `qty · premium` in the ledger's own money). A coin-settled option has no value (the next test).
#[test]
fn the_contract_value_is_the_kind_of_the_row_and_the_unit_is_one() {
    use vike_model::ContractValue;
    let inverse = ContractValue::inverse(1.0);
    let linear = ContractValue::linear(1.0);
    for (name, want) in [
        ("BTC-PERPETUAL", inverse),
        ("BTC-9OCT26", inverse),
        ("BTC_USDC-PERPETUAL", linear),
        ("AVAX_USDC-5OCT26", linear),
        ("SOL_USDC-12OCT26-96-P", linear),
        ("BTC_USDC", linear),
    ] {
        assert_eq!(contract_value_of(&real_row(name)), Ok(want), "{name}");
    }
}

/// **A coin-settled option has no contract value, so no account is MOUNTED on one** (decision 0233,
/// item 4, now at the mount as it already was at every admission): its premium is quoted in its coin
/// (0.0225 BTC), a `ContractValue` carries no currency and Deribit has no rate source, so the engine
/// would book the premium as dollars. Over the REAL rows of both coin books, through the mount's own
/// read (`crate::execution::parse_instrument_facts`), the value is the refusal in words, the same words the
/// added-instrument path says; the USDC option of the same table keeps its value.
#[test]
fn a_coin_settled_option_has_no_contract_value_so_it_is_never_mounted() {
    for (name, coin) in [("BTC-5OCT26-74000-C", "BTC"), ("ETH-5OCT26-2300-C", "ETH")] {
        let row = real_row(name);
        let why = contract_value_of(&row).expect_err("a coin-settled option is not valued");
        assert_eq!(
            why,
            DeribitContractRefusal::CoinSettledOption {
                name: name.to_string(),
                settle: coin.to_string()
            },
            "{name}"
        );
        let facts = crate::execution::parse_instrument_facts(&serde_json::json!({ "result": row }))
            .expect("the row still has a grid");
        assert_eq!(facts.value, Err(why.clone()), "{name}: the mount reads the same refusal");
        let added =
            crate::execution::rest::money_model_of_row(&real_row(name)).expect_err("never added");
        assert_eq!(added, why.to_string(), "{name}: one sentence for the mount and the addition");
    }
    assert_eq!(
        contract_value_of(&real_row("SOL_USDC-12OCT26-96-P")),
        Ok(vike_model::ContractValue::linear(1.0))
    );
}

/// The measured pattern the mapping rests on (plan S0.3): every `reversed` perpetual and future is
/// counted in USD, every `linear` one has a lot equal to its minimum trade amount. A row that
/// breaks it stops the carry: the `amount` convention would not be the one decision 0170 assumes.
#[test]
fn every_real_future_row_follows_the_amount_convention_the_value_rests_on() {
    let rows = envelope("future")["result"].as_array().unwrap().clone();
    assert!(!rows.is_empty());
    let mut reversed = 0;
    let mut linear = 0;
    for r in &rows {
        let name = r["instrument_name"].as_str().unwrap();
        match r["instrument_type"].as_str() {
            Some("reversed") => {
                reversed += 1;
                assert_eq!(r["counter_currency"].as_str(), Some("USD"), "{name}");
            }
            Some("linear") => {
                linear += 1;
                assert_eq!(r["contract_size"], r["min_trade_amount"], "{name}");
            }
            other => panic!("{name}: unexpected instrument_type {other:?}"),
        }
    }
    assert!(reversed > 0 && linear > 0, "the fixture must hold both kinds");
}

/// A future or perpetual that states no margining is an ERROR, never `Linear`; a spot row needs none.
#[test]
fn a_future_that_states_no_margining_has_no_contract_value() {
    let mut odd = real_row("BTC-PERPETUAL");
    odd["instrument_type"] = serde_json::json!("quanto?");
    assert_eq!(
        contract_value_of(&odd).map_err(|why| why.id()),
        Err(DeribitContractRefusalId::MarginingUnstated),
        "an unknown margining word is not guessed"
    );
    let mut bare = real_row("BTC-9OCT26");
    bare.as_object_mut().unwrap().remove("instrument_type");
    assert_eq!(
        contract_value_of(&bare),
        Err(DeribitContractRefusal::MarginingUnstated { name: "BTC-9OCT26".to_string() })
    );
    let mut spot = real_row("BTC_USDC");
    spot.as_object_mut().unwrap().remove("instrument_type");
    assert_eq!(contract_value_of(&spot), Ok(vike_model::ContractValue::default()));
}

/// The name a reader of `row` is handed: its USER name (decision 0290: `BTCUSD.P` for `BTC-PERPETUAL`), or the venue's
/// own name for a row that has none (a coin-settled option, a combo: nothing trades them by a user name).
fn handed_name(row: &vike_model::Instrument) -> String {
    // the user name the datahub's write stores for the parsed row (`vike_catalog::spelled`)
    let named = vike_catalog::spelled(vec![row.clone()]);
    vike_catalog::user_name_of_row(&named[0]).unwrap_or_else(|_| row.raw_symbol.clone())
}

/// `vike_catalog::is_inverse_contract` reads the USER name, for a process that holds no row; this holds it
/// to every real row. A name that parted from its row would be a backtest refused or booked wrong.
#[test]
fn the_name_rule_agrees_with_every_row() {
    let mut inverse = 0;
    for key in ["future", "spot", "option_btc", "option_eth", "option_usdc", "unfiltered_btc"] {
        for row in parse_instruments(&envelope(key)) {
            let name = handed_name(&row);
            let by_row = vike_model::ContractValue::of(&row.contract, 1.0).map(|v| v.kind)
                == Some(vike_model::NotionalKind::Inverse);
            let by_name = vike_catalog::is_inverse_contract("deribit", &name);
            assert_eq!(by_name, by_row, "{name} ({})", row.raw_symbol);
            // The settle coin the door's one-coin fence reads off the name is the row's settlement currency.
            let coin_by_row =
                by_row.then(|| row.contract.settle_asset().map(str::to_string)).flatten();
            let coin_by_name = vike_catalog::inverse_settle_coin("deribit", &name);
            assert_eq!(coin_by_name, coin_by_row, "{name}");
            inverse += usize::from(by_row);
        }
    }
    assert!(inverse > 0, "the fixture holds an inverse row, or this proves nothing");
}

/// The catalog's whole value of a name (`vike_catalog::contract_value`, what a paper book and a backtest
/// read) is the value the live mount books from the row (`contract_value_of`), for every committed row, by the
/// name a reader is handed: one symbol, one value, kind and unit, and NONE for a coin-settled option, which the
/// mount refuses. A row the catalog does not keep (inactive, or lacking a field) has no user name to be handed by.
#[test]
fn the_catalogs_value_is_the_live_value_of_every_row() {
    let mut rows = 0;
    for key in ["future", "spot", "option_btc", "option_eth", "option_usdc", "unfiltered_btc"] {
        for raw in envelope(key)["result"].as_array().unwrap() {
            let parsed = parse_instruments(&serde_json::json!({ "result": [raw] }));
            let Some(name) = parsed.first().map(handed_name) else { continue };
            assert_eq!(
                vike_catalog::contract_value("deribit", &name),
                contract_value_of(raw).ok(),
                "{name}"
            );
            rows += 1;
        }
    }
    assert!(rows > 0, "the fixture holds rows, or this proves nothing");
}
