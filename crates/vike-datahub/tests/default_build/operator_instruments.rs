//! **`OperatorInstruments` on a real server over a real settings store**: an instrument added by hand is a row the
//! next client reads AND a row of the venue's kept list, which is what every reader of the kept lists reads (the
//! `InstrumentLists` reply here); a looked-up row is the venue's answer, a stated one needs every fact, a symbol the
//! venue's own list carries is refused, and a remove takes the row out of both. The lookup is a fake (no gatewayhub,
//! no venue).

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::thread;

use vike_catalog::VenueInstrumentList;
use vike_catalog::read_kept_list;
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::instrument_lists::write_list;
use vike_datahub::server::{InstrumentLookup, ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::catalog::StatedInstrumentFacts;
use vike_model::{Contract, ContractFx, Instrument};
use vike_secrets::{CredentialScope, CredentialStore, StoreLocation};

fn fx() -> Contract {
    Contract::Fx(ContractFx { pip_size: None, lot_size: None, swap_long: None, swap_short: None })
}

/// The fake venue lookup: `EURUSD` on ig is its Mini (the CFD form passed over); anything else is refused in words.
fn lookup() -> InstrumentLookup {
    Arc::new(|venue: &str, symbol: &str| match (venue, symbol) {
        ("ig", "EURUSD") => Ok(vec![
            Instrument::new("ig", "CS.D.EURUSD.MINI.IP", "EUR", "USD", fx())
                .with_description("EUR/USD Mini"),
            Instrument::new("ig", "CS.D.EURUSD.CFD.IP", "", "", fx()),
        ]),
        _ => Err(format!("{venue}: no lookup for {symbol}")),
    })
}

struct Hub {
    _settings: tempfile::TempDir,
    instruments: tempfile::TempDir,
    addr: SocketAddr,
}

fn serve(with_store: bool) -> Hub {
    let settings = tempfile::tempdir().expect("temp");
    let instruments = tempfile::tempdir().expect("temp");
    let settings_store = with_store.then(|| {
        vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8")))
            .expect("an empty store");
        CredentialStore::open(Some(StoreLocation::new(settings.path())), CredentialScope::Full)
    });
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let lanes = ServerLanes {
        settings_store,
        instruments: Some(instruments.path().to_path_buf()),
        instrument_lookup: Some(lookup()),
        ..ServerLanes::default()
    };
    thread::spawn(move || {
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            None,
            None,
            None,
            lanes,
            None,
            false,
            ServerTimings::PRODUCTION,
        );
    });
    Hub { _settings: settings, instruments, addr }
}

fn dukascopy_listing(dir: &Path) {
    let list = VenueInstrumentList {
        venue: "dukascopy".into(),
        refreshed_ms: Some(1_794_124_800_000),
        truncated: false,
        last_error: None,
        last_try_ms: Some(1_794_124_800_000),
        instruments: vec![Instrument::new("dukascopy", "EURUSD", "EUR", "USD", fx())],
        added_by_operator: Vec::new(),
    };
    write_list(dir, &list).expect("the venue's own listing");
}

fn aapl() -> StatedInstrumentFacts {
    StatedInstrumentFacts {
        tick: 0.001,
        quote: "usd".into(),
        class: "share".into(),
        point_scale: 1000.0,
    }
}

