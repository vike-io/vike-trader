//! T16: `MdUpdate` on an already-live session — the second door into the registry, with its private helpers.

use super::*;
use vike_data::StreamStatus;

// ------------------------------------------------------------------------------------------------
// T16 — the SECOND door into the registry: `MdUpdate` on an already-live session
// ------------------------------------------------------------------------------------------------

/// Seed one depth key with a book AND a `Live` status, so an attach for it legitimately owes a
/// snapshot rather than a status alone. Without the status leg `attach_frames` returns status-only
/// by contract and the tests below would prove nothing about the book.
fn seed_live_depth(h: &MdHub, venue: &str, symbol: &str, levels: usize) {
    let sink = h.sink();
    let bids: Vec<BookLevel> = (0..levels).map(|i| BookLevel::new(100.0 - i as f64, 1.0)).collect();
    let asks: Vec<BookLevel> = (0..levels).map(|i| BookLevel::new(200.0 + i as f64, 1.0)).collect();
    sink.l2_snapshot(venue, symbol, 0.1, bids, asks, 7);
    sink.stream_status(venue, symbol, "depth", StreamStatus::Live { gap_started_ts_ms: None });
}

/// Where a `Status` naming exactly this key sits in a drained run, if it is there at all.
///
/// ⚠ Matched on the KEY FIELDS, never on the frame kind alone: this repository has been bitten by
/// an assertion that matched a substring of the wrong answer, and every test below runs on a hub
/// holding a SECOND key whose frames are the wrong answer in exactly that way.
fn status_at(frames: &[MdFrame], venue: &str, symbol: &str, lane: MdLane) -> Option<usize> {
    frames.iter().position(|f| match f {
        MdFrame::Status { venue: v, symbol: s, lane: l, .. } => {
            v.as_str() == venue && s.as_str() == symbol && *l == lane
        }
        _ => false,
    })
}

/// Where a `Depth` snapshot naming exactly this key sits in a drained run, if it is there at all.
fn depth_at(frames: &[MdFrame], venue: &str, symbol: &str) -> Option<usize> {
    frames.iter().position(|f| match f {
        MdFrame::Depth(bk) => bk.venue == venue && bk.symbol == symbol,
        _ => false,
    })
}

