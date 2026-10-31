//! The core-emulated OTO/OCO drive: fill and terminal cascades, held and conditional scope clears.

use super::*;
use vike_model::OrderType;

impl<C: ExecutionClient> CoreThread<C> {
    /// Live-runtime OTO/OCO fill drive: after `filled_coid` fully fills and the engine has folded it,
    /// ARM this leg's held OTO children (submit each to the venue — the parent's fill is their
    /// trigger) and CANCEL its OCO siblings. The DECISION is the shared [`vike_exec::ContingencyBook`]
    /// resolver — the SAME `on_fill` the paper/backtest oracle drives, so the live path cannot
    /// diverge from it; this method owns only the book MECHANICS (release the held request, cancel
    /// the resting sibling). A no-op for a plain fill (not in the book). COLD path — per FILL, never
    /// the market-data fold.
    pub(crate) fn drive_contingency_on_fill(&mut self, filled_coid: &str, now: i64) {
        if self.contingency.is_empty() {
            return;
        }
        // `on_fill` arms every held child of the filled leg (flips it active) AND returns the leg's
        // OCO siblings (its `linked` ids that are NOT its own children) to cancel, in `linked` order.
        let siblings = self.contingency.on_fill(filled_coid);
        // RELEASE the just-armed OTO children: a child of the filled leg that is now active has its
        // held request drained + submitted to the venue. (A child still held — a deeper OTO tier not
        // armed by THIS fill — is left resting.)
        for child in self.contingency.children_of(filled_coid) {
            if !self.contingency.is_held(&child)
                && let Some(req) = self.held_orders.shift_remove(&child)
            {
                self.submit_resolved(&req, now);
                self.note(format!("OTO child {child} released by {filled_coid} fill"));
            }
        }
        // CANCEL the OCO siblings: one still HELD (never went live) is simply dropped from the held
        // map; one resting at the venue is canceled (`cancel_order` is `is_live`-guarded and the
        // venue emits the authoritative `OrderCanceled`). Each leaves the book.
        //
        // Unclassified (`CancelIntent::Unspecified`) on purpose: the sibling is removed from the
        // contingency book on the SAME pass, so nothing re-issues this cancel — a venue that held
        // it back to protect its own budget would strand a live protective leg whose OCO twin has
        // already filled. Book maintenance is not "routine churn" in the shed-and-re-offer sense.
        //
        // ⚠ A cancel the engine did NOT issue for a sibling the book still lists is WARNED, once per
        // sibling: after a restart the book can be restored while the registry is not (an order
        // nothing adopted), `cancel_order` is then a no-op, and the leg may still rest at the venue.
        // The link leaves the book all the same: nothing consumes a kept link, it would only leak,
        // make `contingency_terminal` clone per order event and later print "died unfilled"; the
        // WARN is what tells the operator to look at the venue. A sibling the book no longer lists
        // is a leg that already resolved (the venue cancelled it first, or it died unfilled):
        // nothing to say, as before.
        for sib in siblings {
            if self.held_orders.shift_remove(&sib).is_some() {
                self.note(format!("OCO sibling {sib} dropped (was held) on {filled_coid} fill"));
            } else {
                let eidx = self.coid_venue.get(&sib).copied().unwrap_or(0);
                if !self.eng_mut(eidx).cancel_order(&sib) && self.contingency.contains(&sib) {
                    // A row that exists but is terminal is "not live"; no row at all is "not in".
                    let why = if self.eng(eidx).registry.contains_key(&sib) {
                        "not live in the registry"
                    } else {
                        "not in the registry"
                    };
                    tracing::warn!(
                        target: "vike_core::core",
                        filled = filled_coid,
                        "OCO sibling {sib} NOT cancelled: {why}"
                    );
                    self.note(format!(
                        "OCO sibling {sib} NOT cancelled: {why} (its twin {filled_coid} filled; \
                         check the sibling at the venue)"
                    ));
                } else {
                    self.note(format!("OCO sibling {sib} canceled on {filled_coid} fill"));
                }
            }
            self.contingency.remove(&sib);
        }
        // The filled leg leaves the book (it terminalized). Its just-armed children STAY (active/live)
        // so a later child fill can still resolve its own OCO sibling.
        self.contingency.remove(filled_coid);
        // NB: no `pump_client` here on purpose. This runs from BOTH fold sites — the `Ingest::Event`
        // arm AND `pump_client`'s own drain loop (client-synthesized fills, e.g. the paper exchange).
        // Pumping here would re-enter `pump_client` from inside its own loop; instead each call site
        // pumps after driving, and the `pump_client` loop naturally drains the events this queued.
    }

