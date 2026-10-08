//! What a session — or the operator — ASKS the hub for: pin a resident key, open a session, take
//! keys for it and change what it holds. Every method here is registry bookkeeping: **it never
//! blocks, never calls a venue and never fails on I/O**, because `reconcile.rs`'s
//! [`MdHub::reconcile`] is the only holder of a `DataClient`.
//!
//! The other halves of the same lifecycle are siblings: `release.rs` (`close_session` and
//! `release_key`, the end of a hold), `publish.rs` (`attach_frames`, what a NEW holder is handed,
//! and the publisher) and `reconcile.rs` (the venue side). The parent module's doc is the argument
//! for the shape; this file moved its methods out of one `impl` block and changed none of them
//! beyond the module paths its doc links name.

use super::*;

impl MdHub {
    /// PIN a key as tier R — a refcount floor of 1, never released. Idempotent.
    ///
    /// # ⚠ It RUNS [`MdHub::acquire`]'s CHECKS, and it did not
    ///
    /// This went straight to the registry: no `vike_model::VENUES` membership, no `served` check, no
    /// [`vike_data::require_live_verb`], and no cap of any kind. Three things followed, none of them
    /// visible to an operator:
    ///
    /// 1. A typo'd `VIKE_DATAHUB_LIVE_RESIDENT` row (`notavenue:X:depth`, or a real venue on a lane
    ///    it does not serve) PARSED — [`crate::md::parse_resident_set`] validates only the three-field
    ///    shape and the lane word — and then produced an endless retry: a resident entry is
    ///    permanently `wanted`, so [`MdHub::reconcile`] phase 1 attempts it every pass and
    ///    [`MdHub::spawn`]'s loop logs the failure every [`crate::md::MD_REAP_INTERVAL`], forever.
    /// 2. [`MD_MAX_KEYS_PER_VENUE`]'s own doc says "RESIDENT INCLUDED", and it was read only inside
    ///    `acquire`'s on-demand admission — so residents were counted BY it and never bounded by it.
    /// 3. With [`MD_MAX_KEYS_TOTAL`] deliberately exempting residents (§5.4), nothing bounded the
    ///    tier-R set at all: N rows armed N venue subscriptions, which is memory (§12.6's hub term)
    ///    and venue budget (200 resident binance depth keys is ~2,000 weight/min of steady-state
    ///    re-seed against a 2,400/min IP budget shared with the order-signing daemon).
    ///
    /// # The refusal is a `String`, not an `MdRefusal`, deliberately
    ///
    /// This one never crosses the wire — a resident row is the DAEMON's own declaration, and the
    /// only reader of the refusal is the operator's log. A wire variant would widen a type whose
    /// every arm is a client-facing classification (`MdRefusal::is_permanent`) with a case no client
    /// can ever see. The caller (`crate::datahub_cli`) logs it beside the unparseable-row warning:
    /// degrade and NAME the row, never refuse startup — a venue subscription is a CAPABILITY
    /// (`docs/decisions/0013-degrade-vs-refuse.md`).
    pub fn add_resident(&self, spec: &MdSpec) -> Result<(), String> {
        // The same order `acquire` refuses in — cheapest and most permanent first, which since
        // 2026-09-11 starts with the SYMBOL. Sharing the validator is what stops the daemon
        // admitting through `VIKE_DATAHUB_LIVE_RESIDENT` what it refuses on the wire; it also
        // removes an asymmetry that ran the other way, because `crate::md::parse_resident_set` has
        // always refused an empty symbol in the operator's own declaration while `acquire` accepted
        // one from a network client.
        validate_md_symbol(&spec.symbol)?;
        let key = MdKey::of(spec);
        let depth = spec.resolved_depth();
        if !vike_model::VENUES.contains(&key.venue.as_str()) {
            return Err(format!("`{}` is not a venue in vike_model::VENUES", key.venue));
        }
        if !self.served.iter().any(|v| v == &key.venue) {
            return Err(format!(
                "this build links no market-data client for `{}` — it serves [{}]",
                key.venue,
                self.served.join(", ")
            ));
        }
        if let Err(e) = vike_data::require_live_verb(&key.venue, key.lane.live_verb()) {
            return Err(e.to_string());
        }

        let mut g = self.keys.write().unwrap_or_else(PoisonError::into_inner);
        let already = g
            .get(&key.venue)
            .and_then(|s| s.get(&key.symbol))
            .and_then(|slots| slots[lane_index(key.lane)].as_ref())
            .is_some();
        if !already {
            let venue_keys: u32 = g
                .get(&key.venue)
                .map(|s| s.values().flatten().flatten().count() as u32)
                .unwrap_or(0);
            if venue_keys >= MD_MAX_KEYS_PER_VENUE {
                return Err(format!(
                    "`{}` already holds {venue_keys} keys on venue `{}` (MD_MAX_KEYS_PER_VENUE = \
                     {MD_MAX_KEYS_PER_VENUE}, the cap that leaves the order-signing daemon its share \
                     of this box's per-IP venue budget)",
                    key.symbol, key.venue
                ));
            }
            let residents: u32 = g
                .values()
                .flat_map(|s| s.values())
                .flatten()
                .flatten()
                .filter(|e| e.is_resident())
                .count() as u32;
            if residents >= MD_MAX_KEYS_RESIDENT {
                return Err(format!(
                    "this daemon already holds {residents} resident keys (MD_MAX_KEYS_RESIDENT = \
                     {MD_MAX_KEYS_RESIDENT}, the term §12.6's hub-memory arithmetic — and with it \
                     the 32 MB co-hosting claim — is computed over)"
                ));
            }
        }
        let slots = g.entry(key.venue.clone()).or_default().entry(key.symbol.clone()).or_default();
        let idx = lane_index(key.lane);
        match &slots[idx] {
            // A key already admitted on demand becomes resident too; the declared depth is a FLOOR
            // that outlives every subscriber, so it is recorded separately from the live max.
            Some(e) => {
                e.pin_resident(depth);
            }
            None => {
                slots[idx] = Some(Arc::new(StreamEntry::new(key, true, depth)));
            }
        }
        drop(g);
        self.poke();
        Ok(())
    }

