//! **fxcm STARTS and LISTS with no kept list** (decision 0290; fxcm is a held-session venue). The account's own
//! running session is what writes `<project>/instruments/fxcm.json` (its reconcile session's Offers table, listed by
//! `vike_fxcm::CatalogProviderFxcm` and kept by the datahub), so nothing on the way there may wait for that list: the
//! listing and a fill work without it. What reads a NAME (the owner's ruling «Б», 2026-11-16: names are stored on the
//! kept rows and only read) waits for the list and refuses in words until then: an order's wire name, a reconcile
//! pass's rows of a symbol, a mounted CFD's multiplier (`crate::instrument::names`' unit tests).
//!
//! Its own test binary, because the kept list is PROCESS-WIDE: nothing here holds fxcm's, and every test asserts so.

use std::sync::Arc;

use vike_catalog::CatalogProvider;
use vike_fxcm::account::quote_rate::{PropertiesRead, mount_contract_value, parse_offers};
use vike_fxcm::account::reads::SessionReads;
use vike_fxcm::refusals::FxcmSymbolRefusal;
use vike_model::events::Event;

/// The measured Offers tables of the EUR demo (48 forex rows and 449 of every other class), as one shim array.
fn measured_offers() -> String {
    let rows: Vec<serde_json::Value> = [
        include_str!("fixtures/eur_demo_offers.json"),
        include_str!("fixtures/offers_classes.json"),
    ]
    .iter()
    .flat_map(|table| {
        let fixture: serde_json::Value = serde_json::from_str(table).expect("the fixture is JSON");
        fixture["offers"].as_array().expect("an array").clone()
    })
    .collect();
    serde_json::Value::Array(rows).to_string()
}

/// A running account's reconcile session that answers the measured table.
struct Measured;

impl SessionReads for Measured {
    fn system_properties(&self) -> PropertiesRead {
        PropertiesRead::Json(r#"{"BASE_CRNCY":"EUR"}"#.to_string())
    }
    fn offers(&self) -> Result<String, String> {
        Ok(measured_offers())
    }
}

fn no_list_is_held() {
    assert!(vike_catalog::kept_list("fxcm").is_none(), "this binary holds no fxcm kept list");
}

/// The catalog lists the running session's Offers table (what becomes the kept list) with no kept list; a mounted
/// CFD's multiplier waits for the name STORED on its kept row (the owner's ruling «Б», 2026-11-16: nothing spells a
/// name at a read), so with no list it is refused in words, never guessed.
#[test]
fn the_listing_needs_no_kept_list_and_a_mounted_cfd_waits_for_its_stored_name() {
    no_list_is_held();
    let entry = vike_fxcm::account::reads::register("", Arc::new(Measured));
    let listed = vike_fxcm::CatalogProviderFxcm.list_instruments().expect("the session's table");
    vike_fxcm::account::reads::unregister("", &entry);
    assert_eq!(listed.len(), 497, "every measured row is listed");
    assert!(listed.iter().any(|row| row.raw_symbol == "USOil"), "the venue's own name, case kept");

    let offers = parse_offers(&measured_offers()).expect("the shim's shape");
    let why = mount_contract_value(&offers, "USOIL.CFD").expect_err("no stored name yet");
    assert_eq!(why, FxcmSymbolRefusal::NotInOffers { symbol: "USOIL.CFD".to_string() });
    no_list_is_held();
}

/// A fill needs no kept list: a venue row no kept row names is booked under the venue's own spelling, never dropped.
/// (A reconcile pass on a symbol no row names fails in words instead: `ReconcileClientFxcm`'s `parse_each`.)
#[test]
fn a_fill_needs_no_kept_list() {
    no_list_is_held();
    let fill = serde_json::json!({"kind": "fill", "trade_id": "T1", "instrument": "EUR/USD", "side": "B",
                                  "amount": 1000, "rate": 1.1, "timestamp": 1});
    let events = vike_fxcm::execution::event_mapper::map_fxcm_event(&fill, "c1");
    let Some(Event::Fill(booked)) = events.first() else { panic!("a fill: {events:?}") };
    assert_eq!(booked.symbol, "EUR/USD", "no kept row: the venue's own name");
    no_list_is_held();
}