    /// Live-runtime OTO/OCO terminal drive: a contingency leg that terminalized WITHOUT filling
    /// (canceled / rejected / DENIED / expired) can never arm its still-held OTO children —
    /// cascade-drop them (they were never sent to the venue, so there is nothing to cancel there) and
    /// drop the terminated leg's OWN book entry (stop the stale-entry leak). The paper oracle's
    /// `expire_children_of` on the live path, plus own-entry cleanup. Already-armed (live) children
    /// are LEFT — they are real resting orders now, cleaned by their own terminal event.
    ///
    /// A dying PROTECTIVE EXIT (a released leg with a parent — e.g. a stop-loss the venue rejected)
    /// is SURFACED (warn + recent-events note) so the operator knows a leg of the protection died;
    /// when it was a STOP and no other stop covers the position it protected, a second line,
    /// `POSITION WITHOUT STOP` ([`Self::note_bracket_stop_died`]), says the position is naked now.
    /// Its surviving OCO sibling's fate is the [`crate::CoreConfig::oco_cancel_sibling_on_dead_exit`]
    /// knob:
    ///
    /// - **OFF (default)** — the sibling is KEPT, not auto-canceled: a position that just lost one
    ///   protective leg retains whatever protection it still has (a naked position is the worse
    ///   default). Byte-identical to the feature as first shipped; only a FILL cancels a sibling.
    /// - **ON** — the surviving OCO sibling is ALSO canceled and cleaned from the book, through the
    ///   SAME mechanics the fill-driven OCO-cancel uses ([`Self::drive_contingency_on_fill`]): a
    ///   still-held sibling is dropped, a resting one is canceled at the venue (its authoritative
    ///   `OrderCanceled` re-enters here, but the book no longer holds it, so that is a no-op). A
    ///   deployment that prefers a fully flat book over partial protection opts into this.
    pub(crate) fn drive_contingency_on_terminal(&mut self, coid: &str) {
        if self.contingency.is_empty() || !self.contingency.contains(coid) {
            return;
        }
        // Snapshot whether this is a dead protective exit BEFORE the removals below borrow the book.
        let dead_exit_parent = self.contingency.parent_of(coid).map(str::to_string);
        // The dead exit's surviving OCO siblings, captured NOW, before its own book entry is removed
        // below — that removal drops the `linked` list `siblings_of` reads. They are what the keep line
        // below names (is one of them a STOP?), and, with the opt-in knob
        // (`CoreConfig::oco_cancel_sibling_on_dead_exit`, default OFF), what is cancelled: OFF leaves
        // `doomed_siblings` empty and the cancel block inert, so the keep-protection default is unchanged.
        let linked_siblings: Vec<String> = if dead_exit_parent.is_some() {
            self.contingency.siblings_of(coid)
        } else {
            Vec::new()
        };
        // The ones the book still lists survive: a sibling it dropped already resolved.
        let survivors: Vec<String> =
            linked_siblings.iter().filter(|sib| self.contingency.contains(sib)).cloned().collect();
        let doomed_siblings: Vec<String> =
            if self.config.oco_cancel_sibling_on_dead_exit { linked_siblings } else { Vec::new() };
        let mut queue = vec![coid.to_string()];
        while let Some(parent) = queue.pop() {
            for child in self.contingency.children_of(&parent) {
                if self.contingency.is_held(&child) {
                    self.held_orders.shift_remove(&child);
                    self.contingency.remove(&child);
                    self.note(format!("OTO child {child} dropped: parent {parent} terminated"));
                    queue.push(child);
                }
            }
        }
        self.contingency.remove(coid);
        let Some(parent) = dead_exit_parent else {
            return;
        };
        // A dead STOP leaves the position it protected with none: say so, once (alert only; the
        // lines below keep the take-profit and, with the knob, cancel it). Per dead exit, never
        // per event: `recon_stops.rs` carries the rule and the reasons.
        self.note_bracket_stop_died(coid, &parent);
        if doomed_siblings.is_empty() {
            self.say_dead_exit_kept(coid, &parent, &survivors);
            return;
        }
        // KNOB ON: cancel every surviving OCO sibling too — the same mechanics as the fill-driven
        // cancel (`drive_contingency_on_fill`): a still-held sibling is dropped, a resting one is
        // canceled at the venue; each leaves the book.
        for sib in doomed_siblings {
            if self.held_orders.shift_remove(&sib).is_some() {
                self.note(format!(
                    "OCO sibling {sib} dropped (was held): protective exit {coid} died unfilled"
                ));
            } else {
                let eidx = self.coid_venue.get(&sib).copied().unwrap_or(0);
                self.eng_mut(eidx).cancel_order(&sib);
                self.note(format!(
                    "OCO sibling {sib} canceled: protective exit {coid} died unfilled"
                ));
            }
            self.contingency.remove(&sib);
        }
        tracing::warn!(
            target: "vike_core::core",
            coid,
            parent = %parent,
            "protective exit leg terminated UNFILLED — sibling-cancel knob ON: the surviving OCO \
             sibling was canceled (book left flat)"
        );
        self.note(format!(
            "protective exit {coid} died unfilled (parent {parent}); surviving OCO sibling CANCELED"
        ));
    }

