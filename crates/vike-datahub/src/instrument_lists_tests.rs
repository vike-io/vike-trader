use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::catalog::{CatalogFn, CatalogTable};
use vike_catalog::read_kept_lists;
use vike_model::{Contract, Instrument};

const T: i64 = 1_794_124_800_000;

fn instrument(venue: &str, symbol: &str) -> Instrument {
    Instrument::new(venue, symbol, "BTC", "USDT", Contract::CryptoSpot)
}

/// `list` with its rows' names spelled, as the refresh writes it (`vike_catalog::spelled`).
fn named(mut list: VenueInstrumentList) -> VenueInstrumentList {
    list.instruments = vike_catalog::spelled(list.instruments);
    list
}

/// A provider that answers `answers[n]` on its n-th call (the last answer repeats), counting the calls.
fn scripted(
    venue: &str,
    answers: Vec<Result<usize, String>>,
    calls: Arc<AtomicUsize>,
) -> (String, CatalogFn) {
    let v = venue.to_string();
    let provider: CatalogFn = Box::new(move || {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        match answers[n.min(answers.len() - 1)].clone() {
            Ok(count) => Ok((0..count).map(|i| instrument(&v, &format!("SYM{i}"))).collect()),
            Err(e) => Err(e.into()),
        }
    });
    (venue.to_string(), provider)
}

/// A lane with NO memo (a zero TTL), so every refresh asks the provider; its bucket admits two fetches back to back.
fn lane(entries: Vec<(String, CatalogFn)>) -> CatalogLane {
    CatalogLane::new(CatalogTable::new(entries), Duration::ZERO)
}

fn listed(n: usize) -> Result<CatalogOutcome, String> {
    Ok(CatalogOutcome::Listed {
        instruments: (0..n).map(|i| instrument("binance", &format!("S{i}"))).collect(),
        truncated: false,
        cached: false,
    })
}

/// **No timer writes a kept list; the operator's refresh is the one writer** (the owner's ruling of 2026-11-16, every
/// venue). This module starts no thread, and the daemon's boot calls ONE thing of it, once: the naming of a list an
/// older build wrote (`name_older_kept_lists`, the owner's "make it smart" of 2026-11-17), which never rewrites a
/// named file. Beyond that only the verbs the server routes (`server/route.rs`: the read, the refresh) and the
/// operator's own rows (`server/operator_instruments.rs`) reach it.
///
/// MUTATION PROOF: put a `std::thread::Builder` back into this module, or another call of it into `datahub_cli.rs`.
#[test]
fn no_timer_writes_a_kept_list() {
    let module = include_str!("instrument_lists.rs");
    for spawn in ["thread::", "spawn(", "sleep("] {
        assert!(!module.contains(spawn), "the kept lists' module starts no timer: `{spawn}`");
    }
    let boot = include_str!("datahub_cli.rs");
    assert_eq!(
        boot.matches("instrument_lists::").count(),
        1,
        "the boot calls one thing of the kept lists"
    );
    assert!(
        boot.contains("instrument_lists::name_older_kept_lists("),
        "and that one thing is the naming of an older build's lists"
    );
}

/// **The refresh rewrites the kept file at once, atomically, the operator's rows kept** — the case measured on the CI box
/// (v0.1.102, 2026-11-16): a dukascopy file of 11-12 with 89 rows, many of them without a tick, and a refresh that
/// answered 905 rows from the list the account's login handed over. The file must hold the new listing and the
/// operator's row the moment the refresh answers, with no timer involved, one venue or every venue alike.
#[test]
fn a_refresh_rewrites_the_kept_file_at_once_keeping_the_operators_rows() {
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let hand = vike_secrets::operator_instrument::OperatorInstrument {
        asked: "AAPL.US/USD".into(),
        added: "2026-11-13".into(),
        instrument: instrument("dukascopy", "AAPL.US/USD"),
    };
    vike_secrets::operator_instrument::write_operator_instrument(settings.path(), &hand)
        .expect("row");
    let old = with_operator_rows(
        fold(
            "dukascopy",
            None,
            &Ok(CatalogOutcome::Listed {
                instruments: (0..3).map(|i| instrument("dukascopy", &format!("OLD{i}"))).collect(),
                truncated: false,
                cached: false,
            }),
            T,
        ),
        "dukascopy",
        std::slice::from_ref(&hand.instrument),
    )
    .expect("the 11-12 file");
    write_list(dir.path(), &old).expect("written");
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings.path())),
        vike_secrets::CredentialScope::Full,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let lane = lane(vec![
        scripted("dukascopy", vec![Ok(5)], Arc::clone(&calls)),
        scripted("binance", vec![Ok(2)], Arc::clone(&calls)),
    ]);

    let one = refresh_instrument_lists_verb(
        Some(dir.path()),
        Some(&store),
        Some(&lane),
        Some("dukascopy"),
    );
    let Response::InstrumentListsRefreshed(rows) = one else { panic!("{one:?}") };
    assert!(rows[0].outcome.is_ok(), "{rows:?}");
    let kept = read_kept_list(dir.path(), "dukascopy").expect("kept");
    let names: Vec<&str> = kept.instruments.iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(
        names,
        ["SYM0", "SYM1", "SYM2", "SYM3", "SYM4", "AAPL.US/USD"],
        "the new listing and the hand row"
    );
    assert_eq!(kept.added_by_operator, ["AAPL.US/USD"]);
    assert!(kept.refreshed_ms.is_some_and(|at| at > T), "stamped now: {:?}", kept.refreshed_ms);
    assert!(read_kept_list(dir.path(), "binance").is_none(), "one venue touches one file");

    let all = refresh_instrument_lists_verb(Some(dir.path()), Some(&store), Some(&lane), None);
    let Response::InstrumentListsRefreshed(rows) = all else { panic!("{all:?}") };
    assert_eq!(rows.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["dukascopy", "binance"]);
    assert_eq!(read_kept_list(dir.path(), "binance").expect("every venue").instruments.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "one fetch per venue per refresh, nothing else");
}

#[test]
fn the_fold_replaces_on_a_listing_and_keeps_the_list_on_a_failure_or_an_empty_answer() {
    let kept = fold("binance", None, &listed(3), T).expect("written");
    assert_eq!(
        (kept.instruments.len(), kept.refreshed_ms, kept.last_error.clone()),
        (3, Some(T), None)
    );

    let failed =
        fold("binance", Some(kept.clone()), &Err("the venue did not answer".into()), T + 5)
            .expect("written");
    assert_eq!(failed.instruments, kept.instruments, "a failure keeps the list");
    assert_eq!(failed.refreshed_ms, Some(T), "and its age");
    assert_eq!(failed.last_try_ms, Some(T + 5));
    assert_eq!(failed.last_error.as_deref(), Some("the venue did not answer"));

    let empty = fold("binance", Some(kept.clone()), &listed(0), T + 6).expect("written");
    assert_eq!(empty.instruments, kept.instruments, "an empty answer never replaces a list");
    assert!(empty.last_error.as_deref().is_some_and(|e| e.contains("answered with nothing")));

    let replaced = fold("binance", Some(failed), &listed(1), T + 7).expect("written");
    assert_eq!(
        (replaced.instruments.len(), replaced.last_error),
        (1, None),
        "a success clears the error"
    );
    assert!(
        fold("binance", Some(kept), &Ok(CatalogOutcome::NotArmed), T).is_none(),
        "a refusal changes nothing"
    );
}

