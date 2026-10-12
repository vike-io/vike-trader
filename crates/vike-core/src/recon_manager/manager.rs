//! One venue ACCOUNT's reconcile leg, and the manager that runs one pass over every leg.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc::WeakSender;

use vike_exec::recon::{MassStatus, ReconClient};
use vike_exec::{Command, Ingest, ReconcileReports};

use super::liveness::{ESCALATION_REMINDER, SUPPRESSED_REASON, VenueLiveness, staleness_threshold};
use super::{HealthProbe, ReconConfig, ReconHealth};

/// **ONE venue ACCOUNT's reconcile leg** — the unit a pass runs over, and the type that keeps the
/// two facts a payload needs from collapsing back into one string.
///
/// ⚠ **This replaced a `(String, Box<dyn ReconClient>)` tuple whose single key was doing two
/// incompatible jobs**, and both spellings of that tuple were wrong in a different direction the
/// moment a venue had two accounts — which is exactly the failure
/// [`vike_exec::ReconcileReports`]'s own doc enumerates from the payload's side:
///
/// * Key it CANONICALLY (`"binance"`, what every default account was keyed as) and a second
///   account's pass carries `venue: "binance"` with no route key, so
///   `vike_core::runtime::CoreThread::reconcile_reports` resolves the FIRST account's engine:
///   account B's venue truth is diffed against account A's local view. Under `hybrid` that is not
///   an alert — `PositionDrift` auto-applies, rewriting A's position size onto B's number and
///   booking realized PnL at B's average price, with no operator in front of it.
/// * Key it by ROUTE KEY (`"binance#ALT"`, what the node assembly's account fan-out was keyed as) and the
///   routing lands right, but the string then travels in the payload's `venue` field: the per-venue
///   health gate ([`ReconManager::should_reconcile`]) probes `"binance#ALT"`, misses, reads
///   `Healthy`, and that account is never suppressed while binance's feed is mid-gap. The same
///   decorated string then labels every ring note and log line, keys the held alerts' dedup
///   identity, and is handed to the journal-view provider.
///
/// So it is TWO fields, both carried, neither derived from the other — the same split
/// [`vike_exec::ExecutionEngine`] already carries as `venue`/`route_key`, and the same one
/// [`vike_exec::RouteKey`]'s two constructors exist to make a caller name out loud.
///
/// # Byte-identity on a single-account box
///
/// Structural, not remembered: [`ReconLeg::sole_account_of`] is the only constructor a
/// one-account-per-venue assembly reaches and it sets `route_key: None`, which is the literal
/// value the pre-split producer hardcoded. [`ReconLeg::account`] additionally NORMALIZES a route
/// key equal to its own venue back to `None`, so a default account routed through the labelled
/// constructor is byte-identical too — that is the invariant `a_default_accounts_leg_carries_no_route_key`
/// pins, and it is what makes "every deployment running today is unchanged" a property of the type
/// rather than of the caller's care.
pub struct ReconLeg {
    /// The CANONICAL exchange id (`"binance"`) — a `vike_model::VENUES` row for every real mount.
    ///
    /// ⚠ Never a route key, however many accounts this box mounts. This is the health-probe key,
    /// the label on every note and log line a pass emits, the dedup identity of a held alert, and
    /// the string the journal-view provider is asked about. [`Self::route_key`] is the other half.
    pub venue: String,
    /// Which ENGINE this leg's divergences fold into — `None` meaning "[`Self::venue`]'s sole
    /// account in this process", the inert default every single-account box produces.
    ///
    /// Copied verbatim onto [`vike_exec::ReconcileReports::route_key`], which is read through its
    /// `route()` accessor and nowhere else.
    pub route_key: Option<String>,
    /// This account's own reconcile client — a fresh signer+transport dedicated to reconcile
    /// reads, never the exec side's transport. `vike_mount::make_engine_for_account` builds one
    /// per account, which is why the client side of a per-account pass needed no work.
    pub client: Box<dyn ReconClient>,
}