/// **A KEY ADDED TO AN ALREADY-LIVE SESSION IS ATTACHED IMMEDIATELY — it does not wait for that
/// venue's next update.**
///
/// There are TWO doors into the registry and until this landed only one of them attached.
/// `MdSubscribe` goes through `crate::server`'s `run_market_writer`, which pushes `attach_frames`
/// per accepted key before it enters its drain loop; `MdUpdate` — **which is the path every DOM
/// window after the first takes** (`crates/vike-app-core/src/data/md_session.rs`'s `push_update`, on a
/// fresh short-lived connection, because the stream socket has left its read loop) — went through
/// `MdHub::update`, which bumped the refcount and pushed nothing.
///
/// `publish_tick` skips any entry whose `dirty` bit is clear and `acquire` never sets it, so the
/// adding session received **no status and no snapshot until that venue's next update** — seconds
/// to minutes on a quiet polymarket instrument, and the whole of §12.5's reconnect state on binance
/// depth. Worse than blank: `MdSession::gapped` is populated only by a `Status` frame, so a key that
/// receives NOTHING is not gapped and the Connections tool counts it LIVE over an empty ladder —
/// precisely the failure that field's own doc says it exists to prevent, reached through a door
/// that doc did not know about.
///
/// ⚠ **Neither `publish_tick` nor the sink is touched after the update.** That is the whole
/// assertion: a fix that marks the key dirty instead of attaching it delivers at the next tick, in
/// the wrong ORDER (no `Status` — `publish_tick` emits one only when `status_dirty` is set), with no
/// `Live` gate on the book, and nothing at all when the key has no book yet.
///
/// Three non-vacuity floors, because "a frame arrived" is cheap to satisfy by accident:
/// 1. the ordinary path DID deliver first (`publish_tick` filled both mailboxes);
/// 2. both mailboxes were drained EMPTY going in, so nothing below can be a leftover;
/// 3. the frames are matched on this key's venue/symbol/lane, not on kind — session A is holding a
///    second key whose frames would satisfy a kind-only assertion.
///
/// Mutation: delete the attach push from `MdHub::update` -> red on the missing `Status`.
#[test]
fn a_key_added_to_a_live_session_attaches_without_waiting_for_a_venue_update() {
    let log = Log::default();
    let h = hub(&log);
    let one = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let two = spec("binance", "ETHUSDT.P", MdLane::Depth);

    let mut a = h.open_session().unwrap();
    let mut b = h.open_session().unwrap();
    h.acquire(a.id(), &one).expect("binance depth is servable");
    h.acquire(b.id(), &two).expect("binance depth is servable");
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 4);
    seed_live_depth(&h, "binance", "ETHUSDT.P", 4);
    h.publish_tick();

    // Floor 1 — the ordinary path works on this harness, so an absence below means something.
    assert!(
        !drain(a.mailbox()).is_empty(),
        "the FIRST window's key must flow on the ordinary path, or every assertion below is vacuous"
    );
    assert!(!drain(b.mailbox()).is_empty(), "...and so must the second session's");
    // Floor 2 — nothing is left over, so the frames drained after the update cannot be leftovers.
    assert!(a.mailbox().is_empty(), "session A's mailbox must be drained EMPTY going in");
    assert!(b.mailbox().is_empty(), "session B's mailbox must be drained EMPTY going in");

    // THE SECOND DOM WINDOW. No venue update follows it and `publish_tick` is NOT called again.
    let (accepted, refused, released) = h.update(a.id(), std::slice::from_ref(&two), &[], now());
    assert_eq!(accepted.len(), 1, "the add was accepted: {accepted:?} / refused {refused:?}");
    assert!(released.is_empty(), "{released:?}");

    let frames = drain(a.mailbox());
    let s = status_at(&frames, "binance", "ETHUSDT.P", MdLane::Depth).unwrap_or_else(|| {
        panic!(
            "the ADDING session received no Status for the key it just added — a DOM window after \
             the first paints a blank ladder while the Connections tool reads it as live: {frames:?}"
        )
    });
    let d = depth_at(&frames, "binance", "ETHUSDT.P").unwrap_or_else(|| {
        panic!("...and no snapshot either, though the hub holds a LIVE book for it: {frames:?}")
    });
    assert!(s < d, "§5.2 step 5: the STATUS comes first, then the book: {frames:?}");

    // ⚠ AND IT IS NOT A BROADCAST. Session B already holds this key and asked for nothing, so it
    // receives nothing — the §6.1 guard that separates this fix from marking the key dirty, which
    // would republish the book to every subscriber of it on the next tick.
    assert!(
        b.mailbox().is_empty(),
        "a session that already held the key was republished to: {:?}",
        drain(b.mailbox())
    );
    a.release_at(now());
    b.release_at(now());
}

