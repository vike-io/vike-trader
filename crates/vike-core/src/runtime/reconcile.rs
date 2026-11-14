//! Fold-thread half of reconcile: `reconcile_reports` (reports in, compute on the fold) and the operator `confirm_reconcile`.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Fold-thread half of the reconcile runtime driver (Option A: reports-in, compute-on-fold).
    /// The reconcile MANAGER thread (`vike_tradehub_core::ReconcileManager`) did the blocking REST fetch OFF
    /// this thread and enqueued the raw reports as [`Command::ReconcileReports`]; HERE, on the
    /// single writer, we read THIS engine's own [`Engine::local_view`] (cheap, same-thread
    /// — so no cross-thread local-state read and no staleness window), run the PURE
    /// [`vike_execution::recon::diff`] + [`vike_execution::recon::resolve`], and fold each synthesized event
    /// through the SAME [`Self::publish_to`] path real venue events use. An OCCASIONAL command
    /// (startup, plus Task 16's interval cadence), never the per-message hot fold, so the pure
    /// diff/resolve compute here is within budget.
    ///
    /// `recon.snapshot` is the `resolve` default (empty) for positions/orders: `resolve` seeds
    /// position state via the synthesized FILL events, not the snapshot, so applying THOSE fields
    /// would be a no-op at best (`apply_snapshot` skips empty positions) and DESTRUCTIVE at worst —
    /// `Engine::apply_snapshot`'s order-reap terminalizes any live local order absent from
    /// `snapshot.open_orders` (see `crates/vike-execution/tests/recon/reconcile_reap.rs`), so applying an
    /// all-empty-except-balance snapshot would cancel EVERY live order on this venue as a side
    /// effect. Task 3 (balance activation) therefore never calls it: `reports.balance` (from
    /// [`vike_execution::recon::ReconcileClient::fetch_balance`]) is seeded directly onto `Account::balance`/`balance_mode` below — the narrowest path that
    /// updates cash without touching positions or the order registry. `recon.snapshot.balance` is
    /// still set (mirrors the `Recon` shape / a future `CoreSnapshot.recon` surface) but is NOT
    /// itself applied.
    ///
    /// ⚠ **On the DIFFED path (`VIKE_RECONCILE_BALANCE=1`) saying "it adopts" of that seed is now
    /// wrong.** Two writes remain, neither of them an adoption of a venue number this daemon cannot
    /// explain: the FIRST SYNC (an account with no venue-anchored baseline — the cold-start adopt)
    /// and the ROLL-FORWARD (the local figure with this daemon's own realized PnL folded in, which
    /// the diff and equity are both invariant under: `Account::realized_since_balance_sync` reads the
    /// same PnL from the same anchor). A sub-tolerance venue move is neither absorbed nor written: this
    /// pass's anchor does not move onto the venue's figure. Beyond the band it becomes a
    /// `BalanceDrift` like any other divergence. The application site below spells both writes out;
    /// `vike_execution::recon::ReconcileBalanceCheck` carries the algebra.
    ///
    /// ⚠ **What that buys a DEPLOYMENT is not stated here, and its absence is deliberate.** Every
    /// phrasing tried so far — "the absorption is bounded", "a slow bleed accumulates against a
    /// fixed reference and crosses the band", "on venue X this pass is the sole adopter" — assumes
    /// nothing else writes `Account::realized_pnl_at_balance_sync` first, and that assumption has
    /// been stated wrongly five times running, each time by the fix for the previous one. The last
    /// two rounds were a venue LIST welded into a gate (the wrong list) and its complement
    /// ("every other roster venue", false for ctrader). A sentence that a bridge could falsify by
    /// changing one subscription does not belong in a comment.
    ///
    /// Which venues' private socket actually receives a live wallet frame — the fold
    /// `Account::apply_account_state` performs, which writes this anchor absolutely — is DERIVED
    /// and gated by `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs`, re-run from the bridge
    /// tree on every test pass. Read the set there; it is deliberately not restated here, because
    /// restating it is the defect.
    ///
    /// Two of the other writers can be stated without a scope, because neither depends on a venue:
    /// `Engine::apply_snapshot` writes the anchor but no runtime path calls it (tests and
    /// the frozen `fixtures/r5/` replay only; `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`
    /// states the same for the reap it guards);
    /// and `Account::restore` restores it as `None` (deliberately absent from `EngineSnapshotAccount`),
    /// so the first pass after a process start is an `Adopt`.
    ///
    /// ⚠ The DEFAULT path (the flag unset) is untouched and still adopts every pass — see the
    /// application site for what protects a deployment there.
    ///
    /// FORWARD-NOTE (Task 7): a canceled-with-zero-fills order that has already dropped out of the
    /// venue `openOrders` set produces NO report row, so this pass canNOT observe every terminal
    /// transition — the journal cross-check (Task 14) is the closer, not this.
    ///
    /// Task 17: `recon.events` (Synthesize / auto-Hybrid kinds) still fold immediately, byte-
    /// identical to before. `recon.alerts` (Quarantine / Hybrid-quarantined kinds) are no longer
    /// just a ring note — each is assigned a monotonic id, held in `self.reconcile_alerts` (its
    /// `proposed_events` kept for `Self::confirm_reconcile` to fold later), and surfaced structurally
    /// via `CoreSnapshot.recon.alerts` (built by `Self::reconcile_block` at the next publish). The
    /// ring note is kept too (cheap, and useful chronological context in `recent_events`).
    /// Alerts carrying a `dedup_key` (a by-design recurring divergence, e.g. an UnknownOrder under
    /// `generate_missing_orders`) refresh the matching held row in place instead of appending —
    /// see the alert loop below.
    ///
    /// ⚠ **Routing here reads [`ReconcileReports::route`], never `reports.venue`.** They are the
    /// same string for every pass this tree produces, which is exactly why the distinction has to
    /// be made by the TYPE rather than by reading: this one resolution decides which local view a
    /// pass diffs against, which book its synthesized fills fold into, and — through the
    /// `route_key` stored on every held alert below — which book an operator confirm folds into
    /// later. `venue` keeps every job that is genuinely about the exchange: the ring notes and log
    /// lines an operator reads, the `(venue, symbol)` coin-delta keys (whose venue comes off the
    /// position REPORT, minted canonically by an adapter), and the dedup identity of a held row.
    pub(crate) fn reconcile_reports(&mut self, reports: ReconcileReports) {
        // Resolve the target ENGINE first, off the payload's routing key, while the payload is
        // still whole — destructuring it below moves `venue` out and would leave `route()`
        // unreachable.
        let route = self.engine_index_for_route_key(reports.route());
        // How many engines of this EXCHANGE does this process run? Read before the destructure
        // (it needs `&self` and the payload whole) and used once, by the refusal below. Off the
        // fold — a reconcile pass is interval work — and `1` on every single-account core, where
        // `engines_of_venue` is the venue lookup it has always been.
        let accounts_of_venue = self.engines_of_venue(&reports.venue).len();
        let ReconcileReports {
            venue,
            since,
            mut orders,
            mut fills,
            positions,
            policy,
            balance,
            generate_missing_orders,
            reconcile_balance,
            balance_tol,
            route_key,
            admitted_symbols,
            lists_trigger_orders,
        } = reports;
        // **CLASS E — THE REFUSAL. A pass that cannot say WHICH account it read is not reported
        // against the venue's default.**
        //
        // `route_key: None` means "[`ReconcileReports::venue`]'s SOLE account in this process".
        // On a venue this process runs several engines of, that is a false statement, and the
        // engine it resolves to — the default account's — is a GUESS about which book the reports
        // describe. Folding it is the misroute the per-account producer exists to remove, wearing
        // a replayed payload's clothes: under `hybrid`, `PositionDrift` auto-applies, so it would
        // rewrite the default account's position size onto another account's number and book
        // realized PnL at that account's average price with no operator in front of it.
        //
        // ⚠ **No live producer in this process can reach this arm.** Every leg of a venue this
        // process runs several ENGINES of names itself (`ReconcileLeg::name_accounts_of_shared_venues`,
        // applied where the legs are assembled), so a `None` here is a payload this process did not
        // write: a REPLAYED journal `Command::ReconcileReports` from a build that predates the
        // account fan-out, or from a run when this venue genuinely had one account. That is also
        // the one path on which an account can vanish between a pass and its confirm, and refusing
        // is the safe end of it — the pass folds NOTHING, the operator keeps every row already
        // held, and the next live pass re-raises whatever is still true against the right book.
        //
        // ⚠ **That claim is only true because the stamp counts the SAME SET this arm does**, which
        // it did not until 2026-09-14: the stamp counted LEGS while this counts ENGINES, and
        // `crates/vike-tradehub-core/src/node/accounts.rs`'s `runnable_accounts_of` mounts an engine for every armed account while pushing a
        // reconcile leg only for the ones that produced a client. Two engines and one leg was
        // therefore a live refusal of the surviving account's own legitimate pass, every interval.
        // The predicate is the engine set now (that function's doc carries the argument), so the
        // unreachability above is a property rather than a hope.
        //
        // ⚠ **Structurally unreachable on a single-account box**: `accounts_of_venue` is 1 there
        // for every venue, whatever the payload carries, so this is byte-identical to before it
        // existed on every deployment running today.
        if route_key.is_none() && accounts_of_venue > 1 {
            tracing::error!(
                target: "vike_core::reconcile",
                venue = %venue,
                accounts = accounts_of_venue,
                orders = orders.len(),
                fills = fills.len(),
                positions = positions.len(),
                "reconcile pass REFUSED: it names no account and this process runs several \
                 accounts of this venue — folding it would diff one book's venue truth against \
                 another book's local view"
            );
            self.note(format!(
                "RECON refused: pass for venue {venue} names no account, and this process runs \
                 {accounts_of_venue} accounts of it (nothing folded)"
            ));
            return;
        }
        // **THE ACCOUNT this pass is about**, resolved ONCE and used for every job below that has
        // to name a book rather than an exchange: the ring notes, the per-pass line, the held-alert
        // announcer's key, each held alert's stored `route_key` and each held alert's announcement
        // IDENTITY. A venue with one account resolves its own venue id here, so every one of those
        // reads exactly as it did before accounts existed.
        let account_key = route_key.unwrap_or_else(|| venue.clone());
        let Some(index) = route else {
            // ⚠ BOTH strings: the venue this note has always named, and the ACCOUNT. On a box
            // with one account per venue they are equal and this reads as it always did; on one
            // that dropped a labelled account between runs, a REPLAYED pass for that account
            // would otherwise say "no engine for venue binance" on a box that plainly has a
            // binance engine — true, and unreadable.
            //
            // The pass folds NOTHING either way, which is the safe direction: a route key with
            // no engine is skipped, never re-pointed at the venue's default account.
            self.note(format!(
                "RECON skipped: no engine for venue {venue} (account {account_key})"
            ));
            return;
        };
        // A ledger restored from a window of days (`vike_execution::account::fill_window`) cannot judge a fill stamped
        // before its floor, so this pass does not read one (a history endpoint that ignores the start time): it
        // would raise as missing, and `hybrid` would book again, a fill the window left out. A ledger that left
        // nothing out holds no floor and keeps every row.
        let account = &self.engine(index).account;
        fills.retain(|f| !account.before_fill_floor(f.timestamp));
        // Wave 5d: capture any venue-REPORTED per-position coin delta (Deribit `get_positions.delta`)
        // into the side map the snapshot publishes, so the greeks tool can fold a perp/future hedge
        // leg via `coin_delta × spot`. Only `Some(delta)` rows upsert; a venue that reports no delta
        // (every venue but Deribit today) leaves the map untouched and byte-identical.
        //
        // ⚠ **DECLARED RESIDUAL — this map is keyed `(venue, symbol)` and is NOT per account.**
        // Two accounts of one exchange holding the same instrument overwrite each other here: the
        // last pass of the interval wins, and the greeks tool folds one account's venue delta for
        // both books. It is NOT fixed here, and the reason is the CONSUMER rather than this site:
        // the reader is `vike_app_core`'s greeks tool, which builds its legs from
        // `CoreSnapshot::positions` and looks each one up by `(p.venue, p.symbol)` — a
        // `PositionView` carries no account handle at all, so keying this map per account would
        // make every lookup MISS and silently un-price every hedge leg, which is strictly worse
        // than the collision. Closing it is a change to the POSITION VIEW (a per-account handle on
        // `PositionView`, then this key and that lookup together), which is a portfolio-plane
        // change rather than a reconcile one.
        //
        // What bounds the exposure today: `delta` is `Some` for Deribit alone
        // (`crates/bridges/deribit/src/reconcile/mod.rs`), and it collides only when two Deribit
        // accounts hold the SAME instrument. It moves no book — the map is read-only decoration
        // for a greeks display, never folded.
        for pr in &positions {
            if let Some(d) = pr.delta {
                self.reconcile_coin_deltas.insert((pr.venue.clone(), pr.symbol.clone()), d);
            }
        }
        // ORDER RESTORE (`restore.rs`'s module doc): the orders a previous session left resting are
        // adopted into THIS engine's registry (or told gone) BEFORE the diff below reads the
        // registry, so an adopted order is a known local order and raises no `UnknownOrder`. Inert
        // and cheap unless the core restored entries AND the venue's gate is open. A failed fetch
        // never reaches here (the manager sends no command on an error). A client whose reports
        // name an order by a wire id gets the pass's rows rewritten IN PLACE (`restore.rs`'s
        // translation step), so the diff below reads the same rows the planner did.
        self.restore_orders_at_pass(
            index,
            &venue,
            &account_key,
            &mut orders,
            &mut fills,
            super::restore::ReconcilePassFacts {
                since,
                admitted: admitted_symbols.as_deref(),
                lists_trigger_orders,
            },
        );
        // The admitted symbols this pass's client was NOT told about (decision 0216): the diff's two
        // local-side sweeps leave their orders and positions alone, as the restore above did.
        let unread = self.unread_symbols(index, admitted_symbols.as_deref());
        // The engine's quote-asset selector for the synthesized/adopted balance (Feature 2). Read
        // before the &mut publish below; unused on the legacy-seed path (`reconcile_balance` off)
        // except by the crossing line, which never fires there.
        let quote_asset = self.engine(index).quote_asset.clone();
        // ⚠ AN INVERSE ENGINE'S LEDGER IS ONE CURRENCY, THE ENGINE'S QUOTE (decision 0170, item 7):
        // the venue's wallet figure is a COIN amount (deribit: BTC), and adopting, rolling or diffing it
        // against a quote ledger would write a coin number into cash or raise a drift of 65,000x. The
        // balance lane sees "the venue reported none" for such an engine; the positions, orders and
        // fills lanes are untouched. (`Account::apply_account_state` withholds the same figure from a
        // venue balance frame.)
        //
        // A LEDGER THAT HOLDS A FOREIGN-QUOTED SYMBOL TAKES THE FIGURE (decision 0183, amended by the
        // owner's ruling of 2026-10-19: "the reconcile balance needs no currency label: it is, by
        // definition, in the account's currency, known by the API, else USD"). Such a ledger is in the
        // ACCOUNT's currency and the engine could only admit the symbol once it was TOLD that currency
        // (`account-ccy-unset`), so a venue's account balance IS the ledger's currency and is adopted,
        // rolled and diffed like any other. This used to be withheld because `balance` carries no
        // asset label: the cost was an OANDA equity of seed + balance after a restart, and a restored
        // `Authoritative` balance that no pass ever re-adopted (`Account::restore_folded`).
        //
        // ⚠ THIS CORE NAMES NO VENUE. A venue whose figure is NOT the account currency's (IBKR's
        // `fetch_balance` is the MOUNTED contract's `CashBalance` row, `parse_socket_balance`, and a
        // foreign stock settles in its own currency's cash) withholds it ITSELF, in its own
        // `ReconcileClient` (`vike-ibkr`'s `holds_foreign_stock`): the pass then carries no balance, which
        // this lane already reads as "the venue reported none" (no adopt, no roll, no drift).
        // `Account::apply_account_state` still takes only the balance of a frame whose asset IS the
        // account's currency.
        //
        // ⚠ A COIN LEDGER TAKES IT (decision 0258): its money IS the settle coin, the figure the venue's
        // client reports (deribit: `balance + session_rpl`, less what 08:00 settled for a future still open, of
        // the instrument's settlement currency: decision 0258's item 4 and its 2026-11-09 amendment), so it
        // is adopted, rolled and diffed like any other, against a tolerance in the coin: the dollar floor
        // converted at the engine's last JOURNALED fill price (`Account::coin_tolerance`), never at a mark,
        // which this journaled pass may not read.
        let (balance, balance_tol) = {
            let a = &self.engine(index).account;
            let balance = if a.trades_inverse() && a.coin_key().is_none() { None } else { balance };
            (balance, a.coin_tolerance(balance_tol))
        };
        // What this delivered pass can say about the rows already held (`reconcile_pass`'s module doc).
        let cover = reconcile_pass::ReconcilePassCover {
            positions: !positions.is_empty(),
            balance: reconcile_balance && balance.is_some(),
            journal: self.config.journal_view_provider.is_some(),
        };
        // Read before `reconcile_compute` takes the policy by value.
        let adopts_increase = policy.adopt_balance_increase;
        let (mut recon, mut plan) = self.reconcile_compute(
            index,
            &venue,
            &quote_asset,
            &orders,
            &fills,
            &positions,
            policy,
            balance,
            reconcile_balance,
            balance_tol,
            generate_missing_orders,
            &unread,
        );
        self.adopt_a_pure_balance_increase(
            index,
            &account_key,
            cover,
            adopts_increase,
            &mut plan,
            &mut recon,
        );
        self.adopt_cash_the_book_left_out(
            index,
            &account_key,
            cover,
            adopts_increase,
            &mut plan,
            &mut recon,
        );
        if reconcile_balance && balance.is_some() {
            self.engine_mut(index).account.spend_unfolded_cash_pass();
        }
        // The venue's own net position per symbol, REPLACED wholesale every pass, is the pre-trade
        // gate's evidence for a reduce-only close of a position this book has not folded (held by
        // `quarantine`): `Engine::cover_position_size` reads it and nothing else does.
        // Hedge buckets sum, as `gate_position_size` nets them. A symbol the pass did not report
        // drops out, which is the safe direction: no evidence, no exemption.
        let mut venue_positions = std::collections::HashMap::<String, f64>::new();
        for p in &positions {
            *venue_positions.entry(p.symbol.clone()).or_insert(0.0) += p.qty;
        }
        self.engine_mut(index).venue_reported_positions = venue_positions;
        self.apply_reconcile_balance(
            index,
            &venue,
            &account_key,
            &quote_asset,
            &plan,
            balance,
            reconcile_balance,
            &mut recon,
        );
        let n_events = recon.events.len();
        for event in recon.events {
            self.publish_to(index, event);
        }
        // Mirror the Ingest::Event path: drain any events an in-process client synthesized from
        // the folded fills (real venue clients return None here).
        self.pump_client();
        let n_alerts = recon.alerts.len();
        let transitions = self.hold_reconcile_alerts(&venue, &account_key, recon.alerts, cover);
        // A held missed fill the book has since folded, exactly, goes (`reconcile_pass`'s module doc).
        self.clear_folded_missing_fills(index, &account_key, &fills);
        // A fill no stamp vouches for (the stream's, or one this pass folded under `hybrid`) is held, never booked.
        self.hold_unproven_fills();
        self.reconcile_last_pass_timestamp = self.engine.now_ms;
        // POSITION WITHOUT STOP (`reconcile_stops.rs`): the pass's own two reports and the core's books,
        // edge-triggered. Pass cadence, after the diff; it folds and holds nothing.
        self.note_positions_without_stop(index, &venue, &account_key, &orders, &positions);
        // A RESTORED core-held stop waits for the first pass of its account (`core_held.rs`): this one
        // has landed. Pass cadence; one `is_empty` test on a node that restored no such stop.
        if !self.stop_holds.is_empty() {
            self.release_stop_holds(index);
        }
        // ⚠ A quiet PASS is a STATE too, and this line was the last per-pass repetition left after
        // the WARN flood above was fixed. It fired on every pass carrying ANY alert, so under
        // `quarantine` it repeated a minute apart forever — `events=0 alerts=1` measured identical
        // on the CI box at 2026-08-25 11:41:43 and 11:42:43 — saying only "the same divergence is still
        // held", which `reconcile_held`'s announcer now reports properly. INFO, so on a daemon running
        // a `warn` file level it floods the JOURNAL rather than the log file: the same
        // defect one severity down, which is why it was deferred out of that fix.
        //
        // What still logs EVERY time: a pass that FOLDED events (that changed the book — a fact
        // about this pass, not about the held set, which is why it is ORed in here rather than
        // asked of the announcer), and a pass whose held set CHANGED. What no longer logs: a pass
        // that merely repeated the previous one. The backlog stays visible because `silent()` is
        // also false on the announcer's periodic summary pass, so this line rides that ONE clock
        // instead of a second rate limiter of its own — see
        // `crate::runtime::recon_held`'s `ReconcileHeldTransitions::silent`.
        //
        // ⚠ The verdict is over the held-set IDENTITIES, not over `n_alerts`: two alerts sharing
        // one identity are one divergence (that is what bounds the store), so a pass whose COUNT
        // moved while its identity set did not is a repetition and stays quiet. And a venue with
        // nothing held at all is silent exactly as it was before this change — a heartbeat where
        // there was never a line is new noise, and `CoreSnapshot.recon.last_pass_timestamp` (set on the
        // line above, published every pass) is the liveness signal that answers structurally.
        if n_events > 0 || !transitions.silent() {
            // Pass-boundary log only (not per-event) — within the cold-path logging budget.
            tracing::info!(
                target: "vike_core::reconcile",
                venue = %venue,
                // ⚠ WHICH pass. At fifty accounts of one exchange this line fires up to fifty
                // times an interval, and `venue=binance events=0 alerts=1` fifty times over is
                // not a report. Equal to `venue` on a venue with one account.
                account = %account_key,
                events = n_events,
                alerts = n_alerts,
                "reconcile pass folded"
            );
        }
    }

    /// The closed `&self` region of [`Self::reconcile_reports`]: this engine's local view, the pure
    /// diff, the balance verdict, the adopt context, the instance-origin stamp and `resolve`.
    /// `local`, `local_view` and `adopt` all borrow inside it and die at `resolve`, so nothing
    /// borrowed leaves it: the caller gets the owned `Recon` and the balance verdict as a Copy
    /// `BalancePlan`. It reads this engine, `config.journal_view_provider`, `order_client_id_gen.origin()` and
    /// `engine.now_ms`, and writes nothing.
    ///
    /// Its comments were written inside `reconcile_reports` and moved here verbatim, so "this fn's
    /// doc" in them means [`Self::reconcile_reports`]'s.
    #[expect(clippy::too_many_arguments)]
    fn reconcile_compute(
        &self,
        index: usize,
        venue: &str,
        quote_asset: &str,
        orders: &[vike_model::OrderStatusReport],
        fills: &[vike_model::FillReport],
        positions: &[vike_model::PositionStatusReport],
        policy: vike_execution::recon::ReconcilePolicy,
        balance: Option<f64>,
        reconcile_balance: bool,
        balance_tol: vike_execution::recon::ReconcileBalanceTolerance,
        generate_missing_orders: bool,
        unread: &[String],
    ) -> (vike_execution::recon::ReconcilePassOutcome, BalancePlan) {
        // local_view returns OWNED data — the &self borrow is released before the &mut publish.
        let local = self.engine(index).local_view();
        // Journal cross-check (#3): build the venue's ReconcileJournalView from the materialized Tier-2 log
        // (via the root-supplied provider) and pass `Some(&journal)` to enable the three-way check.
        // `None` (provider unset) keeps this pass byte-identical to the two-way (local-vs-venue)
        // comparison — and `None` is every live mount today, because no root supplies a provider
        // (`ReconcileJournalViewHook`'s doc says who last did). A bounded lookback query on this OCCASIONAL
        // path, not the hot fold.
        let journal = self.config.journal_view_provider.as_ref().map(|p| p(venue));
        let local_view = local.as_view();
        // A spot row's local dust under the symbol's OWN lot is flat, like the wallet's (decision 0276);
        // an unadmitted symbol has none and is compared unrounded, never on the mount's lot.
        let engine = self.engine(index);
        let mut divergences = vike_execution::recon::diff_scoped(
            orders,
            fills,
            positions,
            &local_view,
            journal.as_ref(),
            unread,
            &|symbol| engine.gate.limits.own_lot_size(symbol, &engine.symbol),
        );
        // The TERMS of each matched order (`vike_execution::recon::diff_order_terms`), when the pass's
        // policy compares them (`ReconcilePolicy::order_terms`: every named preset; a pass journaled
        // before the key compares none, so a replay folds what it folded). On the engine's grid for
        // the symbol (a price on its tick, a size on its lot, the size only where the venue's row
        // states the TOTAL: `vike_venues::venue_amend::report_quantity_is_total`); an amend sent or
        // answered within the last pass, or a cancel-then-new amend in flight, races the venue's row
        // and is left alone.
        if policy.order_terms.is_some() {
            let engine = self.engine(index);
            let now = self.engine.now_ms;
            divergences.extend(vike_execution::recon::diff_order_terms(
                orders,
                &local_view,
                &|symbol| engine.gate.limits.grid_for(symbol),
                vike_venues::venue_amend::report_quantity_is_total(&engine.venue),
                &|order_client_id| {
                    engine.amend_settling(order_client_id, now)
                        || self.amend_in_flight(order_client_id)
                },
            ));
        }
        // Feature 2 (`VIKE_RECONCILE_BALANCE`): promote balance to a first-class DIFFED dimension.
        // Off (`false`, the default) is byte-identical: the legacy silent seed runs after
        // `resolve`, `diff_balance` unreached.
        //
        // ⚠ **The three verdicts are THREE DIFFERENT WRITES, and this used to collapse two of
        // them.** `diff_balance` answered `Option<Divergence>`, and its `None` meant either "first
        // sync, nothing to compare against" or "the figures agree" — both landed on one
        // `balance_adopt` arm that overwrote `Account::balance` with the VENUE's figure and
        // RE-ANCHORED `Account::realized_pnl_at_balance_sync` onto it, on every single pass.
        // Re-anchoring is what made the tolerance unbounded IN AGGREGATE: the next pass then
        // measured drift from the venue's own number, so a sub-threshold move absorbed this pass
        // could be absorbed again the next one, forever, with nothing ever raised. With the default
        // `ReconcileBalanceTolerance` on a 250k account that is 25 a pass at the 60 s interval default — about
        // 36k a day, silently, on an account a third party can fund or drain.
        // `vike_execution::recon::ReconcileBalanceCheck` is the split, and its doc carries the algebra.
        //
        //   - `Adopt` — FIRST SYNC only (`BalanceMode::Delta`, or no `realized_at_sync`). Take the
        //     venue's figure whole and anchor the realized-PnL baseline beside it. This is what
        //     keeps a COLD START usable: without it a fresh live mount stays in `Delta` and judges
        //     every order against the arbitrary `seed_cash`.
        //   - `Anchored { expected, .. }` — ROLL THE LOCAL FIGURE FORWARD, on every diffed pass and
        //     whether or not a drift was raised. It adopts NOTHING: `expected` is
        //     `balance + (realized_pnl − baseline)`, the local number with this daemon's OWN
        //     realized PnL folded in, and writing it back (with the baseline moved to
        //     `realized_pnl`) leaves the next pass's `expected` bit-unchanged, and equity with it:
        //     `Account::realized_since_balance_sync` reads the same PnL from this same anchor. What it
        //     BUYS is that the `balance` an operator reads and a `Snapshot` holds stays current.
        //   - `Anchored { drift: Some(d), .. }` — the venue's figure is beyond tolerance. `d` flows
        //     through the SAME `resolve`/policy machinery as every other kind, exactly as before.
        //
        // THIS PASS's anchor never moves onto the venue's number. That is the mechanism, it needs
        // no qualifier, and it is where this comment stops.
        //
        // ⚠ **What it buys a deployment is NOT claimed here.** "Absorption is bounded", "an
        // unexplained move accumulates against a fixed reference and crosses the band", "on venue X
        // this pass is the sole adopter" — each of those is a consequence derived from the
        // mechanism plus an assumption about what ELSE re-anchors, and that assumption has been
        // wrong five times in a row on this one feature, most recently as a venue list welded into
        // a gate. Which venues receive a live wallet frame (the other unconditional anchor write,
        // through `Account::apply_account_state`) is derived and gated by
        // `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs`; read it there rather than trusting a
        // sentence a bridge could falsify by changing one subscription. See this fn's doc.
        let mut balance_seed: Option<f64> = None;
        let mut balance_roll: Option<f64> = None;
        // Captured for the crossing line below — see the fold-visibility note after `resolve`.
        let mut balance_crossed: Option<(f64, f64)> = None;
        if reconcile_balance {
            match vike_execution::recon::diff_balance(
                &local_view,
                balance,
                quote_asset,
                balance_tol,
                self.engine.now_ms,
            ) {
                vike_execution::recon::ReconcileBalanceCheck::NotReported => {}
                vike_execution::recon::ReconcileBalanceCheck::Adopt(b) => balance_seed = Some(b),
                vike_execution::recon::ReconcileBalanceCheck::Anchored { expected, drift } => {
                    balance_roll = Some(expected);
                    if let Some(d) = drift {
                        if let vike_execution::recon::ReconcileDivergence::BalanceDrift {
                            venue_balance,
                            ..
                        } = &d
                        {
                            balance_crossed = Some((expected, *venue_balance));
                        }
                        divergences.push(d);
                    }
                }
            }
        }
        // `generate_missing_orders` rode in on THIS pass's `ReconcileReports` — the fold thread has
        // no other view of `vike_tradehub_core::ReconcileConfig` (see that struct's own doc). It becomes
        // `resolve`'s ReconcileAdoptContext, built from the same pass's fill reports + the engine's seen
        // trade ids (mirrors `vike_execution::recon::run_pass`, the offline twin of this composition).
        let fill_order_ids: std::collections::HashSet<String> =
            fills.iter().map(|f| f.venue_order_id.to_string()).collect();
        let adopt =
            generate_missing_orders.then_some(vike_execution::recon::ReconcileAdoptContext {
                pass_fill_order_ids: &fill_order_ids,
                seen_trade_ids: local_view.seen_trade_ids,
            });
        // Stamp THIS core's own instance origin onto the pass's policy, so a divergence naming
        // another deployment's order is recognised rather than folded blind
        // (`vike_execution::recon::mode_applies_divergence`). The tag comes off the order client id GENERATOR, not
        // off configuration: it is the identity actually on the wire, and it is restored from the
        // journal's `order_client_id_session`, so a replay reaches the same fold decision on any box —
        // `ReconcilePolicy::local_instance_origin` argues both halves. An explicitly-supplied identity
        // wins, and a core minting untagged ids stamps nothing (byte-identical to before).
        let mut policy = policy;
        if policy.local_instance_origin.is_none()
            && let Some(tag) = self.order_client_id_gen.origin()
            // An unparseable tag cannot happen while `with_origin` is the only stamper (it starts
            // from a validated `OrderClientIdOrigin`), so the `Err` fall-through is a fail-SAFE rather
            // than a real branch: no identity means exactly today's behaviour.
            && let Ok(origin) = vike_model::OrderClientIdOrigin::parse(tag)
        {
            policy.local_instance_origin = Some(origin);
        }
        let recon = vike_execution::recon::resolve(divergences, &policy, None, adopt);
        (
            recon,
            BalancePlan {
                seed: balance_seed,
                roll: balance_roll,
                crossed: balance_crossed,
                adopted: None,
            },
        )
    }

    /// **A deposit needs no human: under the policy that opts in (`quarantine`, the default) the
    /// pass ITSELF adopts a held `BalanceDrift` whose venue cash is ABOVE the expected local figure
    /// by more than the tolerance** (the tolerance is `diff_balance`'s and is not widened here: a
    /// move inside it raises no drift at all). Option B of the decision record
    /// `docs/decisions/0179-a-pure-increase-of-free-balance-is-adopted-by-reconcile-under-quarantine.md`,
    /// owner ruling 2026-10-31 ("who all the time will click manual?").
    ///
    /// It moves the alert `resolve` raised into `recon.events`: the SAME `Event::AccountState` an
    /// operator `confirm_reconcile` would fold, through the same `publish_to`. `apply_reconcile_balance`
    /// then writes the ABSORBED line and ring note (the wording a folding policy already writes)
    /// because no `BalanceDrift` alert is left; the fold re-anchors the cash baseline on the venue's
    /// figure, so the next pass agrees and the announcement is one-shot.
    ///
    /// ⚠ **Adopted only when NOTHING ELSE is held or moved for this account** (the guard the record
    /// did not carry, added with it): an unexplained increase includes the realized profit of a
    /// position closed EXTERNALLY, and adopting the cash while the book still holds the phantom (or
    /// still owes the unbooked fill) counts that profit twice once the row is confirmed. So it stays
    /// HELD while this pass raised, or the account's store still holds beyond this pass's
    /// reach, any kind that `ReconcileDivergenceKind::blocks_balance_adoption` names (position rows, a
    /// `MissingFill`, `MissingTerminal`, `UnknownOrder`, `JournalDivergence`), or while the pass folded
    /// any other event. It is adopted on the first pass where none stands. A row of ANOTHER account
    /// never blocks (the store is read by `route_key`), and a standing row this pass covers and no
    /// longer raises is about to be pruned, so it does not block either (`ReconcilePassCover::covers`;
    /// a `MissingFill` is never covered, so it blocks until an operator confirms it).
    ///
    /// ⚠ **And only while the BOOK IS FLAT, on the SAME figure twice** (decision 0179's amendment of
    /// 2026-10-31, the answer to two failures an independent review built on this rule):
    ///
    /// * **Flat book** ([`Self::book_is_flat`]): the account holds no open position and no live order.
    ///   Cash that a POSITION causes (an IBKR short's proceeds, a perp wallet figure that includes
    ///   unrealized PnL) cannot be told from a deposit without a per-venue statement of what the
    ///   venue's balance figure means, and a live order may have filled at the venue before its fill
    ///   reached this book. Otherwise the increase stays HELD, exactly as before this rule existed;
    ///   it is adopted by the first pass that finds the book flat.
    /// * **Seen twice** ([`Self::previous_pass_saw_this_balance`]): the account's PREVIOUS pass held a
    ///   `BalanceDrift` carrying exactly this venue figure. A funding payment or a closing fill that
    ///   the venue has credited but this book folds a moment later (bybit, okx and hyperliquid fold
    ///   funding from a 60 s poller) would otherwise be adopted from the venue AND booked again by its
    ///   own event. A real deposit lands one pass later; a figure that moves between two passes is
    ///   never adopted. The standing row IS the memory: no new state, no journal frame.
    ///
    /// ⚠ A DECREASE is never adopted here: a withdrawal or a liquidation is exactly the surprise
    /// decision 0043 holds, and `hybrid`, `synthesize` and `external-quarantine` behave as they did.
    ///
    /// Cold path: one pass, interval cadence, on the fold thread like the rest of this file. It
    /// reads `reconcile_alerts`, `reconcile_pass`, the engine's positions and registry and the pass's own
    /// result, and touches nothing per message. It depends on the account's held store, as
    /// `confirm_reconcile` does; `replay_offline` re-derives it from the replayed tail (the module doc of
    /// `reconcile_adopts_deposit_replay.rs` has the argument and the pins).
    fn adopt_a_pure_balance_increase(
        &self,
        index: usize,
        account_key: &str,
        cover: reconcile_pass::ReconcilePassCover,
        adopts: bool,
        plan: &mut BalancePlan,
        recon: &mut vike_execution::recon::ReconcilePassOutcome,
    ) {
        let Some((expected, venue_balance)) = plan.crossed else { return };
        if !adopts || venue_balance <= expected {
            return;
        }
        let Some(at) = recon
            .alerts
            .iter()
            .position(|a| a.kind == vike_execution::recon::ReconcileDivergenceKind::BalanceDrift)
        else {
            return;
        };
        if self.balance_adoption_is_blocked(account_key, cover, recon)
            || !self.book_is_flat(index)
            || !self.previous_pass_saw_this_balance(index, account_key, venue_balance)
        {
            return;
        }
        recon.events.extend(recon.alerts.remove(at).proposed_events);
        plan.adopted = Some(BalanceAdoption::Deposit);
    }

    /// **A fill or a liquidation this book did not fold still reaches the balance, with no operator:**
    /// the pass takes the venue's figure, up or down, for the two passes after one
    /// (`Account::note_unfolded_cash`: a hand trade on a symbol the engine does not accept, a liquidation
    /// of a position the book does not hold). Nothing else carries that money here since a perpetual's
    /// wallet frame of a fill is no longer folded, and on bybit and okx nothing ever did, so its
    /// `BalanceDrift` used to freeze the balance until a confirm.
    ///
    /// The same event a confirm folds, under the same guards against money owed elsewhere as a deposit
    /// ([`Self::balance_adoption_is_blocked`]) and under the same policy word. What differs from
    /// [`Self::adopt_a_pure_balance_increase`] is that a decrease is taken too, because the refusal
    /// EXPLAINS the move, and that one sighting is enough, for the same reason.
    ///
    /// ⚠ **Beside an open position only on a venue whose figure is wallet cash**
    /// (`vike_venues::balance_is_wallet_cash`, and no `Cash` position, which moves any wallet by its
    /// notional): the statement decision 0179 waits for. Elsewhere it waits for a flat book, inside the
    /// leave. ⚠ A third party's withdrawal in the same two passes is taken with it, and the ABSORBED line
    /// names the amount.
    ///
    /// Cold: one pass. A replay re-derives it: the refusal and the pass are journaled, and the first pass
    /// after a base `Snapshot` adopts the venue's figure anyway (`Account::restore` keeps no anchor).
    fn adopt_cash_the_book_left_out(
        &self,
        index: usize,
        account_key: &str,
        cover: reconcile_pass::ReconcilePassCover,
        adopts: bool,
        plan: &mut BalancePlan,
        recon: &mut vike_execution::recon::ReconcilePassOutcome,
    ) {
        let engine = self.engine(index);
        if !adopts || plan.adopted.is_some() || !engine.account.has_unfolded_cash() {
            return;
        }
        let Some(at) = recon
            .alerts
            .iter()
            .position(|a| a.kind == vike_execution::recon::ReconcileDivergenceKind::BalanceDrift)
        else {
            return;
        };
        let cash_is_wallet = vike_venues::balance_is_wallet_cash(&engine.venue, &engine.symbol)
            && !engine
                .account
                .positions
                .values()
                .any(|p| p.size != 0.0 && p.margin_mode == vike_model::MarginMode::Cash);
        if self.balance_adoption_is_blocked(account_key, cover, recon)
            || !(cash_is_wallet || self.book_is_flat(index))
        {
            return;
        }
        recon.events.extend(recon.alerts.remove(at).proposed_events);
        plan.adopted = Some(BalanceAdoption::LeftOut);
    }

    /// The guard both adoptions share: the pass folded something else, or this account holds (or this
    /// pass raised) a row that owes money the venue's figure may already hold, so taking the figure now
    /// would count it twice once the row is confirmed (`ReconcileDivergenceKind::blocks_balance_adoption`). A row
    /// of another account never blocks; a standing row this pass covers and no longer raises is about to
    /// be pruned, so it does not block either.
    fn balance_adoption_is_blocked(
        &self,
        account_key: &str,
        cover: reconcile_pass::ReconcilePassCover,
        recon: &vike_execution::recon::ReconcilePassOutcome,
    ) -> bool {
        !recon.events.is_empty()
            || recon.alerts.iter().any(|a| a.kind.blocks_balance_adoption())
            || self.reconcile_alerts.values().any(|h| {
                h.route_key == account_key
                    && h.kind.blocks_balance_adoption()
                    && !cover.covers(h.kind)
            })
    }

    /// The flat-book half of [`Self::adopt_a_pure_balance_increase`]: `true` when engine `index` holds
    /// no open position (any non-zero `size`, the engine's own test: a closed position leaves a
    /// zero-size entry behind) and no LIVE order (`OrderStatus::is_live`: every status but the five
    /// terminal ones and `Liquidated`, so an order not yet answered, a resting one and a
    /// partially filled one all count, whatever their kind or side). A resting order is a cash
    /// source that has not landed yet: it may have filled at the venue, and the venue's balance
    /// may already carry that fill while this book has not. A core-held conditional that is only
    /// ARMED is not in the registry and does not count: it cannot have touched the venue's cash.
    fn book_is_flat(&self, index: usize) -> bool {
        let engine = self.engine(index);
        !engine.account.positions.values().any(|p| p.size != 0.0)
            && !engine.registry.values().any(|o| o.status.is_live())
    }

    /// The seen-twice half of [`Self::adopt_a_pure_balance_increase`]: `true` when the account's
    /// latest DELIVERED pass (`reconcile_pass`, still the previous one: this pass opens its own mark in
    /// `hold_reconcile_alerts`, after this call) raised a `BalanceDrift` row whose correcting
    /// `AccountState` carries exactly `venue_balance` for the engine's quote asset. Exact equality on
    /// purpose: it is the same decimal read twice, and a venue figure that jitters between passes
    /// is simply held (the old behaviour), never adopted. A row from an older pass (a pass since
    /// then reported no balance) is not the previous pass's sighting.
    fn previous_pass_saw_this_balance(
        &self,
        index: usize,
        account_key: &str,
        venue_balance: f64,
    ) -> bool {
        let Some(mark) = self.reconcile_pass.get(account_key) else { return false };
        let asset = self.engine(index).quote_asset.as_str();
        self.reconcile_alerts.values().any(|h| {
            h.route_key == account_key
                && h.kind == vike_execution::recon::ReconcileDivergenceKind::BalanceDrift
                && h.raised_sequence == mark.sequence
                && h.proposed_events.iter().any(|e| {
                    matches!(e, vike_model::events::Event::AccountState(s)
                        if s.balances.iter().any(|(a, v)| a.as_str() == asset && *v == venue_balance))
                })
        })
    }

    /// The balance-application step of [`Self::reconcile_reports`], between `resolve` and the event
    /// fold: the Feature 2 FIRST SYNC and ROLL-FORWARD writes plus the folded-crossing line, or the
    /// legacy silent seed when `reconcile_balance` is off. It writes `recon.snapshot.balance`, the
    /// routed engine's `Account` and the ring, and folds no event. Each write is argued by the
    /// comment block below, which moved here verbatim from `reconcile_reports` — so "this fn's
    /// doc" in it means [`Self::reconcile_reports`]'s.
    fn apply_reconcile_balance(
        &mut self,
        index: usize,
        venue: &str,
        account_key: &str,
        quote_asset: &str,
        plan: &BalancePlan,
        balance: Option<f64>,
        reconcile_balance: bool,
        recon: &mut vike_execution::recon::ReconcilePassOutcome,
    ) {
        let BalancePlan {
            seed: balance_seed,
            roll: balance_roll,
            crossed: balance_crossed,
            adopted,
        } = *plan;
        // Balance application. Two mutually-exclusive paths, gated by `reconcile_balance`:
        //
        // - Feature 2 ON: any BEYOND-tolerance drift already resolved into `recon.events`
        //   (synthesize) or `recon.alerts` (quarantine/hybrid) above. What is left here is the two
        //   writes that are NOT an adoption of the venue's number:
        //
        //     1. `balance_seed` — the FIRST SYNC, and the only place a venue cash figure enters
        //        the local book without either agreeing with it or an operator confirming it.
        //        Flips the account to `Authoritative` and anchors the realized-PnL baseline.
        //     2. `balance_roll` — the ROLL-FORWARD, applied on EVERY diffed pass thereafter, drift
        //        or no drift. It writes `expected` (the local figure with this daemon's own
        //        realized PnL since the anchor folded in) and moves the baseline to `realized_pnl`.
        //        Two things make it not an adoption: the venue's figure appears nowhere in it, and
        //        it is bit-invariant for the diff — the next pass's `expected` is unchanged by
        //        having performed it (`crates/vike-execution/src/recon/diff_tests.rs`'s
        //        `rolling_the_local_figure_forward_leaves_expected_invariant` is the identity).
        //        Equity does not move either: `Account::realized_since_balance_sync` read that PnL from
        //        the anchor until now, between passes, when the margin sweep runs. What the write BUYS
        //        is that the `balance` an operator reads and a `Snapshot` holds stays current. Applied on a
        //        DRIFT pass too, deliberately: a divergence held for an operator must not freeze it
        //        while the account keeps trading.
        //
        //   ⚠ Neither write RE-ANCHORS onto venue truth, and that is the whole point. The
        //   every-pass adopt this replaced moved the baseline to the venue's own number, so the
        //   next pass measured drift from there and a sub-threshold third-party move was absorbed
        //   again and again with nothing raised — no cumulative bound at all.
        //
        // - Feature 2 OFF (default): the LEGACY silent authoritative seed, byte-identical to before
        //   this feature — NOT via `Engine::apply_snapshot` (its order-reap would
        //   terminalize every live order given an otherwise-empty snapshot; see this fn's doc).
        //   ⚠ This path adopts the venue's figure on every pass and computes no tolerance at all,
        //   so the bound above does NOT reach it. Bounding it means gating the seed on the POLICY,
        //   which is `docs/decisions/0048-the-equity-a-strategy-sizes-against-is-capped-not-disputed.md`'s
        //   refused half; what protects a deployment on this path is
        //   `vike_config::Policy::max_sizing_equity`, the ceiling on what the figure may be USED
        //   for.
        //
        // `None` balance (not reported / fetch failed) leaves balance/mode untouched in both.
        if reconcile_balance {
            if let Some(b) = balance {
                recon.snapshot.balance = b;
            }
            if let Some(b) = balance_seed {
                let account_name = &mut self.engine_mut(index).account;
                account_name.balance = b;
                account_name.balance_mode = BalanceMode::Authoritative;
                account_name.realized_pnl_at_balance_sync = Some(account_name.realized_pnl);
                self.note(format!(
                    "RECON balance {venue}: adopted authoritative {b} [account {account_key}]"
                ));
            } else if let Some(expected) = balance_roll {
                // No `balance_mode` write: `Anchored` is reachable only from an account that is
                // already `Authoritative` WITH a baseline (see `diff_balance`), so setting it here
                // would be a second, weaker copy of that rule. No ring note either — this is the
                // quiet steady state, and a per-pass note is what buried the ring before.
                //
                // ⚠ **DECLARED CONSEQUENCE: past a crossing under a HOLDING policy the local
                // figure FREEZES.** This arm runs on a drift pass too (deliberately — a held
                // divergence must not stall equity while the account keeps trading), and it writes
                // the LOCAL number. So once a drift is standing and the policy holds it, local
                // `balance` stays at the anchor plus this daemon's own realized PnL and never
                // converges on the venue: on a downward bleed it freezes HIGH, where the every-pass
                // adopt this replaced tracked it DOWN. That over-states `resolved_equity`, which
                // is what the margin-call sweep judges solvency against — it under-liquidates —
                // and, if `max_sizing_equity` is unset or above the frozen figure, what sizing
                // multiplies against. Accepted, not overlooked: the freeze is the dual of refusing
                // to adopt an unexplained figure, and it was ALREADY the behaviour for a single
                // over-tolerance move (that pass wrote nothing at all before this change).
                //
                // ⚠ **What ENDS it is thinner than it looks.** One of the paths a reader would
                // reach for does not exist in a shipped binary at all, and the fact does not
                // depend on a venue: no runtime path calls `Engine::apply_snapshot`
                // (tests and the frozen `fixtures/r5/` replay only). The operator "act",
                // `Command::ConfirmReconcile`, now has a door (`vike-cli trade recon confirm`,
                // `CommandWire::ReconcileConfirm`), so a held `BalanceDrift` ends when an operator
                // confirms it; until one does, the freeze stands. The one drift that needs no
                // operator is a pure INCREASE under `quarantine` with nothing else held for the
                // account, a flat book and the same venue figure on two passes:
                // `adopt_a_pure_balance_increase` folds it in the second pass, so this arm
                // sees it standing for one pass at most. A process
                // RESTART re-anchors, because `Account::restore` restores the anchor as `None`.
                // ⚠ Whether anything ELSE ends it on a given deployment turns on whether that
                // venue receives a live wallet frame, which is derived and gated by
                // `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs` — deliberately not restated
                // here, and not summarised into a per-venue consequence either: the last five
                // attempts to write that sentence were each wrong in a different way.
                //
                // ⚠ It is not silent, but the alerting is a CEILING rather than a floor.
                // `reconcile_held` warns on the pass a divergence ENTERS the held set, and re-states
                // the standing set only after `HELD_SUMMARY_INTERVAL_MS` (one hour) of that
                // VENUE saying nothing at all — `ReconcileHeldAnnouncer::observe` keeps one
                // `last_announce_ms` per venue and any changed pass re-arms it. So a second
                // divergence churning on the same venue more often than hourly starves the summary
                // entirely, and the standing drift is announced ONCE and then never again. "Told
                // once, then hourly" is the best case, not the guarantee. Alongside it there is
                // `max_sizing_equity` on the sizing half. Bounding the figure instead would be the
                // disputed-balance FLOOR that
                // `docs/decisions/0048-the-equity-a-strategy-sizes-against-is-capped-not-disputed.md`
                // refuses, for the reasons that record gives.
                let account_name = &mut self.engine_mut(index).account;
                account_name.balance = expected;
                account_name.realized_pnl_at_balance_sync = Some(account_name.realized_pnl);
            }
            // ⚠ **THE CROSSING IS THE ONE MOMENT AN OPERATOR CAN ACT ON, and under a FOLDING
            // policy it is invisible.** `vike_execution::recon::resolve`'s `BalanceDrift` arm emits the
            // correcting `Event::AccountState` and NO alert (its own
            // `balance_drift_synthesize_folds_one_account_state_no_alert` pins that), so a folding
            // policy lands the venue's figure in the book with nothing said — which is exactly the
            // silent absorption this change exists to end, arriving one band later.
            // A HELD drift needs no line from here: it IS an alert, and `reconcile_held` below already
            // announces the transition and summarises the standing set.
            //
            // ⚠ **Which policies fold a `BalanceDrift` is `vike_execution::recon::mode_applies`'s
            // answer, and this comment does not repeat it.** It used to, naming `hybrid` — which
            // does NOT fold — and the correction then enumerated the set instead, which is the same
            // defect one round later. `crates/vike-execution/src/recon/types.rs`'s
            // `hybrid_quarantines_only_no_local_origin_kinds` pins the real rule beside the code it
            // governs. This line fires on whatever `mode_applies` folds, which is deliberately
            // narrow; that narrowness is the residual, not a reason to widen the condition.
            //
            // So the condition is read off the RESOLVED alerts rather than by asking the policy a
            // second time — one authority for what happened to this divergence, and no second copy
            // of the fold rule beside it. It is ONE-SHOT rather than a per-pass repetition (the
            // flood `reconcile_held` exists to stop): a folded `AccountState` re-anchors the baseline
            // through `Account::apply_account_state`, so the very next pass agrees.
            if let Some((expected, venue_balance_value)) = balance_crossed
                && !recon
                    .alerts
                    .iter()
                    .any(|a| a.kind == vike_execution::recon::ReconcileDivergenceKind::BalanceDrift)
            {
                let unexplained = venue_balance_value - expected;
                // `adopted`: the pass itself took a drift that `quarantine` would otherwise hold
                // (`adopt_a_pure_balance_increase`, `adopt_cash_the_book_left_out`), so the line says
                // why instead of naming a folding policy. Same anchor and same amounts either way.
                let why = match adopted {
                    Some(BalanceAdoption::Deposit) => {
                        "the venue's cash is ABOVE the expected figure by more than the tolerance, the \
                         same figure on two passes, the book is flat and nothing else is held for this \
                         account, so the pass adopted it as a deposit (a decrease would have been held)"
                    }
                    Some(BalanceAdoption::LeftOut) => {
                        "this book refused a fill or a liquidation of this account within the last two \
                         passes and nothing else is held for it, so the pass took the venue's figure, \
                         which holds that money"
                    }
                    None => {
                        "the venue's figure was folded because this policy auto-applies BalanceDrift"
                    }
                };
                tracing::warn!(
                    target: "vike_core::recon",
                    venue = %venue,
                    account = %account_key,
                    asset = %quote_asset,
                    expected,
                    venue_balance = venue_balance_value,
                    unexplained,
                    "cash reconcile ABSORBED an unexplained balance move — {why}"
                );
                let adopted_note = match adopted {
                    Some(BalanceAdoption::Deposit) => {
                        " — adopted as a deposit: same figure on two passes, flat book, nothing else held for this account"
                    }
                    Some(BalanceAdoption::LeftOut) => {
                        " — taken: holds a fill or a liquidation this book did not fold, nothing else held for this account"
                    }
                    None => "",
                };
                self.note(format!(
                    "RECON balance {venue} {quote_asset}: absorbed {unexplained:+} unexplained \
                     (venue {venue_balance_value} vs expected {expected}){adopted_note} [account {account_key}]"
                ));
            }
        } else if let Some(b) = balance {
            recon.snapshot.balance = b;
            let account_name = &mut self.engine_mut(index).account;
            account_name.balance = b;
            account_name.balance_mode = BalanceMode::Authoritative;
            // Equity reads the realized PnL since this figure (`Account::realized_since_balance_sync`).
            // `diff_balance` never runs on this path, and a flag change needs a restart, which clears it.
            account_name.realized_pnl_at_balance_sync = Some(account_name.realized_pnl);
            self.note(format!(
                "RECON balance {venue}: seeded authoritative {b} [account {account_key}]"
            ));
        }
    }

    /// The held-alert step of [`Self::reconcile_reports`], after the event fold: the announcer's
    /// transitions for this ACCOUNT's held set, then the store/refresh loop over `reconcile_alerts`.
    /// The transitions are returned so the pass line can ask `silent()`. It stays in this file:
    /// `crates/vike-core/tests/wiring/held_divergence_is_visible_gate.rs` reads the raise site's
    /// text here.
    fn hold_reconcile_alerts(
        &mut self,
        venue: &str,
        account_key: &str,
        alerts: Vec<vike_execution::recon::ReconAlert>,
        cover: reconcile_pass::ReconcilePassCover,
    ) -> recon_held::ReconcileHeldTransitions {
        // Stamp this pass FIRST: rows raised below carry `seq`; the closing prune drops the covered rest.
        let seq = self.open_reconcile_pass(account_key, cover);
        // ⚠ A HELD divergence is a STATE, not an EVENT. Every alert below used to emit its own
        // `tracing::warn!` on EVERY pass, and under `quarantine` (what the the CI box daemon runs) a
        // divergence nothing heals is re-diffed and re-held every interval forever: 396 identical
        // WARN lines on 2026-08-25 and 345 on 2026-08-24, all one bybit `PositionOnlyExternal`.
        // So what is logged is now the TRANSITIONS of this ACCOUNT's held set — the identity
        // choice, the summary cadence and the argument for both live in
        // `crate::runtime::recon_held`. Nothing about what folds, what holds, which alerts are
        // STORED, their confirm ids or the published snapshot changes here; only which of them
        // reach the log.
        //
        // ⚠ **ACCOUNT's, not VENUE's, and that word is the whole of the fifty-account fix.** A
        // pass runs per account, so one exchange arrives here as N calls, each carrying only its
        // own account's raised set — and `observe` REPLACES its key's set with what it is handed.
        // Keyed by venue, leg i+1 erases leg i: 49 of 50 accounts' parked alerts are reported
        // CLEARED (the good-news line) while they are still held, all 50 are re-announced as new
        // on the next pass, and the backlog summary can never fire because the venue's set never
        // sits still. `reconcile_held`'s module doc works it through.
        let raised: std::collections::BTreeSet<recon_held::ReconcileHeldId> = alerts
            .iter()
            .map(|a| {
                recon_held::ReconcileHeldId::new(
                    venue,
                    account_key,
                    a.kind,
                    a.dedup_key.as_deref(),
                    a.identity_detail.as_deref().unwrap_or(&a.detail),
                    &a.proposed_events,
                )
            })
            .collect();
        let transitions = self.reconcile_announce.observe(account_key, self.engine.now_ms, raised);
        for id in &transitions.cleared {
            recon_held::info_cleared(id);
        }
        if let Some(summary) = &transitions.summary {
            recon_held::warn_summary(venue, account_key, summary);
        }
        for a in alerts {
            // This alert's ANNOUNCEMENT identity (see the block above). Computed before `a` is
            // consumed, and with the SAME constructor `confirm_reconcile` uses, so a confirmed-then-
            // recurring divergence cannot be announced under a second key.
            let ident = recon_held::ReconcileHeldId::new(
                venue,
                account_key,
                a.kind,
                a.dedup_key.as_deref(),
                a.identity_detail.as_deref().unwrap_or(&a.detail),
                &a.proposed_events,
            );
            let announce = transitions.newly_held.contains(&ident);
            self.file_held_alert(venue, account_key, a, ident, announce, seq);
        }
        // A covered kind this pass did not raise has gone away: not confirmable (`reconcile_pass`'s module doc).
        self.prune_stale_reconcile(account_key);
        transitions
    }

    /// File ONE held alert in the store: refresh the row that already holds its identity, or add a new
    /// one under its confirm id, and say so once when `announce` (the announcer's verdict, computed by
    /// the caller from the SAME [`recon_held::ReconcileHeldId`]). Shared by the pass ([`Self::hold_reconcile_alerts`])
    /// and the one divergence raised between passes ([`Self::hold_nothing_to_reduce`]), so the two cannot
    /// file a row differently. `seq` is the sequence the row carries (`reconcile_pass`'s freshness).
    ///
    /// The body moved here from `hold_reconcile_alerts`'s loop unchanged, and so did every comment in it;
    /// `crates/vike-core/tests/wiring/held_divergence_is_visible_gate.rs` reads this file's text.
    fn file_held_alert(
        &mut self,
        venue: &str,
        account_key: &str,
        a: vike_execution::recon::ReconAlert,
        ident: recon_held::ReconcileHeldId,
        announce: bool,
        seq: u64,
    ) {
        // ⚠ A recurring divergence REFRESHES its held row in place — same confirm id,
        // freshened payload — so a pass adds NO new alert row for something already held.
        // This used to apply only to alerts carrying a `dedup_key`; everything else appended a
        // row per pass with a fresh id, and nothing in this tree ever prunes the store, so a
        // quarantined divergence nothing heals grew it forever (see `ReconcileHeldAlert::identity`
        // for the the CI box measurement).
        //
        // ⚠ SEMANTIC CHANGE, stated because it is not a refactor: collapsing N per-pass rows
        // into one means a single `Command::ConfirmReconcile` now resolves what previously needed
        // N confirms. That is what an operator wants — the N rows described ONE divergence and
        // confirming one of them folded a stale snapshot of it — but it IS a change to the
        // confirm surface. A genuinely different divergence still gets its own row and its own
        // id; that is exactly what `recon_held::ReconcileHeldId` is for.
        //
        // ⚠ **This match is per ACCOUNT because `ident` carries one**, and until it did, this
        // line was the held path's own misroute: a `dedup_key` names an instrument and a side
        // (`position:BTCUSDT:Both`), never a book, so fifty accounts holding one divergence
        // produced FIFTY EQUAL identities. The first account's row absorbed all fifty — its
        // `detail`/`proposed_events` refreshed below with whichever leg ran last, its
        // `route_key` still naming the first account — so 49 accounts got no row and no
        // confirm id at all, and the one confirm that existed folded another account's
        // synthesized fills into the first account's book.
        let existing =
            self.reconcile_alerts.iter().find(|(_, h)| h.identity == ident).map(|(&id, _)| id);
        if let Some(id) = existing {
            // A refreshed row is normally an ALREADY-announced identity, so this is silent. It
            // can only be `true` if the announcer forgot the identity (an operator confirm)
            // while the row survived — which is news by construction, and is announced against
            // the row's EXISTING confirm id rather than a fabricated one.
            if announce {
                recon_held::warn_newly_held(id, &ident, &a.detail);
            }
            let held = self.reconcile_alerts.get_mut(&id).expect("found above");
            // ⚠ The payload is refreshed UNCONDITIONALLY, but the ring note is gated on the
            // operator-facing TEXT alone. `resolve`'s synthesized position legs bake the pass
            // clock into their `ts`/`trade_id`/`order_client_id`
            // (`vike_execution::recon::resolve`'s `synth_position_legs`), so `proposed_events`
            // differs on EVERY pass for an unchanged divergence — keeping it in the
            // note condition would move the per-minute repetition this batch is removing out
            // of the log and into the bounded recent-events ring, flushing real events out of
            // it. Refreshing the payload anyway is the point: a confirm must fold the LATEST
            // view of the divergence, not one from hours ago.
            let detail_changed = held.detail != a.detail;
            held.detail = a.detail;
            held.proposed_events = a.proposed_events;
            held.recover_orders = a.recover_orders;
            held.orphan_position = a.orphan_position;
            held.raised_sequence = seq;
            held.confirm_refused = a.confirm_refused;
            if detail_changed {
                self.note(format!(
                    "RECON alert #{id} {:?} refreshed (awaiting confirm) [account {account_key}]",
                    a.kind
                ));
            }
            return;
        }
        // The id is the identity's digest, stable across a restart (`ReconcileHeldId::confirm_id`); no row
        // holds THIS identity (`existing`), so an occupied id is a 48-bit collision: probe on.
        let mut id = ident.confirm_id();
        while self.reconcile_alerts.contains_key(&id) {
            id = (id + 1) & recon_held::CONFIRM_ID_MASK;
        }
        // ⚠ The `[account …]` suffix is appended rather than woven in: it keeps the note's
        // leading text byte-identical for `crates/vike-core/tests/wiring/held_divergence_is_visible_gate.rs`,
        // which anchors the raise SITE on this literal. On a venue with one account it prints
        // the venue id, as every other `account` field on this path does.
        self.note(format!(
            "RECON alert #{id} {:?}: {} (awaiting confirm) [account {account_key}]",
            a.kind, a.detail
        ));
        // ...and SAY SO where an operator can read it. `note` writes the in-memory ring, which
        // is reachable only through the control channel — and that channel is OFF on the
        // shipped daemon unless an operator turns it on. Measured on the live the CI box box
        // 2026-08-24: `reconcile pass folded venue=bybit events=0 alerts=2` every 60s for
        // hours, zero WARN lines, no way to learn WHICH two divergences were held. The count
        // reached the log and the content did not.
        //
        // ⚠ This is a per-BOUNDARY line, not a per-message one, so it does not violate the
        // hot-fold logging rule (`crates/vike-core/CLAUDE.md`): a reconcile pass runs on the
        // interval cadence, and a HELD divergence is a fault transition — exactly the class
        // that rule says to instrument. Raising is `warn` because it needs an operator.
        //
        // ⚠ ...but ONLY on the pass the divergence ENTERS the held set. The line above used to
        // fire unconditionally, which under `quarantine` meant every 60s forever for a
        // divergence nothing heals — measured on the CI box, 396 lines in a day for ONE bybit
        // `PositionOnlyExternal`. A repetition is not a transition, and burying the NEW
        // divergence under 396 copies of an old one is the opposite of instrumenting a fault.
        // `reconcile_held` owns that decision; the backlog stays visible through its periodic
        // summary, and a divergence going away is announced too.
        if announce {
            recon_held::warn_newly_held(id, &ident, &a.detail);
        }
        self.reconcile_alerts.insert(
            id,
            ReconcileHeldAlert {
                venue: venue.to_string(),
                route_key: account_key.to_string(),
                kind: a.kind,
                detail: a.detail,
                proposed_events: a.proposed_events,
                recover_orders: a.recover_orders,
                orphan_position: a.orphan_position,
                confirm_refused: a.confirm_refused,
                identity: ident,
                raised_sequence: seq,
            },
        );
    }

    /// **The unreconciled venue's witness: a reduce-only close the VENUE refused with "nothing to
    /// reduce" (`RejectCause::NothingToReduce`) is held for the operator as an `OrphanLocalPosition`.**
    ///
    /// A venue whose reconcile leg has not compared the book yet (dukascopy before its first pass, or with
    /// reconciliation off, or on a jar that cannot answer the question: `Wiring::SidecarExecutionReconcile`) has nothing
    /// that compares the book with the venue, so a position the venue closed without telling the node (a
    /// stop-loss that fired after a sidecar restart, a close made in the platform, a close during a
    /// session loss) stays in the book and counts against the exposure ceilings until a pass raises it or an
    /// account cycle drops the whole engine. The venue's refusal of the flatten is the first moment anything says so;
    /// the engine parks it (`Engine::take_nothing_to_reduce`, from the `OrderRejected` arm of
    /// the fold) and THIS sweep, on the stuck-order clock (`TimerKind::StuckSweep`), holds it.
    ///
    /// ⚠ **Where the cold path is, stated.** Nothing runs on the per-message fold or in `dispatch`: the
    /// fold's only addition is the one cold `OrderRejected` arm that already records the refusal's
    /// reason, and this sweep is a timer arm. A box without the watchdog armed (`submit_ack_timeout`
    /// unset, a replay core) leaves the claims parked and raises nothing.
    ///
    /// ⚠ **It HOLDS and folds NOTHING, under every policy.** No `ReconcilePolicy` reaches this sweep (the
    /// policy rides inside a pass), and none could widen it: the only fold is the operator's own
    /// `recon confirm`, which drops the local position at its own average price with zero PnL
    /// (`drop_orphan_position`) and refuses when the quantity moved. That is decision 0043's rollout
    /// rule kept to the letter: this is not a `PositionDrift` and writes no position size.
    ///
    /// The held row is the ordinary `OrphanLocalPosition` of a pass (`orphan_local_position_alert`):
    /// the same `position:{symbol}:{side}` identity, so a venue that DOES reconcile has the row
    /// refreshed by its next pass, or pruned by it when the pass covered positions and does not raise it.
    /// A claim for a position the book no longer holds (a strategy or an operator closed it first) raises
    /// nothing. It does not survive a restart: the next refused close raises it again.
    #[cold]
    #[inline(never)]
    pub(crate) fn hold_nothing_to_reduce(&mut self) {
        for index in 0..=self.extra_engines.len() {
            let claims = self.engine_mut(index).take_nothing_to_reduce();
            // A STOPPED account's engine (decision 0169) is the operator's to close; its claim is moot.
            if claims.is_empty() || self.is_retired(index) {
                continue;
            }
            let (venue, account_key) = {
                let e = self.engine(index);
                (e.venue.clone(), e.route_key.clone())
            };
            for claim in claims {
                // The buckets the refused order meant to reduce: this symbol, held on the side the
                // order opposes. A hedge book has up to two; a one-way book has one.
                let mut held: Vec<(vike_model::events::PositionSide, f64)> = self
                    .engine(index)
                    .account
                    .positions
                    .iter()
                    .filter(|((v, s, _), p)| {
                        v.as_str() == venue.as_str()
                            && s.as_str() == claim.symbol.as_str()
                            && p.size != 0.0
                            && vike_model::is_reducing_direction(claim.side, p.size)
                    })
                    .map(|((_, _, side), p)| (*side, p.size))
                    .collect();
                if held.is_empty() {
                    self.note(format!(
                        "RECON nothing-to-reduce on {venue} {} (order client id {}): the book holds no position \
                         that close could reduce, so nothing is held [account {account_key}]",
                        claim.symbol, claim.order_client_id
                    ));
                    continue;
                }
                held.sort_by_key(|(side, _)| side.to_string());
                for (side, local_quantity) in held {
                    let alert = vike_execution::recon::orphan_local_position_alert(
                        &claim.symbol,
                        side,
                        local_quantity,
                        format!(
                            "local {venue} {} {side} position {local_quantity}: the VENUE refused a \
                             reduce-only close of it (order client id {}) because it holds nothing in that \
                             direction to reduce. The book and the venue disagree (the venue closed \
                             it without telling this node, or it never held it). Nothing is \
                             auto-applied: look at the account at the venue, then `recon confirm` \
                             drops the local position at its own average price (zero PnL, no fee).",
                            claim.symbol, claim.order_client_id
                        ),
                    );
                    let ident = recon_held::ReconcileHeldId::new(
                        &venue,
                        &account_key,
                        alert.kind,
                        alert.dedup_key.as_deref(),
                        alert.identity_detail.as_deref().unwrap_or(&alert.detail),
                        &alert.proposed_events,
                    );
                    let now = self.engine.now_ms;
                    let announce = self.reconcile_announce.raise(&account_key, now, ident.clone());
                    // The pass the account last had, if any: a later pass that covers positions and does
                    // not raise this row prunes it, as the venue's own report contradicting the claim.
                    let seq = self.reconcile_pass.get(&account_key).map_or(0, |m| m.sequence);
                    self.file_held_alert(&venue, &account_key, alert, ident, announce, seq);
                }
            }
        }
        self.dirty = true;
    }

    /// **A fill that cannot be proven new is held for the operator, never booked.** The account refuses a fill
    /// that states NO timestamp, under an id its windowed ledger does not hold, once that ledger holds a floor
    /// (`vike_execution::account::fill_window`: a bounded `Snapshot` left older ids out, so the fill may be one of those
    /// sent again), and the engine parks it (`Engine::take_unproven_fills`). THIS takes the parked fills
    /// of every engine and holds each as a `MissingFill` row carrying the fill, the posture of every held
    /// divergence: visible, and folding nothing under any policy. An operator's `recon confirm` of the row is
    /// refused in words, because the engine would drop the fill (`fill_drop_reason`); a later delivery of the
    /// same fill WITH a stamp the window vouches for books it and clears the row (`clear_folded_missing_fills`).
    /// Called after every pass's event fold (a pass's own `hybrid` fold of such a fill is refused and parked the
    /// same way) and on the stuck-order clock beside [`Self::hold_nothing_to_reduce`]. Cold; one `is_empty` per
    /// engine while nothing is parked.
    #[cold]
    #[inline(never)]
    pub(crate) fn hold_unproven_fills(&mut self) {
        for index in 0..=self.extra_engines.len() {
            let fills = self.engine_mut(index).take_unproven_fills();
            if fills.is_empty() || self.is_retired(index) {
                continue;
            }
            let (venue, account_key) = {
                let e = self.engine(index);
                (e.venue.clone(), e.route_key.clone())
            };
            for fill in fills {
                let alert = vike_execution::recon::ReconAlert {
                    kind: vike_execution::recon::ReconcileDivergenceKind::MissingFill,
                    detail: format!(
                        "fill {} ({venue} {} side {} qty {} px {}) carries NO timestamp and its trade id is not \
                         in the days of fills this account's ledger keeps (restored from a bounded journal \
                         Snapshot): it cannot be told from a fill booked before them, so it was NOT booked. Read \
                         the account at the venue; a confirm books nothing.",
                        fill.trade_id, fill.symbol, fill.side, fill.last_qty, fill.last_price
                    ),
                    dedup_key: Some(format!("unproven-fill:{}", fill.trade_id)),
                    proposed_events: vec![vike_model::events::Event::Fill(fill)],
                    recover_orders: Vec::new(),
                    identity_detail: None,
                    orphan_position: None,
                    confirm_refused: false,
                };
                let ident = recon_held::ReconcileHeldId::new(
                    &venue,
                    &account_key,
                    alert.kind,
                    alert.dedup_key.as_deref(),
                    alert.identity_detail.as_deref().unwrap_or(&alert.detail),
                    &alert.proposed_events,
                );
                let now = self.engine.now_ms;
                let announce = self.reconcile_announce.raise(&account_key, now, ident.clone());
                let seq = self.reconcile_pass.get(&account_key).map_or(0, |m| m.sequence);
                self.file_held_alert(&venue, &account_key, alert, ident, announce, seq);
            }
            self.dirty = true;
        }
    }

    /// Fold-thread handling of `Command::ConfirmReconcile` (Task 17): operator approval of one held
    /// Quarantine/Hybrid-quarantined alert. Looks up its `proposed_events` and folds each through
    /// the SAME [`Self::publish_to`] path real venue events (and a Synthesize-mode reconcile pass)
    /// use — so a confirmed alert folds byte-identically to what an immediate Synthesize would
    /// have produced — then removes it from the held store, so a re-confirm of the same id hits
    /// the unknown-id no-op below. An unknown id (never held, already confirmed, or a replayed
    /// command from a prior process run — see [`Command::ConfirmReconcile`]'s doc) is surfaced to the
    /// recent-events ring rather than silently dropped or panicking.
    ///
    /// The operator's production door is `CommandWire::ReconcileConfirm` (`vike-cli trade recon
    /// confirm <id>`). An `OrphanLocalPosition` row proposes no events: its confirm DROPS the local
    /// position through [`Self::drop_orphan_position`], which books nothing but a zero-PnL close.
    /// So does a `PositionDrift` whose venue row is flat and unpriced, which `resolve` holds under
    /// EVERY policy (`unpriced_position_hold`): closing it at the row's `0.0` price would book the
    /// whole position as a loss. A held row with a NON-flat unpriced venue row
    /// (`ReconcileHeldAlert::confirm_refused`) books nothing at all and says so.
    ///
    /// ⚠ **This is the RETURN leg of the reconcile round trip, and it routes on
    /// [`ReconcileHeldAlert::route_key`] — the key the raising pass resolved — not on `held.venue`.**
    /// The two are equal for every alert this tree raises, so the wrong one would look right and
    /// stay right until a second account of one exchange existed, at which point an operator's
    /// approval would fold synthesized fills and position deltas into the FIRST account's book.
    /// `held.venue` keeps the operator-facing label in the notes below, which is what an operator
    /// asked about.
    pub(crate) fn confirm_reconcile(&mut self, id: u64) {
        let Some(held) = self.reconcile_alerts.shift_remove(&id) else {
            self.note(format!("RECON confirm: unknown alert id {id}"));
            tracing::warn!(
                target: reconcile_pass::CONFIRM_VERDICT_TARGET, id,
                "reconcile confirm REFUSED: unknown id"
            );
            return;
        };
        // Drop this divergence's ANNOUNCEMENT identity along with its row — the SAME key the row
        // was stored under, so the two can never fall out of step. If the confirm did not actually
        // resolve the divergence (the venue keeps reporting it), the next pass re-raises a fresh
        // row — and without this it would do so in SILENCE, leaving the operator to believe an
        // action took that did not. Logging-only; the fold below is untouched.
        self.reconcile_announce.forget(&held.identity);
        // Re-validate against the account's LATEST pass before anything folds (`reconcile_pass`).
        if self.refuse_stale_confirm(id, &held) {
            return;
        }
        // A row `resolve` held for want of a usable venue price (`unpriced_position_hold`) that no
        // local price can stand in for: the confirm books NOTHING. The row is removed like any
        // confirmed one, and the next pass re-raises it while the venue row stays unpriced (the
        // announcement was forgotten above, so it is re-announced too).
        if held.confirm_refused {
            self.note(format!(
                "RECON confirm #{id} {:?} REFUSED, nothing booked (the detail says what to fix at \
                 the venue): {} [account {}]",
                held.kind, held.detail, held.route_key
            ));
            // The ring note alone is not findable once it rolls out; the verdict goes to the log too.
            reconcile_pass::log_confirm(
                true,
                id,
                &held.route_key,
                held.kind,
                "the venue row is unpriced and not flat: nothing booked",
            );
            return;
        }
        let Some(index) = self.engine_index_for_route_key(RouteKey::declared(&held.route_key))
        else {
            // Venues are fixed at spawn, so this should not happen; surface it rather than
            // silently dropping the operator-approved events.
            self.note(format!(
                // ⚠ BOTH strings: `venue` is the operator-facing exchange label this note has
                // always carried, and `route_key` is the ACCOUNT the approved events were meant
                // for. On a box with one account per venue they are equal and this reads as it
                // always did; on a two-account box the venue alone cannot say which book was
                // lost.
                "RECON confirm #{id}: no engine for venue {} (account {}) (approved events dropped)",
                held.venue, held.route_key
            ));
            let why = "no engine for the account";
            reconcile_pass::log_confirm(true, id, &held.route_key, held.kind, why);
            return;
        };
        // A proposed fill the fold would DROP (a symbol the engine does not trade, an unknown contract kind or
        // quote currency, no rate yet, a non-finite number, another venue, a trade id already folded) used to
        // be dropped without a word while the confirm read "applied": polymarket's wallet tokens, the CI box
        // 2026-11-10. `fill_drop_reason` asks the fold's own guards first, and the row is refused whole, in words.
        let engine = self.engine(index);
        let refusal = held.proposed_events.iter().find_map(|event| match event {
            vike_model::events::Event::Fill(f) => engine.fill_drop_reason(f).map(|r| (f.symbol, r)),
            _ => None,
        });
        if let Some((symbol, reason)) = refusal {
            let why = format!(
                "the engine would drop the fill on {symbol}: {}. Nothing booked; the next pass \
                 raises the row again",
                reason.words()
            );
            self.note(format!(
                "RECON confirm #{id} {:?} REFUSED: {why} [account {}]",
                held.kind, held.route_key
            ));
            reconcile_pass::log_confirm(true, id, &held.route_key, held.kind, &why);
            return;
        }
        // The fold's verdict is read back too, so no drop the guards above did not foresee reads "applied".
        let (mut fills, mut dropped_fills) = (0usize, 0usize);
        for event in held.proposed_events {
            let is_fill = matches!(event, vike_model::events::Event::Fill(_));
            let fold = self.publish_to(index, event);
            if is_fill {
                fills += 1;
                dropped_fills += usize::from(fold == vike_execution::EventBusFold::Dropped);
            }
        }
        // Order-loss recovery: re-register any venue orders local state lost (recon JournalDivergence).
        // INSERT-ONLY seed straight into the routed engine's registry — NOT the event fold (a
        // lifecycle event for an unknown order client id is dropped), so this cannot ride `proposed_events`.
        let reregistered = if held.recover_orders.is_empty() {
            0
        } else {
            let n = self.engine_mut(index).reregister_orders(&held.recover_orders);
            // The client hears where they rest too (`announce_recovered_orders`).
            self.announce_recovered_orders(index, &held.venue, &held.recover_orders);
            n
        };
        // `OrphanLocalPosition` (the venue never reported a position local holds), or a
        // `PositionDrift` on a flat unpriced venue row: the operator's confirm DROPS the local row; a
        // spot wallet holding less than the book: it SHRINKS the row to the wallet's figure.
        // Nothing else in `proposed_events` folds for these.
        let dropped = held.orphan_position.as_ref().map(|o| self.drop_orphan_position(index, o));
        // Mirror reconcile_reports: drain any events an in-process client synthesized from the
        // now-folded fills.
        self.pump_client();
        // ⚠ `[account …]` on both: a confirm FOLDS into one book, and at fifty accounts of one
        // exchange the kind and the detail can be identical across all of them — the account is
        // the only thing on the line that says which book just moved. It is `held.route_key`
        // rather than a re-derivation, i.e. the key the RAISING pass resolved and the same one
        // `engine_index_for_route_key` was just handed above. Equal to the venue on a venue with one
        // account.
        // `drop_orphan_position` says "NOT dropped: …" when it refused: that is a refusal, not a fold.
        let partly = (dropped_fills > 0).then(|| {
            format!(
                "NOT booked whole: the engine dropped {dropped_fills} of {fills} proposed fill(s)"
            )
        });
        let outcome =
            partly.as_deref().or(dropped.as_deref()).unwrap_or("folded what the row proposed");
        let refused = outcome.starts_with("NOT");
        reconcile_pass::log_confirm(refused, id, &held.route_key, held.kind, outcome);
        let dropped = partly.or(dropped);
        let dropped = dropped.map(|d| format!(" ({d})")).unwrap_or_default();
        if reregistered > 0 {
            self.note(format!(
                "RECON confirmed #{id} {:?}: {} (re-registered {reregistered} lost order(s)){dropped} \
                 [account {}]",
                held.kind, held.detail, held.route_key
            ));
        } else {
            self.note(format!(
                "RECON confirmed #{id} {:?}: {}{dropped} [account {}]",
                held.kind, held.detail, held.route_key
            ));
        }
    }
}

/// What `reconcile_compute` decided about the balance, handed to `apply_reconcile_balance`: the three
/// `vike_execution::recon::ReconcileBalanceCheck` outcomes as Copy values, so no borrow of the pass's local view
/// outlives `resolve`. All three are `None` when `reconcile_balance` is off.
struct BalancePlan {
    /// `ReconcileBalanceCheck::Adopt` — the FIRST SYNC figure.
    seed: Option<f64>,
    /// `ReconcileBalanceCheck::Anchored`'s `expected` — the ROLL-FORWARD figure.
    roll: Option<f64>,
    /// `(expected, venue_balance)` of a beyond-tolerance `BalanceDrift`, for the crossing line.
    crossed: Option<(f64, f64)>,
    /// The pass itself took that drift, where the policy would otherwise have held it: only the wording
    /// of the crossing line differs.
    adopted: Option<BalanceAdoption>,
}

/// Why a pass took a `BalanceDrift` a holding policy would have held.
#[derive(Clone, Copy)]
enum BalanceAdoption {
    /// `CoreThread::adopt_a_pure_balance_increase`.
    Deposit,
    /// `CoreThread::adopt_cash_the_book_left_out`.
    LeftOut,
}
