//! Half 4: a row ceiling is not a frame, so the reply BYTES are cut or refused by name too.

use super::budget_store::{BudgetOnlyStore, head_of};
use super::*;

// ------------------------------------------------------------------------------------------------
// Half 4 — the bytes
// ------------------------------------------------------------------------------------------------

/// Every verb the byte cap guards that carries a `limit`: the seven scans and `LoadBars`.
const BYTE_RANGED: [Kind; 8] = [
    Kind::Bars,
    Kind::Quotes,
    Kind::Trades,
    Kind::Book,
    Kind::Depth,
    Kind::Cohort,
    Kind::Perp,
    Kind::Equity,
];

/// Book events this many levels deep are the FAT rows here: one event's JSON is several levels
/// wide, while four of them (eight stored rows) still sit under the injected row ceiling.
const FAT_LEVELS: usize = 2;

fn levels_of(kind: Kind) -> usize {
    match kind {
        Kind::Book | Kind::Depth => FAT_LEVELS,
        _ => 1,
    }
}

/// The length of `kind`'s reply carrying `rows` — `write_frame`'s own body.
fn reply_len(kind: Kind, rows: &[Planted]) -> usize {
    frame_of(&kind.response(rows, levels_of(kind))).len()
}

/// The page the ROW caps alone give `limit` over the whole series: the double's head of
/// `min(limit, CEILING)`, cut by the wire's whole-`ts` rule — what the server sent before the byte
/// cap existed.
fn row_capped_page(kind: Kind, rows: &[Planted], limit: u32) -> Vec<Planted> {
    let n = (limit as usize).min(CEILING);
    old_reply(&head_of(rows, n, levels_of(kind)), Some(n as u32))
}

/// The oracle for the byte cap: the longest whole-`ts` prefix of `page` whose reply fits `frame`,
/// found by serialising each candidate page WHOLE — never by summing row lengths, which is what the
/// server does, so a defect in its arithmetic cannot be repeated here. `None` when not even the
/// first timestamp's rows fit.
fn longest_page_that_fits(kind: Kind, page: &[Planted], frame: usize) -> Option<Vec<Planted>> {
    let mut best = None;
    for end in 1..=page.len() {
        let whole_ts = end == page.len() || page[end].0 != page[end - 1].0;
        if whole_ts && reply_len(kind, &page[..end]) <= frame {
            best = Some(page[..end].to_vec());
        }
    }
    best
}

/// A server over the planted series (book events [`FAT_LEVELS`] deep) under a frame of `frame`.
fn spawn_framed(kind: Kind, rows: Vec<Planted>, frame: usize) -> (Arc<BudgetOnlyStore>, Wire) {
    let store = Arc::new(BudgetOnlyStore::new(rows, levels_of(kind)));
    let wire = Wire::open(spawn_with(store.clone(), injected_with_frame(frame)));
    (store, wire)
}

/// The window the no-`limit` byte tests read: four rows, so eight stored book rows — under the
/// injected row ceiling for every kind, so only the FRAME can refuse it.
fn no_limit_window() -> TsRange {
    TsRange::of(4_000, 6_000)
}

/// A reply that fits the frame is UNTOUCHED — with a `limit`, and with none — even when it fits
/// with not one byte to spare.
#[test]
fn a_reply_that_fits_the_frame_is_untouched() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let page = row_capped_page(kind, &rows, CEILING as u32);
        let (store, mut wire) = spawn_framed(kind, rows.clone(), reply_len(kind, &page));
        let got = wire.ask(&kind.request(TsRange::all(), Some(CEILING as u32))).expect("answers");
        assert_eq!(got, frame_of(&kind.response(&page, levels_of(kind))), "{kind:?}, with a limit");

        let whole = in_range(&rows, no_limit_window());
        let (_, mut wire) = spawn_framed(kind, rows.clone(), reply_len(kind, &whole));
        let got = wire.ask(&kind.request(no_limit_window(), None)).expect("answers");
        assert_eq!(got, frame_of(&kind.response(&whole, levels_of(kind))), "{kind:?}, no limit");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
    let series = in_range(&planted(), TsRange::of(1_000, 7_000));
    let (_, mut wire) =
        spawn_framed(Kind::ExecFills, series.clone(), reply_len(Kind::ExecFills, &series));
    let got = wire.ask(&Kind::ExecFills.request(TsRange::all(), None)).expect("answers");
    assert_eq!(got, frame_of(&Kind::ExecFills.response(&series, 1)), "the whole fill series");
}

/// A `limit`ed reply whose rows number no more than the ceiling but whose BYTES pass the frame is
/// cut to the longest whole-`ts` page that fits — shorter than the row caps alone would send, and
/// as legal as any soft-capped page: the pager continues from its last `ts`. Over frames that cut
/// one byte short of the whole page, exactly at a group boundary, one byte past it, and one byte
/// short of the next group — book events fat, so a tick kind is cut by bytes too.
#[test]
fn a_limited_reply_over_the_frame_is_cut_to_a_shorter_whole_ts_page() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let page = row_capped_page(kind, &rows, CEILING as u32);
        assert!(page.len() > 5, "{kind:?}: the page must have groups to cut: {page:?}");
        for frame in [
            reply_len(kind, &page) - 1,
            reply_len(kind, &page[..4]),
            reply_len(kind, &page[..4]) + 1,
            reply_len(kind, &page[..5]) - 1,
        ] {
            let expected = longest_page_that_fits(kind, &page, frame)
                .unwrap_or_else(|| panic!("{kind:?}, frame {frame}: the first ts fits"));
            assert!(
                !expected.is_empty() && expected.len() < page.len(),
                "{kind:?}, frame {frame}: the fixture must make the frame bite"
            );
            let (store, mut wire) = spawn_framed(kind, rows.clone(), frame);
            let got = wire
                .ask(&kind.request(TsRange::all(), Some(CEILING as u32)))
                .unwrap_or_else(|e| panic!("{kind:?}, frame {frame}: a reply, not a drop: {e}"));
            assert!(got.len() <= frame, "{kind:?}, frame {frame}: the reply fits the frame");
            assert_eq!(
                got,
                frame_of(&kind.response(&expected, levels_of(kind))),
                "{kind:?}, frame {frame}: the longest whole-ts page that fits ({} of {} rows)",
                expected.len(),
                page.len()
            );
            assert_eq!(store.asked(), vec![(kind, TsRange::all(), CEILING)], "{kind:?}");
            assert_eq!(store.loads(), 0, "{kind:?}");
        }
    }
}