/// **A DUPLICATE `add` OF A KEY THIS SESSION ALREADY HOLDS AT THE SAME DEPTH PUSHES NOTHING** — the
/// bound that keeps the attach above from being #1753 wearing a new producer.
///
/// `MdHub::acquire` returns `Ok` for a key the session already holds (`already_held_by_session`
/// only suppresses the refcount bump), so `update`'s add loop cannot tell a duplicate from a new
/// key by the result alone. An attach push per ACCEPTED spec therefore costs one CTRL frame per
/// spec SENT rather than per state CHANGE — and `vike_datahub::md::MD_MAILBOX_CTRL` is 128 while a
/// request may legally name 64. Three such requests back to back, with no stall, no slow link and
/// no venue event, set `must_close` and the peer gets `MdBye::ControlLaneOverflow` — which is
/// `docs/decisions/0052`'s decision 2 (*"bounded by server constants, never by the request"*)
/// false again on exactly the term #1753 restored.
///
/// The gate is that the push follows a CHANGE to this session's holding, not an acceptance.
///
/// ⚠ Non-vacuity: the session must genuinely hold a MULTI-key set that was genuinely delivered
/// first, or "nothing arrived" passes against a hub that attaches nothing at all.
#[test]
fn a_duplicate_add_of_a_held_key_pushes_nothing_and_cannot_overflow_the_control_lane() {
    use vike_datahub::md::{MD_MAILBOX_CTRL, MD_MAX_KEYS_PER_VENUE};

    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();
    let mut held = Vec::new();
    for (venue, lanes) in [
        ("binance", [MdLane::Depth, MdLane::Trades]),
        ("bybit", [MdLane::Depth, MdLane::Trades]),
        ("polymarket", [MdLane::Book, MdLane::Trades]),
    ] {
        for i in 0..8 {
            for lane in lanes {
                let s = spec(venue, &format!("SYM{i}"), lane);
                held.push(h.acquire(g.id(), &s).expect("a servable lane inside every cap"));
            }
        }
    }
    assert_eq!(held.len(), 3 * MD_MAX_KEYS_PER_VENUE as usize, "the harness must reach a big set");
    h.reconcile(now());

    // The floor: this session's keys DO deliver, so an empty mailbox below is a decision rather
    // than a broken harness.
    for i in 0..8 {
        seed_live_depth(&h, "binance", &format!("SYM{i}"), 2);
    }
    h.publish_tick();
    assert!(!drain(g.mailbox()).is_empty(), "the ordinary path delivered nothing — vacuous");
    assert!(g.mailbox().is_empty(), "drained empty going in");

    // Three full re-sends of the SAME already-held set. 3 x 48 = 144 CTRL frames against a lane of
    // 128 if every accepted spec were attached.
    assert!(3 * held.len() > MD_MAILBOX_CTRL, "the floor: the burst must EXCEED the ctrl lane");
    for round in 1..=3 {
        let (accepted, refused, released) = h.update(g.id(), &held, &[], now());
        assert_eq!(accepted.len(), held.len(), "round {round}: still accepted {refused:?}");
        assert!(released.is_empty(), "round {round}: {released:?}");
        assert!(
            !g.mailbox().must_close(),
            "round {round}: a re-send of the session's OWN held set closed its connection"
        );
        assert!(
            g.mailbox().is_empty(),
            "round {round}: a duplicate add pushed frames for keys nothing changed about: {:?}",
            drain(g.mailbox())
        );
    }
    g.release_at(now());
}

/// **A SESSION JOINING AN EXISTING KEY AT A LARGER DEPTH SEES THE DEEPER BOOK IMMEDIATELY** — the
/// depth half of the same defect, closed by the same push and with no `mark_dirty` anywhere.
///
/// `MdHub::acquire` raises `StreamEntry::depth` and settles it, and NEITHER marks the entry
/// dirty — so without an attach the joining session waits for that venue's next update before it
/// sees the cut it asked for. `attach_frames` cuts its snapshot at `effective_depth()`, which
/// `acquire` has already raised by the time the push runs, so the deeper frame rides the same
/// attach that fixes the blank ladder.
///
/// ⚠ **The residual is DECLARED, not closed, and it is asserted here rather than left implied**:
/// the key's OTHER subscribers keep receiving the previous cut until the next venue update. They
/// asked for LESS — depth is a per-key max — so a windfall arriving one tick late is not a loss,
/// and marking the entry dirty to deliver it would republish the key to every subscriber of it on
/// every window open and every window close, which is the §6.1 cost this whole fix is shaped to
/// avoid. `StreamEntry::settle_depth`'s own doc carries the argument.
#[test]
fn a_session_joining_an_existing_key_deeper_attaches_at_the_deeper_cut() {
    let log = Log::default();
    let h = hub(&log);
    let shallow = MdSpec { depth_levels: Some(5), ..spec("binance", "BTCUSDT.P", MdLane::Depth) };
    let deep = MdSpec { depth_levels: Some(60), ..shallow.clone() };

    let mut b = h.open_session().unwrap();
    h.acquire(b.id(), &shallow).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 100);
    h.publish_tick();

    // The floor: B's own frame is cut at FIVE, so 60 below is a different number arrived at by the
    // fold rather than the harness's default.
    let first = drain(b.mailbox());
    match first.iter().find(|f| matches!(f, MdFrame::Depth(_))) {
        Some(MdFrame::Depth(bk)) => assert_eq!(bk.bids.len(), 5, "B's own cut: {first:?}"),
        _ => panic!("B received no book on the ordinary path: {first:?}"),
    }
    assert!(b.mailbox().is_empty(), "drained empty going in");

    let mut a = h.open_session().unwrap();
    let (accepted, refused, _) = h.update(a.id(), std::slice::from_ref(&deep), &[], now());
    assert_eq!(accepted.len(), 1, "{accepted:?} / {refused:?}");

    let frames = drain(a.mailbox());
    let d = depth_at(&frames, "binance", "BTCUSDT.P")
        .unwrap_or_else(|| panic!("the deeper joiner received no snapshot at all: {frames:?}"));
    match &frames[d] {
        MdFrame::Depth(bk) => assert_eq!(
            bk.bids.len(),
            60,
            "the joiner was handed the PREVIOUS cut and must wait for a venue update for the depth \
             it asked for: {frames:?}"
        ),
        other => panic!("{other:?}"),
    }
    // The declared residual, pinned: the shallow holder is not republished to.
    assert!(
        b.mailbox().is_empty(),
        "the key's OTHER subscriber was republished to: {:?}",
        drain(b.mailbox())
    );
    a.release_at(now());
    b.release_at(now());
}