#[test]
fn a_failed_refresh_keeps_the_previous_list_and_says_why() {
    let dir = tempfile::tempdir().expect("temp");
    let calls = Arc::new(AtomicUsize::new(0));
    let lane = lane(vec![scripted("binance", vec![Ok(2), Err("503".into())], calls)]);
    assert!(refresh_venue(dir.path(), None, Some(&lane), "binance", T).outcome.is_ok());
    let answer = refresh_venue(dir.path(), None, Some(&lane), "binance", T + 1);
    assert!(answer.outcome.as_ref().is_err_and(|e| e.contains("503")), "{answer:?}");
    let kept = read_kept_list(dir.path(), "binance").expect("kept");
    assert_eq!(kept.instruments.len(), 2, "the old list stands");
    assert_eq!(kept.refreshed_ms, Some(T));
    assert!(kept.last_error.as_deref().is_some_and(|e| e.contains("503")));
}

/// **No held session is a routine refusal: no file, no error, nothing kept changes** (a venue listed
/// from a session the gatewayhub holds, with no account of it running). The venue here is only the
/// lane's key; the provider's answer is what is under test.
#[test]
fn no_held_session_changes_nothing_and_is_no_error() {
    let dir = tempfile::tempdir().expect("temp");
    let provider: CatalogFn = Box::new(|| {
        Err(crate::catalog::CatalogNotListed::NoSession("no account holds a session".into()))
    });
    let lane = lane(vec![("binance".to_string(), provider)]);
    let answer = refresh_venue(dir.path(), None, Some(&lane), "binance", T);
    assert_eq!(
        answer.outcome,
        Ok(CatalogOutcome::Refused(vike_datahub_client::catalog::CatalogRefusal::NeedsCredentials))
    );
    assert!(read_kept_list(dir.path(), "binance").is_none(), "nothing is written for a refusal");

    // Over a kept (named) list too: it stands exactly as it was.
    let kept = named(fold("binance", None, &listed(2), T).expect("written"));
    write_list(dir.path(), &kept).expect("write");
    refresh_venue(dir.path(), None, Some(&lane), "binance", T + 5);
    assert_eq!(read_kept_list(dir.path(), "binance"), Some(kept));
}

/// **DER-2**: deribit's list is kept like every venue's, only when the operator asks (the owner: no timers), and a
/// refresh asked for it writes `deribit.json`, the file a deribit backtest reads its rows from.
#[test]
fn deribit_is_kept_by_a_refresh_asked_for() {
    let dir = tempfile::tempdir().expect("temp");
    let calls = Arc::new(AtomicUsize::new(0));
    let lane = lane(vec![scripted("deribit", vec![Ok(3)], Arc::clone(&calls))]);
    assert!(lane.table().supported().contains(&"deribit"), "the lane serves it");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing asks deribit before the refresh");
    assert!(read_kept_list(dir.path(), "deribit").is_none());

    let asked = refresh_instrument_lists_verb(Some(dir.path()), None, Some(&lane), Some("deribit"));
    let Response::InstrumentListsRefreshed(rows) = asked else { panic!("{asked:?}") };
    assert!(rows[0].outcome.is_ok(), "{rows:?}");
    assert_eq!(read_kept_list(dir.path(), "deribit").expect("kept").instruments.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "only the refresh asked for it reached the venue");
}

#[test]
fn refresh_now_serves_one_venue_or_every_venue_the_lane_serves() {
    let dir = tempfile::tempdir().expect("temp");
    let none = Arc::new(AtomicUsize::new(0));
    let lane = lane(vec![
        scripted("binance", vec![Ok(1)], Arc::clone(&none)),
        scripted("okx", vec![Ok(2)], Arc::clone(&none)),
    ]);
    let one = refresh_instrument_lists_verb(Some(dir.path()), None, Some(&lane), Some("okx"));
    let Response::InstrumentListsRefreshed(rows) = one else { panic!("{one:?}") };
    assert_eq!(rows.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["okx"]);
    assert!(read_kept_list(dir.path(), "binance").is_none(), "one venue touches one file");

    let all = refresh_instrument_lists_verb(Some(dir.path()), None, Some(&lane), None);
    let Response::InstrumentListsRefreshed(rows) = all else { panic!("{all:?}") };
    assert_eq!(rows.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["binance", "okx"]);
    assert!(rows.iter().all(|r| r.outcome.is_ok()), "{rows:?}");
    let read = instrument_lists_verb(Some(dir.path()), None, None);
    let Response::InstrumentLists(reply) = read else { panic!("{read:?}") };
    assert_eq!(reply.venues.iter().map(|l| l.instruments.len()).collect::<Vec<_>>(), [1, 2]);

    let unarmed = refresh_instrument_lists_verb(Some(dir.path()), None, None, Some("binance"));
    let Response::InstrumentListsRefreshed(rows) = unarmed else { panic!("{unarmed:?}") };
    assert_eq!(rows[0].outcome, Ok(CatalogOutcome::NotArmed), "an unarmed lane changes nothing");
    assert_eq!(read_kept_list(dir.path(), "binance").expect("kept").instruments.len(), 1);
}

/// **An FX venue's kept list survives a disconnect, and is served from its file** (owner 2026-11-14: a backtest of a
/// closed venue's symbol runs on what was stored from it). For each of the five FX-week venues: a refresh while its
/// venue is not connected (the fetch fails, here with the FX login gate's own words, or there is no lane at all, the
/// gatewayhub away) keeps the list it held, and `Request::InstrumentLists`, the one verb a backtest and every client
/// read the lists through, answers it from `<project>/instruments/<venue>.json` with no lane and no venue.
#[test]
fn an_fx_venues_kept_list_survives_a_disconnect_and_is_served_from_its_file() {
    const FX: [&str; 5] = ["ctrader", "dukascopy", "fxcm", "ig", "oanda"];
    let dir = tempfile::tempdir().expect("temp");
    for venue in FX {
        let listed = Ok(CatalogOutcome::Listed {
            instruments: vec![instrument(venue, "EURUSD")],
            truncated: false,
            cached: false,
        });
        let kept = named(fold(venue, None, &listed, T).expect("written"));
        write_list(dir.path(), &kept).expect("kept");
        let calls = Arc::new(AtomicUsize::new(0));
        let closed = Err("FX market closed: login at 2026-11-15T21:00:00Z".to_string());
        let lane = lane(vec![scripted(venue, vec![closed], Arc::clone(&calls))]);
        // Each is a held-session venue (ig too, since 2026-11-16): its fetch fails, and a failure lists nothing.
        let answer = refresh_venue(dir.path(), None, Some(&lane), venue, T + 1);
        assert!(
            !matches!(answer.outcome, Ok(CatalogOutcome::Listed { .. })),
            "{venue}: {answer:?}"
        );
        let _ = refresh_venue(dir.path(), None, None, venue, T + 2);
        let now = read_kept_list(dir.path(), venue).expect("still kept");
        assert_eq!(now.instruments, kept.instruments, "{venue}: the kept rows stand");
        assert_eq!(now.refreshed_ms, Some(T), "{venue}: and their age");
    }
    let read = instrument_lists_verb(Some(dir.path()), None, None);
    let Response::InstrumentLists(reply) = read else { panic!("{read:?}") };
    let mut served: Vec<&str> = reply.venues.iter().map(|l| l.venue.as_str()).collect();
    served.sort_unstable();
    assert_eq!(served, FX, "every FX venue's kept list is served from its file");
    assert!(reply.venues.iter().all(|l| l.instruments.len() == 1), "{reply:?}");
}

/// **Atomic**: a write replaces an existing file whole (a rename onto it, which `write_atomic` makes work on Windows
/// too), leaves no temp file behind, and a stray file in the folder is read by nothing.
#[test]
fn a_list_is_written_whole_over_the_last_and_a_stray_file_is_ignored() {
    let dir = tempfile::tempdir().expect("temp");
    let first = fold("binance", None, &listed(3), T).expect("written");
    write_list(dir.path(), &first).expect("first");
    let second = fold("binance", Some(first), &listed(1), T + 1).expect("written");
    write_list(dir.path(), &second).expect("over it");
    assert_eq!(
        read_kept_list(dir.path(), "binance"),
        Some(named(second)),
        "written with its names"
    );
    std::fs::write(dir.path().join("catalog.json"), b"{ a desktop's old cache").expect("stray");
    std::fs::write(dir.path().join("okx.json"), b"not json").expect("corrupt");
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 3, "no temp file left behind: {names:?}");
    assert_eq!(read_kept_lists(dir.path()).len(), 1, "only a venue's own parsable file is a list");
}

