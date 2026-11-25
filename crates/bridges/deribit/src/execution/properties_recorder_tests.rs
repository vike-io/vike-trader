//! PIT properties recording (task 7): `VenueRestDeribit::with_properties_recorder` writes the constructed
//! instrument's REAL properties grid into the store when a recorder is present, keyed by venue
//! `"deribit"`. Uses the DataFusion-free `HistoryStoreMemory` test double (vike-data's `test-support`
//! dev-feature) so this venue's default build never pulls DataFusion in. `DeribitTransport`
//! is constructed but never `connect()`-ed — `with_properties_recorder` never touches the socket.
use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, PropertiesRecorder, TimestampRange};
use vike_model::SymbolProperties;

use super::VenueRestDeribit;
use crate::transport::client::DeribitTransport;

const SYMBOL: &str = "BTC-1JAN27-100000-C";

fn make_rest(properties: SymbolProperties) -> VenueRestDeribit {
    let transport = DeribitTransport::new("wss://test.invalid", "id", "secret", None);
    VenueRestDeribit::new(transport, SYMBOL, properties, "BTC")
}

#[test]
fn deribit_records_properties_when_recorder_present() {
    let store = Arc::new(HistoryStoreMemory::new());
    let rec = Arc::new(PropertiesRecorder::new(store.clone(), true));
    let f =
        SymbolProperties { tick_size: 0.0005, step_size: 0.1, min_qty: 0.1, ..Default::default() };
    let _rest = make_rest(f).with_properties_recorder(&rec);
    let rows = store.scan_symbol_properties("deribit", SYMBOL, TimestampRange::all()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, f);
}

#[test]
fn deribit_disabled_recorder_writes_nothing() {
    let store = Arc::new(HistoryStoreMemory::new());
    let rec = Arc::new(PropertiesRecorder::new(store.clone(), false));
    let _rest = make_rest(SymbolProperties::default()).with_properties_recorder(&rec);
    assert!(
        store.scan_symbol_properties("deribit", SYMBOL, TimestampRange::all()).unwrap().is_empty()
    );
}
