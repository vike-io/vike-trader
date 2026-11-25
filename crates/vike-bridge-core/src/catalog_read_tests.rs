//! The catalog read loop's pins: the tolerant read keeps every read that succeeded and says every
//! one that failed; the page walk reproduces each of today's walks (bybit's picker categories and
//! route listings, polymarket's Gamma offset browse and CLOB cursor walk) through its two knobs.

use super::*;

/// A walk with these knobs.
fn rules(max_pages: usize, strict: bool) -> CatalogPageWalk {
    CatalogPageWalk { max_pages, strict }
}

/// One page of `n` rows numbered from `start`, naming `next`.
fn page(start: usize, n: usize, next: Option<&str>) -> CatalogPage<String, usize> {
    CatalogPage { rows: (start..start + n).collect(), next: next.map(str::to_string) }
}

/// THE POINT of the tolerant shape (a `?` on one endpoint once wiped a venue from the picker): a
/// failed read drops nothing another read returned, the rows keep read order, and the failure
/// reaches the venue's line once, with the read it belongs to and its own error.
#[test]
fn a_failed_read_never_drops_another_reads_rows() {
    let mut said = Vec::new();
    let out = catalog_read_tolerant(
        ["spot", "perp", "option"],
        |read| match *read {
            "perp" => Err(format!("{read} HTTP 500")),
            other => Ok(vec![format!("{other}-1"), format!("{other}-2")]),
        },
        |read, e| said.push(format!("{read}: {e}")),
    );
    assert_eq!(out, ["spot-1", "spot-2", "option-1", "option-2"]);
    assert_eq!(said, ["perp: perp HTTP 500"]);
}

/// …and every read failing is an EMPTY list, never an error, with every failure said.
#[test]
fn every_read_failing_is_an_empty_list_and_every_failure_is_said() {
    let mut said = 0;
    let out = catalog_read_tolerant(1..=3, |_| Err::<Vec<u8>, _>("down"), |_, _| said += 1);
    assert!(out.is_empty());
    assert_eq!(said, 3);
}

/// A cursor walk follows each page's `next` to the venue's end, reading the first position first,
/// and keeps the rows in page order.
#[test]
fn a_cursor_walk_follows_each_next_to_the_end_of_the_listing() {
    let mut asked = Vec::new();
    let (rows, end) = rules(20, false)
        .walk(String::new(), |at| {
            asked.push(at.clone());
            Ok::<_, ()>(match at.as_str() {
                "" => page(0, 3, Some("c1")),
                "c1" => page(3, 3, Some("c2")),
                _ => page(6, 2, None),
            })
        })
        .unwrap();
    assert_eq!(rows, (0..8).collect::<Vec<_>>(), "3 + 3 + 2, in page order");
    assert_eq!(asked, ["", "c1", "c2"]);
    assert_eq!(end, CatalogPageWalkEnd::Listed);
}

/// The polymarket Gamma shape: the position is an offset the caller counts, and a short page names
/// no next one, so it ends the walk without another request.
#[test]
fn an_offset_walk_ends_on_the_short_page() {
    const LIMIT: usize = 4;
    let mut asked = Vec::new();
    let (rows, end) = rules(40, false)
        .walk(0usize, |&offset| {
            asked.push(offset);
            let n = if offset < 2 * LIMIT { LIMIT } else { 1 };
            let rows: Vec<usize> = (offset..offset + n).collect();
            let next = (rows.len() >= LIMIT).then_some(offset + LIMIT);
            Ok::<_, ()>(CatalogPage { rows, next })
        })
        .unwrap();
    assert_eq!(asked, [0, LIMIT, 2 * LIMIT]);
    assert_eq!(rows.len(), 2 * LIMIT + 1);
    assert_eq!(end, CatalogPageWalkEnd::Listed);
}