/// **The one rule of the operator's rows**: a row added by hand joins where the venue's listing has no row of its
/// name (the venue's own row wins), the joined rows are named, and the split gives the listing back exactly.
#[test]
fn an_operator_row_joins_only_where_the_listing_has_none_and_splits_back_out() {
    let listing = fold("binance", None, &listed(2), T).expect("written");
    let hand = [
        instrument("binance", "S1"),
        instrument("binance", "HANDMADE"),
        instrument("okx", "ELSEWHERE"),
    ];
    let merged = with_operator_rows(Some(listing.clone()), "binance", &hand).expect("a list");
    let names: Vec<&str> = merged.instruments.iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(
        names,
        ["S0", "S1", "HANDMADE"],
        "S1 is the venue's own; another venue's row stays out"
    );
    assert_eq!(merged.added_by_operator, ["HANDMADE"]);
    let (back, carried) = split_operator_rows(Some(merged));
    assert_eq!(back, Some(listing), "the split is the listing, byte for byte");
    assert_eq!(carried, [instrument("binance", "HANDMADE")]);

    assert_eq!(with_operator_rows(None, "ig", &[]), None, "no listing and no row: no list");
    let only =
        with_operator_rows(None, "ig", &[instrument("ig", "CS.D.EURUSD.MINI.IP")]).expect("one");
    assert_eq!((only.refreshed_ms, only.added_by_operator.len()), (None, 1));
}

/// **A listing row with no price step is no fact**: a hand row that states one takes its place and is named; a listing
/// row that states a step still wins; with no step on either side the listing's row stays. The split gives the listing
/// back without the replaced row, and folding the same hand rows over it again writes the same list.
#[test]
fn a_listing_row_with_no_price_step_gives_way_to_a_hand_row_that_states_one() {
    let row = |symbol: &str, tick_size: f64| {
        instrument("dukascopy", symbol)
            .with_properties(vike_model::SymbolProperties { tick_size, ..Default::default() })
    };
    let mut listing = empty_list("dukascopy");
    listing.instruments = vec![row("EUR/USD", 0.0), row("GBP/USD", 0.00001), row("USD/JPY", 0.0)];
    let hand = [row("EUR/USD", 0.00001), row("GBP/USD", 0.0001), row("USD/JPY", 0.0)];
    let merged = with_operator_rows(Some(listing.clone()), "dukascopy", &hand).expect("a list");
    assert_eq!(
        merged.instruments,
        [row("GBP/USD", 0.00001), row("USD/JPY", 0.0), row("EUR/USD", 0.00001)]
    );
    assert_eq!(merged.added_by_operator, ["EUR/USD"]);

    let (back, carried) = split_operator_rows(Some(merged.clone()));
    let mut without = listing;
    without.instruments.remove(0);
    assert_eq!(back, Some(without), "the replaced listing row is gone until the next listing");
    assert_eq!(carried, [row("EUR/USD", 0.00001)]);
    assert_eq!(
        with_operator_rows(back, "dukascopy", &hand),
        Some(merged),
        "a second fold changes nothing"
    );
}

/// **A hand row names only a CFD the venue gives no name** (`lent_naming`, decision 0290's O-6): dukascopy's index row
/// of base `USA500.IDX` takes the name `SPX500.CFD` spelled from the hand row's base `SPX500`, keeping its own base and
/// step; a venue row that has a name keeps it; a hand row whose base is no word lends nothing; and no other class is
/// named by hand (an FX row the grammar cannot name stays unnamed).
///
/// MUTATION PROOF: drop the `Unnamed` check of `lent_naming` and the named row is renamed; drop the class check and the
/// FX row is named.
#[test]
fn a_hand_row_names_only_a_cfd_the_venue_gives_no_name() {
    let cfd = |symbol: &str, base: &str, underlying_kind| {
        Instrument::new(
            "dukascopy",
            symbol,
            base,
            "USD",
            Contract::Cfd(vike_model::ContractCfd {
                underlying_kind,
                underlying: None,
                product: vike_model::ContractCfdProduct::Cfd,
                expiry: vike_model::ContractCfdExpiry::Rolling,
                margin_rate: None,
                swap_long: None,
                swap_short: None,
            }),
        )
        .with_properties(vike_model::SymbolProperties { tick_size: 0.001, ..Default::default() })
    };
    let index = vike_model::ContractCfdUnderlying::Index;
    let fx = |symbol: &str, base: &str| {
        Instrument::new("dukascopy", symbol, base, "USD", Contract::Fx(Default::default()))
            .with_properties(vike_model::SymbolProperties {
                tick_size: 0.001,
                ..Default::default()
            })
    };
    let mut listing = empty_list("dukascopy");
    listing.instruments = vec![
        cfd("USA500.IDX/USD", "USA500.IDX", index),
        cfd("USATECH.IDX/USD", "USATECH", index),
        cfd("GBR.IDX/GBP", "GBR.IDX", index),
        fx("ODD/USD", "OD.D"),
    ];
    let listing = named(listing);
    let hand = [
        cfd("USA500.IDX/USD", "SPX500", index),
        cfd("USATECH.IDX/USD", "NAS100", index),
        cfd("GBR.IDX/GBP", "UK.100", index),
        fx("ODD/USD", "ODD"),
    ];
    let merged = with_operator_rows(Some(listing), "dukascopy", &hand).expect("a list");
    let names: Vec<(String, Result<String, String>)> = merged
        .instruments
        .iter()
        .map(|i| (i.base.clone(), vike_catalog::user_name_of_row(i)))
        .collect();
    assert_eq!(
        names[0],
        ("USA500.IDX".to_string(), Ok("SPX500.CFD".to_string())),
        "named by hand, its own base kept"
    );
    assert_eq!(
        names[1],
        ("USATECH".to_string(), Ok("USATECH.CFD".to_string())),
        "its own name stays"
    );
    assert!(names[2].1.is_err(), "a base of no word lends nothing: {:?}", names[2]);
    assert!(names[3].1.is_err(), "an FX row is not named by hand: {:?}", names[3]);
    assert!(merged.added_by_operator.is_empty(), "every row is the venue's own");
    assert_eq!(merged.instruments[0].properties.tick_size, 0.001, "the venue's facts stay");
}

