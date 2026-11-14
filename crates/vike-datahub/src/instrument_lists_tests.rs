use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::catalog::{CatalogFn, CatalogTable};
use vike_model::{Contract, Instrument};

const T: i64 = 1_794_124_800_000;

fn instrument(venue: &str, symbol: &str) -> Instrument {
    Instrument::new(venue, symbol, "BTC", "USDT", Contract::CryptoSpot)
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

#[test]
fn a_list_is_due_after_seven_days_and_not_a_millisecond_before() {
    assert!(is_due(T, None), "a venue never fetched is due at once");
    let kept = fold("binance", None, &listed(1), T).expect("written");
    assert!(!is_due(T + CATALOG_REFRESH_PERIOD_MS - 1, Some(&kept)));
    assert!(is_due(T + CATALOG_REFRESH_PERIOD_MS, Some(&kept)));
    let failed_only = fold("binance", None, &Err("down".into()), T).expect("written");
    assert!(is_due(T + 1, Some(&failed_only)), "a list that never succeeded stays due");
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
fn the_weekly_pass_fetches_a_due_venue_and_leaves_a_fresh_one_alone() {
    let dir = tempfile::tempdir().expect("temp");
    let calls = Arc::new(AtomicUsize::new(0));
    let lane = lane(vec![scripted("binance", vec![Ok(2)], Arc::clone(&calls))]);

    assert_eq!(refresh_due(dir.path(), None, &lane, T).len(), 1, "never fetched: due");
    assert_eq!(read_kept_list(dir.path(), "binance").expect("kept").refreshed_ms, Some(T));
    assert!(
        refresh_due(dir.path(), None, &lane, T + CATALOG_REFRESH_PERIOD_MS - 1).is_empty(),
        "not before"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a fresh list asks the venue nothing");
    assert_eq!(
        refresh_due(dir.path(), None, &lane, T + CATALOG_REFRESH_PERIOD_MS).len(),
        1,
        "due after seven days"
    );
    assert_eq!(
        read_kept_list(dir.path(), "binance").expect("kept").refreshed_ms,
        Some(T + CATALOG_REFRESH_PERIOD_MS)
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
    assert!(is_due(T + 1, None), "and the venue stays due, so the next tick asks again");

    // Over a kept list too: it stands exactly as it was.
    let kept = fold("binance", None, &listed(2), T).expect("written");
    write_list(dir.path(), &kept).expect("write");
    refresh_venue(dir.path(), None, Some(&lane), "binance", T + 5);
    assert_eq!(read_kept_list(dir.path(), "binance"), Some(kept));
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
    let read = instrument_lists_verb(Some(dir.path()), None);
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
        let kept = fold(venue, None, &listed, T).expect("written");
        write_list(dir.path(), &kept).expect("kept");
        let calls = Arc::new(AtomicUsize::new(0));
        let closed = Err("FX market closed: login at 2026-11-15T21:00:00Z".to_string());
        let lane = lane(vec![scripted(venue, vec![closed], Arc::clone(&calls))]);
        // fxcm and dukascopy fetch (and fail); ctrader, oanda and ig are refused at the lane's gates (their catalog needs a
        // login, or has no bulk list): neither answer lists anything.
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
    let read = instrument_lists_verb(Some(dir.path()), None);
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
    assert_eq!(read_kept_list(dir.path(), "binance"), Some(second));
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
    );
}