    /// **The words of a dead protective exit whose siblings were KEPT** (knob OFF, or nothing left to cancel): surface
    /// the death and say what protection the position still has. Only a STOP among the survivors is protection: a
    /// take-profit closes a winner and leaves the loss open, and with no survivor at all the bracket has nothing left,
    /// so the line says which of the three it is. Cold and out of line on purpose: [`Self::drive_contingency_on_terminal`]
    /// is called from the fold's `dispatch`, so the words must not grow it.
    #[cold]
    #[inline(never)]
    fn say_dead_exit_kept(&mut self, coid: &str, parent: &str, survivors: &[String]) {
        if survivors.iter().any(|sib| self.leg_is_stop(sib)) {
            tracing::warn!(
                target: "vike_core::core",
                coid,
                parent = %parent,
                "protective exit leg terminated UNFILLED — the position keeps its remaining OCO \
                 protection (the surviving sibling is not auto-canceled)"
            );
            self.note(format!(
                "protective exit {coid} died unfilled (parent {parent}); surviving OCO sibling KEPT"
            ));
        } else if survivors.is_empty() {
            tracing::warn!(
                target: "vike_core::core",
                coid,
                parent = %parent,
                "protective exit leg terminated UNFILLED — no OCO sibling is left, so this bracket \
                 leaves the position with NO stop"
            );
            self.note(format!(
                "protective exit {coid} died unfilled (parent {parent}); no OCO sibling left: NO stop \
                 from this bracket"
            ));
        } else {
            let sibs = survivors.join(", ");
            tracing::warn!(
                target: "vike_core::core",
                coid,
                parent = %parent,
                survivors = %sibs,
                "protective exit leg terminated UNFILLED — the surviving OCO sibling is not a stop \
                 (a take-profit closes a winner and leaves the loss open), so the position has NO \
                 stop from this bracket; the sibling is not auto-canceled"
            );
            self.note(format!(
                "protective exit {coid} died unfilled (parent {parent}); surviving OCO sibling {sibs} \
                 KEPT, but it is not a stop: NO stop from this bracket"
            ));
        }
    }