/// **A name lent by hand follows the operator's rows alone** (the owner's "fix" of 2026-11-20): dukascopy's
/// `VOL.IDX/USD`, named `VIX.CFD` by a hand row, is renamed `VOLA.CFD` by the next fold of a hand row stating `VOLA`
/// (one `add`, no refresh), is unnamed by a fold with no hand row of it (one `rm`), and keeps its own base throughout.
/// IG's own stated names are never taken back: its catalog names its rows (`NAMED_BY_THEIR_CATALOG`).
///
/// MUTATION PROOF: drop the take-back loop of `with_operator_rows` and the renamed and the removed row keep `VIX.CFD`;
/// drop `is_lent`'s venue check and IG's `DE40.CFD` is taken back.
#[test]
fn a_name_lent_by_hand_is_renamed_by_one_add_and_taken_back_by_one_remove() {
    let cfd = |venue: &str, symbol: &str, base: &str| {
        Instrument::new(
            venue,
            symbol,
            base,
            "USD",
            Contract::Cfd(vike_model::ContractCfd {
                underlying_kind: vike_model::ContractCfdUnderlying::Index,
                underlying: None,
                product: vike_model::ContractCfdProduct::Cfd,
                expiry: vike_model::ContractCfdExpiry::Rolling,
                margin_rate: None,
                swap_long: None,
                swap_short: None,
            }),
        )
        .with_properties(vike_model::SymbolProperties { tick_size: 0.01, ..Default::default() })
    };
    let mut listing = empty_list("dukascopy");
    listing.instruments = vec![cfd("dukascopy", "VOL.IDX/USD", "VOL.IDX")];
    let listing = named(listing);
    let fold = |list: Option<VenueInstrumentList>, base: Option<&str>| {
        let hand: Vec<Instrument> =
            base.map(|b| cfd("dukascopy", "VOL.IDX/USD", b)).into_iter().collect();
        let merged = with_operator_rows(list, "dukascopy", &hand).expect("a list");
        let row = merged.instruments[0].clone();
        (split_operator_rows(Some(merged)).0, row)
    };
    let (kept, row) = fold(Some(listing), Some("VIX"));
    assert_eq!(
        (row.base.as_str(), vike_catalog::user_name_of_row(&row)),
        ("VOL.IDX", Ok("VIX.CFD".to_string()))
    );
    let (kept, row) = fold(kept, Some("VOLA"));
    assert_eq!(
        vike_catalog::user_name_of_row(&row),
        Ok("VOLA.CFD".to_string()),
        "renamed by one fold"
    );
    let (_, row) = fold(kept, None);
    assert!(
        vike_catalog::user_name_of_row(&row).is_err(),
        "unnamed once its hand row is gone: {row:?}"
    );
    assert_eq!(row.base, "VOL.IDX", "the venue's base was never written over");

    let mut ig = cfd("ig", "IX.D.DAX.IFD.IP", "IX.D.DAX");
    ig.naming = Some(vike_model::InstrumentNaming::User("DE40.CFD".into()));
    let mut ig_listing = empty_list("ig");
    ig_listing.instruments = vec![ig];
    let merged = with_operator_rows(Some(ig_listing), "ig", &[]).expect("a list");
    assert_eq!(
        vike_catalog::user_name_of_row(&merged.instruments[0]),
        Ok("DE40.CFD".to_string()),
        "IG's own name"
    );
}

/// **A refresh never erases a row added by hand**: a new listing replaces the venue's rows and the operator's row is
/// folded back in from the settings database; an empty answer and a failure keep both; a row removed from the
/// database leaves the list at the next refresh; a store that will not read keeps the rows the file carried.
#[test]
fn a_refresh_never_erases_a_row_added_by_hand() {
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let hand = vike_secrets::operator_instrument::OperatorInstrument {
        asked: "HANDMADE".into(),
        added: "2026-11-13".into(),
        instrument: instrument("binance", "HANDMADE"),
    };
    vike_secrets::operator_instrument::write_operator_instrument(settings.path(), &hand)
        .expect("row");
    // A lane per refresh: one lane's bucket admits two fetches back to back.
    let answering = |answer: Result<usize, String>| {
        lane(vec![scripted("binance", vec![answer], Arc::new(AtomicUsize::new(0)))])
    };
    let names = || -> Vec<String> {
        read_kept_list(dir.path(), "binance")
            .expect("kept")
            .instruments
            .into_iter()
            .map(|i| i.raw_symbol)
            .collect()
    };

    assert!(
        refresh_venue(dir.path(), Some(settings.path()), Some(&answering(Ok(2))), "binance", T)
            .outcome
            .is_ok()
    );
    assert_eq!(names(), ["SYM0", "SYM1", "HANDMADE"]);
    refresh_venue(dir.path(), Some(settings.path()), Some(&answering(Ok(3))), "binance", T + 1);
    assert_eq!(names(), ["SYM0", "SYM1", "SYM2", "HANDMADE"], "a new listing, the row folded back");
    let empty =
        refresh_venue(dir.path(), Some(settings.path()), Some(&answering(Ok(0))), "binance", T + 2);
    assert!(
        empty.outcome.is_err(),
        "an empty answer over a LISTING is kept, the hand row not counted"
    );
    assert_eq!(names(), ["SYM0", "SYM1", "SYM2", "HANDMADE"]);

    // A store that will not read: the rows the file carried stay (no store at all reads the same way).
    refresh_venue(dir.path(), None, Some(&answering(Err("503".into()))), "binance", T + 3);
    assert_eq!(names(), ["SYM0", "SYM1", "SYM2", "HANDMADE"]);

    vike_secrets::operator_instrument::delete_operator_instrument(
        settings.path(),
        "binance",
        "HANDMADE",
    )
    .expect("removed");
    keep_operator_rows(dir.path(), settings.path(), "binance").expect("folded now");
    assert_eq!(names(), ["SYM0", "SYM1", "SYM2"], "removed from the store, gone from the list");
    assert!(read_kept_list(dir.path(), "binance").expect("kept").added_by_operator.is_empty());
}

/// Today's lists, with no row added by hand anywhere: a routine refusal still writes nothing and a kept list stands
/// byte for byte (the operator's fold changes nothing it does not carry).
#[test]
fn with_no_operator_row_a_refresh_writes_what_it_wrote_before() {
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let unarmed = refresh_venue(dir.path(), Some(settings.path()), None, "binance", T);
    assert_eq!(unarmed.outcome, Ok(CatalogOutcome::NotArmed));
    assert!(read_kept_list(dir.path(), "binance").is_none(), "no file for a refusal");
    let calls = Arc::new(AtomicUsize::new(0));
    let lane = lane(vec![scripted("binance", vec![Ok(2)], calls)]);
    refresh_venue(dir.path(), Some(settings.path()), Some(&lane), "binance", T);
    assert_eq!(
        read_kept_list(dir.path(), "binance"),
        fold(
            "binance",
            None,
            &{
                Ok(CatalogOutcome::Listed {
                    instruments: (0..2)
                        .map(|i| instrument("binance", &format!("SYM{i}")))
                        .collect(),
                    truncated: false,
                    cached: false,
                })
            },
            T
        )
        .map(named),
        "the same list, each row with its names"
    );
}

/// **A kept list's names are written ONCE, by its writer** (decision 0290, the owner's ruling «Б» of 2026-11-16,
/// simplified): a refresh writes each listed row's ONE stored name, its user name, into the file, a row the grammar
/// cannot name stores why, and an operator's row added by hand (`vike-cli data instruments add`, folded in by
/// `keep_operator_rows`) is written with its own; a reader of the file finds a row by its stored name, and a stored
/// series by the store's rule over it.
#[test]
fn a_refresh_and_an_operators_row_write_their_names_into_the_kept_file() {
    use vike_model::{
        ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying, ContractFx,
        InstrumentNaming,
    };
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let pair =
        Instrument::new("dukascopy", "EUR/USD", "EUR", "USD", Contract::Fx(ContractFx::default()));
    let index = Instrument::new("dukascopy", "USA500.IDX/USD", "", "USD", Contract::Index);
    // A naming the venue's catalog states on its own row is kept as stated, never spelled over.
    let mut stated = Instrument::new("dukascopy", "USA30.IDX/USD", "", "USD", Contract::Index);
    stated.naming = Some(InstrumentNaming::User("US30.CFD".into()));
    let provider: CatalogFn =
        Box::new(move || Ok(vec![pair.clone(), index.clone(), stated.clone()]));
    let lane = lane(vec![("dukascopy".to_string(), provider)]);
    let answer = refresh_venue(dir.path(), Some(settings.path()), Some(&lane), "dukascopy", T);
    assert!(answer.outcome.is_ok(), "{answer:?}");
    let kept = read_kept_list(dir.path(), "dukascopy").expect("kept");
    assert_eq!(
        kept.instruments[0].naming,
        Some(InstrumentNaming::User("EURUSD".into())),
        "the ONE name a row stores is its user name"
    );
    let names = vike_catalog::names_of_row(&kept.instruments[0]).expect("the pair is named");
    assert_eq!((names.user.as_str(), names.wire.as_str()), ("EURUSD", "EUR/USD"));
    assert!(
        matches!(&kept.instruments[1].naming, Some(InstrumentNaming::Unnamed(why)) if why.contains("no user name")),
        "the index stores why: {:?}",
        kept.instruments[1].naming
    );
    assert_eq!(
        kept.instruments[2].naming,
        Some(InstrumentNaming::User("US30.CFD".into())),
        "a stated naming is kept"
    );

    let share = Contract::Cfd(ContractCfd {
        underlying_kind: ContractCfdUnderlying::Share,
        underlying: None,
        product: ContractCfdProduct::Cfd,
        expiry: ContractCfdExpiry::Unknown,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    });
    let hand = vike_secrets::operator_instrument::OperatorInstrument {
        asked: "AAPL.US/USD".into(),
        added: "2026-11-16".into(),
        instrument: Instrument::new("dukascopy", "AAPL.US/USD", "", "USD", share),
    };
    vike_secrets::operator_instrument::write_operator_instrument(settings.path(), &hand)
        .expect("row");
    keep_operator_rows(dir.path(), settings.path(), "dukascopy").expect("folded now");
    let kept = read_kept_list(dir.path(), "dukascopy").expect("kept");
    let row =
        kept.instruments.iter().find(|i| i.raw_symbol == "AAPL.US/USD").expect("the hand row");
    assert_eq!(row.naming, Some(InstrumentNaming::User("AAPL.CFD".into())));
    let names = vike_catalog::names_of_row(row).expect("the hand row is named");
    assert_eq!((names.user.as_str(), names.wire.as_str()), ("AAPL.CFD", "AAPL.US/USD"));
    let found = vike_catalog::kept_row_of_user_name(&kept.instruments, "dukascopy", "aapl.cfd")
        .expect("its stored user name finds it");
    assert_eq!(found.raw_symbol, "AAPL.US/USD");
    assert_eq!(
        vike_catalog::kept_row_of_store_name(&kept.instruments, "dukascopy", "EURUSD")
            .map(|row| row.raw_symbol.as_str()),
        Ok("EUR/USD")
    );
}