/// The first page failing is the listing being unreachable: an error, strict or not, and no
/// second request.
#[test]
fn the_first_page_failing_is_the_walks_error_in_both_modes() {
    for strict in [false, true] {
        let mut calls = 0;
        let got = rules(20, strict).walk(String::new(), |_| {
            calls += 1;
            Err::<CatalogPage<String, usize>, _>("down")
        });
        assert_eq!(got, Err("down"), "strict {strict}");
        assert_eq!(calls, 1, "strict {strict}");
    }
}

/// A later page failing: a tolerant walk keeps the pages before it and says which page failed with
/// which error (bybit's and polymarket's pickers warn from it); a strict walk answers the error.
#[test]
fn a_later_failure_keeps_the_rows_read_unless_the_walk_is_strict() {
    let fetch = |at: &String| match at.as_str() {
        "" => Ok(page(0, 5, Some("c1"))),
        _ => Err("mid-walk hiccup"),
    };
    assert_eq!(
        rules(20, false).walk(String::new(), fetch),
        Ok((
            (0..5).collect::<Vec<_>>(),
            CatalogPageWalkEnd::Failed { page: 1, error: "mid-walk hiccup" }
        ))
    );
    assert_eq!(rules(20, true).walk(String::new(), fetch), Err("mid-walk hiccup"));
}

/// EVERY walk, tolerant or strict (owner ruling C-D2): a venue echoing one cursor forever stops at
/// the page that repeats it (page 0 spends the cursor, page 1 hands it back), strictly before the
/// ceiling, with both pages' rows. A strict walk used to read the same cursor again until the
/// ceiling (bybit's route listings: 20 requests, then a refusal).
#[test]
fn a_repeated_position_stops_every_walk() {
    for strict in [false, true] {
        let mut calls = 0;
        let (rows, end) = rules(20, strict)
            .walk(String::new(), |_| {
                calls += 1;
                Ok::<_, ()>(page(0, 2, Some("stuck")))
            })
            .unwrap();
        assert_eq!(calls, 2, "strict {strict}");
        assert_eq!(rows.len(), 4, "strict {strict}");
        assert_eq!(end, CatalogPageWalkEnd::Repeated { page: 1 }, "strict {strict}");
    }
}

/// The ceiling bounds a listing whose cursor never repeats and never ends, after exactly
/// `max_pages` requests (no extra page is read to find out), while a listing that ends ON the last
/// allowed page is the venue's end, not the ceiling.
#[test]
fn the_ceiling_bounds_a_never_ending_listing_and_only_that() {
    let mut calls = 0;
    let (rows, end) = rules(5, false)
        .walk(String::new(), |_| {
            calls += 1;
            let fresh = format!("c{calls}");
            Ok::<_, ()>(page(calls, 1, Some(fresh.as_str())))
        })
        .unwrap();
    assert_eq!((calls, rows.len(), end), (5, 5, CatalogPageWalkEnd::Ceiling));

    let mut calls = 0;
    let (_, end) = rules(5, false)
        .walk(String::new(), |_| {
            calls += 1;
            Ok::<_, ()>(page(0, 1, (calls < 5).then(|| format!("c{calls}")).as_deref()))
        })
        .unwrap();
    assert_eq!((calls, end), (5, CatalogPageWalkEnd::Listed));
}

/// On the last allowed page the repeat check comes first, as bybit's picker walk ordered it: the end
/// says the cursor repeated, not that the ceiling was reached.
#[test]
fn on_the_last_page_a_repeat_is_said_before_the_ceiling() {
    let got = rules(2, false).walk(String::new(), |_| Ok::<_, ()>(page(0, 1, Some("stuck"))));
    assert_eq!(got, Ok((vec![0, 0], CatalogPageWalkEnd::Repeated { page: 1 })));
}

/// A walk allowed no page reads none.
#[test]
fn a_walk_of_no_pages_reads_nothing() {
    let got = rules(0, true).walk(String::new(), |_| -> Result<CatalogPage<String, u8>, ()> {
        panic!("no page may be read")
    });
    assert_eq!(got, Ok((Vec::new(), CatalogPageWalkEnd::Ceiling)));
}