    /// Open a market-data session, refusing past [`MD_MAX_STREAM_CONNS`].
    ///
    /// The refusal is a whole-REQUEST condition, so it is a `Result<_, String>` the caller answers
    /// with `Response::Error` — never an all-refused `MdSubscribed`, which would mode-switch a
    /// connection into a writer with nothing to write. See
    /// `vike_datahub_client::market::MdRefusal`'s doc for the invariant.
    pub fn open_session(self: &Arc<Self>) -> Result<SessionGuard, String> {
        let n = self.stream_conns.fetch_add(1, Ordering::AcqRel);
        if n as usize >= MD_MAX_STREAM_CONNS {
            self.stream_conns.fetch_sub(1, Ordering::AcqRel);
            return Err(format!(
                "this datahub already serves {MD_MAX_STREAM_CONNS} market-data stream connections \
                 (the cap that bounds this plane's mailbox memory); nothing was subscribed. Close a \
                 stream and retry."
            ));
        }
        let id = MdSessionId::fresh();
        let mailbox = Mailbox::new();
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, SessionState { keys: BTreeMap::new(), mailbox: Arc::clone(&mailbox) });
        Ok(SessionGuard { hub: Arc::clone(self), id, mailbox, released: false })
    }

    /// This session's mailbox, or `None` if it has ended.
    pub fn mailbox_of(&self, id: MdSessionId) -> Option<Arc<Mailbox>> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|s| Arc::clone(&s.mailbox))
    }

    /// Whether this session is still open — what an `MdUpdate` naming an unknown or expired session
    /// is refused on.
    pub fn has_session(&self, id: MdSessionId) -> bool {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner).contains_key(&id)
    }

    /// The keys one session holds, in key order.
    pub fn session_keys(&self, id: MdSessionId) -> Vec<MdKey> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|s| s.keys.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Take one spec for `session`. **Never blocks, never calls a venue, never fails on I/O.**
    ///
    /// Returns the AUTHORITATIVE spec — the request with its depth clamped, which §4.3 says is an
    /// acceptance with a smaller number rather than a refusal.
    ///
    /// The refusal ORDER is cheapest-and-most-permanent first, each naming its own number:
    /// [`MdRefusal::SymbolRejected`] → [`MdRefusal::UnknownVenue`] → [`MdRefusal::VenueNotServed`]
    /// → [`MdRefusal::LaneUnsupported`]
    /// (from [`vike_data::require_live_verb`], so §7.5's gate is a re-read of the declared matrix
    /// rather than a hand list) → [`MdRefusal::SpecCapSession`] → [`MdRefusal::KeyCapVenue`] →
    /// [`MdRefusal::KeyCapTotal`].
    ///
    /// # ⚠ The SYMBOL rung is new, and this function used to validate everything but it
    ///
    /// Of a spec's five fields, `venue` faced `vike_model::VENUES`, `lane` is a fieldless enum serde
    /// refuses to decode as anything else, `depth_levels` is clamped by
    /// `vike_datahub_client::market::MdSpec::resolved_depth`, and `session` is a `u128`. `symbol`
    /// was bounded by nothing but the post-auth 64 MiB frame ceiling. A subscribe naming a
    /// 10,000-character symbol was ACCEPTED: two `String` clones into [`MdKey`], both locks, an
    /// `Arc<StreamEntry>`, two more clones into the registry, and a `poke()` that woke the
    /// reconciler into calling `subscribe_depth(venue, <10 KB>)` on the REAL venue. Then
    /// [`MdHub::attach_frames`] emitted a `Status` carrying that string verbatim — it does so for
    /// every accepted key, whether or not any venue ever streams for it — far over
    /// [`crate::md::MD_CTRL_FRAME_CEILING_BYTES`], which is a TERM in `MD_MAILBOX_BYTES`' compile-time
    /// assertion and therefore in the 32 MB claim
    /// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` rests on. That record's
    /// decision 2 — *"A subscription's cost is bounded by server constants and by typed refusals,
    /// never by the request"* — is what the bound RESTORES rather than adds.
    ///
    /// It goes FIRST because it is both the cheapest check (a `len()` and a byte scan, no lock, no
    /// roster walk) and the most permanent, which is the order this list already declares — and
    /// because "refused before anything is allocated or spawned" is only true if it precedes
    /// [`MdKey::of`].
    pub fn acquire(&self, id: MdSessionId, spec: &MdSpec) -> Result<MdSpec, MdRefusal> {
        self.acquire_changed(id, spec).map(|(s, _)| s)
    }

    /// [`MdHub::acquire`] plus the one fact [`MdHub::update`] needs and the wire does not: whether
    /// this session's HOLDING of the key changed — it is new to the session, or the depth it asks
    /// for is a different number from the one it asked for before.
    ///
    /// # ⚠ Why an acceptance is not the same as a change, and why the difference is a BOUND
    ///
    /// [`MdHub::acquire`] returns `Ok` for a key the session already holds — `already_held_by_session`
    /// suppresses the refcount bump and nothing else — so a caller reading only the `Result` cannot
    /// tell a duplicate from a new key. [`MdHub::update`] attaches one `Status` (and, when the key is
    /// `Live`, one book) per key it pushes, and `crate::server`'s `refuse_an_oversized_spec_list`
    /// bounds ONE request at [`MD_MAX_SPECS_PER_SESSION`] = 64 against a
    /// [`crate::md::MD_MAILBOX_CTRL`] of 128. Attaching per ACCEPTED spec would therefore make the ctrl
    /// cost a function of what the client SENT rather than of what changed: three requests re-naming
    /// the same 64 already-held specs — no stall, no slow link, no venue event — push 192 frames onto
    /// a 128-deep lane and the peer is answered with `MdBye::ControlLaneOverflow`. That is #1753's
    /// failure reached through a new producer, and `docs/decisions/0052`'s decision 2 — *"a
    /// subscription's cost is bounded by server constants … never by the request"* — false again on
    /// the one term that record had already had to restore.
    ///
    /// Attaching per CHANGE costs the shipped desktop nothing:
    /// `crates/vike-app-core/src/data/md_session.rs`'s `MdSession::diff` computes `add` by
    /// `MdSpec::key()` against the server's authoritative `served` set, so it never re-names a key it
    /// is already served. The gate bites only a client re-sending its own set.
    ///
    /// The DEPTH half of the predicate is what closes the depth-change case with no `mark_dirty`
    /// anywhere: see [`StreamEntry::settle_depth`].
    ///
    /// ⚠ **This gate alone does NOT give [`MdHub::update`] the bound above, and the second half is
    /// in that function.** The depth term is what makes a re-send at a NEW depth a change, which is
    /// correct per spec and wrong per KEY: `MdKey::of` ignores `depth_levels`, so one key named 64
    /// times at 64 different depths answers `true` 64 times. `update` collects into a `BTreeSet`
    /// for exactly that reason, and the bound it actually delivers is *distinct keys changed*.
    fn acquire_changed(&self, id: MdSessionId, spec: &MdSpec) -> Result<(MdSpec, bool), MdRefusal> {
        if let Err(why) = validate_md_symbol(&spec.symbol) {
            return Err(MdRefusal::SymbolRejected(why));
        }
        if !vike_model::VENUES.contains(&spec.venue.as_str()) {
            return Err(MdRefusal::UnknownVenue);
        }
        if !self.served.iter().any(|v| v == &spec.venue) {
            return Err(MdRefusal::VenueNotServed(self.served.join(", ")));
        }
        if let Err(e) = vike_data::require_live_verb(&spec.venue, spec.lane.live_verb()) {
            // The venue's own `&'static str`, forwarded verbatim — the wire's refusal set cannot
            // drift from `crates/vike-model/src/venues/venue_caps.rs`'s declared rows.
            return Err(MdRefusal::LaneUnsupported(e.to_string()));
        }

        let key = MdKey::of(spec);
        let depth = spec.resolved_depth();

        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = sessions.get_mut(&id) else {
            // A caller that lost its session between the check and here; treat as a total-cap
            // refusal rather than inventing a variant nothing else can produce.
            return Err(MdRefusal::KeyCapTotal { held: 0, cap: MD_MAX_KEYS_TOTAL });
        };
        let already_held_by_session = state.keys.contains_key(&key);
        if !already_held_by_session && state.keys.len() as u32 >= MD_MAX_SPECS_PER_SESSION {
            return Err(MdRefusal::SpecCapSession {
                held: state.keys.len() as u32,
                cap: MD_MAX_SPECS_PER_SESSION,
            });
        }

        let mut g = self.keys.write().unwrap_or_else(PoisonError::into_inner);
        let existing = g
            .get(&key.venue)
            .and_then(|s| s.get(&key.symbol))
            .and_then(|slots| slots[lane_index(key.lane)].clone());

        if existing.is_none() {
            // ⚠ RESIDENT keys count against the per-VENUE cap and NOT against the total (§5.4): an
            // operator's declared set must never be squeezed out by ad-hoc charts, and
            // `MD_MAX_KEYS_TOTAL = 0` must still serve tier R.
            let venue_keys: u32 = g
                .get(&key.venue)
                .map(|s| s.values().flatten().flatten().count() as u32)
                .unwrap_or(0);
            if venue_keys >= MD_MAX_KEYS_PER_VENUE {
                return Err(MdRefusal::KeyCapVenue {
                    venue: key.venue.clone(),
                    held: venue_keys,
                    cap: MD_MAX_KEYS_PER_VENUE,
                });
            }
            let total_on_demand: u32 = g
                .values()
                .flat_map(|s| s.values())
                .flatten()
                .flatten()
                .filter(|e| !e.is_resident())
                .count() as u32;
            if total_on_demand >= MD_MAX_KEYS_TOTAL {
                return Err(MdRefusal::KeyCapTotal {
                    held: total_on_demand,
                    cap: MD_MAX_KEYS_TOTAL,
                });
            }
        }

        let entry = match existing {
            Some(e) => {
                e.raise_depth(depth);
                e
            }
            None => {
                let e = Arc::new(StreamEntry::new(key.clone(), false, depth));
                g.entry(key.venue.clone()).or_default().entry(key.symbol.clone()).or_default()
                    [lane_index(key.lane)] = Some(Arc::clone(&e));
                e
            }
        };

        // ⚠ **BOTH OF THESE ARE PUBLISHED UNDER `keys.write()`, AND THAT IS WHAT MAKES
        // `reconcile` PHASE 3's RE-CHECK MEAN ANYTHING.** They used to run after `drop(g)`, and the
        // two lines were the whole remaining refcount race: the reaper re-evaluates `wanted` from
        // the slot's current `Arc` while it holds this same lock, so an admission that had already
        // released it — but had not yet bumped the count or cleared the deadline — was still read as
        // unwanted and its slot cleared under a session that now holds the key. Narrowing that
        // window to a few atomics is not closing it, and the comment in phase 3 claimed closed.
        // Neither line needs the lock to be released: both are plain atomics on the entry, and the
        // one thing that genuinely cannot move up (`settle_depth`, which folds over `sessions`) is
        // still below.
        if !already_held_by_session {
            entry.subscribers.fetch_add(1, Ordering::AcqRel);
        }
        // ⚠ CLEAR the linger deadline. Without this a key released and re-acquired inside the linger
        // keeps its ABSOLUTE deadline and is reaped 60 s later anyway — a Trade window toggled off and
        // on then loses its book, in the exact series somebody just asked for.
        entry.zero_since.store(-1, Ordering::Release);
        drop(g);

        // ⚠ RECORDED PER SESSION, and re-recorded on a re-`acquire` of a key this session already
        // holds — an `MdUpdate` may LOWER what this subscriber wants, and the entry's own field is a
        // max that cannot be recomputed from anything else. See `StreamEntry::depth`.
        //
        // The previous value is also the CHANGE answer this function is here to give: `None` is a
        // key new to the session, a different depth is a new request for one it already held, and
        // the SAME depth is a re-send that asks for nothing the first mention did not.
        let changed = state.keys.insert(key.clone(), depth) != Some(depth);
        // The `&mut` borrow of one session ends above, so the fold over ALL of them is legal here
        // and still inside the ONE `sessions` lock this function took — no second acquisition, no
        // window in which a concurrent release could fold against a set this one is not in yet.
        let want = max_requested_depth(&sessions, &key);
        entry.settle_depth(want);
        drop(sessions);
        self.poke();

        Ok((MdSpec { depth_levels: Some(depth), ..spec.clone() }, changed))
    }

    /// The `MdUpdate` verb's registry mutation: add, remove, **attach**, report.
    ///
    /// `remove` matches on the KEY and ignores `depth_levels` (`MdSpec::key`), and a `remove` naming
    /// a spec this session never held is silently absent from `released` rather than an error.
    ///
    /// # ⚠ THIS IS THE SECOND DOOR INTO THE REGISTRY, AND FOR A WHILE ONLY THE FIRST ONE ATTACHED
    ///
    /// `MdSubscribe` reaches the registry through `crate::server`'s `run_market_writer`, which
    /// pushes [`MdHub::attach_frames`] per accepted key before it enters its drain loop. This verb
    /// is the OTHER door, and it is the one **every Trade window after the first** takes:
    /// `crates/vike-app-core/src/data/md_session.rs`'s `push_update` opens a fresh short-lived connection
    /// and sends `MdUpdate`, because the stream socket's server side has left its read loop for
    /// good. It used to bump the refcount, fold the depth and push NOTHING.
    ///
    /// Nothing else covered for it. [`MdHub::publish_tick`] skips any entry whose `dirty` bit is
    /// clear, and the three writers of that bit are all SINK-side (`store_book`, `store_status`,
    /// `push_trade`) — `acquire` sets it nowhere. So the adding session received no status and no
    /// snapshot **until that venue's next update**: seconds to minutes on a quiet polymarket
    /// instrument, and the whole of §12.5's reconnect state on binance depth.
    ///
    /// The client cannot paper over it and is not allowed to try — §10 rules out a snapshot-request
    /// verb deliberately — and the symptom is worse than a blank ladder. `MdSession::gapped` is
    /// populated only by an arriving `Status` frame, so a key that receives NOTHING is not gapped
    /// and `refresh_statuses` counts it among the LIVE streams: an empty DOM with the Connections
    /// tool reporting the venue is fine, which is the exact failure that field's own doc says it
    /// exists to prevent, reached through a door that doc did not know about.
    ///
    /// # Why the attach and not `acquire().mark_dirty()`
    ///
    /// Marking the entry dirty was the cheaper-looking fix and it is a correctness regression, in
    /// four ways that no amount of care in `publish_tick` would fix without turning it into this
    /// function: it emits a `Status` only when `status_dirty` is set, so the status-first rule §5.2
    /// step 5 states would be bypassed on the one path where the ordering is new information; its
    /// data arm has NO `Live` gate, so a key sitting in `GapStart`/`Stale` or lingering with a
    /// two-minute-old book would be shipped to a client that stamps a FRESH local receipt (§7.4) and
    /// renders it live — the hazard [`MdHub::attach_frames`]' structure exists to enforce against;
    /// it emits nothing at all when `book_slot` is `None`, which is the quiet-instrument case this
    /// whole fix is about; and it serves the `Trades` lane not at all. It would also BROADCAST:
    /// one dirty key enqueues to every subscriber of it, so S−1 sessions that already hold the book
    /// are republished to on behalf of a session that did not ask — work on the box that also signs
    /// orders, scaling with the number of open Trade windows on a hot symbol.
    ///
    /// So the push reuses `attach_frames` **verbatim** — status first, book only when `Live`, tape
    /// never replayed are properties of THAT function's structure, and a second copy of them here is
    /// where the next divergence would land — and it goes to the ONE session that asked.
    /// [`MdHub::publish_tick`] is untouched by this fix, so §6.1's "one serialization per dirty key
    /// per tick" stays literally true: the attach path is the documented per-SUBSCRIBER lane
    /// ([`push_attach_frame`]), it runs on the short-lived `MdUpdate` dispatch thread, and it holds
    /// no lock the publisher wants.
    ///
    /// ⚠ It attaches once per KEY CHANGED, not per spec ACCEPTED — two gates, not one, because a
    /// spec carries a depth and a key does not. [`MdHub::acquire_changed`] is the first and carries
    /// why the ctrl lane needs it; the `BTreeSet` in the add loop below is the second.
    ///
    /// ⚠ It runs AFTER the add loop, not inside it: `acquire` has by then raised and settled
    /// [`StreamEntry::depth`] for every spec in the batch, so the snapshot is cut at the depth the
    /// whole request asked for rather than at the depth the batch had reached mid-loop.
    ///
    /// **This defect was found by the wire CLIENT while §9's half was being built (#1758) and
    /// reported against the server rather than fixed there.**
    #[allow(clippy::type_complexity)]
    pub fn update(
        &self,
        id: MdSessionId,
        add: &[MdSpec],
        remove: &[MdSpec],
        now_ms: i64,
    ) -> (Vec<MdSpec>, Vec<(MdSpec, MdRefusal)>, Vec<MdSpec>) {
        // ONE lookup for both halves below. `None` is a session that has already ended, and both
        // halves are then correctly skipped — `Mailbox::push` on a closed box discards anyway.
        let mailbox = self.mailbox_of(id);
        let mut released = Vec::new();
        for spec in remove {
            let key = MdKey::of(spec);
            let dropped = {
                let mut g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
                match g.get_mut(&id) {
                    Some(s) => s.keys.remove(&key).is_some(),
                    None => false,
                }
            };
            if dropped {
                self.release_key(&key, now_ms);
                // ⚠ AND THE KEY'S QUEUED BOOK GOES WITH IT. The BOOK lane is the one lane nothing
                // evicts — `Mailbox::enforce_bounds` drains only the tape, and says why: the byte
                // bound's compile-time assertion *"is what guarantees a full session of
                // ceiling-depth books fits"*. That assertion is
                // `MD_MAX_SPECS_PER_SESSION * MD_FRAME_CEILING_BYTES + …`, so it rests entirely on
                // `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` — and this loop broke it: a removed
                // key's already-queued slot was never dropped, so one `remove 64 + add 64` against a
                // writer that is a tick behind leaves 128 unevictable slots, 1.86× the bound, with
                // nothing able to reclaim them. Attaching on update (below) makes that reachable
                // INSIDE one request rather than one tick later, which is why it is closed here.
                //
                // It is also right on its own terms: `MdSession::apply_frame` routes a book into
                // `BookStore` by `(venue, symbol)` and never consults the served set, so a late
                // frame for a removed key repopulates a store entry the window that wanted it has
                // just dropped.
                //
                // The TAPE is deliberately left alone: it is bounded AND evictable, so it cannot
                // falsify the assertion, and a mailbox's owed gaps are a disclosure that must
                // survive a key leaving and coming back.
                //
                // ⚠ THIS IS ONE THIRD OF THE CLOSE AND NOT THE WHOLE OF IT. Reclaiming the slot
                // here is worth nothing on its own while something can enqueue a NEW one for the
                // same key a moment later, and there are two somethings. `MdHub::publish_tick`
                // reads the session table once per TICK, so a list taken at its top says this
                // session still holds the key for the whole remainder of that tick — closed by
                // `MdHub::fanout`, which re-reads that table under its lock at push time, the same
                // lock the `s.keys.remove` two lines above took. And the attach push at the end of
                // this function cannot test membership where it pushes (it serializes, so it may
                // not hold that lock) — closed by the sweep that follows it. All three, or the
                // bound is luck.
                //
                // The declared residual: `crate::server`'s `run_market_writer` attaches the same
                // way after `MdSubscribe`, and it has no sweep. A client that fires an `MdUpdate`
                // removing a key it has only just subscribed to — it has the session id, the
                // `MdSubscribed` reply precedes that attach loop — can leave one slot per raced key
                // behind. That door predates this function and is not widened by it; its own
                // ctrl-lane `Status` reaches `MD_MAILBOX_CTRL` and closes the connection long
                // before the accumulation becomes interesting.
                if let Some(mailbox) = &mailbox {
                    mailbox.drop_book(&key);
                }
                released.push(spec.clone());
            }
        }
        let mut accepted = Vec::new();
        let mut refused = Vec::new();
        // ⚠ A SET, not a `Vec`, and that is the OTHER half of the ctrl-lane bound the `changed` gate
        // starts. `MdKey::of` ignores `depth_levels` while `MdSpec::resolved_depth` maps every
        // `Some(n)` in 1..=MD_DEPTH_LEVELS_CEILING to its own number, so ONE key named 64 times at
        // 64 different depths is 64 accepted specs, 64 `changed == true` answers — each `insert`
        // returns the PREVIOUS spec's depth — and, from a `Vec`, 64 attach pushes for one key.
        // `crate::server`'s `refuse_an_oversized_spec_list` caps the LENGTH at
        // `MD_MAX_SPECS_PER_SESSION` and leaves dedup to the client ("drop any duplicate spec" is
        // advice in the refusal text, not enforcement), so three such requests against a wedged
        // writer reach 192 on a 128-deep `MD_MAILBOX_CTRL` — the exact bound `acquire_changed`'s doc
        // claims the `changed` gate restores, breached through the term it does not cover. With the
        // set the push count is bounded by DISTINCT keys touched, which is what that doc says.
        //
        // Deduping by key is also what the snapshot WANTS: `attach_frames` cuts at
        // `effective_depth()`, which `acquire` has already folded to the deepest of the request's
        // mentions by the time this loop ends, so one push per key carries the answer all 64 asked
        // for and 63 of them would have carried a cut nobody is owed twice.
        let mut attach = BTreeSet::new();
        for spec in add {
            match self.acquire_changed(id, spec) {
                Ok((s, changed)) => {
                    if changed {
                        attach.insert(MdKey::of(&s));
                    }
                    accepted.push(s);
                }
                Err(r) => refused.push((spec.clone(), r)),
            }
        }
        if let Some(mailbox) = &mailbox {
            for key in &attach {
                for frame in self.attach_frames(key, now_ms) {
                    push_attach_frame(mailbox, key, frame);
                }
            }
            // ⚠ THE ATTACH PUSH IS THE ONE BOOK PRODUCER THAT CANNOT TEST MEMBERSHIP WHERE IT
            // PUSHES, so it tests it AFTERWARDS. `push_attach_frame` SERIALIZES (`frame_bytes`), and
            // serializing up to `MD_MAX_SPECS_PER_SESSION` ceiling-depth snapshots while holding the
            // session table would block the publisher and every `acquire` for the duration — §6.1's
            // rule, and the reason `MdHub::fanout` can afford the lock and this loop cannot.
            //
            // The hole it leaves is the same one `fanout` closes for the publisher, reached from a
            // second `MdUpdate` on THIS session (each arrives on its own short-lived connection and
            // is dispatched on its own thread, so a client may race its own two requests): that one
            // removes the key and reclaims the slot with `Mailbox::drop_book` while this one is
            // mid-attach, and our push then re-creates it for a key the session no longer holds —
            // unevictable, against the bound `MD_MAILBOX_BYTES`' assertion rests on.
            //
            // A sweep AFTER the pushes closes every interleaving of the two, which a re-check
            // BEFORE them would not. Write P for our push, S for this sweep, R for their
            // `keys.remove` and D for their `drop_book`; program order is P<S and R<D, and the six
            // orders are P S R D | P R S D | P R D S | R P S D | R P D S | R D P S. D reclaims in
            // four of them and S in the other two, so no order ends holding the slot. A key that a
            // third request legitimately RE-ADDS before S reads the table is held, so S leaves it —
            // which is right: it has a subscriber again.
            let unheld: Vec<MdKey> = {
                let g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
                match g.get(&id) {
                    Some(s) => {
                        attach.iter().filter(|k| !s.keys.contains_key(*k)).cloned().collect()
                    }
                    // The session ended under us; its mailbox is closed and nothing will drain it.
                    None => attach.iter().cloned().collect(),
                }
            };
            for key in &unheld {
                mailbox.drop_book(key);
            }
        }
        self.poke();
        (accepted, refused, released)
    }
}
