//! Half 1: no range verb ever reads the range; no `limit` over the ceiling is refused by name.

use super::budget_store::BudgetOnlyStore;
use super::*;

// ------------------------------------------------------------------------------------------------
// Half 1 — no verb reads the range
// ------------------------------------------------------------------------------------------------

/// A `limit` within the ceiling asks the store for exactly that many rows and answers EXACTLY the
/// frame the old handler sent — over limits on a group boundary, inside a straddling group, and
/// inside the FIRST group of a range.
#[test]
fn a_limited_request_reads_a_budget_and_answers_the_old_frame() {
    for kind in RANGED {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        let mut expected_asks = Vec::new();
        for range in ranges() {
            for limit in 1..=CEILING as u32 {
                let got =
                    wire.ask(&kind.request(range, Some(limit))).expect("a limited scan answers");
                let old = old_reply(&in_range(&rows, range), Some(limit));
                assert_eq!(
                    got,
                    frame_of(&kind.response(&old, 1)),
                    "{kind:?}, range {range:?}, limit {limit}: byte-identical to the old frame"
                );
                expected_asks.push((kind, range, limit as usize));
            }
        }
        assert_eq!(
            store.asked(),
            expected_asks,
            "{kind:?}: each request reads its own limit, once"
        );
        assert_eq!(store.loads(), 0, "{kind:?}: nothing read a range");
    }
}