/// With no `limit`, a reply under its row ceiling whose BYTES pass the frame is REFUSED BY NAME —
/// never cut in silence, never sent to fail at `write_frame` — and the connection stays: the same
/// window asked WITH a `limit` is then answered with the page that fits.
#[test]
fn a_reply_without_a_limit_over_the_frame_is_refused_by_name_and_the_connection_survives() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let whole = in_range(&rows, no_limit_window());
        let frame = reply_len(kind, &whole) - 1;
        let (store, mut wire) = spawn_framed(kind, rows.clone(), frame);
        let got = wire.ask(&kind.request(no_limit_window(), None)).expect("a refusal is a reply");
        let reply: Response = serde_json::from_slice(&got).expect("a Response");
        let Response::Error(why) = &reply else {
            panic!("{kind:?}: a reply one byte over the frame must be REFUSED: {reply:?}")
        };
        let (_, name) = kind.unit_and_ceiling();
        assert!(why.contains("MAX_FRAME_LEN"), "{kind:?} names the frame: {why}");
        assert!(why.contains(&format!("{frame} bytes")), "{kind:?} names its size: {why}");
        assert!(why.contains(name), "{kind:?} says it is under {name}: {why}");
        assert!(why.contains("`limit`"), "{kind:?} names the remedy: {why}");

        assert!(matches!(wire.ping(), Ok(Response::Pong)), "{kind:?}: the connection survives");
        let page = longest_page_that_fits(kind, &whole, frame).expect("the first ts fits");
        let next =
            wire.ask(&kind.request(no_limit_window(), Some(CEILING as u32))).expect("answers");
        assert_eq!(next, frame_of(&kind.response(&page, levels_of(kind))), "{kind:?}");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }

    let series = in_range(&planted(), TsRange::of(1_000, 7_000));
    let frame = reply_len(Kind::ExecFills, &series) - 1;
    let (_, mut wire) = spawn_framed(Kind::ExecFills, series, frame);
    let got = wire.ask(&Kind::ExecFills.request(TsRange::all(), None)).expect("a reply");
    let reply: Response = serde_json::from_slice(&got).expect("a Response");
    assert!(
        matches!(&reply, Response::Error(why) if why.contains("MAX_FRAME_LEN") && why.contains("SCAN_EXEC_FILLS_CEILING")),
        "a fill series one byte over the frame is refused by name: {reply:?}"
    );
    assert!(matches!(wire.ping(), Ok(Response::Pong)));
}

/// ⚠ The rows of the FIRST timestamp alone passing the frame is a NAMED ERROR — never an empty page,
/// which `RemoteHistStore`'s pager reads as the end of the range, losing the rest with no error.
#[test]
fn a_first_timestamp_too_wide_for_any_page_is_an_error_never_an_empty_page() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let from = TsRange { start: Some(2_000), end: None };
        let group = in_range(&rows, TsRange::of(2_000, 2_000));
        assert_eq!(group.len(), 3, "the fixture stores 2_000 three times");
        let frame = reply_len(kind, &group) - 1;
        let (store, mut wire) = spawn_framed(kind, rows.clone(), frame);

        let got = wire.ask(&kind.request(from, Some(CEILING as u32))).expect("a reply, not a drop");
        assert_ne!(
            got,
            frame_of(&kind.response(&[], 1)),
            "{kind:?}: an EMPTY page would end a paged read here in silence"
        );
        let reply: Response = serde_json::from_slice(&got).expect("a Response");
        let Response::Error(why) = &reply else {
            panic!("{kind:?}: a first ts wider than the frame must be a named error: {reply:?}")
        };
        assert!(why.contains("ts 2000"), "{kind:?} names the timestamp: {why}");
        assert!(why.contains("MAX_FRAME_LEN"), "{kind:?} names the frame: {why}");

        // ...the connection survives, and with no `limit` over that one timestamp it is refused too.
        assert!(matches!(wire.ping(), Ok(Response::Pong)), "{kind:?}");
        let got = wire.ask(&kind.request(TsRange::of(2_000, 2_000), None)).expect("a reply");
        let reply: Response = serde_json::from_slice(&got).expect("a Response");
        assert!(
            matches!(&reply, Response::Error(why) if why.contains("MAX_FRAME_LEN")),
            "{kind:?}: {reply:?}"
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// Production serves the frame `write_frame` enforces — an injected test frame that production
/// never received would leave every byte test above green and the real replies uncut.
#[test]
fn the_production_frame_is_the_wire_frame() {
    assert_eq!(
        ReadCeilings::PRODUCTION.frame_bytes,
        vike_datahub_client::proto::MAX_FRAME_LEN as usize,
        "the byte cap must cut to the frame write_frame refuses to pass"
    );
}
