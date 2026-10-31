//! `ExecutionClient` for cTrader: `CtraderExec` turns canonical `OrderRequest`s into cTrader
//! `Command`s over the actor channel (`conn::ActorHandle`) and honors the emitter split — Rust
//! emits `OrderSubmitted` synchronously at submit; the venue side emits every subsequent state
//! change (`OrderAccepted`/`OrderFilled`/…) which the actor thread decodes from `EXECUTION_EVENT`
//! frames and pushes onto the core ingest lane (see `conn::on_inbound` → `event_mapper::exec_event_to_events`).
//!
//! No order may silently vanish: a `submit` whose `OrderRequest` cannot be mapped (unknown symbol,
//! sub-minimum volume, bad type/side, missing conditional price) — or whose command cannot reach
//! the actor — synthesizes a terminal `OrderRejected`. A `cancel`/`modify` for a coid whose venue
//! `orderId` has not yet been learned surfaces a NON-terminal `OrderCancelRejected`/
//! `OrderModifyRejected` (the order stays live) rather than being dropped.
//!
//! cancel/modify need cTrader's numeric `orderId`, which only arrives on execution events — the
//! actor keeps a shared `client_order_id → order_id` map ([`crate::conn::OrderIdMap`]) that this client
//! reads. Ports nothing — cTrader Open API (https://help.ctrader.com/open-api/).
//!
//! # The operator HALT sentinel: the shared rule, PLUS what this client's own book can PROVE
//!
//! cTrader was the last LIVE-MOUNTABLE venue the out-of-band kill switch did not reach. Eleven
//! venues inherit it by delegating `submit` to `crates/vike-bridge-core/src/exec_actor.rs`'s
//! `halt_engaged`; hyperliquid keeps a bespoke copy of the same check; this client is neither — it
//! owns its submit path end to end — so `touch <project>/settings/state/HALT` blocked nothing here.
//!
//! The rule here is a UNION, and [`CtraderExec::halt_admits_this_submit`] spells it as one:
//!
//! > While the sentinel exists, admit everything `vike_exec::halt::halt_admits_submit` admits (the
//! > caller-asserted `reduce_only` flag — the whole rule on every other client), PLUS every submit
//! > this client's own position book PROVES is a close (the ones that route to
//! > [`ClosePlan::Close`]). Refuse the rest, which is by construction what OPENS, ADDS or FLIPS.
//!
//! **The second half exists because a flag-only gate would refuse the ordinary flatten on this
//! venue.** [`ExecutionClient::submit`] routes reduces by INSPECTING the actor-maintained position
//! map ([`positions::plan_reduce`]) precisely because on a HEDGING account an opposite-side MARKET
//! order does NOT need the flag to close a position — that is the bug the close-by-position-id path
//! exists to fix. A `reduce_only`-only gate would refuse a plain opposite-side market order that
//! genuinely flattens, on the one venue where that is the usual way to flatten. (A resting limit or
//! stop is never routed here: it rests, and falls to the shared flag rule; on a hedge-capable account
//! a `reduce_only` one is first BOUND to an open position, or refused by name when none can take it:
//! see [`NO_POSITION_TO_BIND_AN_EXIT`].)
//!
//! ⚠ **The first half exists because ABSENCE FROM THE BOOK IS NOT EVIDENCE THAT AN ORDER OPENS — and
//! a position-verified-ONLY gate trapped the operator on every restart.** The book used to start
//! EMPTY on a fresh connect and be filled only forwards — by execution events THIS process saw, and
//! by `conn`'s `reconcile_after_reconnect`, which runs on a RECONNECT and never ran on the first
//! one. So after a daemon restart it knew nothing, every pre-existing position was invisible, and a
//! gate that read "not in the book" as "this opens" refused the operator's exit from a position that
//! plainly existed. That is exactly the failure `docs/ops/kill-switches.md`'s first promise —
//! **halting cannot trap you** — and the whole reducing exemption exist to prevent.
//!
//! ⚠ **`crates/bridges/ctrader/src/conn/reconcile.rs`'s `seed_positions_at_connect` NARROWED that window and
//! did not close it, so the `||` still carries the argument.** The seed is BEST-EFFORT by design,
//! exactly like the reconnect twin it shares `rebuild_position_book` with — a write failure, a venue
//! `ERROR_RES` and the bound `crates/bridges/ctrader/src/conn.rs`'s `SEED_TIMEOUT` sets (its own doc
//! carries the measurement that sized it) each leave the book UNFETCHED — and it runs on EXEC
//! starts only. A socket death then clears the evidence flag again
//! (`crates/bridges/ctrader/src/positions.rs`'s `PositionBook::invalidate`) until a reconcile
//! actually answers. So "demand a fetch first" still cannot be the safety argument: the operator's
//! exit would hang on a round trip that is allowed to fail, and a book that has not been told
//! remains SILENCE rather than a denial.
//!
//! The book therefore answers only in one direction here: a position it HOLDS is evidence, a
//! position it lacks is silence. Net effect — **this client admits a STRICT SUPERSET of what every
//! other client admits.** It can only ever let MORE out under a halt, never less, so an operator's
//! 3am muscle memory (`market-exit`, `flatten`, any `reduce_only` order) works here exactly as it
//! does everywhere else, and a plain opposite-side MARKET order works here in addition (a resting
//! limit or stop needs `reduce_only` here too).
//!
//! ⚠ **One refusal is not the halt's, and it comes AFTER the halt's verdict:** a `reduce_only` RESTING
//! order on a hedge-capable account with NO open position to bind to is refused by name
//! ([`NO_POSITION_TO_BIND_AN_EXIT`], [`EXIT_ALREADY_COVERED`], [`EXIT_SPANS_POSITIONS`]) once the rule above has admitted it, where
//! another client would let it rest on the flag. It cannot trap anyone: such an order closes nothing (its
//! trigger would OPEN a position), and the way out of a position under a halt is the MARKET close.
//!
//! ⚠ **A `reduce_only` MARKET order that nothing in the book faces is asked about the venue FIRST, and a
//! halt is exempt from the refusal that may follow** (0.1.97). On a hedge-capable account a plain market
//! order against nothing OPENS a position beside the one it meant to close (the locked pair), and an empty
//! book may only mean the venue was never asked (a seed that failed on a start that held a position). So a
//! book that is not authoritative is read from the venue once (`Command::ReadPositions`, the actor's own
//! `ProtoOAReconcileReq`) and what it lists facing the order is closed by position id as above; nothing
//! facing it after that is a terminal `OrderRejected` by name ([`NOTHING_TO_REDUCE`] when the list was
//! read, [`POSITIONS_UNVERIFIED`] when it could not be) and nothing is sent. Under an engaged HALT the
//! order is still admitted on its flag, exactly as on every other venue: refusing it would trap an exit
//! that the venue could not be asked about, and the halt predicate above is untouched. A NETTED account
//! is not asked and not refused: it is the plain order it always was.
//!
//! Three consequences worth stating, because each is a decision:
//!
//! - **The mis-tagged-entry residual is INHERITED under the DEFAULT mode, and only NARROWED by
//!   `halt_admit = "verify"`.** A `reduce_only` order on a book showing nothing to close is admitted
//!   under `admit`, exactly as at the `ExecActor` boundary — the residual
//!   `crates/vike-exec/src/halt.rs` documents. It is tempting to refuse it wholesale "because the
//!   book can see that", and that is the trap above wearing its most persuasive hat: an EMPTY map is
//!   byte-identical for "flat", for "not learned yet", and for "the venue's own answer omitted it",
//!   so refusing the first necessarily refuses the other two. `verify` therefore refuses only where
//!   the venue POSITIVELY reported a position in that very symbol and none of it opposes the order —
//!   `crates/bridges/ctrader/src/positions.rs`'s `unauthoritative_for` is the rule, and
//!   its type doc carries the measured trap that made COVERAGE (not decodability) the test.
//! - **A genuine FLIP is refused.** A plain order larger than the total opposing exposure routes to
//!   [`ClosePlan::Open`] (preserving netting-account flip behaviour) and therefore opens risk past
//!   flat, which is what a halt is for. It does not trap: the position still closes under an
//!   exactly-sized or `reduce_only` order, which routes to `Close`.
//! - **[`CtraderExec::close_all`] is NOT gated, and that is load-bearing.** It closes by position id
//!   and can express nothing else — there is no side and no quantity in it that could open — so
//!   gating it could only ever remove an exit. It is the first-class flatten for a whole HEDGED book
//!   in ONE call (a reduce order nets one side at a time), i.e. exactly the tool an operator reaches
//!   for during the incident that made them `touch` the file.

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use vike_exec::halt::PositionEvidence;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{
    Event, OrderCancelRejected, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{HaltAdmit, OrderRequest, OrderType, now_ms};

use crate::conn::{ActorHandle, Command, ConnShared, OrderIdMap, POSITIONS_READ_WAIT};
use crate::event_mapper;
use crate::positions::{self, ClosePlan, TrackedPosition};
use crate::symbols::SymbolMap;

/// The terminal reason a `reduce_only` limit or stop is refused with on a hedge-capable account
/// ([`crate::symbols::SymbolMap::account_may_hold_a_hedge`]) when NO open position faces it. Named so
/// the refusal is greppable from a log and assertable from a test.
///
/// ⚠ **Sending it plain is the measured defect, not a choice.** cTrader has no reduce-only flag, and a
/// resting order sent with no `positionId` opens a position when it triggers: on the HEDGED demo account
/// a `reduce_only` SELL STOP under an open long OPENED a short beside the long (2026-10-25; the numbers
/// are in `crates/bridges/ctrader/CLAUDE.md`'s "A hedged account binds a reduce-only resting exit").
/// With a position to bind to the order carries its `positionId` and closes it instead; with none there is
/// nothing to close, so the order is refused rather than sent.
pub const NO_POSITION_TO_BIND_AN_EXIT: &str = "ctrader: a reduce_only limit or stop needs an open position to be bound to on a \
     HEDGED account, and none faces this order (cTrader has no reduce-only flag: an exit sent with no \
     position opens a new one when it triggers); nothing was sent. Open the position first, or place \
     the order without reduce_only if you mean to open one";

/// The terminal reason a `reduce_only` limit or stop is refused with when it is larger than the room any
/// ONE open position facing it has, more than one has room, and it CANNOT be split over them: one exit binds to
/// one position, so capping it would cover one position and leave the others naked while the engine (the venue's
/// accept carries no quantity) believes the whole size is protected. Since 0.1.97 such an exit rests as one bound
/// order per position ([`crate::exit_legs`]); this is what is left of the refusal: fewer than two positions can take
/// a leg of at least the symbol's minimum volume on its grid. One exit per position, each no larger than its
/// position, is what binds.
pub const EXIT_SPANS_POSITIONS: &str = "ctrader: this reduce_only exit is larger than the room any ONE open position facing it has, and more \
     than one position has room, but it cannot be split over them into orders of at least the \
     symbol's minimum volume; nothing was sent. Place one exit per position, each no larger than \
     its position";

/// The terminal reason a `reduce_only` limit or stop is refused with when every open position that
/// faces it already has exits OF ITS KIND (stops, or limits) covering its whole volume: another would
/// close more than the position holds, and the excess would OPEN a position when it triggered. A
/// bracket's stop and take-profit are different kinds, so both bind the same position; two independent
/// stops are not, and the second is this.
pub const EXIT_ALREADY_COVERED: &str = "ctrader: every open position facing this reduce_only order already has exits of this kind \
     covering its whole volume, so another would close more than the position holds; nothing was \
     sent. Cancel or amend the existing exit instead";

/// The terminal reason a `reduce_only` MARKET order is refused with on a hedge-capable account when the
/// position book is AUTHORITATIVE (the venue's own list, read in full) and no open position faces the order.
/// Sent plain, a market order against nothing OPENS a position on a hedged account, the locked pair the
/// engine books as flat (`crates/bridges/ctrader/CLAUDE.md`'s "A hedged account closes a reduce-only market
/// order by position id"). Not a halt's refusal: under an engaged HALT the order is admitted on its flag, as
/// on every other venue.
pub const NOTHING_TO_REDUCE: &str = "ctrader: a reduce_only market order has no open position to close on this HEDGED account (the \
     venue's position list shows none facing it); nothing was sent. Sent plain, a market order \
     would OPEN a position beside what it was meant to close";

/// The terminal reason a `reduce_only` MARKET order is refused with on a hedge-capable account when the
/// position book shows nothing facing it and is NOT authoritative, and the venue's positions could not be
/// read just now (the actor did not answer in time, the venue refused or did not reply, or the connection is
/// down): the order cannot be told from an opening one. The same defect as [`NOTHING_TO_REDUCE`], by the same
/// rule: nothing is sent, and a HALT admits the order on its flag as ever.
pub const POSITIONS_UNVERIFIED: &str = "ctrader: a reduce_only market order cannot be told from an opening one on this HEDGED account: \
     the position book is not authoritative and the venue's positions could not be read just now; \
     nothing was sent. Sent plain, a market order would OPEN a position beside what it was meant to \
     close. Try again once the connection is up";

/// Live cTrader exec client over an already-authenticated connection. Holds the connection's
/// shared state ([`ConnShared`]: command channel + symbol map + coid→orderId map + ctid) and its
/// own [`EventSender`] clone for the synchronous `OrderSubmitted` + any synthetic reject. The
/// authoritative async events return on the same ingest lane via the actor's `EXECUTION_EVENT`
/// routing.
///
/// `owner` distinguishes the two construction paths: `Some(ActorHandle)` for the single-client
/// [`CtraderExec::new`] path (this view owns + joins the actor thread — and thus keeps the socket
/// alive for its lifetime), `None` for the shared-socket [`CtraderExec::from_shared`] view (the
/// actor is owned by a [`crate::client::CtraderClient`] co-mounting a `CtraderData`; dropping this
/// view does not close the shared socket).
pub struct CtraderExec {
    shared: ConnShared,
    /// `Some` iff this view owns the actor thread (single-client path); its `Drop` joins the thread
    /// and closes the socket. `None` for a shared view (`from_shared`) — dropping it must not tear
    /// down the co-mounted socket. Held purely as an RAII drop-guard: this client overrides neither
    /// of `ExecutionClient`'s teardown hooks (`begin_detach`/`detach` — see the note on the impl
    /// below), so the field is never read — the owned `ActorHandle`'s `Drop` is the teardown.
    #[expect(dead_code)]
    owner: Option<ActorHandle>,
    events: EventSender,
    /// The operator HALT sentinel this client watches — handed in by `crate::adapter` from
    /// `AdapterInputs::process.halt_path` ([`Self::with_halt_path`]), never resolved here.
    ///
    /// ⚠ **`None` here means WATCHES NOTHING, and it used to mean ARMED.** The default on BOTH
    /// constructors was the process-wide sentinel `vike_boot::halt::halt_path_from_env`
    /// resolves, on the argument that this client only ever exists behind a live authenticated
    /// socket and so a default of "unarmed" could only be a way to forget. That default was the
    /// bridge reading a process-global path at the moment an order arrived — the one thing the start
    /// contract (decision 0096, rule 1) says a bridge may not do — and decision 0099 removed it. The
    /// argument against "unarmed" is answered where it belongs:
    /// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs`'s
    /// `every_live_start_hands_its_client_the_halt_path` fails a start that forgets, and a client
    /// with no path says so once at `error` (`vike_exec::halt::sentinel_engaged`). The paper
    /// book's `None` is a different thing again (the simulation primitive
    /// `crates/vike-sim/tests/r7_gate.rs` drives, which must never read a file on the box).
    halt_path: Option<PathBuf>,
    /// The halt-admit POLICY in force — `vike_config::Policy::halt_admit`, threaded from the
    /// composition root through `vike_tradehub_core::AdapterPolicy`. [`HaltAdmit::Admit`] (the default) is
    /// byte-identical to trusting the caller's `reduce_only` flag; [`HaltAdmit::Verify`] checks the
    /// venue's own position book first. **cTrader is the only venue where `Verify` is real** — see
    /// `vike_model::halt_verify_support`.
    halt_admit: HaltAdmit,
    /// How long a `reduce_only` market order waits for the actor's positions read
    /// ([`CtraderExec::read_positions_now`]); [`POSITIONS_READ_WAIT`] unless a test narrows it.
    positions_read_wait: Duration,
}

impl CtraderExec {
    /// Build over the handle returned by [`conn::connect_and_auth_exec`](crate::conn::connect_and_auth_exec).
    /// This view OWNS the actor thread (joins on drop). Pass the SAME `EventSender` clone that was
    /// wired into the actor, so this client's synchronous emits and the actor's async venue events
    /// land on one ingest lane, in order. Use this for the exec-only start.
    pub fn new(handle: ActorHandle, events: EventSender) -> Self {
        let shared = handle.shared();
        CtraderExec {
            shared,
            owner: Some(handle),
            events,
            halt_path: None,
            halt_admit: HaltAdmit::default(),
            positions_read_wait: POSITIONS_READ_WAIT,
        }
    }

    /// Build a `CtraderExec` VIEW over an actor owned elsewhere, from a cloned [`ConnShared`]
    /// (F4 single-socket start). Does NOT own the actor thread — dropping this view leaves the
    /// socket up for the co-mounted `CtraderData`; the actor is torn down solely by the owning
    /// [`crate::client::CtraderClient`]. `events` must be the SAME `EventSender` clone wired into
    /// the actor at connect time.
    pub fn from_shared(shared: ConnShared, events: EventSender) -> Self {
        CtraderExec {
            shared,
            owner: None,
            events,
            halt_path: None,
            halt_admit: HaltAdmit::default(),
            positions_read_wait: POSITIONS_READ_WAIT,
        }
    }

    /// Hand this client the HALT sentinel it watches: [`crate::adapter::live_runtime_for_account`]
    /// passes `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole
    /// process. Mirrors `vike_bridge_core::exec_actor::ExecActor::with_halt_path`; without it the
    /// client watches nothing (see the field's doc).
    ///
    /// A test passes a path of its own — `crates/bridges/ctrader/tests/common/mod.rs`'s
    /// `exec_with_no_halt` is the constructor that names one it never creates, so a test that
    /// submits an opening order is wired (an unwired client logs the NOT WIRED error) and
    /// deterministic. Since decision 0099 nothing on the box can engage a sentinel under such a
    /// test: the client no longer consults a process-wide one, so the 13 tests that used to go red
    /// under an operator's kill switch (MEASURED on the CI box, when the sentinel still had a
    /// `VIKE_HALT_FILE` override) cannot any more.
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// Set the halt-admit POLICY (default [`HaltAdmit::Admit`], which is byte-identical to trusting
    /// the caller's `reduce_only` flag). [`crate::adapter::live_runtime_for_account`] — called by
    /// [`crate::adapter::CtraderVenueAdapter`], which threads the operator's `policy.halt_admit` value
    /// in from `AdapterRequest::halt_admit` as a plain [`HaltAdmit`] — passes it here; this is the
    /// ONE venue on the roster where [`HaltAdmit::Verify`] does anything, because it is the only
    /// adapter holding a position book at its halt boundary.
    #[must_use]
    pub fn with_halt_admit(mut self, mode: HaltAdmit) -> Self {
        self.halt_admit = mode;
        self
    }

    /// Narrow how long a `reduce_only` market order waits for the actor's positions read (default
    /// [`POSITIONS_READ_WAIT`]). For a test that has no actor behind its command channel and must not sit out
    /// the production wait; a start never calls it.
    #[must_use]
    pub fn with_positions_read_wait(mut self, wait: Duration) -> Self {
        self.positions_read_wait = wait;
        self
    }

    /// Ask the actor to read the account's open positions from the venue and wait for its answer: `true` when
    /// the book was refreshed from an answer about this account. `false` for everything else, a dead actor, an
    /// unanswered command (a reconnect in flight drops it), a venue refusal, a timeout. The lock on the book
    /// is not held while waiting: the actor writes it.
    fn read_positions_now(&self) -> bool {
        let (reply, answer) = mpsc::channel();
        if self.tx().send(Command::ReadPositions { reply }).is_err() {
            return false;
        }
        answer.recv_timeout(self.positions_read_wait).unwrap_or(false)
    }

    /// Whether the position book has been fetched AUTHORITATIVELY and not invalidated since
    /// ([`crate::positions::PositionBook::is_fetched`]); a poisoned lock is "no".
    fn book_is_fetched(&self) -> bool {
        self.shared.positions.lock().is_ok_and(|book| book.is_fetched())
    }

    /// Is the operator kill switch engaged right now? Re-read per submit, so `rm` resumes trading
    /// and nothing latches — the same contract every other client honours.
    fn halt_engaged(&self) -> bool {
        vike_exec::halt::sentinel_engaged(self.halt_path.as_deref())
    }

    /// **The cTrader halt rule**: while the sentinel is engaged, this submit is admitted iff this
    /// client's own position book PROVES it closes **or** the SHARED predicate admits it under the
    /// halt-admit `mode` in force ([`HaltAdmit::Admit`], the default: `request.reduce_only`;
    /// [`HaltAdmit::Verify`]: that, and not a book that PROVES the order opens).
    ///
    /// `closes` is not a second opinion about the request — it is the routing decision
    /// [`ExecutionClient::submit`] has already made ([`positions::plan_reduce`] returned
    /// [`ClosePlan::Close`]). Passing the routing verdict in, rather than re-deriving one, is what
    /// makes it impossible for the halt gate and the reduce router to disagree: there is exactly one
    /// place that decides what closes on this venue, and a future change to it moves this gate with
    /// it.
    ///
    /// ⚠ **The `||` is the anti-trap half, and dropping it re-breaks the restart case.** A
    /// [`crate::positions::PositionBook`] the venue has not answered for is EMPTY, so "the book does
    /// not know this position" and "there is no such position" are the same state in it — and
    /// `conn`'s `seed_positions_at_connect` narrows that window without closing it (the seed is
    /// best-effort, exec-mounts-only, and a socket death re-clears the evidence flag). A
    /// verified-only rule (`closes` alone) therefore refused a genuine exit from any position opened
    /// before this process started — the trap `docs/ops/kill-switches.md` promises cannot happen.
    /// Deferring to `halt_admits_submit_under` when the book is silent makes this venue a SUPERSET
    /// of every other client at the same `mode` rather than a different answer, which is also what
    /// stops the two from drifting: the flag half is not re-implemented here, it is CALLED.
    ///
    /// ⚠ **`mode` can only ever TIGHTEN the second disjunct, never the first.** Under
    /// [`HaltAdmit::Verify`] a book that PROVES the account is flat refuses a `reduce_only` order —
    /// there is nothing to exit, so nothing is trapped — while `closes` still short-circuits to
    /// ADMIT, because a routing decision that produced [`ClosePlan::Close`] is the strongest
    /// evidence in existence that the order reduces. `evidence` is therefore consulted only when the
    /// book proved nothing; see [`Self::halt_evidence`] for why every failure to answer is
    /// `Unknown` (⇒ admit) and never `Flat`.
    ///
    /// ⚠ **BOTH call sites OBEY the verdict**, and that is the difference between a gate and a
    /// decoration. The close-route arm used to compute this, log inside the `if`, and then run
    /// `route_close` unconditionally — so replacing this function's body with `false` left
    /// `a_plain_opposite_order_that_closes_is_admitted_under_halt` and
    /// `verify_still_admits_what_the_position_book_proves_closes` both GREEN (measured; the same
    /// mutation now reddens both — see the test below). That is the shape a
    /// "reachable" gate rots into: the only observable effect of a future tightening would have
    /// been an order leaving under an engaged HALT with NO log line, which is the one thing the
    /// NEVER-SILENT rule in the same function forbids. The refusal branch there is UNREACHABLE
    /// today — `closes` short-circuits — but that is a property of THIS function, pinned by
    /// `tests::the_close_disjunct_is_total_today_which_is_why_the_close_arm_never_refuses`, not
    /// an assumption the call site makes.
    ///
    /// See the module doc for the full argument.
    fn halt_admits_this_submit(
        closes: bool,
        request: &OrderRequest,
        mode: HaltAdmit,
        evidence: PositionEvidence,
    ) -> bool {
        closes || vike_exec::halt::halt_admits_submit_under(request, mode, evidence)
    }

    /// What this venue's own book says about `request` — the evidence
    /// [`vike_exec::halt::halt_admits_submit_under`] weighs under [`HaltAdmit::Verify`].
    ///
    /// ⚠ **Every failure to answer is `Unknown`, never `Flat`, and each one names itself.** A
    /// symbol that will not resolve, a poisoned lock, and — the one that matters most — a book that
    /// has never been AUTHORITATIVELY fetched, or was invalidated by a reconnect, or was rebuilt
    /// from a reconcile answer this build could not read in full
    /// (`crates/bridges/ctrader/src/positions.rs`'s `PositionBook`) all ADMIT. That is the whole
    /// difference between this and the trap: an empty map after a restart must not be read as "you
    /// are flat", or a halt stops the operator closing in the exact situation halts exist for.
    ///
    /// ⚠ **`Flat` here is manufactured from an ABSENCE** — `opposing_available` returning `0` over
    /// the tracked slice — so the question that decides a refusal is whether this book may speak
    /// about this symbol AT ALL. `PositionBook::unauthoritative_for` is that rule and it demands two
    /// independent things: PROVENANCE (an authoritative answer, for THIS account, read in full, no
    /// socket death since) and COVERAGE (the venue positively reported a position in this very
    /// symbol). Either missing ⇒ `Unknown` ⇒ ADMIT.
    ///
    /// ⚠ **Provenance alone was a measured trap.** A `RECONCILE_RES` that simply OMITS a position
    /// the venue holds is well-formed and fully classifiable, so no row count can see it: the book
    /// called itself authoritative, the absent symbol summed to `0`, and a `reduce_only` exit from
    /// a live 1000-unit long was REFUSED under an engaged halt. `verify` may only ever refuse where
    /// the venue said something POSITIVE about the instrument in question.
    ///
    /// ⚠ **Units.** `TrackedPosition::volume` is cTrader CENTI-units; the number handed to the
    /// predicate (and to the log line) is divided back to UNITS so it is comparable to
    /// `OrderRequest::qty` at a glance. Nothing compares it to the request today — see
    /// `halt_admits_submit_under`'s doc for why any opposing exposure admits — but a magnitude that
    /// is silently 100x off is the kind of thing a future comparison inherits.
    ///
    /// Evidence is taken over ALL opposing positions, including any already being closed under
    /// another coid: exposure that exists is exposure this order can reduce, and excluding it would
    /// manufacture a `Flat` (a refusal) out of concurrency.
    fn halt_evidence(&self, request: &OrderRequest) -> PositionEvidence {
        let Some(symbol_id) = self.symbols().id_of(&request.symbol) else {
            return PositionEvidence::Unknown(
                "the submit's symbol does not resolve to a venue symbol id",
            );
        };
        let Ok(book) = self.shared.positions.lock() else {
            return PositionEvidence::Unknown("the position book's lock is poisoned");
        };
        if let Some(why) = book.unauthoritative_for(symbol_id) {
            return PositionEvidence::Unknown(why);
        }
        let opposing_centi =
            positions::opposing_available(request.side.signum(), &book.for_symbol(symbol_id));
        PositionEvidence::fetched(opposing_centi as f64 / 100.0)
    }

    fn tx(&self) -> &Sender<Command> {
        &self.shared.tx
    }

    fn symbols(&self) -> &SymbolMap {
        &self.shared.symbols
    }

    fn orders(&self) -> &OrderIdMap {
        &self.shared.orders
    }

    /// Resolve a coid to the venue's numeric order id learned from execution events, if any.
    fn order_id_of(&self, coid: &str) -> Option<i64> {
        self.orders().lock().ok().and_then(|m| m.get(coid).copied())
    }

    /// The venue's numeric `orderId` learned for a coid from execution events, or `None` if the
    /// order has not yet been acknowledged. Public so callers (and tests) can confirm a coid is
    /// cancelable/modifiable before issuing the request.
    pub fn venue_order_id(&self, client_order_id: &str) -> Option<i64> {
        self.order_id_of(client_order_id)
    }

    /// Push one canonical event onto the ingest lane (synchronous emitter half). A dead core is
    /// logged, not panicked — the client keeps running.
    fn emit(&self, event: Event) {
        if self.events.blocking_send(event).is_err() {
            tracing::warn!(target: "ctrader", "core ingest gone; dropping synchronous exec event");
        }
    }

    /// Synthesize a terminal `OrderRejected` so a failed submit never vanishes.
    fn reject(&self, coid: &str, reason: &str) {
        self.emit(Event::OrderRejected(OrderRejected {
            cause: None,
            client_order_id: coid.to_string(),
            reason: reason.into(),
            ts: now_ms(),
        }));
    }

    /// **The broker's own state for the symbol, said in words BEFORE the order is sent**
    /// (`crate::trading_state`): `Some(reason)` when the symbol is restricted (`tradingMode` disabled or
    /// close-only, or `enabled=false`) AND this order is one we can show OPENS risk. Otherwise `None` and
    /// the order goes on as before; the broker still decides everything this does not read (a closed
    /// market, a holiday, short selling, exposure).
    ///
    /// ⚠ **A close is never refused here.** An order is "opening" only if it is not `reduce_only`, the
    /// position book has been fetched (`PositionBook::is_fetched`: an unfetched book is silence, not
    /// proof that a position is absent, the trap `halt_evidence` documents), and it is larger than the
    /// opposing volume the book holds for this symbol (a MARKET order only; a resting limit or stop is
    /// never routed to a close). So a plain opposite-side market order up to the open size, a
    /// `reduce_only` exit of either kind and every order on an unfetched book reach the broker. The
    /// decision reads the book, not the account type, so a netted and a hedged account take the same
    /// path. One empty-map lookup for an ordinary symbol.
    fn symbol_state_refusal(&self, request: &OrderRequest) -> Option<String> {
        let symbol_id = self.symbols().id_of(&request.symbol)?;
        let restriction = self.symbols().restriction(symbol_id)?;
        let (fetched, closing_room) = match self.shared.positions.lock() {
            Ok(book) => (
                book.is_fetched(),
                if matches!(request.order_type, OrderType::Market) {
                    positions::opposing_available(
                        request.side.signum(),
                        &book.for_symbol(symbol_id),
                    )
                } else {
                    0
                },
            ),
            Err(_) => (false, 0),
        };
        let requested_centi = (request.qty * 100.0).round() as i64;
        let opens = fetched && !request.reduce_only && requested_centi > closing_room;
        crate::trading_state::refusal(&request.symbol, restriction, opens)
    }

    /// Snapshot the account's currently-OPEN positions in `symbol_id` (from the actor-maintained
    /// map) as `(position_id, TrackedPosition)` pairs — the input the reduce planner reads. A clone,
    /// so the lock is released before any command is enqueued.
    fn open_positions_for(&self, symbol_id: i64) -> Vec<(i64, TrackedPosition)> {
        self.shared.positions.lock().map(|book| book.for_symbol(symbol_id)).unwrap_or_default()
    }

    /// The subset of `all` NOT already being closed under another coid (see
    /// [`crate::positions::CloseTracker::is_closing`]). One `closes` lock; a poisoned lock excludes
    /// nothing (fail-open — the worst case is the old overwrite path, and a poisoned close-state is a
    /// far bigger problem already logged elsewhere).
    fn free_positions(&self, all: &[(i64, TrackedPosition)]) -> Vec<(i64, TrackedPosition)> {
        let closing = self.shared.closes.lock().ok();
        all.iter()
            .copied()
            .filter(|(id, _)| match &closing {
                Some(t) => !t.is_closing(*id),
                None => true,
            })
            .collect()
    }

    /// Route a planned reduce to close-by-position-id: snap each FIFO leg's centi-volume to the
    /// symbol's step grid (a full-position leg is already valid; a partial leg rounds to a whole
    /// step), REGISTER the per-coid aggregate BEFORE enqueuing (so the actor correlates the venue's
    /// closing execution events back to this coid), then enqueue one [`Command::ClosePosition`] per
    /// leg. A dead actor (or a reduce that snaps to nothing) synthesizes the terminal `OrderRejected`
    /// the venue-adapter contract requires — the reduce never silently vanishes.
    fn route_close(
        &self,
        coid: &str,
        symbol_id: i64,
        open: &[(i64, TrackedPosition)],
        legs: Vec<(i64, i64)>,
    ) {
        let step = self.symbols().volume_grid(symbol_id).map(|g| g.step_volume).unwrap_or(0);
        let mut snapped: Vec<(i64, i64)> = Vec::new();
        for (position_id, want) in legs {
            let pos_vol = open.iter().find(|(id, _)| *id == position_id).map(|(_, p)| p.volume);
            let Some(pos_vol) = pos_vol else { continue };
            let vol = positions::snap_close_volume(want, pos_vol, step);
            if vol > 0 {
                snapped.push((position_id, vol));
            }
        }
        if snapped.is_empty() {
            self.reject(coid, "reduce resolved to zero closable volume");
            return;
        }
        if let Ok(mut t) = self.shared.closes.lock() {
            t.register(coid, &snapped);
        }
        for (position_id, volume) in snapped {
            if self
                .tx()
                .send(Command::ClosePosition {
                    position_id,
                    volume,
                    client_order_id: coid.to_string(),
                })
                .is_err()
            {
                if let Ok(mut t) = self.shared.closes.lock() {
                    t.forget(coid);
                }
                self.reject(coid, "ctrader actor thread gone");
                return;
            }
        }
    }

    /// **Place an exit that SPANS positions as one bound order per position**, `legs` already planned and
    /// recorded in the position book ([`crate::positions::PositionBook::split_exit_among`]: each leg a bound-exit
    /// row under its own coid, the group registered for the fold in [`crate::exit_legs`]). Every order is built
    /// BEFORE any is sent, so an exit with one leg the mapper cannot build is refused whole and leaves nothing
    /// behind; a dead actor is the same (its channel closes for good, so the first send is the one that fails).
    /// The engine placed ONE order under `request`'s coid and sees one lifecycle for it; the legs are the bridge's.
    ///
    /// ⚠ The legs cover what the positions' rooms allow, which is less than asked when the rooms together are
    /// smaller than the exit (`reduce_only` never closes more than the positions hold); the venue's accept carries
    /// no quantity, so the engine counts the size it asked for as protected, and the log says so.
    fn submit_split(
        &self,
        request: &OrderRequest,
        legs: &[positions::ExitLeg],
        requested_centi: i64,
    ) {
        let covered: i64 = legs.iter().map(|leg| leg.volume).sum();
        if covered < requested_centi {
            tracing::warn!(
                target: "ctrader",
                coid = %request.client_order_id,
                asked_centi = requested_centi,
                covered_centi = covered,
                "an exit that spans positions is CAPPED at the room they have left; the engine still \
                 counts the size it asked for as protected"
            );
        }
        tracing::info!(
            target: "ctrader",
            coid = %request.client_order_id,
            symbol = %request.symbol,
            legs = legs.len(),
            "a reduce_only exit larger than any one position's room rests as one bound order per position"
        );
        let mut orders = Vec::with_capacity(legs.len());
        for leg in legs {
            let leg_request = OrderRequest {
                client_order_id: leg.coid.clone(),
                qty: leg.volume as f64 / 100.0,
                ..request.clone()
            };
            let Some(mut order) =
                event_mapper::order_to_new_order(&leg_request, self.shared.ctid, self.symbols())
            else {
                self.forget_split(&request.client_order_id);
                self.reject(
                    &request.client_order_id,
                    "unmappable order (a leg of an exit that spans positions: unknown symbol, \
                     sub-minimum volume, or missing price)",
                );
                return;
            };
            order.position_id = Some(leg.position_id);
            order.volume = order.volume.min(leg.volume);
            orders.push(order);
        }
        // What is really sent is what the book records as covering each position.
        if let Ok(mut book) = self.shared.positions.lock() {
            for (leg, order) in legs.iter().zip(&orders) {
                book.set_exit_volume(&leg.coid, order.volume);
            }
        }
        for order in orders {
            if self.tx().send(Command::NewOrder(order)).is_err() {
                self.forget_split(&request.client_order_id);
                self.reject(&request.client_order_id, "ctrader actor thread gone");
                return;
            }
        }
    }

    /// Forget a split exit that never reached the venue: its group and every leg's bound-exit row.
    fn forget_split(&self, parent: &str) {
        if let Ok(mut book) = self.shared.positions.lock() {
            book.forget_split_exit(parent);
        }
    }

    /// Cancel the legs of the split exit `parent` that still rest, all or none: a leg whose venue `orderId` is not
    /// learned yet (its accept has not arrived) refuses the whole cancel, NON-terminally, and nothing is written
    /// (the engine retries it; cancelling some legs would leave the rest resting under an exit the engine thinks is
    /// going away). A leg the bridge is already cancelling itself (its position closed: the orphan rule) is not
    /// written a second time. The legs' cancel events fold into ONE `OrderCanceled` of `parent`, when the last
    /// leg is gone (`crate::exit_legs`).
    fn cancel_legs(&self, parent: &str, legs: &[String]) {
        let refuse = |reason: &str| {
            self.emit(Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: parent.to_string(),
                reason: reason.into(),
                ts: now_ms(),
            }));
        };
        let mut to_write: Vec<(String, i64)> = Vec::new();
        {
            let Ok(mut book) = self.shared.positions.lock() else {
                refuse("ctrader: the position book is unavailable");
                return;
            };
            for leg in legs {
                if book.cancel_in_flight(leg) {
                    continue;
                }
                let Some(order_id) = self.order_id_of(leg) else {
                    drop(book);
                    refuse(
                        "no venue orderId for a leg of this exit (not yet acknowledged); \
                         nothing was cancelled",
                    );
                    return;
                };
                to_write.push((leg.clone(), order_id));
            }
            for (leg, _) in &to_write {
                book.mark_cancelling(leg);
            }
        }
        for (leg, order_id) in to_write {
            if self
                .tx()
                .send(Command::CancelOrder { order_id, client_order_id: leg.clone() })
                .is_err()
            {
                if let Ok(mut book) = self.shared.positions.lock() {
                    book.cancel_failed(&leg);
                }
                refuse("ctrader actor thread gone");
                return;
            }
        }
    }

    /// Close EVERY position given as `(position_id, volume_centi)` legs — the first-class flatten for
    /// a whole HEDGED book, in one call. Fetch the legs from a reconcile
    /// ([`crate::recon_client::CtraderReconClient::open_positions`], which keeps the `position_id`
    /// that `PositionStatusReport` drops).
    ///
    /// ⚠ **Its original justification is GONE, and that is worth saying rather than leaving a stale
    /// rationale in place.** This existed because a fresh start tracked NO positions — against an
    /// empty map [`positions::plan_reduce`] sees no opposing exposure and OPENS a hedge instead of
    /// closing (the demo-account "12 stacked hedged positions a fresh connect doesn't know" case).
    /// `crates/bridges/ctrader/src/conn/reconcile.rs`'s `seed_positions_at_connect` fixed that at the source,
    /// so an ordinary reduce order now reaches a pre-existing position. What `close_all` still does
    /// that a reduce cannot: flatten BOTH sides of a hedged book at once (a reduce nets one side at
    /// a time), with no symbol resolution at all, since close-by-position-id is side- and
    /// symbol-agnostic.
    ///
    /// Each leg becomes its OWN close order under a fresh coid: it emits `OrderSubmitted` synchronously
    /// (the emitter split, mirroring [`ExecutionClient::submit`]), REGISTERS a one-position close in the
    /// shared [`crate::positions::CloseTracker`] BEFORE enqueuing (so the actor correlates the venue's
    /// closing execution events — keyed by `positionId`, since the closing order carries no coid of ours
    /// — back to it, folding a proper `OrderAccepted → OrderFilled` plus the bare `Event::Fill` that
    /// nets `Account` toward flat), then enqueues one [`Command::ClosePosition`]. Close-by-position-id is
    /// side-agnostic — a long closes via a SELL deal, a short via a BUY — so a hedged book flattens with
    /// no per-side special-casing. A position already being closed under another coid is SKIPPED (its
    /// coid owns the tracker mapping; re-registering would strand it); a dead actor synthesizes the
    /// terminal `OrderRejected` the venue-adapter contract requires. Returns the coid issued per closed
    /// position (skipped legs excluded). `&self`: closes read shared state and enqueue, never mutate the
    /// client.
    pub fn close_all(&self, positions: &[(i64, i64)]) -> Vec<String> {
        let ts = now_ms();
        let mut coids = Vec::with_capacity(positions.len());
        for &(position_id, volume) in positions {
            if position_id == 0 || volume <= 0 {
                continue; // nothing closable
            }
            // Don't re-close a position a concurrent reduce already has in flight — registering here
            // would clobber that coid's `by_position` mapping and strand it (fail-open on a poisoned
            // lock: proceed, same as `free_positions`). None for the pure flatten-all case.
            if self.shared.closes.lock().map(|t| t.is_closing(position_id)).unwrap_or(false) {
                continue;
            }
            let coid = format!("closeall-{position_id}-{ts}");
            self.emit(Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.clone(), ts }));
            if let Ok(mut t) = self.shared.closes.lock() {
                t.register(&coid, &[(position_id, volume)]);
            }
            if self
                .tx()
                .send(Command::ClosePosition { position_id, volume, client_order_id: coid.clone() })
                .is_err()
            {
                if let Ok(mut t) = self.shared.closes.lock() {
                    t.forget(&coid);
                }
                self.reject(&coid, "ctrader actor thread gone");
            }
            coids.push(coid);
        }
        coids
    }
}