/// **Two user names one store path would hold are refused at the write** (the owner's ruling «Б», 2026-11-16): a
/// listing in which `XYZ:TSLAUSDC.P` and `XYZ-TSLAUSDC.P` (one series each, by the store's `:`-to-`-` rule, the same
/// one) sit on ONE venue is refused in words naming both rows and both user names, nothing is written and the old file
/// stands; another venue holding the same names keeps its list, since the venue is part of every store path.
#[test]
fn a_list_two_of_whose_user_names_share_a_store_path_is_refused_and_the_old_file_stands() {
    use vike_model::InstrumentNaming;
    // The rows state their naming, as a venue's catalog may: the grammar spells no `-`.
    let named = |venue: &str, raw: &str, user: &str| {
        let mut row = instrument(venue, raw);
        row.naming = Some(InstrumentNaming::User(user.into()));
        row
    };
    let colon = move |venue: &str| named(venue, "xyz:TSLA", "XYZ:TSLAUSDC.P");
    let dash = move |venue: &str| named(venue, "XYZ-TSLA", "XYZ-TSLAUSDC.P");
    let dir = tempfile::tempdir().expect("temp");
    let first = Ok(CatalogOutcome::Listed {
        instruments: vec![colon("binance")],
        truncated: false,
        cached: false,
    });
    write_list(dir.path(), &fold("binance", None, &first, T).expect("a list")).expect("written");
    let before = read_kept_list(dir.path(), "binance");

    let both: CatalogFn = Box::new(move || Ok(vec![colon("binance"), dash("binance")]));
    let okx: CatalogFn = Box::new(move || Ok(vec![dash("okx")]));
    let lanes = lane(vec![("binance".to_string(), both), ("okx".to_string(), okx)]);
    // okx keeps the dash name beside binance's colon name: another venue is another store path.
    let kept = refresh_venue(dir.path(), None, Some(&lanes), "okx", T + 1);
    assert!(kept.outcome.is_ok(), "{kept:?}");
    let okx_before = read_kept_list(dir.path(), "okx");
    assert!(okx_before.is_some());

    let refused = refresh_venue(dir.path(), None, Some(&lanes), "binance", T + 1);
    let why = refused.outcome.expect_err("two user names, one store path");
    for word in ["`xyz:TSLA`", "`XYZ:TSLAUSDC.P`", "`XYZ-TSLA`", "`XYZ-TSLAUSDC.P`", "not kept"] {
        assert!(why.contains(word), "{word}: {why}");
    }
    assert_eq!(
        read_kept_list(dir.path(), "binance"),
        before,
        "nothing written: the old file stands"
    );
    assert_eq!(read_kept_list(dir.path(), "okx"), okx_before, "another venue is untouched");
}

/// **The operator's share splits ride with the kept lists** (decision 0289): `Request::InstrumentLists` answers the
/// `stock_split` rows of the settings database, read at the ask; a datahub with no settings store answers none.
#[test]
fn the_operators_splits_ride_with_the_kept_lists() {
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings.path())),
        vike_secrets::CredentialScope::Full,
    );
    let splits = |store: Option<&vike_secrets::CredentialStore>| {
        let read = instrument_lists_verb(Some(dir.path()), store, None);
        let Response::InstrumentLists(reply) = read else { panic!("{read:?}") };
        reply.splits
    };
    assert!(splits(Some(&store)).is_empty(), "no row, none served");

    let split =
        vike_model::StockSplit::new("dukascopy", "AAPLUSUSD", "2020-08-31", 4.0).expect("a split");
    vike_secrets::stock_split::write_stock_split(settings.path(), &split).expect("row");
    assert_eq!(splits(Some(&store)), [split], "served at the next ask, no restart");
    assert!(splits(None).is_empty(), "no settings store, none");
}

/// **The lists of named venues are the whole answer's, byte for byte** (`Request::InstrumentListsOfVenues`): each named
/// venue's kept list and share splits exactly as the whole answer carries them, in its order, the same as the one
/// filter over the whole answer (`InstrumentListsReply::of_venues`, a client's fallback), and a venue that keeps no
/// list answers none.
#[test]
fn the_lists_of_named_venues_are_the_whole_answers_byte_for_byte() {
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings.path())),
        vike_secrets::CredentialScope::Full,
    );
    for (venue, rows) in [("okx", 3), ("binance", 5), ("dukascopy", 4)] {
        write_list(dir.path(), &kept_of(venue, rows, T)).expect("a list");
    }
    for venue in ["dukascopy", "binance"] {
        let split = vike_model::StockSplit::new(venue, "SYM0", "2020-08-31", 4.0).expect("a split");
        vike_secrets::stock_split::write_stock_split(settings.path(), &split).expect("row");
    }
    let reply = |venues: Option<&[String]>| {
        let read = instrument_lists_verb(Some(dir.path()), Some(&store), venues);
        let Response::InstrumentLists(reply) = read else { panic!("{read:?}") };
        reply
    };
    fn json<T: serde::Serialize + ?Sized>(value: &T) -> Vec<u8> {
        serde_json::to_vec(value).expect("json")
    }
    let whole = reply(None);
    assert_eq!(whole.venues.len(), 3);
    assert_eq!(whole.splits.len(), 2);
    for named in [&["binance"][..], &["okx", "dukascopy"], &["deribit"], &[]] {
        let named: Vec<String> = named.iter().map(|v| (*v).to_string()).collect();
        let part = reply(Some(named.as_slice()));
        for venue in vike_model::VENUES {
            let of = |r: &InstrumentListsReply| {
                let list = r.venues.iter().find(|l| l.venue == *venue).map(json);
                let splits: Vec<Vec<u8>> =
                    r.splits.iter().filter(|s| s.venue == *venue).map(json).collect();
                (list, splits)
            };
            let expected =
                if named.iter().any(|v| v == *venue) { of(&whole) } else { (None, Vec::new()) };
            assert_eq!(of(&part), expected, "{venue} asked as {named:?}");
        }
        assert_eq!(json(&part), json(&whole.clone().of_venues(&named)), "{named:?}");
    }
}

