//! What the hub HANDS OUT: a new holder's per-key attach (`attach_frames`) and the publisher's
//! per-tick fold of every dirty key into at most one status frame and one data frame, serialized
//! once and enqueued to every subscriber (`publish_tick` and its `fanout`). Nothing here writes a
//! socket and nothing calls a `DataClient` — `crates/vike-tradehub/src/publish.rs`'s `broadcast`
//! discipline.
//!
//! The registry mutations that decide WHO holds a key are `subscribe.rs` and `release.rs`; the free
//! functions that cut and frame a snapshot (`snapshot_of`, `frame_bytes`, `push_attach_frame`) stay
//! in the parent module, because the sink and the server reach them too.

use super::*;

impl MdHub {
    /// What a NEW subscriber is handed for one key, in this order: the key's current `Status`, then
    /// — **only if that status is `Live`** — its current book snapshot (§5.2 step 5).
    ///
    /// ⚠ **The status-first rule is not decoration, and the ORDER is enforced by this function's
    /// STRUCTURE rather than by a comment.** Without it, a slot that has been stale for two minutes
    /// (venue in `GapStart`, or a quiet resident stream) is written into `BookStore` with a FRESH
    /// LOCAL receipt stamp and reads LIVE at the desktop for a whole `DOM_STALE_MS` window — a
    /// populated, fresh-looking ladder for a market that stopped ticking two minutes ago, which is
    /// indistinguishable from a quiet market. All three source proposals had this hole.
    ///
    /// A key with no status yet emits `GapStart{now_ms}` — the honest reading of "wanted, not yet
    /// live" — and no book, so the client shows "waiting for first book" rather than an empty ladder
    /// that reads as a real, thin market.
    ///
    /// ⚠ **The TAPE is deliberately NOT replayed, and §8 left that open.** §7.3 rule 4 is the
    /// answer: never resume a tape across a socket. The server keeps no replay buffer, so a
    /// reconnect must be a HOLE, never a duplicate — handing a new subscriber prints from before it
    /// existed, with no gap marker, double-counts every reconnect into `OrderflowAgg`, which has no
    /// per-trade dedup.
    ///
    /// It assigns NO `seq`: the publisher owns the counter, so an attach reads the entry's CURRENT
    /// seq and the first pushed frame for that key is `seq + 1`, contiguous by construction.
    pub fn attach_frames(&self, key: &MdKey, now_ms: i64) -> Vec<MdFrame> {
        let Some(entry) = self.entry(&key.venue, &key.symbol, key.lane) else { return Vec::new() };
        let status = entry
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or(WireStreamStatus::GapStart { at_ts_ms: now_ms });
        let mut out = vec![MdFrame::Status {
            venue: key.venue.clone(),
            symbol: key.symbol.clone(),
            lane: key.lane,
            status,
        }];
        if !matches!(status, WireStreamStatus::Live { .. }) {
            return out;
        }
        if matches!(key.lane, MdLane::Depth | MdLane::Book) {
            let book = entry.book_slot.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(b) = book {
                let seq = entry.seq.load(Ordering::Acquire);
                let snap = snapshot_of(&entry.key, &b, entry.effective_depth(), seq);
                out.push(match key.lane {
                    MdLane::Book => MdFrame::Book(snap),
                    _ => MdFrame::Depth(snap),
                });
            }
        }
        out
    }