impl ReconLeg {
    /// `venue` is a CANONICAL venue id AND this process mounts exactly ONE account of it, so its
    /// venue id doubles as its routing key and the payload carries no route key at all.
    ///
    /// The twin of [`vike_exec::RouteKey::sole_account_of`], and the same claim: true for every
    /// venue on every box with no `[accounts]` table, and the one that stops being true the moment
    /// a second account of one exchange is mounted.
    pub fn sole_account_of(venue: impl Into<String>, client: Box<dyn ReconClient>) -> Self {
        ReconLeg { venue: venue.into(), route_key: None, client }
    }

    /// ONE named account of `venue` — `route_key` is that account's own
    /// `vike_exec::ExecutionEngine::route_key` (`vike_mount::account_route_key` renders it), and
    /// `venue` stays the canonical exchange id beside it.
    ///
    /// ⚠ A `route_key` EQUAL to `venue` normalizes to `None` rather than being carried: that is
    /// what a default account's key is (`account_route_key` renders the bare venue id for
    /// `AccountLabel::Default`), and storing it would put a `Some` on the wire — and in the
    /// journal, where a command payload is a persisted schema — for every existing deployment.
    /// `RouteKey::declared(k) == RouteKey::sole_account_of(v)` whenever `k == v`, so the two route
    /// identically and the normalization changes no decision, only bytes.
    pub fn account(
        venue: impl Into<String>,
        route_key: impl Into<String>,
        client: Box<dyn ReconClient>,
    ) -> Self {
        let venue = venue.into();
        let route_key = route_key.into();
        let route_key = (route_key != venue).then_some(route_key);
        ReconLeg { venue, route_key, client }
    }

    /// This leg's routing key as the payload will carry it — `route_key` when it has one, else the
    /// venue. Used for the LOG lines that must name a leg unambiguously on a two-account box; the
    /// payload itself carries the two fields separately and is routed through
    /// `ReconcileReports::route`.
    pub(crate) fn leg_key(&self) -> &str {
        self.route_key.as_deref().unwrap_or(&self.venue)
    }

    /// **Every account of a venue this process runs SEVERAL ENGINES of NAMES ITSELF** — the
    /// assembly step that makes `vike_exec::ReconcileReports::route_key == None` mean exactly one
    /// thing, so the fold thread can REFUSE the other one.
    ///
    /// ## Why an assembly step rather than a constructor rule
    ///
    /// A venue's DEFAULT account reaches the manager through [`Self::sole_account_of`] (the
    /// `build_node` list) while its labelled accounts reach it through [`Self::account`] (the
    /// per-account fan-out), and neither constructor can see the other's rows. So on a fifty-
    /// account binance box the default leg carried `route_key: None` — the literal string a
    /// single-account box produces — beside forty-nine legs that named themselves. It ROUTED
    /// correctly (`RouteKey::sole_account_of("binance")` is an exact match on the default engine's
    /// own route key, not a guess), which is precisely why it could not be spotted by reading: the
    /// value was right and its MEANING was wrong. `None`'s documented meaning is "this venue's
    /// sole account in this process", and on that box it is not.
    ///
    /// That mattered the moment the spec's Class E refusal was built: the fold thread refuses a
    /// `None`-routed pass on a venue it runs several engines of, and without this step it would
    /// have refused the default account's own leg every interval. One stamp here, and the refusal
    /// can only ever fire for a payload NO producer in this process wrote — a replayed journal
    /// from a build that predates the account fan-out.
    ///
    /// ## ⚠ THE PREDICATE IS THE ENGINE SET, AND IT HAS TO BE
    ///
    /// `engine_venues` is the canonical `vike_exec::ExecutionEngine::venue` of EVERY engine this
    /// process is about to run — the primary plus every extra, in `spawn_core_multi`'s own order.
    /// It is a PARAMETER rather than a count taken off `legs`, and that is the whole correction of
    /// 2026-09-14: this function used to count LEGS, while the refusal it exists to keep unreachable
    /// counts ENGINES (`CoreThread::reconcile_reports` asks `CoreThread::engines_of_venue`). Two
    /// guards, two different sets — and `crates/vike-mount/src/node/accounts.rs`'s `mount_accounts_of` makes them diverge BY
    /// CONSTRUCTION: it pushes `AccountExtras::engines` unconditionally and `AccountExtras::recon`
    /// only when that account produced a `ReconClient`.
    ///
    /// So a venue with two ARMED accounts whose second one built no reconcile client — a cTrader or
    /// IBKR account whose synchronous connect failed at mount, an IG / OANDA / deribit account whose
    /// dedicated recon handshake returned `None` while its exec client spawned live — arrives here
    /// as TWO engines and ONE leg. ⚠ Per ACCOUNT, not per venue: each account's handle is built
    /// inside its own `vike_mount::make_engine_for_account` call, with its own credentials and its
    /// own handshake, so one succeeding while its sibling fails is a single transient error apart.
    /// Counting legs, nothing was stamped; counting engines, the fold thread then refused the
    /// surviving DEFAULT account's own pass, every interval, for as long as the process ran.
    /// Counting engines here instead makes the two guards read one set, and the refusal goes back
    /// to being reachable only by a replayed payload.
    ///
    /// The same shape is what `CoreThread::multi_account` is computed from (`assemble_core`), for
    /// the same reason: a venue's engines are what a routing key can address, and a leg is only
    /// evidence that one of them is being read.
    ///
    /// ## What it does NOT touch
    ///
    /// A venue with ONE ENGINE, which is every venue on every box with no `[accounts]` table.
    /// Those keep `route_key: None` and their journal bytes are unchanged, which is the whole
    /// reason this is a scan over the assembled vector rather than a blanket stamp. A leg naming a
    /// venue this process runs NO engine of is untouched too — nothing can address it, and the
    /// fold thread's own no-engine note is what answers such a pass.
    pub fn name_accounts_of_shared_venues(legs: &mut [ReconLeg], engine_venues: &[&str]) {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for venue in engine_venues {
            *seen.entry(*venue).or_insert(0) += 1;
        }
        let shared: BTreeSet<&str> =
            seen.into_iter().filter(|&(_, n)| n > 1).map(|(v, _)| v).collect();
        for leg in legs.iter_mut() {
            if leg.route_key.is_none() && shared.contains(leg.venue.as_str()) {
                leg.route_key = Some(leg.venue.clone());
            }
        }
    }
}