    /// Whether the contingency leg `coid` is a STOP (`Stop` or `StopLimit`), the one kind of exit that protects a
    /// position: a take-profit closes a winner and leaves the loss open. A leg the core still holds is read from the
    /// held map, a live one from its engine's registry; a leg found in neither is not claimed to be a stop.
    fn leg_is_stop(&self, coid: &str) -> bool {
        let kind = match self.held_orders.get(coid) {
            Some(req) => Some(&req.order_type),
            None => {
                let eidx = self.coid_venue.get(coid).copied().unwrap_or(0);
                self.eng(eidx).registry.get(coid).map(|mo| &mo.request.order_type)
            }
        };
        matches!(kind, Some(OrderType::Stop | OrderType::StopLimit))
    }

    /// Cancel a HELD (not-yet-at-venue) contingency leg: drop it from `held_orders` and the book.
    /// Returns `true` iff `coid` was held (so a cancel caller skips the venue path). A held leg has no
    /// registered order to terminalize, so this NOTES the removal rather than emitting an
    /// `OrderCanceled` the FSM would drop as an unknown coid. Its OTO parent's `linked` list still
    /// names it, but `on_fill`/`children_of` resolve by the BOOK, from which it is now gone, so the
    /// parent's later fill neither releases nor references it.
    pub(crate) fn cancel_held(&mut self, coid: &str) -> bool {
        if self.held_orders.shift_remove(coid).is_some() {
            self.contingency.remove(coid);
            self.note(format!("held bracket exit {coid} canceled before its parent filled"));
            true
        } else {
            false
        }
    }

    /// Drop every HELD contingency leg matching a `(venue, symbol)` scope (`None` = any) from
    /// `held_orders` + the book — the `MassCancel` scoped twin of [`Self::cancel_held`]. Live legs are
    /// NOT touched here (the engine's `mass_cancel` cancels those at the venue; their `OrderCanceled`
    /// cleans the book).
    ///
    /// ⚠ **The scope is a VENUE, never an account, and that is now the right shape rather than an
    /// oversight.** A held exit is matched on its `OrderRequest::venue` — the canonical exchange id
    /// both accounts of a venue carry — so this drops both accounts' held exits. It was ASYMMETRIC
    /// while the arm above it cancelled one engine: the default account's live orders went, and
    /// BOTH accounts' protective held exits went with them, so the second account kept its resting
    /// orders and lost the exits that covered them. Now that the arm fans over
    /// [`Self::exit_scope_engines`], the operator path cancels exactly the books this empties.
    ///
    /// **`engines` NARROWS it to the held exits of those engines** — `Some` exactly when the
    /// payload NAMED an account (`OrderIntent::MassCancel`'s `account`), carrying the engine set
    /// the cancel itself reached. Each held exit is attributed to the engine its release would
    /// reach ([`Self::held_release_engine`], the one answer `submit_resolved` acts on), so
    /// `mass-cancel binance ALT` drops ALT's held exits and leaves the default account's, whose
    /// entries are still live and still need them. `None` is the venue-wide clear above,
    /// unchanged. The replay twin (`crate::replay`'s `fold_intent_scope`) has no engine to narrow
    /// by and needs none: it is single-engine by construction, and on one engine the narrowed set
    /// IS the venue scope.
    ///
    /// **Residual, declared:** where the caller named one engine by its ROUTE rather than by the
    /// payload (a labelled mount's own account-less venue-scoped mass-cancel), this still clears
    /// the other account's held exits. The mount's intent carries no account for it to narrow by,
    /// and narrowing on the route instead would change a strategy-lane behaviour this change is
    /// not about.
    pub(crate) fn clear_held_scope(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        let doomed: Vec<String> = self
            .held_orders
            .iter()
            .filter(|(c, r)| {
                venue.is_none_or(|v| r.venue == v)
                    && symbol.is_none_or(|s| r.symbol == s)
                    && engines
                        .is_none_or(|set| set.contains(&self.held_release_engine(c, &r.venue)))
            })
            .map(|(c, _)| c.clone())
            .collect();
        for c in doomed {
            self.held_orders.shift_remove(&c);
            self.contingency.remove(&c);
        }
    }

