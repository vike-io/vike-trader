//! `MdSession`'s methods: the frame-thread surface, the status lines and the frame router.

use super::wire::ReaderState;
use super::*;

impl MdSession {
    // ── the FRAME-THREAD surface: every method here is wait-free ─────────────────────────────────

    /// Point (or re-point) the session at a datahub address. Called once per frame by the shell
    /// from the resolution it already computes; a value equal to the current one is a no-op, so the
    /// per-frame cost is one `Mutex` and a compare.
    ///
    /// ⚠ **A change to a DIFFERENT `Some` tears the stream down and applies §7.3 rule 4's bracket**
    /// (forget every book, forget every tape position, disclose) rather than carrying state across.
    /// That contradicts [`backend_conn`](crate::backend::backend_conn)'s standing claim that "a switch leaves
    /// this plane running and painted" — a claim that was true while the feed plane was venue-direct
    /// and stops being true now the plane is sourced through the backend's ADVERTISED datahub. The
    /// conservative reading is the correct one: a different server is a different book.
    ///
    /// ⚠ **`None` is NOT a different server — it is "no new answer", and the session HOLDS what it
    /// has.** `crates/vike-app-core/src/data/datahub_resolve.rs`'s second rung is the active backend's
    /// advertisement, and [`observe_bridge`](crate::backend::observe_bridge)'s reconnect path CLEARS that
    /// cell the moment the link drops, restoring it only after the next handshake a `DIAL_BACKOFF`
    /// later. On the documented default box — no explicit `config.datahub_addr`, the advertisement
    /// IS the whole address ladder — every tradehub blip the bridge is designed to absorb
    /// transparently would otherwise arrive here as a re-point: stream torn down, [`Caps`] thrown
    /// away (the exact churn that type's doc argues against), every permanent refusal re-opened, and
    /// every venue's line rewritten to name a `config.datahub_addr` misconfiguration that does not
    /// exist. A datahub is a DIFFERENT PROCESS from the tradehub that advertised it and did not go
    /// anywhere. The cost of holding is narrow and stated: an operator who deletes
    /// `config.datahub_addr` on a box with no advertisement keeps streaming from the server already
    /// connected until the process restarts — which is data that is real, from a server that is up.
    pub fn set_addr(&self, addr: Option<&str>) {
        let next = addr.map(str::to_string);
        {
            let mut cur = lock(&self.addr);
            if *cur == next {
                return;
            }
            *cur = next;
        }
        self.poke();
    }

    /// Point the session at a datahub observe key NAME — [`crate::backend::backend_registry::
    /// datahub_observe_key_name`] of the ACTIVE backend record, pushed once per frame beside
    /// [`MdSession::set_addr`] for the same reason: it is a property of a record the shell switches
    /// at runtime, and a session that took it once at `App::new` would sign the NEW backend's
    /// datahub with the PREVIOUS record's key — `bad mac` at the handshake with nothing on screen
    /// that could explain it, which is the exact symptom
    /// `crates/vike-app-core/src/backend/backend_editor_tests.rs`'s
    /// `an_edit_preserves_the_datahub_key_name_the_form_does_not_carry` names.
    ///
    /// Wait-free: a compare and a store. RESOLVING the name opens a file, so it happens on the
    /// reconciler (`resolve_keys_if_renamed`) and never here.
    pub fn set_key_name(&self, name: &str) {
        {
            let mut cur = lock(&self.key_name);
            if *cur == name {
                return;
            }
            *cur = name.to_string();
        }
        self.poke();
    }