impl fmt::Debug for ReconLeg {
    /// Hand-written because `Box<dyn ReconClient>` is not `Debug`; prints the two routing facts,
    /// which are the whole of what a reader of a driver dump needs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReconLeg")
            .field("venue", &self.venue)
            .field("route_key", &self.route_key)
            .finish_non_exhaustive()
    }
}

/// The reconcile manager: owns the per-venue [`ReconClient`]s + config + a WEAK ingest sender.
/// Lives on the driver thread; its only side effect is enqueuing [`Command::ReconcileReports`].
pub struct ReconManager {
    /// ONE ROW PER ACCOUNT, not per venue — see [`ReconLeg`]. Two accounts of one exchange are
    /// two rows carrying the SAME canonical `venue` and DIFFERENT route keys, and the pass below
    /// runs each of them against its own engine.
    pub(crate) clients: Vec<ReconLeg>,
    pub(crate) config: ReconConfig,
    pub(crate) ingest: WeakSender<Ingest>,
    /// Task 15: mirrors `config.health` (hoisted onto the struct so [`ReconManager::should_reconcile`]
    /// doesn't need to reach through `config` — same shape as `ingest`/`clients`).
    pub(crate) health: Option<HealthProbe>,
    /// **The staleness escalation's whole state** (2026-09-11). Per venue, in `clients` order.
    pub(crate) liveness: Vec<VenueLiveness>,
}

impl ReconManager {
    /// Task 15 gate, consulted before EACH VENUE's leg of a pass (startup, trigger-driven, AND Task
    /// 16's interval-driven — see [`ReconManager::run_startup_pass`]). `true` (reconcile this venue)
    /// when no probe is wired (`health: None` — back-compat, byte-identical to pre-Task-15 behavior)
    /// or the probe reports [`ReconHealth::Healthy`] for `venue`; `false` (suppress this venue's leg)
    /// when it reports [`ReconHealth::Degraded`] (that venue's feed mid-gap, or local network down).
    /// Suppression DEFERS this venue's leg only — it sets no pending flag and never blocks a DIFFERENT
    /// healthy venue in the same pass, so a Degraded venue simply isn't reconciled until the next
    /// poke lands while it reads Healthy.
    ///
    /// ⚠ An ASSOCIATED function over the probe rather than a `&self` method: the pass now mutates
    /// per-venue liveness state in the same loop, so a `&self` call would borrow the whole struct
    /// and conflict with it. Taking the one field it actually reads is the narrower borrow and
    /// says so at the signature.
    fn should_reconcile(health: &Option<HealthProbe>, venue: &str) -> bool {
        match health {
            None => true,
            Some(probe) => probe(venue) == ReconHealth::Healthy,
        }
    }