/// **A REMOVED KEY'S QUEUED BOOK LEAVES THE MAILBOX WITH IT.**
///
/// `vike_datahub::md::mailbox::Mailbox`'s BOOK lane is the one lane nothing can evict —
/// `enforce_bounds` evicts only from the tape, and its comment says why: *"`MD_MAILBOX_BYTES`'
/// compile-time assertion is what guarantees a full session of ceiling-depth books fits"*. That
/// assertion multiplies `MD_FRAME_CEILING_BYTES` by `MD_MAX_SPECS_PER_SESSION`, so it rests
/// entirely on the invariant `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` — and `update`'s
/// remove path broke it: a removed key's already-queued slot was never dropped, so a single
/// `remove 64 + add 64` left 128 unevictable slots, 1.86x the byte bound, with nothing able to
/// reclaim them. Attaching on update makes it reachable INSIDE one request rather than one tick
/// later, which is why it is closed here rather than declared.
///
/// It is also the right answer on its own terms: a book for a key the client has just unsubscribed
/// is routed into `crates/vike-app-core/src/data/md_session.rs`'s `BookStore` by `(venue, symbol)`
/// regardless of the served set, repopulating a store entry the window that wanted it just dropped.
///
/// ⚠ The TAPE is deliberately left alone — it is bounded and evictable, so it cannot break the
/// assertion, and the mailbox's owed gaps are a disclosure that must survive.
///
/// ⚠ **WHAT THIS TEST CAN AND CANNOT SEE.** It drives `update` and `publish_tick` sequentially on
/// one thread, so it proves the RECLAIM and nothing about the race beside it: reclaiming a slot is
/// worth nothing while a publish tick can enqueue a new one for the same key a moment later, and a
/// per-tick target snapshot says the session still holds the key for the whole remainder of that
/// tick. The other half is `MdHub::fanout`, which re-reads the session table under that table's own
/// lock at push time — a LOCK-ORDERING property, provable by reading the two call sites and not by
/// a single-threaded assertion, which is why it is stated on that function rather than pretended at
/// here. A timing test for it would flake on a loaded runner and, if it ever regressed, would hang
/// or pass at random rather than go red: the same reason `mailbox::PushOutcome` asserts
/// never-blocks as a TYPE property instead of with a stopwatch.
#[test]
fn a_removed_keys_queued_book_leaves_the_mailbox_with_it() {
    let log = Log::default();
    let h = hub(&log);
    let one = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let two = spec("binance", "ETHUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &one).unwrap();
    h.acquire(g.id(), &two).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 4);
    seed_live_depth(&h, "binance", "ETHUSDT.P", 4);
    h.publish_tick();
    drain(g.mailbox());

    // A BOOK-ONLY re-dirty (no `stream_status`, so no ctrl frame rides along), then a tick with NO
    // drain after it: two book slots are queued, which is the state a writer one tick behind is in.
    let sink = h.sink();
    sink.l2_snapshot(
        "binance",
        "BTCUSDT.P",
        0.1,
        vec![BookLevel::new(1.0, 1.0)],
        vec![BookLevel::new(2.0, 1.0)],
        8,
    );
    sink.l2_snapshot(
        "binance",
        "ETHUSDT.P",
        0.1,
        vec![BookLevel::new(1.0, 1.0)],
        vec![BookLevel::new(2.0, 1.0)],
        8,
    );
    h.publish_tick();
    assert_eq!(
        g.mailbox().len(),
        2,
        "the floor: two book slots must be queued or the removal below proves nothing"
    );

    h.update(g.id(), &[], std::slice::from_ref(&two), now());

    let frames = drain(g.mailbox());
    assert!(
        depth_at(&frames, "binance", "ETHUSDT.P").is_none(),
        "a book for the key this session just REMOVED is still queued against the one bound nothing \
         can evict: {frames:?}"
    );
    assert!(
        depth_at(&frames, "binance", "BTCUSDT.P").is_some(),
        "...and the key it still holds must be untouched: {frames:?}"
    );
    g.release_at(now());
}