    /// Enqueue one ALREADY-SERIALIZED frame to every session that holds `key` **at this instant**,
    /// building the per-subscriber envelope once per target.
    ///
    /// # ⚠ The membership test and the push are under ONE lock, and that is the whole point
    ///
    /// [`MdHub::publish_tick`] reads the session table once per tick and then serializes its way
    /// down the dirty set, so a target list taken at the top is stale by the time most keys are
    /// pushed — a window a whole tick wide, opening at every DOM-window close. Inside it,
    /// [`MdHub::update`]'s remove path can drop the key from the session, `release_key` it and
    /// reclaim its queued frame with `Mailbox::drop_book`, and a push against the stale list then
    /// RE-CREATES the slot. Nothing evicts a book slot — `Mailbox::enforce_bounds` drains the tape
    /// only, because [`crate::md::MD_MAILBOX_BYTES`]' assertion is what guarantees the books fit — so
    /// the resurrected slot survives until the writer drains, which under a wedged writer is never,
    /// and `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` stops being an invariant. The client half
    /// is the same frame's other cost: `crates/vike-app-core/src/data/md_session.rs`'s `apply_frame`
    /// routes a book into `BookStore` by `(venue, symbol)` without consulting the served set, so it
    /// repopulates a store entry the window that wanted it has just dropped.
    ///
    /// Taking `sessions` HERE closes both, because the remove path holds the same lock while it
    /// takes the key out of the session and calls `drop_book` after it: a push either precedes the
    /// removal — and `drop_book` then reclaims what it queued — or reads a table the key is already
    /// gone from and enqueues nothing. There is no third ordering.
    ///
    /// ⚠ It closes the PUBLISHER and only the publisher. The other book producer is the attach
    /// push, which serializes and therefore may not hold this lock at all; [`MdHub::update`] answers
    /// that one with a sweep after its pushes instead, and declares the one door that still has
    /// neither.
    ///
    /// ⚠ **It costs one uncontended `sessions` acquisition per FRAME, not per subscriber, and no
    /// serialization happens inside it** — `bytes` is an `Arc` the caller built before calling, so
    /// §6.1's "serialize once, never under a lock" is untouched and only the envelope is per-target.
    /// The lock order is the file's own, `sessions` then the mailbox's: nothing in
    /// [`crate::md::mailbox::Mailbox`] can reach back for the session table, and `close_session` already
    /// nests the two this way.
    fn fanout(&self, key: &MdKey, make: impl Fn() -> Outgoing) {
        let g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        for s in g.values() {
            if s.keys.contains_key(key) {
                s.mailbox.push(make());
            }
        }
    }