/// ⚠ **Teardown here is by DROP, not by `detach`, and this venue is the one DECLARED RESIDUAL of
/// the two-phase core teardown** (`crates/vike-exec/src/execution_engine/client.rs`'s
/// `ExecutionClient::begin_detach`). This impl overrides neither `begin_detach` nor `detach`: the
/// actor thread stops when the owning [`ActorHandle`] drops with the engine
/// (`crates/bridges/ctrader/src/conn.rs`'s `ActorHandle::stop` sends `Command::Shutdown` and joins;
/// its `impl Drop` does the same), and `actor_loop` drains its command channel only between socket
/// reads, so the join waits out up to one 1 s read timeout. An exec-only start therefore pays
/// exactly ONE un-overlapped wind-down, at engine drop, after every other venue has already been
/// joined. It DOES have a flag it could raise — `Command::Shutdown` over the shared `tx` IS that
/// signal — and wiring it is deliberately out of scope: one wind-down is the cost the phase split
/// exists to bring a twelve-venue start DOWN to, and the [`CtraderExec::from_shared`] view owns no
/// thread at all (its actor belongs to a [`crate::client::CtraderClient`], which is no
/// `ExecutionClient`), so a `begin_detach` here would have to reach through a handle this view
/// does not own.
impl ExecutionClient for CtraderExec {
    /// Emit `OrderSubmitted` synchronously (the emitter split), then EITHER route the submit to
    /// close-by-position-id (when it REDUCES a tracked open position — the mode-agnostic flatten
    /// that fixes the hedging-account "opposite order opens a hedge" bug) OR map + enqueue a
    /// `NewOrder` (an open/add/flip — unchanged). A mapping failure (unknown symbol / sub-min volume
    /// / bad type-side / missing conditional price) or a dead actor synthesizes a terminal
    /// `OrderRejected`.
    ///
    /// The reduce decision ([`positions::plan_reduce`]) reads the ACTUAL tracked positions, so it is
    /// identical on hedging and netting accounts: an opposite-side MARKET order up to the open size
    /// closes FIFO (oldest first); a same-side add, a flat book, or a flip past flat falls through to
    /// the new-order path, except that a `reduce_only` order on an account that can hold a hedge never
    /// does so against a flat book: the venue is read first when the book is not authoritative, and
    /// nothing facing the order then is a refusal by name outside a HALT ([`NOTHING_TO_REDUCE`],
    /// [`POSITIONS_UNVERIFIED`]).
    ///
    /// ⚠ **A RESTING order (limit / stop) is never routed to a close**, whatever its side, size or
    /// `reduce_only` flag: the planner is not asked, and the order is a `NewOrder` that rests. What
    /// the venue does when it triggers on a HEDGING account is MEASURED: sent with no `positionId` it
    /// OPENS an opposite position. So on a hedge-capable account (not stated NETTED) a `reduce_only`
    /// limit or stop is BOUND to the oldest free open position it faces whose room its kind (stop or
    /// limit) leaves for all of it (`ProtoOaNewOrderReq::position_id`; capped at the room when exactly one
    /// position has any), or placed as one bound order per position when it fits none of several
    /// (`submit_split`: the engine sees one order, [`crate::exit_legs`]), or refused by name
    /// when there is none ([`NO_POSITION_TO_BIND_AN_EXIT`], [`EXIT_ALREADY_COVERED`]) or the split is
    /// impossible ([`EXIT_SPANS_POSITIONS`]); on a netted account it is sent exactly as before. `crates/bridges/ctrader/CLAUDE.md`'s "A hedged account binds a
    /// reduce-only resting exit".
    ///
    /// ⚠ **The operator HALT sentinel gates the NEW-ORDER path only** — see the module doc. A submit
    /// the book PROVES is a close has already returned down the close route by then; what is left
    /// falls to the same `reduce_only` predicate every other client uses, so this venue admits a
    /// strict superset of what they do, and the binding above runs AFTER that verdict.
    fn submit(&mut self, request: &OrderRequest) {
        self.emit(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: now_ms(),
        }));
        // The broker's own word for the symbol (`tradingMode`, `enabled`), BEFORE anything is sent or
        // routed: an order that would open in a symbol it forbids is refused by name here instead of
        // dying as the broker's bare reject. Closes pass (`symbol_state_refusal`).
        if let Some(reason) = self.symbol_state_refusal(request) {
            tracing::warn!(
                target: "ctrader",
                coid = %request.client_order_id,
                symbol = %request.symbol,
                reason = %reason,
                "refusing an opening order: the broker's own state for the symbol forbids it"
            );
            self.reject(&request.client_order_id, &reason);
            return;
        }
        // ONE filesystem `exists()` per submit, evaluated here and reused below — the same budget
        // `ExecActor::submit` holds itself to. Rearranging the arms must not turn it into two.
        let halted = self.halt_engaged();
        // ⚠ Only a MARKET order is a flatten. A resting order (limit / stop) against an open
        // position is a protective exit (a bracket's stop-loss and take-profit are exactly that):
        // routed as a close it was an immediate market close, so a stop never rested and never
        // protected. It goes to `ProtoOANewOrderReq` like any other conditional.
        let market = matches!(request.order_type, OrderType::Market);
        // Reduce routing: only for a MARKET order, when the symbol resolves and the side is
        // directional (an unknown symbol / side-0 falls through and is rejected by
        // `order_to_new_order` below, unchanged).
        if market && let Some(symbol_id) = self.symbols().id_of(&request.symbol) {
            let order_side = request.side.signum();
            let requested_centi = (request.qty * 100.0).round() as i64;
            let mut all_open = self.open_positions_for(symbol_id);
            // ⚠ A `reduce_only` MARKET order with nothing facing it in the book, on an account that can hold a
            // hedge, is the order that OPENS a hedge when it is sent plain (the 2026-10-25 locked pair), and an
            // empty book may only mean the venue was never asked (a seed that failed on a start that held a
            // position, a socket death since). So the venue is asked first, ONCE, when the book is not
            // authoritative: whatever it lists facing the order is then closed by position id, below.
            let nothing_facing = request.reduce_only
                && requested_centi > 0
                && self.symbols().account_may_hold_a_hedge()
                && positions::opposing_available(order_side, &all_open) == 0;
            if nothing_facing && !self.book_is_fetched() && self.read_positions_now() {
                all_open = self.open_positions_for(symbol_id);
            }
            // Exclude positions ALREADY being closed under another coid: the `PositionMap` only
            // clears a position when the venue's close event lands, so a SECOND reduce issued before
            // the first round-trips (scale-out, a stop right after a flatten, a retry) would
            // otherwise re-plan against the SAME position_id and `register` would OVERWRITE the first
            // coid's mapping — misattributing its fill and stranding it. Plan only over FREE positions.
            let free_open = self.free_positions(&all_open);
            if let ClosePlan::Close(legs) =
                positions::plan_reduce(order_side, requested_centi, request.reduce_only, &free_open)
            {
                // ⚠ The predicate's VERDICT is OBEYED here, not merely consulted. It was once
                // computed, logged on, and then discarded — `route_close` ran unconditionally
                // underneath it — which made this arm's gate decorative: replacing
                // `halt_admits_this_submit`'s body with `false` left both close-route tests GREEN,
                // so the only effect of a future tightening would have been an order leaving under
                // an engaged HALT with no log line at all, breaking the NEVER-SILENT rule three
                // lines below in the same function. Today `closes = true` makes the call total (the
                // `closes ||` short-circuits), so the refusal branch is unreachable — that is a
                // property of the predicate, pinned by `halt_admits_this_submit`'s own unit tests,
                // NOT an assumption this call site is allowed to make.
                if halted {
                    let evidence = self.halt_evidence(request);
                    if !Self::halt_admits_this_submit(true, request, self.halt_admit, evidence) {
                        tracing::warn!(
                            target: "ctrader",
                            coid = %request.client_order_id,
                            symbol = %request.symbol,
                            legs = legs.len(),
                            mode = %self.halt_admit.as_str(),
                            evidence = %evidence.label(),
                            "HALT engaged: REFUSING a submit that routes to a position CLOSE — the \
                             halt-admit rule refused it even though the book proves it reduces"
                        );
                        self.reject(&request.client_order_id, vike_exec::halt::HALT_REJECT_REASON);
                        return;
                    }
                    // ADMITTED UNDER HALT, and verified rather than trusted: this order closes
                    // tracked exposure, so refusing it would trap the operator in the position. Said
                    // out loud because an order leaving while the switch is engaged must be findable
                    // in the log — the same "NEVER SILENT" rule `ExecActor::submit` follows, minus
                    // its caveat: that one has to admit it is trusting a flag, this one names the
                    // positions it checked.
                    tracing::warn!(
                        target: "ctrader",
                        coid = %request.client_order_id,
                        symbol = %request.symbol,
                        legs = legs.len(),
                        verified = true,
                        mode = %self.halt_admit.as_str(),
                        evidence = %evidence.label(),
                        "HALT engaged: admitting a submit this account's OWN position book says \
                         CLOSES, so the halt cannot trap you in a position"
                    );
                }
                self.route_close(&request.client_order_id, symbol_id, &free_open, legs);
                return;
            }
            // Past the close route, the FREE slice could not (fully) satisfy this order. If opposing
            // exposure exists, this is a REDUCE intent — and it must NEVER fall through to a
            // hedge-opening NewOrder. Distinguish a GENUINE FLIP (a PLAIN order larger than TOTAL
            // opposing exposure — legitimately opens the other way, unchanged) from a reduce the free
            // portion can't cover because part is LOCKED in another coid's in-flight close (reject,
            // retry later). `reduce_only` never flips: reaching here means its free opposing was 0
            // (it caps at the free portion and would otherwise have routed to Close), so it rejects.
            let total_opposing = positions::opposing_available(order_side, &all_open);
            if total_opposing > 0 {
                let genuine_flip = !request.reduce_only && requested_centi > total_opposing;
                if !genuine_flip {
                    self.reject(
                        &request.client_order_id,
                        "opposing exposure already closing in flight; retry after it settles",
                    );
                    return;
                }
                // genuine flip → fall through to the new-order path below (unchanged).
            } else if nothing_facing {
                // Still nothing facing the order, after the venue was asked when the book was not
                // authoritative: sent plain it would OPEN a position beside what it meant to close. Refuse by
                // name, and say whether the venue's list was read (the book is authoritative) or not. ⚠ NOT
                // under an engaged HALT: there the order is admitted on its flag as on every other venue
                // (the operator's exit must never be trapped by a book that may only be empty because the venue
                // could not be asked), and the line below says what it was admitted on.
                let verified = self.book_is_fetched();
                if !halted {
                    tracing::warn!(
                        target: "ctrader",
                        coid = %request.client_order_id,
                        symbol = %request.symbol,
                        verified,
                        "refusing a reduce_only market order: no open position faces it, and sent plain it \
                         would OPEN a hedge"
                    );
                    self.reject(
                        &request.client_order_id,
                        if verified { NOTHING_TO_REDUCE } else { POSITIONS_UNVERIFIED },
                    );
                    return;
                }
                tracing::warn!(
                    target: "ctrader",
                    coid = %request.client_order_id,
                    symbol = %request.symbol,
                    verified,
                    "HALT engaged: admitting a reduce_only market order no open position faces, on its \
                     flag alone, as every venue does; sent plain it may OPEN a position"
                );
            }
        }
        // ── the HALT boundary ────────────────────────────────────────────────────────────────
        // Everything that reaches here routes to `ProtoOANewOrderReq`. The position book has said
        // nothing that PROVES a close — the close route returned above, and the in-flight arm
        // rejected above with the reason an operator can act on ("retry after it settles" — the
        // truthful cause, which the halt reason would have masked) — so `closes` is `false` here by
        // construction, and the verdict falls to the SHARED flag predicate — weighed under the
        // halt-admit POLICY this start was given (`admit`, the default, ignores the evidence and is
        // byte-identical to what shipped before the policy existed).
        //
        // ⚠ That fallback is the anti-trap half, not laxity. The book is only ever evidence FOR a
        // close, never against one: a book the venue has not answered for is empty, so "unknown" and
        // "flat" are the same value in it, and `conn`'s `seed_positions_at_connect` narrows that
        // window without closing it (best-effort, exec-mounts-only, re-cleared by a socket death).
        // Refusing a declared exit on that silence is precisely the trap — which is why
        // `halt_evidence` answers `Unknown` (⇒ ADMIT) for every book it cannot vouch for, and why
        // `verify` refuses only a book that PROVES the order opens.
        //
        // A genuine FLIP is still refused (it carries no flag and opens risk past flat), and it does
        // not trap either — the same position closes under an exactly-sized or `reduce_only` order.
        //
        // Terminal rejection, synthesized: the venue never sees the order, but the intent must not
        // vanish (the venue-adapter contract), and the reason is the SHARED wording so a halt
        // rejection is recognizable by the same string on every venue and in the GUI.
        if halted {
            let evidence = self.halt_evidence(request);
            if !Self::halt_admits_this_submit(false, request, self.halt_admit, evidence) {
                // A `reduce_only` order refused by the POLICY rather than by the flag is the
                // surprising case and gets its own line: the operator asked for `verify`, and this
                // is the book saying the order opens. Without it, a refused exit is indistinguishable
                // from an ordinary halt rejection in the log.
                if request.reduce_only {
                    tracing::warn!(
                        target: "ctrader",
                        coid = %request.client_order_id,
                        symbol = %request.symbol,
                        qty = request.qty,
                        mode = %self.halt_admit.as_str(),
                        evidence = %evidence.label(),
                        "HALT engaged under halt_admit=verify: REFUSING a reduce_only submit — the \
                         venue's own position book PROVES it opens risk"
                    );
                }
                self.reject(&request.client_order_id, vike_exec::halt::HALT_REJECT_REASON);
                return;
            }
            // NEVER SILENT: an order leaving while the switch is engaged must be findable in the
            // log. `verified = false` is the honest distinction from the close-route line above —
            // this one is the caller's flag taken on trust, exactly as at the `ExecActor` boundary,
            // because the book had nothing to say about the position it claims to close. `evidence`
            // names WHY it had nothing to say, so an admit granted on silence is distinguishable
            // from one granted on a real opposing position.
            //
            // Two wordings, because "the book cannot confirm" is false for a RESTING exit (never
            // routed to a close) against a position the book plainly holds: that one is admitted on
            // the flag too, and rests (bound to that position and capped at its room on a
            // hedge-capable account, at its own size on a netted one).
            let book_holds_the_position =
                self.symbols().id_of(&request.symbol).is_some_and(|symbol_id| {
                    positions::opposing_available(
                        request.side.signum(),
                        &self.open_positions_for(symbol_id),
                    ) > 0
                });
            if book_holds_the_position {
                tracing::warn!(
                    target: "ctrader",
                    coid = %request.client_order_id,
                    symbol = %request.symbol,
                    verified = false,
                    mode = %self.halt_admit.as_str(),
                    evidence = %evidence.label(),
                    "HALT engaged: admitting a reduce_only submit that RESTS against a position this \
                     account's book holds — the flag is taken on trust (a hedge-capable account \
                     then binds it to that position)"
                );
            } else {
                tracing::warn!(
                    target: "ctrader",
                    coid = %request.client_order_id,
                    symbol = %request.symbol,
                    verified = false,
                    mode = %self.halt_admit.as_str(),
                    evidence = %evidence.label(),
                    "HALT engaged: admitting a reduce_only submit this account's position book \
                     cannot confirm (a book the venue has not answered for is empty) — refusing it \
                     would trap you in a position opened before this process started"
                );
            }
        }
        // ── BIND a reduce_only resting exit to the position it closes (hedge-capable accounts) ──────
        // cTrader has no reduce-only flag, and a resting order sent with NO `positionId` opens a
        // position when it triggers (measured: the 2026-10-25 hedge). Sent with the `positionId` of an
        // OPEN position it closes that position instead (measured 2026-10-26). So the exit takes the
        // oldest free position facing it whose room its kind (stop or limit) leaves for all of it, capped
        // at the room when exactly one position has any; with none, or with several and none big enough,
        // it is refused by name and NOTHING is sent. A NETTED account skips all of this and sends exactly
        // what it always did.
        let requested_centi = (request.qty * 100.0).round() as i64;
        let mut bind: Option<(i64, i64)> = None;
        let kind = match request.order_type {
            OrderType::Stop => Some(positions::ExitKind::Stop),
            OrderType::Limit => Some(positions::ExitKind::Limit),
            _ => None,
        };
        if request.reduce_only
            && requested_centi > 0
            && let Some(kind) = kind
            && self.symbols().account_may_hold_a_hedge()
            && let Some(symbol_id) = self.symbols().id_of(&request.symbol)
        {
            let side = request.side.signum();
            let all_open = self.open_positions_for(symbol_id);
            let free_open = self.free_positions(&all_open);
            let mut chosen = match self.shared.positions.lock() {
                Ok(mut book) => book.bind_exit_among(
                    &request.client_order_id,
                    side,
                    kind,
                    requested_centi,
                    &free_open,
                ),
                Err(_) => Err(positions::ExitRefusal::NothingToBind),
            };
            // One exit larger than any ONE position's room while several have room: it rests as one bound
            // order per position (`crate::exit_legs`), and the engine still sees one order. A plan of fewer
            // than two legs is the same refusal as before.
            if chosen == Err(positions::ExitRefusal::Spans) {
                let grid = self.symbols().volume_grid(symbol_id).unwrap_or_default();
                let split = match self.shared.positions.lock() {
                    Ok(mut book) => book.split_exit_among(
                        &request.client_order_id,
                        side,
                        kind,
                        requested_centi,
                        &free_open,
                        grid,
                    ),
                    Err(_) => Err(positions::ExitRefusal::Spans),
                };
                match split {
                    Ok(legs) => {
                        self.submit_split(request, &legs, requested_centi);
                        return;
                    }
                    Err(refusal) => chosen = Err(refusal),
                }
            }
            match chosen {
                Ok(found) => bind = Some(found),
                Err(refusal) => {
                    let reason = match refusal {
                        positions::ExitRefusal::Covered => EXIT_ALREADY_COVERED,
                        positions::ExitRefusal::Spans => EXIT_SPANS_POSITIONS,
                        positions::ExitRefusal::NothingToBind
                            if positions::opposing_available(side, &all_open) > 0 =>
                        {
                            "opposing exposure already closing in flight; retry after it settles"
                        }
                        positions::ExitRefusal::NothingToBind => NO_POSITION_TO_BIND_AN_EXIT,
                    };
                    tracing::warn!(
                        target: "ctrader",
                        coid = %request.client_order_id,
                        symbol = %request.symbol,
                        order_type = %request.order_type.as_str(),
                        reason,
                        "refusing a reduce_only resting order: no open position it can be bound to"
                    );
                    self.reject(&request.client_order_id, reason);
                    return;
                }
            }
        }
        // The exit goes out at the room the position had, which may be less than asked (reduce_only
        // never closes more than the position holds).
        let capped;
        let to_map = match bind {
            Some((_, volume)) if volume < requested_centi => {
                capped = OrderRequest { qty: volume as f64 / 100.0, ..request.clone() };
                // The venue's accept carries no quantity, so the engine will believe the whole size is
                // protected: say so where an operator can see it.
                tracing::warn!(
                    target: "ctrader",
                    coid = %request.client_order_id,
                    asked_centi = requested_centi,
                    sent_centi = volume,
                    "a bound reduce_only exit is CAPPED at the room its position has left; the engine \
                     still counts the size it asked for as protected"
                );
                &capped
            }
            _ => request,
        };
        let unbind = |this: &Self| {
            if bind.is_some()
                && let Ok(mut book) = this.shared.positions.lock()
            {
                book.unbind_exit(&request.client_order_id);
            }
        };
        match event_mapper::order_to_new_order(to_map, self.shared.ctid, self.symbols()) {
            Some(mut new_order) => {
                new_order.position_id = bind.map(|(position_id, _)| position_id);
                if let Some((_, room)) = bind {
                    // The mapper puts the volume on the grid by NEAREST step, which can round UP past the
                    // room: clamp DOWN, on the grid, and record what is really sent (two same-kind exits
                    // must never sum above their position).
                    let grid = self.symbols().volume_grid(new_order.symbol_id).unwrap_or_default();
                    let mut volume = new_order.volume.min(room);
                    if grid.step_volume > 0 {
                        volume -= volume % grid.step_volume;
                    }
                    if volume <= 0 || volume < grid.min_volume {
                        unbind(self);
                        self.reject(
                            &request.client_order_id,
                            "unmappable order (sub-minimum volume once capped at the room its \
                             position has)",
                        );
                        return;
                    }
                    new_order.volume = volume;
                    if let Ok(mut book) = self.shared.positions.lock() {
                        book.set_exit_volume(&request.client_order_id, volume);
                    }
                }
                if self.tx().send(Command::NewOrder(new_order)).is_err() {
                    unbind(self);
                    self.reject(&request.client_order_id, "ctrader actor thread gone");
                }
            }
            None => {
                unbind(self);
                // A kind this venue never maps (stop-limit, take-profit, a word nobody knows) is
                // named, so the refusal says WHICH order it is, not a list of unrelated causes.
                let mapped = matches!(
                    request.order_type,
                    OrderType::Market | OrderType::Limit | OrderType::Stop
                );
                let reason = if mapped {
                    "unmappable order (unknown symbol, sub-minimum volume, or missing price)"
                        .to_string()
                } else {
                    format!(
                        "unmappable order: {} is not supported on this venue (market, limit and \
                         stop only)",
                        request.order_type.as_str()
                    )
                };
                self.reject(&request.client_order_id, &reason);
            }
        }
    }

    /// Cancel by coid: resolve the venue `orderId` from the correlation map and enqueue a
    /// `CancelOrder`. An unknown coid (no `orderId` learned yet) → NON-terminal
    /// `OrderCancelRejected` (the order stays live); the venue's authoritative `OrderCanceled`
    /// returns on the ingest lane.
    fn cancel(&mut self, client_order_id: &str) {
        // An exit that spans positions rests as one order per position under legs of its own: every leg that still
        // rests is cancelled (all or none), and the engine hears one `OrderCanceled` when the last is gone.
        let legs =
            self.shared.positions.lock().ok().and_then(|book| book.live_exit_legs(client_order_id));
        if let Some(legs) = legs {
            self.cancel_legs(client_order_id, &legs);
            return;
        }
        // The bridge is already cancelling this exit itself (its position closed, so it was orphaned,
        // `crate::positions::BoundExit`): a second `CancelOrderReq` for the same order, e.g. the engine's
        // OCO-sibling cancel racing the bridge's, would only come back as a refusal. The venue's
        // `OrderCancelled` for the first one is on its way and ends the order.
        if self.shared.positions.lock().is_ok_and(|book| book.cancel_in_flight(client_order_id)) {
            tracing::debug!(
                target: "ctrader",
                coid = client_order_id,
                "cancel not written twice: the bridge already cancelled this exit (its position closed)"
            );
            return;
        }
        match self.order_id_of(client_order_id) {
            Some(order_id) => {
                // A bound exit being cancelled stops counting toward its position's cover at once, so a
                // stop cancelled and immediately re-armed (a trailing stop) binds instead of being refused
                // as already covered; a refused cancel (or a failed write) puts it back.
                if let Ok(mut book) = self.shared.positions.lock() {
                    book.mark_cancelling(client_order_id);
                }
                if self
                    .tx()
                    .send(Command::CancelOrder {
                        order_id,
                        client_order_id: client_order_id.to_string(),
                    })
                    .is_err()
                {
                    if let Ok(mut book) = self.shared.positions.lock() {
                        book.cancel_failed(client_order_id);
                    }
                    self.emit(Event::OrderCancelRejected(OrderCancelRejected {
                        client_order_id: client_order_id.to_string(),
                        reason: "ctrader actor thread gone".into(),
                        ts: now_ms(),
                    }));
                }
            }
            None => self.emit(Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: client_order_id.to_string(),
                reason: "no venue orderId for coid (not yet acknowledged)".into(),
                ts: now_ms(),
            })),
        }
    }

    /// In-place amend (RUST-NATIVE HFT surface). Resolve the venue `orderId`, then enqueue an
    /// `AmendOrder` carrying the changed qty (units → centi-units) and/or price (absolute double).
    /// An unknown coid → NON-terminal `OrderModifyRejected` (the order keeps its terms); the
    /// venue's authoritative `OrderModified` (from `ORDER_REPLACED`) returns on the ingest lane.
    ///
    /// ⚠ **HALT blocks a modify outright, with NO position-verified exemption** — unlike
    /// [`ExecutionClient::submit`] above, and deliberately unlike it. The position book answers
    /// "does this order close exposure", and an amend is not an order: it changes the terms of a
    /// RESTING one, which may be unfilled, and `new_qty` can raise it. There is nothing here for the
    /// book to verify, so this falls back to the conservative rule every other client applies
    /// (`crates/vike-bridge-core/src/exec_actor.rs`'s `modify`): the exit path under a halt is
    /// CANCEL, which is never gated, and never modify.
    ///
    /// It says so rather than returning silently. This client was the FIRST to — the other three
    /// returned nothing at all, which is `docs/ops/kill-switches.md` section 5 ("a blocked modify is
    /// silent", now CLOSED) — because it already owes a NON-terminal advisory on every other refusal
    /// it makes here, so silence would have been the odd case out inside this very function. All
    /// four now emit the same event with the same reason, and the divergence that remains is none:
    /// the resting order keeps its terms on every client, and so does the verdict.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        if self.halt_engaged() {
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: vike_exec::halt::HALT_REJECT_REASON.into(),
                ts: now_ms(),
            }));
            return;
        }
        // An exit that spans positions is several orders, and an in-place amend of one of them would resize the
        // engine's whole order to that leg's size: it is cancelled and placed afresh, never amended.
        if self
            .shared
            .positions
            .lock()
            .is_ok_and(|book| book.live_exit_legs(&order.client_order_id).is_some())
        {
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: "ctrader: an exit that spans several positions rests as one order per position and \
                         cannot be amended in place; cancel it and place the new terms"
                    .into(),
                ts: now_ms(),
            }));
            return;
        }
        let Some(order_id) = self.order_id_of(&order.client_order_id) else {
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: "no venue orderId for coid (not yet acknowledged)".into(),
                ts: now_ms(),
            }));
            return;
        };
        // A bound exit may not be amended UP past the room its position has (it would close more than the
        // position holds, and the excess opens a position): refused here, non-terminal, nothing sent.
        if let Some(q) = new_qty
            && let Some(room) = self
                .shared
                .positions
                .lock()
                .ok()
                .and_then(|book| book.exit_room(&order.client_order_id))
            && (q * 100.0).round() as i64 > room
        {
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: format!(
                    "ctrader: a bound reduce_only exit cannot be amended to {q} units: its position \
                     has room for {} units of this kind of exit (it would close more than the \
                     position holds)",
                    room as f64 / 100.0
                )
                .into(),
                ts: now_ms(),
            }));
            return;
        }
        let amend = match event_mapper::amend_order_req(
            order,
            self.shared.ctid,
            order_id,
            new_qty,
            new_price,
            self.symbols(),
        ) {
            Ok(amend) => amend,
            // A size under the symbol's minimum: refused here, non-terminal, nothing sent.
            Err(reason) => {
                self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: reason.into(),
                    ts: now_ms(),
                }));
                return;
            }
        };
        // The cover count follows the REQUEST, not the venue's confirmation: until the answer arrives the exit counts
        // at the larger of its size and the asked one, so another exit binding in that instant cannot see room the
        // amend is about to take (`PositionBook::request_exit_volume`; a refusal, or `ORDER_REPLACED`, ends it).
        if let Some(volume) = amend.volume
            && let Ok(mut book) = self.shared.positions.lock()
        {
            book.request_exit_volume(&order.client_order_id, volume);
        }
        if self
            .tx()
            .send(Command::AmendOrder {
                client_order_id: order.client_order_id.clone(),
                req: amend,
            })
            .is_err()
        {
            if let Ok(mut book) = self.shared.positions.lock() {
                book.exit_amend_failed(&order.client_order_id);
            }
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: "ctrader actor thread gone".into(),
                ts: now_ms(),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(reduce_only: bool) -> OrderRequest {
        OrderRequest {
            client_order_id: "c1".into(),
            venue: "ctrader".into(),
            symbol: "EURUSD".into(),
            side: -1,
            qty: 1000.0,
            order_type: vike_model::OrderType::Market,
            reduce_only,
            ..Default::default()
        }
    }

    /// Every evidence value the boundary can produce, so no case below is silently skipped.
    fn all_evidence() -> Vec<PositionEvidence> {
        vec![
            PositionEvidence::NO_BOOK,
            PositionEvidence::Unknown("never fetched"),
            PositionEvidence::Unknown("the venue has reported no position in this symbol at all"),
            PositionEvidence::Flat,
            PositionEvidence::Opposing(0.5),
            PositionEvidence::Opposing(1e9),
        ]
    }

    /// ⚠ **The pin the close-route call site is allowed to rely on, and NOT the other way round.**
    /// A routing decision that produced close legs (`closes = true`) admits under every mode and
    /// every evidence value, because the rule is `closes || …`. That is what makes the refusal
    /// branch in `submit`'s close arm unreachable TODAY — and it is asserted here, at the
    /// predicate, so the call site can obey the verdict unconditionally instead of assuming it.
    ///
    /// ⚠ **MEASURED** (the CI box, 2026-08-08, lane a): replace
    /// [`CtraderExec::halt_admits_this_submit`]'s body with `false` for every input — the tightening
    /// the close arm used to be immune to — and this module runs `4 tests run: 1 passed, 3 failed`
    /// with THIS among them, plus `crates/bridges/ctrader/tests/exec_halt.rs` at
    /// `17 tests run: 8 passed, 9 failed` INCLUDING
    /// `a_plain_opposite_order_that_closes_is_admitted_under_halt` and
    /// `verify_still_admits_what_the_position_book_proves_closes`. ⚠ **Those last two are the
    /// proof of the repair**: with the verdict discarded they both stayed GREEN under exactly this
    /// mutation, which is what made the close arm's gate decorative.
    #[test]
    fn the_close_disjunct_is_total_today_which_is_why_the_close_arm_never_refuses() {
        for mode in [HaltAdmit::Admit, HaltAdmit::Verify] {
            for evidence in all_evidence() {
                for reduce_only in [true, false] {
                    assert!(
                        CtraderExec::halt_admits_this_submit(
                            true,
                            &req(reduce_only),
                            mode,
                            evidence
                        ),
                        "a submit the position book ROUTED to a close was refused \
                         ({mode:?}/{evidence:?}/reduce_only={reduce_only}) — the close route obeys \
                         this verdict, so a `false` here is an operator trapped in a position"
                    );
                }
            }
        }
    }

    /// …and with NO routing proof the venue is exactly the shared predicate, so this client can
    /// never refuse what another venue would have let out.
    #[test]
    fn without_a_routing_proof_this_client_is_exactly_the_shared_predicate() {
        for mode in [HaltAdmit::Admit, HaltAdmit::Verify] {
            for evidence in all_evidence() {
                for reduce_only in [true, false] {
                    let r = req(reduce_only);
                    assert_eq!(
                        CtraderExec::halt_admits_this_submit(false, &r, mode, evidence),
                        vike_exec::halt::halt_admits_submit_under(&r, mode, evidence),
                        "the flag half must be CALLED, not re-implemented \
                         ({mode:?}/{evidence:?}/reduce_only={reduce_only})"
                    );
                }
            }
        }
    }

    /// The union is a SUPERSET of the shared rule under both modes: this client can let more out
    /// under a halt, never less.
    #[test]
    fn the_union_never_refuses_what_the_shared_predicate_admits() {
        for mode in [HaltAdmit::Admit, HaltAdmit::Verify] {
            for evidence in all_evidence() {
                for reduce_only in [true, false] {
                    for closes in [true, false] {
                        let r = req(reduce_only);
                        let shared = vike_exec::halt::halt_admits_submit_under(&r, mode, evidence);
                        let here = CtraderExec::halt_admits_this_submit(closes, &r, mode, evidence);
                        assert!(
                            here || !shared,
                            "cTrader refused something the shared boundary admits \
                             ({mode:?}/{evidence:?}/closes={closes}/reduce_only={reduce_only})"
                        );
                    }
                }
            }
        }
    }

    /// `verify` is still a SUBSET of `admit` here, exactly as at the shared predicate — the knob
    /// cannot widen this venue's sentinel either.
    #[test]
    fn verify_is_a_subset_of_admit_at_this_venue_too() {
        for evidence in all_evidence() {
            for reduce_only in [true, false] {
                for closes in [true, false] {
                    let r = req(reduce_only);
                    let admit = CtraderExec::halt_admits_this_submit(
                        closes,
                        &r,
                        HaltAdmit::Admit,
                        evidence,
                    );
                    let verify = CtraderExec::halt_admits_this_submit(
                        closes,
                        &r,
                        HaltAdmit::Verify,
                        evidence,
                    );
                    assert!(
                        admit || !verify,
                        "verify admitted something admit refuses \
                         ({evidence:?}/closes={closes}/reduce_only={reduce_only})"
                    );
                }
            }
        }
    }
}
