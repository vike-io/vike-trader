//! Fold-thread half of reconcile: `reconcile_reports` (reports in, compute on the fold) and the operator `confirm_recon`.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Fold-thread half of the reconcile runtime driver (Option A: reports-in, compute-on-fold).
    /// The reconcile MANAGER thread (`vike_core::recon_manager`) did the blocking REST fetch OFF
    /// this thread and enqueued the raw reports as [`Command::ReconcileReports`]; HERE, on the
    /// single writer, we read THIS engine's own [`ExecutionEngine::local_view`] (cheap, same-thread
    /// — so no cross-thread local-state read and no staleness window), run the PURE
    /// [`vike_exec::recon::diff`] + [`vike_exec::recon::resolve`], and fold each synthesized event
    /// through the SAME [`Self::publish_to`] path real venue events use. An OCCASIONAL command
    /// (startup, plus Task 16's interval cadence), never the per-message hot fold, so the pure
    /// diff/resolve compute here is within budget — the identical reasoning that keeps
    /// `diff_snapshot` on this thread at the `ApplySnapshot` choke point.
    ///
    /// `recon.snapshot` is the `resolve` default (empty) for positions/orders: `resolve` seeds
    /// position state via the synthesized FILL events, not the snapshot, so applying THOSE fields
    /// would be a no-op at best (`apply_snapshot` skips empty positions) and DESTRUCTIVE at worst —
    /// `ExecutionEngine::apply_snapshot`'s order-reap terminalizes any live local order absent from
    /// `snapshot.open_orders` (see `crates/vike-exec/tests/recon/reconcile_reap.rs`), so routing an
    /// all-empty-except-balance snapshot through the full `Command::ApplySnapshot` arm would cancel
    /// EVERY live order on this venue as a side effect. Task 3 (balance activation) therefore does
    /// NOT reuse that arm: `reports.balance` (from [`vike_exec::recon::ReconClient::fetch_balance`])
    /// is seeded directly onto `Account::balance`/`balance_mode` below — the narrowest path that
    /// updates cash without touching positions or the order registry. `recon.snapshot.balance` is
    /// still set (mirrors the `Recon` shape / a future `CoreSnapshot.recon` surface) but is NOT
    /// itself applied.
    ///
    /// ⚠ **On the DIFFED path (`VIKE_RECONCILE_BALANCE=1`) saying "it adopts" of that seed is now
    /// wrong.** Two writes remain, neither of them an adoption of a venue number this daemon cannot
    /// explain: the FIRST SYNC (an account with no venue-anchored baseline — the cold-start adopt)
    /// and the ROLL-FORWARD (the local figure with this daemon's own realized PnL folded in, which
    /// the diff is invariant under and which keeps `Account::equity_all` from going stale in
    /// `Authoritative` mode). A sub-tolerance venue move is neither absorbed nor written: this
    /// pass's anchor does not move onto the venue's figure. Beyond the band it becomes a
    /// `BalanceDrift` like any other divergence. The application site below spells both writes out;
    /// `vike_exec::recon::BalanceCheck` carries the algebra.
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
    /// `ExecutionEngine::apply_snapshot` writes the anchor but is reachable only through
    /// `Command::ApplySnapshot`, whose one production driver would be
    /// `CoreHandle::spawn_periodic_reconcile` — and nothing outside
    /// `crates/vike-core/tests/runtime_smoke.rs` calls that
    /// (`crates/vike-exec/tests/recon/recon_policy_pin.rs` states the same for the reap it guards);
    /// and `Account::restore` restores it as `None` (deliberately absent from `AccountSnapshot`),
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
    /// just a ring note — each is assigned a monotonic id, held in `self.recon_alerts` (its
    /// `proposed_events` kept for `Self::confirm_recon` to fold later), and surfaced structurally
    /// via `CoreSnapshot.recon.alerts` (built by `Self::recon_block` at the next publish). The
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
        let route = self.engine_idx_for_route_key(reports.route());
        // How many engines of this EXCHANGE does this process run? Read before the destructure
        // (it needs `&self` and the payload whole) and used once, by the refusal below. Off the
        // fold — a reconcile pass is interval work — and `1` on every single-account core, where
        // `engines_of_venue` is the venue lookup it has always been.
        let accounts_of_venue = self.engines_of_venue(&reports.venue).len();
        let ReconcileReports {
            venue,
            since: _,
            orders,
            fills,
            positions,
            policy,
            balance,
            generate_missing_orders,
            reconcile_balance,
            balance_tol,
            route_key,
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
        // process runs several ENGINES of names itself (`ReconLeg::name_accounts_of_shared_venues`,
        // applied where the legs are assembled), so a `None` here is a payload this process did not
        // write: a REPLAYED journal `Command::ReconcileReports` from a build that predates the
        // account fan-out, or from a run when this venue genuinely had one account. That is also
        // the one path on which an account can vanish between a pass and its confirm, and refusing
        // is the safe end of it — the pass folds NOTHING, the operator keeps every row already
        // held, and the next live pass re-raises whatever is still true against the right book.
        //
        // ⚠ **That claim is only true because the stamp counts the SAME SET this arm does**, which
        // it did not until 2026-09-14: the stamp counted LEGS while this counts ENGINES, and
        // `crates/vike-mount/src/node/accounts.rs`'s `mount_accounts_of` mounts an engine for every armed account while pushing a
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
        let Some(idx) = route else {
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
        // (`crates/bridges/deribit/src/recon_client.rs`), and it collides only when two Deribit
        // accounts hold the SAME instrument. It moves no book — the map is read-only decoration
        // for a greeks display, never folded.
        for pr in &positions {
            if let Some(d) = pr.delta {
                self.recon_coin_deltas.insert((pr.venue.clone(), pr.symbol.clone()), d);
            }
        }
        // The engine's quote-asset selector for the synthesized/adopted balance (Feature 2). Read
        // before the &mut publish below; unused on the legacy-seed path (`reconcile_balance` off)
        // except by the crossing line, which never fires there.
        let quote_asset = self.eng(idx).quote_asset.clone();
        let (mut recon, plan) = self.reconcile_compute(
            idx,
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
        );
        self.apply_recon_balance(
            idx,
            &venue,
            &account_key,
            &quote_asset,
            &plan,
            balance,
            reconcile_balance,
            &mut recon,
        );
        let n_events = recon.events.len();
        for ev in recon.events {
            self.publish_to(idx, ev);
        }
        // Mirror the Ingest::Event path: drain any events an in-process client synthesized from
        // the folded fills (real venue clients return None here).
        self.pump_client();
        let n_alerts = recon.alerts.len();
        let transitions = self.hold_recon_alerts(&venue, &account_key, recon.alerts);
        self.recon_last_pass_ts = self.engine.now_ms;
        // ⚠ A quiet PASS is a STATE too, and this line was the last per-pass repetition left after
        // the WARN flood above was fixed. It fired on every pass carrying ANY alert, so under
        // `quarantine` it repeated a minute apart forever — `events=0 alerts=1` measured identical
        // on the CI box at 2026-08-25 11:41:43 and 11:42:43 — saying only "the same divergence is still
        // held", which `recon_held`'s announcer now reports properly. INFO, so on a daemon running
        // `VIKE_LOG_FILE_LEVEL=warn` it floods the JOURNAL rather than the log file: the same
        // defect one severity down, which is why it was deferred out of that fix.
        //
        // What still logs EVERY time: a pass that FOLDED events (that changed the book — a fact
        // about this pass, not about the held set, which is why it is ORed in here rather than
        // asked of the announcer), and a pass whose held set CHANGED. What no longer logs: a pass
        // that merely repeated the previous one. The backlog stays visible because `silent()` is
        // also false on the announcer's periodic summary pass, so this line rides that ONE clock
        // instead of a second rate limiter of its own — see
        // `crate::runtime::recon_held`'s `HeldTransitions::silent`.
        //
        // ⚠ The verdict is over the held-set IDENTITIES, not over `n_alerts`: two alerts sharing
        // one identity are one divergence (that is what bounds the store), so a pass whose COUNT
        // moved while its identity set did not is a repetition and stays quiet. And a venue with
        // nothing held at all is silent exactly as it was before this change — a heartbeat where
        // there was never a line is new noise, and `CoreSnapshot.recon.last_pass_ts` (set on the
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
    /// `BalancePlan`. It reads this engine, `config.journal_view_provider`, `coid_gen.origin()` and
    /// `engine.now_ms`, and writes nothing.
    ///
    /// Its comments were written inside `reconcile_reports` and moved here verbatim, so "this fn's
    /// doc" in them means [`Self::reconcile_reports`]'s.
    #[allow(clippy::too_many_arguments)]
    fn reconcile_compute(
        &self,
        idx: usize,
        venue: &str,
        quote_asset: &str,
        orders: &[vike_model::OrderStatusReport],
        fills: &[vike_model::FillReport],
        positions: &[vike_model::PositionStatusReport],
        policy: vike_exec::recon::ReconPolicy,
        balance: Option<f64>,
        reconcile_balance: bool,
        balance_tol: vike_exec::recon::BalanceTol,
        generate_missing_orders: bool,
    ) -> (vike_exec::recon::Recon, BalancePlan) {
        // local_view returns OWNED data — the &self borrow is released before the &mut publish.
        let local = self.eng(idx).local_view();
        // Journal cross-check (#3): build the venue's JournalView from the materialized Tier-2 log
        // (via the root-supplied provider) and pass `Some(&journal)` to enable the three-way check.
        // `None` (provider unset) keeps this pass byte-identical to the two-way (local-vs-venue)
        // comparison — and `None` is every live mount today, because no root supplies a provider
        // (`JournalViewHook`'s doc says who last did). A bounded lookback query on this OCCASIONAL
        // path, not the hot fold.
        let journal = self.config.journal_view_provider.as_ref().map(|p| p(venue));
        let local_view = local.as_view();
        let mut divergences =
            vike_exec::recon::diff(orders, fills, positions, &local_view, journal.as_ref());
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
        // `BalanceTol` on a 250k account that is 25 a pass at the 60 s interval default — about
        // 36k a day, silently, on an account a third party can fund or drain.
        // `vike_exec::recon::BalanceCheck` is the split, and its doc carries the algebra.
        //
        //   - `Adopt` — FIRST SYNC only (`BalanceMode::Delta`, or no `realized_at_sync`). Take the
        //     venue's figure whole and anchor the realized-PnL baseline beside it. This is what
        //     keeps a COLD START usable: without it a fresh live mount stays in `Delta` and judges
        //     every order against the arbitrary `seed_cash`.
        //   - `Anchored { expected, .. }` — ROLL THE LOCAL FIGURE FORWARD, on every diffed pass and
        //     whether or not a drift was raised. It adopts NOTHING: `expected` is
        //     `balance + (realized_pnl − baseline)`, the local number with this daemon's OWN
        //     realized PnL folded in, and writing it back (with the baseline moved to
        //     `realized_pnl`) leaves the next pass's `expected` bit-unchanged. What it BUYS is that
        //     `Account::equity_all` — which in `Authoritative` mode is `balance + unrealized` and
        //     reads no realized-PnL term at all — does not go stale by Σ realized-since-anchor,
        //     which is the only thing the every-pass re-seed was doing correctly.
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
            match vike_exec::recon::diff_balance(
                &local_view,
                balance,
                quote_asset,
                balance_tol,
                self.engine.now_ms,
            ) {
                vike_exec::recon::BalanceCheck::NotReported => {}
                vike_exec::recon::BalanceCheck::Adopt(b) => balance_seed = Some(b),
                vike_exec::recon::BalanceCheck::Anchored { expected, drift } => {
                    balance_roll = Some(expected);
                    if let Some(d) = drift {
                        if let vike_exec::recon::Divergence::BalanceDrift { venue_bal, .. } = &d {
                            balance_crossed = Some((expected, *venue_bal));
                        }
                        divergences.push(d);
                    }
                }
            }
        }
        // `generate_missing_orders` rode in on THIS pass's `ReconcileReports` — the fold thread has
        // no other view of `vike_core::ReconConfig` (see that struct's own doc). It becomes
        // `resolve`'s AdoptContext, built from the same pass's fill reports + the engine's seen
        // trade ids (mirrors `vike_exec::recon::run_pass`, the offline twin of this composition).
        let fill_order_ids: std::collections::HashSet<String> =
            fills.iter().map(|f| f.venue_order_id.to_string()).collect();
        let adopt = generate_missing_orders.then_some(vike_exec::recon::AdoptContext {
            pass_fill_order_ids: &fill_order_ids,
            seen_trade_ids: local_view.seen_trade_ids,
        });
        // Stamp THIS core's own instance origin onto the pass's policy, so a divergence naming
        // another deployment's order is recognised rather than folded blind
        // (`vike_exec::recon::mode_applies_divergence`). The tag comes off the coid GENERATOR, not
        // off configuration: it is the identity actually on the wire, and it is restored from the
        // journal's `coid_session`, so a replay reaches the same fold decision on any box —
        // `ReconPolicy::local_instance_origin` argues both halves. An explicitly-supplied identity
        // wins, and a core minting untagged ids stamps nothing (byte-identical to before).
        let mut policy = policy;
        if policy.local_instance_origin.is_none()
            && let Some(tag) = self.coid_gen.origin()
            // An unparseable tag cannot happen while `with_origin` is the only stamper (it starts
            // from a validated `InstanceOrigin`), so the `Err` fall-through is a fail-SAFE rather
            // than a real branch: no identity means exactly today's behaviour.
            && let Ok(origin) = vike_model::InstanceOrigin::parse(tag)
        {
            policy.local_instance_origin = Some(origin);
        }
        let recon = vike_exec::recon::resolve(divergences, &policy, None, adopt);
        (recon, BalancePlan { seed: balance_seed, roll: balance_roll, crossed: balance_crossed })
    }

    /// The balance-application step of [`Self::reconcile_reports`], between `resolve` and the event
    /// fold: the Feature 2 FIRST SYNC and ROLL-FORWARD writes plus the folded-crossing line, or the
    /// legacy silent seed when `reconcile_balance` is off. It writes `recon.snapshot.balance`, the
    /// routed engine's `Account` and the ring, and folds no event. Each write is argued by the
    /// comment block below, which moved here verbatim from `reconcile_reports` — so "this fn's
    /// doc" in it means [`Self::reconcile_reports`]'s.
    #[allow(clippy::too_many_arguments)]
    fn apply_recon_balance(
        &mut self,
        idx: usize,
        venue: &str,
        account_key: &str,
        quote_asset: &str,
        plan: &BalancePlan,
        balance: Option<f64>,
        reconcile_balance: bool,
        recon: &mut vike_exec::recon::Recon,
    ) {
        let BalancePlan { seed: balance_seed, roll: balance_roll, crossed: balance_crossed } =
            *plan;
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
        //        having performed it (`crates/vike-exec/src/recon/diff_tests.rs`'s
        //        `rolling_the_local_figure_forward_leaves_expected_invariant` is the identity).
        //        What it BUYS is that `Account::equity_all` — `balance + unrealized` in
        //        `Authoritative` mode, with no realized-PnL term at all — does not go stale by
        //        Σ realized-since-anchor, which is the one job the every-pass re-seed was doing.
        //        Applied on a DRIFT pass too, deliberately: a divergence held for an operator must
        //        not freeze equity while the account keeps trading.
        //
        //   ⚠ Neither write RE-ANCHORS onto venue truth, and that is the whole point. The
        //   every-pass adopt this replaced moved the baseline to the venue's own number, so the
        //   next pass measured drift from there and a sub-threshold third-party move was absorbed
        //   again and again with nothing raised — no cumulative bound at all.
        //
        // - Feature 2 OFF (default): the LEGACY silent authoritative seed, byte-identical to before
        //   this feature — NOT via `Command::ApplySnapshot` (that path's order-reap would
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
                let acct = &mut self.eng_mut(idx).account;
                acct.balance = b;
                acct.balance_mode = BalanceMode::Authoritative;
                acct.realized_pnl_at_balance_sync = Some(acct.realized_pnl);
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
                // ⚠ **What ENDS it is thinner than it looks.** Two of the paths a reader would
                // reach for do not exist in a shipped binary at all, and neither fact depends on a
                // venue: `ExecutionEngine::apply_snapshot` is reachable only through
                // `Command::ApplySnapshot`, which nothing sends outside
                // `crates/vike-core/tests/runtime_smoke.rs`; and `Command::ConfirmRecon` — the
                // operator "act" — has NO production constructor at all
                // (`crates/vike-app-core/src/backend/tradehub_control.rs`'s `wire_from_command` returns
                // `None` for it and `crates/vike-tradehub-client/src/wire.rs` calls it
                // core-internal plumbing), which is blocker (1) of the record below. A process
                // RESTART re-anchors, because `Account::restore` restores the anchor as `None`.
                // ⚠ Whether anything ELSE ends it on a given deployment turns on whether that
                // venue receives a live wallet frame, which is derived and gated by
                // `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs` — deliberately not restated
                // here, and not summarised into a per-venue consequence either: the last five
                // attempts to write that sentence were each wrong in a different way.
                //
                // ⚠ It is not silent, but the alerting is a CEILING rather than a floor.
                // `recon_held` warns on the pass a divergence ENTERS the held set, and re-states
                // the standing set only after `HELD_SUMMARY_INTERVAL_MS` (one hour) of that
                // VENUE saying nothing at all — `HeldAnnouncer::observe` keeps one
                // `last_announce_ms` per venue and any changed pass re-arms it. So a second
                // divergence churning on the same venue more often than hourly starves the summary
                // entirely, and the standing drift is announced ONCE and then never again. "Told
                // once, then hourly" is the best case, not the guarantee. Alongside it there is
                // `max_sizing_equity` on the sizing half. Bounding the figure instead would be the
                // disputed-balance FLOOR that
                // `docs/decisions/0048-the-equity-a-strategy-sizes-against-is-capped-not-disputed.md`
                // refuses, for the reasons that record gives.
                let acct = &mut self.eng_mut(idx).account;
                acct.balance = expected;
                acct.realized_pnl_at_balance_sync = Some(acct.realized_pnl);
            }
            // ⚠ **THE CROSSING IS THE ONE MOMENT AN OPERATOR CAN ACT ON, and under a FOLDING
            // policy it is invisible.** `vike_exec::recon::resolve`'s `BalanceDrift` arm emits the
            // correcting `Event::AccountState` and NO alert (its own
            // `balance_drift_synthesize_folds_one_account_state_no_alert` pins that), so a folding
            // policy lands the venue's figure in the book with nothing said — which is exactly the
            // silent absorption this change exists to end, arriving one band later.
            // A HELD drift needs no line from here: it IS an alert, and `recon_held` below already
            // announces the transition and summarises the standing set.
            //
            // ⚠ **Which policies fold a `BalanceDrift` is `vike_exec::recon::mode_applies`'s
            // answer, and this comment does not repeat it.** It used to, naming `hybrid` — which
            // does NOT fold — and the correction then enumerated the set instead, which is the same
            // defect one round later. `crates/vike-exec/src/recon/types.rs`'s
            // `hybrid_quarantines_only_no_local_origin_kinds` pins the real rule beside the code it
            // governs. This line fires on whatever `mode_applies` folds, which is deliberately
            // narrow; that narrowness is the residual, not a reason to widen the condition.
            //
            // So the condition is read off the RESOLVED alerts rather than by asking the policy a
            // second time — one authority for what happened to this divergence, and no second copy
            // of the fold rule beside it. It is ONE-SHOT rather than a per-pass repetition (the
            // flood `recon_held` exists to stop): a folded `AccountState` re-anchors the baseline
            // through `Account::apply_account_state`, so the very next pass agrees.
            if let Some((expected, venue_bal)) = balance_crossed
                && !recon
                    .alerts
                    .iter()
                    .any(|a| a.kind == vike_exec::recon::DivergenceKind::BalanceDrift)
            {
                let unexplained = venue_bal - expected;
                tracing::warn!(
                    target: "vike_core::recon",
                    venue = %venue,
                    account = %account_key,
                    asset = %quote_asset,
                    expected,
                    venue_balance = venue_bal,
                    unexplained,
                    "cash reconcile ABSORBED an unexplained balance move — the venue's figure was \
                     folded because this policy auto-applies BalanceDrift"
                );
                self.note(format!(
                    "RECON balance {venue} {quote_asset}: absorbed {unexplained:+} unexplained \
                     (venue {venue_bal} vs expected {expected}) [account {account_key}]"
                ));
            }
        } else if let Some(b) = balance {
            recon.snapshot.balance = b;
            let acct = &mut self.eng_mut(idx).account;
            acct.balance = b;
            acct.balance_mode = BalanceMode::Authoritative;
            self.note(format!(
                "RECON balance {venue}: seeded authoritative {b} [account {account_key}]"
            ));
        }
    }

    /// The held-alert step of [`Self::reconcile_reports`], after the event fold: the announcer's
    /// transitions for this ACCOUNT's held set, then the store/refresh loop over `recon_alerts`.
    /// The transitions are returned so the pass line can ask `silent()`. It stays in this file:
    /// `crates/vike-ops/tests/wiring/held_divergence_is_visible_gate.rs` reads the raise site's
    /// text here.
    fn hold_recon_alerts(
        &mut self,
        venue: &str,
        account_key: &str,
        alerts: Vec<vike_exec::recon::ReconAlert>,
    ) -> recon_held::HeldTransitions {
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
        // sits still. `recon_held`'s module doc works it through.
        let raised: std::collections::BTreeSet<recon_held::HeldId> = alerts
            .iter()
            .map(|a| {
                recon_held::HeldId::new(
                    venue,
                    account_key,
                    a.kind,
                    a.dedup_key.as_deref(),
                    a.identity_detail.as_deref().unwrap_or(&a.detail),
                    &a.proposed_events,
                )
            })
            .collect();
        let transitions = self.recon_announce.observe(account_key, self.engine.now_ms, raised);
        for id in &transitions.cleared {
            recon_held::info_cleared(id);
        }
        if let Some(summary) = &transitions.summary {
            recon_held::warn_summary(venue, account_key, summary);
        }
        for a in alerts {
            // This alert's ANNOUNCEMENT identity (see the block above). Computed before `a` is
            // consumed, and with the SAME constructor `confirm_recon` uses, so a confirmed-then-
            // recurring divergence cannot be announced under a second key.
            let ident = recon_held::HeldId::new(
                venue,
                account_key,
                a.kind,
                a.dedup_key.as_deref(),
                a.identity_detail.as_deref().unwrap_or(&a.detail),
                &a.proposed_events,
            );
            let announce = transitions.newly_held.contains(&ident);
            // ⚠ A recurring divergence REFRESHES its held row in place — same confirm id,
            // freshened payload — so a pass adds NO new alert row for something already held.
            // This used to apply only to alerts carrying a `dedup_key`; everything else appended a
            // row per pass with a fresh id, and nothing in this tree ever prunes the store, so a
            // quarantined divergence nothing heals grew it forever (see `HeldReconAlert::identity`
            // for the the CI box measurement).
            //
            // ⚠ SEMANTIC CHANGE, stated because it is not a refactor: collapsing N per-pass rows
            // into one means a single `Command::ConfirmRecon` now resolves what previously needed
            // N confirms. That is what an operator wants — the N rows described ONE divergence and
            // confirming one of them folded a stale snapshot of it — but it IS a change to the
            // confirm surface. A genuinely different divergence still gets its own row and its own
            // id; that is exactly what `recon_held::HeldId` is for.
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
                self.recon_alerts.iter().find(|(_, h)| h.identity == ident).map(|(&id, _)| id);
            if let Some(id) = existing {
                // A refreshed row is normally an ALREADY-announced identity, so this is silent. It
                // can only be `true` if the announcer forgot the identity (an operator confirm)
                // while the row survived — which is news by construction, and is announced against
                // the row's EXISTING confirm id rather than a fabricated one.
                if announce {
                    recon_held::warn_newly_held(id, &ident, &a.detail);
                }
                let held = self.recon_alerts.get_mut(&id).expect("found above");
                // ⚠ The payload is refreshed UNCONDITIONALLY, but the ring note is gated on the
                // operator-facing TEXT alone. `resolve`'s synthesized position legs bake the pass
                // clock into their `ts`/`trade_id`/`client_order_id`
                // (`vike_exec::recon::resolve`'s `synth_position_legs`), so `proposed_events`
                // differs on EVERY pass for an unchanged divergence — keeping it in the
                // note condition would move the per-minute repetition this batch is removing out
                // of the log and into the bounded recent-events ring, flushing real events out of
                // it. Refreshing the payload anyway is the point: a confirm must fold the LATEST
                // view of the divergence, not one from hours ago.
                let detail_changed = held.detail != a.detail;
                held.detail = a.detail;
                held.proposed_events = a.proposed_events;
                held.recover_orders = a.recover_orders;
                if detail_changed {
                    self.note(format!(
                        "RECON alert #{id} {:?} refreshed (awaiting confirm) [account {account_key}]",
                        a.kind
                    ));
                }
                continue;
            }
            let id = self.recon_next_alert_id;
            self.recon_next_alert_id += 1;
            // ⚠ The `[account …]` suffix is appended rather than woven in: it keeps the note's
            // leading text byte-identical for `crates/vike-ops/tests/wiring/held_divergence_is_visible_gate.rs`,
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
            // `recon_held` owns that decision; the backlog stays visible through its periodic
            // summary, and a divergence going away is announced too.
            if announce {
                recon_held::warn_newly_held(id, &ident, &a.detail);
            }
            self.recon_alerts.insert(
                id,
                HeldReconAlert {
                    venue: venue.to_string(),
                    route_key: account_key.to_string(),
                    kind: a.kind,
                    detail: a.detail,
                    proposed_events: a.proposed_events,
                    recover_orders: a.recover_orders,
                    identity: ident,
                },
            );
        }
        transitions
    }

    /// Fold-thread handling of `Command::ConfirmRecon` (Task 17): operator approval of one held
    /// Quarantine/Hybrid-quarantined alert. Looks up its `proposed_events` and folds each through
    /// the SAME [`Self::publish_to`] path real venue events (and a Synthesize-mode reconcile pass)
    /// use — so a confirmed alert folds byte-identically to what an immediate Synthesize would
    /// have produced — then removes it from the held store, so a re-confirm of the same id hits
    /// the unknown-id no-op below. An unknown id (never held, already confirmed, or a replayed
    /// command from a prior process run — see [`Command::ConfirmRecon`]'s doc) is surfaced to the
    /// recent-events ring rather than silently dropped or panicking.
    ///
    /// ⚠ **This is the RETURN leg of the reconcile round trip, and it routes on
    /// [`HeldReconAlert::route_key`] — the key the raising pass resolved — not on `held.venue`.**
    /// The two are equal for every alert this tree raises, so the wrong one would look right and
    /// stay right until a second account of one exchange existed, at which point an operator's
    /// approval would fold synthesized fills and position deltas into the FIRST account's book.
    /// `held.venue` keeps the operator-facing label in the notes below, which is what an operator
    /// asked about.
    pub(crate) fn confirm_recon(&mut self, id: u64) {
        let Some(held) = self.recon_alerts.shift_remove(&id) else {
            self.note(format!("RECON confirm: unknown alert id {id}"));
            return;
        };
        // Drop this divergence's ANNOUNCEMENT identity along with its row — the SAME key the row
        // was stored under, so the two can never fall out of step. If the confirm did not actually
        // resolve the divergence (the venue keeps reporting it), the next pass re-raises a fresh
        // row — and without this it would do so in SILENCE, leaving the operator to believe an
        // action took that did not. Logging-only; the fold below is untouched.
        self.recon_announce.forget(&held.identity);
        let Some(idx) = self.engine_idx_for_route_key(RouteKey::declared(&held.route_key)) else {
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
            return;
        };
        for ev in held.proposed_events {
            self.publish_to(idx, ev);
        }
        // Order-loss recovery: re-register any venue orders local state lost (recon JournalDivergence).
        // INSERT-ONLY seed straight into the routed engine's registry — NOT the event fold (a
        // lifecycle event for an unknown coid is dropped), so this cannot ride `proposed_events`.
        let reregistered = if held.recover_orders.is_empty() {
            0
        } else {
            self.eng_mut(idx).reregister_orders(&held.recover_orders)
        };
        // Mirror reconcile_reports: drain any events an in-process client synthesized from the
        // now-folded fills.
        self.pump_client();
        // ⚠ `[account …]` on both: a confirm FOLDS into one book, and at fifty accounts of one
        // exchange the kind and the detail can be identical across all of them — the account is
        // the only thing on the line that says which book just moved. It is `held.route_key`
        // rather than a re-derivation, i.e. the key the RAISING pass resolved and the same one
        // `engine_idx_for_route_key` was just handed above. Equal to the venue on a venue with one
        // account.
        if reregistered > 0 {
            self.note(format!(
                "RECON confirmed #{id} {:?}: {} (re-registered {reregistered} lost order(s)) \
                 [account {}]",
                held.kind, held.detail, held.route_key
            ));
        } else {
            self.note(format!(
                "RECON confirmed #{id} {:?}: {} [account {}]",
                held.kind, held.detail, held.route_key
            ));
        }
    }
}

/// What `reconcile_compute` decided about the balance, handed to `apply_recon_balance`: the three
/// `vike_exec::recon::BalanceCheck` outcomes as Copy values, so no borrow of the pass's local view
/// outlives `resolve`. All three are `None` when `reconcile_balance` is off.
struct BalancePlan {
    /// `BalanceCheck::Adopt` — the FIRST SYNC figure.
    seed: Option<f64>,
    /// `BalanceCheck::Anchored`'s `expected` — the ROLL-FORWARD figure.
    roll: Option<f64>,
    /// `(expected, venue_bal)` of a beyond-tolerance `BalanceDrift`, for the crossing line.
    crossed: Option<(f64, f64)>,
}