    /// One reconcile pass: for each venue client, blocking-fetch the atomic mass-status bundle
    /// (orders + positions + fills via [`ReconClient::fetch_mass_status`], whose default composes
    /// the same three per-report fetches) and enqueue it for the fold thread to diff/resolve.
    /// Blocking REST runs HERE (never the fold). A fetch
    /// error skips that venue with a pass-boundary warning; a dropped core (weak upgrade / send
    /// fails) ends the pass. Called once at startup, again (Task 13) on every reconcile-trigger
    /// poke, and again (Task 16) on every [`ReconConfig::interval`] tick — always the SAME pass
    /// over ALL configured venues, not just the one that reconnected (simplest correct behavior; a
    /// per-venue-targeted pass is out of scope if ever needed). Idempotent by construction: a
    /// report the fold thread has already seen (fill trade_id already in `seen_trade_ids`, order
    /// already terminal, position already matching) diffs to zero divergences, so a redundant pass
    /// folds nothing new (see `tests/recon_continuous_audit.rs`). Task 15: gated PER VENUE by
    /// [`ReconManager::should_reconcile`] — a Degraded probe suppresses only THAT venue's leg (no
    /// fetch, no enqueue for it), so a healthy venue in the same pass still reconciles.
    pub(crate) fn run_startup_pass(&mut self, stop: &AtomicBool) {
        self.run_pass_at(stop, vike_model::now_ms());
    }