    /// This `(venue, symbol)`'s tape-gap epoch — a monotone counter the reader bumps on every
    /// disclosed hole, and [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core)'s only
    /// route to the aggregators, which live on the FRAME thread where the reader cannot reach them.
    ///
    /// ⚠ **The fold calls this AFTER its drain, per key, and that is why it is a CALL rather than a
    /// map.** It was a `HashMap` snapshot taken as an argument expression, i.e. strictly BEFORE
    /// `sync_from_core` was entered, while the drain it is meant to be read after happens hundreds
    /// of lines inside — so the rule the fold asserts in three places did not hold, and the whole
    /// prologue of that fold was a window in which a gap could be disclosed, its buffered ticks
    /// drained by the reader, and the POST-hole batch then folded into an unrepaired aggregator.
    /// The next frame repairs, but [`TickVolAgg::mark_gap`](vike_orderflow::tickvol::TickVolAgg::mark_gap)
    /// keeps `closed` whole on the argument that every bar in it closed BEFORE the hole — so a
    /// `100t` bar made of 60 pre-hole and 40 post-hole prints is permanent and unmarked.
    #[must_use]
    pub fn tape_gap_epoch(&self, venue: &str, symbol: &str) -> u64 {
        lock(&self.gap_epochs).get(&(venue.to_string(), symbol.to_string())).copied().unwrap_or(0)
    }