#[test]
fn a_looked_up_pair_is_stored_kept_and_read_by_the_kept_lists_reader() {
    let hub = serve(true);
    let mut client = DatahubClient::connect(hub.addr).expect("handshake");
    let reply =
        client.operator_instruments("ig", vec!["eurusd".into()], Vec::new(), None).expect("added");
    let said = reply.outcomes[0].done.as_ref().expect("ok");
    assert!(
        said.contains("CS.D.EURUSD.MINI.IP") && said.contains("passed over: CS.D.EURUSD.CFD.IP"),
        "{said}"
    );
    assert_eq!(reply.list_error, None);
    assert_eq!(reply.rows.len(), 1);
    assert_eq!(
        (reply.rows[0].asked.as_str(), reply.rows[0].instrument.raw_symbol.as_str()),
        ("EURUSD", "CS.D.EURUSD.MINI.IP")
    );

    // ig keeps no list of its own: the kept list is the operator's row alone, named as theirs.
    let kept = read_kept_list(hub.instruments.path(), "ig").expect("an ig list now");
    assert_eq!(kept.added_by_operator, ["CS.D.EURUSD.MINI.IP"]);
    assert_eq!(kept.instruments[0].asset_class(), vike_model::AssetClass::Fx);
    let lists = client.instrument_lists().expect("the kept lists' reader");
    let ig = lists.venues.iter().find(|l| l.venue == "ig").expect("served");
    assert_eq!(ig.instruments[0].raw_symbol, "CS.D.EURUSD.MINI.IP");

    // A symbol the venue cannot answer for is refused in its own words; nothing is stored.
    let reply = client
        .operator_instruments("ig", vec!["XAUXAG".into()], Vec::new(), None)
        .expect("answered");
    assert!(reply.outcomes[0].done.as_ref().is_err_and(|e| e.contains("no lookup")), "{reply:?}");
    assert_eq!(reply.rows.len(), 1);

    // An empty change reads the rows; a remove by the typed word takes the row out of the store and the list.
    let read = client.operator_instruments("", Vec::new(), Vec::new(), None).expect("read");
    assert_eq!(read.rows.len(), 1);
    let gone = client
        .operator_instruments("ig", Vec::new(), vec!["EURUSD".into()], None)
        .expect("removed");
    assert!(gone.outcomes[0].done.is_ok() && gone.rows.is_empty(), "{gone:?}");
    let kept = read_kept_list(hub.instruments.path(), "ig").expect("the list stays, emptied");
    assert!(kept.instruments.is_empty() && kept.added_by_operator.is_empty(), "{kept:?}");
}

#[test]
fn a_stated_row_needs_every_fact_and_joins_the_venues_own_listing() {
    let hub = serve(true);
    dukascopy_listing(hub.instruments.path());
    let mut client = DatahubClient::connect(hub.addr).expect("handshake");

    let wrong = StatedInstrumentFacts { point_scale: 100.0, ..aapl() };
    let reply = client
        .operator_instruments("dukascopy", vec!["AAPL.US/USD".into()], Vec::new(), Some(wrong))
        .expect("answered");
    assert!(
        reply.outcomes[0].done.as_ref().is_err_and(|e| e.contains("--point-scale")),
        "{reply:?}"
    );
    let wrong = StatedInstrumentFacts { class: "spaceship".into(), ..aapl() };
    let reply = client
        .operator_instruments("dukascopy", vec!["AAPL.US/USD".into()], Vec::new(), Some(wrong))
        .expect("answered");
    assert!(reply.outcomes[0].done.as_ref().is_err_and(|e| e.contains("--class")), "{reply:?}");
    assert!(reply.rows.is_empty(), "a row with a wrong fact is never stored");

    let reply = client
        .operator_instruments(
            "dukascopy",
            vec!["AAPL.US/USD".into(), "EURUSD".into()],
            Vec::new(),
            Some(aapl()),
        )
        .expect("answered");
    assert!(reply.outcomes[0].done.is_ok(), "{reply:?}");
    assert!(
        reply.outcomes[1].done.as_ref().is_err_and(|e| e.contains("own kept list already")),
        "a listed symbol is the venue's: {reply:?}"
    );
    let kept = read_kept_list(hub.instruments.path(), "dukascopy").expect("kept");
    let names: Vec<&str> = kept.instruments.iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(names, ["EURUSD", "AAPL.US/USD"], "the listing first, the operator's row after it");
    assert_eq!(kept.added_by_operator, ["AAPL.US/USD"]);
    let row = &kept.instruments[1];
    assert_eq!((row.quote.as_str(), row.properties.tick_size), ("USD", 0.001));
    assert_eq!(row.asset_class(), vike_model::AssetClass::Cfd);
    assert_eq!(
        kept.refreshed_ms,
        Some(1_794_124_800_000),
        "the listing's own stamps are untouched"
    );
}

#[test]
fn a_bad_request_changes_nothing_and_a_server_with_no_store_says_so() {
    let hub = serve(true);
    let mut client = DatahubClient::connect(hub.addr).expect("handshake");
    let e = client
        .operator_instruments("nowhere", vec!["X".into()], Vec::new(), None)
        .expect_err("not a venue");
    assert!(e.contains("not a roster venue"), "{e}");
    let e = client
        .operator_instruments("ig", vec!["EUR USD".into()], Vec::new(), None)
        .expect_err("not a symbol");
    assert!(e.contains("is not a symbol"), "{e}");
    assert!(read_kept_list(hub.instruments.path(), "ig").is_none(), "no file was made");

    let bare = serve(false);
    let mut client = DatahubClient::connect(bare.addr).expect("handshake");
    let e = client.operator_instruments("ig", Vec::new(), Vec::new(), None).expect_err("no store");
    assert!(e.contains("no settings store"), "{e}");
}