    /// [`Self::run_startup_pass`] with the wall clock injected, so the staleness escalation is
    /// testable without sleeping through a ten-minute threshold.
    pub(crate) fn run_pass_at(&mut self, stop: &AtomicBool, now_ms: i64) {
        // Wall clock: reconcile `since` bounds are venue REST timestamps, so a wall clock is correct
        // here (unlike the core's injectable fold clock).
        let since = (now_ms - self.config.lookback_ms).max(0);
        // Split the borrow by FIELD: the loop reads `clients`/`config`/`ingest`/`health` while
        // mutating `liveness`, and a `&self` method call would borrow the whole struct.
        let ReconManager { clients, config, ingest, health, liveness } = self;
        for (idx, leg) in clients.iter().enumerate() {
            // Destructured rather than reached through `leg.` so every line below reads exactly as
            // it did when this loop walked a `(venue, client)` tuple — and so the ONE new fact,
            // `route_key`, has to be spelled at the payload rather than defaulted there.
            let ReconLeg { venue, route_key, client } = leg;
            if stop.load(Ordering::Relaxed) || ingest.upgrade().is_none() {
                return;
            }
            // Per-venue health gate: skip only THIS venue's leg when its feed is Degraded; other
            // venues in the same pass are unaffected.
            if !Self::should_reconcile(health, venue) {
                // ⚠ EDGE-TRIGGERED, and that is the incident's second lesson. This line used to
                // fire on EVERY pass: on the CI box it produced 2,516 identical copies over 42 hours,
                // which buried the fact rather than reporting it. Entering suppression speaks once;
                // leaving it speaks once (below); the staleness escalation is what says the
                // condition PERSISTS. Same rule `crate::runtime::recon_held`'s `warn_newly_held`
                // already learned here for a different repetition.
                //
                // ⚠ The edge is on the REASON, not on a bool — see `VenueLiveness::degraded`. A
                // bool shared with the fetch arm below swallowed the 401 of a venue that recovered
                // its feed and lost its REST in the same window.
                if liveness[idx].degraded.as_deref() != Some(SUPPRESSED_REASON) {
                    liveness[idx].degraded = Some(SUPPRESSED_REASON.to_string());
                    tracing::warn!(
                        target: "vike_core::reconcile",
                        venue = %venue,
                        // WHICH LEG. At fifty accounts of one exchange the venue alone names
                        // fifty of them; equal to `venue` on a venue with one account.
                        account = %leg.leg_key(),
                        "reconcile leg suppressed: {SUPPRESSED_REASON}"
                    );
                }
                continue;
            }
            // Atomic mass-status snapshot (the `MassStatus` bundle): one bundled
            // fetch of orders + positions + fills. The default `fetch_mass_status` composes the same
            // three per-report fetches in the same order (orders → fills → positions), so this is
            // byte-identical to the pre-seam per-report fetch for every current venue; a venue with a
            // real single-call snapshot endpoint may override it for cross-report consistency. A
            // fetch error still skips only THIS venue's leg with a pass-boundary warning, exactly as
            // the per-report fetches did.
            let MassStatus { orders, positions, fills } = match client.fetch_mass_status(since) {
                Ok(v) => v,
                Err(e) => {
                    // ⚠ Edge-triggered for the SAME reason as the suppression line above, and this
                    // is the arm that makes an elapsed-time escalation strictly wider than a
                    // suppression counter: a venue whose fetch fails every pass is exactly as
                    // silently dead as a suppressed one, and carried no counter at all before now.
                    //
                    // ⚠ On the REASON TEXT, which here VARIES with the error. That is deliberate:
                    // `{e}` is the only per-occurrence diagnostic this arm carries, so a cause that
                    // changes (401 → timeout → 500) must be reportable, and a suppression that
                    // preceded it must not swallow the first one. `VenueLiveness::degraded` argues
                    // the bound.
                    let reason = format!("mass-status report fetch failed: {e}");
                    if liveness[idx].degraded.as_deref() != Some(reason.as_str()) {
                        tracing::warn!(
                            target: "vike_core::reconcile",
                            venue = %venue,
                            account = %leg.leg_key(),
                            "{reason}"
                        );
                        liveness[idx].degraded = Some(reason);
                    }
                    continue;
                }
            };
            // Task 3: the venue's authoritative cash. A failure is swallowed here rather than
            // skipping the whole pass — a venue that doesn't report balance still gets its
            // order/fill/position divergences diffed and folded, it just leaves `Account` balance
            // untouched.
            //
            // ⚠ **It used to be swallowed SILENTLY, and this was the one fetch in the pass with no
            // log at all.** `Err` was collapsed into `None` by `unwrap_or(None)`, which is also
            // what a venue that doesn't surface a balance returns, so the two were
            // indistinguishable downstream and neither reached an operator. The incident's
            // headline loss was a wallet figure frozen across 2,209 summaries, so a balance fetch
            // that fails silently is precisely the shape that must not stay silent. Same
            // reason-keyed edge as the two arms above, so a permanently-401ing balance endpoint
            // says so once rather than once a minute.
            //
            // The RESIDUAL is carried to the end of the leg rather than written here: this half
            // failing does not stop the pass (orders/fills/positions still fold), so the
            // recovery bookkeeping below owns the transition and this arm only reports.
            let (balance, balance_reason) = match client.fetch_balance() {
                Ok(b) => {
                    // `Ok(Some(_))` ARMS and advances the balance clock; `Ok(None)` is "this venue
                    // does not report one" and must never arm it (see `last_balance_ms`).
                    if b.is_some() && config.reconcile_balance {
                        liveness[idx].last_balance_ms = Some(now_ms);
                    }
                    (b, None)
                }
                Err(e) => {
                    let reason = format!("balance fetch failed: {e}");
                    if liveness[idx].degraded.as_deref() != Some(reason.as_str()) {
                        tracing::warn!(
                            target: "vike_core::reconcile",
                            venue = %venue,
                            account = %leg.leg_key(),
                            "{reason}"
                        );
                    }
                    (None, Some(reason))
                }
            };
            // Upgrade at the last moment: a core that exited mid-fetch ends the pass losslessly.
            let Some(tx) = ingest.upgrade() else { return };
            let reports = ReconcileReports {
                venue: venue.clone(),
                since,
                orders,
                fills,
                positions,
                policy: config.policy.clone(),
                balance,
                generate_missing_orders: config.generate_missing_orders,
                reconcile_balance: config.reconcile_balance,
                balance_tol: config.balance_tol,
                // **THE PRODUCER.** Both facts, stamped from the same `ReconLeg` — and this is the
                // one edit the type's whole existence was waiting for.
                //
                // ⚠ `venue` is CANONICAL and stays canonical on a two-account box: it is the key
                // `should_reconcile` probed above, the label on the ring note and log lines the
                // fold thread writes, the dedup identity of every held alert, and the string
                // `journal_view_provider` is asked about. `route_key` is the OTHER question — which
                // of this process's engines the divergences fold into — and `CoreThread::
                // reconcile_reports` reads it through `ReconcileReports::route` and never off
                // `venue`. Collapsing the two back into one string is the defect this replaced;
                // `ReconLeg`'s doc enumerates both directions it fails in.
                //
                // `None` for every single-account box, which is every deployment running today:
                // `ReconLeg::sole_account_of` sets it and `ReconLeg::account` normalizes a default
                // account's self-named key back to it, so the payload is byte-identical there.
                //
                // ⚠ The JOURNAL half of that claim used to be argued from a
                // `skip_serializing_if` the field did not carry — a `None` was therefore
                // SERIALIZED, as `"route_key":null`, and the bytes were not identical at all. The
                // attribute is on the field now (`vike_exec::ReconcileReports::route_key`), so the
                // sentence and the code finally agree; the conclusion was right for a weaker
                // reason (serde's `default` means a pre-field journal still REPLAYS identically,
                // whatever this pass writes).
                //
                // ⚠ And on a box that runs several ENGINES of this venue, none of its legs reaches
                // here with `None`: `ReconLeg::name_accounts_of_shared_venues` stamps the default
                // account's leg too, precisely so the fold thread can refuse a `None` it cannot
                // attribute instead of folding it against a guess. ⚠ Several ENGINES, not several
                // legs — a venue can mount two engines and produce ONE leg, and counting legs there
                // left this payload unstamped for the fold thread to refuse (see that function).
                route_key: route_key.clone(),
            };
            // Plain std thread (no tokio runtime) → blocking_send is legal and lossless.
            if tx
                .blocking_send(Ingest::Command(Command::ReconcileReports(Box::new(reports))))
                .is_err()
            {
                return; // core exited
            }
            // This venue's leg RAN. Clear the degraded edge (announcing the recovery, since a
            // divergence going away is a fact too) and stamp the liveness clock the escalation
            // reads.
            //
            // ⚠ A pass whose BALANCE half failed is not a full recovery: the reports folded, so
            // `last_pass_ms` advances and no `resumed` is claimed, and `balance_reason` becomes the
            // carried state so the next pass's balance arm dedups against it rather than warning
            // again. `last_balance_ms` deliberately does NOT advance — that is what makes the
            // escalation see a wallet that stopped being re-read behind a healthy order leg.
            let prior = liveness[idx].degraded.take();
            if prior.is_some() && balance_reason.is_none() {
                tracing::info!(
                    target: "vike_core::reconcile",
                    venue = %venue,
                    // WHICH LEG resumed — the recovery half of the three WARNs above, and equally
                    // unreadable at fifty accounts without it.
                    account = %leg.leg_key(),
                    stale_ms = liveness[idx].last_pass_ms.map(|t| now_ms - t).unwrap_or(0),
                    prior = %prior.as_deref().unwrap_or(""),
                    "reconcile leg resumed"
                );
            }
            liveness[idx].degraded = balance_reason;
            liveness[idx].last_pass_ms = Some(now_ms);
            // ⚠ The escalation reminder is re-armed by [`Self::escalate_stale_venues`] when NOTHING
            // is late, not here. Clearing it on any successful pass would make a venue whose
            // BALANCE is stale behind a healthy order leg re-fire its ERROR every single pass —
            // the 2,516-line flood one severity up, which is the failure this whole escalation
            // exists to replace.
        }
        self.escalate_stale_venues(now_ms);
    }