    /// Clear every ARMED conditional matching a `(venue, symbol)` scope (`None` = any) — the
    /// `MassCancel` twin of [`Self::clear_held_scope`], and the ONE spelling of "a book is being
    /// emptied", so an arm's account entry ([`CoreThread::cond_engine`]) cannot be left behind by a
    /// site that remembered to clear the book and forgot the map.
    ///
    /// Two passes rather than one because the ids are read from `conditional_books` and spent on
    /// `cond_engine`; the books are few (one per armed `(venue, symbol)`) and this is command
    /// cadence, never the per-message fold.
    ///
    /// ⚠ **VENUE-KEYED, like the books themselves.** `conditional_books` is keyed `(venue, symbol)`
    /// — an EXCHANGE fact, which is why an arm needs `cond_engine` to remember which account armed
    /// it at all — so emptying one book empties both accounts' arms. Same story as
    /// [`Self::clear_held_scope`]: asymmetric while the `MassCancel` arm cancelled a single engine,
    /// matched to the cancel now that the arm fans over [`Self::exit_scope_engines`], and carrying
    /// the same declared residual for a caller that named ONE engine by its route.
    ///
    /// `engines` narrows it exactly as it narrows [`Self::clear_held_scope`]: `Some` when the
    /// payload NAMED an account, and then only the arms that would FIRE onto one of those engines
    /// ([`Self::armed_engine`]) leave the shared book — each through `ConditionalBook::disarm`, so
    /// the other account's stop keeps its place in fire order. An arm whose fire reaches NO engine
    /// (a restored arm on a venue with several accounts, which the Submit arm refuses as ambiguous)
    /// is in no named account's scope, so a narrowed clear leaves it. `None` empties the whole
    /// book, as it always did.
    pub(crate) fn clear_conditional_scope(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        let in_scope = |(v, s): &(String, String)| {
            venue.is_none_or(|want| v == want) && symbol.is_none_or(|want| s == want)
        };
        // A core-held stop that fired and is awaiting its close's verdict is part of the book being
        // emptied: do not re-arm it behind the operator's mass-cancel or market-exit.
        self.forget_fired_stops(venue, symbol, engines);
        let Some(engines) = engines else {
            // The venue-wide clear, exactly as it always ran: every arm in scope leaves, and the
            // books are emptied whole.
            let doomed: Vec<String> = self
                .conditional_books
                .iter()
                .filter(|(k, _)| in_scope(k))
                .flat_map(|(_, b)| b.iter().map(|(id, _)| id.to_string()))
                .collect();
            for id in doomed {
                self.cond_engine.remove(&id);
            }
            for (k, book) in self.conditional_books.iter_mut() {
                if in_scope(k) {
                    book.clear();
                }
            }
            self.refresh_tick_conditionals();
            return;
        };
        // The NARROWED clear: only the arms that fire onto one of `engines`, each disarmed out of
        // the shared book by id so the survivors keep their fire order. An arm whose fire reaches
        // no engine (`armed_engine` answers `None`) is not in the named account's scope.
        let mut doomed: Vec<((String, String), String)> = Vec::new();
        for (k, book) in self.conditional_books.iter().filter(|(k, _)| in_scope(k)) {
            for (id, _) in book.iter() {
                if self.armed_engine(id, &k.0).is_some_and(|e| engines.contains(&e)) {
                    doomed.push((k.clone(), id.to_string()));
                }
            }
        }
        for (k, id) in &doomed {
            self.cond_engine.remove(id);
            if let Some(book) = self.conditional_books.get_mut(k) {
                let removed = book.disarm(id);
                debug_assert!(removed, "the walk above read this arm out of this book");
            }
        }
        self.refresh_tick_conditionals();
    }
}