/// **A kept list an older build wrote is named ONCE, at the start** (deploy 2's first try, 2026-11-17: the 0290 build
/// started on lists with no stored names and refused every symbol): each row gets its user name from the refresh's own
/// naming, the operator's rows it carried stay when no store is read, the older file is kept beside it byte for byte,
/// the next start finds every name stored and writes nothing, and ig's file, which only IG's catalog names, is left to
/// its refresh untouched.
#[test]
fn an_older_builds_kept_list_is_named_once_at_the_start_and_kept_beside() {
    let dir = tempfile::tempdir().expect("temp");
    let hand = Instrument::new("binance", "ETHUSDT", "ETH", "USDT", Contract::CryptoSpot);
    let mut old = empty_list("binance");
    old.instruments = vec![instrument("binance", "BTCUSDT"), hand.clone()];
    old.added_by_operator = vec![hand.raw_symbol.clone()];
    let older_bytes = serde_json::to_vec(&old).expect("json");
    std::fs::write(kept_file(dir.path(), "binance"), &older_bytes).expect("the older build's file");
    let mut ig = empty_list("ig");
    ig.instruments = vec![instrument("ig", "CS.D.EURUSD.CFD.IP")];
    let ig_bytes = serde_json::to_vec(&ig).expect("json");
    std::fs::write(kept_file(dir.path(), "ig"), &ig_bytes).expect("ig's older file");

    assert_eq!(name_older_kept_lists(dir.path(), None), ["binance"], "ig is left to its refresh");
    let binance = read_kept_list(dir.path(), "binance").expect("binance");
    assert!(
        binance.instruments.iter().all(|row| row.naming.is_some()),
        "every row stores its naming"
    );
    let row = vike_catalog::kept_row_of_user_name(&binance.instruments, "binance", "BTCUSDT")
        .expect("named");
    assert_eq!(row.raw_symbol, "BTCUSDT");
    let carried = std::slice::from_ref(&hand.raw_symbol);
    assert_eq!(binance.added_by_operator, carried, "the store unread: its hand row stays");
    let mut older = kept_file(dir.path(), "binance").into_os_string();
    older.push(OLDER_FILE_SUFFIX);
    assert_eq!(std::fs::read(&older).expect("the older copy"), older_bytes, "kept byte for byte");
    assert_eq!(std::fs::read(kept_file(dir.path(), "ig")).expect("ig"), ig_bytes, "ig untouched");

    let after = std::fs::read(kept_file(dir.path(), "binance")).expect("binance");
    assert_eq!(
        name_older_kept_lists(dir.path(), None),
        Vec::<&str>::new(),
        "the next start names nothing"
    );
    assert_eq!(
        std::fs::read(kept_file(dir.path(), "binance")).expect("binance"),
        after,
        "and rewrites nothing"
    );
}

/// `venue`'s list of `rows` rows as a refresh at `at` keeps it (`SYM0`, `SYM1`, ...).
fn kept_of(venue: &str, rows: usize, at: i64) -> VenueInstrumentList {
    let listed = Ok(CatalogOutcome::Listed {
        instruments: (0..rows).map(|i| instrument(venue, &format!("SYM{i}"))).collect(),
        truncated: false,
        cached: false,
    });
    fold(venue, None, &listed, at).expect("a list")
}

/// The kept lists `Request::InstrumentLists` answers for `dir`.
fn answered(dir: &Path) -> Vec<VenueInstrumentList> {
    let read = instrument_lists_verb(Some(dir), None, None);
    let Response::InstrumentLists(reply) = read else { panic!("{read:?}") };
    reply.venues
}

/// `file`'s modification time put back to `time`, as `touch -d` or a restored copy leaves it.
fn put_back(file: &Path, time: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .and_then(|f| f.set_modified(time))
        .expect("a time");
}

/// **The parsed lists answer what a fresh read of the files answers** (perf audit item 8: every backtest run asks for
/// every list, ~14 MB of JSON on the CI box): the first ask parses the files, the next answers from memory, both equal to
/// `read_kept_lists`, every row, in roster order, and a file that does not parse is no list either way. With
/// `--nocapture` it prints what a fresh read costs against the parsed answer.
#[test]
fn the_parsed_lists_answer_what_a_fresh_read_answers() {
    let dir = tempfile::tempdir().expect("temp");
    write_list(dir.path(), &kept_of("okx", 3, T)).expect("okx");
    write_list(dir.path(), &kept_of("binance", 20_000, T)).expect("binance");
    std::fs::write(kept_file(dir.path(), "bybit"), b"not json")
        .expect("a file that does not parse");
    let started = std::time::Instant::now();
    let files = read_kept_lists(dir.path());
    let fresh = started.elapsed();
    assert_eq!(files.iter().map(|l| l.venue.as_str()).collect::<Vec<_>>(), ["binance", "okx"]);
    assert_eq!(answered(dir.path()), files, "the first ask parses the files");
    let started = std::time::Instant::now();
    let held = parsed_kept_lists(dir.path(), vike_catalog::KEPT_LIST_REREAD, None);
    let parsed = started.elapsed();
    assert_eq!(held, files, "the next answers from memory: the same lists, rows and order");
    eprintln!("20003 kept rows: a fresh read {fresh:?}, the parsed lists {parsed:?}");
}

/// **A write drops its venue's parsed list**, even one that leaves the file's length and time as they were (the time
/// is put back here, so only `write_list`'s drop can tell the lists apart), and a refresh, the operator's own rows and
/// the start's naming all write through it.
///
/// MUTATION PROOF: take the `PARSED` drop out of `write_list`; the second ask then answers the first list.
#[test]
fn a_write_drops_its_venues_parsed_list() {
    let dir = tempfile::tempdir().expect("temp");
    write_list(dir.path(), &kept_of("binance", 2, T)).expect("first");
    let file = kept_file(dir.path(), "binance");
    let before = std::fs::metadata(&file).expect("the file");
    assert_eq!(answered(dir.path())[0].refreshed_ms, Some(T), "parsed now");

    write_list(dir.path(), &kept_of("binance", 2, T + 1)).expect("over it");
    put_back(&file, before.modified().expect("a time"));
    assert_eq!(std::fs::metadata(&file).expect("the file").len(), before.len(), "the same length");
    let now = answered(dir.path());
    assert_eq!(now[0].refreshed_ms, Some(T + 1), "the write is answered");
    assert_eq!(now, read_kept_lists(dir.path()));

    let lane = lane(vec![scripted("binance", vec![Ok(5)], Arc::new(AtomicUsize::new(0)))]);
    assert!(refresh_venue(dir.path(), None, Some(&lane), "binance", T + 2).outcome.is_ok());
    assert_eq!(answered(dir.path())[0].instruments.len(), 5, "and so is a refresh");
}

/// **An operator's hand edit of a kept file is answered at the next ask** (an emergency edit, as on the CI box on
/// 2026-11-18): an edit changes the file's length or its modification time. One that keeps both (a same-length file
/// whose old time is put back) is answered once the parsed list is `vike_catalog::KEPT_LIST_REREAD` old (a minute),
/// and not before: the one bound on a hand edit.
#[test]
fn a_hand_edit_is_answered_at_the_next_ask_or_within_the_reread() {
    let dir = tempfile::tempdir().expect("temp");
    write_list(dir.path(), &kept_of("binance", 2, T)).expect("kept");
    let file = kept_file(dir.path(), "binance");
    assert_eq!(answered(dir.path())[0].instruments.len(), 2, "parsed now");

    let edited = named(kept_of("binance", 3, T));
    std::fs::write(&file, serde_json::to_vec(&edited).expect("json")).expect("a hand edit");
    assert_eq!(answered(dir.path()), std::slice::from_ref(&edited), "answered at the next ask");

    let time = std::fs::metadata(&file).and_then(|m| m.modified()).expect("a time");
    let same_length = named(kept_of("binance", 3, T + 1));
    std::fs::write(&file, serde_json::to_vec(&same_length).expect("json")).expect("an edit");
    put_back(&file, time);
    assert_eq!(
        parsed_kept_lists(dir.path(), Duration::MAX, None),
        [edited],
        "not before the reread"
    );
    assert_eq!(
        parsed_kept_lists(dir.path(), Duration::ZERO, None),
        [same_length],
        "once it is that old"
    );
}