/// **ONE KEY NAMED SIXTY-FOUR TIMES AT SIXTY-FOUR DEPTHS IS ONE ATTACH** — the half of the
/// ctrl-lane bound that `MdHub::acquire_changed`'s `changed` gate cannot supply, because a spec
/// carries a depth and a KEY does not.
///
/// `MdKey::of` ignores `depth_levels` while `MdSpec::resolved_depth` maps every `Some(n)` in
/// `[1, MD_DEPTH_LEVELS_CEILING]` to its own number, so one key named at 64 distinct depths is 64
/// ACCEPTED specs — `already_held_by_session` suppresses the session cap for a key already held —
/// and 64 `changed == true` answers, each `insert` returning the PREVIOUS spec's depth. Attaching
/// from a `Vec` therefore pushes 64 CTRL frames for ONE key, and `crate::server`'s
/// `refuse_an_oversized_spec_list` is no defence: it caps the LENGTH at
/// `MD_MAX_SPECS_PER_SESSION` and leaves dedup to the client (*"drop any duplicate spec"* is advice
/// in the refusal text, not enforcement). Three such requests against a wedged writer reach 192 on
/// a 128-deep `MD_MAILBOX_CTRL` — the exact bound `acquire_changed`'s doc claims the `changed` gate
/// restores, breached through the term that gate does not cover.
///
/// The sibling test above re-sends an IDENTICAL set, where `insert` returns the same depth and the
/// `changed` gate alone holds. This one is the case that gate answers `true` to every time.
///
/// ⚠ The single attach must also carry the DEEPEST cut asked for, not the first or the last one
/// processed: `attach_frames` runs after the add loop and cuts at `effective_depth()`, which
/// `acquire` has by then folded to the max over the whole request. Asserting the COUNT without the
/// CUT would pass against a fix that pushed the wrong one of the 64.
///
/// Mutation: make `attach` a `Vec<MdKey>` again -> red on the frame count.
#[test]
fn one_key_named_at_many_depths_attaches_once_at_the_deepest_cut() {
    use vike_datahub::md::MD_MAX_SPECS_PER_SESSION;

    let log = Log::default();
    let h = hub(&log);
    let base = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &MdSpec { depth_levels: Some(1), ..base.clone() }).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 120);
    h.publish_tick();

    // The floor: the ordinary path delivers on this harness, and it delivers the SHALLOW cut — so
    // the deep number below is arrived at by the fold rather than by the seed's own length.
    let first = drain(g.mailbox());
    match first.iter().find(|f| matches!(f, MdFrame::Depth(_))) {
        Some(MdFrame::Depth(bk)) => {
            assert_eq!(bk.bids.len(), 1, "the session's own cut: {first:?}")
        }
        _ => panic!("no book on the ordinary path — every assertion below is vacuous: {first:?}"),
    }
    assert!(g.mailbox().is_empty(), "drained empty going in");

    let deepest = MD_MAX_SPECS_PER_SESSION as u16 + 1;
    let many: Vec<MdSpec> =
        (2..=deepest).map(|d| MdSpec { depth_levels: Some(d), ..base.clone() }).collect();
    assert_eq!(many.len(), MD_MAX_SPECS_PER_SESSION as usize, "a request-legal list of one key");

    let (accepted, refused, released) = h.update(g.id(), &many, &[], now());
    assert_eq!(accepted.len(), many.len(), "every spec is accepted: {refused:?}");
    assert!(released.is_empty(), "{released:?}");
    assert!(
        !g.mailbox().must_close(),
        "one key at many depths closed the connection on its own control lane"
    );

    let frames = drain(g.mailbox());
    assert_eq!(
        frames.len(),
        2,
        "ONE key changed, so ONE status and ONE snapshot are owed — a push per accepted SPEC makes \
         the ctrl cost a function of what the client SENT rather than of what changed: {frames:?}"
    );
    let s = status_at(&frames, "binance", "BTCUSDT.P", MdLane::Depth)
        .unwrap_or_else(|| panic!("no Status for the key that changed: {frames:?}"));
    let d = depth_at(&frames, "binance", "BTCUSDT.P")
        .unwrap_or_else(|| panic!("no snapshot for the key that changed: {frames:?}"));
    assert!(s < d, "§5.2 step 5: the STATUS comes first, then the book: {frames:?}");
    match &frames[d] {
        MdFrame::Depth(bk) => assert_eq!(
            bk.bids.len(),
            deepest as usize,
            "the one attach must carry the DEEPEST cut the request asked for: {frames:?}"
        ),
        other => panic!("{other:?}"),
    }
    g.release_at(now());
}