    /// Raise the stop flags and RETURN — the `begin_shutdown` half of the two-phase teardown.
    /// Shutting the socket is what returns the reader from its blocking read AT ONCE instead of at
    /// the 45 s deadline, which is the whole reason this phase exists. Idempotent, and safe to call
    /// concurrently from every feed — which is exactly what `App::run_bounded_teardown` does.
    pub fn begin_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = lock(&self.stream_handle).as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        self.poke();
    }

    /// [`MdSession::begin_stop`] plus a JOIN of both threads. ⚠ `App::run_bounded_teardown` fans
    /// `DataClient::shutdown` out in PARALLEL over every feed, so this is called N times at once on
    /// ONE session: the handles are `take`n under a lock, so exactly one caller joins each thread
    /// and the rest return immediately. An unguarded double join would panic five times out of six.
    pub fn stop_and_join(&self) {
        self.begin_stop();
        let reader = lock(&self.reader).take();
        let reconciler = lock(&self.reconciler).take();
        if let Some(h) = reader {
            let _ = h.join();
        }
        if let Some(h) = reconciler {
            let _ = h.join();
        }
    }

    /// The cached advertisement, or `None` before any handshake has landed (or right after an
    /// address change, until the new server has answered).
    pub fn caps(&self) -> Option<Caps> {
        lock(&self.caps).clone()
    }

    /// The link half of the status lines as the reconciler last wrote it — `idle`, `datahub <addr>`,
    /// `reconnecting (…)`, `datahub <addr> error: … (retrying in Ns)` — and, with NO venue mounted,
    /// the only place it is written down at all. A datahub that advertises nothing mounts no feed and
    /// so has no per-venue line to say why; this is that sentence, naming the fix.
    #[must_use]
    pub fn link_status(&self) -> String {
        lock(&self.link).clone()
    }

    /// Mount the venues the connected datahub advertises, and unmount the ones it stopped
    /// advertising: add one [`DatahubFeed`](crate::data::datahub_feed::DatahubFeed) and one status
    /// line per newly advertised roster venue to the shell's maps, drop the ones the server no longer
    /// serves. Called once per frame beside [`MdSession::set_addr`]; the steady state is two short
    /// lock holds and a scan of at most one roster's worth of slugs, with no allocation.
    ///
    /// * **Nothing happens until the first advertisement lands** ([`MdSession::caps`] is `None`):
    ///   that is also the state right after an address change, and clearing then would flap every
    ///   idle feed across a backend switch. The module doc has the argument.
    /// * **A server with no `market_data` plane advertises no venue**, whatever `md_venue=` entries
    ///   it carries: they name feeds it will not serve.
    /// * **A feed some key still holds is KEPT** even when the server no longer advertises its venue
    ///   (`App`'s subscription ledgers unsubscribe through `App::feeds`); it goes on a later call,
    ///   once the last holder released it. A venue's `want`s are refused locally in the meantime.
    ///
    /// ⚠ Takes `&Arc<Self>` because each mounted feed holds a clone of the session. Every status
    /// line is rendered from the CURRENT state the moment it is mounted, so a venue mounted while a
    /// link error stands reads that error rather than an invented `connecting`.
    pub fn sync_mount(
        self: &Arc<Self>,
        feeds: &mut FeedMap,
        statuses: &mut HashMap<String, Arc<Mutex<String>>>,
    ) {
        let (mount, unmount): (Vec<&'static str>, Vec<&'static str>) = {
            let desired = lock(&self.desired);
            let caps = lock(&self.caps);
            let Some(caps) = caps.as_ref() else { return };
            let mount = if caps.market_data {
                caps.venues
                    .iter()
                    .filter_map(|v| vike_model::venues::roster_id(v))
                    .filter(|v| !feeds.contains_key(*v))
                    .collect()
            } else {
                Vec::new()
            };
            let unmount = feeds
                .keys()
                .copied()
                .filter(|v| !(caps.market_data && caps.serves(v)))
                .filter(|v| !desired.iter().any(|w| w.spec.venue == *v))
                .collect();
            (mount, unmount)
        };
        if mount.is_empty() && unmount.is_empty() {
            return;
        }
        for venue in unmount {
            feeds.remove(venue);
            statuses.remove(venue);
            lock(&self.mounted).remove(venue);
        }
        for venue in mount {
            // A repeated slug in one advertisement is mounted once.
            if feeds.contains_key(venue) {
                continue;
            }
            let line = Arc::new(Mutex::new(String::new()));
            lock(&self.mounted).insert(venue, line.clone());
            statuses.insert(venue.to_string(), line);
            let feed: Box<dyn vike_data::DataClient + Send> =
                Box::new(crate::data::datahub_feed::DatahubFeed::new(venue, Arc::clone(self)));
            feeds.insert(venue, feed);
        }
        self.restate_statuses();
    }

    /// Record the intent to stream one key, and answer the SYNCHRONOUS refusal ladder.
    ///
    /// ⚠ **The intent is recorded BEFORE anything is sent** — structurally, because this method only
    /// ever touches `desired` and the reconciler is the only thing that sends. Frames for an added
    /// key can arrive on the stream socket before the ack arrives on the control socket, which is
    /// the same ordering rule `crates/vike-recorder/src/runtime.rs` states as *"membership is
    /// published BEFORE the subscription is made"*. Here it cannot be got wrong: routing is by
    /// `(venue, symbol)` straight into the shared stores and consults no intent ledger at all.
    pub(crate) fn want(
        &self,
        venue: &'static str,
        symbol: &str,
        lane: MdLane,
    ) -> Result<SubscriptionId, LiveDataError> {
        // 1. The STATIC capability matrix — the same authority the server consults, so a venue that
        //    declares no such lane is refused here with no wire traffic at all.
        vike_data::require_live_verb(venue, lane.live_verb())?;
        // 2. The cached advertisement. `None` (nothing has handshaked yet) accepts OPTIMISTICALLY:
        //    blocking here is forbidden, and a premature `Unsupported` would be recorded by
        //    `FeedRetries` as PERMANENT and never re-asked.
        if let Some(caps) = self.caps() {
            if !caps.market_data {
                return Err(LiveDataError::Unsupported(NO_MD_PLANE));
            }
            if !caps.serves(venue) {
                return Err(LiveDataError::Unsupported(NOT_ADVERTISED));
            }
        }
        // 3. A permanent refusal this session has already SEEN for this exact key.
        if lock(&self.refused)
            .iter()
            .any(|(s, _)| s.venue == venue && s.symbol == symbol && s.lane == lane)
        {
            return Err(LiveDataError::Unsupported(SERVER_REFUSED));
        }
        let spec = MdSpec {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            lane,
            // `None` = the server's own `MD_DEPTH_LEVELS_DEFAULT` (50/side). Asking for more would
            // inflate the frame for EVERY subscriber of that key — the server resolves the depth as
            // the max over subscribers — for rows the ladder does not paint.
            depth_levels: None,
        };
        {
            let mut desired = lock(&self.desired);
            match desired.iter_mut().find(|w| w.spec.key() == spec.key()) {
                Some(w) => w.holders = w.holders.saturating_add(1),
                None => desired.push(Wanted { spec, holders: 1 }),
            }
        }
        self.poke();
        Ok(SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed)))
    }

    /// Drop one holder of a key; the key leaves the desired set when the last one goes. No I/O.
    pub(crate) fn unwant(&self, spec: &MdSpec) {
        let mut changed = false;
        {
            let mut desired = lock(&self.desired);
            if let Some(i) = desired.iter().position(|w| w.spec.key() == spec.key()) {
                desired[i].holders = desired[i].holders.saturating_sub(1);
                if desired[i].holders == 0 {
                    desired.remove(i);
                    changed = true;
                }
            }
        }
        if changed {
            self.poke();
        }
    }

    // ── internals ───────────────────────────────────────────────────────────────────────────────

    pub(super) fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub(super) fn poke(&self) {
        {
            let mut g = lock(&self.poke_gen);
            *g = g.wrapping_add(1);
        }
        self.cv.notify_all();
    }

    pub(super) fn poke_generation(&self) -> u64 {
        *lock(&self.poke_gen)
    }

    /// Park until poked, or for `at_most`. `seen` is the generation sampled BEFORE this iteration
    /// read any state: if it has moved, something changed while we worked and we must not sleep.
    pub(super) fn park(&self, seen: u64, at_most: Duration) {
        let g = lock(&self.poke_gen);
        if *g != seen {
            return;
        }
        let _ = self.cv.wait_timeout(g, at_most);
    }

    /// Write `text` on every mounted line AND as the link ([`MdSession::link_status`]): with no venue
    /// mounted there is no line to carry it, and a line mounted later is rendered from the link.
    pub(super) fn set_all_statuses(&self, text: &str) {
        *lock(&self.link) = text.to_string();
        for h in lock(&self.mounted).values() {
            *lock(h) = text.to_string();
        }
    }

    /// Whether any advertisement has been cached — [`MdSession::caps`] without the clone, for the
    /// reconciler's per-pass "do I owe a probe" question.
    pub(super) fn has_caps(&self) -> bool {
        lock(&self.caps).is_some()
    }

    /// The link text for a pass with NOTHING wanted: `idle`, unless the cached advertisement leaves
    /// nothing to mount ([`Caps::nothing_to_mount`]), in which case the sentence that names the fix.
    /// ⚠ The `error:` prefix is not decoration — see `refresh_statuses`: the sentence carries `live`.
    pub(super) fn idle_link(&self, addr: &str) -> String {
        match self.caps().as_ref().and_then(Caps::nothing_to_mount) {
            Some(why) => format!("datahub {addr} error: {why}"),
            None => "idle".to_string(),
        }
    }

    /// Re-render every venue's status line from the CURRENT desired/served/refused/gapped state.
    /// One place, so a line can never describe a state two frames old.
    ///
    /// ⚠ **THESE STRINGS ARE PARSED, NOT DISPLAYED, AND FOUR WORDS ARE LOAD-BEARING, NOT ONE.**
    /// `vike_model::feed_status::parse_feed_status` is an ORDERED substring classifier — it tests
    /// `disconnected`, then `fault|error|failed`, then **`connected` | `live` | `streaming` |
    /// `subscribed`**, then `connect*` — and the Connections tool renders whatever it answers. So
    /// NONE of those four may appear on a line that is not serving, and this was got wrong twice by
    /// guarding `live` alone:
    ///
    /// * `{link} — nothing subscribed on this venue` contains **`subscribed`** and therefore read
    ///   **Connected**. That is not an edge case, it is the steady state: `reconcile_loop` renders
    ///   it through `refresh_statuses("idle")` on the `desired_specs().is_empty()` branch, which
    ///   every venue is in from the first frame an address resolves until a Trade window or chart
    ///   opens on it. At launch EVERY mounted slug read Connected with no socket open,
    ///   and after one binance ladder opens the other five stay that way for the session. Note that
    ///   rung 1's `idle` test is `==`, not `contains`, so prefixing the link does not save it.
    /// * the refusal arm interpolated `{why:?}`, and `MdRefusal::LaneUnsupported` carries
    ///   `vike_data::require_live_verb`'s own words — every one of which contains
    ///   `VenueCaps.live_data`, i.e. **`live`**. So a refused venue read Connected too, and the arm
    ///   OUTRANKED every other, so it kept reading Connected while the link was down.
    ///
    /// The rule this file now follows: **no uncontrolled text is interpolated into a parsed
    /// string.** A refusal renders through [`refusal_label`], a keyword-free `&'static str` per
    /// variant; the server's own words go to the `tracing::warn!` `absorb_refusals` already emits.
    ///
    /// ⚠ `served` means SUBSCRIBED, not FLOWING — the last arm's `live` count therefore subtracts
    /// the book keys `gapped` holds, or a venue whose ladder this client has just emptied would
    /// still read `1/1 stream(s) live`.
    pub(super) fn refresh_statuses(&self, link: &str) {
        *lock(&self.link) = link.to_string();
        let desired = lock(&self.desired);
        let served = lock(&self.served);
        let refused = lock(&self.refused);
        let gapped = lock(&self.gapped);
        let mounted = lock(&self.mounted);
        for (venue, handle) in mounted.iter() {
            let wanted = desired.iter().filter(|w| w.spec.venue == *venue).count();
            let serving = served.iter().filter(|s| s.venue == *venue);
            let mut live = 0usize;
            let mut stalled = 0usize;
            for s in serving {
                let book_lane = matches!(s.lane, MdLane::Depth | MdLane::Book);
                if book_lane && gapped.contains(&(s.venue.clone(), s.symbol.clone())) {
                    stalled += 1;
                } else {
                    live += 1;
                }
            }
            let refusal = refused.iter().find(|(s, _)| s.venue == *venue);
            let text = match (wanted, live, refusal) {
                // A venue with streams actually flowing reads Connected even if ANOTHER of its keys
                // was refused: the refusal is disclosed in the log and in the count, and reporting
                // a working venue as not-working is the worse error. This arm is the ONLY one that
                // may carry a rung-3 keyword.
                (_, 1.., _) => format!("{link} — {live}/{wanted} stream(s) live"),
                // Refused and serving nothing. No rung-3 keyword at all, so this reads Unknown.
                // Deliberate — the LINK is fine and the DATA is not flowing, and
                // `parse_feed_status` has no rung that says that. Error would blame the transport.
                (_, 0, Some((s, why))) => {
                    format!("{link} — refused {}/{:?} ({})", s.symbol, s.lane, refusal_label(why))
                }
                // Nothing wanted here. ⚠ NOT "nothing subscribed" — see this function's doc.
                (0, 0, None) => format!("{link} — no streams wanted on this venue"),
                // Wanted but not yet serving. This arm carries `connecting`, so it lands on rung 4
                // whatever `{link}` says, and adds no rung-3 keyword to outrank it.
                //
                // ⚠ `stalled` rides HERE rather than in an arm of its own, and the reason is that
                // the two states are indistinguishable on screen and nearly so on the wire: a key
                // whose book was dropped by a disclosed `GapStart` and a key that has not received
                // its first snapshot both show a BLANK ladder, and `MdHub::attach_frames` hands
                // every brand-new subscriber `GapStart{now_ms}` as the honest "wanted, not yet
                // live". Rendering the first as a `fault` would therefore paint an Error dot over
                // an ordinary subscribe. What the operator needs is the half that WAS being got
                // wrong — that this venue is not flowing — and that is now true on both paths.
                (_, 0, None) => {
                    let why = if stalled > 0 { ", the venue feed has sent no book" } else { "" };
                    format!("{link} — connecting, 0/{wanted} stream(s){why}")
                }
            };
            *lock(handle) = text;
        }
    }

    /// [`MdSession::refresh_statuses`] against the link text it last wrote — what the READER calls
    /// when a health change (not a subscription change) has moved what a line should say.
    fn restate_statuses(&self) {
        let link = lock(&self.link).clone();
        self.refresh_statuses(&link);
    }

    /// `add`/`remove`, computed against the server's authoritative `served` set.
    ///
    /// The two rules that matter, both `SubscriptionSet::reconcile`'s: **stop what left and start
    /// what arrived, and leave survivors strictly alone** (a survivor re-subscribed would drop the
    /// server's book state for a key somebody is looking at).
    pub(super) fn diff(&self) -> (Vec<MdSpec>, Vec<MdSpec>) {
        let desired = lock(&self.desired);
        let served = lock(&self.served);
        let add: Vec<MdSpec> = desired
            .iter()
            .filter(|w| !served.iter().any(|s| s.key() == w.spec.key()))
            .map(|w| w.spec.clone())
            .collect();
        let remove: Vec<MdSpec> = served
            .iter()
            .filter(|s| !desired.iter().any(|w| w.spec.key() == s.key()))
            .cloned()
            .collect();
        (add, remove)
    }

    pub(super) fn desired_specs(&self) -> Vec<MdSpec> {
        lock(&self.desired).iter().map(|w| w.spec.clone()).collect()
    }

    /// Fold one `refused` list from the wire: a PERMANENT refusal is recorded and leaves the desired
    /// set (nothing can make it succeed); a CAP refusal stays wanted and is retried on backoff,
    /// because a cap frees up when another window closes.
    pub(super) fn absorb_refusals(&self, refused: Vec<(MdSpec, MdRefusal)>) {
        for (spec, why) in refused {
            if why.is_permanent() {
                tracing::warn!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    lane = ?spec.lane,
                    "datahub market-data: permanently refused — {why:?}"
                );
                lock(&self.desired).retain(|w| w.spec.key() != spec.key());
                let mut refused_now = lock(&self.refused);
                if !refused_now.iter().any(|(s, _)| s.key() == spec.key()) {
                    refused_now.push((spec, why));
                }
            } else {
                tracing::info!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    "datahub market-data: capped for now, will retry — {why:?}"
                );
            }
        }
    }

    /// Bump a key's tape epoch — the reader's only way to reach the frame thread's aggregators.
    pub(super) fn bump_gap(&self, venue: &str, symbol: &str) {
        let mut g = lock(&self.gap_epochs);
        let e = g.entry((venue.to_string(), symbol.to_string())).or_insert(0);
        *e = e.saturating_add(1);
    }

    /// §7.3 rule 4's OPENING bracket, synthesized locally because the server that would have sent
    /// it is the one that went away: forget every book and every tape position this connection
    /// held, and disclose it. Never resume a book or a tape across a socket.
    pub(super) fn open_gap_bracket(&self, why: &str) {
        let now = vike_model::now_ms();
        let held: Vec<MdSpec> = lock(&self.served).clone();
        for spec in &held {
            match spec.lane {
                MdLane::Depth | MdLane::Book => {
                    self.sink.books.remove(&spec.venue, &spec.symbol);
                }
                MdLane::Trades => self.bump_gap(&spec.venue, &spec.symbol),
            }
            self.sink.stream_status(
                &spec.venue,
                &spec.symbol,
                spec.lane.feed_stream_label(),
                vike_model::StreamStatus::GapStart { at_ts_ms: now },
            );
        }
        if !held.is_empty() {
            tracing::warn!(
                keys = held.len(),
                "datahub market-data stream ended ({why}) — every book dropped and every tape \
                 position forgotten; nothing is resumed across a socket"
            );
        }
    }

    /// Mark (or unmark) one BOOK key as not-flowing and re-render the venue lines if that CHANGED.
    /// Returns nothing: the only observable is the status string, and the re-render is conditional
    /// precisely so an ordinary snapshot — which arrives at the hub's publish cadence — does not
    /// take five mutexes and rewrite six strings on every frame.
    fn note_book_health(&self, venue: &str, symbol: &str, flowing: bool) {
        let key = (venue.to_string(), symbol.to_string());
        let changed = {
            let mut g = lock(&self.gapped);
            if flowing { g.remove(&key) } else { g.insert(key) }
        };
        if changed {
            self.restate_statuses();
        }
    }

    /// Route one pushed frame. Returns `false` when the stream is over.
    pub(super) fn apply_frame(&self, frame: MdFrame, st: &mut ReaderState) -> bool {
        let ReaderState { last_seq, disclosed } = st;
        match frame {
            // Both lanes go through `l2_snapshot`, never `book`: the wire carries RAW LEVELS plus
            // the feed's authoritative `tick_size`, and `BookStore::update` is exactly the rebuild
            // path for those (`insert_book` wants an already-folded `L2Book`). The lanes differ
            // only by NAME, which is precisely the property `MdLane`'s split asserts.
            MdFrame::Depth(s) | MdFrame::Book(s) => {
                // A snapshot IS the flow signal for this key — see `MdSession::gapped` for why a
                // `Status::Live` cannot be waited for instead.
                self.note_book_health(&s.venue, &s.symbol, true);
                self.sink.l2_snapshot(
                    &s.venue,
                    &s.symbol,
                    s.tick_size,
                    s.bids,
                    s.asks,
                    // §7.4 — the LOCAL clock. `venue_ts`/`venue_seq` are diagnostics.
                    vike_model::now_ms(),
                );
                (self.wake)();
            }
            MdFrame::Trades { venue, symbol, ticks, seq } => {
                // §7.2's backstop: a tape `seq` jump means this connection did not receive frames
                // the server produced, so the hole is treated as a `TapeGap` of the implied size
                // rather than folded. (On the book lanes a jump is informational: the lane is
                // conflating by contract and each frame is a self-healing whole snapshot.)
                //
                // ⚠ **THE DIAGNOSIS SPLITS ON WHETHER THIS KEY HAS ALREADY BEEN DISCLOSED ON THIS
                // CONNECTION, and reading every jump as a server bug was a FALSE ACCUSATION on the
                // server's own documented path.** `crates/vike-datahub/src/md/mailbox.rs`'s
                // `Inner::owe` merges two holes for one key by taking the MINIMUM `resume_at` — so
                // the marker lands at the EARLIEST hole and `dropped` is the total, and its own doc
                // states the consequence as intended: "a later hole inside the same disclosure
                // shows up as a §7.2 `seq` jump rather than a second marker — which is exactly what
                // that backstop is for". A mailbox drop implies a backlog by definition, so that is
                // the ORDINARY slow-consumer case, not an edge. Logging it at `error!` as a SERVER
                // bug clears the default `info` console filter and sends an operator hunting a
                // defect that does not exist. The `bump_gap` is identical on both paths; only the
                // words differ, and §7.2's actual claim is the undisclosed one.
                let key = (venue.clone(), symbol.clone());
                if let Some(prev) = last_seq.get(&key).copied()
                    && seq > prev.saturating_add(1)
                {
                    if disclosed.contains(&key) {
                        tracing::warn!(
                            venue = %venue, symbol = %symbol, from_seq = prev, to_seq = seq,
                            "datahub tape sequence jumped after a TapeGap on this key — a later \
                             hole inside a MERGED disclosure (the server's `Inner::owe` discloses \
                             at the earliest hole, by design); repairing again"
                        );
                    } else {
                        tracing::error!(
                            venue = %venue, symbol = %symbol, from_seq = prev, to_seq = seq,
                            "datahub tape sequence jumped with NO TapeGap on this key — that is a \
                             SERVER bug; treating it as a gap of the implied size rather than \
                             folding a hole"
                        );
                    }
                    self.bump_gap(&venue, &symbol);
                }
                last_seq.insert(key, seq);
                for mut tick in ticks {
                    // ⚠⚠ THE RE-STAMP. The hub STRIPS this field (`md/hub.rs`'s `push_trade`) and
                    // the envelope carries it once; `GuiFeedSink::trade` keys `TradeStore` by
                    // `tick.symbol`, so an un-restamped tick lands under `(venue, "")` and every
                    // tick/volume and orderflow chart stays empty for ever, silently. See this
                    // module's doc.
                    tick.symbol.clone_from(&symbol);
                    self.sink.trade(&venue, &symbol, tick);
                }
                (self.wake)();
            }
            MdFrame::Status { venue, symbol, lane, status } => {
                let broken = matches!(
                    status,
                    vike_model::StreamStatus::GapStart { .. }
                        | vike_model::StreamStatus::Stale { .. }
                );
                match lane {
                    // §7.3 rules 1 and 2: a book lane that has gapped or gone stale must be
                    // FORGOTTEN, not left frozen on screen. `Live` needs no action for the STORE —
                    // the next snapshot repopulates — but it does un-mark the key, so a venue that
                    // recovers before its next publish tick stops reading as not-flowing.
                    MdLane::Depth | MdLane::Book => {
                        if broken {
                            self.sink.books.remove(&venue, &symbol);
                        }
                        self.note_book_health(&venue, &symbol, !broken);
                    }
                    // ⚠ **THE TAPE GETS THE SYMMETRIC TREATMENT, and §7.3 rule 1 names only the
                    // book lanes — so this is a deliberate WIDENING, flagged rather than silent.**
                    // A venue-side hole in a trade tape is prints that were genuinely lost at the
                    // venue, and NOTHING ELSE discloses it: it increments no `entry.dropped`, so the
                    // server emits no `TapeGap`; the wire `seq` counts published FRAMES rather than
                    // venue ticks, so §7.2's backstop sees nothing either. Forwarding the notice and
                    // folding on would leave `OrderflowAgg::ingest` — which that method's own doc
                    // calls out as having no per-trade dedup and no gap concept — understating that
                    // bar's cells and every CVD value after it, permanently and undisclosed, which
                    // §6.2's "Loss here must be impossible to hide" forbids. It is reachable today
                    // rather than latent: `crates/bridges/polymarket/src/market_feed/state.rs`'s
                    // `PumpMode::Trades` renders `stream_label` as `"trades"`, and both its recovery
                    // and freshness paths call `sink.stream_status` with it.
                    //
                    // `bump_gap` is idempotent per epoch VALUE at the fold, and the producer-side
                    // `StreamHealth` is edge-triggered (one `Stale` per episode), so a repeated
                    // disclosure costs one rebuild, not one per frame.
                    MdLane::Trades => {
                        if broken {
                            let _ = self.sink.trades.drain(&venue, &symbol);
                            self.bump_gap(&venue, &symbol);
                            disclosed.insert((venue.clone(), symbol.clone()));
                        }
                    }
                }
                self.sink.stream_status(&venue, &symbol, lane.feed_stream_label(), status);
                (self.wake)();
            }
            MdFrame::TapeGap { venue, symbol, dropped, from_seq, to_seq } => {
                // ORDER MATTERS: discard what is buffered, THEN bump the epoch. The frame thread
                // reads the epoch after ITS drain, so the two together mean "everything that was
                // in flight for this key at the moment of the hole is gone".
                let _ = self.sink.trades.drain(&venue, &symbol);
                self.bump_gap(&venue, &symbol);
                last_seq.insert((venue.clone(), symbol.clone()), to_seq);
                // §7.2's backstop reads this: a LATER jump on a key already disclosed is the
                // server's merged-`owe` path, not a server bug. See the `Trades` arm.
                disclosed.insert((venue.clone(), symbol.clone()));
                tracing::warn!(
                    venue = %venue, symbol = %symbol, dropped, from_seq, to_seq,
                    "datahub tape gap disclosed — this key's aggregators are rebuilt from empty; \
                     the missing prints come from the store, never from this wire"
                );
                (self.wake)();
            }
            // The READ itself is the liveness proof; nothing else to do, and deliberately no wake —
            // a heartbeat changes no pixel, and it must never refresh a book's staleness receipt.
            MdFrame::Heartbeat => {}
            MdFrame::Bye(why) => {
                match why {
                    MdBye::TooSlow { lapses } => tracing::warn!(
                        lapses,
                        "datahub ended the market-data stream: this client fell behind. It will \
                         re-dial; if this repeats, fewer open ladders or a faster link is the fix"
                    ),
                    MdBye::ServerStopping => {
                        tracing::info!("datahub is stopping — market-data stream closed")
                    }
                    MdBye::SessionIdle => {
                        tracing::info!("datahub closed an idle market-data session")
                    }
                    // ⚠ A DISTINCT diagnosis from `SessionIdle`, and the server branch added the
                    // variant precisely because its writer used to send that one here. They are
                    // OPPOSITE findings — idle means this session asked for nothing, this means it
                    // asked for more than it could read — and once the socket is gone the reason is
                    // the only thing anybody has to go on. So it is logged as the slow-consumer
                    // fault it is, at `warn!` beside `TooSlow`, rather than at `info!` beside the
                    // two orderly closes.
                    MdBye::ControlLaneOverflow => tracing::warn!(
                        "datahub ended the market-data stream: this client could not absorb even \
                         the status frames its own keys produced. It will re-dial; if this \
                         repeats, fewer open ladders or a faster link is the fix"
                    ),
                }
                return false;
            }
        }
        true
    }
}