/// `Request::KeptRowsOfVenues`' answer of `venues` to a reader holding `known`, decoded.
fn stamped(
    dir: &Path,
    store: &vike_secrets::CredentialStore,
    venues: &[&str],
    known: &BTreeMap<String, String>,
) -> Vec<vike_datahub_client::proto::KeptRowsOfVenue> {
    let venues: Vec<String> = venues.iter().map(|v| (*v).to_string()).collect();
    let body = kept_rows_verb(Some(dir), Some(store), &venues, known).expect("answered");
    vike_datahub_client::proto::decode_kept_rows(&body).expect("a kept-rows body")
}

/// **The stamp moves with every writer of what a venue is answered with, and nothing else moves it**: a refresh, the
/// operator's rows, a share split written past this daemon (`vike-cli data splits add` writes the settings store), a
/// hand edit and the start's naming of an older build's file each give binance a stamp never seen before; a reader
/// holding the stamps is answered "unchanged", stamp alone; a write of the same rows (a refresh that listed nothing
/// new, at a later time) keeps the stamp; and the rows a changed answer carries are the kept file's, as msgpack.
///
/// MUTATION PROOF: hash the rows alone (drop the splits from the stamp); the split step then keeps the stamp.
#[test]
fn the_stamp_moves_with_every_writer_and_a_held_stamp_is_answered_unchanged() {
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings.path())),
        vike_secrets::CredentialScope::Full,
    );
    write_list(dir.path(), &kept_of("binance", 3, T)).expect("a list");
    let none = BTreeMap::new();

    let first = stamped(dir.path(), &store, &["binance", "okx", "binance"], &none);
    let venues: Vec<&str> = first.iter().map(|v| v.venue.as_str()).collect();
    assert_eq!(venues, ["binance", "okx"], "each venue once, in the order asked");
    assert!(first.iter().all(|v| v.changed.is_some()), "nothing held: everything is sent");
    let rows = |answer: &[vike_datahub_client::proto::KeptRowsOfVenue]| {
        answer[0].changed.as_ref().expect("rows").rows.clone()
    };
    let kept = read_kept_list(dir.path(), "binance").expect("kept");
    assert_eq!(rows(&first), vike_datahub_client::proto::kept_msgpack(&kept.instruments).unwrap());
    let known: BTreeMap<String, String> =
        first.iter().map(|v| (v.venue.clone(), v.stamp.clone())).collect();
    let again = stamped(dir.path(), &store, &["binance", "okx"], &known);
    assert!(again.iter().all(|v| v.changed.is_none()), "held stamps: nothing but the stamps");
    assert_eq!(
        again.iter().map(|v| &v.stamp).collect::<Vec<_>>(),
        first.iter().map(|v| &v.stamp).collect::<Vec<_>>()
    );

    write_list(dir.path(), &kept_of("binance", 3, T + 9)).expect("the same rows, later");
    assert_eq!(stamped(dir.path(), &store, &["binance"], &none)[0].stamp, known["binance"]);

    let mut seen = vec![known["binance"].clone()];
    let mut moved = |writer: &str| {
        let now = stamped(dir.path(), &store, &["binance"], &none).remove(0);
        assert!(!seen.contains(&now.stamp), "{writer} moves the stamp");
        let held = BTreeMap::from([("binance".to_string(), now.stamp.clone())]);
        assert!(stamped(dir.path(), &store, &["binance"], &held)[0].changed.is_none(), "{writer}");
        seen.push(now.stamp.clone());
        now
    };

    let lane = lane(vec![scripted("binance", vec![Ok(5)], Arc::new(AtomicUsize::new(0)))]);
    let refreshed = refresh_venue(dir.path(), Some(settings.path()), Some(&lane), "binance", T + 1);
    assert!(refreshed.outcome.is_ok(), "{refreshed:?}");
    let after = moved("a refresh");
    let kept = read_kept_list(dir.path(), "binance").expect("refreshed");
    assert_eq!(kept.instruments.len(), 5);
    let new_rows = after.changed.expect("new rows").rows;
    assert_eq!(new_rows, vike_datahub_client::proto::kept_msgpack(&kept.instruments).unwrap());

    let hand = vike_secrets::operator_instrument::OperatorInstrument {
        asked: "HANDMADE".into(),
        added: "2026-11-19".into(),
        instrument: instrument("binance", "HANDMADE"),
    };
    vike_secrets::operator_instrument::write_operator_instrument(settings.path(), &hand)
        .expect("row");
    keep_operator_rows(dir.path(), settings.path(), "binance").expect("folded");
    moved("an operator's row");

    let split = vike_model::StockSplit::new("binance", "SYM0", "2020-08-31", 4.0).expect("split");
    vike_secrets::stock_split::write_stock_split(settings.path(), &split).expect("row");
    moved("a share split");

    let edited = named(kept_of("binance", 7, T + 2));
    std::fs::write(kept_file(dir.path(), "binance"), serde_json::to_vec(&edited).expect("json"))
        .expect("a hand edit");
    moved("a hand edit");

    let mut older = kept_of("binance", 2, T + 3);
    older.instruments.iter_mut().for_each(|row| row.naming = None);
    std::fs::write(kept_file(dir.path(), "binance"), serde_json::to_vec(&older).expect("json"))
        .expect("an older build's file");
    moved("an older build's file");
    assert_eq!(name_older_kept_lists(dir.path(), Some(settings.path())), ["binance"]);
    moved("the start's naming");
}

/// **The stamped verb refuses as the JSON verb does**: no project folder is the same words, never an empty answer.
#[test]
fn the_stamped_verb_refuses_with_no_project_in_the_json_verbs_words() {
    let why = kept_rows_verb(None, None, &["binance".to_string()], &BTreeMap::new())
        .expect_err("no project");
    assert_eq!(why, NO_PROJECT);
    let json = instrument_lists_verb(None, None, None);
    assert!(matches!(&json, Response::Error(w) if w == NO_PROJECT), "{json:?}");
}

/// What the reply writes in msgpack with no rows held, off FRESH reads of the files (`read_kept_lists`): the cache
/// off.
fn fresh_bytes(
    dir: &Path,
    store: Option<&vike_secrets::CredentialStore>,
    venues: Option<&[String]>,
) -> Vec<u8> {
    let mut reply = settings_rows(store).expect("the settings rows");
    reply.venues = read_kept_lists(dir);
    let reply = match venues {
        Some(named) => reply.of_venues(named),
        None => reply,
    };
    msgpack_of(&Response::InstrumentLists(reply)).expect("msgpack")
}

/// `venue`'s rows as [`PARSED`] holds them written, under `dir`.
fn held_rows(dir: &Path, venue: &str) -> Option<Arc<EncodedRows>> {
    let parsed = PARSED.lock().unwrap_or_else(PoisonError::into_inner);
    parsed.get(&(dir.to_path_buf(), venue.to_string())).and_then(|held| held.rows.clone())
}