/// A `limit` above the ceiling is CLAMPED to it, `u32::MAX` included.
#[test]
fn a_limit_above_the_ceiling_is_clamped_to_it() {
    for kind in RANGED {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        for limit in [CEILING as u32 + 1, 25, u32::MAX] {
            let got = wire.ask(&kind.request(TsRange::all(), Some(limit))).expect("answers");
            assert_eq!(
                got,
                frame_of(&kind.response(&old_reply(&rows, Some(CEILING as u32)), 1)),
                "{kind:?}, limit {limit}: answers what a limit of the ceiling answers"
            );
        }
        assert_eq!(store.asked(), vec![(kind, TsRange::all(), CEILING); 3], "{kind:?}");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// No `limit` — and `Some(0)`, which the wire has always read as "no cap" — over a range holding no
/// more than the ceiling answers the WHOLE range, byte-identical to the old frame, from a read of
/// `ceiling + 1`. `ScanExecFills` has no range: its series holds exactly the ceiling.
#[test]
fn a_request_without_a_limit_within_the_ceiling_is_the_old_whole_frame() {
    let rows = planted();
    // 1_000 ..= 7_000 is 1 + 3 + 1 + 1 + 2 + 1 + 1 rows: EXACTLY the ceiling.
    let at_ceiling = TsRange::of(1_000, 7_000);
    assert_eq!(in_range(&rows, at_ceiling).len(), CEILING, "the fixture sits AT the ceiling");
    for kind in RANGED {
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        for range in [TsRange::of(4_000, 9_000), at_ceiling, TsRange::of(50_000, 60_000)] {
            for limit in [None, Some(0)] {
                let got = wire.ask(&kind.request(range, limit)).expect("answers");
                assert_eq!(
                    got,
                    frame_of(&kind.response(&in_range(&rows, range), 1)),
                    "{kind:?}, range {range:?}, limit {limit:?}: the whole range, byte-identical"
                );
            }
        }
        assert!(
            store.asked().iter().all(|(_, _, n)| *n == CEILING + 1),
            "{kind:?}: a no-limit request reads ceiling + 1 and no more: {:?}",
            store.asked()
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }

    let series = in_range(&rows, at_ceiling);
    let store = Arc::new(BudgetOnlyStore::new(series.clone(), 1));
    let mut wire = Wire::open(spawn(store.clone()));
    let got = wire.ask(&Kind::ExecFills.request(TsRange::all(), None)).expect("answers");
    assert_eq!(got, frame_of(&Kind::ExecFills.response(&series, 1)), "the whole series");
    assert_eq!(store.asked(), vec![(Kind::ExecFills, TsRange::all(), CEILING + 1)]);
    assert_eq!(store.loads(), 0);
}

/// No `limit` over MORE than the ceiling is refused BY NAME — not read, not clamped in silence, not
/// sent to fail at `write_frame` — and the connection stays positional: the next request on it is
/// answered.
#[test]
fn a_request_without_a_limit_over_the_ceiling_is_refused_by_name_and_the_connection_survives() {
    for kind in ALL {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        let limits: &[Option<u32>] =
            if kind == Kind::ExecFills { &[None] } else { &[None, Some(0)] };
        for &limit in limits {
            let got = wire.ask(&kind.request(TsRange::all(), limit)).expect("a refusal is a reply");
            let reply: Response = serde_json::from_slice(&got).expect("a Response");
            let Response::Error(why) = &reply else {
                panic!(
                    "{kind:?}, limit {limit:?}: 26 rows over a ceiling of {CEILING} must be REFUSED: {reply:?}"
                )
            };
            let (unit, name) = kind.unit_and_ceiling();
            assert!(
                why.contains(&format!("more than {CEILING} {unit}")),
                "{kind:?} names the ceiling: {why}"
            );
            assert!(why.contains(name), "{kind:?} names the constant {name}: {why}");
            assert!(why.contains(&format!("{VENUE}:{SYMBOL}")), "{kind:?} names the series: {why}");
            assert!(why.contains("`limit`"), "{kind:?} names the limit: {why}");

            // ...and the SAME connection answers the next request.
            assert!(
                matches!(wire.ping(), Ok(Response::Pong)),
                "{kind:?}: a Ping after it is answered"
            );
            if kind != Kind::ExecFills {
                let next = wire.ask(&kind.request(TsRange::all(), Some(4))).expect("answers");
                assert_eq!(
                    next,
                    frame_of(&kind.response(&old_reply(&rows, Some(4)), 1)),
                    "{kind:?}"
                );
            }
        }
        assert!(
            store.asked().iter().all(|(_, _, n)| *n == CEILING + 1 || *n == 4),
            "{kind:?}: the refusal is decided from a read of ceiling + 1: {:?}",
            store.asked()
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// The book ceiling is in the STORE's unit — a stored row per level — so a range of few events but
/// many levels is refused, and the same events one level shallower are answered. The read is still
/// `ceiling + 1`, of levels.
#[test]
fn the_book_ceiling_counts_stored_levels_not_events() {
    for kind in [Kind::Book, Kind::Depth] {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 3));
        let mut wire = Wire::open(spawn(store.clone()));

        // 4_000 ..= 6_000: four events, twelve levels — under the ceiling in events, over it in rows.
        let over = TsRange::of(4_000, 6_000);
        assert_eq!(in_range(&rows, over).len(), 4);
        let reply: Response =
            serde_json::from_slice(&wire.ask(&kind.request(over, None)).expect("answers")).unwrap();
        assert!(
            matches!(&reply, Response::Error(why) if why.contains("book levels")),
            "{kind:?}: 4 events of 3 levels are 12 stored rows, over {CEILING}: {reply:?}"
        );
        // 4_000 ..= 5_000: three events, nine levels — answered whole.
        let under = TsRange::of(4_000, 5_000);
        let got = wire.ask(&kind.request(under, None)).expect("answers");
        assert_eq!(got, frame_of(&kind.response(&in_range(&rows, under), 3)), "{kind:?}");
        assert!(store.asked().iter().all(|(_, _, n)| *n == CEILING + 1), "{kind:?}");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// The entries a daemon actually runs pass [`ReadCeilings::PRODUCTION`] — an injected test ceiling
/// that production never received would leave every test above green and the real verbs
/// unbounded. The double holds 26 rows, so this asks the production numbers without planting them.
#[test]
fn the_production_entries_serve_the_production_ceilings() {
    for kind in ALL {
        let store = Arc::new(BudgetOnlyStore::new(planted(), 1));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let served: Arc<dyn HistStore + Send + Sync> = store.clone();
        thread::spawn(move || {
            let _ = vike_datahub::serve(listener, served);
        });
        let mut wire = Wire::open(addr);
        let whole = wire.ask(&kind.request(TsRange::all(), None)).expect("answers");
        assert_eq!(whole, frame_of(&kind.response(&planted(), 1)), "{kind:?}: 26 rows fit");
        let c = kind.production_ceiling();
        let mut expected = vec![(kind, TsRange::all(), c + 1)];
        if kind != Kind::ExecFills {
            wire.ask(&kind.request(TsRange::all(), Some(u32::MAX))).expect("answers");
            expected.push((kind, TsRange::all(), c));
        }
        assert_eq!(
            store.asked(),
            expected,
            "{kind:?}: ceiling + 1 for no limit, a huge limit clamped"
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}