    /// Fold every DIRTY key into at most one status frame and one data frame, serialize each ONCE,
    /// and enqueue to every subscriber of that key.
    ///
    /// ⚠ **The wire `seq` is assigned HERE, before any drop decision** (§7.2). That is what gives
    /// `TapeGap` a backstop the dropper cannot lie about: a contiguity break at the client means
    /// exactly one thing — this connection did not receive frames the server produced. An
    /// implementation that assigned it AFTER the drop decision would still look self-consistent
    /// (the dropper reports its own count, the surviving frames are contiguous) and would have no
    /// pre-drop numbers to report, so every disclosed range would collapse to `from + 1`.
    ///
    /// It never writes a socket and never calls a `DataClient` —
    /// `crates/vike-tradehub/src/publish.rs`'s `broadcast` discipline.
    pub fn publish_tick(&self) -> TickReport {
        let mut report = TickReport::default();
        // ⚠ SESSIONS FIRST, THEN KEYS — every path in this file takes the two in that order
        // (`acquire` takes `sessions` then `keys.write`), and taking them the other way round here
        // would be the classic inversion: one publish tick against one concurrent subscribe is all
        // it needs. (This line said "ONE pass over the session table per tick, not per key" and it
        // is no longer the whole truth: this pass is the pre-test, and `MdHub::fanout` takes the
        // table again per FRAME PRODUCED — uncontended, O(sessions ≤ MD_MAX_STREAM_CONNS), with
        // nothing serialized inside it. What the sentence was protecting — no per-SUBSCRIBER walk
        // of the table, no serialization under a lock — still holds.)
        //
        // ⚠ **THIS IS A PRE-TEST AND NOT THE TARGET LIST**, and the difference is a bound. Its only
        // job is to answer *is anyone holding this key at all* before the tick pays for a
        // serialization — §6.1's rule that a key nobody subscribes to costs nothing. The frames are
        // handed out by [`MdHub::fanout`], which re-reads the session table UNDER ITS LOCK at push
        // time. A target list snapshotted HERE is stale for the whole remainder of the tick, and a
        // concurrent `MdUpdate` removing a key inside that window re-created the book slot
        // [`MdHub::update`] had just reclaimed with `Mailbox::drop_book` — a slot nothing evicts
        // (`Mailbox::enforce_bounds` drains the tape only), so the invariant
        // `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` that [`crate::md::MD_MAILBOX_BYTES`]'
        // assertion rests on was breachable by ordinary DOM-window churn against a writer a tick
        // behind. Being stale in the OTHER direction is harmless and deliberately not chased: a
        // session that joins a key mid-tick is handed `attach_frames` by whichever door it came
        // through, so the worst case is one book delivered twice, superseded in its own slot.
        let subscribed: BTreeSet<MdKey> = {
            let g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            g.values().flat_map(|s| s.keys.keys().cloned()).collect()
        };
        let entries = self.snapshot();

        for entry in entries {
            if !entry.dirty.swap(false, Ordering::AcqRel) {
                continue;
            }
            report.keys_walked += 1;
            let key = &entry.key;
            let listening = subscribed.contains(key);

            // ⚠ **THE PER-TICK DELTAS ARE CONSUMED WHETHER OR NOT ANYONE IS LISTENING, and that is
            // what makes §7.3 rule 4 STRUCTURAL.** A tape held while a key has no subscriber hands
            // the NEXT subscriber prints from before it existed — with no gap marker, because from
            // the hub's side nothing was dropped — and `OrderflowAgg` has no per-trade dedup to
            // notice. The server keeps NO replay buffer, so a reconnect is a HOLE, never a
            // duplicate, and the cheapest way to guarantee that is never to accumulate one. The BOOK
            // slot is deliberately NOT consumed: it is latest-wins state, and `attach_frames` is
            // what decides whether a new subscriber may see it (only when the status is `Live`).
            let status_changed = entry.status_dirty.swap(false, Ordering::AcqRel);
            let drained = if matches!(key.lane, MdLane::Trades) {
                let mut g = entry.tape.lock().unwrap_or_else(PoisonError::into_inner);
                let batch: Vec<TradeTick> = g.drain(..).collect();
                drop(g);
                Some((batch, entry.dropped.swap(0, Ordering::AcqRel)))
            } else {
                None
            };
            if !listening {
                continue;
            }

            // 1. STATUS, if it changed. Ctrl lane, so it is drained ahead of any book backlog.
            if status_changed {
                let status = *entry.status.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(status) = status {
                    let frame = MdFrame::Status {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        lane: key.lane,
                        status,
                    };
                    if let Some(bytes) = frame_bytes(frame) {
                        report.frames_produced += 1;
                        self.fanout(key, || Outgoing {
                            key: key.clone(),
                            lane: LaneClass::Ctrl,
                            bytes: Arc::clone(&bytes),
                            seq: 0,
                            ticks: 0,
                            hub_dropped: 0,
                        });
                    }
                }
            }

            // 2. THE DATA FRAME. One per key per tick — the property §12.4's whole mailbox
            //    derivation rests on, and the reason no `MD_TAPE_BATCH_MAX` is introduced.
            match key.lane {
                MdLane::Depth | MdLane::Book => {
                    let book =
                        entry.book_slot.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    let Some(b) = book else { continue };
                    // ⚠ seq BEFORE any drop decision.
                    let seq = entry.seq.fetch_add(1, Ordering::AcqRel) + 1;
                    let snap = snapshot_of(key, &b, entry.effective_depth(), seq);
                    let frame = match key.lane {
                        MdLane::Book => MdFrame::Book(snap),
                        _ => MdFrame::Depth(snap),
                    };
                    let Some(bytes) = frame_bytes(frame) else { continue };
                    entry.note_frame_size(bytes.len());
                    report.frames_produced += 1;
                    self.fanout(key, || Outgoing {
                        key: key.clone(),
                        lane: LaneClass::Book,
                        bytes: Arc::clone(&bytes),
                        seq,
                        ticks: 0,
                        hub_dropped: 0,
                    });
                }
                MdLane::Trades => {
                    let (batch, hub_dropped) = drained.expect("the tape lane always drains above");
                    if batch.is_empty() && hub_dropped == 0 {
                        continue;
                    }
                    let seq = entry.seq.fetch_add(1, Ordering::AcqRel) + 1;
                    let ticks = batch.len() as u64;
                    let frame = MdFrame::Trades {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        ticks: batch,
                        seq,
                    };
                    let Some(bytes) = frame_bytes(frame) else { continue };
                    report.frames_produced += 1;
                    self.fanout(key, || Outgoing {
                        key: key.clone(),
                        lane: LaneClass::Tape,
                        bytes: Arc::clone(&bytes),
                        seq,
                        ticks,
                        hub_dropped,
                    });
                }
            }
        }
        report
    }
}