/// **TWO SESSIONS HOLDING ONE KEY ARE BOTH SERVED BY ONE SERIALIZATION** — the fan-out guard for
/// `MdHub::fanout`, which replaced `publish_tick`'s per-tick target snapshot with a membership test
/// taken under the session table's own lock at push time.
///
/// The snapshot was stale for the whole remainder of a tick, which let a concurrent `MdUpdate`
/// remove re-create the book slot `MdHub::update` had just reclaimed — see `fanout`'s own doc for
/// why that is a bound and not a tidiness. The RACE is a lock-ordering property no single-threaded
/// test can see; what a test can hold is that the rewrite still reaches every holder of the key and
/// nobody else, which is the property a wrong membership test would break loudly.
///
/// ⚠ Non-vacuity: a THIRD session holds a different key on the same venue, so "pushed to everyone"
/// fails here rather than passing as a wider version of the right answer.
#[test]
fn one_serialization_reaches_every_holder_of_the_key_and_no_one_else() {
    let log = Log::default();
    let h = hub(&log);
    let shared = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let other = spec("binance", "ETHUSDT.P", MdLane::Depth);

    let mut a = h.open_session().unwrap();
    let mut b = h.open_session().unwrap();
    let mut c = h.open_session().unwrap();
    h.acquire(a.id(), &shared).unwrap();
    h.acquire(b.id(), &shared).unwrap();
    h.acquire(c.id(), &other).unwrap();
    h.reconcile(now());
    seed_live_depth(&h, "binance", "BTCUSDT.P", 4);
    let r = h.publish_tick();

    let fa = drain(a.mailbox());
    let fb = drain(b.mailbox());
    let fc = drain(c.mailbox());
    assert!(depth_at(&fa, "binance", "BTCUSDT.P").is_some(), "A holds the key: {fa:?}");
    assert!(depth_at(&fb, "binance", "BTCUSDT.P").is_some(), "B holds it too: {fb:?}");
    assert!(
        depth_at(&fc, "binance", "BTCUSDT.P").is_none(),
        "C holds a DIFFERENT key and was served this one anyway: {fc:?}"
    );
    // §6.1: one serialization per dirty key per tick, however many hold it. The tick produced a
    // status and a book for ONE key — two frames — and handed the same `Arc` to both holders.
    assert_eq!(r.frames_produced, 2, "one status + one book, serialized once each: {r:?}");

    // ...and a session that GIVES the key up is not served it on the next tick.
    h.update(b.id(), &[], std::slice::from_ref(&shared), now());
    let sink = h.sink();
    sink.l2_snapshot(
        "binance",
        "BTCUSDT.P",
        0.1,
        vec![BookLevel::new(1.0, 1.0)],
        vec![BookLevel::new(2.0, 1.0)],
        9,
    );
    h.publish_tick();
    assert!(depth_at(&drain(a.mailbox()), "binance", "BTCUSDT.P").is_some(), "A still holds it");
    assert!(
        b.mailbox().is_empty(),
        "a session that removed the key was served it anyway: {:?}",
        drain(b.mailbox())
    );
    a.release_at(now());
    b.release_at(now());
    c.release_at(now());
}