    /// **THE ESCALATION.** A venue whose reconcile leg has not run for longer than
    /// [`staleness_threshold`] gets an `tracing::error!`, once, then a decaying reminder every
    /// [`ESCALATION_REMINDER`].
    ///
    /// ⚠ **TWO CLOCKS, not one, and the second is the one that sees the incident's headline loss.**
    /// The leg clock ([`VenueLiveness::last_pass_ms`]) answers "did this venue reconcile at all";
    /// the balance clock ([`VenueLiveness::last_balance_ms`]) answers "was its authoritative cash
    /// re-read". They come apart because `fetch_balance`'s failure does not stop a pass: the
    /// orders/fills/positions still fold and the leg clock still advances. the CI box's loss was
    /// reported as a wallet figure frozen across 2,209 summaries, so a one-clock escalation would
    /// have been silent about exactly that shape (it caught the actual 2026-09-10 incident only
    /// because the WHOLE leg was suppressed). Each ERROR names which claim went stale and carries
    /// the last reported `cause` — the reason string [`VenueLiveness::degraded`] holds — so the
    /// escalation is not a bare age with no diagnosis.
    ///
    /// ⚠ **Why an ERROR line and not `CoreSnapshot.fault`, which is the obvious-looking carrier.**
    /// That field's doc reads "set once a handler panicked — the core is HALTED in safe-state", and
    /// both of its setters prove it: `crate::runtime::watchdog`'s `enter_safe_state` sets `fault`,
    /// sets `TradingState::Halted`, halts every extra engine and cancels every working order with
    /// `CancelIntent::RiskOff`; `crate::runtime::publish`'s `publish_guarded` does the same on a
    /// publish panic. Writing a stale-reconcile escalation there would either HALT THE DAEMON AND
    /// PULL ITS BOOK over a health-gate false positive — the exact inverse of
    /// `vike_tradehub::reconcile_config`'s fail-soft asymmetry, and this incident's false positive is
    /// precisely what would have triggered it — or, set without `enter_safe_state`, publish
    /// `fault: "…"` beside `trading_state: "Active"` and break the invariant every reader of that
    /// field relies on.
    ///
    /// ⚠ **What is DEFERRED, and it is the consumer half.** This escalation is a JOURNAL line —
    /// visible to `journalctl -p err` and to any unit-level notifier, which is more than the 2,516
    /// WARNs offered and is deliberately the cheapest thing that is not nothing. What it is NOT is
    /// a channel-delivered alert, and TWO pieces of that are blocked rather than merely unbuilt:
    ///
    /// 1. `vike_exec::ReconBlock` should gain `last_pass_by_venue: Vec<(String, i64)>`,
    ///    written where `recon_last_pass_ts` is written today. That scalar is a single GLOBAL
    ///    written by whichever venue folded last, so on a multi-venue mount one healthy venue keeps
    ///    it fresh while another goes dark forever — a pre-existing defect this field would also
    ///    fix. The one EXHAUSTIVE construction of `ReconBlock` is
    ///    `crates/vike-core/src/runtime/publish.rs`'s `recon_block`, so a new field lands there;
    ///    `crates/vike-app-core/src/backend/observe_bridge.rs`'s `wire_to_core` builds the block
    ///    with `ReconBlock::default()`, and the alerting tests use their own `SnapshotFacts`.
    /// 2. `crates/vike-tradehub/src/summary.rs`'s `summary_line` should then gain
    ///    `recon_stale_venues` and `recon_oldest_pass_age_ms`, computed from that map and placed
    ///    BESIDE `fault`, never inside it. It has nothing to read until (1) lands.
    ///
    /// And the alerting RULE itself is a third: `crates/vike-alerting/src/rule.rs` has exactly one
    /// recon rule and it keys on `snap.recon.alerts` PRESENCE, so pass-staleness is a new rule
    /// shape in that same crate. **Sequencing: (1) must land before the rule, or the rule has
    /// nothing to read.**
    ///
    /// ⚠ **Not covered, stated so it is not claimed.** Nothing here escalates a venue that was
    /// never in `clients` at all — an unarmed venue has no leg to be stale. That is the mount's
    /// `ARMING:` line's job (`vike_mount`'s `report_capped_to_paper`), not this counter's. And this
    /// runs at the END of an attempted PASS, so a deployment with `VIKE_RECONCILE_INTERVAL_MS=0`
    /// (the interval arm disabled entirely) evaluates staleness only when a trigger poke lands.
    /// That is the honest scope rather than a gap: with no cadence there is no expected pass for a
    /// venue to be late for, and adding a second timer to judge a driver that was asked not to run
    /// would be a different feature.
    fn escalate_stale_venues(&mut self, now_ms: i64) {
        let threshold = staleness_threshold(self.config.interval).as_millis() as i64;
        let balance_claimed = self.config.reconcile_balance;
        for row in &mut self.liveness {
            // The FIRST pass establishes this venue's baseline and never escalates on it: before
            // it there is no expected pass for the venue to be late for, and a `0` sentinel would
            // make every venue read 55 years stale on the driver's first tick.
            let Some(last) = row.last_pass_ms else {
                row.last_pass_ms = Some(now_ms);
                continue;
            };
            let pass_age = now_ms - last;
            // ⚠ **TWO CLOCKS, because the leg clock cannot see the incident's headline loss.** The
            // BALANCE clock is judged only when this deployment reconciles balances at all AND this
            // venue has proven it reports one (`VenueLiveness::last_balance_ms` argues both gates);
            // otherwise `None` and no balance claim is made. Without it a venue whose orders, fills
            // and positions reconcile perfectly while its balance endpoint 401s keeps a permanently
            // fresh `last_pass_ms` and escalates nothing — which is exactly the state the CI box
            // reported as "the wallet figure frozen at an identical value across all 2,209
            // summaries".
            let balance_age =
                if balance_claimed { row.last_balance_ms.map(|t| now_ms - t) } else { None };
            let stale_pass = pass_age >= threshold;
            let stale_balance = balance_age.is_some_and(|a| a >= threshold);
            if !stale_pass && !stale_balance {
                // Nothing is late. Re-arm the reminder HERE rather than on any successful pass, so
                // a venue that is half-healthy (leg running, balance stale) keeps its hourly decay
                // instead of re-escalating every minute.
                row.last_escalation_ms = 0;
                continue;
            }
            let due = row.last_escalation_ms == 0
                || now_ms - row.last_escalation_ms >= ESCALATION_REMINDER.as_millis() as i64;
            if !due {
                continue;
            }
            row.last_escalation_ms = now_ms;
            if stale_pass {
                tracing::error!(
                    target: "vike_core::reconcile",
                    venue = %row.venue,
                    account = %row.leg_key,
                    stale_ms = pass_age,
                    threshold_ms = threshold,
                    cause = %row.degraded.as_deref().unwrap_or("unknown"),
                    "reconcile leg has not run for longer than its staleness threshold — this \
                     venue's orders, fills, positions and authoritative balance are UNCHECKED \
                     against the exchange"
                );
            } else {
                tracing::error!(
                    target: "vike_core::reconcile",
                    venue = %row.venue,
                    account = %row.leg_key,
                    stale_ms = balance_age.unwrap_or(0),
                    threshold_ms = threshold,
                    cause = %row.degraded.as_deref().unwrap_or("unknown"),
                    "reconcile legs are RUNNING but this venue's authoritative BALANCE has not \
                     been re-read for longer than the staleness threshold — the equity the \
                     pre-trade margin lane judges against is the last figure the venue gave"
                );
            }
        }
    }