/// **The route's bytes are the value's msgpack, byte for byte, and each venue's rows are written once per parse of its
/// file** (`instrument_lists_msgpack`, the rows `kept_rows_verb` answers too): every way of asking (every venue, some, one with no list, none) answers what the value
/// writes and what fresh reads of the files write; asked again, no list is written again. A share split and an
/// operator's list are answered at the next ask and write no kept list again; a refresh, an operator's row and a hand
/// edit of a file each write that venue's list again.
#[test]
fn the_reply_bytes_are_the_values_and_each_list_is_written_once_per_parse() {
    use vike_secrets::operator_instrument::{OperatorInstrument, write_operator_instrument};
    use vike_secrets::operator_instrument_list::{
        OperatorInstrumentList, write_operator_instrument_list,
    };
    use vike_secrets::stock_split::write_stock_split;

    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings.path())),
        vike_secrets::CredentialScope::Full,
    );
    for (venue, rows) in [("okx", 3), ("binance", 5), ("dukascopy", 4)] {
        write_list(dir.path(), &kept_of(venue, rows, T)).expect("a list");
    }
    let split = vike_model::StockSplit::new("binance", "SYM0", "2020-08-31", 4.0).expect("a split");
    write_stock_split(settings.path(), &split).expect("row");
    let asked: [Option<&[&str]>; 5] =
        [None, Some(&["binance"]), Some(&["okx", "dukascopy"]), Some(&["deribit"]), Some(&[])];
    let check = |after: &str| {
        for named in asked {
            let named: Option<Vec<String>> =
                named.map(|n| n.iter().map(|v| (*v).to_string()).collect());
            let named = named.as_deref();
            let body =
                instrument_lists_msgpack(Some(dir.path()), Some(&store), named).expect("bytes");
            assert!(body == fresh_bytes(dir.path(), Some(&store), named), "{after}, {named:?}");
            let value = instrument_lists_verb(Some(dir.path()), Some(&store), named);
            assert!(body == msgpack_of(&value).expect("msgpack"), "{after}, {named:?}");
        }
    };
    let held = |venue: &str| held_rows(dir.path(), venue).expect("written");

    check("the first ask");
    let binance = held("binance");
    check("asked again");
    assert!(Arc::ptr_eq(&binance, &held("binance")), "asked again: not written again");

    write_stock_split(
        settings.path(),
        &vike_model::StockSplit::new("okx", "SYM1", "2021-01-04", 2.0).expect("a split"),
    )
    .expect("row");
    let list = OperatorInstrumentList {
        venue: "binance".into(),
        fetched: "2026-11-19".into(),
        qualifier: "demo".into(),
        instruments: vec![instrument("binance", "OWN")],
    };
    write_operator_instrument_list(settings.path(), &list).expect("an operator's list");
    check("a split and an operator's list");
    assert!(Arc::ptr_eq(&binance, &held("binance")), "neither writes a kept list again");

    let lane = lane(vec![scripted("binance", vec![Ok(7)], Arc::new(AtomicUsize::new(0)))]);
    let refreshed = refresh_venue(dir.path(), Some(settings.path()), Some(&lane), "binance", T + 1);
    assert!(refreshed.outcome.is_ok(), "{refreshed:?}");
    check("a refresh");
    let after_refresh = held("binance");
    assert!(!Arc::ptr_eq(&binance, &after_refresh), "a refresh writes its list again");

    let row = OperatorInstrument {
        asked: "HAND".into(),
        added: "2026-11-19".into(),
        instrument: instrument("binance", "HAND"),
    };
    write_operator_instrument(settings.path(), &row).expect("an operator's row");
    keep_operator_rows(dir.path(), settings.path(), "binance").expect("folded");
    check("an operator's row");
    assert!(!Arc::ptr_eq(&after_refresh, &held("binance")), "and so does an operator's row");

    let okx = held("okx");
    let edited = serde_json::to_vec(&named(kept_of("okx", 9, T))).expect("json");
    std::fs::write(kept_file(dir.path(), "okx"), edited).expect("a hand edit");
    check("a hand edit");
    assert!(!Arc::ptr_eq(&okx, &held("okx")), "and so does a hand edit");
}

/// **One venue's list through the parsed lists** (`parsed_kept_list`, what `SeriesGaps` and `StrategySymbolFacts`
/// read): what a fresh read answers, the same parse while its file holds, a write and a hand edit read again; a venue
/// with no file, or one off the roster, as a fresh read answers it.
#[test]
fn one_venues_list_is_read_through_the_parsed_lists() {
    let dir = tempfile::tempdir().expect("temp");
    write_list(dir.path(), &kept_of("dukascopy", 2, T)).expect("kept");
    let first = parsed_kept_list(dir.path(), "dukascopy").expect("kept");
    assert_eq!(Some(&*first), read_kept_list(dir.path(), "dukascopy").as_ref());
    let again = parsed_kept_list(dir.path(), "dukascopy").expect("kept");
    assert!(Arc::ptr_eq(&first, &again), "held: not parsed again");

    write_list(dir.path(), &kept_of("dukascopy", 3, T + 1)).expect("a write");
    let written = parsed_kept_list(dir.path(), "dukascopy").expect("kept");
    assert_eq!(written.instruments.len(), 3, "a write is read again");
    let edited = serde_json::to_vec(&named(kept_of("dukascopy", 5, T))).expect("json");
    std::fs::write(kept_file(dir.path(), "dukascopy"), edited).expect("a hand edit");
    let edited = parsed_kept_list(dir.path(), "dukascopy").expect("kept");
    assert_eq!(edited.instruments.len(), 5, "a hand edit is read again");

    assert!(parsed_kept_list(dir.path(), "oanda").is_none(), "no file, no list");
    let off_roster = serde_json::to_vec(&named(kept_of("nowhere", 1, T))).expect("json");
    std::fs::write(kept_file(dir.path(), "nowhere"), off_roster).expect("a file");
    assert_eq!(
        parsed_kept_list(dir.path(), "nowhere").as_deref(),
        read_kept_list(dir.path(), "nowhere").as_ref(),
        "a venue off the roster reads as a fresh read reads it"
    );
}

/// What one `InstrumentListsOfVenues` of binance costs the datahub, its 2,211 rows as the CI box keeps them (2026-11-19),
/// as the value the route wrote before (the lists copied out of `PARSED`, then written) against the held bytes; and
/// what one `SeriesGaps` probe's read of a dukascopy list of 1,100 rows costs, the file parsed against the parsed list.
/// `#[ignore]`d, a measurement. Run:
/// `cargo test -p vike-datahub --release --lib instrument_list_reply_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn instrument_list_reply_cost() {
    let dir = tempfile::tempdir().expect("temp");
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings.path())),
        vike_secrets::CredentialScope::Full,
    );
    let store = Some(&store);
    write_list(dir.path(), &kept_of("binance", 2_211, T)).expect("a list");
    let binance = ["binance".to_string()];
    let best = |ask: &dyn Fn() -> usize| {
        (0..20)
            .map(|_| {
                let started = std::time::Instant::now();
                assert!(ask() > 0);
                started.elapsed()
            })
            .min()
            .expect("asked")
    };
    let value = || {
        let answer = instrument_lists_verb(Some(dir.path()), store, Some(&binance));
        msgpack_of(&answer).expect("msgpack").len()
    };
    let body =
        || instrument_lists_msgpack(Some(dir.path()), store, Some(&binance)).expect("bytes").len();
    assert_eq!(value(), body());
    let (before, after) = (best(&value), best(&body));
    eprintln!("binance's {} bytes: the value written {before:?}, the held bytes {after:?}", body());

    write_list(dir.path(), &kept_of("dukascopy", 1_100, T)).expect("a list");
    let file = || read_kept_list(dir.path(), "dukascopy").expect("kept").instruments.len();
    let parsed = || parsed_kept_list(dir.path(), "dukascopy").expect("kept").instruments.len();
    let bytes = std::fs::metadata(kept_file(dir.path(), "dukascopy")).expect("the file").len();
    let (before, after) = (best(&file), best(&parsed));
    eprintln!(
        "dukascopy's {bytes}-byte list per probe: the file parsed {before:?}, the parsed list {after:?}"
    );
}
