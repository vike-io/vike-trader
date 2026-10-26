//! The VENUE side of the hub: drive every venue client to exactly the desired set, and wait for the
//! poke that says the desired set moved. [`MdHub::reconcile`] is **the only place a `DataClient`
//! method is called** (the parent module's doc is the argument), and it runs on the one thread the
//! parent's `spawn` starts for it — so nothing a venue does can stall a request path.
//!
//! The signalling half it waits on (`poke`, which `subscribe.rs` and `release.rs` call) stays in
//! the parent module beside the `wake` field it writes.

use super::*;

impl MdHub {
    /// Drive every venue client to exactly the DESIRED set — the ONLY place a `DataClient` method is
    /// called. See the module doc for why this replaces §5.2 step 4 and §5.3's janitor.
    pub fn reconcile(&self, now_ms: i64) -> ReconcileReport {
        let mut report = ReconcileReport::default();
        let entries = self.snapshot();

        let wanted = |e: &StreamEntry| -> bool {
            if e.is_resident() || e.held() > 0 {
                return true;
            }
            let z = e.zero_since.load(Ordering::Acquire);
            z >= 0 && now_ms < z.saturating_add(MD_LINGER.as_millis() as i64)
        };

        let mut clients = self.venues.lock().unwrap_or_else(PoisonError::into_inner);

        // -- phase 1: START what arrived -----------------------------------------------------
        for entry in &entries {
            if !wanted(entry.as_ref()) {
                continue;
            }
            if entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
                continue; // a SURVIVOR: leave it strictly alone.
            }
            let venue = entry.key.venue.clone();
            if !clients.contains_key(&venue) {
                match (self.builder)(&venue, self.sink()) {
                    Ok(c) => {
                        clients.insert(venue.clone(), c);
                    }
                    Err(e) => {
                        report.failed.push(format!("{venue}: {e}"));
                        continue;
                    }
                }
            }
            let client = clients.get_mut(&venue).expect("just inserted");
            let started = match entry.key.lane {
                MdLane::Depth => client.subscribe_depth(&entry.key.symbol),
                MdLane::Book => client.subscribe_book(&entry.key.symbol),
                MdLane::Trades => client.subscribe_trades(&entry.key.symbol),
            };
            match started {
                Ok(id) => {
                    *entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner) = Some(id);
                    report.started += 1;
                }
                Err(e) => {
                    // ⚠ NO phantom refcount and NO phantom sub_id: the key stays WANTED with
                    // `sub_id = None`, which is the `GapStart` state a client already understands,
                    // and the next pass retries it. A failure here must never look like a live
                    // subscription — that is §6.1's "connects, reports healthy, delivers nothing".
                    report.failed.push(format!("{}/{}: {e}", entry.key.venue, entry.key.symbol));
                }
            }
        }

        // -- phase 2: STOP what left ---------------------------------------------------------
        let mut reap: HashMap<String, Vec<(Arc<StreamEntry>, SubscriptionId)>> = HashMap::new();
        let mut live_per_venue: HashMap<String, usize> = HashMap::new();
        for entry in &entries {
            if entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
                *live_per_venue.entry(entry.key.venue.clone()).or_default() += 1;
            }
        }
        for entry in &entries {
            if wanted(entry.as_ref()) {
                continue;
            }
            let id = *entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(id) = id {
                reap.entry(entry.key.venue.clone()).or_default().push((Arc::clone(entry), id));
            }
        }
        for (venue, victims) in reap {
            let whole = live_per_venue.get(&venue).copied().unwrap_or(0) == victims.len();
            if let Some(client) = clients.get_mut(&venue) {
                if whole {
                    // ⚠ The two-phase idiom is legitimate ONLY here: `begin_shutdown` is
                    // `FeedRegistry::raise_stops`, which raises EVERY flag this client owns, so it
                    // is correct exactly when the reap set IS the client's whole live set. This is
                    // also the only place the raise-all-then-join win is available.
                    client.begin_shutdown();
                    client.shutdown();
                } else {
                    // A partial reap: per-key `unsubscribe` only. One socket read timeout per key,
                    // on THIS thread — off every serving path, and invisible to every client.
                    for (_, id) in &victims {
                        client.unsubscribe(*id);
                    }
                }
            }
            if whole {
                clients.remove(&venue);
                report.clients_dropped += 1;
            }
            for (entry, _) in &victims {
                *entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
            report.stopped += victims.len();
        }
        drop(clients);

        // -- phase 3: forget the reaped entries -----------------------------------------------
        //
        // ⚠ **THE DECISION IS RE-TAKEN UNDER THE WRITE LOCK, AND THE SLOT IS IDENTITY-CHECKED.**
        // Deciding outside it and deleting by KEY inside it is a real race, not a theoretical one:
        // `acquire` publishes its refcount AFTER it releases `keys.write` (it must — `settle_depth`
        // and the session bookkeeping run under `sessions`, not `keys`), so a filter that saw a key
        // unwanted, then blocked on this lock while `acquire` took it, cleared a slot a session now
        // holds. Every consequence was SILENT: `attach_frames` returns an empty vec for a missing
        // entry so the client is not even sent its `GapStart`; the sink's `lookup` finds nothing;
        // `publish_tick` never walks it; `release_key` returns early so the accounting never
        // unwinds; and because `state.keys` still names the key, a later `acquire` takes the
        // already-held branch, skips the `fetch_add`, and leaves the session holding a key against
        // an entry it holds no reference on.
        //
        // Re-evaluating `wanted` from the SLOT's current `Arc` closes the refcount race, and
        // `Arc::ptr_eq` closes the second one — a slot REPLACED between the two steps must not be
        // deleted on the strength of a verdict about the entry it replaced.
        //
        // ⚠ **The re-check is only half of the closure, and the other half is in `acquire`.** This
        // comment claimed the race closed while `acquire` still published its refcount and cleared
        // its linger deadline AFTER releasing this lock — so a re-check taken inside that window
        // reads the same stale `held() == 0` the filter did, and re-deciding changes nothing. Both
        // atomics now happen under `keys.write()` there, which is what makes "the decision is
        // re-taken under the write lock" a closure rather than a narrower window.
        let doomed: Vec<MdKey> =
            entries.iter().filter(|e| !wanted(e.as_ref())).map(|e| e.key.clone()).collect();
        if !doomed.is_empty() {
            let judged: HashMap<&MdKey, &Arc<StreamEntry>> =
                entries.iter().map(|e| (&e.key, e)).collect();
            let mut g = self.keys.write().unwrap_or_else(PoisonError::into_inner);
            for key in &doomed {
                if let Some(symbols) = g.get_mut(&key.venue) {
                    if let Some(slots) = symbols.get_mut(&key.symbol) {
                        let idx = lane_index(key.lane);
                        let still_doomed = match (&slots[idx], judged.get(key)) {
                            // `judged` holds BORROWED entries, so the pattern takes the inner
                            // reference out of the `&&Arc` `HashMap::get` hands back.
                            (Some(current), Some(&at_filter_time)) => {
                                Arc::ptr_eq(current, at_filter_time) && !wanted(current.as_ref())
                            }
                            _ => false,
                        };
                        if still_doomed {
                            slots[idx] = None;
                        }
                        if slots.iter().all(Option::is_none) {
                            symbols.remove(&key.symbol);
                        }
                    }
                    if symbols.is_empty() {
                        g.remove(&key.venue);
                    }
                }
            }
        }
        report
    }

    /// Wait up to `timeout` for an `acquire`/`release` poke. The reconcile thread's whole loop body
    /// besides [`MdHub::reconcile`].
    ///
    /// ⚠ **It CONSULTS the flag, and for a while it did not** — it went straight into
    /// `wait_timeout` and then cleared `*g`, so the boolean was written by [`MdHub::poke`] and read
    /// by nobody. A `notify_all` delivered while no thread was parked was simply discarded: the
    /// classic missed notification the flag exists to defeat.
    ///
    /// The lost window is not microseconds. [`MdHub::reconcile`] is by design the thread that
    /// performs every blocking venue call — a whole-venue reap JOINS feed threads, a partial one
    /// costs a socket read timeout per key — so an `MdSubscribe` arriving during a pass had all of
    /// its pokes swallowed and then waited out the pass PLUS a full [`crate::md::MD_REAP_INTERVAL`]
    /// before its venue subscription was even attempted. Since `run_market_writer` acquires each
    /// spec separately, a multi-spec subscribe straddling that boundary could leave some keys
    /// started and the rest stalled in `GapStart`.
    ///
    /// The same predicate loop is what makes a SPURIOUS wakeup harmless: it re-parks instead of
    /// spending a reconcile pass on nothing.
    pub fn wait_for_poke(&self, timeout: std::time::Duration) {
        let (m, cv) = &self.wake;
        let mut g = m.lock().unwrap_or_else(PoisonError::into_inner);
        if !*g {
            let (g2, _) = cv
                .wait_timeout_while(g, timeout, |woken| !*woken)
                .unwrap_or_else(PoisonError::into_inner);
            g = g2;
        }
        *g = false;
    }
}
