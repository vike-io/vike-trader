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
        base: None,
        expiry: None,
    }
}

/// **A datahub with no project keeps no list, and says so by not advertising one** (decision 0290): an empty answer
/// would read as "every venue keeps an empty list" and refuse every name a reader types, where a store that keeps no
/// list has its names read as typed. The store reader then sends nothing and answers `None`; the verb, asked anyway,
/// refuses in words. A datahub WITH a project serves its lists (the test below reads them).
#[test]
fn a_datahub_with_no_project_advertises_no_kept_lists() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(!client.serves_instrument_lists(), "advertised: {:?}", client.features());
    assert!(client.instrument_lists().is_err_and(|e| e.contains("no project folder")));
    let remote = vike_datahub_client::HistoryStoreDatahub::new(addr.to_string());
    assert!(remote.kept_instruments().expect("answered").is_none());
}

/// **A backtest takes the kept rows and the splits off ONE answer** (`HistoryStore::kept_instruments_and_splits`,
/// one `Request::InstrumentLists` where a run used to ask twice for the whole lists, 14 MB of JSON each on the CI box,
/// 2026-11-17), and it is what the two separate asks answer.
#[test]
fn the_kept_rows_and_the_splits_are_one_ask_answering_what_two_did() {
    let hub = serve(true);
    dukascopy_listing(hub.instruments.path());
    let remote = vike_datahub_client::HistoryStoreDatahub::new(hub.addr.to_string());
    let (kept, splits) = remote.kept_instruments_and_splits();
    let kept = kept.expect("answered").expect("a kept list");
    assert_eq!(kept.len(), 1, "dukascopy's one row: {kept:?}");
    assert_eq!(Some(kept), remote.kept_instruments().expect("answered"));
    assert_eq!(splits.expect("answered"), remote.stock_splits().expect("answered"));
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
    assert!(
        reply.outcomes[0].done.as_ref().is_err_and(|e| e.contains("--asset-class")),
        "{reply:?}"
    );
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

    // DUK-8: another spelling of a kept row (the venue's `EURUSD` by its door spelling, the hand row `AAPL.US/USD` by its
    // store name) would be a second row of one instrument; re-stating the same hand row stays an update.
    let reply = client
        .operator_instruments(
            "dukascopy",
            vec!["EUR/USD".into(), "AAPLUSUSD".into(), "AAPL.US/USD".into()],
            Vec::new(),
            Some(aapl()),
        )
        .expect("answered");
    for (outcome, kept_as) in reply.outcomes.iter().zip(["as EURUSD", "as AAPL.US/USD"]) {
        assert!(
            outcome.done.as_ref().is_err_and(|e| e.contains(kept_as)),
            "{}: {reply:?}",
            outcome.symbol
        );
    }
    assert!(reply.outcomes[2].done.is_ok(), "the same hand row again: {reply:?}");
    let kept = read_kept_list(hub.instruments.path(), "dukascopy").expect("kept");
    assert_eq!(kept.instruments.len(), 2, "no second row of one instrument: {kept:?}");
}

/// **One `add` renames a CFD named by hand, and one `rm` unnames it** (the owner's "fix" of 2026-11-20; it took an
/// `rm`, a `data catalog refresh` and an `add` on the CI box that day): dukascopy's `VOL.IDX/USD`, which the venue gives no
/// name, is named `VIX.CFD` by a hand row stating `VIX`, renamed `VOLA.CFD` by the next add stating `VOLA` (the hand row
/// replaces itself), and unnamed by the remove. The venue's row keeps its own base throughout.
#[test]
fn one_add_renames_a_cfd_named_by_hand_and_one_remove_unnames_it() {
    let hub = serve(true);
    let index = Contract::Cfd(vike_model::ContractCfd {
        underlying_kind: vike_model::ContractCfdUnderlying::Index,
        underlying: None,
        product: vike_model::ContractCfdProduct::Cfd,
        expiry: vike_model::ContractCfdExpiry::Rolling,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    });
    let vol = Instrument::new("dukascopy", "VOL.IDX/USD", "VOL.IDX", "USD", index)
        .with_properties(vike_model::SymbolProperties { tick_size: 0.01, ..Default::default() });
    let list = VenueInstrumentList {
        venue: "dukascopy".into(),
        refreshed_ms: Some(1_794_124_800_000),
        truncated: false,
        last_error: None,
        last_try_ms: Some(1_794_124_800_000),
        instruments: vec![vol],
        added_by_operator: Vec::new(),
    };
    write_list(hub.instruments.path(), &list).expect("the venue's own listing");
    let mut client = DatahubClient::connect(hub.addr).expect("handshake");
    let named = |base: &str| StatedInstrumentFacts {
        tick: 0.01,
        quote: "USD".into(),
        class: "index".into(),
        point_scale: 100.0,
        base: Some(base.into()),
        expiry: None,
    };
    let kept_row = || {
        read_kept_list(hub.instruments.path(), "dukascopy")
            .expect("kept")
            .instruments
            .into_iter()
            .find(|i| i.raw_symbol == "VOL.IDX/USD")
            .expect("the venue's row stays")
    };

    let reply = client
        .operator_instruments(
            "dukascopy",
            vec!["VOL.IDX/USD".into()],
            Vec::new(),
            Some(named("VIX")),
        )
        .expect("answered");
    assert!(reply.outcomes[0].done.is_ok(), "{reply:?}");
    assert_eq!(vike_catalog::user_name_of_row(&kept_row()), Ok("VIX.CFD".to_string()));

    let reply = client
        .operator_instruments(
            "dukascopy",
            vec!["VOL.IDX/USD".into()],
            Vec::new(),
            Some(named("VOLA")),
        )
        .expect("answered");
    assert!(
        reply.outcomes[0].done.as_ref().is_ok_and(|said| said
            .contains("was named VIX.CFD by hand")
            && said.contains("VOLA.CFD")),
        "{reply:?}"
    );
    assert_eq!(reply.rows.len(), 1, "the hand row replaced itself: {reply:?}");
    let row = kept_row();
    assert_eq!(
        vike_catalog::user_name_of_row(&row),
        Ok("VOLA.CFD".to_string()),
        "renamed by one add"
    );
    assert_eq!(row.base, "VOL.IDX", "the venue's base is never written over");

    let reply = client
        .operator_instruments("dukascopy", Vec::new(), vec!["VOL.IDX/USD".into()], None)
        .expect("answered");
    assert!(reply.outcomes[0].done.is_ok() && reply.rows.is_empty(), "{reply:?}");
    let row = kept_row();
    assert!(vike_catalog::user_name_of_row(&row).is_err(), "unnamed by one remove: {row:?}");
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