    /// Task 16 [`ReconConfig::audit_interval`] tick — DELEGATED, not a reimplemented sweep. This
    /// does NOT run a reconcile pass and does NOT itself decide anything is stuck: it enqueues
    /// [`Ingest::Watchdog`], the same message `runtime::spawn_core`'s own `vt-core-watchdog`
    /// thread already injects on a cadence whenever `CoreConfig::submit_ack_timeout` is
    /// configured. `Ingest::Watchdog` is a pure WAKER on the fold thread (`Ingest::Watchdog =>
    /// {}` in the dispatch match) — its only effect is making the core reach its drain-loop
    /// boundary sooner, where [`crate::runtime`]'s `DeadlineTimerWheel` runs the existing
    /// `sweep_stuck_orders` if-and-when it is actually due. So: with `submit_ack_timeout` unset
    /// (the default), this poke is inert — this manager has no visibility into `CoreConfig` (and
    /// must not; down-only layering) and does not need any to stay correct. Best-effort: a failed
    /// upgrade/send is silently dropped, same as any other poke on the weak ingest sender.
    pub(crate) fn run_audit_tick(&self) {
        if let Some(tx) = self.ingest.upgrade() {
            // Best-effort poke (see above): `CoreGone` = shutting down; nothing to wake.
            let _ = tx.blocking_send(Ingest::Watchdog);
        }
    }
}
